//! Signal-related syscall handlers.
//!
//! Extracted from `signal.rs` to separate delivery logic from syscall glue.

extern crate alloc;
use alloc::vec::Vec;
use crate::mm::{PTEFlags, VirtAddr};
use crate::sched::process::PROCESS_TABLE;
use crate::syscall::remove_pid_from_futex_waiters;
use crate::syscall::user::{read_user_buffer, write_user_buffer};
use super::signal::{
    pending_interrupting_signal,
    pop_pending_siginfo_for, enqueue_pending_siginfo_process,
    clear_pending_signal_if_no_payload,
    current_user_write_range_ready, fault_in_user_write_range,
    select_signal_restorer, build_si_user_siginfo, build_si_siginfo,
    default_ignored_signal,
    pending_interrupting_signal_for_process,
    SIGINFO_BUF_LEN, SIGINFO_SIZE, UCONTEXT_SIZE,
    SA_RESTORER, RT_SIGRETURN_TRAMPOLINE,
};

pub(crate) fn sys_rt_sigsuspend(mask_ptr: usize, sigsetsize: usize) -> isize {
    let mask_bytes = core::cmp::min(sigsetsize, 8);
    let new_mask = if mask_ptr != 0 && mask_bytes > 0 {
        match read_user_buffer(mask_ptr as *const u8, mask_bytes) {
            Ok(b) => {
                let mut arr = [0u8; 8];
                arr[..mask_bytes].copy_from_slice(&b[..mask_bytes]);
                u64::from_le_bytes(arr)
            }
            Err(_) => return -14, // EFAULT
        }
    } else {
        0
    };

    // 不可屏蔽 SIGKILL(9), SIGSTOP(19)
    let unmaskable: u64 = (1u64 << 8) | (1u64 << 18);
    let effective_mask = new_mask & !unmaskable;

    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            if let Some(pid) = t.current {
                let (tgid, is_thread) = match t.get(pid) {
                    Some(p) => (p.tgid, p.is_thread),
                    None => return -3,
                };
                if let Some(p) = t.get_mut(pid) {
                    // 首次进入：保存 sigsuspend 之前的真实 mask 到独立字段；
                    // 二次进入（信号唤醒重执行 ecall）保持已保存的原 mask。
                    if p.sigsuspend_restore_mask.is_none() {
                        p.sigsuspend_restore_mask = Some(p.signal_mask);
                    }
                    p.signal_mask = effective_mask;
                }
                let leader_pid = if is_thread { tgid } else { pid };
                let mut deliverable = t.get(pid).map_or(0u64, |p| p.pending_signals & !p.signal_mask);
                while deliverable != 0 {
                    let sig_bit = deliverable & deliverable.wrapping_neg();
                    let signum = sig_bit.trailing_zeros() as usize + 1;
                    let handler = t
                        .get(leader_pid)
                        .map_or(0usize, |l| l.signal_handlers[signum - 1].handler);
                    let ignored = handler == 1 || (handler == 0 && default_ignored_signal(signum));
                    if ignored {
                        if let Some(p) = t.get_mut(pid) {
                            p.pending_signals &= !sig_bit;
                            let signo = signum as i32;
                            p.pending_signal_fds.retain(|(sig, _)| *sig != signo);
                            p.pending_siginfo.retain(|(sig, _)| *sig != signo);
                        }
                        deliverable &= !sig_bit;
                        continue;
                    }
                    if handler == 0 {
                        if let Some(p) = t.get_mut(pid) {
                            if let Some(restore) = p.sigsuspend_restore_mask.take() {
                                p.signal_mask = restore;
                            }
                        }
                    }
                    return -4; // EINTR
                }
            }
        }
    }

    // 挂起直到信号到达（SIGNAL_WAIT_SENTINEL）
    // sleep_current_for_signal() 是 -> ! 函数；信号唤醒后会重新执行 ecall，
    // 再次进入本 syscall 并从上面的 pending 分支返回 -EINTR。
    // Slice 2.5 wchan：rt_sigsuspend → Linux `do_sigsuspend`
    crate::sched::process::enter_wait_channel("do_sigsuspend");
    crate::sched::process::sleep_current_for_signal();
}

/// rt_sigaction(134) — 设置/获取信号处理函数
///
/// Linux struct kernel_sigaction (RISC-V):
///   handler: usize (8 bytes) — sa_handler/sa_sigaction
///   flags:   usize (8 bytes) — sa_flags
///   restorer: usize (8 bytes) — sa_restorer
///   mask:    u64   (8 bytes) — sa_mask (sigset_t, 仅取第一个 u64)
/// 总计 32 bytes
#[allow(dead_code)]
pub(crate) fn sys_rt_sigaction(signum: usize, act: usize, oldact: usize, sigsetsize: usize) -> isize {
    // Linux `sys_rt_sigaction`：第一道关卡是 sigsetsize == sizeof(sigset_t)(=8)，
    // 否则 EINVAL（rt_sigaction03 用 sigsetsize=-1 期望 EINVAL）。
    if sigsetsize != core::mem::size_of::<u64>() {
        return -22; // -EINVAL
    }
    if signum == 0 || signum > 64 || signum == 9 || signum == 19 {
        // SIGKILL(9) 和 SIGSTOP(19) 不可捕获
        if signum == 9 || signum == 19 { return -22; } // -EINVAL
        if signum == 0 || signum > 64 { return -22; }
    }

    let idx = signum - 1;

    // 先在不持锁的情况下读取用户空间的 new action（避免死锁）
    let new_action = if act != 0 {
        let buf = match read_user_buffer(act as *const u8, 32) {
            Ok(v) => v,
            Err(_) => return -14,
        };
        Some(decode_kernel_sigaction(&buf))
    } else {
        None
    };

    // 持锁操作：获取旧 action + 设置新 action（操作 tgid leader，CLONE_SIGHAND 共享语义）
    let old_action = {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                // 确定 signal handler 的持有者：tgid leader
                let leader_pid = t.get(pid).map(|p| if p.is_thread { p.tgid } else { pid }).unwrap_or(pid);
                if let Some(leader) = t.get_mut(leader_pid) {
                    let old = leader.signal_handlers[idx];

                    if let Some(ref new_act) = new_action {
                        leader.signal_handlers[idx] = *new_act;
                    }

                    if crate::cmdline::execdbg_enabled()
                        && (idx == 10 || idx == 6 || idx == 3)
                        && crate::kernel::signal::sigdbg_budget_ok()
                    {
                        crate::println!(
                            "[EXECDBG] sigaction pid={} leader={} signum={} old={:#x} new={:#x} flags={:#x}",
                            pid,
                            leader_pid,
                            idx + 1,
                            old.handler,
                            new_action.as_ref().map_or(usize::MAX, |a| a.handler),
                            new_action.as_ref().map_or(0, |a| a.flags)
                        );
                    }

                    Some(old)
                } else { None }
            } else { None }
        } else { None }
    }; // 锁释放

    let old_action = match old_action {
        Some(a) => a,
        None => return -3,
    };

    // 在不持锁的情况下写回旧 action 到用户空间
    if oldact != 0 {
        let mut buf = [0u8; 32];
        encode_kernel_sigaction(old_action, &mut buf);
        if write_user_buffer(oldact as *mut u8, &buf).is_err() {
            return -14; // -EFAULT
        }
    }

    0
}

#[cfg(target_arch = "loongarch64")]
fn decode_kernel_sigaction(buf: &[u8]) -> crate::sched::process::SignalAction {
    let handler = usize::from_le_bytes(buf[0..8].try_into().unwrap());
    let flags = usize::from_le_bytes(buf[8..16].try_into().unwrap());
    let mask = u64::from_le_bytes(buf[16..24].try_into().unwrap());
    crate::sched::process::SignalAction { handler, flags, restorer: 0, mask }
}

#[cfg(not(target_arch = "loongarch64"))]
fn decode_kernel_sigaction(buf: &[u8]) -> crate::sched::process::SignalAction {
    let handler = usize::from_le_bytes(buf[0..8].try_into().unwrap());
    let flags = usize::from_le_bytes(buf[8..16].try_into().unwrap());
    let restorer = usize::from_le_bytes(buf[16..24].try_into().unwrap());
    let mask = u64::from_le_bytes(buf[24..32].try_into().unwrap());
    crate::sched::process::SignalAction { handler, flags, restorer, mask }
}

#[cfg(target_arch = "loongarch64")]
fn encode_kernel_sigaction(action: crate::sched::process::SignalAction, buf: &mut [u8; 32]) {
    buf[0..8].copy_from_slice(&action.handler.to_le_bytes());
    buf[8..16].copy_from_slice(&action.flags.to_le_bytes());
    buf[16..24].copy_from_slice(&action.mask.to_le_bytes());
}

#[cfg(not(target_arch = "loongarch64"))]
fn encode_kernel_sigaction(action: crate::sched::process::SignalAction, buf: &mut [u8; 32]) {
    buf[0..8].copy_from_slice(&action.handler.to_le_bytes());
    buf[8..16].copy_from_slice(&action.flags.to_le_bytes());
    buf[16..24].copy_from_slice(&action.restorer.to_le_bytes());
    buf[24..32].copy_from_slice(&action.mask.to_le_bytes());
}

/// rt_sigprocmask(135) — 设置/获取信号屏蔽字
/// how: SIG_BLOCK=0, SIG_UNBLOCK=1, SIG_SETMASK=2
/// set/oldset: 指向 sigset_t (u64)
#[allow(dead_code)]
pub(crate) fn sys_rt_sigprocmask(how: usize, set: usize, oldset: usize, sigsetsize: usize) -> isize {
    // Linux `sys_rt_sigprocmask`：先校验 sigsetsize == sizeof(sigset_t)(=8) → EINVAL，
    // 再 copy_from_user(set) → EFAULT（rt_sigprocmask02：size=1→EINVAL；
    // set=(void*)-1,size=8→EFAULT）。
    if sigsetsize != core::mem::size_of::<u64>() {
        return -22; // -EINVAL
    }
    // 先在不持锁的情况下读取用户空间数据（避免与 read_user_buffer 的锁死锁）
    let new_mask_val = if set != 0 {
        match read_user_buffer(set as *const u8, 8) {
            Ok(buf) => Some(u64::from_le_bytes(buf[0..8].try_into().unwrap())),
            Err(_) => return -14,
        }
    } else {
        None
    };

    // 持锁操作进程信号掩码
    let old_mask_val = {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get_mut(pid) {
                    let old = process.signal_mask;

                    if let Some(new_mask) = new_mask_val {
                        // SIGKILL(9) 和 SIGSTOP(19) 不可屏蔽
                        let unmaskable = (1u64 << 8) | (1u64 << 18);
                        match how {
                            0 => process.signal_mask |= new_mask & !unmaskable,   // SIG_BLOCK
                            1 => process.signal_mask &= !new_mask,                 // SIG_UNBLOCK
                            2 => process.signal_mask = new_mask & !unmaskable,     // SIG_SETMASK
                            _ => return -22,
                        }
                    }

                    Some(old)
                } else { None }
            } else { None }
        } else { None }
    }; // 锁释放

    let old_mask_val = match old_mask_val {
        Some(v) => v,
        None => return -3,
    };

    // 在不持锁的情况下写回旧掩码到用户空间
    if oldset != 0 {
        let old_bytes = old_mask_val.to_le_bytes();
        if write_user_buffer(oldset as *mut u8, &old_bytes).is_err() {
            return -14;
        }
    }

    0
}

/// `rt_sigpending(136)` — POSIX 真实现：把"已 raise 但被当前信号屏蔽字阻塞、
/// 因而尚未投递"的信号集合写入用户 `sigset_t`（u64）。
///
/// 语义与 Linux `kernel/signal.c` 的 `do_sigpending` 对齐：
///   * 返回值 = 写入用户的 `sigset_t = pending_signals & signal_mask`
///   * `sigsetsize` 必须 == 8（Linux 强制），否则 `-EINVAL`
///   * `set == 0` → `-EFAULT`
#[allow(dead_code)]
pub(crate) fn sys_rt_sigpending(set: usize, sigsetsize: usize) -> isize {
    if sigsetsize != 8 {
        return -22;
    }
    if set == 0 {
        return -14;
    }
    let pending_blocked = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -3 };
        let pid = match t.current { Some(p) => p, None => return -3 };
        match t.get(pid) {
            Some(p) => p.pending_signals & p.signal_mask,
            None => return -3,
        }
    };
    if write_user_buffer(set as *mut u8, &pending_blocked.to_le_bytes()).is_err() {
        return -14;
    }
    0
}

/// rt_sigtimedwait(137) — 等待指定集合中的待处理信号
/// set: 指向 sigset_t(u64)
/// info: 指向 siginfo_t（可为 NULL，这里仅填写最小前缀）
/// timeout: 指向 timespec（可为 NULL，NULL 表示无限等待）
#[allow(dead_code)]
pub(crate) fn sys_rt_sigtimedwait(set: usize, info: usize, timeout: usize, _sigsetsize: usize) -> isize {
    const EAGAIN: isize = -11;
    const EFAULT: isize = -14;
    const EINVAL: isize = -22;
    const CLOCK_FREQ: u64 = crate::arch::consts::CLOCK_FREQ as u64;

    if set == 0 {
        return EFAULT;
    }

    let wait_mask = match read_user_buffer(set as *const u8, 8) {
        Ok(buf) => u64::from_le_bytes(buf[0..8].try_into().unwrap()),
        Err(_) => return EFAULT,
    };
    // 计算绝对 deadline：若 wakeup_channel > 1 说明是重入，复用已存的 deadline
    let deadline = {
        let stored = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_ref() {
                if let Some(pid) = t.current {
                    t.get(pid).map(|p| p.wakeup_channel as u64).unwrap_or(0)
                } else { 0 }
            } else { 0 }
        };

        if stored > 1 {
            // 重入：复用存储的绝对 deadline
            Some(stored)
        } else if timeout != 0 {
            let buf = match read_user_buffer(timeout as *const u8, 16) {
                Ok(buf) => buf,
                Err(_) => return EFAULT,
            };
            let sec = i64::from_le_bytes(buf[0..8].try_into().unwrap());
            let nsec = i64::from_le_bytes(buf[8..16].try_into().unwrap());
            if sec < 0 || !(0..1_000_000_000).contains(&nsec) {
                return EINVAL;
            }
            let now: u64;
            now = crate::arch::asm_helpers::rdtime();
            let ticks = (sec as u64)
                .saturating_mul(CLOCK_FREQ)
                .saturating_add((nsec as u64).saturating_mul(CLOCK_FREQ) / 1_000_000_000);
            Some(now.saturating_add(ticks))
        } else {
            None
        }
    };

    let wait_result = {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let table = match table.as_mut() {
            Some(t) => t,
            None => return -3,
        };
        let pid = match table.current {
            Some(p) => p,
            None => return -3,
        };
        let deliverable = match table.get(pid) {
            Some(p) => p.pending_signals & wait_mask,
            None => return -3,
        };
        if deliverable != 0 {
            let process = match table.get_mut(pid) {
                Some(p) => p,
                None => return -3,
            };
            let sig_bit = deliverable & deliverable.wrapping_neg();
            let signum = sig_bit.trailing_zeros() as usize + 1;
            let siginfo = pop_pending_siginfo_for(process, signum as i32)
                .unwrap_or_else(|| build_si_user_siginfo(signum as i32, 0, 0));
            clear_pending_signal_if_no_payload(process, signum, sig_bit);
            process.clear_wait_state();
            Ok(Some((signum, siginfo)))
        } else {
            if let Some(process) = table.get_mut(pid) {
                process.clear_wait_state();
            } else {
                return -3;
            }
            if pending_interrupting_signal_for_process(table, pid, wait_mask) {
                Err(-4)
            } else {
                if let Some(process) = table.get_mut(pid) {
                    if process.waiting_for != Some(crate::sched::process::SIGNAL_WAIT_SENTINEL) {
                        process.wait_channel = None;
                    }
                }
                Ok(None)
            }
        }
    };

    let signum = match wait_result {
        Ok(value) => value,
        Err(errno) => return errno,
    };

    if let Some((signum, siginfo)) = signum {
        if info != 0 {
            if !current_user_write_range_ready(info, SIGINFO_BUF_LEN) {
                return EFAULT;
            }
            let _ = write_user_buffer(info as *mut u8, &siginfo);
        }
        return signum as isize;
    }

    // 检查超时
    if let Some(dl) = deadline {
        let now: u64;
        now = crate::arch::asm_helpers::rdtime();
        if now >= dl {
            // 超时——清除 wakeup_channel 并返回 EAGAIN
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_mut() {
                if let Some(pid) = t.current {
                    if let Some(p) = t.get_mut(pid) {
                        p.clear_wait_state();
                    }
                }
            }
            return EAGAIN;
        }
    }

    // 未超时、无信号 → 存储 deadline 并睡眠
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            if let Some(pid) = t.current {
                if let Some(p) = t.get_mut(pid) {
                    p.wakeup_channel = deadline.unwrap_or(0) as usize;
                    // 记录本次 sigtimedwait 的等待集，供 child-exit 路径判断
                    // SIGCHLD 是否落在等待集内而提前唤醒（无需安装 handler）。
                    p.signal_wait_set = wait_mask;
                }
            }
        }
    }
    // STWDBG: gated evidence probe — show the sigtimedwait waiter about to
    // sleep, its wait mask, and how far away its timeout deadline is. Proves
    // whether a child-exit SIGCHLD wakes it early or it sleeps to the deadline.
    if crate::cmdline::stwdbg_enabled() {
        use core::sync::atomic::{AtomicU32, Ordering as StwOrd};
        static STW_SLEEP_CNT: AtomicU32 = AtomicU32::new(0);
        if STW_SLEEP_CNT.fetch_add(1, StwOrd::Relaxed) < 40 {
            let now = crate::arch::asm_helpers::rdtime();
            let rel_ticks = deadline.map(|d| d.saturating_sub(now)).unwrap_or(0);
            let rel_ms = rel_ticks.saturating_mul(1000) / CLOCK_FREQ;
            let pid = {
                let t = crate::sched::process::PROCESS_TABLE.lock();
                t.as_ref().and_then(|t| t.current).unwrap_or(0)
            };
            crate::println!(
                "[STWDBG] pid={} SLEEP mask=0x{:x} (sigchld_in_mask={}) deadline_rel={}ms",
                pid, wait_mask, (wait_mask & (1u64 << 16)) != 0, rel_ms
            );
        }
    }
    // Slice 2.5 wchan：rt_sigtimedwait → Linux `do_sigtimedwait`
    crate::sched::process::enter_wait_channel("do_sigtimedwait");
    crate::sched::process::sleep_current_for_signal();
}

/// Resolve a kill(2) `pid` argument into the set of (non-zombie) target process
/// ids, mirroring Linux `kill_something_info`:
///   * `pid  > 0` → that single process (empty if it does not exist),
///   * `pid == 0` → every process in the caller's process group,
///   * `pid <  -1`→ every process in process group `-pid`,
///   * `pid == -1`→ every process except init (broadcast).
fn collect_kill_targets(
    t: &crate::sched::process::ProcessTable,
    pid: isize,
    caller: usize,
) -> alloc::vec::Vec<usize> {
    use crate::sched::process::ProcessState;
    let mut targets: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
    // 调用者 PID namespace 作用域(同 do_kill 通用路径)：进程组/广播只命中调用者
    // ns 及后代可见的进程。pidns10：容器 init 的 kill(-1, SIGKILL) 不得波及宿主。
    let caller_root = t.get(caller).map(|p| p.pidns_root).unwrap_or(0);
    let caller_tgid = t.get(caller).map(|p| if p.is_thread { p.tgid } else { caller }).unwrap_or(caller);
    let visible = |cand_global: usize| caller_root == 0 || cand_global > caller_root;
    // kill(-1) 广播跳过的「init」：调用者 ns 的 1 号进程(全局 caller_root+1;
    // 全局 ns 时即 pid 1)。Linux do_kill 对 -1 额外跳过调用者自身线程组。
    let ns_init_global = caller_root + 1;
    if pid > 0 {
        if t.get(pid as usize).is_some() {
            targets.push(pid as usize);
        }
    } else if pid == 0 || pid < -1 {
        let pgid_target = if pid == 0 {
            match t.get(caller) {
                Some(p) => p.pgid,
                None => return targets,
            }
        } else {
            ((-pid) as usize) + caller_root
        };
        for cand in t.pids() {
            if let Some(p) = t.get(cand) {
                if p.pgid == pgid_target && visible(cand) && p.state != ProcessState::Zombie {
                    targets.push(cand);
                }
            }
        }
    } else {
        // pid == -1: broadcast to every visible process except the caller's ns
        // init (vpid 1) and the caller's own thread group.
        for cand in t.pids() {
            if cand == ns_init_global {
                continue;
            }
            if !visible(cand) {
                continue;
            }
            if let Some(p) = t.get(cand) {
                let cand_tgid = if p.is_thread { p.tgid } else { cand };
                if cand_tgid == caller_tgid {
                    continue;
                }
                if p.state != ProcessState::Zombie {
                    targets.push(cand);
                }
            }
        }
    }
    targets
}

/// kill(129) / tkill(130) — 发送信号
/// 将信号投递到目标进程的 pending_signals
#[allow(dead_code)]
pub(crate) fn sys_kill(pid: isize, sig: i32) -> isize {
    // kill(2)：siginfo.si_code = SI_USER(0)。
    do_kill(pid, sig, SI_USER)
}

/// siginfo.si_code 取值：kill→SI_USER(0)，tkill/tgkill→SI_TKILL(-6)。
/// glibc NPTL 的取消信号 handler 会校验 `si_code == SI_TKILL && si_pid == getpid()`，
/// 否则忽略取消请求（musl 不校验，故仅 glibc 受影响）。
pub(crate) const SI_USER: i32 = 0;
pub(crate) const SI_TKILL: i32 = -6;

fn do_kill(pid: isize, sig: i32, si_code: i32) -> isize {
    if sig < 0 || sig > 64 { return -22; } // -EINVAL

    if sig == 0 {
        // sig=0: existence/permission probe — no signal delivered, but the
        // target set is still resolved exactly like a real signal. ESRCH when
        // the target does not exist (Linux kill(2)).
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -3 };
        if pid > 0 {
            return if t.get(pid as usize).is_some() { 0 } else { -3 };
        } else if pid == 0 {
            // caller's own process group always contains the caller
            return 0;
        } else if pid < -1 {
            let pgid_target = (-pid) as usize;
            let exists = t.pids().into_iter().any(|c| {
                t.get(c).map_or(false, |p| {
                    p.pgid == pgid_target
                        && p.state != crate::sched::process::ProcessState::Zombie
                })
            });
            return if exists { 0 } else { -3 };
        } else {
            // pid == -1: broadcast to every process except init(1)
            let exists = t.pids().into_iter().any(|c| {
                c != 1 && t.get(c).map_or(false, |p| {
                    p.state != crate::sched::process::ProcessState::Zombie
                })
            });
            return if exists { 0 } else { -3 };
        }
    }

    // 单 pid 且非特殊处置信号（无默认终止/停止/继续专路，lat_sig 的
    // kill(getpid(), SIGUSR1) 即此类）：发送者信息、权限检查、SIGPENDING
    // 限额、投递合并在一次 PROCESS_TABLE 锁内完成，替代慢路径的 4 次加锁。
    if pid > 0 && !matches!(sig, 1 | 2 | 3 | 6 | 9 | 11 | 13 | 14 | 15 | 18 | 19) {
        let target = pid as usize;
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_mut() { Some(t) => t, None => return -3 };
        let current_pid = t.current;
        let (sender_pid, sender_uid) = match current_pid.and_then(|c| t.get(c)) {
            Some(s) => (
                if s.is_thread { s.tgid } else { current_pid.unwrap() },
                s.uid,
            ),
            None => (0, 0),
        };
        if let Some(cur) = current_pid {
            if let (Some(s), Some(tp)) = (t.get(cur), t.get(target)) {
                const CAP_KILL: u64 = 1u64 << 5;
                let privileged = s.euid == 0 || (s.cap_effective & CAP_KILL) != 0;
                let uid_ok = s.euid == tp.uid
                    || s.euid == tp.suid
                    || s.uid == tp.uid
                    || s.uid == tp.suid;
                if !(privileged || uid_ok) {
                    return -1; // -EPERM
                }
                if sig >= 32 {
                    let limit = tp.rlimits[11].0;
                    let pending = tp.pending_signals.count_ones() as u64
                        + tp.pending_siginfo.len() as u64;
                    if pending + 1 > limit {
                        return -11; // -EAGAIN
                    }
                }
            }
        }
        if t.get(target).is_none() {
            return -3; // -ESRCH
        }
        deliver_pending_signal_locked(t, target, sig, sender_pid, sender_uid, si_code, current_pid);
        return 0;
    }

    let (current_pid, sender_pid, sender_uid) = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let current = table.as_ref().and_then(|t| t.current);
        let uid = current
            .and_then(|pid| table.as_ref().and_then(|t| t.get(pid)).map(|p| p.uid))
            .unwrap_or(0);
        // Linux 的 siginfo.si_pid 填发送者的 tgid（task_tgid_vnr），而非线程 tid；
        // glibc 取消 handler 比对的正是 getpid()=tgid。
        let tgid = current
            .and_then(|pid| table.as_ref().and_then(|t| t.get(pid)).map(|p| if p.is_thread { p.tgid } else { pid }))
            .unwrap_or(0);
        (current, tgid, uid)
    };
    // check_kill_permission (Linux kernel/signal.c): a sender lacking privilege
    // may signal a positive-pid target only when its real or effective uid
    // matches the target's real or saved-set uid. SIGCONT is additionally
    // permitted within the same session. Group/broadcast targets (pid<=0) are
    // filtered per-target by the dedicated paths below, so they are not gated
    // here.
    if pid > 0 {
        if let Some(cur) = current_pid {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_ref() {
                if let (Some(s), Some(tp)) = (t.get(cur), t.get(pid as usize)) {
                    const CAP_KILL: u64 = 1u64 << 5;
                    let privileged = s.euid == 0 || (s.cap_effective & CAP_KILL) != 0;
                    let uid_ok = s.euid == tp.uid
                        || s.euid == tp.suid
                        || s.uid == tp.uid
                        || s.uid == tp.suid;
                    let cont_same_session = sig == 18 && s.sid == tp.sid;
                    if !(privileged || uid_ok || cont_same_session) {
                        return -1; // -EPERM
                    }
                    // __sigqueue_alloc / RLIMIT_SIGPENDING (Linux kernel/signal.c):
                    // a real-time signal (>= SIGRTMIN) allocates a queue entry and
                    // fails delivery with EAGAIN once the target's pending-signal
                    // count would exceed its RLIMIT_SIGPENDING soft limit. Legacy
                    // signals (1..31) collapse onto a single pending bit and never
                    // hit this limit.
                    if sig >= 32 {
                        let limit = tp.rlimits[11].0;
                        let pending = tp.pending_signals.count_ones() as u64
                            + tp.pending_siginfo.len() as u64;
                        if pending + 1 > limit {
                            return -11; // -EAGAIN
                        }
                    }
                }
            }
        }
    }
    if matches!(sig, 1 | 2 | 3 | 6 | 11 | 13 | 14 | 15) {
        if let Some(cur) = current_pid {
            let target_is_current = pid as usize == cur || pid == 0 || pid == -1;
            if target_is_current {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                let default_action = table
                    .as_mut()
                    .and_then(|t| t.get_mut(cur))
                    .map(|process| {
                        let action = process.signal_handlers[sig as usize - 1];
                        let sig_bit = 1u64 << (sig as u32 - 1);
                        if action.handler == 0 && (process.signal_mask & sig_bit) == 0 {
                            // 未注册 handler → 走默认动作；按 POSIX core-dumping
                            // 名单决定 ExitReason.core_dumped 字段。
                            let reason = if crate::sched::process::signal_is_core_dumping(sig) {
                                crate::sched::process::ExitReason::signaled_with_dump(sig)
                            } else {
                                crate::sched::process::ExitReason::signaled(sig)
                            };
                            process.exit_status = Some(reason);
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                drop(table);
                if default_action {
                    let reason = if crate::sched::process::signal_is_core_dumping(sig) {
                        crate::kernel::coredump::do_coredump(sig);
                        crate::sched::process::ExitReason::signaled_with_dump(sig)
                    } else {
                        crate::sched::process::ExitReason::signaled(sig)
                    };
                    // Linux do_group_exit: an uncaught fatal signal terminates
                    // every thread in the group, not just the caller (raise()
                    // in a multi-threaded process is tgkill on self).
                    crate::sched::process::exit_group_with_reason(reason);
                }
            }
        }
    }
    if sig == 9 {
        // SIGKILL: reap every targeted thread group (single pid / process group
        // / broadcast). A targeted self is reaped last by exiting the current
        // task — kill06 SIGKILLs its own process group with a negative pid, so
        // the caller is one of the group members.
        let cur = match current_pid {
            Some(c) => c,
            None => return -3,
        };
        let targets = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            match table.as_ref() {
                Some(t) => collect_kill_targets(t, pid, cur),
                None => return -3,
            }
        };
        if targets.is_empty() {
            return -3;
        }
        let self_targeted = targets.contains(&cur);

        let mut closed_fds_pending: Vec<(usize, Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>)> =
            Vec::new();
        let mut posix_lock_release_pids: Vec<usize> = Vec::new();
        {
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref mut t) = *table {
                for &target in &targets {
                    if target == cur {
                        continue; // defer the caller until the others are reaped
                    }
                    let (children, parent_pid, tgid, is_thread, child_pgid) = {
                        t.transfer_fd_table_owner_on_exit(target);
                        if let Some(process) = t.get_mut(target) {
                            remove_pid_from_futex_waiters(target);
                            let children = process.children.clone();
                            let parent_pid = process.parent_pid;
                            let tgid = process.tgid;
                            let is_thread = process.is_thread;
                            let child_pgid = process.pgid;
                            let closed = process.release_resources();
                            if !closed.is_empty() {
                                closed_fds_pending.push((target, closed));
                            }
                            posix_lock_release_pids.push(target);
                            (children, parent_pid, tgid, is_thread, child_pgid)
                        } else {
                            continue;
                        }
                    };
                    t.mark_zombie(target, crate::sched::process::ExitReason::killed());
                    // SIGKILL 终止整个线程组：杀死同 tgid 的所有线程（除调用者自身）
                    let siblings: Vec<usize> = t.pids().into_iter()
                        .filter(|&p| p != target && p != cur && t.get(p).map_or(false, |proc| {
                            proc.tgid == tgid && proc.state != crate::sched::process::ProcessState::Zombie
                        }))
                        .collect();
                    for sib in siblings {
                        remove_pid_from_futex_waiters(sib);
                        t.transfer_fd_table_owner_on_exit(sib);
                        if let Some(p) = t.get_mut(sib) {
                            let closed = p.release_resources();
                            if !closed.is_empty() {
                                closed_fds_pending.push((sib, closed));
                            }
                            posix_lock_release_pids.push(sib);
                        }
                        t.mark_zombie(sib, crate::sched::process::ExitReason::killed());
                    }
                    crate::sched::process::reparent_children_locked(t, children, parent_pid);
                    // 唤醒等待的父进程
                    if let Some(ppid) = parent_pid {
                        let should_wake = if let Some(parent) = t.get_mut(ppid) {
                            if !is_thread {
                                parent.pending_signals |= 1u64 << (17 - 1); crate::sched::process::note_signal_hint();
                            }
                            let thread_mode_accepts = match parent.wait_thread_mode {
                                None | Some(crate::sched::process::ThreadMode::NonThreadOnly) => !is_thread,
                                Some(crate::sched::process::ThreadMode::CloneOnly) => is_thread,
                                Some(crate::sched::process::ThreadMode::All) => true,
                            };
                            let target_match = if parent.wait_pgid.is_some() {
                                parent.wait_pgid == Some(child_pgid)
                            } else {
                                parent.waiting_for == Some(target) || parent.waiting_for.is_none()
                            };
                            let any_wait_wake = thread_mode_accepts && target_match;
                            let wake_for_signal = !is_thread
                                && parent.waiting_for == Some(crate::sched::process::SIGNAL_WAIT_SENTINEL);
                            if parent.state == crate::sched::process::ProcessState::Sleeping
                                && (any_wait_wake || wake_for_signal)
                            {
                                if any_wait_wake {
                                    if let Some(cx) = unsafe { parent.trap_context_mut() } {
                                        cx.sepc = cx.sepc.saturating_sub(4);
                                    }
                                }
                                Some(any_wait_wake)
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        if let Some(wake_for_waitpid) = should_wake {
                            t.mark_runnable(ppid, wake_for_waitpid);
                        }
                    }
                }
            }
        }
        for (pid, fds) in closed_fds_pending {
            for fd in fds {
                crate::fs::fd::finalize_closed_fd_for_pid(fd, pid);
            }
        }
        for pid in posix_lock_release_pids {
            crate::fs::file_lock::release_all_locks_for_pid(pid);
            crate::sched::posix_timer::release_for_pid(pid);
            crate::ipc::sysv::sem_undo_for_pid(pid);
        }
        if self_targeted {
            crate::sched::process::exit_with_reason(crate::sched::process::ExitReason::killed());
            unreachable!();
        }
        return 0;
    }

    if sig == 19 || sig == 18 {
        let mut targets: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
        {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_ref() {
                Some(t) => t,
                None => return -3,
            };
            if pid > 0 {
                if t.get(pid as usize).is_none() {
                    return -3;
                }
                targets.push(pid as usize);
            } else if pid == 0 || pid < -1 {
                let pgid_target = if pid == 0 {
                    let cur = match current_pid {
                        Some(p) => p,
                        None => return -3,
                    };
                    match t.get(cur) {
                        Some(p) => p.pgid,
                        None => return -3,
                    }
                } else {
                    (-pid) as usize
                };
                for cand in t.pids() {
                    if let Some(p) = t.get(cand) {
                        if p.pgid == pgid_target
                            && p.state != crate::sched::process::ProcessState::Zombie
                        {
                            targets.push(cand);
                        }
                    }
                }
            } else {
                for cand in t.pids() {
                    if cand == 1 { continue; }
                    if let Some(p) = t.get(cand) {
                        if p.state != crate::sched::process::ProcessState::Zombie {
                            targets.push(cand);
                        }
                    }
                }
            }
        }
        if targets.is_empty() {
            return -3;
        }
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_mut() {
            Some(t) => t,
            None => return -3,
        };
        let stop_current = sig == 19
            && current_pid
                .map(|cur| targets.iter().any(|target| *target == cur))
                .unwrap_or(false);
        for target in targets {
            if stop_current && current_pid == Some(target) {
                continue;
            }
            if sig == 19 {
                t.stop_process_for_signal(target, 19);
            } else {
                // SIGCONT: always resume; also set pending bit so a
                // registered user handler is invoked (Linux semantics).
                t.continue_process_for_signal(target);
                if let Some(process) = t.get_mut(target) {
                    let sig_bit = 1u64 << (18 - 1); // SIGCONT
                    process.pending_signals |= sig_bit; crate::sched::process::note_signal_hint();
                }
            }
        }
        drop(table);
        if stop_current {
            crate::sched::process::stop_current_for_signal(19);
        }
        return 0;
    }

    if (sig == 2 || sig == 15) && pid > 0 {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            let target = pid as usize;
            let sig_bit = 1u64 << (sig as u32 - 1);
            let should_defer_to_pending = match t.get(target) {
                Some(process) => {
                    let action = process.signal_handlers[sig as usize - 1];
                    action.handler != 0
                        || (process.signal_mask & sig_bit) != 0
                        || process.waiting_for == Some(crate::sched::process::SIGNAL_WAIT_SENTINEL)
                }
                None => return -3,
            };
            if !should_defer_to_pending {
                let children = match t.get(target) {
                    Some(process) if !process.children.is_empty() => process.children.clone(),
                    Some(_) => alloc::vec::Vec::new(),
                    None => return -3,
                };
                if !children.is_empty() {
                    let parent_pid = t.get(target).and_then(|p| p.parent_pid);
                    let mut closed_fds_pending: Vec<(usize, Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>)> =
                        Vec::new();
                    let mut posix_lock_release_pids: Vec<usize> = Vec::new();
                    let mut victims = children;
                    victims.push(target);
                    for victim in victims {
                        remove_pid_from_futex_waiters(victim);
                        t.transfer_fd_table_owner_on_exit(victim);
                        if let Some(process) = t.get_mut(victim) {
                            if process.state != crate::sched::process::ProcessState::Zombie {
                                let closed = process.release_resources();
                                if !closed.is_empty() {
                                    closed_fds_pending.push((victim, closed));
                                }
                                posix_lock_release_pids.push(victim);
                                // 不在此处释放 address_space / kernel_stack：与
                                // `release_resources` 的不变量、SIGKILL 路径、以及 `exit()`
                                // 保持一致。提前归还内核栈会把仍可能被该任务内核执行流
                                // 引用的栈帧放回 buddy，被复用为用户页后遭内核栈帧覆写
                                // （内核栈 use-after-free）。用户内存由下面的 `mark_zombie`
                                // 走 `release_address_space_if_last_user`（对标 Linux mmput，
                                // 线程组安全）归还；内核栈在该任务确实离开 CPU、被 reap
                                // （`processes.remove` → PCB Drop）时才释放。
                            }
                        }
                        // sig 来自 SIGTERM(15) / SIGINT(2) 分支（line 746 上游守卫）
                        // 都不属 core-dumping signal；显式选 signaled() 默认 false。
                        t.mark_zombie(victim, crate::sched::process::ExitReason::signaled(sig));
                    }
                    if let Some(ppid) = parent_pid {
                        let should_wake = t.get(ppid).map_or(false, |parent| {
                            parent.state == crate::sched::process::ProcessState::Sleeping
                                && (parent.waiting_for == Some(target) || parent.waiting_for.is_none())
                        });
                        if should_wake {
                            t.mark_runnable(ppid, true);
                        }
                    }
                    t.wake_io_waiters();
                    drop(table);
                    for (pid, fds) in closed_fds_pending {
                        for fd in fds {
                            crate::fs::fd::finalize_closed_fd_for_pid(fd, pid);
                        }
                    }
                    for pid in posix_lock_release_pids {
                        crate::fs::file_lock::release_all_locks_for_pid(pid);
                        crate::sched::posix_timer::release_for_pid(pid);
                        crate::ipc::sysv::sem_undo_for_pid(pid);
                    }
                    return 0;
                }
            }
        } else {
            return -3;
        }
    }

    // 其他信号：投递到 pending_signals。POSIX kill(2) 的目标选择：
    //   pid >  0   → 单个进程
    //   pid == 0   → 调用者所在进程组（pgid）的所有成员
    //   pid == -1  → 除 init 之外、调用者有权投递的所有进程
    //   pid <  -1  → 进程组 |pid| 的所有成员
    let mut targets: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
    {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() {
            Some(t) => t,
            None => return -3,
        };
        // 调用者 PID namespace 作用域：进程组/广播(pid<=0)只能命中调用者所在
        // ns 及其后代的进程(Linux：信号目标按调用者 ns 的 vpid 收集)。扁平偏移
        // 模型下「在调用者 ns 内可见」⇔ 全局 ns(R==0) 或 候选全局 pid > R(祖先
        // 进程全局 pid<=R, 不可见)。pidns10/pidns17：容器 init 的 kill(-1)/kill(0)
        // 不得波及宿主 ns 的进程(否则会打死 LTP 主进程致 TBROK)。
        let caller_root = current_pid
            .and_then(|c| t.get(c))
            .map(|p| p.pidns_root)
            .unwrap_or(0);
        let caller_tgid = current_pid
            .and_then(|c| t.get(c).map(|p| if p.is_thread { p.tgid } else { c }))
            .unwrap_or(0);
        let visible = |cand_global: usize| caller_root == 0 || cand_global > caller_root;
        let ns_init_global = caller_root + 1;
        if pid > 0 {
            if t.get(pid as usize).is_none() {
                return -3;
            }
            targets.push(pid as usize);
        } else if pid == 0 || pid < -1 {
            let pgid_target = if pid == 0 {
                let cur = match current_pid {
                    Some(p) => p,
                    None => return -3,
                };
                match t.get(cur) {
                    Some(p) => p.pgid,
                    None => return -3,
                }
            } else {
                // pid<-1：|pid| 是调用者 ns 视图的 pgid，按偏移翻译回全局。
                ((-pid) as usize) + caller_root
            };
            for cand in t.pids() {
                if let Some(p) = t.get(cand) {
                    if p.pgid == pgid_target
                        && visible(cand)
                        && p.state != crate::sched::process::ProcessState::Zombie
                    {
                        targets.push(cand);
                    }
                }
            }
            if targets.is_empty() {
                return -3;
            }
        } else {
            // pid == -1：广播给除「调用者 ns init(vpid 1)」与「调用者自身线程组」
            // 之外、调用者 ns 内可见的所有非 zombie 进程(对标 Linux do_kill -1)。
            for cand in t.pids() {
                if cand == ns_init_global { continue; }
                if !visible(cand) { continue; }
                if let Some(p) = t.get(cand) {
                    let cand_tgid = if p.is_thread { p.tgid } else { cand };
                    if cand_tgid == caller_tgid { continue; }
                    if p.state != crate::sched::process::ProcessState::Zombie {
                        targets.push(cand);
                    }
                }
            }
            if targets.is_empty() {
                return -3;
            }
        }
    }

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        for target_pid in targets {
            deliver_pending_signal_locked(t, target_pid, sig, sender_pid, sender_uid, si_code, current_pid);
        }
        return 0;
    }
    -3 // -ESRCH
}

/// 生成信号投递的核心动作（须持 PROCESS_TABLE 锁）：ns-init 保护、siginfo
/// 入队（含 si_pid 按接收者 ns 翻译）、置 pending 位、必要时唤醒目标。
/// 由 do_kill 慢路径（组/广播）与单 pid 快路径共用。
fn deliver_pending_signal_locked(
    t: &mut crate::sched::process::ProcessTable,
    target_pid: usize,
    sig: i32,
    sender_pid: usize,
    sender_uid: u32,
    si_code: i32,
    current_pid: Option<usize>,
) {
    {
        {
            let should_wake = if let Some(process) = t.get_mut(target_pid) {
                // PID namespace init 信号保护(Linux SIGNAL_UNKILLABLE / sig_task_ignored)：
                // 子 ns 的 1 号进程(pidns_root!=0 且 tgid==pidns_root+1)对「采用默认
                // 处置(SIG_DFL)」的信号一律忽略 — 唯 SIGKILL/SIGSTOP 例外, 而二者
                // 在前面专路处理不会走到这里。但若该信号被目标阻塞(signal_mask)则
                // 不忽略, 仍入队留给 sigwait/sigtimedwait(pidns30 据此收 SIGUSR1)。
                // pidns17：容器 init 收到自身 ns 内的 SIGUSR1 不应被打死。
                let is_ns_init =
                    process.pidns_root != 0 && process.tgid == process.pidns_root + 1;
                let handler_default = process.signal_handlers[sig as usize - 1].handler == 0;
                let blocked = (process.signal_mask & (1u64 << (sig as u32 - 1))) != 0;
                if is_ns_init && handler_default && !blocked {
                    return;
                }
                if !process.pending_siginfo.iter().any(|(queued_sig, _)| *queued_sig == sig) {
                    // si_pid 必须按「接收者所在 PID namespace」翻译(Linux pid_vnr)：
                    // 发送者处于接收者祖先 ns(tgid<=接收者 pidns_root)则不可见→0,
                    // 否则按偏移翻译。全局 ns(root=0)为恒等, 不影响绝大多数路径。
                    // pidns12 据此校验跨 ns 信号的 si_pid==0。
                    let recv_root = process.pidns_root;
                    let si_pid_ns = if recv_root != 0 && sender_pid <= recv_root {
                        0
                    } else if recv_root != 0 {
                        sender_pid - recv_root
                    } else {
                        sender_pid
                    };
                    enqueue_pending_siginfo_process(
                        process,
                        sig,
                        build_si_siginfo(sig, si_pid_ns, sender_uid, si_code),
                    );
                }
                process.pending_signals |= 1u64 << (sig as u32 - 1); crate::sched::process::note_signal_hint();
                if crate::cmdline::scdbg_sig_enabled() && sig == 10 {
                    crate::println!(
                        "[sigdbg] deliver sig=10 target={} sender={} si_code={} state={:?} waiting_for={:?} mask={:#x} handler={:#x}",
                        target_pid, sender_pid, si_code, process.state, process.waiting_for,
                        process.signal_mask, process.signal_handlers[9].handler
                    );
                }
                let waiting_on_child = !process.children.is_empty()
                    && match process.waiting_for {
                        None => true,
                        Some(wait_pid) => process.children.contains(&wait_pid),
                    };
                // vfork 父只允许"子 execve/exit"唤醒（Linux vfork 等待对普通
                // 信号不可中断）；此处被普通信号唤醒会让父提前从 clone 返回并
                // munmap 仍被子进程使用的 posix_spawn 栈。
                let vfork_blocked = process.vfork_waiting_for.is_some();
                process.state == crate::sched::process::ProcessState::Sleeping
                    && !waiting_on_child
                    && !vfork_blocked
            } else {
                return;
            };
            if should_wake {
                let keep_signal_wait = t
                    .get(target_pid)
                    .map_or(false, |process| {
                        process.waiting_for == Some(crate::sched::process::SIGNAL_WAIT_SENTINEL)
                            || process.waiting_for == Some(crate::sched::process::NANOSLEEP_SENTINEL)
                    });
                t.mark_runnable(target_pid, !keep_signal_wait);
                if let Some(cur) = current_pid {
                    if cur != target_pid {
                        if let Some(current) = t.get_mut(cur) {
                            current.set_yield_to_child(target_pid);
                        }
                    }
                }
            }
        }
    }
}

/// sigreturn(139) — 从信号处理函数返回
/// 从用户栈上的 ucontext 恢复被中断的寄存器和信号掩码（handler 可能已修改它们）
#[allow(dead_code)]
pub(crate) fn sys_sigreturn() -> isize {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            // 确认当前确实在 signal handler 中
            if t.get(pid).map(|p| p.saved_trap_context.is_none()).unwrap_or(true) {
                return -22; // -EINVAL
            }

            #[cfg(target_arch = "riscv64")]
            const REG_SP: usize = 2;
            #[cfg(target_arch = "riscv64")]
            const REG_A0: usize = 10;
            #[cfg(target_arch = "loongarch64")]
            const REG_SP: usize = 3;
            #[cfg(target_arch = "loongarch64")]
            const REG_A0: usize = 4;

            // 从 trap context 取 sp，计算 ucontext 地址
            let (sp, as_pid) = {
                let p = t.get(pid).unwrap();
                let trap_cx = unsafe { &*(p.trap_context_ptr as *const crate::arch::TrapContext) };
                let sp = trap_cx.x[REG_SP];
                let as_pid = if p.mm.is_some() { pid } else { p.tgid };
                (sp, as_pid)
            };
            let ucontext_ptr = sp + SIGINFO_SIZE;

            // 从用户内存读取 uc_sigmask 和 uc_mcontext（handler 可能已修改）
            // Batch-read: 1-2 page translations instead of 33 individual ones.
            let restore_result: Option<(u64, [usize; 32])> = {
                let target = t.get(as_pid).unwrap();
                let mm = target.mm.as_ref().unwrap();
                let addr_space = mm.lock();
                if let Some(mask) = read_u64_from_user_as(&*addr_space, ucontext_ptr + 0x28) {
                    let mut regs = [0usize; 32];
                    #[cfg(target_arch = "loongarch64")]
                    {
                        let mut buf = [0u64; 33];
                        if read_u64_batch_from_user(&*addr_space, ucontext_ptr + 0xb0, &mut buf) {
                            regs[0] = buf[0] as usize; // pc
                            for i in 1..32usize {
                                regs[i] = buf[i + 1] as usize;
                            }
                            Some((mask, regs))
                        } else {
                            None
                        }
                    }
                    #[cfg(not(target_arch = "loongarch64"))]
                    {
                        let mut buf = [0u64; 32];
                        if read_u64_batch_from_user(&*addr_space, ucontext_ptr + 0xb0, &mut buf) {
                            for i in 0..32usize {
                                regs[i] = buf[i] as usize;
                            }
                            Some((mask, regs))
                        } else {
                            None
                        }
                    }
                } else {
                    None
                }
            };
            let (new_mask, new_regs) = match restore_result {
                Some(result) => result,
                None => {
                    drop(table);
                    // SIGSEGV(11) 是 core-dumping signal。
                    crate::sched::process::exit_with_reason(
                        crate::sched::process::ExitReason::signaled_with_dump(11),
                    );
                }
            };
            #[cfg(target_arch = "loongarch64")]
            let new_fpu = {
                let target = t.get(as_pid).unwrap();
                let mm = target.mm.as_ref().unwrap();
                let addr_space = mm.lock();
                match read_loongarch_fpu_signal_frame(&*addr_space, ucontext_ptr) {
                    Some(state) => state,
                    None => {
                        drop(addr_space);
                        drop(table);
                        crate::sched::process::exit_with_reason(
                            crate::sched::process::ExitReason::signaled_with_dump(11),
                        );
                    }
                }
            };
            if crate::cmdline::sigfrdbg_enabled() {
                if let Some(target) = t.get(as_pid) {
                    if let Some(ref mm) = target.mm {
                        let addr_space = mm.lock();
                        sigfrdbg_dump_sigreturn(pid, sp, ucontext_ptr, new_mask, &new_regs, &*addr_space);
                    }
                }
            }
            if crate::cmdline::execdbg_enabled() && crate::kernel::signal::sigdbg_budget_ok() {
                crate::println!(
                    "[EXECDBG] sigreturn pid={} pc={:#x} sp={:#x} mask={:#x}",
                    pid, new_regs[0], new_regs[REG_SP], new_mask
                );
            }
            if crate::cmdline::scdbg_sig_enabled() {
                #[cfg(target_arch = "riscv64")]
                {
                    crate::println!(
                        "[sigdbg] sigreturn pid={} pc={:#x} sp={:#x} ra={:#x} s0={:#x} s1={:#x} s2={:#x} a0={:#x} mask={:#x}",
                        pid,
                        new_regs[0],
                        new_regs[2],
                        new_regs[1],
                        new_regs[8],
                        new_regs[9],
                        new_regs[18],
                        new_regs[10],
                        new_mask,
                    );
                }
            }

            #[cfg(target_arch = "loongarch64")]
            crate::arch::loongarch64::fpu::restore_from_signal(pid, new_fpu);

            if let Some(process) = t.get_mut(pid) {
                process.signal_mask = new_mask;
                process.saved_trap_context.take();
                process.saved_signal_mask.take();
                process.signal_handler_sp = 0;
                process.signal_wait_trap_context.take();
                // handler 已返回：清 SS_ONSTACK（备用栈占用标记，Linux 同款）。
                // SS_DISABLE 若由 SS_AUTODISARM 置位则保留，后续信号走普通栈。
                process.sigaltstack_flags &= !1;

                if process.trap_context_ptr != 0 {
                    let trap_cx = unsafe {
                        &mut *(process.trap_context_ptr as *mut crate::arch::TrapContext)
                    };
                    trap_cx.sepc = new_regs[0]; // pc
                    for i in 1..32usize {
                        trap_cx.x[i] = new_regs[i];
                    }
                    trap_cx.x[0] = 0; // x0 = zero
                    return new_regs[REG_A0] as isize; // a0
                }
            }
        }
    }
    0
}

#[cfg(target_arch = "loongarch64")]
fn read_loongarch_fpu_signal_frame(
    addr_space: &crate::mm::AddressSpace,
    ucontext_ptr: usize,
) -> Option<alloc::boxed::Box<crate::arch::loongarch64::fpu::FpuState>> {
    use super::signal::{
        LOONGARCH_EXTCONTEXT_OFFSET, LOONGARCH_FPU_CTX_MAGIC,
        LOONGARCH_FPU_RECORD_SIZE, LOONGARCH_LASX_CTX_MAGIC,
        LOONGARCH_LASX_RECORD_SIZE, LOONGARCH_LSX_CTX_MAGIC,
        LOONGARCH_LSX_RECORD_SIZE, LOONGARCH_SC_FLAGS_OFFSET,
        LOONGARCH_SC_USED_FP,
    };

    let flags = read_u64_from_user_as(
        addr_space,
        ucontext_ptr + LOONGARCH_SC_FLAGS_OFFSET,
    )? as u32;
    let mut state = alloc::boxed::Box::new(
        crate::arch::loongarch64::fpu::FpuState::zeroed(),
    );
    if flags & LOONGARCH_SC_USED_FP == 0 {
        return Some(state);
    }

    let info = ucontext_ptr + LOONGARCH_EXTCONTEXT_OFFSET;
    let mut header = [0u64; 2];
    if !read_u64_batch_from_user(addr_space, info, &mut header) {
        return None;
    }
    let magic = header[0] as u32;
    let size = (header[0] >> 32) as usize;

    let register_words = match magic {
        LOONGARCH_FPU_CTX_MAGIC if size == LOONGARCH_FPU_RECORD_SIZE => 32,
        LOONGARCH_LSX_CTX_MAGIC if size == LOONGARCH_LSX_RECORD_SIZE => 64,
        LOONGARCH_LASX_CTX_MAGIC if size == LOONGARCH_LASX_RECORD_SIZE => 128,
        _ => return None,
    };

    match register_words {
        32 => {
            let mut regs = [0u64; 32];
            if !read_u64_batch_from_user(addr_space, info + 16, &mut regs) {
                return None;
            }
            for (reg, value) in regs.iter().enumerate() {
                state.regs[reg * 4] = *value;
            }
        }
        64 => {
            let mut regs = [0u64; 64];
            if !read_u64_batch_from_user(addr_space, info + 16, &mut regs) {
                return None;
            }
            for reg in 0..32 {
                state.regs[reg * 4] = regs[reg * 2];
                state.regs[reg * 4 + 1] = regs[reg * 2 + 1];
            }
        }
        128 => {
            if !read_u64_batch_from_user(addr_space, info + 16, &mut state.regs) {
                return None;
            }
        }
        _ => return None,
    }

    let mut tail = [0u64; 2];
    if !read_u64_batch_from_user(
        addr_space,
        info + 16 + register_words * 8,
        &mut tail,
    ) {
        return None;
    }
    state.fcc = tail[0];
    state.fcsr = tail[1] as u32;

    let mut end = [0u64; 2];
    if !read_u64_batch_from_user(addr_space, info + size, &mut end) || end[0] != 0 {
        return None;
    }
    Some(state)
}

pub(super) fn sigfrdbg_dump_words(
    label: &str,
    pid: usize,
    base: usize,
    addr_space: &crate::mm::AddressSpace,
) {
    for off in (0..256usize).step_by(8) {
        let addr = base + off;
        match read_u64_from_user_as(addr_space, addr) {
            Some(v) => crate::println!("[sigfrdbg] {} pid={} {:#x}: {:#018x}", label, pid, addr, v),
            None => crate::println!("[sigfrdbg] {} pid={} {:#x}: <unmapped>", label, pid, addr),
        }
    }
}

pub(super) fn sigfrdbg_dump_delivery(
    pid: usize,
    signum: usize,
    handler: usize,
    old_sp: usize,
    new_sp: usize,
    siginfo_ptr: usize,
    ucontext_ptr: usize,
    restorer: usize,
    saved_ctx: &[usize; 32],
    addr_space: &crate::mm::AddressSpace,
) {
    crate::println!(
        "[sigfrdbg] deliver pid={} sig={} handler={:#x} old_sp={:#x} new_sp={:#x} siginfo={:#x} uctx={:#x} restorer={:#x} saved_pc={:#x} saved_ra={:#x} saved_sp={:#x} saved_s0={:#x} saved_s1={:#x} saved_a0={:#x}",
        pid,
        signum,
        handler,
        old_sp,
        new_sp,
        siginfo_ptr,
        ucontext_ptr,
        restorer,
        saved_ctx[0],
        saved_ctx[1],
        saved_ctx[2],
        saved_ctx[8],
        saved_ctx[9],
        saved_ctx[10],
    );
    sigfrdbg_dump_words("old_sp", pid, old_sp.saturating_sub(32), addr_space);
    sigfrdbg_dump_words("new_sp", pid, new_sp, addr_space);
}

pub(super) fn sigfrdbg_dump_sigreturn(
    pid: usize,
    frame_sp: usize,
    ucontext_ptr: usize,
    new_mask: u64,
    new_regs: &[usize; 32],
    addr_space: &crate::mm::AddressSpace,
) {
    crate::println!(
        "[sigfrdbg] sigreturn pid={} frame_sp={:#x} uctx={:#x} mask={:#x} pc={:#x} ra={:#x} sp={:#x} s0={:#x} s1={:#x} a0={:#x}",
        pid,
        frame_sp,
        ucontext_ptr,
        new_mask,
        new_regs[0],
        new_regs[1],
        new_regs[2],
        new_regs[8],
        new_regs[9],
        new_regs[10],
    );
    sigfrdbg_dump_words("ret_sp", pid, new_regs[2].saturating_sub(32), addr_space);
}

/// 写入用户空间缓冲区
/// 向用户虚拟地址写 8 字节（需已持有 PROCESS_TABLE 锁，直接传入 address_space）。
/// 地址须 8 字节对齐且不跨页。
pub(super) fn write_u64_to_user_as(addr_space: &mut crate::mm::AddressSpace, user_va: usize, val: u64) {
    use crate::mm::{VirtAddr, PTEFlags};
    let va = VirtAddr(user_va);
    let vpn = va.floor();
    let offset = va.page_offset();
    if let Some(f) = addr_space.page_table().get_pte_flags(vpn) {
        if !f.contains(PTEFlags::W) {
            addr_space.handle_page_fault(va, true);
        }
    }
    if let Some(ppn) = addr_space.page_table().translate(vpn) {
        let kva = crate::syscall::ppn_offset_to_kernel_ptr(ppn, offset);
        unsafe { (kva as *mut u64).write_volatile(val); }
    }
}

/// 从用户虚拟地址读 8 字节（需已持有 PROCESS_TABLE 锁或只读访问 address_space）。
/// 地址须 8 字节对齐且不跨页。
pub(super) fn read_u64_from_user_as(addr_space: &crate::mm::AddressSpace, user_va: usize) -> Option<u64> {
    use crate::mm::VirtAddr;
    let va = VirtAddr(user_va);
    let vpn = va.floor();
    let offset = va.page_offset();
    if let Some(ppn) = addr_space.page_table().translate(vpn) {
        let kva = crate::syscall::ppn_offset_to_kernel_ptr(ppn, offset);
        Some(unsafe { (kva as *const u64).read_volatile() })
    } else {
        None
    }
}

/// Batch-write consecutive u64 values to user space, translating each page only once.
/// Assumes pages are already faulted in (call fault_in_user_write_range first).
pub(super) fn write_u64_batch_to_user(
    addr_space: &mut crate::mm::AddressSpace,
    base_va: usize,
    vals: &[u64],
) {
    use crate::mm::{VirtAddr, PAGE_SIZE};
    let mut i = 0;
    while i < vals.len() {
        let va = VirtAddr(base_va + i * 8);
        let vpn = va.floor();
        let page_off = va.page_offset();
        let slots_in_page = (PAGE_SIZE - page_off) / 8;
        let count = slots_in_page.min(vals.len() - i);
        if let Some(ppn) = addr_space.page_table().translate(vpn) {
            let base = crate::syscall::ppn_offset_to_kernel_ptr(ppn, page_off) as *mut u64;
            for j in 0..count {
                unsafe { base.add(j).write_volatile(vals[i + j]); }
            }
        }
        i += count;
    }
}

/// Batch-read consecutive u64 values from user space, translating each page only once.
pub(super) fn read_u64_batch_from_user(
    addr_space: &crate::mm::AddressSpace,
    base_va: usize,
    out: &mut [u64],
) -> bool {
    use crate::mm::{VirtAddr, PAGE_SIZE};
    let mut i = 0;
    while i < out.len() {
        let va = VirtAddr(base_va + i * 8);
        let vpn = va.floor();
        let page_off = va.page_offset();
        let slots_in_page = (PAGE_SIZE - page_off) / 8;
        let count = slots_in_page.min(out.len() - i);
        if let Some(ppn) = addr_space.page_table().translate(vpn) {
            let base = crate::syscall::ppn_offset_to_kernel_ptr(ppn, page_off) as *const u64;
            for j in 0..count {
                out[i + j] = unsafe { base.add(j).read_volatile() };
            }
        } else {
            return false;
        }
        i += count;
    }
    true
}

/// `siginfo_t` 上限 — 与 `sigaction(2)` ABI 一致（Linux 128 字节）。
// SIGINFO_BUF_LEN imported from super::signal

/// Slice 2.0：把用户态 128 字节 `siginfo_t` 拷到内核 buf。
///
/// 容许 `uinfo` 任意 8 字节对齐起点；按 16 个 u64 顺序读，整页之内即可
/// （`siginfo_t` 16 字节对齐，常落在单页内）。若任一 u64 翻译失败（页未
/// 映射或缺页），返回 None — 上层应回 `-EFAULT`。
pub(crate) fn copy_siginfo_from_user(
    addr_space: &crate::mm::AddressSpace,
    uinfo: usize,
) -> Option<[u8; SIGINFO_BUF_LEN]> {
    if uinfo == 0 || (uinfo & 0x7) != 0 {
        return None;
    }
    let mut buf = [0u8; SIGINFO_BUF_LEN];
    for i in 0..(SIGINFO_BUF_LEN / 8) {
        let q = read_u64_from_user_as(addr_space, uinfo + i * 8)?;
        buf[i * 8..(i + 1) * 8].copy_from_slice(&q.to_ne_bytes());
    }
    Some(buf)
}

/// Slice 2.0：把内核 buf 写回用户态 siginfo_t（128 字节）。
///
/// 调用方必须先 `fault_in_user_write_range(uinfo, 128)` 保证目标页已就绪。
pub(super) fn copy_siginfo_to_user(
    addr_space: &mut crate::mm::AddressSpace,
    uinfo: usize,
    buf: &[u8; SIGINFO_BUF_LEN],
) {
    if uinfo == 0 || (uinfo & 0x7) != 0 {
        return;
    }
    for i in 0..(SIGINFO_BUF_LEN / 8) {
        let q = u64::from_ne_bytes(buf[i * 8..(i + 1) * 8].try_into().unwrap());
        write_u64_to_user_as(addr_space, uinfo + i * 8, q);
    }
}

/// Slice 2.0：把 `(sig, siginfo)` 入队到 target PCB 的 `pending_siginfo`。
///
/// FIFO 限长 32：超出时丢弃最早项，仍允许新项进入，保证不漏 deliver。
/// 调用方必须已经持有 `PROCESS_TABLE` 锁。
pub(crate) fn enqueue_pending_siginfo_locked(
    table: &mut crate::sched::process::ProcessTable,
    target_pid: usize,
    sig: i32,
    info: [u8; SIGINFO_BUF_LEN],
) -> bool {
    const PENDING_SIGINFO_MAX: usize = 32;
    if let Some(process) = table.get_mut(target_pid) {
        if process.pending_siginfo.len() >= PENDING_SIGINFO_MAX {
            process.pending_siginfo.pop_front();
        }
        process.pending_siginfo.push_back((sig, info));
        true
    } else {
        false
    }
}

/// 真化〔seccomp SECCOMP_RET_TRAP → SIGSYS+siginfo_t〕：
/// 给当前进程入队一条 SIGSYS(31) siginfo blob，字段按 Linux uapi
/// asm-generic/siginfo.h `_sigsys` 子结构填：
///   si_signo  = 31       (offset 0..4 — 后续 deliver 时 +0 会被 signum
///                         u64 覆盖，但低 4 字节仍是 31，兼容)
///   si_errno  = data     (offset 8..12 — SECCOMP_RET_DATA 低 16 位)
///   si_code   = 1        (offset 12..16 — SYS_SECCOMP, Linux uapi)
///   si_call_addr = pc    (offset 16..24 — 触发 syscall 的用户指令地址)
///   si_syscall   = nr    (offset 24..28 — 触发的 syscall 号)
///   si_arch      = AUDIT_ARCH (offset 28..32 — RV64/LA64 区分)
/// 后续 `check_and_deliver_signals` 通过 `pending_siginfo` 复用通道把整 128B
/// 回写到 handler 收到的 siginfo_t*，handler 即可读 si_call_addr/si_syscall。
///
/// 不在此处投递信号，只入队并 set pending bit；真正切到 handler 仍由
/// trap-return 路径上的 `check_and_deliver_signals` 完成（与 rt_sigqueueinfo
/// 路径一致，保证 SA_SIGINFO 语义统一）。
pub(crate) fn enqueue_sigsys_for_current(nr: u64, pc: usize, data: u16) {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() { Some(t) => t, None => return };
    let pid = match t.current { Some(p) => p, None => return };
    let mut buf = [0u8; SIGINFO_BUF_LEN];
    buf[0..4].copy_from_slice(&31i32.to_ne_bytes());
    buf[8..12].copy_from_slice(&(data as i32).to_ne_bytes());
    buf[12..16].copy_from_slice(&1i32.to_ne_bytes());
    buf[16..24].copy_from_slice(&(pc as u64).to_ne_bytes());
    buf[24..28].copy_from_slice(&(nr as i32).to_ne_bytes());
    buf[28..32].copy_from_slice(&crate::seccomp::SECCOMP_AUDIT_ARCH.to_ne_bytes());
    enqueue_pending_siginfo_locked(t, pid, 31, buf);
    if let Some(p) = t.get_mut(pid) {
        p.pending_signals |= 1u64 << (31 - 1); crate::sched::process::note_signal_hint();
    }
}

/// tkill(tid, sig) — 向指定线程投信号。
/// Linux `do_tkill`：pid<=0 → EINVAL（tkill 不接受进程组/广播）；线程不存在
/// → ESRCH。sig 非法 → EINVAL。其余委托 sys_kill 投递到该 tid。
pub(crate) fn sys_tkill(tid: isize, sig: i32) -> isize {
    if tid <= 0 { return -22; }          // EINVAL：tkill 仅接受正 tid
    if sig < 0 || sig > 64 { return -22; } // EINVAL：非法信号
    {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        match table.as_ref() {
            Some(t) if t.get(tid as usize).is_some() => {}
            _ => return -3, // ESRCH
        }
    }
    // tkill/tgkill：si_code = SI_TKILL(-6)（Linux do_tkill），glibc 取消信号据此校验。
    do_kill(tid, sig, SI_TKILL)
}

/// tgkill(tgid, tid, sig) — 向指定线程组内的指定线程投信号。
/// Linux：tgid<=0 或 tid<=0 → EINVAL；tid 不存在或其 tgid 与传入 tgid 不符
/// → ESRCH；sig 非法 → EINVAL。
pub(crate) fn sys_tgkill(tgid: isize, tid: isize, sig: i32) -> isize {
    if tgid <= 0 || tid <= 0 { return -22; } // EINVAL
    if sig < 0 || sig > 64 { return -22; }   // EINVAL
    {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        match table.as_ref() {
            Some(t) => match t.get(tid as usize) {
                Some(target) if target.tgid == tgid as usize => {}
                _ => return -3, // ESRCH（不存在或线程组不匹配）
            },
            None => return -3,
        }
    }
    do_kill(tid, sig, SI_TKILL)
}

/// rt_sigqueueinfo(pid, sig, uinfo) — POSIX 信号附带 siginfo_t 投递。
///
/// Slice 2.0：从用户态拷 128 字节 `siginfo_t` → 入 target.pending_siginfo
/// 队列 → 走标准 `sys_kill` 投信号。后续 SA_SIGINFO handler 投递路径会
/// FIFO 弹出与 sig 匹配的最早 payload 写回 handler 收到的 `siginfo_t*`，
/// 还原 `si_value` / `si_pid` / `si_uid` / `si_code` 等真实字段。
///
/// 失败语义：
///   - 目标进程不存在 → 走 sys_kill 返回 -ESRCH（-3）。
///   - uinfo 指针非法（NULL / 未对齐 / 页未映射） → 返回 -EFAULT (-14)。
#[allow(dead_code)]
pub(crate) fn sys_rt_sigqueueinfo(pid: isize, sig: i32, uinfo: *const u8) -> isize {
    if pid <= 0 || sig <= 0 || sig as usize > 64 {
        return -22; // EINVAL
    }
    if uinfo.is_null() {
        return -14; // EFAULT
    }
    let info = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -3 };
        let current = match t.current { Some(c) => c, None => return -3 };
        let cur_pcb = match t.get(current) { Some(p) => p, None => return -3 };
        let mm = match cur_pcb.mm.as_ref() {
            Some(m) => m,
            None => return -14,
        };
        let addr_space = mm.lock();
        match copy_siginfo_from_user(&*addr_space, uinfo as usize) {
            Some(b) => b,
            None => return -14,
        }
    };
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            // 目标不存在则不入队，让 sys_kill 自己回 -ESRCH；存在但入队失败
            // 不影响信号语义（仍走默认 4×u64 填充）。
            let _ = enqueue_pending_siginfo_locked(t, pid as usize, sig, info);
        }
    }
    sys_kill(pid, sig)
}

/// rt_tgsigqueueinfo(tgid, tid, sig, uinfo) — 线程组 / 线程级 sigqueueinfo。
///
/// Slice 2.0：与 `sys_rt_sigqueueinfo` 同语义，目标是具体 `tid`（线程），并
/// 校验 `tid` 的 `tgid` 等于传入的 `tgid`（POSIX：tgkill 要求线程归属正确
/// 线程组，否则 -ESRCH）。
#[allow(dead_code)]
pub(crate) fn sys_rt_tgsigqueueinfo(tgid: isize, tid: isize, sig: i32, uinfo: *const u8) -> isize {
    if tgid <= 0 || tid <= 0 || sig <= 0 || sig as usize > 64 {
        return -22; // EINVAL
    }
    if uinfo.is_null() {
        return -14; // EFAULT
    }
    let info = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -3 };
        // 线程组校验
        match t.get(tid as usize) {
            Some(target) if target.tgid == tgid as usize => {}
            Some(_) | None => return -3, // ESRCH
        }
        let current = match t.current { Some(c) => c, None => return -3 };
        let cur_pcb = match t.get(current) { Some(p) => p, None => return -3 };
        let mm = match cur_pcb.mm.as_ref() {
            Some(m) => m,
            None => return -14,
        };
        let addr_space = mm.lock();
        match copy_siginfo_from_user(&*addr_space, uinfo as usize) {
            Some(b) => b,
            None => return -14,
        }
    };
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            let _ = enqueue_pending_siginfo_locked(t, tid as usize, sig, info);
        }
    }
    sys_kill(tid, sig)
}

/// POSIX mq_notify 信号投递（对标 Linux ipc/mqueue.c `__do_notify` →
/// `do_send_sig_info` with `SEND_SIG_PRIV`/`info.si_code = SI_MESGQ`）。
///
/// 与 `sys_kill` 的区别（pidns30/pidns31 据此校验）：
/// - `si_code = SI_MESGQ(-3)`，而非 kill 的 `SI_USER(0)`；
/// - `si_pid` = 触发通知的消息发送者(current)的 tgid，**翻译到接收者所在 PID
///   namespace 的可见值**：发送者若处于接收者的祖先 ns（`tgid <= 接收者
///   pidns_root`）则不可见，按 `pid_vnr` 返回 0；否则按偏移翻译。
/// - `si_uid` = 发送者 uid。
///
/// 实现沿用 `sys_rt_sigqueueinfo` 的「先入队 siginfo，再 sys_kill 置 pending+
/// 唤醒」通道，保证 SA_SIGINFO handler 能读回正确的 si_code/si_pid/si_uid。
pub(crate) fn deliver_mq_notify_signal(target_pid: usize, signo: i32) {
    const SI_MESGQ: i32 = -3;
    if signo <= 0 || signo >= 64 {
        return;
    }
    let (sender_tgid, sender_uid, recv_root) = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return };
        let cur = t.current;
        let (stgid, suid) = match cur.and_then(|c| t.get(c).map(|p| (c, p))) {
            Some((c, p)) => (if p.is_thread { p.tgid } else { c }, p.uid),
            None => (0usize, 0u32),
        };
        let root = t.get(target_pid).map(|p| p.pidns_root).unwrap_or(0);
        (stgid, suid, root)
    };
    // pid_vnr：把发送者 tgid 翻译到接收者 ns 的可见值（祖先 ns 不可见 → 0）。
    let si_pid = if recv_root != 0 && sender_tgid <= recv_root {
        0
    } else if recv_root != 0 {
        sender_tgid - recv_root
    } else {
        sender_tgid
    };
    let info = crate::kernel::signal::build_si_siginfo(signo, si_pid, sender_uid, SI_MESGQ);
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            let _ = enqueue_pending_siginfo_locked(t, target_pid, signo, info);
        }
    }
    sys_kill(target_pid as isize, signo);
}

/// sigaltstack(132) — get/set alternate signal stack.
///
/// Linux semantics:
/// - ss_ptr  → if non-NULL, set new alternate signal stack
/// - oss_ptr → if non-NULL, return old alternate signal stack
/// - Flags: SS_DISABLE (2), SS_ONSTACK (1), SS_AUTODISARM (0x80000000)
/// - Cannot change stack while handler is running on it (EPERM)
/// - SS_DISABLE disables the alternate stack
/// - size must be >= MINSIGSTKSZ (2048) unless SS_DISABLE
pub(crate) fn sys_sigaltstack(ss_ptr: usize, oss_ptr: usize) -> isize {
    const SS_ONSTACK: u32 = 1;
    const SS_DISABLE: u32 = 2;
    const SS_AUTODISARM: u32 = 1u32 << 31;
    const SS_FLAG_BITS: u32 = SS_ONSTACK | SS_DISABLE | SS_AUTODISARM;
    const MINSIGSTKSZ: usize = 2048;

    // stack_t on LP64: ss_sp(8) + ss_flags(4) + pad(4) + ss_size(8) = 24 bytes
    const STACK_T_SIZE: usize = 24;

    let pid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -22 };
        match t.current { Some(p) => p, None => return -22 }
    };

    // Return old stack if requested
    if oss_ptr != 0 {
        let (sp, flags, size) = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_ref() { Some(t) => t, None => return -22 };
            let p = match t.get(pid) { Some(p) => p, None => return -22 };
            (p.sigaltstack_sp, p.sigaltstack_flags, p.sigaltstack_size)
        };
        let mut buf = [0u8; STACK_T_SIZE];
        buf[0..8].copy_from_slice(&(sp as u64).to_le_bytes());
        buf[8..12].copy_from_slice(&flags.to_le_bytes());
        buf[16..24].copy_from_slice(&(size as u64).to_le_bytes());
        if crate::syscall::user::UserSlice::<u8>::new_mut(oss_ptr as *mut u8, STACK_T_SIZE)
            .write_bytes(&buf)
            .is_err()
        {
            return -14; // EFAULT
        }
    }

    // Set new stack if requested
    if ss_ptr != 0 {
        // Check if currently executing on alt stack
        {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_ref() { Some(t) => t, None => return -22 };
            let p = match t.get(pid) { Some(p) => p, None => return -22 };
            if p.sigaltstack_flags & SS_ONSTACK != 0 {
                return -1; // EPERM
            }
        }

        let buf = match crate::syscall::user::UserSlice::<u8>::new(ss_ptr as *const u8, STACK_T_SIZE).read_bytes() {
            Ok(b) => b,
            Err(_) => return -14, // EFAULT
        };
        let new_sp = u64::from_le_bytes(buf[0..8].try_into().unwrap()) as usize;
        let new_flags = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let new_size = u64::from_le_bytes(buf[16..24].try_into().unwrap()) as usize;

        // Validate flags
        if new_flags & !SS_FLAG_BITS != 0 {
            return -22; // EINVAL
        }

        if new_flags & SS_DISABLE != 0 {
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_mut() {
                if let Some(p) = t.get_mut(pid) {
                    p.sigaltstack_sp = 0;
                    p.sigaltstack_size = 0;
                    p.sigaltstack_flags = SS_DISABLE;
                }
            }
            return 0;
        }

        // Validate size
        if new_size < MINSIGSTKSZ {
            return -12; // ENOMEM
        }

        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            if let Some(p) = t.get_mut(pid) {
                p.sigaltstack_sp = new_sp;
                p.sigaltstack_size = new_size;
                p.sigaltstack_flags = new_flags & !SS_ONSTACK;
            }
        }
        return 0;
    }

    0
}
