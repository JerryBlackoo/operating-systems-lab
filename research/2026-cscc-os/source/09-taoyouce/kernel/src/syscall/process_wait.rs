//! wait/waitid/waitpid/wait4 syscall glue — child process reaping.

use super::user::write_user_buffer;
use super::SyscallError;

/// waitid(idtype, id, infop, options, ru) — POSIX 1003.1-2008
#[allow(dead_code)]
pub(in crate::syscall) fn sys_waitid(
    idtype: usize,
    id: usize,
    infop: *mut u8,
    options: usize,
    ru: usize,
) -> isize {
    use crate::sched::process::{self, Pid, WaitTarget};

    const P_ALL: usize   = 0;
    const P_PID: usize   = 1;
    const P_PGID: usize  = 2;
    const P_PIDFD: usize = 3;

    const WNOHANG:     usize = 1;
    const WSTOPPED:    usize = 2;
    const WEXITED:     usize = 4;
    const WCONTINUED:  usize = 8;
    const WNOWAIT:     usize = 0x0100_0000;
    const __WCLONE:    usize = 0x8000_0000;
    const __WALL:      usize = 0x4000_0000;
    const KNOWN_OPTS:  usize =
        WNOHANG | WSTOPPED | WEXITED | WCONTINUED | WNOWAIT | __WCLONE | __WALL;

    if options & !KNOWN_OPTS != 0 {
        return -22; // -EINVAL
    }
    if options & (WEXITED | WSTOPPED | WCONTINUED) == 0 {
        return -22; // -EINVAL
    }

    let target: WaitTarget = match idtype {
        P_ALL  => WaitTarget::Any,
        P_PID  => {
            if id == 0 { WaitTarget::Any } else { WaitTarget::Pid(id as Pid) }
        }
        P_PGID => {
            let pgid = if id == 0 {
                let table = process::PROCESS_TABLE.lock();
                let cur = table.as_ref().and_then(|t| t.current).unwrap_or(0);
                table.as_ref()
                    .and_then(|t| t.get(cur))
                    .map(|p| p.pgid)
                    .unwrap_or(0)
            } else {
                id as Pid
            };
            if pgid == 0 { return -22; }
            WaitTarget::Pgid(pgid)
        }
        P_PIDFD => {
            let pid_from_fd: Option<usize> = {
                let table = process::PROCESS_TABLE.lock();
                let cur = table.as_ref().and_then(|t| t.current);
                cur.and_then(|cp| {
                    table.as_ref()
                        .and_then(|t| t.get_fd_process(cp))
                        .and_then(|proc| crate::fs::fd::get(proc, id))
                })
                .and_then(|fd_obj| {
                    fd_obj.path.as_ref()
                        .and_then(|p| p.strip_prefix("pidfd:").map(alloc::string::String::from))
                })
                // path 形如 "pidfd:<pid>:<instance_id>"（旧形式无 :<instance_id>）
                .and_then(|s| s.split(':').next().unwrap_or(&s).parse::<usize>().ok())
            };
            match pid_from_fd {
                Some(p) if p != 0 => WaitTarget::Pid(p as Pid),
                _ => return -9, // -EBADF
            }
        }
        _ => return -22,
    };

    let nowait = options & WNOWAIT != 0;
    let nohang = options & WNOHANG != 0;
    let wait_exited = options & WEXITED != 0;
    let wait_stopped = options & WSTOPPED != 0;
    let wait_continued = options & WCONTINUED != 0;

    let thread_mode = if options & __WALL != 0 {
        process::ThreadMode::All
    } else if options & __WCLONE != 0 {
        process::ThreadMode::CloneOnly
    } else {
        process::ThreadMode::NonThreadOnly
    };

    match process::waitid_reap_filtered(
        target,
        thread_mode,
        wait_exited,
        wait_stopped,
        wait_continued,
        nowait,
    ) {
        Err(process::WaitidErr::NoChildren) => return -10, // -ECHILD
        Ok(Some((cpid, event, uid, cpu_u, cpu_k))) => {
            let mut siginfo = [0u8; 128];
            crate::sched::process::build_waitid_siginfo_pub(
                &mut siginfo, cpid, event, uid, cpu_u, cpu_k,
            );
            if !infop.is_null() {
                let _ = write_user_buffer(infop, &siginfo);
            }
            if ru != 0 {
                let rusage_buf = process::build_rusage_buf_from_ticks(cpu_u, cpu_k);
                let _ = write_user_buffer(ru as *mut u8, &rusage_buf);
            }
            return 0;
        }
        Ok(None) => {
            // 有匹配 child 但都还在 running
        }
    }

    if nohang {
        if !infop.is_null() {
            let zero = [0u8; 128];
            let _ = write_user_buffer(infop, &zero);
        }
        return 0;
    }

    // 真阻塞
    {
        let mut table = process::PROCESS_TABLE.lock();
        let table = match table.as_mut() {
            Some(t) => t,
            None => return -10,
        };
        let cur = match table.current {
            Some(p) => p,
            None => return -10,
        };
        if let Some(parent) = table.get_mut(cur) {
            parent.wait_siginfo_ptr = infop as usize;
            parent.wait_rusage_ptr_waitid = ru;
            parent.wait_is_waitid = true;
            parent.wait_wnowait = nowait;
        }
    }
    process::wait_for_waitid(
        target,
        thread_mode,
        wait_exited,
        wait_stopped,
        wait_continued,
        nowait,
    );
}

/// sys_waitpid / wait4(pid, status, options, rusage)
#[allow(dead_code)]
pub(in crate::syscall) fn sys_waitpid(
    pid: isize,
    exit_code_ptr: *mut i32,
    options: usize,
    rusage_ptr: *mut u8,
) -> isize {
    use crate::sched::process::{self, Pid, WaitTarget};

    const WNOHANG:    usize = 0x01;
    const WUNTRACED:  usize = 0x02;
    const WCONTINUED: usize = 0x08;
    const __WALL:     usize = 0x4000_0000;
    const __WCLONE:   usize = 0x8000_0000;
    const KNOWN: usize = WNOHANG | WUNTRACED | WCONTINUED | __WALL | __WCLONE;
    if options & !KNOWN != 0 {
        return -22; // -EINVAL
    }
    if pid == i32::MIN as isize {
        return -3;
    }
    if crate::cmdline::waitdbg_enabled()
    {
        crate::println!(
            "[waitarg] pid={} status_ptr={:#x} options={:#x} rusage_ptr={:#x}",
            pid,
            exit_code_ptr as usize,
            options,
            rusage_ptr as usize,
        );
    }
    let nohang = options & WNOHANG != 0;
    let want_all = options & __WALL != 0;
    let want_clone = options & __WCLONE != 0;
    let thread_mode: process::ThreadMode = if want_all {
        process::ThreadMode::All
    } else if want_clone {
        process::ThreadMode::CloneOnly
    } else {
        process::ThreadMode::NonThreadOnly
    };
    let wait_stopped = options & WUNTRACED != 0;
    let wait_continued = options & WCONTINUED != 0;

    let target: WaitTarget = if pid == -1 {
        WaitTarget::Any
    } else if pid > 0 {
        WaitTarget::Pid(pid as Pid)
    } else if pid == 0 {
        let self_pgid = {
            let table = process::PROCESS_TABLE.lock();
            table
                .as_ref()
                .and_then(|t| t.current.and_then(|cp| t.get(cp).map(|p| p.pgid)))
                .unwrap_or(0)
        };
        if self_pgid == 0 {
            WaitTarget::Any
        } else {
            WaitTarget::Pgid(self_pgid)
        }
    } else {
        let want = (-pid) as Pid;
        if want == 0 {
            WaitTarget::Any
        } else {
            WaitTarget::Pgid(want)
        }
    };

    if nohang {
        match process::wait_nonblock(target, thread_mode, wait_stopped, wait_continued) {
            Ok(Some((child_pid, wstatus, cu, ck))) => {
                if !exit_code_ptr.is_null() {
                    if let Err(_) = write_user_buffer(
                        exit_code_ptr as *mut u8,
                        &wstatus.to_ne_bytes(),
                    ) {
                        return SyscallError::InvalidArgument as isize;
                    }
                }
                if !rusage_ptr.is_null() {
                    let rusage_buf = process::build_rusage_buf_from_ticks(cu, ck);
                    if let Err(_) = write_user_buffer(rusage_ptr, &rusage_buf) {
                        return SyscallError::InvalidArgument as isize;
                    }
                }
                child_pid as isize
            }
            Ok(None) => super::SYSCALL_YIELD_SENTINEL,
            Err(_) => -10, // -ECHILD
        }
    } else {
        process::wait(
            target,
            thread_mode,
            wait_stopped,
            wait_continued,
            exit_code_ptr as usize,
            rusage_ptr as usize,
        );
    }
}
