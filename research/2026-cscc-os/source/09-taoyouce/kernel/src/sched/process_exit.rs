//! Process exit, wait, and reaping lifecycle.
//!
//! Extracted from `process.rs` to align with Linux `kernel/exit.c`.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;
use crate::mm::{PhysAddr, VirtAddr};
use super::process::{
    ProcessControlBlock, ProcessTable, PROCESS_TABLE,
    Pid, INVALID_PID, ProcessState, ExitReason,
    IO_WAITER_COUNT, SIGNAL_WAIT_SENTINEL, IO_WAIT_SENTINEL,
    NANOSLEEP_SENTINEL, DEADLINE_RETURN_SENTINEL, FUTEX_WOKEN_SENTINEL,
    POLL_WAIT_SENTINEL, MQ_SLEEP_SENTINEL, MQ_WOKEN_SENTINEL,
    LOCK_WAIT_SENTINEL, SignalAction,
    maybe_print_heap_snapshot,
    clear_sigchld_if_no_waitable_status,
    restore_wait_trap_context,
    sigchld_wakes_signal_wait,
    sigchld_wakes_signal_wait_sig,
    is_valid_user_ptr,
    set_process_state,
};
use super::process_sched::{
    prepare_trap_context_for_user_return,
    enter_wait_channel, leave_wait_channel, note_waitqueue_wake,
    wait_channel_for, account_reap_child,
};
use super::process_wait::{
    build_waitid_siginfo, wait_status_from_continued,
    wait_status_from_exit_reason, wait_status_from_stopped_signal,
    write_user_bytes, WaitidEvent,
};
use super::process_acct::{acct_snapshot, write_process_accounting, ProcessAcctSnapshot};
use super::process_trace::{wtrace_process, wtrace_trap};

#[inline(always)]
fn dbg_sp_exit() -> usize {
    let sp: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("move {}, $sp", out(reg) sp, options(nomem, nostack)); }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    { sp = 0; }
    sp
}

/// [DEBUG] 在 processes.remove 前探测：若被移除进程的内核栈包含当前 sp，
/// 说明我们正运行在它的栈上却要释放它 = UAF 根因。打印 site/pid/current。
fn dbg_reap_probe(table: &ProcessTable, pid: Pid, site: &str) {
    let sp = dbg_sp_exit();
    if let Some(p) = table.processes.get(&pid) {
        if p.kernel_stack_contains(sp) {
            crate::println!(
                "[REAP-SITE] {} remove_pid={} current={:?} is_thread={} state={:?} sp={:#x} — RUNNING ON REMOVED STACK",
                site, pid, table.current, p.is_thread, p.state, sp,
            );
        }
    }
}

fn reap_detached_zombies_locked(table: &mut ProcessTable) {
    // Linux releases an auto-reaped task lazily through `release_task` once the
    // dying task has scheduled away — it never walks the whole task list on the
    // trap path.  We mirror that with an explicit work queue: every zombie that
    // no `wait4` can ever collect (a thread, which is never linked into a
    // parent's `children` list, or a child whose parent ignores SIGCHLD and was
    // therefore detached in `exit()`) is enqueued in `mark_zombie` / the
    // auto-reap branch and drained here in O(k).
    //
    // The previous formulation rescanned the entire `processes` map on every
    // user-mode trap (an O(N) `has_zombie` probe, and originally an O(N²)
    // reference check).  Once a process spawned thousands of unjoined threads
    // (libc-bench `b_pthread_create_serial1` creates 2500) that per-trap scan
    // livelocked the kernel — gdb caught the spin sitting in this exact
    // `BTreeMap` walk — so init's `waitpid` never returned and the platform
    // killed the run as “运行时间过长”, zeroing every later test group.
    if table.detached_zombies.is_empty() {
        super::process::DETACHED_ZOMBIES_PENDING.store(false, Ordering::Release);
        return;
    }
    let pending = core::mem::take(&mut table.detached_zombies);
    for pid in pending {
        if Some(pid) == table.current
            // SMP：另一个 hart 也可能正站在这个 zombie 的内核栈上（它刚在那边
            // 退出、还没切到自己的 idle 栈）。只比对本 hart 的 current 会把仍在
            // 使用的栈还给 buddy → freed-while-mapped。
            || super::process_sched::kstack_in_use_by_any_hart(pid)
        {
            // Never free a task while any CPU still executes on its kernel
            // stack; revisit it on a later trap.
            table.detached_zombies.push(pid);
            continue;
        }
        // A pid may already be gone if `wait4` raced us to it; PIDs are never
        // reused within a run, so a still-present entry is guaranteed to be the
        // same zombie we enqueued.
        let is_zombie = matches!(
            table.processes.get(&pid),
            Some(process) if process.state == ProcessState::Zombie
        );
        if is_zombie {
            table.reap_or_defer(pid);
        }
    }
    // 排空后按实际剩余（被跳过重新入队的 current）重置提示标志。
    super::process::DETACHED_ZOMBIES_PENDING.store(
        !table.detached_zombies.is_empty(),
        Ordering::Release,
    );
}

pub(crate) fn reparent_children_locked(
    table: &mut ProcessTable,
    children: Vec<Pid>,
    parent_pid: Option<Pid>,
) {
    if children.is_empty() {
        return;
    }
    let mut reaper_pid = 1;
    let mut ancestor = parent_pid;
    while let Some(pid) = ancestor {
        match table.get(pid) {
            Some(process) => {
                if process.state != ProcessState::Zombie && process.child_subreaper {
                    reaper_pid = pid;
                    break;
                }
                ancestor = process.parent_pid;
            }
            None => break,
        }
    }
    // 重定向到 init(PID1) 的已死孤儿直接交给延迟回收队列（对标 Linux init
    // 的 reaper 职责，等价于 init 立刻 wait 掉它们）：init 阻塞在指定 pid 的
    // wait4 里时收不到「转来的存量僵尸」，这些僵尸各自持有整套页表结构页，
    // cve-2017-17052 一轮可积累 ~60 个巨型地址空间僵尸（≈550MB），拖垮同组
    // 后续所有用例。子进程收割者（PR_SET_CHILD_SUBREAPER）语义不变——用户态
    // subreaper 有权 wait 这些僵尸，只有 init 走自动回收。
    for &child_pid in &children {
        if reaper_pid == 1
            && table
                .get(child_pid)
                .map_or(false, |c| c.state == ProcessState::Zombie)
        {
            if let Some(child) = table.get_mut(child_pid) {
                child.parent_pid = None;
            }
            table.detached_zombies.push(child_pid);
            super::process::DETACHED_ZOMBIES_PENDING.store(true, Ordering::Release);
            continue;
        }
        if let Some(child) = table.get_mut(child_pid) {
            child.parent_pid = Some(reaper_pid);
        }
        if let Some(reaper) = table.get_mut(reaper_pid) {
            if !reaper.children.contains(&child_pid) {
                reaper.children.push(child_pid);
            }
        }
    }
}

pub fn reap_detached_zombies() {
    // 热路径 lock-free 短路：队列空（绝大多数 trap）⇒ 无需获取 PROCESS_TABLE
    // 全局锁（§1.13① 减每 trap 一次锁/指令）。
    if !super::process::DETACHED_ZOMBIES_PENDING.load(Ordering::Acquire) {
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    if let Some(table) = guard.as_mut() {
        reap_detached_zombies_locked(table);
    }
}

pub fn exit(status: i32) -> ! {
    use crate::arch::usertrapret;
    use crate::arch::TrapContext;

    // 进程退出 = 延迟 inode 析构队列的批量回收点（对标 deferred iput）。
    crate::fs::inode_reap_drain();

    let is_init_process;
    let parent_to_resume: Option<(Pid, Pid, bool)>; // (parent_pid, current_pid, complete_waitpid)
    // (pid, vec<Arc<FileDescriptor>>) tuples whose POSIX locks / OFD locks /
    // inotify CLOSE events / pipe-socket peer wakeups must run AFTER we drop
    // PROCESS_TABLE — the file_lock and close_notify_last paths need to take
    // PROCESS_TABLE themselves to wake blocked lock waiters / readers /
    // writers, and a nested re-acquire would deadlock.
    let mut closed_fds_pending: Vec<(Pid, Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>)> = Vec::new();
    // pids whose POSIX advisory locks should be released in the post-lock
    // cleanup phase. Mirrors `closed_fds_pending` but is a separate list
    // because a single process may have already had `release_resources`
    // collapse its fd_table while we still want to flush stray POSIX locks
    // it acquired (e.g. via a lock-only fd that was duped or moved).
    let mut posix_lock_release_pids: Vec<Pid> = Vec::new();
    // 退出时被 detach 的 SHM shmid 列表（锁外做 nattch-- + maybe_reap）。
    let mut detached_shm_pending: Vec<i32> = Vec::new();
    let mut acct_pending: Option<ProcessAcctSnapshot> = None;

    {
        let mut table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_mut() {
            if let Some(current_pid) = table.current {
                // crate::println!("[EXIT] pid={} status={}", current_pid, status);
                // 检查是否是 init 进程
                is_init_process = current_pid == 1;

                // 收集需要的信息，避免借用冲突
                // --- Robust futex cleanup: walk robust list before releasing resources ---
                {
                    use crate::mm::VirtAddr;
                    const FUTEX_OWNER_DIED: u32 = 0x40000000;
                    const FUTEX_TID_MASK: u32 = 0x3fffffff;
                    const ROBUST_LIST_LIMIT: usize = 2048;

                    let (robust_head, as_pid) = {
                        if let Some(p) = table.get(current_pid) {
                            let asid = if p.mm.is_some() { current_pid } else { p.tgid };
                            (p.robust_list_head, asid)
                        } else { (0, 0) }
                    };

                    if robust_head != 0 {
                        // Helper closures for physical memory access (through mm.lock())
                        // 未建映射的合法懒页（fork 懒建子 PTE / 懒分配）先走缺页补页再重试。
                        let translate_or_fault = |addr_space: &mut crate::mm::AddressSpace,
                                                  va: usize,
                                                  write: bool|
                         -> Option<crate::mm::PhysPageNum> {
                            let vpn = VirtAddr(va).floor();
                            match addr_space.page_table().translate(vpn) {
                                Some(ppn) if !write => Some(ppn),
                                _ => {
                                    addr_space.handle_page_fault(VirtAddr(va), write);
                                    addr_space.page_table().translate(vpn)
                                }
                            }
                        };
                        let read_usize_va = |va: usize| -> Option<usize> {
                            let target = table.get(as_pid)?;
                            let mut addr_space = target.address_space_lock()?;
                            let off = VirtAddr(va).page_offset();
                            let ppn = translate_or_fault(&mut addr_space, va, false)?;
                            let kva = crate::mm::phys_to_kernel_vaddr((ppn.0 << 12) | off);
                            Some(unsafe { *(kva as *const usize) })
                        };
                        let read_i64_va = |va: usize| -> Option<i64> {
                            let target = table.get(as_pid)?;
                            let mut addr_space = target.address_space_lock()?;
                            let off = VirtAddr(va).page_offset();
                            let ppn = translate_or_fault(&mut addr_space, va, false)?;
                            let kva = crate::mm::phys_to_kernel_vaddr((ppn.0 << 12) | off);
                            Some(unsafe { *(kva as *const i64) })
                        };
                        let read_u32_va = |va: usize| -> Option<u32> {
                            let target = table.get(as_pid)?;
                            let mut addr_space = target.address_space_lock()?;
                            let off = VirtAddr(va).page_offset();
                            let ppn = translate_or_fault(&mut addr_space, va, false)?;
                            let kva = crate::mm::phys_to_kernel_vaddr((ppn.0 << 12) | off);
                            Some(unsafe { *(kva as *const u32) })
                        };
                        let write_u32_va = |va: usize, v: u32| -> Option<()> {
                            let target = table.get(as_pid)?;
                            let mut addr_space = target.address_space_lock()?;
                            let off = VirtAddr(va).page_offset();
                            let ppn = translate_or_fault(&mut addr_space, va, true)?;
                            let kva = crate::mm::phys_to_kernel_vaddr((ppn.0 << 12) | off);
                            unsafe { *(kva as *mut u32) = v; }
                            Some(())
                        };

                        // robust_list_head: { list.next(8B), futex_offset(8B), list_op_pending(8B) }
                        let head_next = read_usize_va(robust_head).unwrap_or(0);
                        let futex_offset = read_i64_va(robust_head + 8).unwrap_or(0);
                        let pending = read_usize_va(robust_head + 16).unwrap_or(0);

                        let mut futex_addrs = alloc::vec::Vec::new();
                        let tid_val = current_pid as u32;

                        // Process pending entry
                        if pending != 0 {
                            let fa = (pending as i64 + futex_offset) as usize;
                            if let Some(word) = read_u32_va(fa) {
                                if (word & FUTEX_TID_MASK) == tid_val {
                                    let _ = write_u32_va(fa, (word & !FUTEX_TID_MASK) | FUTEX_OWNER_DIED);
                                    futex_addrs.push(fa);
                                }
                            }
                        }

                        // Walk linked list
                        let mut entry = head_next;
                        let mut count = 0usize;
                        while entry != robust_head && entry != 0 && count < ROBUST_LIST_LIMIT {
                            count += 1;
                            if entry != pending {
                                let fa = (entry as i64 + futex_offset) as usize;
                                if let Some(word) = read_u32_va(fa) {
                                    if (word & FUTEX_TID_MASK) == tid_val {
                                        let _ = write_u32_va(fa, (word & !FUTEX_TID_MASK) | FUTEX_OWNER_DIED);
                                        futex_addrs.push(fa);
                                    }
                                }
                            }
                            entry = read_usize_va(entry).unwrap_or(0);
                        }

                        // Wake one waiter per futex
                        let pids_to_wake: alloc::vec::Vec<Pid> = {
                            let mut waiters = crate::syscall::FUTEX_WAITERS.lock();
                            let mut pids = alloc::vec::Vec::new();
                            for addr in &futex_addrs {
                                if let Some(queue) = waiters.get(addr) {
                                    if let Some(entry) = queue.dequeue_one() {
                                        pids.push(entry.pid);
                                    }
                                    if queue.is_empty() { waiters.remove(addr); }
                                }
                            }
                            pids
                        };
                        // 与其余 futex 唤醒点同一契约：出队即无条件落唤醒标记，
                        // 否则「已入队、尚未 Sleeping」的等待者会被摘走且永不唤醒。
                        for wpid in pids_to_wake {
                            let state = match table.get(wpid) {
                                Some(p) => p.state,
                                None => continue,
                            };
                            if matches!(state, ProcessState::Zombie | ProcessState::Unused) {
                                continue;
                            }
                            if let Some(p) = table.get_mut(wpid) {
                                p.waiting_for =
                                    Some(crate::sched::process::FUTEX_WOKEN_SENTINEL);
                                p.wakeup_channel = 0;
                                p.waitqueue_woken = true;
                            }
                            if state == ProcessState::Sleeping {
                                table.mark_runnable(wpid, true);
                            }
                        }
                    }
                }
                // --- End robust futex cleanup ---

                let now = crate::arch::asm_helpers::rdtime();
                let (children, parent_pid, _fd_count, is_thread, release_current_resources) = {
                    if let Some(process) = table.get_mut(current_pid) {
                        crate::syscall::remove_pid_from_futex_waiters(current_pid);
                        if !process.is_thread {
                            acct_pending = Some(acct_snapshot(process, ExitReason::Normal(status), now));
                            // 在 mm 销毁前结算 ru_maxrss 高水位（present 页统计）。
                            let rss = process.compute_rss_kb();
                            process.maxrss_kb = process.maxrss_kb.max(rss);
                        }

                        // 记录文件描述符数量
                        let fd_count = process.fd_table.iter().filter(|fd| fd.is_some()).count();

                        // 3. 释放进程资源
                        let children = process.children.clone();
                        let parent_pid = process.parent_pid;
                        let is_thread = process.is_thread;
                        // Linux `do_exit` → `exit_shm` ：退出时对每个 attach
                        // 的 SHM 段 nattch--，顺带累计被标记销毁的段 →锁
                        // 外调 helper 真释放。thread (is_thread=true) 共享 leader
                        // 的 mm，不在这里 detach (leader 退出时才算)。
                        let detached_shm_ids: Vec<i32> = if is_thread {
                            Vec::new()
                        } else {
                            process.shm_attachments.iter().map(|&(id, _)| id).collect()
                        };
                        if !is_thread { process.shm_attachments.clear(); }
                        if !detached_shm_ids.is_empty() {
                            detached_shm_pending.extend(detached_shm_ids);
                        }

                        (children, parent_pid, fd_count, is_thread, true)
                    } else {
                        (Vec::new(), None, 0, false, false)
                    }
                };
                if release_current_resources {
                    table.transfer_fd_table_owner_on_exit(current_pid);
                    if let Some(process) = table.get_mut(current_pid) {
                        let closed = process.release_resources();
                        if !closed.is_empty() {
                            closed_fds_pending.push((current_pid, closed));
                        }
                    }
                    // 进程的 POSIX 锁（同 pid）总要在退出时清空，不管 fd 是否被关。
                    posix_lock_release_pids.push(current_pid);
                }
                table.mark_zombie(current_pid, ExitReason::Normal(status));

                reparent_children_locked(table, children, parent_pid);

                // CLONE_VFORK: a parent blocked on this child is sleeping inside
                // clone(), not in wait4 — resume it at the clone return site and
                // leave the zombie for its real wait().
                if let Some(parent_pid) = parent_pid {
                    if let Some(parent) = table.get_mut(parent_pid) {
                        if parent.vfork_waiting_for == Some(current_pid) {
                            parent.vfork_waiting_for = None;
                            if parent.waiting_for
                                == Some(crate::sched::process::VFORK_WAIT_SENTINEL)
                            {
                                parent.waiting_for = None;
                                parent.wait_channel = None;
                            }
                            if parent.state == ProcessState::Sleeping {
                                set_process_state(parent, ProcessState::Runnable);
                                if crate::cmdline::bscrash_enabled() {
                                    crate::println!(
                                        "[VFORK] wake-exit parent={} child={}",
                                        parent_pid, current_pid
                                    );
                                }
                                table.mark_runnable(parent_pid, false);
                            }
                        }
                    }
                }

                // 5. 唤醒等待的父进程（线程不通知父进程，由 reap_detached 回收）
                let auto_reap_by_sigchld = if !is_thread {
                    if let Some(parent_pid) = parent_pid {
                        if let Some(parent) = table.get(parent_pid) {
                            let action = parent.signal_handlers[17 - 1];
                            action.handler == 1 || (action.flags & 2) != 0
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                };

                if auto_reap_by_sigchld {
                    if let Some(parent_pid) = parent_pid {
                        if let Some(parent) = table.get_mut(parent_pid) {
                            parent.children.retain(|&p| p != current_pid);
                        }
                    }
                    if let Some(child) = table.get_mut(current_pid) {
                        child.parent_pid = None;
                    }
                    // Now detached (dropped from the parent's `children` and
                    // orphaned), so no `wait4` can reap it — hand it to the
                    // deferred reaper, matching the thread path in `mark_zombie`.
                    table.detached_zombies.push(current_pid);
                    super::process::DETACHED_ZOMBIES_PENDING.store(true, Ordering::Release);
                }

                // Slice 2.0：移除原 `if !is_thread` 守卫。线程 child exit 也
                // 需要尝试唤醒 parent 的阻塞 `__WALL` / `__WCLONE` waiter
                // (`wait_thread_mode == CloneOnly | All`)。SIGCHLD pending
                // 仍仅对非线程 child 设置（POSIX 行为：线程退出不通知父进程
                // SIGCHLD，线程组的 SIGCHLD 由 tgid leader 退出时统一投递）。
                parent_to_resume = if !auto_reap_by_sigchld {
                    if let Some(parent_pid) = parent_pid {
                        // child 此时已被 mark_zombie，仍在 processes 中，可读 pgid。
                        let child_pgid = table.get(current_pid).map(|c| c.pgid).unwrap_or(0);
                        let child_exit_signal = table.get(current_pid).map(|c| c.exit_signal).unwrap_or(17);
                        // Linux 语义：SIGCHLD 携带 CLD_* siginfo（si_code=
                        // CLD_EXITED/KILLED/DUMPED、si_pid、si_status）。
                        // util-linux `script`(pty-session.c) 从 signalfd 读出
                        // ssi_code 并只对 CLD_EXITED/KILLED/DUMPED 触发
                        // wait+退出；缺 payload 时 si_code=0 会被其忽略。
                        let child_cld_info: Option<(i32, [u8; 128])> = table
                            .get(current_pid)
                            .map(|c| {
                                let (code, status) = match c
                                    .exit_status
                                    .unwrap_or(ExitReason::Normal(status))
                                {
                                    ExitReason::Normal(code) => (1i32, code), // CLD_EXITED
                                    ExitReason::Signaled { sig, core_dumped } => {
                                        (if core_dumped { 3 } else { 2 }, sig & 0x7f)
                                    }
                                };
                                let mut buf = [0u8; 128];
                                buf[0..4].copy_from_slice(&(child_exit_signal as i32).to_ne_bytes());
                                buf[8..12].copy_from_slice(&code.to_ne_bytes());
                                buf[16..20].copy_from_slice(&(current_pid as i32).to_ne_bytes());
                                buf[20..24].copy_from_slice(&c.uid.to_ne_bytes());
                                buf[24..28].copy_from_slice(&status.to_ne_bytes());
                                (child_exit_signal as i32, buf)
                            });
                        let wake = if let Some(parent) = table.get_mut(parent_pid) {
                            // Use child's exit_signal (set by clone/clone3) instead of hardcoded SIGCHLD.
                            if !is_thread && child_exit_signal > 0 && child_exit_signal <= 64 {
                                // Linux sig_task_ignored(): a SIG_DFL disposition
                                // for a kernel-default-ignore signal (SIGCHLD/
                                // SIGURG/SIGWINCH), or SIG_IGN, means the signal
                                // is discarded at generation time and never
                                // queued — UNLESS it is currently blocked (a
                                // blocked signal is still queued so it can be
                                // delivered after sigprocmask unblocks it).
                                // Without this guard a SIG_DFL SIGCHLD from a
                                // reaped vfork child (e.g. tst_cmd's mke2fs) leaks
                                // into pending_signals and is delivered once the
                                // test restores its real handler, tripping LTP
                                // tst_sig "unexpected signal" TBROK.
                                let bit = 1u64 << (child_exit_signal as u64 - 1);
                                let h = parent.signal_handlers[(child_exit_signal - 1) as usize].handler;
                                let blocked = (parent.signal_mask & bit) != 0;
                                let default_ignore = child_exit_signal == 17
                                    || child_exit_signal == 23
                                    || child_exit_signal == 28;
                                let ignored = h == 1 || (h == 0 && default_ignore);
                                if blocked || !ignored {
                                    parent.pending_signals |= bit; crate::sched::process::note_signal_hint();
                                    if let Some((sig, buf)) = child_cld_info {
                                        const PENDING_SIGINFO_MAX: usize = 32;
                                        if parent.pending_siginfo.len() >= PENDING_SIGINFO_MAX {
                                            parent.pending_siginfo.pop_front();
                                        }
                                        parent.pending_siginfo.push_back((sig, buf));
                                    }
                                }
                            }
                            // 三种 sleep 模式互斥分流：
                            //   - pgid 模式：wait_pgid.is_some()，waiting_for
                            //     强制 None（由 wait()/wait_for_waitid 写入）。
                            //     只在 child.pgid == wait_pgid 时唤醒。
                            //   - waitpid 模式：wait_pgid.is_none()，
                            //     waiting_for None=任意 / Some(pid)=指定。
                            //   - 纯 signal sleep（SIGNAL_WAIT_SENTINEL）：
                            //     与 wait 互斥，单独路径。
                            // 叠加 wait_thread_mode 过滤——这是 Slice 2.0
                            // 让线程 exit 唤醒 __WALL/__WCLONE waiter 的核心：
                            //   - NonThreadOnly(默认) 只接受非线程 child
                            //   - CloneOnly(__WCLONE)  只接受线程 child
                            //   - All(__WALL)          全收
                            let thread_mode_accepts = match parent.wait_thread_mode {
                                None | Some(ThreadMode::NonThreadOnly) => !is_thread,
                                Some(ThreadMode::CloneOnly) => is_thread,
                                Some(ThreadMode::All) => true,
                            };
                            let target_match = if parent.wait_pgid.is_some() {
                                parent.wait_pgid == Some(child_pgid)
                            } else {
                                parent.waiting_for == Some(current_pid)
                                    || parent.waiting_for.is_none()
                            };
                            // vfork 阻塞的父进程睡在 clone() 里而不是 wait4 里，
                            // 不得被当成 wait4 睡眠者去完成一次从未发起的 wait。
                            let any_wait_wake = thread_mode_accepts
                                && target_match
                                && parent.vfork_waiting_for.is_none();
                            // SIGNAL_WAIT_SENTINEL 仅在 SIGCHLD 真投递时
                            // 有意义；线程 child 不投 SIGCHLD，所以 signal
                            // sleep waiter 也不应被线程 exit 唤醒。
                            // do_sigtimedwait 语义：调用者把待等信号同步
                            // dequeue，通常先用 sigprocmask 屏蔽该信号且不装
                            // handler，因此 `sigchld_wakes_signal_wait_sig` 的
                            // "未屏蔽 + 已装 handler" 判据恒为 false，导致 child
                            // exit 无法唤醒 sigtimedwait 父进程（实测每例白睡满
                            // 超时 ~5s，libctest 吞吐崩塌）。这里据真实等待集
                            // (`signal_wait_set`) 唤醒：只要退出信号落在等待集
                            // 内即唤醒，重执行 ecall 时再由 sys_rt_sigtimedwait
                            // 把 SIGCHLD dequeue 并返回。
                            let in_sigtimedwait_set = child_exit_signal >= 1
                                && child_exit_signal <= 64
                                && parent.wait_channel == Some("do_sigtimedwait")
                                && (parent.signal_wait_set
                                    & (1u64 << (child_exit_signal as u64 - 1)))
                                    != 0;
                            let wake_for_signal = !is_thread
                                && parent.waiting_for == Some(SIGNAL_WAIT_SENTINEL)
                                && (sigchld_wakes_signal_wait_sig(parent, child_exit_signal)
                                    || in_sigtimedwait_set);
                            // Linux: signals also interrupt nanosleep/poll — wake
                            // parent from any interruptible sleep when exit_signal
                            // is pending and unmasked.
                            let sig_unmasked = (parent.signal_mask
                                & (1u64 << (child_exit_signal as u64 - 1)))
                                == 0;
                            let wake_interruptible = !is_thread
                                && child_exit_signal > 0
                                && child_exit_signal <= 64
                                && ((sig_unmasked
                                    && parent.waiting_for == Some(NANOSLEEP_SENTINEL))
                                    // Linux signalfd_notify(): poll/ppoll/select
                                    // sleepers are woken even for a *blocked*
                                    // signal so a signalfd watching it becomes
                                    // readable (util-linux `script` waits for
                                    // SIGCHLD this way). Spurious wakeups are
                                    // safe: the poll loop re-evaluates readiness
                                    // and goes back to sleep.
                                    || parent.waiting_for == Some(POLL_WAIT_SENTINEL));
                            // STWDBG: gated evidence probe — a child exited and
                            // its SIGCHLD bit was just set on the parent; show
                            // whether a parent blocked in do_sigtimedwait is
                            // (not) woken by this exit.
                            if crate::cmdline::stwdbg_enabled()
                                && parent.waiting_for == Some(SIGNAL_WAIT_SENTINEL)
                                && parent.wait_channel == Some("do_sigtimedwait")
                            {
                                use core::sync::atomic::{AtomicU32, Ordering as StwOrd};
                                static STW_EXIT_CNT: AtomicU32 = AtomicU32::new(0);
                                if STW_EXIT_CNT.fetch_add(1, StwOrd::Relaxed) < 40 {
                                    crate::println!(
                                        "[STWDBG] child exit sig={} -> parent {} in do_sigtimedwait: wake_for_signal={} (sleeping={})",
                                        child_exit_signal, parent_pid, wake_for_signal,
                                        parent.state == ProcessState::Sleeping
                                    );
                                }
                            }
                            if parent.state == ProcessState::Sleeping
                                && (any_wait_wake || wake_for_signal || wake_interruptible)
                            {
                                Some(any_wait_wake)
                            } else {
                                None
                            }
                        } else {
                            None
                        };
                        if let Some(wake_for_waitpid) = wake {
                            if wake_for_waitpid {
                                // wait 结果必须在父任务变 Runnable 之前完整写好；否则
                                // 另一 hart 可先认领父任务并与本 hart 并发修改同一
                                // TrapContext。锁外清理结束后再完成并原子认领。
                                Some((parent_pid, current_pid, true))
                            } else {
                                table.mark_runnable(parent_pid, false);
                                None
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                // 清除当前进程
                table.set_current(None);
            } else {
                is_init_process = false;
                parent_to_resume = None;
            }
        } else {
            is_init_process = false;
            parent_to_resume = None;
        }
    }

    // Post-PROCESS_TABLE-lock cleanup: file_lock release + finalize_closed_fd.
    // Each finalize step may take PROCESS_TABLE again (to wake blocked pipe /
    // socket / inotify / lock-wait readers); we MUST be lock-free here.
    for (pid, fds) in core::mem::take(&mut closed_fds_pending) {
        for fd in fds {
            crate::fs::fd::finalize_closed_fd_for_pid(fd, pid);
        }
    }
    for pid in core::mem::take(&mut posix_lock_release_pids) {
        crate::fs::file_lock::release_all_locks_for_pid(pid);
        // Slice 2.x T4：进程退出时一并释放它持有的 POSIX per-process timer
        // （Linux `do_exit` → `exit_itimers` 等价路径，避免 PosixTimer 漏给
        // 已死 owner 不断 deliver 信号）。
        crate::sched::posix_timer::release_for_pid(pid);
        // 真 Linux `exit_sem`：进程退出时把它对每张 SemSet 的 SEM_UNDO 累
        // 计反向 apply 回 sem.value（clip [0,SEMVMX]），并唤醒该 sem 上
        // pending 的 waiter。锁外调用，避免 SEM_TABLE ↔ PROCESS_TABLE 反
        // 向嵌套。
        crate::ipc::sysv::sem_undo_for_pid(pid);
    }
    // exit_shm 锁外调用。
    if !detached_shm_pending.is_empty() {
        crate::syscall::shm::shm_dec_nattch_bulk(&detached_shm_pending);
    }
    write_process_accounting(acct_pending);

    // 如果是 init 进程退出，关闭系统
    // If init process exits, shutdown the system
    if is_init_process {
        // 使用统一的电源管理关机接口（支持双架构 + 文件系统同步）
        // Use unified power management shutdown (supports dual-arch + fs sync)
        crate::println!("[kernel] Init process exited, shutting down...");
        // 跨核 TLB shootdown 的可观测出口：`timeouts` 不为 0 说明存在「发起方已
        // 改页表但对端未确认失效」的窗口（正确性风险），`acks` 明显小于 `reqs`
        // 说明 IPI 未被对端处理（同步等待会白等到自旋上限，是性能灾难）。
        {
            let (reqs, acks, timeouts) = crate::mm::tlb::shootdown_counters();
            crate::println!(
                "[tlbstat] shootdown reqs={} acks={} timeouts={}",
                reqs, acks, timeouts
            );
        }
        if crate::RTDIAG {
            crate::sched::process::rtstat_dump();
        }
        sbi_shutdown();
    }

    // 6. 切换到父进程继续执行
    if let Some((parent_pid, child_pid, complete_waitpid)) = parent_to_resume {
        // 获取父进程的信息并切换
        let (trap_cx_ptr, user_satp, deliver_signal) = {
            let mut guard = PROCESS_TABLE.lock();
            let table = guard.as_mut().expect("Process table not initialized");

            // **先提取子进程的 exit code、ExitReason、uid 与 CPU ticks**
            // —— 删除 child 前完成读取，避免 None。
            let (exit_status, child_reason, child_uid, child_cpu_u, child_cpu_k) =
                match table.get(child_pid) {
                    Some(c) => (
                        c.exit_status
                            .map(wait_status_from_exit_reason)
                            .unwrap_or(9),
                        c.exit_status.unwrap_or_else(ExitReason::killed),
                        c.uid,
                        c.cpu_user_ticks,
                        c.cpu_kernel_ticks,
                    ),
                    None => (9, ExitReason::killed(), 0, 0, 0),
                };

            let mut reap_child = complete_waitpid;
            if complete_waitpid {
                let waitid_mode = table
                    .get(parent_pid)
                    .map_or(false, |parent| parent.wait_is_waitid);
                if waitid_mode {
                    reap_child = table.complete_waitid_status_locked(
                        parent_pid,
                        child_pid,
                        WaitidEvent::Exited(child_reason),
                        child_uid,
                        child_cpu_u,
                        child_cpu_k,
                    );
                } else if let Some(parent) = table.get_mut(parent_pid) {
                    restore_wait_trap_context(parent);
                    #[cfg(target_arch = "riscv64")]
                    const REG_A0: usize = 10;
                    #[cfg(target_arch = "loongarch64")]
                    const REG_A0: usize = 4;
                    #[cfg(target_arch = "riscv64")]
                    const REG_A1: usize = 11;
                    #[cfg(target_arch = "riscv64")]
                    const REG_A3: usize = 13;
                    #[cfg(target_arch = "riscv64")]
                    const REG_SP_WAIT: usize = 2;
                    #[cfg(target_arch = "loongarch64")]
                    const REG_A1: usize = 5;
                    #[cfg(target_arch = "loongarch64")]
                    const REG_A3: usize = 7;
                    #[cfg(target_arch = "loongarch64")]
                    const REG_SP_WAIT: usize = 3;

                    let exit_code_ptr = parent.wait_exit_code_ptr;
                    let rusage_ptr = parent.wait_rusage_ptr;
                    parent.wait_exit_code_ptr = 0;
                    parent.wait_rusage_ptr = 0;
                    parent.wait_siginfo_ptr = 0;
                    parent.wait_rusage_ptr_waitid = 0;
                    parent.wait_pgid = None;
                    parent.wait_thread_mode = None;
                    parent.wait_is_waitid = false;
                    parent.wait_wuntraced = false;
                    parent.wait_wcontinued = false;
                    parent.wait_wnowait = false;

                    let (wait_sp, wait_a3) = unsafe {
                        if let Some(trap_cx) = parent.trap_context_mut() {
                            trap_cx.x[REG_A0] = child_pid;
                            let _ = REG_A1;
                            (trap_cx.x[REG_SP_WAIT], trap_cx.x[REG_A3])
                        } else {
                            (0, 0)
                        }
                    };
                    if crate::cmdline::waitdbg_enabled()
                    {
                        crate::println!(
                            "[waitwrite] parent={} child={} sp={:#x} status_ptr={:#x} rusage_ptr={:#x} saved_a3={:#x} wstatus={:#x}",
                            parent_pid,
                            child_pid,
                            wait_sp,
                            exit_code_ptr,
                            rusage_ptr,
                            wait_a3,
                            exit_status,
                        );
                    }
                    wtrace_process("exit.resume.parent.before_write", parent_pid, parent);

                    if is_valid_user_ptr(exit_code_ptr) {
                        let wstatus = exit_status.to_le_bytes();
                        if let Some(ref mm) = parent.mm {
                            let mut addr_space = mm.lock();
                            let _ = write_user_bytes(&mut *addr_space, exit_code_ptr, &wstatus);
                        }
                    }

                    if is_valid_user_ptr(rusage_ptr) {
                        let rusage_buf = build_rusage_buf_from_ticks(child_cpu_u, child_cpu_k);
                        if let Some(ref mm) = parent.mm {
                            let mut addr_space = mm.lock();
                            let _ = write_user_bytes(&mut *addr_space, rusage_ptr, &rusage_buf);
                        }
                    }
                }
            }

            if reap_child {
                // 把已 reap zombie 的 CPU 时间合并到父 cpu_children_*；
                // 顺序：先读子的累计（zombie 自己 + 它已 reap 的孙），再删 zombie。
                let (cu, ck, ccu, cck, cmr) = match table.get(child_pid) {
                    Some(c) => (
                        c.cpu_user_ticks,
                        c.cpu_kernel_ticks,
                        c.cpu_children_user_ticks,
                        c.cpu_children_kernel_ticks,
                        c.maxrss_kb.max(c.children_maxrss_kb),
                    ),
                    None => (0, 0, 0, 0, 0),
                };
                if let Some(parent) = table.get_mut(parent_pid) {
                    parent.cpu_children_user_ticks = parent
                        .cpu_children_user_ticks
                        .saturating_add(cu)
                        .saturating_add(ccu);
                    parent.cpu_children_kernel_ticks = parent
                        .cpu_children_kernel_ticks
                        .saturating_add(ck)
                        .saturating_add(cck);
                    parent.children_maxrss_kb = parent.children_maxrss_kb.max(cmr);
                    parent.children.retain(|&p| p != child_pid);
                }
                table.reap_or_defer(child_pid);
                // Do NOT clear SIGCHLD here — the signal handler hasn't fired yet.
                // Linux delivers SIGCHLD to the handler independently of wait().
                // clear_sigchld_if_no_waitable_status is only needed when wait()
                // returns without a handler pending (e.g., in sys_wait4 itself).
            }

            // 唤醒所有 IO-sleeping 进程（退出进程可能关闭了管道写端，让读端可检测 EOF）
            table.wake_io_waiters();

            // wait 返回值、用户 wstatus/rusage 和 reap 状态都已完成后，才把父任务
            // 放回 rq 并在同一锁域内认领；任何认领失败都不得再访问父 TrapContext。
            table.mark_runnable(parent_pid, true);
            if !table.switch_to(parent_pid, 0) {
                drop(guard);
                kernel_idle_until_runnable();
            }

            // 获取父进程的 TrapContext 和 SATP
            let parent = table.get_mut(parent_pid).expect("Parent process not found");
            if complete_waitpid
                && crate::cmdline::waitdbg_enabled()
            {
                #[cfg(target_arch = "riscv64")]
                const DBG_REG_A0: usize = 10;
                #[cfg(target_arch = "riscv64")]
                const DBG_REG_SP: usize = 2;
                #[cfg(target_arch = "loongarch64")]
                const DBG_REG_A0: usize = 4;
                #[cfg(target_arch = "loongarch64")]
                const DBG_REG_SP: usize = 3;
                let status_ptr = parent.wait_exit_code_ptr;
                let pending = parent.pending_signals;
                let sigwait_ctx = parent.signal_wait_trap_context.is_some();
                let sigsuspend_restore = parent.sigsuspend_restore_mask.is_some();
                let sp_v = unsafe {
                    parent.trap_context_mut().map(|cx| cx.x[DBG_REG_SP]).unwrap_or(0)
                };
                let waitdump_enabled = crate::cmdline::waitdump_enabled();
                if waitdump_enabled {
                    crate::println!(
                        "[waitdump] parent={} stack dump sp={:#x}:",
                        parent_pid, sp_v,
                    );
                    if let Some(ref mm) = parent.mm {
                        let addr_space = mm.lock();
                        for off in (0..96usize).step_by(8) {
                            let va = crate::mm::VirtAddr(sp_v + off);
                            let vpn = va.floor();
                            let val = addr_space.page_table().translate(vpn).map(|ppn| {
                                let kva = crate::mm::phys_to_kernel_vaddr(ppn.0 * 4096 + va.page_offset());
                                unsafe { *(kva as *const u64) }
                            });
                            match val {
                                Some(v) => crate::println!("[waitdump]   +{:#x}: {:#018x}", off, v),
                                None => crate::println!("[waitdump]   +{:#x}: <unmapped>", off),
                            }
                        }
                    }
                }
                if let Some(cx) = unsafe { parent.trap_context_mut() } {
                    wtrace_trap("exit.resume.parent.before_prepare", parent_pid, cx);
                    crate::println!(
                        "[waitdbg] complete parent={} child={} sepc={:#x} ra={:#x} sp={:#x} a0={:#x} status_ptr={:#x} pending={:#x} sigwait_ctx={} sigsuspend_restore={}",
                        parent_pid,
                        child_pid,
                        cx.sepc,
                        cx.x[1],
                        cx.x[DBG_REG_SP],
                        cx.x[DBG_REG_A0],
                        status_ptr,
                        pending,
                        sigwait_ctx,
                        sigsuspend_restore,
                    );
                }
            }
            let user_satp = parent.satp();
            let (trap_cx_ptr, deliver_signal) =
                prepare_trap_context_for_user_return(table, parent_pid).expect("Parent has no TrapContext");
            unsafe { wtrace_trap("exit.resume.parent.after_prepare", parent_pid, &*trap_cx_ptr); }

            (trap_cx_ptr, user_satp, deliver_signal)
        };

        // 切换到父进程执行
        unsafe {
            wtrace_trap("exit.resume.before_signal", parent_pid, &*trap_cx_ptr);
            if deliver_signal {
                crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
            }
            wtrace_trap("exit.resume.after_signal", parent_pid, &*trap_cx_ptr);
            usertrapret(&mut *trap_cx_ptr, user_satp);
        }
    }

    // 没有父进程可以唤醒——找到任何 Runnable 进程并切换过去
    {
        let mut guard = PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            // 先唤醒所有 IO-sleeping 进程（子进程退出可能释放了管道/资源）
            table.wake_io_waiters();

            // 找到第一个 Runnable 进程
            let next = table.select_next(0, None);

            if let Some(next_pid) = next {
                if !table.switch_to(next_pid, 0) {
                    drop(guard);
                    kernel_idle_until_runnable();
                }
                let user_satp = table.get_mut(next_pid).unwrap().satp();
                let (trap_cx_ptr, deliver_signal) =
                    prepare_trap_context_for_user_return(table, next_pid)
                        .expect("Next process has no TrapContext");
                drop(guard);
                unsafe {
                    if deliver_signal {
                        crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
                    }
                    usertrapret(&mut *trap_cx_ptr, user_satp);
                }
            }
        }
    }

    // 真的没有任何可运行进程了 — 启用中断进入 idle 循环等待 timer 唤醒进程
    kernel_idle_until_runnable();
}

/// 使用统一电源管理接口关闭系统
///
/// Shutdown the system using the unified power management interface.
/// 支持 RISC-V64（SBI SRST + Legacy + FINISHER）和 LoongArch64（ACPI GED + PM1a_CNT）。
/// Supports RISC-V64 (SBI SRST + Legacy + FINISHER) and LoongArch64 (ACPI GED + PM1a_CNT).
///
/// 关机前会自动刷新文件系统缓冲区。
/// Automatically flushes filesystem buffers before shutdown.
fn sbi_shutdown() -> ! {
    #[cfg(target_arch = "loongarch64")]
    {
        use crate::arch::hal_trait::ArchHal;
        crate::arch::HalImpl::shutdown(0);
    }
    #[cfg(target_arch = "riscv64")]
    {
        unsafe {
            core::arch::asm!(
                "ecall",
                in("a0") 0usize,
                in("a1") 0usize,
                in("a6") 0usize,
                in("a7") 0x53525354usize,
            );
            core::arch::asm!(
                "ecall",
                in("a7") 8usize,
            );
            core::ptr::write_volatile(0x100000 as *mut u32, 0x5555);
        }
        loop {
            crate::arch::wfi();
        }
    }
}

/// Exit - 进程退出（带退出原因）
///
/// 与 exit 类似，但可以指定退出原因（正常退出、信号终止、被杀死）
pub fn exit_with_reason(reason: ExitReason) -> ! {
    crate::fs::inode_reap_drain();
    let _status = match reason {
        ExitReason::Normal(code) => code,
        ExitReason::Signaled { sig, .. } => 128 + sig,
    };

    // (pid, vec<Arc<FileDescriptor>>) pending finalize after PROCESS_TABLE
    // is dropped. Same rationale as `exit()` — see the comment there.
    let mut closed_fds_pending: Vec<(Pid, Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>)> = Vec::new();
    let mut posix_lock_release_pids: Vec<Pid> = Vec::new();
    let mut detached_shm_pending: Vec<i32> = Vec::new();
    let mut acct_pending: Option<ProcessAcctSnapshot> = None;

    {
        let mut table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_mut() {
            if let Some(current_pid) = table.current {
                let now = crate::arch::asm_helpers::rdtime();
                let (children, parent_pid, is_thread, release_current_resources) = {
                    if let Some(process) = table.get_mut(current_pid) {
                        crate::syscall::remove_pid_from_futex_waiters(current_pid);
                        if !process.is_thread {
                            acct_pending = Some(acct_snapshot(process, reason, now));
                            let rss = process.compute_rss_kb();
                            process.maxrss_kb = process.maxrss_kb.max(rss);
                        }

                        let children = process.children.clone();
                        let parent_pid = process.parent_pid;
                        let is_thread = process.is_thread;
                        // exit_group 同理：独享 mm 退出路径一并 detach SHM。
                        let detached_shm_ids: Vec<i32> = if is_thread {
                            Vec::new()
                        } else {
                            process.shm_attachments.iter().map(|&(id, _)| id).collect()
                        };
                        if !is_thread { process.shm_attachments.clear(); }
                        if !detached_shm_ids.is_empty() {
                            detached_shm_pending.extend(detached_shm_ids);
                        }

                        (children, parent_pid, is_thread, true)
                    } else {
                        (Vec::new(), None, false, false)
                    }
                };
                if release_current_resources {
                    table.transfer_fd_table_owner_on_exit(current_pid);
                    if let Some(process) = table.get_mut(current_pid) {
                        let closed = process.release_resources();
                        if !closed.is_empty() {
                            closed_fds_pending.push((current_pid, closed));
                        }
                    }
                    posix_lock_release_pids.push(current_pid);
                }
                table.mark_zombie(current_pid, reason);

                reparent_children_locked(table, children, parent_pid);

                // CLONE_VFORK: wake parent if it was blocking on this child
                if let Some(parent_pid) = parent_pid {
                    if let Some(parent) = table.get_mut(parent_pid) {
                        if parent.vfork_waiting_for == Some(current_pid) {
                            parent.vfork_waiting_for = None;
                            if parent.waiting_for
                                == Some(crate::sched::process::VFORK_WAIT_SENTINEL)
                            {
                                parent.waiting_for = None;
                                parent.wait_channel = None;
                            }
                            if parent.state == ProcessState::Sleeping {
                                set_process_state(parent, ProcessState::Runnable);
                                table.mark_runnable(parent_pid, false);
                            }
                        }
                    }
                }
                if let Some(parent_pid) = parent_pid {
                    let child_pgid = table.get(current_pid).map(|c| c.pgid).unwrap_or(0);
                    let should_wake = if let Some(parent) = table.get_mut(parent_pid) {
                        if !is_thread {
                            parent.pending_signals |= 1u64 << (17 - 1); crate::sched::process::note_signal_hint();
                        }
                        let thread_mode_accepts = match parent.wait_thread_mode {
                            None | Some(ThreadMode::NonThreadOnly) => !is_thread,
                            Some(ThreadMode::CloneOnly) => is_thread,
                            Some(ThreadMode::All) => true,
                        };
                        let target_match = if parent.wait_pgid.is_some() {
                            parent.wait_pgid == Some(child_pgid)
                        } else {
                            parent.waiting_for == Some(current_pid)
                                || parent.waiting_for.is_none()
                        };
                        let any_wait_wake = thread_mode_accepts
                            && target_match
                            && parent.vfork_waiting_for.is_none();
                        let wake_for_signal = !is_thread
                            && parent.waiting_for == Some(SIGNAL_WAIT_SENTINEL)
                            && sigchld_wakes_signal_wait(parent);
                        if parent.state == ProcessState::Sleeping
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
                        table.mark_runnable(parent_pid, wake_for_waitpid);
                    }
                }

                table.set_current(None);
            }
        }
    }

    // Post-PROCESS_TABLE-lock cleanup — must run lock-free, see `exit()`.
    for (pid, fds) in core::mem::take(&mut closed_fds_pending) {
        for fd in fds {
            crate::fs::fd::finalize_closed_fd_for_pid(fd, pid);
        }
    }
    for pid in core::mem::take(&mut posix_lock_release_pids) {
        crate::fs::file_lock::release_all_locks_for_pid(pid);
        // Slice 2.x T4：exit_group / kthread cleanup 路径也释放 POSIX timer。
        crate::sched::posix_timer::release_for_pid(pid);
        // exit_group 同样需要 sem_undo（Linux 行为：thread group leader 退
        // 出时回滚整个 group 共享的 sem_undo_list；我们不实现 CLONE_SYSVSEM，
        // 每个 pid 独立 undo，这里按 pid 回滚即可）。
        crate::ipc::sysv::sem_undo_for_pid(pid);
    }
    if !detached_shm_pending.is_empty() {
        crate::syscall::shm::shm_dec_nattch_bulk(&detached_shm_pending);
    }
    write_process_accounting(acct_pending);

    // 启用中断进入 idle 循环等待 timer 唤醒进程
    kernel_idle_until_runnable();
}

/// 整线程组退出（致命信号 / do_group_exit 语义）。
///
/// Linux: an uncaught fatal signal (SIGSEGV/SIGKILL/…) — or `exit_group` /
/// `SECCOMP_RET_KILL_PROCESS` — terminates **every** thread in the group, and
/// the group leader becomes the `wait()`-able zombie carrying the signal
/// status so the parent's `wait4`/`waitid` reports `WIFSIGNALED`. Non-leader
/// threads are auto-reaped (detached zombies).
///
/// Routing only the single faulting/signalled thread through
/// `exit_with_reason` leaves the other threads — most
/// importantly a leader blocked in `pthread_join` — alive and unreapable, so
/// the parent's `wait` never returns and the run idles to a timeout (LTP
/// `mmap18` `grow_stack_fail`: the recursing thread takes a guard-gap SIGSEGV
/// but its leader stays asleep in `join`).
///
/// For a single-threaded process this degenerates exactly to
/// `exit_with_reason` (no non-leader threads; `current == leader`).
pub fn exit_group_with_reason(reason: ExitReason) -> ! {
    crate::fs::inode_reap_drain();
    let mut reason = reason;
    let mut current_is_leader = true;
    let mut closed_fds_pending: Vec<(Pid, Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>)> = Vec::new();
    let mut posix_lock_release_pids: Vec<Pid> = Vec::new();
    {
        let mut table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_mut() {
            if let Some(current_pid) = table.current {
                let tgid = table.get(current_pid).map(|p| p.tgid).unwrap_or(current_pid);
                current_is_leader = current_pid == tgid;

                if current_is_leader {
                    if let Some(saved) = table
                        .get_mut(tgid)
                        .and_then(|leader| leader.group_exit_reason.take())
                    {
                        reason = saved;
                    }
                } else {
                    // 非 leader 不能改写 current 后借别人的内核栈继续退出。记录
                    // 首个组退出原因，并让 leader 在自己的 hart/内核栈上完成最终
                    // waitable zombie；当前线程随后只退出自己。
                    if let Some(leader) = table.get_mut(tgid) {
                        leader.group_exit_reason = Some(reason);
                        leader.pending_signals |= 1u64 << (9 - 1);
                        crate::sched::process::note_signal_hint();
                    }
                    if let Some(hart) = table.running_on_hart(tgid) {
                        crate::sched::smp::kick_hart(hart);
                    } else {
                        table.mark_runnable(tgid, true);
                    }
                }

                let threads: Vec<Pid> = table.pids().into_iter()
                    .filter(|&pid| {
                        pid != current_pid
                            && table.get(pid).map_or(false, |p| p.tgid == tgid && p.is_thread)
                    })
                    .collect();
                if crate::cmdline::hungdbg_enabled() {
                    crate::println!(
                        "[HUNGDBG] exit_group cur={} tgid={} threads={:?}",
                        current_pid, tgid, threads
                    );
                    for pid in table.pids() {
                        if let Some(p) = table.get(pid) {
                            crate::println!(
                                "[HUNGDBG]   pid={} tgid={} is_thread={} state={:?} comm={}",
                                pid, p.tgid, p.is_thread, p.state, p.name
                            );
                        }
                    }
                }
                for t in threads {
                    if let Some(hart) = table.running_on_hart(t) {
                        table.request_group_kill(t, hart);
                        continue;
                    }
                    crate::syscall::remove_pid_from_futex_waiters(t);
                    table.transfer_fd_table_owner_on_exit(t);
                    if let Some(p) = table.get_mut(t) {
                        let closed = p.release_resources();
                        if !closed.is_empty() {
                            closed_fds_pending.push((t, closed));
                        }
                    }
                    posix_lock_release_pids.push(t);
                    table.mark_zombie(t, reason);
                }
            }
        }
    }
    if current_is_leader {
        // leader 等远端兄弟离开 CPU 后，在自己的栈上完成组级退出。
        wait_for_siblings_off_cpu();
    }
    exit_group_with_reason_finish(closed_fds_pending, posix_lock_release_pids, reason)
}

/// 等待同组兄弟线程全部离开 CPU（见 `exit_group_with_reason` 的调用点）。
///
/// 上限有兜底：目标线程可能正卡在长时间不返回用户态的内核路径里，永久等待会把
/// 组退出变成 hang。超时后按旧行为继续（此时目标多半已被标记为待终止）。
fn wait_for_siblings_off_cpu() {
    let deadline = crate::arch::asm_helpers::rdtime()
        .saturating_add(crate::arch::consts::CLOCK_FREQ as u64); // ~1s
    loop {
        let (tgid, current_pid) = {
            let table = PROCESS_TABLE.lock();
            match table.as_ref().and_then(|t| t.current.map(|c| (t, c))) {
                Some((t, cur)) => (t.get(cur).map(|p| p.tgid).unwrap_or(cur), cur),
                None => return,
            }
        };
        let still_running = {
            let table = PROCESS_TABLE.lock();
            match table.as_ref() {
                Some(t) => t
                    .pids()
                    .into_iter()
                    .any(|pid| {
                        pid != current_pid
                            && t.get(pid)
                                .is_some_and(|p| p.tgid == tgid && p.is_thread)
                            && t.running_on_hart(pid).is_some()
                    }),
                None => return,
            }
        };
        if !still_running {
            return;
        }
        if crate::arch::asm_helpers::rdtime() >= deadline {
            crate::println!(
                "[exit_group] timeout waiting for sibling threads of tgid={} to leave CPU",
                tgid
            );
            return;
        }
        // 开一个中断窗口让本 hart 服务 timer/IPI，同时给目标 hart 推进的时间。
        unsafe {
            crate::arch::enable_interrupts();
            for _ in 0..256 {
                core::hint::spin_loop();
            }
            crate::arch::disable_interrupts();
        }
    }
}

/// `exit_group_with_reason` 的收尾：锁外 finalize 兄弟线程资源后转入
/// `exit_with_reason` 完成 leader 自身的退出。
fn exit_group_with_reason_finish(
    closed_fds_pending: Vec<(Pid, Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>)>,
    posix_lock_release_pids: Vec<Pid>,
    reason: ExitReason,
) -> ! {
    let mut closed_fds_pending = closed_fds_pending;
    let mut posix_lock_release_pids = posix_lock_release_pids;
    // Lock-free finalize of the sibling threads' resources (same protocol as
    // `exit_with_reason`'s tail). The leader's own resources are finalized by
    // the `exit_with_reason` call below.
    for (pid, fds) in core::mem::take(&mut closed_fds_pending) {
        for fd in fds {
            crate::fs::fd::finalize_closed_fd_for_pid(fd, pid);
        }
    }
    for pid in core::mem::take(&mut posix_lock_release_pids) {
        crate::fs::file_lock::release_all_locks_for_pid(pid);
        crate::sched::posix_timer::release_for_pid(pid);
        crate::ipc::sysv::sem_undo_for_pid(pid);
    }
    exit_with_reason(reason)
}

/// Linux `de_thread` (fs/exec.c): execve from a multi-threaded process first
/// kills every *other* thread in the group, so the post-exec image is
/// single-threaded and the CLOEXEC sweep cannot yank fds (e.g. an epoll fd)
/// out from under still-running siblings. Without this, a tokio-based rustup
/// proxy that execs the real toolchain binary leaves its worker threads
/// polling a closed epfd, which then fails with EBADF.
///
/// The caller keeps running; only sibling threads (same tgid, `is_thread`,
/// pid != caller) are torn down, with the same resource-release protocol as
/// `exit_group_with_reason`.
pub fn de_thread_for_exec() {
    crate::fs::inode_reap_drain();
    let mut closed_fds_pending: Vec<(Pid, Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>)> =
        Vec::new();
    let mut released_pids: Vec<Pid> = Vec::new();
    {
        let mut table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_mut() {
            if let Some(current_pid) = table.current {
                let tgid = table.get(current_pid).map(|p| p.tgid).unwrap_or(current_pid);
                let siblings: Vec<Pid> = table
                    .pids()
                    .into_iter()
                    .filter(|&pid| {
                        pid != current_pid
                            && table
                                .get(pid)
                                .is_some_and(|p| p.tgid == tgid && p.is_thread)
                    })
                    .collect();
                for t in siblings {
                    // 同 `exit_group_with_reason`：正在别的 hart 上运行的兄弟线程
                    // 只能挂 SIGKILL 让它自己退出（Linux `de_thread` 语义），
                    // 不能在这里回收它的栈/fd 表。
                    if let Some(hart) = table.running_on_hart(t) {
                        table.request_group_kill(t, hart);
                        continue;
                    }
                    crate::syscall::remove_pid_from_futex_waiters(t);
                    table.transfer_fd_table_owner_on_exit(t);
                    if let Some(p) = table.get_mut(t) {
                        let closed = p.release_resources();
                        if !closed.is_empty() {
                            closed_fds_pending.push((t, closed));
                        }
                    }
                    released_pids.push(t);
                    table.mark_zombie(t, ExitReason::Normal(0));
                }
                // `current_pid` was excluded from `siblings`; its current/on_cpu
                // ownership therefore remains intact throughout de-threading.
            }
        }
    }
    for (pid, fds) in closed_fds_pending {
        for fd in fds {
            crate::fs::fd::finalize_closed_fd_for_pid(fd, pid);
        }
    }
    for pid in released_pids {
        crate::fs::file_lock::release_all_locks_for_pid(pid);
        crate::sched::posix_timer::release_for_pid(pid);
        crate::ipc::sysv::sem_undo_for_pid(pid);
    }
}

/// 内核 idle + 调度循环。BSP 在启动完成后进入；**每个 AP 上线后也进入同一个
/// 循环**，从全局就绪队列取任务并在本 hart 上真正执行（对标 Linux 每个 CPU
/// 的 `cpu_startup_entry` → `schedule_idle`）。
///
/// 多核安全性依据：
/// - 唯一就绪队列 + `PROCESS_TABLE` 大锁串行化选择，`switch_to` 在锁内把目标
///   置 `Running` 并摘出就绪环，故两个 hart 不可能选中同一任务；
/// - 内核态不可抢占（trap 入口关中断，syscall 期间不重开），所以纯自旋的
///   `PROCESS_TABLE` 不会被本 hart 中断重入；
/// - `current` 经 `ProcTableGuard` 的 per-hart 投影，见 `ProcTableLock` 文档；
/// - 每个进程自带内核栈与 TrapContext，`usertrapret` 天然 per-hart 安全。
/// 所有 hart 在 `wfi` 里累计睡眠的 tick 总和，以及被唤醒次数。
///
/// 时间归因用：把它与 `uptime * 在线核数` 相比即可判断整机是"空转"还是"在算"。
pub static IDLE_TICKS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static IDLE_WAKEUPS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

pub(crate) fn kernel_idle_until_runnable() -> ! {
    // 先切到本 hart 专属 idle 栈，再跑循环本体。
    //
    // 所有进入 idle 的路径（AP 上线、进程退出、wait/sleep 无可运行任务…）都经过
    // 这里，因此统一在此切栈：
    // - 退出路径上"进来时的栈"是**即将被回收**的那个进程的内核栈，继续在上面跑
    //   就是 use-after-free；
    // - 阻塞路径上那是仍然存活的进程的内核栈，而该进程随时可能被**另一个 hart**
    //   选中并在 trap 时使用同一份栈 → 两个 hart 互踩。
    // 切栈是幂等的（每次都设成同一个 per-hart 槽顶），从 idle 栈再次进入也安全。
    extern "C" fn body() -> ! {
        idle_loop_body()
    }
    unsafe { crate::arch::run_on_hart_idle_stack(body) }
}

fn idle_loop_body() -> ! {
    use crate::arch::usertrapret;

    // 已经切到本 hart 专属的 idle 栈：现在可以在同一 rq 锁临界区内释放
    // 本 hart 的 current/on_cpu，并同步清 HART_CURRENT_*。若阻塞任务在切栈
    // 窗口被唤醒，set_current(None) 会在释放 on_cpu 后把它重新入队。
    {
        let mut guard = PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            table.set_current(None);
        }
    }
    super::process_sched::HART_KSTACK_OWNER.store(0, core::sync::atomic::Ordering::SeqCst);
    // 栈已释放：之前因 kstack 占用被跳过的任务现在可被其他 idle hart steal。
    super::process_sched::kick_idle_hart_if_any();

    // RISC-V：进入 idle 的第一时间就切回 Bare（satp=0）并退出 active_mm，
    // **不要等到醒来发现没有任务才切**。旧代码在 wfi 期间仍保留上一个进程的
    // 根页表；另一个 hart 若此时 execve/exit 释放该根帧，本 hart 被 timer 唤醒
    // 后第一次取指 __trap_entry 就走已回收的根页表 → InstructionPageFault 风暴
    // （8 核全 99% CPU、串口静默、TIMER_TABLE/POSIX_TIMERS 锁被中断路径占死）。
    #[cfg(target_arch = "riscv64")]
    {
        let was_enabled = crate::arch::is_interrupt_enabled();
        if was_enabled {
            crate::arch::disable_interrupts();
        }
        unsafe {
            let cur: usize;
            core::arch::asm!("csrr {}, satp", out(reg) cur);
            if cur != 0 {
                core::arch::asm!("csrw satp, zero", "sfence.vma");
            }
            // 内核态中断入口必须在 Bare 下也保持有效；后续 wfi 分支还会再设一次。
            crate::arch::asm_helpers::write_kernel_trap_entry();
        }
        crate::mm::tlb::note_active_mm(0);
        if was_enabled {
            crate::arch::enable_interrupts();
        }
    }

    // hrtimer 精确唤醒的忙轮询只允许 BSP 做：AP 也忙轮询同一个 deadline 时，
    // N-1 个 hart 会同时烧宿主 CPU（QEMU TCG 下每个 vCPU 是一个宿主线程），
    // 反而拖慢真正在执行任务的 hart。AP 只走 wfi，由 timer/IPI 唤醒。
    let is_boot_hart =
        crate::arch::asm_helpers::read_cpu_id() == crate::sched::smp::boot_hart();

    extern "C" {
        fn __alltraps();
    }

    // HUNGDBG: hung-task watchdog state. When `HUNGDBG=1`, if the system stays
    // fully idle (no runnable task) for longer than the threshold below, the
    // only explanation is a lost-wakeup deadlock — dump every task + the socket
    // registry exactly once so the stuck waiter and its empty wakeup source are
    // visible. Zero overhead and zero timing perturbation in the normal path.
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering as HungOrd};
    static IDLE_SINCE: AtomicU64 = AtomicU64::new(0);
    static HUNG_DUMPED: AtomicBool = AtomicBool::new(false);
    const HANG_SECS: u64 = 12;

    loop {
        // 先选任务再 WFI：`switch_to` 失败必须立刻重试，不能先睡把孤儿任务冻死。
        let mut guard = PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            super::process_sched::take_need_resched();
            let next = table.select_next(0, None);
            if let Some(next_pid) = next {
                IDLE_SINCE.store(0, HungOrd::Relaxed);
                HUNG_DUMPED.store(false, HungOrd::Relaxed);
                if !table.switch_to(next_pid, 0) {
                    continue;
                }
                let user_satp = table.get_mut(next_pid).unwrap().satp();
                let (trap_cx_ptr, deliver_signal) =
                    prepare_trap_context_for_user_return(table, next_pid).unwrap();
                drop(guard);
                // 恢复 stvec 为用户态 handler
                unsafe {
                    if deliver_signal {
                        crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
                    }
                    usertrapret(&mut *trap_cx_ptr, user_satp);
                }
            } else {
                // 本 hart 没有可运行任务 → 退出所有 mm 的 `mm_cpumask`
                // （对标 Linux `enter_lazy_tlb`：idle CPU 不再被算作某个地址空间
                // 的活跃使用者）。
                //
                // 不清除的后果：`active_mm` 只在 `switch_to` 写入、从不复位，于是
                // 每个**曾经**跑过该 mm 的 hart 都会被 `shootdown_others` 当成目标，
                // 而 shootdown 是同步等 ack 的；发起方于是每次 CoW/unmap 都要等一
                // 圈本已空闲的核。实测：`cargo build -p tg-xtask` 单核 37.77s，
                // 开到 2 核后 >240s 仍未完成。
                // 在退出 mm_cpumask 前先处理已经发布给本 hart 的请求；否则随后
                // drain 的 idle 快路径会跳过本 hart，而旧 ASID 条目直到下次运行
                // 才失效，ack 等待和帧复用不变量都失去可观测闭环。
                crate::mm::tlb::complete_current_pending_flush();
                crate::mm::tlb::note_active_mm(0);
                // RISC-V 内核经用户根页表的 gigapage 项取指/访存（无 LA 式
                // DMW 独立窗口；boot 期 satp=0 Bare 模式）。idle hart 的 satp
                // 若滞留**已退出进程**的根表，该根帧一经回收复用，本 hart 的
                // 内核映射立即经坏根翻译 → __alltraps/__trap_entry 自身取指、
                // 压栈全部 fault，8 核冻结（rv-stallcatch-183357：sp=tp、
                // sp=代码地址的 trap 风暴化石）。对标 Linux enter_lazy_tlb 切
                // swapper_pg_dir：idle 切回 Bare（satp=0，物理直址），退出
                // 进程的页表帧才可安全释放。
                #[cfg(target_arch = "riscv64")]
                unsafe {
                    let cur: usize;
                    core::arch::asm!("csrr {}, satp", out(reg) cur);
                    if cur != 0 {
                        core::arch::asm!(
                            "csrw satp, zero",
                            "sfence.vma",
                        );
                    }
                }
            }
            if next.is_none()
                && (crate::cmdline::hungdbg_enabled() || crate::cmdline::hangdump_enabled())
            {
                // No runnable task. Track how long we've been fully idle; past
                // the threshold this is a lost-wakeup deadlock — dump once.
                let now = crate::arch::asm_helpers::rdtime();
                let since = IDLE_SINCE.load(HungOrd::Relaxed);
                if since == 0 {
                    IDLE_SINCE.store(now, HungOrd::Relaxed);
                } else {
                    let threshold = HANG_SECS * crate::arch::consts::CLOCK_FREQ as u64;
                    if now.wrapping_sub(since) > threshold
                        && !HUNG_DUMPED.swap(true, HungOrd::Relaxed)
                    {
                        // 快照-后打：即使"看似全空闲"，也可能有别的 hart 正拿着
                        // 任务在内核里推进（见 hangdump 误报教训）；持锁打印会把
                        // 它们全部堆到表锁上，把误报变成真停摆。
                        let snapshot = table.snapshot_for_hang();
                        // 每个 futex 等待字的当前真实值：区分「丢唤醒」与
                        // 「确实还没人来唤醒」，两者在任务表里长得一样。
                        let mut futex_lines = alloc::vec::Vec::new();
                        crate::kernel::futex::futex_waiter_lines_for_hang(
                            table,
                            &mut futex_lines,
                        );
                        drop(guard);
                        crate::println!(
                            "[HUNGDBG] system fully idle >{}s with no runnable task — lost-wakeup dump:",
                            HANG_SECS
                        );
                        snapshot.print();
                        crate::net::socket::dump_socket_registry_for_hang();
                        for line in futex_lines {
                            crate::println!("{}", line);
                        }
                        continue;
                    }
                }
            }
        }
        drop(guard);

        // 本地队列空：先做 idle 后台工作——批量预清零空闲帧入池，把
        // page-fault/alloc 热路径上的 4K memset 挪到空闲时间（池满 O(1) 返回）。
        crate::mm::zero_pool_refill(32);

        // hrtimer 忙轮询或 WFI，等 IPI/timer 再选。
        let one_tick = crate::arch::consts::CLOCK_FREQ as u64 / 100;
        let dl = super::process_sched::nearest_deadline();
        let now = crate::arch::asm_helpers::rdtime();
        if is_boot_hart && dl != u64::MAX && dl <= now.saturating_add(one_tick) {
            unsafe {
                crate::arch::asm_helpers::write_kernel_trap_entry();
                while crate::arch::asm_helpers::rdtime() < dl {
                    crate::arch::enable_interrupts();
                    for _ in 0..32 {
                        core::hint::spin_loop();
                    }
                    crate::arch::disable_interrupts();
                }
            }
            super::process_sched::check_sleep_deadlines();
        } else {
            unsafe {
                crate::arch::asm_helpers::write_kernel_trap_entry();
                crate::arch::enable_interrupts();
                let t0 = crate::arch::asm_helpers::rdtime();
                crate::arch::asm_helpers::wfi_once();
                IDLE_TICKS.fetch_add(
                    crate::arch::asm_helpers::rdtime().saturating_sub(t0),
                    HungOrd::Relaxed,
                );
                IDLE_WAKEUPS.fetch_add(1, HungOrd::Relaxed);
                crate::arch::disable_interrupts();
            }
        }
    }
}

pub fn wait(
    target: WaitTarget,
    thread_mode: ThreadMode,
    wait_stopped: bool,
    wait_continued: bool,
    exit_code_ptr: usize,
    rusage_ptr: usize,
) -> ! {
    use crate::arch::usertrapret;
    use crate::arch::TrapContext;

    // Slice 2.5 wchan：wait4/waitpid 阻塞 → Linux `do_wait`
    enter_wait_channel("do_wait");

    // 架构相关的寄存器索引
    // RISC-V64: a0=x[10]
    // LoongArch64: a0=x[4]
    #[cfg(target_arch = "riscv64")]
    const REG_A0: usize = 10;
    #[cfg(target_arch = "loongarch64")]
    const REG_A0: usize = 4;

    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("Process table not initialized");

    let current_pid = table.current.expect("No current process");
    if let Some(proc) = table.get_mut(current_pid) {
        proc.io_wait_trap_context.clear();
    }

    // 1. 收集匹配子进程：同时套用线程过滤（ThreadMode）和 wait target。
    //    NonThreadOnly 对应 POSIX 默认；CloneOnly = __WCLONE；All = __WALL。
    //    waitpid pid==0/pid<-1 走 WaitTarget::Pgid 在此自然实现真实进程组等待。
    let candidates: Vec<Pid> = table
        .get(current_pid)
        .map(|parent| {
            parent
                .children
                .iter()
                .copied()
                .filter(|&c| match table.get(c) {
                    Some(child) => thread_mode.matches(child) && target.matches(child),
                    None => false,
                })
                .collect()
        })
        .unwrap_or_default();
    if candidates.is_empty() {
        // 无任何匹配 → ECHILD
        drop(candidates);
        let proc = table.get_mut(current_pid).unwrap();
        proc.wait_trap_context = None;
        unsafe { proc.trap_context_mut().unwrap().x[REG_A0] = (-10isize) as usize; }
        let user_satp = proc.satp();
        let trap_cx_ptr = unsafe { proc.trap_context_mut().unwrap() as *mut TrapContext };
        drop(guard);
        unsafe { usertrapret(&mut *trap_cx_ptr, user_satp); }
        unreachable!();
    }

    let status_event = candidates.iter().copied().find_map(|pid| {
        table.get(pid).and_then(|child| {
            let ptrace_stop_visible =
                child.ptrace_stopped && child.ptrace_tracer == current_pid;
            if (wait_stopped || ptrace_stop_visible)
                && child.stopped_waitable
                && child.state == ProcessState::Stopped
            {
                Some((
                    pid,
                    wait_status_from_stopped_signal(child.stopped_signal.unwrap_or(19)),
                ))
            } else if wait_continued && child.continued_waitable {
                Some((pid, wait_status_from_continued()))
            } else {
                None
            }
        })
    });

    if let Some((child_pid, wstatus)) = status_event {
        drop(candidates);
        let (child_u, child_k) = match table.get(child_pid) {
            Some(child) => (child.cpu_user_ticks, child.cpu_kernel_ticks),
            None => (0, 0),
        };
        if let Some(child) = table.get_mut(child_pid) {
            if child.state == ProcessState::Stopped {
                child.stopped_waitable = false;
            } else {
                child.continued_waitable = false;
            }
        }
        clear_sigchld_if_no_waitable_status(table, current_pid);
        if let Some(parent) = table.get_mut(current_pid) {
            parent.wait_exit_code_ptr = exit_code_ptr;
            parent.wait_rusage_ptr = rusage_ptr;
            parent.wait_siginfo_ptr = 0;
            parent.wait_rusage_ptr_waitid = 0;
        }
        table.complete_waitpid_status_locked(current_pid, child_pid, wstatus, child_u, child_k);
        let proc = table.get_mut(current_pid).unwrap();
        let user_satp = proc.satp();
        let trap_cx_ptr = unsafe { proc.trap_context_mut().unwrap() as *mut TrapContext };
        drop(guard);
        unsafe {
            crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
            usertrapret(&mut *trap_cx_ptr, user_satp);
        }
        unreachable!();
    }

    // 2. 查找匹配的 zombie 子进程
    let zombie = candidates.iter().copied().find(|&pid| {
        table
            .get(pid)
            .map(|c| c.state == ProcessState::Zombie)
            .unwrap_or(false)
    });

    if let Some(child_pid) = zombie {
        // crate::println!("[WAIT-REAP] pid={} reaped zombie={}", current_pid, child_pid);
        drop(candidates);
        // 回收 zombie
        let exit_status = table.get(child_pid)
            .and_then(|c| c.exit_status)
            .map(wait_status_from_exit_reason)
            .unwrap_or(9);

        // 在抹掉 zombie 之前把它的 CPU 时间合并到父 cpu_children_*。
        let (child_u, child_k, child_cu, child_ck, child_mr) = match table.get(child_pid) {
            Some(c) => (
                c.cpu_user_ticks,
                c.cpu_kernel_ticks,
                c.cpu_children_user_ticks,
                c.cpu_children_kernel_ticks,
                c.maxrss_kb.max(c.children_maxrss_kb),
            ),
            None => (0, 0, 0, 0, 0),
        };
        if let Some(parent) = table.get_mut(current_pid) {
            parent.cpu_children_user_ticks =
                parent.cpu_children_user_ticks
                    .saturating_add(child_u)
                    .saturating_add(child_cu);
            parent.cpu_children_kernel_ticks =
                parent.cpu_children_kernel_ticks
                    .saturating_add(child_k)
                    .saturating_add(child_ck);
            parent.children_maxrss_kb = parent.children_maxrss_kb.max(child_mr);
            parent.children.retain(|&p| p != child_pid);
        }
        table.reap_or_defer(child_pid);
        clear_sigchld_if_no_waitable_status(table, current_pid);

        // 设置 trap context：a0 = child_pid，并按 syscall glue 快照写 wstatus。
        // 用户态 wstatus 类型是 int (4B)；阻塞 wait 返回路径不能再从
        // trap context 的易失参数寄存器反推 status 指针。
        {
            let proc = table.get_mut(current_pid).unwrap();
            let cx = unsafe { proc.trap_context_mut().unwrap() };
            cx.x[REG_A0] = child_pid;        // a0 = waitpid 返回值
        }
        if is_valid_user_ptr(exit_code_ptr) {
            let wstatus_bytes = exit_status.to_le_bytes();
            let proc = table.get_mut(current_pid).unwrap();
            if let Some(ref mm) = proc.mm {
                let mut addr_space = mm.lock();
                let _ = write_user_bytes(&mut *addr_space, exit_code_ptr, &wstatus_bytes);
            }
        }

        // 填充 rusage（来自 syscall glue 快照）—— 仅写 zombie 本体
        // ticks（不含孙），与 POSIX `do_wait_thread` 一致。
        if is_valid_user_ptr(rusage_ptr) {
            let rusage_buf = build_rusage_buf_from_ticks(child_u, child_k);
            let proc = table.get_mut(current_pid).unwrap();
            if let Some(ref mm) = proc.mm {
                let mut addr_space = mm.lock();
                let _ = write_user_bytes(&mut *addr_space, rusage_ptr, &rusage_buf);
            }
        }

        let proc = table.get_mut(current_pid).unwrap();
        let user_satp = proc.satp();
        let trap_cx_ptr = unsafe { proc.trap_context_mut().unwrap() as *mut TrapContext };
        drop(guard);
        unsafe {
            crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
            usertrapret(&mut *trap_cx_ptr, user_satp);
        }
        unreachable!();
    }
    drop(candidates);

    // 3. 无 zombie；candidates 非空意味着至少有一个匹配 child 还活着，必挂起。
    // crate::println!("[WAIT] pid={} blocking for {:?}", current_pid, target);
    if let Some(proc) = table.get_mut(current_pid) {
        proc.io_wait_trap_context.clear();
        proc.wait_exit_code_ptr = exit_code_ptr;
        proc.wait_rusage_ptr = rusage_ptr;
        // waitpid 路径不写 wait_siginfo_ptr：让 exit() reap parent 段走
        // waitpid 分支（写 wstatus + a0=child_pid）。
        proc.wait_siginfo_ptr = 0;
        proc.wait_rusage_ptr_waitid = 0;
        proc.wait_is_waitid = false;
        proc.wait_wuntraced = wait_stopped;
        proc.wait_wcontinued = wait_continued;
        proc.wait_wnowait = false;
        match target {
            WaitTarget::Any => {
                proc.waiting_for = None;
                proc.wait_pgid = None;
            }
            WaitTarget::Pid(p) => {
                proc.waiting_for = Some(p);
                proc.wait_pgid = None;
            }
            WaitTarget::Pgid(g) => {
                // waiting_for = None 接受任意 pid，由 wait_pgid 二次过滤
                // （exit() 中的 wake_for_pgid 检查 child.pgid == g）。
                proc.waiting_for = None;
                proc.wait_pgid = Some(g);
            }
        }
        // 记录线程过滤模式给 exit() 唤醒检查用。None 表示 NonThreadOnly
        // 以减小 PCB 默认填写负担；与默认行为完全一致。
        proc.wait_thread_mode = match thread_mode {
            ThreadMode::NonThreadOnly => None,
            other => Some(other),
        };
        let saved_wait_cx = unsafe { proc.trap_context_mut().map(|cx| *cx) };
        proc.wait_trap_context = saved_wait_cx;
        wtrace_process("wait.before_sleep", current_pid, proc);
        if let Some(cx) = unsafe { proc.trap_context_mut() } {
            wtrace_trap("wait.before_sleep.trap", current_pid, cx);
        }
    }
    let waiting_for = table.get(current_pid).and_then(|p| p.waiting_for);
    table.mark_sleeping(current_pid, waiting_for, 0);
    if let Some(proc) = table.get(current_pid) {
        wtrace_process("wait.after_sleep", current_pid, proc);
    }

    // 找任何 Runnable 进程切换
    let next_pid = table.select_next(current_pid, None);
    // crate::println!("[WAIT] pid={} select_next={:?}", current_pid, next_pid);

    if let Some(next_pid) = next_pid {
        if !table.switch_to(next_pid, 0) {
            drop(guard);
            kernel_idle_until_runnable();
        }
        let user_satp = table.get_mut(next_pid).unwrap().satp();
        let (trap_cx_ptr, deliver_signal) =
            prepare_trap_context_for_user_return(table, next_pid).unwrap();
        unsafe { wtrace_trap("wait.switch.next", next_pid, &*trap_cx_ptr); }
        drop(guard);
        unsafe {
            if deliver_signal {
                crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
            }
            usertrapret(&mut *trap_cx_ptr, user_satp);
        }
    } else {
        // crate::println!("[WAIT] pid={} -> IDLE", current_pid);
        drop(guard);
        kernel_idle_until_runnable();
    }
    unreachable!();
}

/// Wait - 非阻塞版本（waitpid WNOHANG 后端）
///
/// 在调用者的子进程集合中按 `target` 过滤；若有匹配 zombie 立即 reap，
/// 否则返回 `Ok(None)`。
///
/// # 参数
/// - `target`：等待目标，支持 P_ALL/P_PID/P_PGID 三种过滤。
///
/// # 返回
/// - `Ok(Some((Pid, wstatus, cpu_user_ticks, cpu_kernel_ticks)))`：reap 到 zombie。
///   - `wstatus`：Linux 编码（normal exit = `(code & 0xff) << 8`，signal = sig）。
///   - `cpu_user_ticks` / `cpu_kernel_ticks`：zombie 本体 CPU 时间，调用方
///     可据此填 `wait4` 的 `rusage`（不含孙，与 POSIX `do_wait_thread` 一致）。
/// - `Ok(None)`：有匹配 child 但都还在运行。
/// - `Err(&'static str)`：没有任何匹配子进程（ECHILD）。
pub fn wait_nonblock(
    target: WaitTarget,
    thread_mode: ThreadMode,
    wait_stopped: bool,
    wait_continued: bool,
) -> Result<Option<(Pid, i32, u64, u64)>, &'static str> {
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().ok_or("Process table not initialized")?;

    let current_pid = table.current.ok_or("No current process")?;

    // 收集匹配子进程：套用 ThreadMode + WaitTarget 双重过滤（同 wait()）。
    let candidates: Vec<Pid> = {
        let current = table.get(current_pid).ok_or("Current process not found")?;
        current
            .children
            .iter()
            .copied()
            .filter(|&c| match table.get(c) {
                Some(ch) => thread_mode.matches(ch) && target.matches(ch),
                None => false,
            })
            .collect()
    };
    if candidates.is_empty() {
        return Err("No children to wait for");
    }

    let status_event = candidates.iter().copied().find_map(|pid| {
        table.get(pid).and_then(|child| {
            let ptrace_stop_visible =
                child.ptrace_stopped && child.ptrace_tracer == current_pid;
            if (wait_stopped || ptrace_stop_visible)
                && child.stopped_waitable
                && child.state == ProcessState::Stopped
            {
                Some((
                    pid,
                    wait_status_from_stopped_signal(child.stopped_signal.unwrap_or(19)),
                ))
            } else if wait_continued && child.continued_waitable {
                Some((pid, wait_status_from_continued()))
            } else {
                None
            }
        })
    });

    if let Some((child_pid, wstatus)) = status_event {
        if let Some(child) = table.get_mut(child_pid) {
            if child.state == ProcessState::Stopped {
                child.stopped_waitable = false;
            } else {
                child.continued_waitable = false;
            }
        }
        clear_sigchld_if_no_waitable_status(table, current_pid);
        return Ok(Some((child_pid, wstatus, 0, 0)));
    }

    let zombie_child = candidates.iter().copied().find(|&pid| {
        table
            .get(pid)
            .map(|c| c.state == ProcessState::Zombie)
            .unwrap_or(false)
    });

    if let Some(child_pid) = zombie_child {
        // crate::println!("[WNOHANG-REAP] pid={} reaped zombie={}", current_pid, child_pid);
        // 在删 child 之前一次性读出全部需要的字段。
        let (exit_status, cu, ck, ccu, cck, cmr) = match table.get(child_pid) {
            Some(c) => (
                c.exit_status
                    .map(wait_status_from_exit_reason)
                    .unwrap_or(9),
                c.cpu_user_ticks,
                c.cpu_kernel_ticks,
                c.cpu_children_user_ticks,
                c.cpu_children_kernel_ticks,
                c.maxrss_kb.max(c.children_maxrss_kb),
            ),
            None => (9, 0, 0, 0, 0, 0),
        };

        if let Some(current) = table.get_mut(current_pid) {
            current.cpu_children_user_ticks = current
                .cpu_children_user_ticks
                .saturating_add(cu)
                .saturating_add(ccu);
            current.cpu_children_kernel_ticks = current
                .cpu_children_kernel_ticks
                .saturating_add(ck)
                .saturating_add(cck);
            current.children_maxrss_kb = current.children_maxrss_kb.max(cmr);
            current.children.retain(|&p| p != child_pid);
        }
        table.reap_or_defer(child_pid);
        clear_sigchld_if_no_waitable_status(table, current_pid);

        Ok(Some((child_pid, exit_status, cu, ck)))
    } else {
        // No zombie found among candidates; will yield
        static WNOHANG_MISS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
        let n = WNOHANG_MISS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        // if n < 10 || n % 50 == 0 {
        //     let child_states: Vec<(Pid, ProcessState)> = candidates.iter()
        //         .filter_map(|&c| table.get(c).map(|p| (c, p.state)))
        //         .collect();
        //     crate::println!("[WNOHANG-MISS] pid={} n={} children={:?}", current_pid, n, child_states);
        // }
        Ok(None)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitTarget {
    Any,
    Pid(Pid),
    Pgid(Pid),
}

impl WaitTarget {
    /// 判定一个子进程是否匹配本 target。调用方需保证 child 是 parent 的 child。
    #[inline]
    pub fn matches(&self, child: &ProcessControlBlock) -> bool {
        match *self {
            WaitTarget::Any => true,
            WaitTarget::Pid(p) => child.pid == p,
            WaitTarget::Pgid(g) => child.pgid == g,
        }
    }
}

/// waitpid/waitid 的线程过滤模式，对应 Linux `__WALL` / `__WCLONE`。
///
/// Linux 区分 "clone child"（exit_signal != SIGCHLD，通常是 pthread 线程）
/// 与 "non-clone child"（exit_signal == SIGCHLD，通常是 fork 出的子进程）。
/// 我们的模型用 `PCB.is_thread` 近似映射：`is_thread == true` 对应 clone child。
///
/// - `NonThreadOnly`：默认行为，只等 `!is_thread` 的子进程。
/// - `CloneOnly`：`__WCLONE`，反向，只等 `is_thread` 的子进程（pthread）。
/// - `All`：`__WALL`，全部都等（线程 + 非线程）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadMode {
    NonThreadOnly,
    CloneOnly,
    All,
}

impl ThreadMode {
    /// 判定一个子进程是否符合本模式的线程过滤。
    #[inline]
    pub fn matches(&self, child: &ProcessControlBlock) -> bool {
        match *self {
            ThreadMode::NonThreadOnly => !child.is_thread,
            ThreadMode::CloneOnly => child.is_thread,
            ThreadMode::All => true,
        }
    }
}

/// `waitid_reap_filtered` 的错误类型。当前仅一个变体，但保留 enum 以便
/// 未来扩展（例如 P_PIDFD revoked、EFAULT 等）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitidErr {
    /// 没有任何匹配的子进程（POSIX ECHILD）。
    NoChildren,
}

/// 公开版 `build_waitid_siginfo` —— 仅做参数转发，方便 syscall 层使用而不
/// 必把模块私有 helper 改成 `pub`。
#[inline]
pub fn build_waitid_siginfo_pub(
    buf: &mut [u8; 128],
    child_pid: Pid,
    event: WaitidEvent,
    uid: u32,
    cpu_user_ticks: u64,
    cpu_kernel_ticks: u64,
) {
    build_waitid_siginfo(
        buf,
        child_pid,
        event,
        uid,
        cpu_user_ticks,
        cpu_kernel_ticks,
    );
}

/// 把子进程的 CPU ticks 折算为 `struct rusage` 144 字节布局（小端、LP64）。
///
/// 仅填 `ru_utime`/`ru_stime`（前 32 字节），其余字段（maxrss/inblock/...）
/// 暂为 0 ——内核尚未做这些 accounting，POSIX 允许字段为 0。
///
/// ```text
///   0..  8   ru_utime.tv_sec
///   8.. 16   ru_utime.tv_usec
///  16.. 24   ru_stime.tv_sec
///  24.. 32   ru_stime.tv_usec
///  32..144   其余字段 = 0
/// ```
pub fn build_rusage_buf_from_ticks(cpu_user_ticks: u64, cpu_kernel_ticks: u64) -> [u8; 144] {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    let (u_sec, u_usec, s_sec, s_usec) = if freq == 0 {
        (0i64, 0i64, 0i64, 0i64)
    } else {
        (
            (cpu_user_ticks / freq) as i64,
            ((cpu_user_ticks % freq) * 1_000_000 / freq) as i64,
            (cpu_kernel_ticks / freq) as i64,
            ((cpu_kernel_ticks % freq) * 1_000_000 / freq) as i64,
        )
    };
    let mut buf = [0u8; 144];
    buf[0..8].copy_from_slice(&u_sec.to_le_bytes());
    buf[8..16].copy_from_slice(&u_usec.to_le_bytes());
    buf[16..24].copy_from_slice(&s_sec.to_le_bytes());
    buf[24..32].copy_from_slice(&s_usec.to_le_bytes());
    buf
}

/// `waitid` 的非阻塞 reap 快路径，完整版（替代旧 `waitid_reap`）：
///
/// - 完整支持 `WaitTarget::{Any, Pid, Pgid}`。
/// - 返回值除 (pid, ExitReason, uid) 外还带 (cpu_user_ticks, cpu_kernel_ticks)
///   以便上层填 siginfo.si_utime/si_stime 与 rusage。
/// - `nowait`：仅 peek 不回收（POSIX WNOWAIT）；为 false 时把 zombie 的
///   CPU 时间合并入父 `cpu_children_*` 并从 process table 删除。
///
/// 返回语义：
/// - `Err(WaitidErr::NoChildren)`：没有任何匹配子进程（ECHILD）。
/// - `Ok(None)`：有匹配 child 但都还在运行（待阻塞或返回 0/WNOHANG）。
/// - `Ok(Some(..))`：找到 zombie。
pub fn waitid_reap_filtered(
    target: WaitTarget,
    thread_mode: ThreadMode,
    wait_exited: bool,
    wait_stopped: bool,
    wait_continued: bool,
    nowait: bool,
) -> Result<Option<(Pid, WaitidEvent, u32, u64, u64)>, WaitidErr> {
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().ok_or(WaitidErr::NoChildren)?;
    let current_pid = table.current.ok_or(WaitidErr::NoChildren)?;

    // 收集匹配的 child pid：套用 ThreadMode 与 WaitTarget 双重过滤。
    // 与 waitpid 一致：默认 NonThreadOnly；__WALL/__WCLONE 时放宽 / 反向过滤。
    let candidates: Vec<Pid> = match table.get(current_pid) {
        Some(parent) => parent
            .children
            .iter()
            .copied()
            .filter(|&c| match table.get(c) {
                Some(child) => thread_mode.matches(child) && target.matches(child),
                None => false,
            })
            .collect(),
        None => return Err(WaitidErr::NoChildren),
    };

    if candidates.is_empty() {
        return Err(WaitidErr::NoChildren);
    }

    let status_event = candidates.iter().copied().find_map(|pid| {
        table.get(pid).and_then(|child| {
            if wait_stopped && child.stopped_waitable && child.state == ProcessState::Stopped {
                Some((
                    pid,
                    WaitidEvent::Stopped(child.stopped_signal.unwrap_or(19)),
                    child.uid,
                    child.cpu_user_ticks,
                    child.cpu_kernel_ticks,
                    false,
                ))
            } else if wait_continued && child.continued_waitable {
                Some((
                    pid,
                    WaitidEvent::Continued,
                    child.uid,
                    child.cpu_user_ticks,
                    child.cpu_kernel_ticks,
                    false,
                ))
            } else {
                None
            }
        })
    });

    if let Some((pid, event, uid, cpu_u, cpu_k, _)) = status_event {
        if !nowait {
            if let Some(child) = table.get_mut(pid) {
                match event {
                    WaitidEvent::Stopped(_) => child.stopped_waitable = false,
                    WaitidEvent::Continued => child.continued_waitable = false,
                    WaitidEvent::Exited(_) => {}
                }
            }
            clear_sigchld_if_no_waitable_status(table, current_pid);
        }
        return Ok(Some((pid, event, uid, cpu_u, cpu_k)));
    }

    if !wait_exited {
        return Ok(None);
    }

    // 在匹配集中找 zombie。
    let zombie = candidates.iter().copied().find(|&pid| {
        table
            .get(pid)
            .map(|c| c.state == ProcessState::Zombie)
            .unwrap_or(false)
    });
    let Some(zpid) = zombie else { return Ok(None); };

    let (reason, uid, cpu_u, cpu_k, cpu_cu, cpu_ck, cmr) = match table.get(zpid) {
        Some(c) => (
            c.exit_status.unwrap_or_else(ExitReason::killed),
            c.uid,
            c.cpu_user_ticks,
            c.cpu_kernel_ticks,
            c.cpu_children_user_ticks,
            c.cpu_children_kernel_ticks,
            c.maxrss_kb.max(c.children_maxrss_kb),
        ),
        None => (ExitReason::killed(), 0, 0, 0, 0, 0, 0),
    };

    if !nowait {
        // POSIX wait4 rusage 只算 zombie 本体（不含孙），但父 cpu_children_*
        // 累计要把 zombie + 它已 reap 的孙都吸收（do_wait_thread 行为）。
        if let Some(current) = table.get_mut(current_pid) {
            current.cpu_children_user_ticks = current
                .cpu_children_user_ticks
                .saturating_add(cpu_u)
                .saturating_add(cpu_cu);
            current.cpu_children_kernel_ticks = current
                .cpu_children_kernel_ticks
                .saturating_add(cpu_k)
                .saturating_add(cpu_ck);
            current.children_maxrss_kb = current.children_maxrss_kb.max(cmr);
            current.children.retain(|&p| p != zpid);
        }
        table.reap_or_defer(zpid);
        clear_sigchld_if_no_waitable_status(table, current_pid);
    }

    Ok(Some((zpid, WaitidEvent::Exited(reason), uid, cpu_u, cpu_k)))
}

pub fn wait_for_waitid(
    target: WaitTarget,
    thread_mode: ThreadMode,
    wait_exited: bool,
    wait_stopped: bool,
    wait_continued: bool,
    nowait: bool,
) -> ! {
    use crate::arch::usertrapret;
    use crate::arch::TrapContext;

    // Slice 2.5 wchan：waitid 阻塞 → Linux `do_wait`（与 wait4 同标签）
    enter_wait_channel("do_wait");

    #[cfg(target_arch = "riscv64")]
    const REG_A0: usize = 10;
    #[cfg(target_arch = "loongarch64")]
    const REG_A0: usize = 4;

    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("Process table not initialized");
    let current_pid = table.current.expect("No current process");
    if let Some(parent) = table.get_mut(current_pid) {
        parent.io_wait_trap_context.clear();
    }

    // 1) 收集匹配 child：套用 ThreadMode + WaitTarget 双重过滤（同 wait()）。
    let candidates: Vec<Pid> = table
        .get(current_pid)
        .map(|parent| {
            parent
                .children
                .iter()
                .copied()
                .filter(|&c| match table.get(c) {
                    Some(child) => thread_mode.matches(child) && target.matches(child),
                    None => false,
                })
                .collect()
        })
        .unwrap_or_default();

    if candidates.is_empty() {
        // ECHILD — 必须把 sys_waitid 入口处暂存到 PCB 的 infop/ru 清零，
        // 否则下次某个无关 child exit() 时，exit() reap parent 段会按
        // wait_siginfo_ptr != 0 误判走 waitid 协议、把 siginfo 写到陈旧
        // 地址。wait_thread_mode 一并清零。
        let proc = table.get_mut(current_pid).unwrap();
        proc.wait_siginfo_ptr = 0;
        proc.wait_rusage_ptr_waitid = 0;
        proc.wait_pgid = None;
        proc.wait_thread_mode = None;
        proc.wait_is_waitid = false;
        proc.wait_wuntraced = false;
        proc.wait_wcontinued = false;
        proc.wait_wnowait = false;
        proc.wait_trap_context = None;
        drop(candidates);
        unsafe { proc.trap_context_mut().unwrap().x[REG_A0] = (-10isize) as usize; }
        let user_satp = proc.satp();
        let trap_cx_ptr = unsafe { proc.trap_context_mut().unwrap() as *mut TrapContext };
        drop(guard);
        unsafe { usertrapret(&mut *trap_cx_ptr, user_satp); }
        unreachable!();
    }

    let status_event = candidates.iter().copied().find_map(|pid| {
        table.get(pid).and_then(|child| {
            if wait_stopped && child.stopped_waitable && child.state == ProcessState::Stopped {
                Some((
                    pid,
                    WaitidEvent::Stopped(child.stopped_signal.unwrap_or(19)),
                    child.uid,
                    child.cpu_user_ticks,
                    child.cpu_kernel_ticks,
                ))
            } else if wait_continued && child.continued_waitable {
                Some((
                    pid,
                    WaitidEvent::Continued,
                    child.uid,
                    child.cpu_user_ticks,
                    child.cpu_kernel_ticks,
                ))
            } else {
                None
            }
        })
    });

    if let Some((child_pid, event, uid, child_u, child_k)) = status_event {
        drop(candidates);
        if !nowait {
            if let Some(child) = table.get_mut(child_pid) {
                match event {
                    WaitidEvent::Stopped(_) => child.stopped_waitable = false,
                    WaitidEvent::Continued => child.continued_waitable = false,
                    WaitidEvent::Exited(_) => {}
                }
            }
            clear_sigchld_if_no_waitable_status(table, current_pid);
        }
        if let Some(parent) = table.get_mut(current_pid) {
            parent.wait_wnowait = nowait;
        }
        table.complete_waitid_status_locked(current_pid, child_pid, event, uid, child_u, child_k);
        let proc = table.get_mut(current_pid).unwrap();
        let user_satp = proc.satp();
        let trap_cx_ptr = unsafe { proc.trap_context_mut().unwrap() as *mut TrapContext };
        drop(guard);
        unsafe {
            crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
            usertrapret(&mut *trap_cx_ptr, user_satp);
        }
        unreachable!();
    }

    // 2) 立即 reap：找一个 zombie。
    let zombie = if wait_exited {
        candidates.iter().copied().find(|&pid| {
            table
                .get(pid)
                .map(|c| c.state == ProcessState::Zombie)
                .unwrap_or(false)
        })
    } else {
        None
    };

    if let Some(zpid) = zombie {
        drop(candidates);
        // 读 zombie 信息。
        let (reason, uid, cpu_u, cpu_k, cpu_cu, cpu_ck) = match table.get(zpid) {
            Some(c) => (
                c.exit_status.unwrap_or_else(ExitReason::killed),
                c.uid,
                c.cpu_user_ticks,
                c.cpu_kernel_ticks,
                c.cpu_children_user_ticks,
                c.cpu_children_kernel_ticks,
            ),
            None => (ExitReason::killed(), 0, 0, 0, 0, 0),
        };

        if !nowait {
            if let Some(parent) = table.get_mut(current_pid) {
                parent.cpu_children_user_ticks = parent
                    .cpu_children_user_ticks
                    .saturating_add(cpu_u)
                    .saturating_add(cpu_cu);
                parent.cpu_children_kernel_ticks = parent
                    .cpu_children_kernel_ticks
                    .saturating_add(cpu_k)
                    .saturating_add(cpu_ck);
                parent.children.retain(|&p| p != zpid);
            }
            table.reap_or_defer(zpid);
            clear_sigchld_if_no_waitable_status(table, current_pid);
        }

        // 写 siginfo + rusage 到用户态。指针从 PCB 缓存读出
        // （sys_waitid 在挂起前已写入，但快路径未挂起也应已被写）。
        // 同时清掉 wait_thread_mode，避免遗留状态影响下一次 wait。
        let (infop, ru_ptr) = {
            let parent = table.get_mut(current_pid).unwrap();
            let ip = parent.wait_siginfo_ptr;
            let rp = parent.wait_rusage_ptr_waitid;
            parent.wait_siginfo_ptr = 0;
            parent.wait_rusage_ptr_waitid = 0;
            parent.wait_pgid = None;
            parent.wait_thread_mode = None;
            parent.wait_is_waitid = false;
            parent.wait_wuntraced = false;
            parent.wait_wcontinued = false;
            parent.wait_wnowait = false;
            (ip, rp)
        };
        if infop != 0 {
            let mut siginfo = [0u8; 128];
            build_waitid_siginfo(
                &mut siginfo,
                zpid,
                WaitidEvent::Exited(reason),
                uid,
                cpu_u,
                cpu_k,
            );
            let proc = table.get_mut(current_pid).unwrap();
            if let Some(ref mm) = proc.mm {
                let mut addr_space = mm.lock();
                let _ = write_user_bytes(&mut *addr_space, infop, &siginfo);
            }
        }
        if ru_ptr != 0 {
            let rusage_buf = build_rusage_buf_from_ticks(cpu_u, cpu_k);
            let proc = table.get_mut(current_pid).unwrap();
            if let Some(ref mm) = proc.mm {
                let mut addr_space = mm.lock();
                let _ = write_user_bytes(&mut *addr_space, ru_ptr, &rusage_buf);
            }
        }

        // a0 = 0（waitid 成功返 0）。
        let proc = table.get_mut(current_pid).unwrap();
        unsafe { proc.trap_context_mut().unwrap().x[REG_A0] = 0; }
        let user_satp = proc.satp();
        let trap_cx_ptr = unsafe { proc.trap_context_mut().unwrap() as *mut TrapContext };
        drop(guard);
        unsafe {
            crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
            usertrapret(&mut *trap_cx_ptr, user_satp);
        }
        unreachable!();
    }

    // 3) 无 zombie；既然 candidates 非空且都活着，必然要挂起等待。
    //    把 wait target + thread_mode 编码进 PCB，让 exit() 的唤醒检查
    //    走 parent_waiter_should_wake_for_waitid 时同时套用线程过滤。
    {
        let parent = table.get_mut(current_pid).unwrap();
        parent.io_wait_trap_context.clear();
        match target {
            WaitTarget::Any => {
                parent.waiting_for = None;
                parent.wait_pgid = None;
            }
            WaitTarget::Pid(p) => {
                parent.waiting_for = Some(p);
                parent.wait_pgid = None;
            }
            WaitTarget::Pgid(g) => {
                // waiting_for = None（接受任意），由 wait_pgid 二次过滤。
                parent.waiting_for = None;
                parent.wait_pgid = Some(g);
            }
        }
        parent.wait_thread_mode = match thread_mode {
            ThreadMode::NonThreadOnly => None,
            other => Some(other),
        };
        parent.wait_is_waitid = true;
        parent.wait_wuntraced = wait_stopped;
        parent.wait_wcontinued = wait_continued;
        parent.wait_wnowait = nowait;
        let saved_wait_cx = unsafe { parent.trap_context_mut().map(|cx| *cx) };
        parent.wait_trap_context = saved_wait_cx;
    }
    drop(candidates);
    let waiting_for = table.get(current_pid).and_then(|p| p.waiting_for);
    table.mark_sleeping(current_pid, waiting_for, 0);

    let next_pid = table.select_next(current_pid, None);
    if let Some(next_pid) = next_pid {
        if !table.switch_to(next_pid, 0) {
            drop(guard);
            kernel_idle_until_runnable();
        }
        let user_satp = table.get_mut(next_pid).unwrap().satp();
        let (trap_cx_ptr, deliver_signal) =
            prepare_trap_context_for_user_return(table, next_pid).unwrap();
        drop(guard);
        unsafe {
            if deliver_signal {
                crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
            }
            usertrapret(&mut *trap_cx_ptr, user_satp);
        }
    } else {
        drop(guard);
        kernel_idle_until_runnable();
    }
    unreachable!();
}

/// 旧 `waitid_reap` 的兼容包装（仅 P_ALL/P_PID，不返 CPU ticks）。
/// 仅为已存在调用方保留；新代码请用 `waitid_reap_filtered`。
#[deprecated(note = "use waitid_reap_filtered to get full target + CPU ticks")]
#[allow(dead_code)]
pub fn waitid_reap(target: Option<Pid>, nowait: bool) -> Result<Option<(Pid, ExitReason, u32)>, ()> {
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().ok_or(())?;
    let current_pid = table.current.ok_or(())?;

    let candidates: Vec<Pid> = match table.get(current_pid) {
        Some(parent) => parent
            .children
            .iter()
            .copied()
            .filter(|&c| match table.get(c) {
                Some(child) => !child.is_thread && (target.is_none() || target == Some(c)),
                None => false,
            })
            .collect(),
        None => return Err(()),
    };

    if candidates.is_empty() {
        return Err(());
    }

    let zombie = candidates.iter().copied().find(|&pid| {
        table
            .get(pid)
            .map(|c| c.state == ProcessState::Zombie)
            .unwrap_or(false)
    });

    let Some(zpid) = zombie else { return Ok(None); };

    let (reason, uid) = match table.get(zpid) {
        Some(c) => (c.exit_status.unwrap_or_else(ExitReason::killed), c.uid),
        None => (ExitReason::killed(), 0),
    };

    if !nowait {
        // 先把 zombie 的 CPU 累计转给父进程，再回收 PCB；顺序不能反，否则 zombie
        // 已被 remove，account_reap_child 找不到。
        let (cu, ck, ccu, cck) = match table.get(zpid) {
            Some(c) => (
                c.cpu_user_ticks,
                c.cpu_kernel_ticks,
                c.cpu_children_user_ticks,
                c.cpu_children_kernel_ticks,
            ),
            None => (0, 0, 0, 0),
        };
        if let Some(current) = table.get_mut(current_pid) {
            current.cpu_children_user_ticks =
                current.cpu_children_user_ticks.saturating_add(cu).saturating_add(ccu);
            current.cpu_children_kernel_ticks =
                current.cpu_children_kernel_ticks.saturating_add(ck).saturating_add(cck);
            current.children.retain(|&p| p != zpid);
        }
        table.reap_or_defer(zpid);
        clear_sigchld_if_no_waitable_status(table, current_pid);
    }

    Ok(Some((zpid, reason, uid)))
}

/// Kill - 终止进程
pub fn kill(pid: Pid) -> Result<(), &'static str> {
    let mut table = PROCESS_TABLE.lock();
    let table = table.as_mut().ok_or("Process table not initialized")?;

    if table.get(pid).map_or(false, |process| process.state == ProcessState::Zombie) {
        return Err("Process already dead");
    }

    crate::syscall::remove_pid_from_futex_waiters(pid);
    table.mark_zombie(pid, ExitReason::killed());

    Ok(())
}
