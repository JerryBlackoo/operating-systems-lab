use crate::syscall::ppn_offset_to_kernel_ptr;
use crate::syscall::user::read_user_buffer;
use crate::sched::wait_queue::{WaitQueue, WaitReason};

/// futex 等待队列：地址 → 阻塞在该地址上的 PID 列表
pub static FUTEX_WAITERS: crate::sync::TicketSpinLock<alloc::collections::BTreeMap<usize, WaitQueue>> =
    crate::sync::TicketSpinLock::new(alloc::collections::BTreeMap::new());

/// 空队列缓存上限：wait/wake 热路径不再逐次移除变空的 WaitQueue（BTree 节点 +
/// 队列 Vec 反复 alloc/free 是 futex 竞争往返的主要内核堆开销）；仅当 map
/// 超过上限时批量修剪空队列，保证长期有界（线程退出路径也会全量修剪）。
const FUTEX_EMPTY_CACHE_CAP: usize = 512;

fn prune_empty_futex_queues(waiters: &mut alloc::collections::BTreeMap<usize, WaitQueue>) {
    if waiters.len() > FUTEX_EMPTY_CACHE_CAP {
        waiters.retain(|_, queue| !queue.is_empty());
    }
}

pub fn futex_waiter_stats_snapshot() -> Option<(usize, usize)> {
    let waiters = FUTEX_WAITERS.try_lock()?;
    let mut waiter_count = 0usize;
    for queue in waiters.values() {
        waiter_count += queue.len();
    }
    Some((waiters.len(), waiter_count))
}

/// 停摆取证：倒出每个 futex 等待队列，以及**该等待字此刻的真实值**。
///
/// 判据：等待者是带着某个期望值睡下的；若唤醒方已经改写该字却没能把它唤醒，
/// 这里就会看到「仍在队列里睡着，但值已经变了」。这一行证据能把丢唤醒与
/// 「确实还没人来唤醒」区分开，而这两者在任务表快照里长得一模一样。
pub fn futex_waiter_lines_for_hang(
    table: &crate::sched::process::ProcessTable,
    lines: &mut alloc::vec::Vec<alloc::string::String>,
) {
    let waiters = match FUTEX_WAITERS.try_lock() {
        Some(w) => w,
        None => {
            lines.push(alloc::string::String::from("[HUNGDBG] futex waiters: lock busy"));
            return;
        }
    };
    lines.push(alloc::string::String::from("[HUNGDBG] ==== futex waiters ===="));
    for (uaddr, queue) in waiters.iter() {
        if queue.is_empty() {
            continue;
        }
        for pid in queue.snapshot_pids() {
            match read_futex_val_with_table(table, pid, *uaddr) {
                Some(v) => lines.push(alloc::format!(
                    "[HUNGDBG] futex uaddr={:#x} pid={} current_value={}",
                    uaddr, pid, v
                )),
                None => lines.push(alloc::format!(
                    "[HUNGDBG] futex uaddr={:#x} pid={} current_value=<unreadable>",
                    uaddr, pid
                )),
            }
        }
    }
}

/// 用调用方**已持有**的表引用解析并读取等待字。
///
/// 停摆倒出是在持进程表锁的上下文里跑的，重新 `try_lock` 必然失败，值只会打成
/// `<unmapped>`（第一版探针就是这样自废武功的）。
fn read_futex_val_with_table(
    table: &crate::sched::process::ProcessTable,
    pid: usize,
    uaddr: usize,
) -> Option<u32> {
    let p = table.get(pid)?;
    let as_pid = if p.mm.is_some() { pid } else { p.tgid };
    let target = table.get(as_pid)?;
    let mm = target.mm.as_ref()?;
    let addr_space = mm.try_lock()?;
    let va = crate::mm::VirtAddr(uaddr);
    let ppn = addr_space.page_table().translate(va.floor())?;
    let kva = ppn_offset_to_kernel_ptr(ppn, va.page_offset());
    Some(unsafe { core::ptr::read_volatile(kva as *const u32) })
}

pub fn remove_pid_from_futex_waiters(pid: usize) {
    let mut waiters = FUTEX_WAITERS.lock();
    waiters.retain(|_, queue| {
        queue.remove_pid(pid);
        !queue.is_empty()
    });
}

/// 计算 futex 队列键：PRIVATE = (页表根, uaddr) 混合；SHARED = 裸 uaddr。
/// 页表根经 HART_CURRENT_PCB 免锁快路径读取（本任务正运行其上，PCB 稳定）。
///
/// 该函数只应在「当前任务仍绑定于本 hart 上运行」的上下文调用（syscall 执行期），
/// 其读取的 HART_CURRENT_PCB 即当前任务自身，键 = 本任务 mm 的身份。
pub(crate) fn futex_queue_key(uaddr: usize, private: bool) -> usize {
    if !private {
        return uaddr;
    }
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    let root = if pcb_ptr.is_null() {
        0usize
    } else {
        unsafe { (&*pcb_ptr).page_table_root.0 as usize }
    };
    let mut k = uaddr ^ root.rotate_left(17) ^ root.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    k = k.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    k ^ (k >> 29)
}

fn futex_dbg_enabled() -> bool {
    crate::cmdline::ftxdbg_enabled()
}

/// 绝对超时 timespec → 单调 uptime ticks（与 `rdtime()` 同基准）。
///
/// CLOCK_REALTIME 基准的绝对时刻须先减去 `WALLCLOCK_OFFSET_NSEC` 折回
/// uptime（Linux futex 绝对超时经 hrtimer 各自时钟到期；本内核统一以
/// rdtime ticks 判定到期，故在此换算）。已过期的时刻返回最小有效
/// deadline（2），使等待立即按超时路径返回（0/1 是"无超时"哨兵）。
fn abs_timeout_to_uptime_ticks(sec: i64, nsec: i64, realtime: bool, clock_freq: u64) -> u64 {
    let mut total_nsec = (sec as i128) * 1_000_000_000 + nsec as i128;
    if realtime {
        total_nsec -= crate::syscall::time::WALLCLOCK_OFFSET_NSEC
            .load(core::sync::atomic::Ordering::Relaxed) as i128;
    }
    if total_nsec <= 0 {
        return 2;
    }
    let ticks = (total_nsec as u128).saturating_mul(clock_freq as u128) / 1_000_000_000;
    (ticks as u64).max(2)
}

/// futex(98) — 用户空间快速互斥锁
///
/// FUTEX_WAIT: 检查 *uaddr == val，若匹配则阻塞当前线程（使用 suspend 模型）
/// FUTEX_WAKE: 唤醒最多 val 个在 uaddr 上等待的线程
///
/// 阻塞使用 suspend_current_and_run_next() -> ! (re-execute ecall 模型)。
/// 唤醒后重新进入 sys_futex → CAS 失败或检测到 pending signal 即返回。
#[allow(dead_code)]
pub(crate) fn sys_futex(uaddr: usize, op: i32, val: u32, timeout_or_val2: usize, uaddr2: usize, val3: u32) -> isize {
    use crate::mm::VirtAddr;

    let cmd = op & 0x7F; // 去掉 FUTEX_PRIVATE_FLAG(128)
    // FUTEX_CLOCK_REALTIME(256)：绝对超时按 CLOCK_REALTIME 解释（Linux
    // futex(2)），否则绝对超时基准为 CLOCK_MONOTONIC。
    let clock_realtime = op & 256 != 0;
    // P1-1 修复：PRIVATE futex 的队列键必须含地址空间身份。此前裸 uaddr 作键，
    // 两个进程同 VA 的 PRIVATE futex（glibc 全部内部锁都是 PRIVATE）共享同一
    // 队列——A 的 FUTEX_WAKE 按 FIFO 摘走 B 的 waiter（唤醒窃取），双方各自的
    // 真实 waiter 滞留队列 → 永久挂起（BuildStorm 多 rustc 进程并发正是此负载）。
    // 对标 Linux get_futex_key：PRIVATE 键 = (mm, addr)，SHARED 键 = 共享页身份
    // （此处保留裸 addr，与旧行为一致）。
    let private = op & 128 != 0;

    const FUTEX_WAIT: i32 = 0;
    const FUTEX_WAKE: i32 = 1;
    const FUTEX_REQUEUE: i32 = 3;
    const FUTEX_CMP_REQUEUE: i32 = 4;
    const FUTEX_WAKE_OP: i32 = 5;
    const FUTEX_WAIT_BITSET: i32 = 9;
    const FUTEX_WAKE_BITSET: i32 = 10;
    const CLOCK_FREQ: u64 = crate::arch::consts::CLOCK_FREQ as u64;
    const FUTEX_WOKEN_SENTINEL: usize = crate::sched::process::FUTEX_WOKEN_SENTINEL;

    /// 读取用户态 futex word，需要当前进程地址空间
    /// 支持 demand-paging: 若页未映射但在合法 mmap 区域，触发缺页后重试。
    fn read_futex_val(uaddr: usize) -> Result<u32, isize> {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                let as_pid = {
                    let process = match t.get(pid) {
                        Some(p) => p,
                        None => return Err(-14),
                    };
                    if process.mm.is_some() { pid } else { process.tgid }
                };
                if let Some(target) = t.get(as_pid) {
                    if let Some(ref mm) = target.mm {
                        let mut addr_space = mm.lock();
                        let va = VirtAddr(uaddr);
                        let vpn = va.floor();
                        let offset = va.page_offset();
                        let ppn = match addr_space.page_table().translate(vpn) {
                            Some(ppn) => ppn,
                            None => {
                                // Demand-page: trigger page fault for anonymous/lazy pages
                                if addr_space.handle_page_fault(va, false) {
                                    match addr_space.page_table().translate(vpn) {
                                        Some(ppn) => ppn,
                                        None => return Err(-14),
                                    }
                                } else {
                                    return Err(-14);
                                }
                            }
                        };
                        let kva = ppn_offset_to_kernel_ptr(ppn, offset);
                        return Ok(unsafe { *(kva as *const u32) });
                    }
                }
            }
        }
        Err(-14) // -EFAULT
    }

    /// 解析 futex word 的内核可读指针，供「持 FUTEX_WAITERS 锁重读」使用。
    ///
    /// `read_futex_val` 内部要取 PROCESS_TABLE 锁，而 `do_clear_child_tid` 是持
    /// PROCESS_TABLE 再取 FUTEX_WAITERS，故在 FUTEX_WAITERS 之下再取
    /// PROCESS_TABLE 会构成 ABBA。等待路径因此先在锁外把地址定死成内核指针，
    /// 入队时只做一次裸读。
    fn resolve_futex_kptr(uaddr: usize) -> Result<(*const u32, crate::mm::PhysPageNum), isize> {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_mut() { Some(t) => t, None => return Err(-14) };
        let pid = match t.current { Some(p) => p, None => return Err(-14) };
        let as_pid = {
            let process = match t.get(pid) { Some(p) => p, None => return Err(-14) };
            if process.mm.is_some() { pid } else { process.tgid }
        };
        let target = match t.get(as_pid) { Some(p) => p, None => return Err(-14) };
        let mm = match target.mm { Some(ref mm) => mm, None => return Err(-14) };
        let mut addr_space = mm.lock();
        let va = VirtAddr(uaddr);
        let vpn = va.floor();
        let offset = va.page_offset();
        let ppn = match addr_space.page_table().translate(vpn) {
            Some(ppn) => ppn,
            None => {
                if addr_space.handle_page_fault(va, false) {
                    match addr_space.page_table().translate(vpn) {
                        Some(ppn) => ppn,
                        None => return Err(-14),
                    }
                } else {
                    return Err(-14);
                }
            }
        };
        // 页钉扎（对标 Linux get_futex_key 的 page pin）：mm 锁释放后、FW 锁内
        // 重读前，同进程其它线程可能 munmap 该页并让帧被 buddy 复用——钉住引用
        // 计数保证重读期间帧内容稳定（读到的至多是"刚解除映射的旧内容"，与
        // Linux 持 pin 的 futex key 语义一致）。调用方在 FW 锁内重读完成后必须
        // dec_frame_ref 释放。
        crate::mm::inc_frame_ref(ppn);
        Ok((ppn_offset_to_kernel_ptr(ppn, offset) as *const u32, ppn))
    }

    fn write_futex_val(uaddr: usize, val: u32) -> Result<(), isize> {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get(pid) {
                    let as_pid = if process.mm.is_some() { pid } else { process.tgid };
                    if let Some(target) = t.get(as_pid) {
                        if let Some(ref mm) = target.mm {
                            let mut addr_space = mm.lock();
                            let va = VirtAddr(uaddr);
                            let vpn = va.floor();
                            let offset = va.page_offset();
                            // 只读（CoW）或未建映射（懒页）：先走写缺页补页
                            match addr_space.page_table().get_pte_flags(vpn) {
                                Some(f) if f.contains(crate::mm::PTEFlags::W) => {}
                                _ => { addr_space.handle_page_fault(va, true); }
                            }
                            if let Some(ppn) = addr_space.page_table().translate(vpn) {
                                let kva = ppn_offset_to_kernel_ptr(ppn, offset);
                                unsafe { *(kva as *mut u32) = val; }
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
        Err(-14)
    }

    fn wake_futex_waiters(uaddr: usize, count: usize, private: bool) -> (u32, Option<usize>) {
        let to_wake: alloc::vec::Vec<usize> = {
            let mut waiters = FUTEX_WAITERS.lock();
            let mut drained = alloc::vec::Vec::new();
            if let Some(queue) = waiters.get(&futex_queue_key(uaddr, private)) {
                for _ in 0..count {
                    if let Some(entry) = queue.dequeue_one() {
                        drained.push(entry.pid);
                    } else {
                        break;
                    }
                }
                // 变空也不移除：空队列留作缓存供下次 WAIT 复用（有界，见
                // prune_empty_futex_queues）。
            }
            drained
        };
        let mut woken = 0u32;
        let mut first_woken = None;
        if crate::cmdline::ftxtrace_enabled() {
            // 同一地址上「有没有发过 wake」「发的时候队列里有没有人」是区分
            // 「唤醒方没来」与「来了却找不到人」的唯一判据，故 drained=0 也记。
            crate::println!(
                "[FTX] wake uaddr={:#x} count={} drained={} t={}",
                uaddr, count, to_wake.len(), crate::arch::asm_helpers::rdtime()
            );
        }
        if to_wake.is_empty() {
            return (0, None);
        }
        {
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref mut t) = *table {
                for pid in &to_wake {
                    let state = match t.get(*pid) {
                        Some(process)
                            if !matches!(
                                process.state,
                                crate::sched::process::ProcessState::Zombie
                                    | crate::sched::process::ProcessState::Unused
                            ) =>
                        {
                            process.state
                        }
                        _ => continue,
                    };
                    if let Some(process) = t.get_mut(*pid) {
                        process.waiting_for = Some(FUTEX_WOKEN_SENTINEL);
                        process.wakeup_channel = 0;
                        // 目标可能已出队但尚未走到 Sleeping。置通用 waitqueue_woken
                        // 让 suspend_current_for_io_inner 的 prepare_to_wait 契约
                        // 消费掉这次唤醒并重启 syscall，而不是睡进一个没人再唤的队列。
                        process.waitqueue_woken = true;
                    }
                    if state == crate::sched::process::ProcessState::Sleeping {
                        t.mark_runnable(*pid, false);
                    }
                    if first_woken.is_none() {
                        first_woken = Some(*pid);
                    }
                    woken += 1;
                }
            }
        }
        (woken, first_woken)
    }

    /// Dequeue the highest-priority waiter from a PI wait queue.
    /// Per Linux kernel/futex/pi.c: the top waiter (highest sched_priority)
    /// is selected for ownership transfer on FUTEX_UNLOCK_PI.
    ///
    /// `table` 必须由调用方**已持有**的 PROCESS_TABLE guard 传入：禁止在持
    /// FUTEX_WAITERS 时新取 PROCESS_TABLE——do_clear_child_tid / exit 路径按
    /// PT → FW 序持有两锁，FW 下再取 PT 即 ABBA 死锁（审计 C1 结论）。
    fn pi_dequeue_highest_priority(
        queue: &WaitQueue,
        table: Option<&crate::sched::process::ProcessTable>,
    ) -> Option<usize> {
        let entry = queue.dequeue_highest(|pid| {
            table
                .and_then(|t| t.get(pid))
                .map(|p| {
                    let base = p.sched_priority;
                    let boost = p.pi_boost_priority;
                    if boost > base { boost } else { base }
                })
                .unwrap_or(0)
        });
        entry.map(|e| e.pid)
    }

    match cmd {
        FUTEX_WAIT | FUTEX_WAIT_BITSET => {
            // 入口一把 PROCESS_TABLE 锁取齐 pid / 旧等待地址 / 唤醒哨兵 /
            // 可中断信号 / 存量 deadline —— WAIT 高频往返（contended mutex
            // 每次 lock 都是 WAIT+re-entry 两趟 dispatch）下，入口到 CAS 前
            // 的多次全局锁往返是纯开销。
            let (current_pid, waiting_addr, reentry_woken, intr, stored_deadline) = {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                let t = match table.as_mut() { Some(t) => t, None => return -14 };
                let pid = match t.current { Some(p) => p, None => return -14 };
                let intr = crate::kernel::signal::pending_interrupting_signal_for_process(t, pid, 0);
                match t.get(pid) {
                    Some(p) => (
                        pid,
                        p.waiting_for.unwrap_or(uaddr),
                        p.waiting_for == Some(FUTEX_WOKEN_SENTINEL),
                        intr,
                        p.wakeup_channel,
                    ),
                    None => return -14,
                }
            };
            if futex_dbg_enabled() {
                let (sepc, ra) = {
                    let table = crate::sched::process::PROCESS_TABLE.lock();
                    table.as_ref()
                        .and_then(|t| t.get(current_pid))
                        .filter(|p| p.trap_context_ptr != 0)
                        .map(|p| {
                            let cx = unsafe { &*(p.trap_context_ptr as *const crate::arch::TrapContext) };
                            (cx.sepc, cx.x[1])
                        })
                        .unwrap_or((0, 0))
                };
                crate::println!(
                    "[ftxdbg] wait.enter pid={} uaddr={:#x} cmd={} val={} timeout_ptr={:#x} sepc={:#x} ra={:#x}",
                    current_pid, uaddr, cmd, val, timeout_or_val2, sepc, ra
                );
            }

            // re-entry 清理：移除旧 waiters 条目
            {
                let waiters = FUTEX_WAITERS.lock();
                if let Some(queue) = waiters.get(&futex_queue_key(waiting_addr, private)) {
                    queue.remove_pid(current_pid);
                }
            }

            // re-entry 检查（哨兵唤醒 / 可中断信号 / 超时），基于入口快照
            let reentry_timeout = !reentry_woken
                && !intr
                && stored_deadline > 1
                && crate::arch::asm_helpers::rdtime() >= stored_deadline as u64;
            if reentry_woken || intr || reentry_timeout {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(t) = table.as_mut() {
                    if let Some(p) = t.get_mut(current_pid) {
                        p.clear_wait_state();
                        // 唤醒在此消费完毕：残留的 waitqueue_woken 会让该任务
                        // 下一次 suspend 被 prepare_to_wait 契约直接跳过。
                        p.waitqueue_woken = false;
                    }
                }
                if reentry_woken {
                    if futex_dbg_enabled() {
                        crate::println!(
                            "[ftxdbg] wait.reentry_woken pid={} uaddr={:#x}",
                            current_pid, uaddr
                        );
                    }
                    return 0;
                }
                if intr {
                    return -4; // -EINTR
                }
                return -110; // -ETIMEDOUT
            }

            // CAS check。指针在此一并定死：入队时要在 FUTEX_WAITERS 锁内重读，
            // 而那里不能再取 PROCESS_TABLE（见 resolve_futex_kptr）。页钉扎
            // 覆盖 resolve → FW 锁内重读窗口，重读后立即释放。
            let (futex_kptr, futex_pin_ppn) = match resolve_futex_kptr(uaddr) {
                Ok(p) => p,
                Err(e) => return e,
            };
            let current_val = unsafe { core::ptr::read_volatile(futex_kptr) };

            if current_val != val {
                // 清除 wakeup_channel（timeout deadline）
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(t) = table.as_mut() {
                    if let Some(p) = t.get_mut(current_pid) {
                        p.clear_wait_state();
                    }
                }
                if futex_dbg_enabled() {
                    crate::println!(
                        "[ftxdbg] wait.eagain pid={} uaddr={:#x} current={} expected={}",
                        current_pid, uaddr, current_val, val
                    );
                }
                return -11; // -EAGAIN
            }
            if futex_dbg_enabled() {
                crate::println!(
                    "[ftxdbg] wait.match pid={} uaddr={:#x} current={} expected={}",
                    current_pid,
                    uaddr,
                    current_val,
                    val
                );
            }

            // 计算/复用 deadline（存量值已在入口快照取得）
            let deadline: usize = {
                let stored = stored_deadline;
                if stored > 1 {
                    stored // 复用已存储的 deadline
                } else if timeout_or_val2 != 0 {
                    // 解析 timespec（锁外调用 read_user_buffer 避免死锁）
                    if let Ok(buf) = read_user_buffer(timeout_or_val2 as *const u8, 16) {
                        let sec = i64::from_le_bytes(buf[0..8].try_into().unwrap());
                        let nsec = i64::from_le_bytes(buf[8..16].try_into().unwrap());
                        if sec < 0 || nsec < 0 { return -22; }
                        let deadline_ticks = if cmd == FUTEX_WAIT_BITSET {
                            // absolute timeout：REALTIME 基准须先换算回单调 uptime
                            // （减 WALLCLOCK_OFFSET_NSEC），再转 ticks 与 rdtime 可比
                            abs_timeout_to_uptime_ticks(sec, nsec, clock_realtime, CLOCK_FREQ)
                                as usize
                        } else {
                            // relative timeout
                            let now = crate::arch::asm_helpers::rdtime();
                            let ticks = (sec as u64).saturating_mul(CLOCK_FREQ)
                                .saturating_add((nsec as u64).saturating_mul(CLOCK_FREQ) / 1_000_000_000);
                            now.saturating_add(ticks) as usize
                        };
                        deadline_ticks
                    } else { 0 }
                } else { 0 }
            };

            // 存储 deadline 和等待地址，再加入等待队列，避免 wake 标记被后写覆盖。
            // Slice 2.5 wchan 真 hook：futex 阻塞前显式 set 精确标签 —
            // `mark_sleeping` 看 `waiting_for=Some(uaddr)`（用户地址）会落入
            // `wait_pid` 错误分支；这里显式 set "futex_wait_queue_me" 对齐
            // Linux `__futex_wait` 内核函数名。
            {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(t) = table.as_mut() {
                    if let Some(p) = t.get_mut(current_pid) {
                        p.wait_channel = Some("futex_wait_queue_me");
                        p.wakeup_channel = deadline;
                        p.waiting_for = Some(uaddr);
                    }
                }
            }

            // 重读 futex word 并入队，全程持 FUTEX_WAITERS 锁。
            //
            // 上面那次 CAS 校验与本次入队之间存在窗口：唤醒方在窗口内写新值并
            // 发 FUTEX_WAKE 时，队列还是空的，它唤不到任何人，也就无从设置唤醒
            // 哨兵，等待者随后睡死（BuildStorm 下表现为 pthread_join 永久阻塞）。
            // 唤醒方摘队列必须先拿这把锁，故只要「重读 + 入队」在锁内完成，两种
            // 交错都安全：写发生在重读前 → 值不符返回 EAGAIN 不睡；写发生在重读
            // 后 → 其 FUTEX_WAKE 必然排在入队之后，一定能找到本等待者。
            {
                let mut waiters = FUTEX_WAITERS.lock();
                let recheck = unsafe { core::ptr::read_volatile(futex_kptr) };
                if recheck != val {
                    drop(waiters);
                    // 页钉扎窗口结束：重读已完成，释放引用。
                    crate::mm::dec_frame_ref(futex_pin_ppn);
                    let mut table = crate::sched::process::PROCESS_TABLE.lock();
                    if let Some(t) = table.as_mut() {
                        if let Some(p) = t.get_mut(current_pid) {
                            p.clear_wait_state();
                        }
                    }
                    if futex_dbg_enabled() {
                        crate::println!(
                            "[ftxdbg] wait.eagain_recheck pid={} uaddr={:#x} current={} expected={}",
                            current_pid, uaddr, recheck, val
                        );
                    }
                    return -11; // -EAGAIN
                }
                let deadline = if deadline > 1 { Some(deadline as u64) } else { None };
                waiters.entry(futex_queue_key(uaddr, private))
                    .or_insert_with(WaitQueue::new)
                    .enqueue_pid(current_pid, WaitReason::Futex(uaddr), deadline);
                prune_empty_futex_queues(&mut waiters);
            }
            // 页钉扎窗口结束（入队+重读已完成，后续不再解引用 kptr）。
            crate::mm::dec_frame_ref(futex_pin_ppn);
            // 轨迹打印必须在 FUTEX_WAITERS 释放之后：println 要抢串口锁，把它
            // 关进临界区等于把 futex 队列锁与串口锁焊在一起——实测串口锁一旦
            // 拥塞（诊断风暴 + IrqSpinLock 自旋告警自递归），所有正在打轨迹的
            // syscall 全部冻结在临界区内，整机停摆且现场被误读成调度缺陷。
            if crate::cmdline::ftxtrace_enabled() {
                crate::println!(
                    "[FTX] wait pid={} uaddr={:#x} val={} t={}",
                    current_pid, uaddr, val, crate::arch::asm_helpers::rdtime()
                );
            }
            if futex_dbg_enabled() {
                crate::println!(
                    "[ftxdbg] wait.sleep pid={} uaddr={:#x} deadline={:?}",
                    current_pid,
                    uaddr,
                    if deadline > 1 { Some(deadline) } else { None }
                );
            }

            {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(t) = table.as_mut() {
                    if let Some(p) = t.get_mut(current_pid) {
                        if p.waiting_for == Some(FUTEX_WOKEN_SENTINEL) {
                            if futex_dbg_enabled() {
                                crate::println!(
                                    "[ftxdbg] wait.pre_sleep_woken pid={} uaddr={:#x}",
                                    current_pid, uaddr
                                );
                            }
                            p.clear_wait_state();
                            p.waitqueue_woken = false;
                            return 0;
                        }
                    }
                }
            }

            crate::sched::process::suspend_current_and_run_next();
        }
        FUTEX_WAKE | FUTEX_WAKE_BITSET => {
            let (woken, first_woken) = wake_futex_waiters(uaddr, val as usize, private);
            if futex_dbg_enabled() {
                let wake_pid = {
                    let table = crate::sched::process::PROCESS_TABLE.lock();
                    table.as_ref().and_then(|t| t.current).unwrap_or(0)
                };
                let wake_word = read_futex_val(uaddr).ok();
                crate::println!(
                    "[ftxdbg] wake pid={} uaddr={:#x} count={} word={:?} woken={} first={:?}",
                    wake_pid, uaddr, val, wake_word, woken, first_woken
                );
            }
            woken as isize
        }
        FUTEX_WAKE_OP => {
            let old = match read_futex_val(uaddr2) {
                Ok(v) => v,
                Err(e) => return e,
            };
            let encoded = val3;
            let op = ((encoded >> 28) & 0xf) as u32;
            let cmp = ((encoded >> 24) & 0xf) as u32;
            let mut oparg = ((encoded >> 12) & 0xfff) as u32;
            let cmparg = (encoded & 0xfff) as u32;
            if op & 8 != 0 {
                oparg = 1u32 << oparg;
            }
            let new_val = match op & 7 {
                0 => oparg,
                1 => old.wrapping_add(oparg),
                2 => old | oparg,
                3 => old & !oparg,
                4 => old ^ oparg,
                _ => old,
            };
            if (op & 7) <= 4 {
                if let Err(e) = write_futex_val(uaddr2, new_val) {
                    return e;
                }
            }
            let cmp_ok = match cmp {
                0 => old == cmparg,
                1 => old != cmparg,
                2 => (old as i32) < (cmparg as i32),
                3 => (old as i32) <= (cmparg as i32),
                4 => (old as i32) > (cmparg as i32),
                5 => (old as i32) >= (cmparg as i32),
                _ => false,
            };
            let (mut woken, _) = wake_futex_waiters(uaddr, val as usize, private);
            if cmp_ok {
                let (woken2, _) = wake_futex_waiters(uaddr2, timeout_or_val2, private);
                woken += woken2;
            }
            woken as isize
        }
        FUTEX_REQUEUE | FUTEX_CMP_REQUEUE => {
            // Linux: nr_wake and nr_requeue are signed; negative → EINVAL
            let nr_wake = val as i32;
            let nr_requeue = timeout_or_val2 as i32;
            if nr_wake < 0 || nr_requeue < 0 {
                return -22; // -EINVAL
            }
            // CMP_REQUEUE: compare *uaddr with val3, if mismatch return -EAGAIN
            if cmd == FUTEX_CMP_REQUEUE {
                let current_val = match read_futex_val(uaddr) {
                    Ok(v) => v,
                    Err(e) => return e,
                };
                if current_val != val3 {
                    return -11; // -EAGAIN
                }
            }
            // Wake up to `val` waiters from uaddr, requeue up to `timeout_or_val2` to uaddr2
            let max_wake = val as usize;
            let max_requeue = timeout_or_val2;
            let mut woken = 0usize;

            let (to_wake, to_requeue): (alloc::vec::Vec<usize>, alloc::vec::Vec<usize>) = {
                let mut waiters = FUTEX_WAITERS.lock();
                let mut waked = alloc::vec::Vec::new();
                let mut requeued_entries = alloc::vec::Vec::new();
                let should_remove = if let Some(queue) = waiters.get(&futex_queue_key(uaddr, private)) {
                    for _ in 0..max_wake {
                        if let Some(entry) = queue.dequeue_one() {
                            waked.push(entry.pid);
                        } else {
                            break;
                        }
                    }
                    for _ in 0..max_requeue {
                        if let Some(entry) = queue.dequeue_one() {
                            requeued_entries.push(entry);
                        } else {
                            break;
                        }
                    }
                    queue.is_empty()
                } else {
                    false
                };
                if should_remove {
                    waiters.remove(&futex_queue_key(uaddr, private));
                }
                let requeued: alloc::vec::Vec<usize> = requeued_entries.iter().map(|entry| entry.pid).collect();
                if !requeued_entries.is_empty() {
                    let target_queue = waiters.entry(futex_queue_key(uaddr2, private)).or_insert_with(WaitQueue::new);
                    for entry in requeued_entries {
                        target_queue.enqueue_pid(entry.pid, WaitReason::Futex(uaddr2), entry.deadline);
                    }
                }
                (waked, requeued)
            };
            // 唤醒 to_wake 中的线程
            {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(ref mut t) = *table {
                    for pid in &to_wake {
                        let state = match t.get(*pid) {
                            Some(process)
                                if !matches!(
                                    process.state,
                                    crate::sched::process::ProcessState::Zombie
                                        | crate::sched::process::ProcessState::Unused
                                ) =>
                            {
                                process.state
                            }
                            _ => continue,
                        };
                        if let Some(process) = t.get_mut(*pid) {
                            process.waiting_for = Some(FUTEX_WOKEN_SENTINEL);
                            process.wakeup_channel = 0;
                            process.waitqueue_woken = true;
                        }
                        if state == crate::sched::process::ProcessState::Sleeping {
                            t.mark_runnable(*pid, false);
                        }
                        woken += 1;
                    }
                    for pid in &to_requeue {
                        if let Some(process) = t.get_mut(*pid) {
                            if process.waiting_for == Some(uaddr) {
                                process.waiting_for = Some(uaddr2);
                            }
                        }
                    }
                }
            }
            (woken + to_requeue.len()) as isize
        }
        // ================================================================
        // FUTEX_LOCK_PI — per Linux kernel/futex/pi.c
        // Priority-inheritance futex lock. The futex word stores the TID
        // of the owner (with FUTEX_WAITERS bit 0x80000000 set when there
        // are waiters). Userspace does atomic cmpxchg(0, tid) first; only
        // calls into kernel when contended.
        //
        // Real PI semantics: when waiter blocks, boost owner's effective
        // priority to prevent priority inversion (per rt_mutex_adjust_prio).
        // ================================================================
        6 /* FUTEX_LOCK_PI */ => {
            let current_pid = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                match table.as_ref().and_then(|t| t.current) {
                    Some(p) => p,
                    None => return -14,
                }
            };

            // Check for re-entry (already woken by UNLOCK_PI)
            enum PiReentry { Woken, Signal, Timeout, Continue }
            let reentry_result = {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(ref mut t) = *table {
                    let intr = crate::kernel::signal::pending_interrupting_signal_for_process(t, current_pid, 0);
                    if let Some(p) = t.get_mut(current_pid) {
                        if p.waiting_for == Some(FUTEX_WOKEN_SENTINEL) {
                            p.clear_wait_state();
                            PiReentry::Woken
                        } else if intr {
                            p.clear_wait_state();
                            PiReentry::Signal
                        } else if p.wakeup_channel > 1 {
                            let now = crate::arch::asm_helpers::rdtime();
                            if now >= p.wakeup_channel as u64 {
                                p.clear_wait_state();
                                PiReentry::Timeout
                            } else {
                                PiReentry::Continue
                            }
                        } else {
                            PiReentry::Continue
                        }
                    } else { PiReentry::Continue }
                } else { PiReentry::Continue }
            };
            match reentry_result {
                PiReentry::Woken => return 0,
                PiReentry::Signal => {
                    let mut waiters = FUTEX_WAITERS.lock();
                    if let Some(queue) = waiters.get(&futex_queue_key(uaddr, private)) {
                        queue.remove_pid(current_pid);
                        if queue.is_empty() { waiters.remove(&futex_queue_key(uaddr, private)); }
                    }
                    return -4; // -EINTR
                }
                PiReentry::Timeout => {
                    let mut waiters = FUTEX_WAITERS.lock();
                    if let Some(queue) = waiters.get(&futex_queue_key(uaddr, private)) {
                        queue.remove_pid(current_pid);
                        if queue.is_empty() { waiters.remove(&futex_queue_key(uaddr, private)); }
                    }
                    return -110; // -ETIMEDOUT
                }
                PiReentry::Continue => {}
            }

            let current_val = match read_futex_val(uaddr) {
                Ok(v) => v,
                Err(e) => return e,
            };

            let owner_tid = current_val & 0x3FFF_FFFF;

            // Handle FUTEX_OWNER_DIED: previous owner died while holding lock.
            // Per Linux: new waiter takes ownership, clearing the DIED bit.
            if current_val & 0x4000_0000 != 0 {
                let new_val = current_pid as u32;
                let _ = write_futex_val(uaddr, new_val);
                return 0;
            }

            if owner_tid == 0 {
                // Uncontended: take ownership atomically
                let _ = write_futex_val(uaddr, current_pid as u32);
                return 0;
            }

            if owner_tid == current_pid as u32 {
                return -35; // -EDEADLK
            }

            // --- Priority Inheritance: boost owner ---
            // Get waiter's effective priority (for PI boost propagation)
            let waiter_priority = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                table.as_ref()
                    .and_then(|t| t.get(current_pid))
                    .map(|p| {
                        let base = p.sched_priority;
                        let boost = p.pi_boost_priority;
                        if boost > base { boost } else { base }
                    })
                    .unwrap_or(0)
            };

            // Boost owner if waiter has higher priority (higher numeric = higher prio for RT)
            if waiter_priority > 0 {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(t) = table.as_mut() {
                    if let Some(owner) = t.get_mut(owner_tid as usize) {
                        if waiter_priority > owner.pi_boost_priority {
                            owner.pi_boost_priority = waiter_priority;
                        }
                    }
                }
            }

            // Contended: set FUTEX_WAITERS bit and block
            let _ = write_futex_val(uaddr, current_val | 0x8000_0000);

            // Parse timeout: FUTEX_LOCK_PI takes an absolute CLOCK_REALTIME
            // timespec pointer (per Linux kernel/futex/pi.c futex_lock_pi).
            let deadline: Option<u64> = if timeout_or_val2 != 0 {
                if let Ok(buf) = read_user_buffer(timeout_or_val2 as *const u8, 16) {
                    let sec = i64::from_le_bytes(buf[0..8].try_into().unwrap());
                    let nsec = i64::from_le_bytes(buf[8..16].try_into().unwrap());
                    if sec < 0 || nsec < 0 { return -22; }
                    Some(abs_timeout_to_uptime_ticks(sec, nsec, true, CLOCK_FREQ))
                } else {
                    None
                }
            } else {
                None
            };

            // 解析 futex 字的内核指针：只取 PT（不可在持 FW 时取 PT，见
            // resolve_futex_kptr 注释），供下方 FW 锁内重读使用。页钉扎覆盖
            // resolve → FW 锁内重读窗口，重读后立即释放。
            let (futex_kptr, futex_pin_ppn) = match resolve_futex_kptr(uaddr) {
                Ok(p) => p,
                Err(e) => return e,
            };

            // Block on the futex
            crate::sched::process::enter_wait_channel("futex_lock_pi");
            {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(t) = table.as_mut() {
                    if let Some(p) = t.get_mut(current_pid) {
                        p.waiting_for = Some(uaddr);
                        if let Some(dl) = deadline {
                            p.wakeup_channel = dl as usize;
                        }
                    }
                }
            }
            // 入队 + FW 锁内重读（与 FUTEX_WAIT 同款防丢唤醒闭环，见该处注释）：
            // 设 WAITERS 位与入队之间 owner 可能已 UNLOCK_PI——其摘队发生在本
            // 入队之前、队列为空唤不到我们，我们随后睡死（审计 C7 根因）。锁内
            // 重读发现 owner 已归零时按 Linux futex_lock_pi 的锁内复查语义直接
            // 接管锁、不入队即返回 0。
            let (claim, has_others) = {
                let mut waiters = FUTEX_WAITERS.lock();
                let recheck = unsafe { core::ptr::read_volatile(futex_kptr) };
                if recheck & 0x3FFF_FFFF == 0 {
                    let has_others = waiters.get(&futex_queue_key(uaddr, private)).map_or(false, |q| !q.is_empty());
                    (true, has_others)
                } else {
                    waiters
                        .entry(futex_queue_key(uaddr, private))
                        .or_insert_with(WaitQueue::new)
                        .enqueue_pid(current_pid, WaitReason::Futex(uaddr), deadline);
                    (false, false)
                }
            };
            // 页钉扎窗口结束（重读/入队已完成）。
            crate::mm::dec_frame_ref(futex_pin_ppn);
            if claim {
                let new_val = current_pid as u32 | if has_others { 0x8000_0000 } else { 0 };
                let _ = write_futex_val(uaddr, new_val);
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(t) = table.as_mut() {
                    if let Some(p) = t.get_mut(current_pid) {
                        p.clear_wait_state();
                    }
                }
                return 0;
            }
            crate::sched::process::suspend_current_and_run_next();
        }

        // ================================================================
        // FUTEX_UNLOCK_PI — per Linux kernel/futex/pi.c
        // Release PI futex. Wake the highest-priority waiter and transfer
        // ownership to it. De-boost the releasing owner's priority.
        // ================================================================
        7 /* FUTEX_UNLOCK_PI */ => {
            let current_pid = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                match table.as_ref().and_then(|t| t.current) {
                    Some(p) => p,
                    None => return -14,
                }
            };

            let current_val = match read_futex_val(uaddr) {
                Ok(v) => v,
                Err(e) => return e,
            };

            let owner_tid = current_val & 0x3FFF_FFFF;
            if owner_tid != current_pid as u32 {
                return -1; // -EPERM: not the owner
            }

            // De-boost + 摘最高优先级 waiter：一次 PT 锁贯通，FW 锁嵌套其下
            // （PT → FW，与 do_clear_child_tid / exit 路径同序）。禁止持 FW 时
            // 新取 PT——旧实现经 dequeue 闭包取 PT 构成 ABBA（审计 C1 已修）。
            // PI 摘队的优先级查询因此走「已持表锁直读」。
            let first_woken = {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(t) = table.as_mut() {
                    if let Some(owner) = t.get_mut(current_pid) {
                        owner.pi_boost_priority = 0;
                    }
                }
                let mut waiters = FUTEX_WAITERS.lock();
                if let Some(queue) = waiters.get_mut(&futex_queue_key(uaddr, private)) {
                    // Dequeue highest-priority waiter (per Linux: pick top waiter)
                    // `table.as_ref()`：guard Deref 到 Option<ProcessTable>，
                    // 取 Option<&ProcessTable> 供闭包只读查优先级。
                    let best = pi_dequeue_highest_priority(queue, table.as_ref());
                    if queue.is_empty() {
                        waiters.remove(&futex_queue_key(uaddr, private));
                    }
                    best
                } else {
                    None
                }
            };

            if let Some(new_owner_pid) = first_woken {
                // Transfer ownership: write new owner TID (with WAITERS bit if
                // more waiters remain)
                let has_more = {
                    let waiters = FUTEX_WAITERS.lock();
                    waiters.get(&futex_queue_key(uaddr, private)).map_or(false, |q| !q.is_empty())
                };
                let new_val = new_owner_pid as u32
                    | if has_more { 0x8000_0000 } else { 0 };
                let _ = write_futex_val(uaddr, new_val);

                // Wake the new owner
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(ref mut t) = *table {
                    if let Some(p) = t.get_mut(new_owner_pid) {
                        p.waiting_for = Some(FUTEX_WOKEN_SENTINEL);
                        p.wakeup_channel = 0;
                        p.waitqueue_woken = true;
                    }
                    if matches!(t.get(new_owner_pid).map(|p| p.state), Some(crate::sched::process::ProcessState::Sleeping)) {
                        t.mark_runnable(new_owner_pid, false);
                    }
                }
            } else {
                // No waiters: clear the futex word
                let _ = write_futex_val(uaddr, 0);
            }
            return 0;
        }

        // ================================================================
        // FUTEX_TRYLOCK_PI (8) — non-blocking PI lock attempt
        // ================================================================
        8 /* FUTEX_TRYLOCK_PI */ => {
            let current_pid = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                match table.as_ref().and_then(|t| t.current) {
                    Some(p) => p,
                    None => return -14,
                }
            };

            let current_val = match read_futex_val(uaddr) {
                Ok(v) => v,
                Err(e) => return e,
            };

            if current_val == 0 {
                let _ = write_futex_val(uaddr, current_pid as u32);
                return 0;
            }
            return -11; // -EAGAIN: already locked
        }

        // ================================================================
        // FUTEX_CMP_REQUEUE_PI (12) — requeue waiters from non-PI to PI futex
        // ================================================================
        12 /* FUTEX_CMP_REQUEUE_PI */ => {
            let nr_wake = val as i32;
            if nr_wake < 0 {
                return -22; // -EINVAL
            }
            // CMP check
            let current_val = match read_futex_val(uaddr) {
                Ok(v) => v,
                Err(e) => return e,
            };
            if current_val != val3 {
                return -11; // -EAGAIN
            }
            // Same as CMP_REQUEUE but target is a PI futex
            let max_wake = val as usize;
            let max_requeue = timeout_or_val2;
            let mut woken = 0usize;

            let (to_wake, to_requeue): (alloc::vec::Vec<usize>, alloc::vec::Vec<usize>) = {
                let mut waiters = FUTEX_WAITERS.lock();
                let mut waked = alloc::vec::Vec::new();
                let mut requeued_entries = alloc::vec::Vec::new();
                let should_remove = if let Some(queue) = waiters.get(&futex_queue_key(uaddr, private)) {
                    for _ in 0..max_wake {
                        if let Some(entry) = queue.dequeue_one() {
                            waked.push(entry.pid);
                        } else { break; }
                    }
                    for _ in 0..max_requeue {
                        if let Some(entry) = queue.dequeue_one() {
                            requeued_entries.push(entry);
                        } else { break; }
                    }
                    queue.is_empty()
                } else { false };
                if should_remove { waiters.remove(&futex_queue_key(uaddr, private)); }
                let requeued: alloc::vec::Vec<usize> = requeued_entries.iter().map(|e| e.pid).collect();
                if !requeued_entries.is_empty() {
                    let target_queue = waiters.entry(futex_queue_key(uaddr2, private)).or_insert_with(WaitQueue::new);
                    for entry in requeued_entries {
                        target_queue.enqueue_pid(entry.pid, WaitReason::Futex(uaddr2), entry.deadline);
                    }
                }
                (waked, requeued)
            };
            {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(ref mut t) = *table {
                    for pid in &to_wake {
                        if let Some(p) = t.get_mut(*pid) {
                            p.waiting_for = Some(FUTEX_WOKEN_SENTINEL);
                            p.wakeup_channel = 0;
                            p.waitqueue_woken = true;
                        }
                        if matches!(t.get(*pid).map(|p| p.state), Some(crate::sched::process::ProcessState::Sleeping)) {
                            t.mark_runnable(*pid, false);
                        }
                        woken += 1;
                    }
                    for pid in &to_requeue {
                        if let Some(p) = t.get_mut(*pid) {
                            if p.waiting_for == Some(uaddr) {
                                p.waiting_for = Some(uaddr2);
                            }
                        }
                    }
                }
            }
            (woken + to_requeue.len()) as isize
        }

        _ => 0
    }
}

/// futex_waitv (Linux 5.16+) — block on multiple futexes simultaneously.
///
/// struct futex_waitv { __u64 val; void *uaddr; __u32 flags; __u32 __reserved; }
pub(crate) fn sys_futex_waitv(
    waiters_ptr: *const u8,
    nr: u32,
    flags: u32,
    timeout: *const u8,
    clockid: i32,
) -> isize {
    if flags != 0 { return -22; }
    if nr == 0 || nr > 128 { return -22; }
    if !timeout.is_null() && clockid != 0 && clockid != 1 { return -22; }

    let entry_size = 24usize;
    let total = (nr as usize) * entry_size;
    let buf = match read_user_buffer(waiters_ptr, total) {
        Ok(buf) => buf,
        Err(_) => return -14,
    };

    let deadline_ticks: usize = if timeout.is_null() {
        0
    } else {
        match read_user_buffer(timeout, 16) {
            Ok(tb) => {
                let sec = i64::from_le_bytes(tb[0..8].try_into().unwrap());
                let nsec = i64::from_le_bytes(tb[8..16].try_into().unwrap());
                if sec < 0 || nsec < 0 || nsec >= 1_000_000_000 { return -22; }
                let freq = crate::arch::consts::CLOCK_FREQ as u64;
                let ticks = (sec as u64).saturating_mul(freq)
                    .saturating_add((nsec as u64).saturating_mul(freq) / 1_000_000_000);
                ticks as usize
            }
            Err(_) => return -14,
        }
    };

    use crate::mm::VirtAddr;
    struct FutexWaitvEntry { val: u64, uaddr: usize, private: bool }
    let mut entries = alloc::vec::Vec::with_capacity(nr as usize);

    for i in 0..(nr as usize) {
        let off = i * entry_size;
        let val = u64::from_le_bytes(buf[off..off + 8].try_into().unwrap());
        let uaddr = u64::from_le_bytes(buf[off + 8..off + 16].try_into().unwrap()) as usize;
        let fflags = u32::from_le_bytes(buf[off + 16..off + 20].try_into().unwrap());
        if fflags & !0x02 != 0 { return -22; }
        if uaddr & 3 != 0 { return -22; }
        // 与 sys_futex 的 private 语义对齐：flags 未置 FUTEX_WAITV_PRIVATE(0x02)
        // 的条目按 SHARED 处理（Linux futex_waitv 同款：私有位缺席即共享路径）。
        entries.push(FutexWaitvEntry { val, uaddr, private: fflags & 0x02 != 0 });
    }

    // 入队前要在 FUTEX_WAITERS 锁内重读，而锁内不能再取 PROCESS_TABLE（ABBA），
    // 故这里连同校验一次性把内核指针定死。
    let mut kptrs: alloc::vec::Vec<*const u32> = alloc::vec::Vec::with_capacity(entries.len());
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                let as_pid = {
                    let process = match t.get(pid) { Some(p) => p, None => return -14 };
                    if process.mm.is_some() { pid } else { process.tgid }
                };
                if let Some(target) = t.get(as_pid) {
                    if let Some(ref mm) = target.mm {
                        let mut addr_space = mm.lock();
                        for e in entries.iter() {
                            let va = VirtAddr(e.uaddr);
                            let vpn = va.floor();
                            let offset = va.page_offset();
                            let ppn = match addr_space.page_table().translate(vpn) {
                                Some(ppn) => ppn,
                                None => {
                                    if addr_space.handle_page_fault(va, false) {
                                        match addr_space.page_table().translate(vpn) {
                                            Some(ppn) => ppn,
                                            None => return -14,
                                        }
                                    } else {
                                        return -14;
                                    }
                                }
                            };
                            let kva = ppn_offset_to_kernel_ptr(ppn, offset);
                            let current_val = unsafe { *(kva as *const u32) } as u64;
                            if current_val != e.val {
                                return -11;
                            }
                            kptrs.push(kva as *const u32);
                        }
                    } else { return -14; }
                } else { return -14; }
            } else { return -14; }
        } else { return -14; }
    }

    if deadline_ticks != 0 {
        let now = crate::arch::asm_helpers::rdtime();
        if now as usize >= deadline_ticks { return -110; }
    }

    let current_pid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table.as_ref().and_then(|t| t.current).unwrap_or(0)
    };

    let deadline = if deadline_ticks != 0 { Some(deadline_ticks as u64) } else { None };
    // 与 FUTEX_WAIT 同一契约：重读所有 word 与入队必须同处 FUTEX_WAITERS 锁内，
    // 否则唤醒方可能在校验与入队之间看到空队列而唤不到本等待者。
    {
        let mut waiters = FUTEX_WAITERS.lock();
        for (i, entry) in entries.iter().enumerate() {
            if unsafe { core::ptr::read_volatile(kptrs[i]) } as u64 != entry.val {
                return -11;
            }
        }
        for entry in entries.iter() {
            let queue = waiters.entry(futex_queue_key(entry.uaddr, entry.private)).or_insert_with(WaitQueue::new);
            queue.enqueue_pid(current_pid, WaitReason::Futex(entry.uaddr), deadline);
        }
    }

    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            let registered = if let Some(process) = t.get_mut(current_pid) {
                process.waiting_for = Some(entries[0].uaddr);
                process.wakeup_channel = deadline_ticks;
                process.state = crate::sched::process::ProcessState::Sleeping;
                deadline_ticks > 1
            } else {
                false
            };
            if registered {
                crate::sched::process_sched::note_sleep_deadline(deadline_ticks as u64);
                t.register_sleep_timer(current_pid, deadline_ticks as u64, false);
            }
        }
    }
    crate::sched::process::suspend_current_and_run_next();

    let woken_addr = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table.as_ref().and_then(|t| {
            t.current.and_then(|pid| t.get(pid).and_then(|process| process.waiting_for))
        }).unwrap_or(0)
    };

    {
        let mut waiters = FUTEX_WAITERS.lock();
        for entry in entries.iter() {
            if let Some(queue) = waiters.get_mut(&futex_queue_key(entry.uaddr, entry.private)) {
                queue.remove_pid(current_pid);
                if queue.is_empty() { waiters.remove(&futex_queue_key(entry.uaddr, entry.private)); }
            }
        }
    }

    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(process) = t.get_mut(current_pid) {
                process.waiting_for = None;
                process.wakeup_channel = 0;
            }
        }
    }

    if deadline_ticks != 0 {
        let now = crate::arch::asm_helpers::rdtime();
        if now as usize >= deadline_ticks && woken_addr == 0 {
            return -110;
        }
    }

    const FUTEX_WOKEN_SENTINEL: usize = crate::sched::process::FUTEX_WOKEN_SENTINEL;
    if woken_addr == FUTEX_WOKEN_SENTINEL || woken_addr == 0 {
        return 0;
    }

    for (index, entry) in entries.iter().enumerate() {
        if entry.uaddr == woken_addr {
            return index as isize;
        }
    }

    0
}
