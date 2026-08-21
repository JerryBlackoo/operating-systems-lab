use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::fs::FileDescriptor;
use crate::sched::process::ProcessControlBlock;

fn cloexec_mask(fd: usize) -> Option<u64> {
    if fd < 64 {
        Some(1u64 << fd)
    } else {
        None
    }
}

pub fn get(process: &ProcessControlBlock, fd: usize) -> Option<Arc<FileDescriptor>> {
    process.fd_table.get(fd).and_then(|entry| entry.clone())
}

pub fn is_open(process: &ProcessControlBlock, fd: usize) -> bool {
    process.fd_table.get(fd).map_or(false, |entry| entry.is_some())
}

pub fn alloc(
    process: &mut ProcessControlBlock,
    fd_obj: Arc<FileDescriptor>,
    min_fd: usize,
    enforce_rlimit: bool,
) -> Result<usize, isize> {
    if enforce_rlimit && min_fd >= process.rlimit_nofile {
        return Err(-24);
    }

    let search_start = if min_fd == 0 {
        process.fd_next_hint.min(process.fd_table.len())
    } else {
        min_fd
    };

    if let Some(fd) = (search_start..process.fd_table.len()).find(|&fd| process.fd_table[fd].is_none()) {
        fd_obj.inc_fd_ref();
        process.fd_table[fd] = Some(fd_obj);
        clear_cloexec(process, fd);
        if min_fd == 0 && fd == process.fd_next_hint {
            process.fd_next_hint = fd + 1;
        }
        return Ok(fd);
    }

    let fd = process.fd_table.len().max(min_fd);
    if enforce_rlimit && fd >= process.rlimit_nofile {
        return Err(-24);
    }

    while process.fd_table.len() < fd {
        process.fd_table.push(None);
    }
    fd_obj.inc_fd_ref();
    process.fd_table.push(Some(fd_obj));
    clear_cloexec(process, fd);
    if min_fd == 0 && fd == process.fd_next_hint {
        process.fd_next_hint = fd + 1;
    }
    Ok(fd)
}

pub fn install_at(
    process: &mut ProcessControlBlock,
    fd: usize,
    fd_obj: Arc<FileDescriptor>,
    max_fd: Option<usize>,
    enforce_rlimit: bool,
) -> Result<(usize, Option<Arc<FileDescriptor>>), isize> {
    if max_fd.map_or(false, |max| fd > max) {
        return Err(-22);
    }
    if enforce_rlimit && fd >= process.rlimit_nofile {
        return Err(-24);
    }

    while process.fd_table.len() <= fd {
        process.fd_table.push(None);
    }
    let displaced = process.fd_table[fd].take();
    fd_obj.inc_fd_ref();
    process.fd_table[fd] = Some(fd_obj);
    clear_cloexec(process, fd);
    if displaced.is_none() && fd < process.fd_next_hint {
        process.fd_next_hint = fd;
    }
    if fd == process.fd_next_hint {
        process.fd_next_hint = fd + 1;
    }
    Ok((fd, displaced))
}

pub fn duplicate(
    process: &mut ProcessControlBlock,
    old_fd: usize,
    min_fd: usize,
    enforce_rlimit: bool,
) -> Result<usize, isize> {
    let fd_obj = get(process, old_fd).ok_or(-9isize)?;
    alloc(process, fd_obj, min_fd, enforce_rlimit)
}

pub fn close(process: &mut ProcessControlBlock, fd: usize) -> Result<Arc<FileDescriptor>, isize> {
    if !is_open(process, fd) {
        return Err(-9);
    }
    let fd_obj = process.fd_table[fd].take().ok_or(-9isize)?;
    clear_cloexec(process, fd);
    if fd < process.fd_next_hint {
        process.fd_next_hint = fd;
    }
    Ok(fd_obj)
}

pub fn is_cloexec(process: &ProcessControlBlock, fd: usize) -> bool {
    match cloexec_mask(fd) {
        Some(mask) => process.cloexec_bits & mask != 0,
        None => process.cloexec_fds.contains(&fd),
    }
}

pub fn set_cloexec(process: &mut ProcessControlBlock, fd: usize, enabled: bool) -> Result<(), isize> {
    if !is_open(process, fd) {
        return Err(-9);
    }
    if let Some(mask) = cloexec_mask(fd) {
        if enabled {
            process.cloexec_bits |= mask;
        } else {
            process.cloexec_bits &= !mask;
        }
    } else {
        if enabled {
            process.cloexec_fds.insert(fd);
        } else {
            process.cloexec_fds.remove(&fd);
        }
    }
    Ok(())
}

pub fn clear_cloexec(process: &mut ProcessControlBlock, fd: usize) {
    if let Some(mask) = cloexec_mask(fd) {
        process.cloexec_bits &= !mask;
    } else {
        process.cloexec_fds.remove(&fd);
    }
}

/// Final-close finalizer used by `sys_close`, `sys_close_range`, `sys_dup3`
/// displaced slots, and `execve` CLOEXEC cleanup. Must be called *without*
/// holding `PROCESS_TABLE`, since the underlying `close_notify_last`
/// implementations and `file_lock::release_*` paths may re-acquire the
/// process table to wake blocked readers/writers/lock waiters.
///
/// `pid` is the process whose ownership we're closing on behalf of. It's
/// needed because POSIX advisory lock release happens *per process* (not
/// per fd) and execve's CLOEXEC and process-exit paths may close fds for
/// processes other than the one that is currently scheduled.
///
/// Operations performed:
///   1. POSIX advisory locks owned by `pid` on this inode are released —
///      POSIX requires this even when other fds still reference the file.
///   2. Drop one fd-table reference from the open file description.  If this
///      was the last userspace-visible descriptor, run final-close cleanup
///      immediately; transient syscall `Arc` clones do not delay EOF/HUP.
pub fn finalize_closed_fd_for_pid(fd_obj: Arc<FileDescriptor>, pid: usize) {
    let inode_id = fd_obj.file.ino();
    // POSIX semantics: closing any fd to the file releases every POSIX lock
    // the calling process owns on that inode, even when other fds remain
    // open elsewhere in the same process or its children.
    crate::fs::file_lock::release_posix_locks_for_inode_pid(inode_id, pid);

    // ETXTBSY: release write-access tracking when a writable fd is closed.
    // This enables the execve(2) ↔ open(O_WRONLY) bidirectional ETXTBSY check.
    let acc = fd_obj.flags & 0o3; // O_ACCMODE
    if (acc == 1 || acc == 2) && fd_obj.file.file_type() == crate::fs::FileType::Regular {
        crate::fs::put_write_access(inode_id);
    }

    fd_obj.dec_fd_ref_and_notify_if_last();
}

/// Convenience wrapper around `finalize_closed_fd_for_pid` that looks up the
/// currently-scheduled pid. Use this from syscall paths where the caller is
/// the owning process (sys_close, sys_dup3, sys_close_range, execve).
pub fn finalize_closed_fd(fd_obj: Arc<FileDescriptor>) {
    // 快路径：per-hart HART_CURRENT_PID 就是本次 syscall 正在执行的进程，
    // 由调度提交点发布、无需中央表锁。仅在影子为 0 的极早期回退表锁。
    let pid = {
        let pid = crate::sched::process_sched::HART_CURRENT_PID
            .load(core::sync::atomic::Ordering::Acquire);
        if pid != 0 {
            pid
        } else {
            let t = crate::sched::process::PROCESS_TABLE.lock();
            t.as_ref().and_then(|tt| tt.current).unwrap_or(0)
        }
    };
    finalize_closed_fd_for_pid(fd_obj, pid);
}

pub fn apply_cloexec_on_exec(process: &mut ProcessControlBlock) -> Vec<Arc<FileDescriptor>> {
    let bits = process.cloexec_bits;
    if bits == 0 && process.cloexec_fds.is_empty() {
        return Vec::new();
    }
    let mut closed = Vec::new();
    for fd in 0..64.min(process.fd_table.len()) {
        if bits & (1u64 << fd) != 0 {
            if let Some(fd_obj) = process.fd_table[fd].take() {
                if fd < process.fd_next_hint {
                    process.fd_next_hint = fd;
                }
                closed.push(fd_obj);
            }
        }
    }
    let high_fds: Vec<usize> = process.cloexec_fds.iter().copied().collect();
    for fd in high_fds {
        if let Some(fd_obj) = process.fd_table.get_mut(fd).and_then(|slot| slot.take()) {
            if fd < process.fd_next_hint {
                process.fd_next_hint = fd;
            }
            closed.push(fd_obj);
        }
    }
    process.cloexec_bits = 0;
    process.cloexec_fds.clear();
    closed
}
