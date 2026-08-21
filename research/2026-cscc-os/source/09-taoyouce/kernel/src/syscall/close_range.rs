//! Linux close_range(2) — close a range of file descriptors atomically.

/// close_range(2): Linux 5.9+ with CLOSE_RANGE_UNSHARE and CLOSE_RANGE_CLOEXEC.
pub(in crate::syscall) fn sys_close_range(first: u32, last: u32, flags: u32) -> isize {
    const CLOSE_RANGE_UNSHARE: u32 = 0x02;
    const CLOSE_RANGE_CLOEXEC: u32 = 0x04;
    const ALLOWED: u32 = CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC;
    if first > last { return -22; }
    if flags & !ALLOWED != 0 { return -22; }

    // CLOSE_RANGE_CLOEXEC: only mark CLOEXEC, don't actually close
    if flags & CLOSE_RANGE_CLOEXEC != 0 {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                if flags & CLOSE_RANGE_UNSHARE != 0 && !t.unshare_fd_table_for_pid(pid) {
                    return -9;
                }
                if let Some(process) = t.get_fd_process_mut(pid) {
                    let first = first as usize;
                    if first < process.fd_table.len() {
                        let last = (last as usize).min(process.fd_table.len().saturating_sub(1));
                        for fd in first..=last {
                            let _ = crate::fs::fd::set_cloexec(process, fd, true);
                        }
                    }
                }
            }
        }
        return 0;
    }

    // Default path: close [first, last] range.
    // CLOSE_RANGE_UNSHARE is noop in our model (fork defaults to unshared).
    let closed: alloc::vec::Vec<alloc::sync::Arc<crate::fs::FileDescriptor>> = {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let mut closed = alloc::vec::Vec::new();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                if flags & CLOSE_RANGE_UNSHARE != 0 && !t.unshare_fd_table_for_pid(pid) {
                    return -9;
                }
                if let Some(process) = t.get_fd_process_mut(pid) {
                    let first = first as usize;
                    if first < process.fd_table.len() {
                        let last = (last as usize).min(process.fd_table.len().saturating_sub(1));
                        for fd in first..=last {
                            if let Ok(fd_obj) = crate::fs::fd::close(process, fd) {
                                closed.push(fd_obj);
                            }
                        }
                    }
                }
            }
        }
        closed
    };
    for fd_obj in closed {
        crate::fs::fd::finalize_closed_fd(fd_obj);
    }
    0
}
