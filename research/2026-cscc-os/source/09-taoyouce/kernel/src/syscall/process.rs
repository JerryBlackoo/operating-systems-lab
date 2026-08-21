//! Process lifecycle — exit/exit_group and shared helpers.
//!
//! Clone/fork → process_clone.rs, exec → process_exec.rs, wait → process_wait.rs

use alloc::vec::Vec;

use super::{remove_pid_from_futex_waiters, FUTEX_WAITERS};

// Re-exports for external callers that use `crate::syscall::process::` paths
pub(crate) use super::process_clone::sys_clone;
pub(crate) use super::process_exec::{sys_execve, sys_execve_with_path};

// Re-export clone namespace constants needed by process_clone.rs
pub(in crate::syscall) const CLONE_NEWNS:     usize = 0x0002_0000;
pub(in crate::syscall) const CLONE_NEWCGROUP: usize = 0x0200_0000;
pub(in crate::syscall) const CLONE_NEWUTS:    usize = 0x0400_0000;
pub(in crate::syscall) const CLONE_NEWIPC:    usize = 0x0800_0000;
pub(in crate::syscall) const CLONE_NEWUSER:   usize = 0x1000_0000;
pub(in crate::syscall) const CLONE_NEWPID:    usize = 0x2000_0000;
pub(in crate::syscall) const CLONE_NEWNET:    usize = 0x4000_0000;
pub(in crate::syscall) const CLONE_NEW_MASK:  usize = CLONE_NEWNS | CLONE_NEWCGROUP | CLONE_NEWUTS
    | CLONE_NEWIPC | CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET;

/// 退出进程
#[allow(dead_code)]
pub(in crate::syscall) fn sys_exit(exit_code: i32) -> isize {
    {
        let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = table_guard.as_mut() {
            if let Some(pid) = table.current {
                do_clear_child_tid(pid, table);
                remove_pid_from_futex_waiters(pid);
            }
        }
    }
    crate::sched::process::exit(exit_code);
}

/// 退出进程组（musl libc 的 exit() 最终调用此 syscall）
#[allow(dead_code)]
pub(in crate::syscall) fn sys_exit_group(exit_code: i32) -> isize {
    use crate::sched::process::PROCESS_TABLE;
    let mut dump_sched_trace = false;
    let mut closed_fds_pending: Vec<(usize, Vec<alloc::sync::Arc<crate::fs::FileDescriptor>>)> = Vec::new();
    let mut posix_lock_release_pids: Vec<usize> = Vec::new();
    {
        let mut table_guard = PROCESS_TABLE.lock();
        if let Some(table) = table_guard.as_mut() {
            if let Some(current_pid) = table.current {
                do_clear_child_tid(current_pid, table);
                remove_pid_from_futex_waiters(current_pid);
                let (tgid, policy, priority) = table.get(current_pid)
                    .map(|p| (p.tgid, p.sched_policy, p.sched_priority))
                    .unwrap_or((current_pid, 0, 0));
                crate::sched::trace::record_event(
                    crate::sched::trace::EV_EXIT_GROUP,
                    current_pid,
                    tgid,
                    policy,
                    priority,
                    exit_code as i64 as u64,
                    0,
                );
                if crate::sched::trace::is_focus_tgid(tgid) {
                    dump_sched_trace = true;
                }
                let siblings: Vec<usize> = table.pids().into_iter()
                    .filter(|&pid| pid != current_pid && table.get(pid).map_or(false, |p| p.tgid == tgid && p.is_thread))
                    .collect();
                for sib in siblings {
                    do_clear_child_tid(sib, table);
                    remove_pid_from_futex_waiters(sib);
                    table.transfer_fd_table_owner_on_exit(sib);
                    if let Some(p) = table.get_mut(sib) {
                        let closed = p.release_resources();
                        if !closed.is_empty() {
                            closed_fds_pending.push((sib, closed));
                        }
                        posix_lock_release_pids.push(sib);
                    }
                    table.mark_zombie(sib, crate::sched::process::ExitReason::Normal(exit_code));
                }
                posix_lock_release_pids.push(tgid);
            }
        }
    }
    for (pid, fds) in core::mem::take(&mut closed_fds_pending) {
        for fd in fds {
            crate::fs::fd::finalize_closed_fd_for_pid(fd, pid);
        }
    }
    for pid in core::mem::take(&mut posix_lock_release_pids) {
        crate::fs::file_lock::release_all_locks_for_pid(pid);
    }
    if dump_sched_trace {
        crate::sched::trace::dump_once();
    }
    crate::sched::process::exit(exit_code);
}

/// 对进程/线程执行 clear_child_tid: 写 0 到 clear_child_tid 地址并 futex wake
pub(in crate::syscall) fn do_clear_child_tid(pid: usize, table: &mut crate::sched::process::ProcessTable) {
    let (addr, tgid) = {
        let p = match table.get(pid) {
            Some(p) => p,
            None => return,
        };
        let a = p.clear_child_tid;
        if a == 0 { return; }
        (a, p.tgid)
    };
    if let Some(p) = table.get_mut(pid) {
        p.clear_child_tid = 0;
    }
    let target_pid = if table.get(pid).and_then(|p| p.mm.as_ref()).is_some() {
        pid
    } else {
        tgid
    };
    if let Some(target) = table.get(target_pid) {
        if let Some(ref mm) = target.mm {
            let mut addr_space = mm.lock();
            let va = crate::mm::VirtAddr(addr);
            let vpn = va.floor();
            let offset = va.page_offset();
            match addr_space.page_table().get_pte_flags(vpn) {
                Some(f) if f.contains(crate::mm::PTEFlags::W) => {}
                // 只读（CoW）或未建映射（懒页）：先走写缺页补页
                _ => { addr_space.handle_page_fault(va, true); }
            }
            if let Some(ppn) = addr_space.page_table().translate(vpn) {
                let kva = crate::mm::phys_to_kernel_vaddr(ppn.0 * 4096 + offset);
                unsafe { core::ptr::write_volatile(kva as *mut i32, 0); }
            }
        }
    }
    let to_wake: Vec<usize> = {
        let mut waiters = FUTEX_WAITERS.lock();
        if let Some(queue) = waiters.remove(&addr) {
            queue.drain().into_iter().map(|entry| entry.pid).collect()
        } else {
            Vec::new()
        }
    };
    if crate::cmdline::ftxtrace_enabled() {
        // 线程退出写 0 到 tid 字并唤醒 joiner：这是 pthread_join 的唤醒来源，
        // 与普通 FUTEX_WAKE 分开记，便于判断丢的是哪一类唤醒。
        crate::println!(
            "[FTX] cleartid pid={} uaddr={:#x} drained={} t={}",
            pid, addr, to_wake.len(), crate::arch::asm_helpers::rdtime()
        );
    }
    // 等待者已被无条件摘出队列，唤醒标记也必须无条件落下：若它此刻还没走到
    // Sleeping（已入队、尚未让出），旧写法既摘了队又不做标记，此后再无任何人
    // 能找到它——pthread_join 在 8 核下就是这样永久阻塞的。置位后由
    // suspend_current_for_io_inner 的 prepare_to_wait 契约消费。
    for wake_pid in to_wake {
        let state = match table.get(wake_pid) {
            Some(p) => p.state,
            None => continue,
        };
        if matches!(
            state,
            crate::sched::process::ProcessState::Zombie
                | crate::sched::process::ProcessState::Unused
        ) {
            continue;
        }
        if let Some(p) = table.get_mut(wake_pid) {
            p.waiting_for = Some(crate::sched::process::FUTEX_WOKEN_SENTINEL);
            p.wakeup_channel = 0;
            p.waitqueue_woken = true;
        }
        if state == crate::sched::process::ProcessState::Sleeping {
            table.mark_runnable(wake_pid, true);
        }
    }
}
