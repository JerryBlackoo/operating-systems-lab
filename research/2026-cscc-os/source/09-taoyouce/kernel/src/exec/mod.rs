use alloc::string::String;
use alloc::vec::Vec;

mod error;
mod cache;
mod image;
mod path;
mod process;
mod script;
mod stack;
mod trap;

use error::{exec_error_to_errno, ExecError};
use script::ExecRequest;

pub(crate) use path::current_process_exec_path;

pub(crate) fn execve_from_kernel_args(path_str: String, args: Vec<String>, envp: Vec<String>) -> isize {
    // Linux execve(2) file checks (inode_permission(MAY_EXEC) + format probe) on
    // the resolved target: EACCES (no execute permission), ENOEXEC (empty
    // executable), ENOTDIR/ENAMETOOLONG/ELOOP (path resolution). ENOENT (-2)
    // falls through so `resolve_exec_request` reports it uniformly.
    // (execve02/execve03.)
    let resolved_target = path::resolve_exec_path(&path_str);
    if let Some(err) = crate::syscall::path::may_exec_errno(&resolved_target) {
        if err != -2 {
            return err;
        }
    }

    let ExecRequest { exec_target, exec_name, exec_args } = match script::resolve_exec_request(&path_str, args) {
        Ok(request) => request,
        Err(error) => return error as isize,
    };

    // Reformatting a device (mkfs.*) must discard any persistent filesystem the
    // kernel kept for that device, so the next mount sees an empty fs (matching
    // real mkfs). Without this, fanotify10's mkfs between cycles would wrongly
    // resurrect stale files from the device-fs registry.
    maybe_reset_device_fs_on_mkfs(&exec_name, &exec_args);

    match image::do_exec_from_fs_path(&exec_target, &exec_name, &exec_args, &envp) {
        Ok(_entry_info) => {
            // Linux flush_thread 语义：exec 成功后浮点上下文清零。
            #[cfg(target_arch = "loongarch64")]
            {
                let pid = crate::sched::process_sched::HART_CURRENT_PID
                    .load(core::sync::atomic::Ordering::Relaxed);
                if pid != 0 {
                    crate::arch::loongarch64::fpu::exec_reset(pid);
                }
            }
            drop(exec_args);
            drop(envp);
            drop(path_str);
            drop(exec_target);
            drop(exec_name);

            trap::return_to_user_after_exec();
        }
        Err(e) => {
            if !matches!(&e, ExecError::LoadFailed(crate::loader::LoaderError::InvalidMagic)) {
                crate::println!("[exec] sys_exec: failed: {:?}", e);
            }
            exec_error_to_errno(&e)
        }
    }
}

/// If `exec_name` is an mkfs-family applet, drop the kernel's persistent fs for
/// every device argument so the reformatted device mounts empty next time.
fn maybe_reset_device_fs_on_mkfs(exec_name: &str, args: &[String]) {
    // basename of the program (handles /sbin/mke2fs and bare mke2fs alike).
    let base = exec_name.rsplit('/').next().unwrap_or(exec_name);
    let is_mkfs = base == "mke2fs"
        || base == "mkdosfs"
        || base == "mkswap"
        || base.starts_with("mkfs");
    if !is_mkfs {
        return;
    }
    // The device is the positional argument(s); reset any arg that names a
    // device path. We skip option flags and their inline values.
    for arg in args.iter().skip(1) {
        if arg.starts_with('/') {
            crate::syscall::mount::device_fs_reset(arg);
        }
    }
}

pub(crate) fn sys_execveat(
    dirfd: isize,
    pathname: *const u8,
    argv: *const usize,
    envp: *const usize,
    flags: i32,
) -> isize {
    const AT_FDCWD: isize = -100;
    const AT_EMPTY_PATH: i32 = 0x1000;
    const AT_SYMLINK_NOFOLLOW: i32 = 0x100;
    const VALID_FLAGS: i32 = AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW;

    if flags & !VALID_FLAGS != 0 {
        return -22;
    }

    let path_str = match crate::syscall::user::read_path(pathname) {
        Ok(s) => s,
        Err(e) => {
            if flags & AT_EMPTY_PATH != 0 && pathname.is_null() {
                return -14;
            }
            return e;
        }
    };

    if (flags & AT_EMPTY_PATH) != 0 && path_str.is_empty() {
        if dirfd < 0 && dirfd != AT_FDCWD {
            return -9;
        }
        let resolved = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref t) = *table {
                if let Some(pid) = t.current {
                    t.get_fd_process(pid).and_then(|process| {
                        crate::fs::fd::get(process, dirfd as usize)
                            .and_then(|fd_obj| fd_obj.path.clone())
                    })
                } else {
                    None
                }
            } else {
                None
            }
        };
        return match resolved {
            Some(p) => crate::syscall::process::sys_execve_with_path(&p, argv, envp),
            None => -9,
        };
    }

    if dirfd == AT_FDCWD || path_str.starts_with('/') {
        if flags & AT_SYMLINK_NOFOLLOW != 0 {
            if let Ok(inode) = crate::fs::lookup_no_follow(&path_str) {
                if inode.file_type() == crate::fs::FileType::Symlink {
                    return -40;
                }
            }
        }
        return crate::syscall::process::sys_execve(pathname, argv, envp);
    }

    let dir_info = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                t.get_fd_process(pid).and_then(|process| {
                    crate::fs::fd::get(process, dirfd as usize).map(|fd_obj| {
                        let ftype = fd_obj.file.file_type();
                        (fd_obj.path.clone(), ftype)
                    })
                })
            } else {
                None
            }
        } else {
            None
        }
    };
    match dir_info {
        Some((Some(dp), ftype)) => {
            if ftype != crate::fs::FileType::Directory {
                return -20;
            }
            let full_path = if dp.ends_with('/') {
                alloc::format!("{}{}", dp, path_str)
            } else {
                alloc::format!("{}/{}", dp, path_str)
            };
            if flags & AT_SYMLINK_NOFOLLOW != 0 {
                if let Ok(inode) = crate::fs::lookup_no_follow(&full_path) {
                    if inode.file_type() == crate::fs::FileType::Symlink {
                        return -40;
                    }
                }
            }
            crate::syscall::process::sys_execve_with_path(&full_path, argv, envp)
        }
        Some((None, _)) => -9,
        None => -9,
    }
}

#[allow(dead_code)]
pub fn exec_from_data(elf_data: &[u8], name: &str) -> isize {
    let argv = alloc::vec![String::from(name)];
    let envp: Vec<String> = Vec::new();
    match image::do_exec(elf_data, name, &argv, &envp) {
        Ok(_entry_info) => 0,
        Err(e) => exec_error_to_errno(&e),
    }
}
