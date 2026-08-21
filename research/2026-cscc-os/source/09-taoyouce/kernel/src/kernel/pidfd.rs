//! Linux-aligned pidfd helpers and syscall entry points.

use alloc::sync::Arc;

use crate::fs::fd as fd_table;

/// Helper: create a pidfd for a given pid and install it in the current process fd table.
/// Returns the fd number on success, negative errno on failure.
/// Used by both pidfd_open and clone3(CLONE_PIDFD).
/// `open_flags`: raw O_* flags to store on the fd (e.g. O_NONBLOCK).
pub(crate) fn create_pidfd_for_pid(target: usize, open_flags: u32) -> isize {
    static PIDFD_INO: core::sync::atomic::AtomicUsize =
        core::sync::atomic::AtomicUsize::new(0x7000_0000);
    let ino = PIDFD_INO.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let inode: Arc<dyn crate::fs::Inode> =
        Arc::new(crate::fs::ramfs::RamInode::new_memfd(ino, false));
    // Bind the pidfd to the target's process *instance*, not just its numeric
    // PID. The instance id lets pidfd_send_signal reject a stale pidfd whose
    // PID was later reused by a different process (Linux `struct pid` semantics;
    // LTP pidfd_send_signal03). Path form: "pidfd:<pid>:<instance_id>".
    let instance_id = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table
            .as_ref()
            .and_then(|t| t.get(target))
            .map(|p| p.instance_id)
            .unwrap_or(0)
    };
    let mut path = alloc::string::String::from("pidfd:");
    path.push_str(&alloc::format!("{}:{}", target, instance_id));
    let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
        inode,
        open_flags,
        Some(path),
    ));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                return match fd_table::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        let _ = fd_table::set_cloexec(process, fd, true);
                        fd as isize
                    }
                    Err(e) => e,
                };
            }
        }
    }
    -22
}

/// pidfd_open(pid, flags) — delegates to create_pidfd_for_pid after validation.
#[allow(dead_code)]
pub(crate) fn sys_pidfd_open(pid: isize, flags: usize) -> isize {
    const PIDFD_NONBLOCK: usize = 0o4000;
    const O_NONBLOCK: u32 = 0o4000;
    if pid <= 0 {
        return -22;
    } // -EINVAL
    if flags & !PIDFD_NONBLOCK != 0 {
        return -22;
    }
    let target = pid as usize;
    {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let alive = table
            .as_ref()
            .and_then(|t| t.get(target))
            .map(|p| p.state != crate::sched::process::ProcessState::Zombie)
            .unwrap_or(false);
        if !alive {
            return -3;
        } // -ESRCH
    }
    let open_flags = if flags & PIDFD_NONBLOCK != 0 {
        O_NONBLOCK | 0o2
    } else {
        0o2
    };
    create_pidfd_for_pid(target, open_flags)
}

/// 从 pidfd 的 path 中还原目标 pid 及（可选的）绑定实例 id。
/// 支持三种形式：
///   1. "pidfd:<pid>:<instance_id>" — sys_pidfd_open / clone3 创建的匿名 fd
///   2. "pidfd:<pid>" — 兼容旧形式（无实例绑定）
///   3. "/proc/<pid>" 或 "/proc/<pid>/" — open(/proc/self) 形式（older Linux API）
/// 返回 `(pid, Some(instance_id))`；实例 id 缺省（旧形式 / /proc 形式）为 `None`。
fn pidfd_resolve(pidfd: usize) -> Option<(usize, Option<u64>)> {
    let fd_obj = crate::syscall::fd::get_fd_obj(pidfd)?;
    let path = fd_obj.path.as_ref()?;
    if let Some(rest) = path.strip_prefix("pidfd:") {
        if let Some((pid_str, iid_str)) = rest.split_once(':') {
            let pid = pid_str.parse::<usize>().ok()?;
            let iid = iid_str.parse::<u64>().ok();
            return Some((pid, iid));
        }
        return Some((rest.parse::<usize>().ok()?, None));
    }
    let stripped = path.strip_prefix("/proc/")?;
    let pid_part = stripped.strip_suffix('/').unwrap_or(stripped);
    if pid_part == "self" {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        return table.as_ref().and_then(|t| t.current).map(|p| (p, None));
    }
    let pid = pid_part.parse::<usize>().ok()?;
    // A `/proc/<pid>` directory fd carries its instance binding in the fd
    // object (recorded at open time), not in the path string. 0 means unbound.
    let iid = fd_obj.pidfd_instance.load(core::sync::atomic::Ordering::SeqCst);
    Some((pid, if iid != 0 { Some(iid) } else { None }))
}

/// 解析 pidfd 并校验其绑定的进程实例仍然存活。返回目标 pid；若 pidfd 无效返回
/// `Err(-9)`（EBADF），若绑定实例已退出/被回收、或 PID 被另一进程实例复用则返回
/// `Err(-3)`（ESRCH）。
fn pidfd_resolve_live(pidfd: usize) -> Result<usize, isize> {
    let (pid, want_iid) = pidfd_resolve(pidfd).ok_or(-9isize)?;
    if let Some(want) = want_iid {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let cur_iid = table
            .as_ref()
            .and_then(|t| t.get(pid))
            .map(|p| p.instance_id);
        match cur_iid {
            Some(iid) if iid == want => Ok(pid),
            _ => Err(-3), // PID 已被复用或原实例已消失
        }
    } else {
        Ok(pid)
    }
}

/// pidfd_send_signal(pidfd, sig, info, flags) — signal delivery via pidfd.
/// When info is non-null, copies the user-provided siginfo and enqueues it
/// so that SA_SIGINFO handlers receive the correct si_value/si_pid/si_code.
#[allow(dead_code)]
pub(crate) fn sys_pidfd_send_signal(
    pidfd: usize,
    sig: i32,
    info: *const u8,
    flags: usize,
) -> isize {
    if flags != 0 {
        return -22;
    }
    if sig < 0 || sig > 64 {
        return -22;
    }
    let pid = match pidfd_resolve_live(pidfd) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // When a siginfo is supplied, copy it in and reject a signal number that
    // disagrees with si_signo (Linux pidfd_send_signal: `if (sig !=
    // kinfo.si_signo) return -EINVAL`). The payload is only enqueued after the
    // permission-checked delivery below succeeds, so a denied send never leaks
    // a queued siginfo into the target.
    let mut siginfo_buf = None;
    if sig > 0 && !info.is_null() {
        let buf = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_ref() {
                Some(t) => t,
                None => return -3,
            };
            let current = match t.current {
                Some(c) => c,
                None => return -3,
            };
            let cur_pcb = match t.get(current) {
                Some(p) => p,
                None => return -3,
            };
            let mm = match cur_pcb.mm.as_ref() {
                Some(m) => m,
                None => return -14,
            };
            let addr_space = mm.lock();
            match crate::kernel::signal::copy_siginfo_from_user(&*addr_space, info as usize) {
                Some(b) => b,
                None => return -14,
            }
        };
        let si_signo = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if si_signo != sig {
            return -22;
        }
        siginfo_buf = Some(buf);
    }
    // Enqueue the payload BEFORE the send: the kill path only synthesizes its
    // SI_USER siginfo when no entry for `sig` is queued yet, and delivery pops
    // FIFO — enqueueing after would leave a zero-si_value SI_USER entry ahead
    // of ours and the handler would read si_value=0 (clone301). On a denied /
    // failed send the speculative entry is removed again so nothing leaks.
    if let Some(buf) = siginfo_buf {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            let _ = crate::kernel::signal::enqueue_pending_siginfo_locked(t, pid, sig, buf);
        }
    }
    let ret = crate::kernel::signal::sys_kill(pid as isize, sig);
    if ret != 0 && !info.is_null() && sig > 0 {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            if let Some(p) = t.get_mut(pid) {
                if let Some(pos) = p.pending_siginfo.iter().rposition(|(s, _)| *s == sig) {
                    p.pending_siginfo.remove(pos);
                }
            }
        }
    }
    ret
}

/// pidfd_getfd(pidfd, targetfd, flags) — 真从对端进程 fd 表复制 fd 到本进程。
#[allow(dead_code)]
pub(crate) fn sys_pidfd_getfd(pidfd: usize, targetfd: usize, flags: usize) -> isize {
    if flags != 0 {
        return -22;
    }
    let target_pid = match pidfd_resolve_live(pidfd) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() {
        Some(t) => t,
        None => return -3,
    };
    let cur_pid = match t.current {
        Some(p) => p,
        None => return -3,
    };
    {
        let caller = match t.get(cur_pid) {
            Some(p) => p,
            None => return -3,
        };
        let target = match t.get(target_pid) {
            Some(p) => p,
            None => return -3,
        };
        let privileged = caller.euid == 0;
        let same_user = caller.uid == target.uid && caller.uid == target.euid;
        if !privileged && !same_user {
            return -1;
        } // -EPERM
    }
    let fd_obj = {
        let target_proc = match t.get(target_pid) {
            Some(p) => p,
            None => return -3,
        };
        match fd_table::get(target_proc, targetfd) {
            Some(f) => f,
            None => return -9,
        }
    };
    let process = match t.get_fd_process_mut(cur_pid) {
        Some(p) => p,
        None => return -3,
    };
    match fd_table::alloc(process, fd_obj, 0, true) {
        Ok(fd) => {
            let _ = fd_table::set_cloexec(process, fd, true);
            fd as isize
        }
        Err(e) => e,
    }
}
