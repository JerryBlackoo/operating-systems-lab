//! Linux-style file handle syscall helpers.

use alloc::sync::Arc;

use crate::fs::fd as fd_table;
use crate::syscall::user::{read_path, read_user_buffer, write_user_buffer};

/// name_to_handle_at(dirfd, pathname, handle, mount_id, flags)
///
/// Linux semantics:
/// - Resolves pathname relative to dirfd (AT_FDCWD = cwd)
/// - Reads struct file_handle { u32 handle_bytes; i32 handle_type; u8 f_handle[]; }
/// - If handle_bytes is too small: write back needed size and return -EOVERFLOW
/// - Otherwise: fill handle with opaque data (inode number), write mount_id, return 0
/// - flags: AT_EMPTY_PATH(0x1000), AT_SYMLINK_FOLLOW(0x400)
pub(crate) fn sys_name_to_handle_at(
    dirfd: isize,
    path: *const u8,
    handle: *mut u8,
    mount_id: *mut i32,
    flags: usize,
) -> isize {
    const AT_EMPTY_PATH: usize = 0x1000;
    const AT_SYMLINK_FOLLOW: usize = 0x400;
    // AT_HANDLE_FID: caller wants a handle for fanotify FID comparison only
    // (not necessarily openable). Our handle payload already matches the
    // fanotify FID encoding (handle_type=1 + 8-byte ino), so it's accepted
    // and produces the same handle.
    const AT_HANDLE_FID: usize = 0x200;
    const VALID_FLAGS: usize = AT_EMPTY_PATH | AT_SYMLINK_FOLLOW | AT_HANDLE_FID;
    const MAX_HANDLE_SZ: u32 = 128;
    const HANDLE_SIZE: u32 = 8;

    if flags & !VALID_FLAGS != 0 {
        return -22;
    }

    if handle.is_null() {
        return -14;
    }
    if mount_id.is_null() || write_user_buffer(mount_id as *mut u8, &0i32.to_le_bytes()).is_err() {
        return -14;
    }

    let hdr = match read_user_buffer(handle, 4) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let user_handle_bytes = u32::from_le_bytes(hdr[0..4].try_into().unwrap());

    if user_handle_bytes > MAX_HANDLE_SZ {
        return -22;
    }

    let resolved = if flags & AT_EMPTY_PATH != 0
        && (path.is_null()
            || match read_path(path) {
                Ok(s) => s.is_empty(),
                Err(_) => true,
            }) {
        const AT_FDCWD: isize = -100;
        if dirfd == AT_FDCWD {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_ref() {
                Some(t) => t,
                None => return -9,
            };
            let pid = match t.current {
                Some(p) => p,
                None => return -9,
            };
            let proc = match t.get(pid) {
                Some(p) => p,
                None => return -9,
            };
            proc.cwd()
        } else if dirfd < 0 {
            return -9;
        } else {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_ref() {
                Some(t) => t,
                None => return -9,
            };
            let pid = match t.current {
                Some(p) => p,
                None => return -9,
            };
            let proc = match t.get_fd_process(pid) {
                Some(p) => p,
                None => return -9,
            };
            match fd_table::get(proc, dirfd as usize) {
                Some(f) => f
                    .path
                    .clone()
                    .unwrap_or_else(|| alloc::format!("/proc/self/fd/{}", dirfd)),
                None => return -9,
            }
        }
    } else {
        let path_str = match read_path(path) {
            Ok(s) if !s.is_empty() => s,
            Ok(_) => return -2,
            Err(e) => return e,
        };
        match crate::syscall::resolve_at_path(dirfd, &path_str) {
            Ok(p) => p,
            Err(e) => return e,
        }
    };

    // Without AT_SYMLINK_FOLLOW the handle must describe the symlink itself,
    // not its target, so that open_by_handle_at(2) on it fails with ELOOP
    // (open_by_handle_at02 symlink case).
    let inode = match if flags & AT_SYMLINK_FOLLOW != 0 {
        crate::fs::lookup(&resolved)
    } else {
        crate::fs::lookup_no_follow(&resolved)
    } {
        Ok(i) => i,
        Err(crate::fs::FsError::NotFound) => return -2,
        Err(crate::fs::FsError::NotDirectory) => return -20,
        Err(_) => return -2,
    };

    if user_handle_bytes < HANDLE_SIZE {
        let needed = HANDLE_SIZE.to_le_bytes();
        let _ = write_user_buffer(handle, &needed);
        return -75;
    }

    let ino = inode.ino() as u64;

    crate::fs::fhandle_register(ino as usize, &resolved);

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&HANDLE_SIZE.to_le_bytes());
    out[4..8].copy_from_slice(&1i32.to_le_bytes());
    out[8..16].copy_from_slice(&ino.to_le_bytes());
    let _ = write_user_buffer(handle, &out);

    let mid = 0i32.to_le_bytes();
    let _ = write_user_buffer(mount_id as *mut u8, &mid);

    0
}

/// open_by_handle_at(mount_fd, handle, flags)
///
/// Linux semantics:
/// - Validates mount_fd is a valid directory fd (or use mount_id to find fs)
/// - Reads struct file_handle from handle pointer
/// - Looks up the inode by the handle data and opens it with given flags
pub(crate) fn sys_open_by_handle_at(mount_fd: usize, handle: *const u8, flags: usize) -> isize {
    if handle.is_null() {
        return -14;
    }

    let mount_fd_i = mount_fd as isize;
    if mount_fd_i != -100 {
        if mount_fd_i < 0 {
            return -9;
        }
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() {
            Some(t) => t,
            None => return -9,
        };
        let pid = match t.current {
            Some(p) => p,
            None => return -9,
        };
        let proc = match t.get_fd_process(pid) {
            Some(p) => p,
            None => return -9,
        };
        let fdesc = match fd_table::get(proc, mount_fd) {
            Some(f) => f,
            None => return -9,
        };
        // The mount fd must reference the filesystem that exported the handle.
        // Linux accepts *any* file on that filesystem (mount_fd only identifies
        // the mount — open_by_handle_at01 passes a regular file fd), but a
        // descriptor on a filesystem without export ops (e.g. the console on
        // stdin, devtmpfs) cannot host the handle's object -> ESTALE
        // (open_by_handle_at02 stale-dfd).
        match fdesc.file.file_type() {
            crate::fs::FileType::Directory | crate::fs::FileType::Regular => {}
            _ => return -116, // -ESTALE
        }
    }

    // open_by_handle_at(2) requires CAP_DAC_READ_SEARCH; without it the call is
    // rejected with EPERM regardless of the handle (open_by_handle_at02
    // no-capability case).
    const CAP_DAC_READ_SEARCH: u64 = 1u64 << 2;
    let has_cap = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table
            .as_ref()
            .and_then(|t| t.current.and_then(|pid| t.get(pid)))
            .map(|p| p.cap_effective & CAP_DAC_READ_SEARCH != 0)
            .unwrap_or(true)
    };
    if !has_cap {
        return -1; // -EPERM
    }

    let hdr = match read_user_buffer(handle, 8) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let handle_bytes = u32::from_le_bytes(hdr[0..4].try_into().unwrap()) as usize;
    if handle_bytes == 0 || handle_bytes > 128 {
        return -22;
    }

    let full = match read_user_buffer(handle, 8 + handle_bytes) {
        Ok(b) => b,
        Err(_) => return -14,
    };

    if handle_bytes < 8 {
        return -116;
    }
    let ino = u64::from_le_bytes(full[8..16].try_into().unwrap()) as usize;

    let inode = match crate::fs::lookup_by_ino(ino) {
        Some(i) => i,
        None => return -116,
    };

    // A handle for a symlink can only be opened with O_PATH (optionally
    // O_NOFOLLOW); a plain open follows into ELOOP (open_by_handle_at02
    // symlink case).
    const O_PATH: u32 = 0o10000000;
    if inode.file_type() == crate::fs::FileType::Symlink && (flags as u32 & O_PATH) == 0 {
        return -40; // -ELOOP
    }

    open_inode(inode, flags as u32)
}

fn open_inode(inode: Arc<dyn crate::fs::Inode>, flags: u32) -> isize {
    const O_CLOEXEC: u32 = 0o2000000;

    let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(inode, flags, None));

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(table) = table.as_mut() {
        if let Some(current_pid) = table.current {
            if let Some(process) = table.get_fd_process_mut(current_pid) {
                match fd_table::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        if flags & O_CLOEXEC != 0 {
                            let _ = fd_table::set_cloexec(process, fd, true);
                        }
                        fd as isize
                    }
                    Err(e) => e,
                }
            } else {
                -22
            }
        } else {
            -22
        }
    } else {
        -22
    }
}
