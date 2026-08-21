//! File access/attribute syscalls: faccessat, faccessat_flags, utimensat.
//!
//! Split from `path.rs` to align with Linux `fs/open.c` attribute operations.

use super::user::UserSlice;
use crate::fs::fd as fd_table;
use super::path::{
    access_allowed, current_access_ids, current_access_ids_with_caps,
    access_fs_errno, access_path_read, access_lookup_target,
    access_check_search,
};

#[allow(dead_code)]
pub(in crate::syscall) fn sys_faccessat(dirfd: isize, path_ptr: *const u8, mode: u32) -> isize {
    sys_faccessat_flags(dirfd, path_ptr, mode, 0)
}

pub(in crate::syscall) fn sys_faccessat_flags(
    dirfd: isize,
    path_ptr: *const u8,
    mode: u32,
    flags: u32,
) -> isize {
    const X_OK: u32 = 1;
    const W_OK: u32 = 2;
    const R_OK: u32 = 4;
    const ALL_OK: u32 = X_OK | W_OK | R_OK;
    const AT_EACCESS: u32 = 0x200;
    if mode & !ALL_OK != 0 {
        return -22;
    }
    let path = match access_path_read(path_ptr) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let (full_path, inode) = match access_lookup_target(dirfd, &path, flags) {
        Ok(target) => target,
        Err(errno) => return errno,
    };
    if mode & W_OK != 0 && !full_path.is_empty() && crate::fs::is_readonly_path(&full_path) {
        return -30;
    }
    let stat = match inode.stat() {
        Ok(stat) => stat,
        Err(err) => return access_fs_errno(err),
    };
    let (uid, gid) = current_access_ids(flags & AT_EACCESS != 0);
    if let Err(errno) = access_check_search(&full_path, uid, gid) {
        return errno;
    }
    if access_allowed(&stat, uid, gid, mode) {
        // Any successful access to a mount clears its MNT_EXPIRE mark.
        crate::fs::clear_mount_expired(&full_path);
        0
    } else {
        -13
    }
}

/// utimensat(88) — 更新文件访问/修改时间
pub(in crate::syscall) fn sys_utimensat(dirfd: isize, path_ptr: *const u8, times_ptr: *const u8, _flags: u32) -> isize {

    const UTIME_NOW:  i64 = 0x3FFFFFFF;
    const UTIME_OMIT: i64 = 0x3FFFFFFE;

    // Validate flags early: only AT_SYMLINK_NOFOLLOW (0x100) and AT_EMPTY_PATH (0x1000) are valid.
    const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
    const AT_EMPTY_PATH: u32 = 0x1000;
    if _flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return -22; // -EINVAL: unknown flags
    }


    // 当前 CLOCK_REALTIME 整秒（与 clock_gettime 同源）
    let now_sec = crate::syscall::time::realtime_sec().max(0) as u64;

    // 读取 timespec[2] 从用户空间
    let (atime_sec, atime_nsec, mtime_sec, mtime_nsec) = if times_ptr.is_null() || times_ptr as usize == 0 {
        (now_sec as i64, UTIME_NOW, now_sec as i64, UTIME_NOW)
    } else {
        let buf = match UserSlice::<u8>::new(times_ptr, 32).read_bytes() {
            Ok(v) => v,
            Err(_) => return -14, // -EFAULT
        };
        if buf.len() < 32 { return -14; }
        let a_sec  = i64::from_le_bytes(buf[0..8].try_into().unwrap());
        let a_nsec = i64::from_le_bytes(buf[8..16].try_into().unwrap());
        let m_sec  = i64::from_le_bytes(buf[16..24].try_into().unwrap());
        let m_nsec = i64::from_le_bytes(buf[24..32].try_into().unwrap());
        (a_sec, a_nsec, m_sec, m_nsec)
    };

    // Validate nsec fields: must be UTIME_NOW, UTIME_OMIT, or in [0, 999999999].
    if !times_ptr.is_null() && times_ptr as usize != 0 {
        if atime_nsec != UTIME_NOW && atime_nsec != UTIME_OMIT
           && (atime_nsec < 0 || atime_nsec > 999_999_999) {
            return -22; // -EINVAL
        }
        if mtime_nsec != UTIME_NOW && mtime_nsec != UTIME_OMIT
           && (mtime_nsec < 0 || mtime_nsec > 999_999_999) {
            return -22; // -EINVAL
        }
    }

    // Linux do_utimes：NULL path 且 dirfd != AT_FDCWD → futimens 语义，
    // 直接对 dirfd 操作（flags 必须为 0，否则 EINVAL；坏 fd → EBADF）。
    // NULL path 且 dirfd == AT_FDCWD → getname(NULL) → EFAULT。
    const AT_FDCWD: isize = -100;
    if path_ptr.is_null() || path_ptr as usize == 0 {
        if dirfd == AT_FDCWD {
            return -14; // -EFAULT
        }
        if _flags != 0 {
            return -22; // -EINVAL
        }
        let new_atime = if atime_nsec == UTIME_OMIT { None }
            else if atime_nsec == UTIME_NOW { Some(now_sec) }
            else { Some(atime_sec as u64) };
        let new_mtime = if mtime_nsec == UTIME_OMIT { None }
            else if mtime_nsec == UTIME_NOW { Some(now_sec) }
            else { Some(mtime_sec as u64) };
        return utimens_on_fd(dirfd, new_atime, new_mtime);
    }

    // 解析路径
    let path = if path_ptr.is_null() || path_ptr as usize == 0 {
        alloc::string::String::new()
    } else {
        match super::user::read_path(path_ptr) {
            Ok(s) => s,
            Err(e) => return e, // -EFAULT or -ENAMETOOLONG
        }
    };
    let path = match path.as_str() {
        s if !s.is_empty() => alloc::string::String::from(s),
        _ if _flags & AT_EMPTY_PATH != 0 => {
            // AT_EMPTY_PATH (0x1000) → futimens on dirfd
            let new_atime = if atime_nsec == UTIME_OMIT { None }
                else if atime_nsec == UTIME_NOW { Some(now_sec) }
                else { Some(atime_sec as u64) };
            let new_mtime = if mtime_nsec == UTIME_OMIT { None }
                else if mtime_nsec == UTIME_NOW { Some(now_sec) }
                else { Some(mtime_sec as u64) };
            return utimens_on_fd(dirfd, new_atime, new_mtime);
        }
        _ => {
            // Empty/NULL path without AT_EMPTY_PATH: Linux returns EINVAL
            // when pathname is NULL (regardless of flags).
            if path_ptr.is_null() || path_ptr as usize == 0 {
                return -22; // -EINVAL
            }
            // Empty string path without AT_EMPTY_PATH → ENOENT
            return -2;
        }
    };
    let full_path = match super::resolve_at_path(dirfd, &path) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Read-only filesystem check (EROFS)
    if crate::fs::is_readonly_path(&full_path) {
        return -30; // -EROFS
    }

    // 查找文件并更新时间戳
    match crate::fs::lookup(&full_path) {
        Ok(inode) => {
            let new_atime = if atime_nsec == UTIME_OMIT { None }
                else if atime_nsec == UTIME_NOW { Some(now_sec) }
                else { Some(atime_sec as u64) };
            let new_mtime = if mtime_nsec == UTIME_OMIT { None }
                else if mtime_nsec == UTIME_NOW { Some(now_sec) }
                else { Some(mtime_sec as u64) };
            // Permission check using Linux capability model.
            // CAP_DAC_OVERRIDE (bit 1) bypasses file permission checks.
            // CAP_FOWNER (bit 3) bypasses ownership checks.
            let stat = match inode.stat() {
                Ok(s) => s,
                Err(_) => return -5,
            };
            let (euid, _egid, cap_eff) = current_access_ids_with_caps();
            const CAP_DAC_OVERRIDE: u64 = 1u64 << 1;
            const CAP_FOWNER: u64 = 1u64 << 3;
            let has_dac = (cap_eff & CAP_DAC_OVERRIDE) != 0;
            let has_fowner = (cap_eff & CAP_FOWNER) != 0;

            // Linux inode attribute checks (FS_IMMUTABLE_FL / FS_APPEND_FL).
            const FS_IMMUTABLE_FL: u32 = 0x10;
            const FS_APPEND_FL: u32 = 0x20;
            let iflags = inode.get_inode_flags();
            let both_omit = atime_nsec == UTIME_OMIT && mtime_nsec == UTIME_OMIT;
            // Immutable: reject all time changes (except both OMIT).
            if iflags & FS_IMMUTABLE_FL != 0 && !both_omit {
                return -1; // -EPERM
            }
            // Append-only: only NULL times, both-UTIME_NOW, or both-UTIME_OMIT are allowed.
            // Any explicit value or mixed NOW/OMIT combination → EPERM.
            if iflags & FS_APPEND_FL != 0 && !(times_ptr.is_null() || times_ptr as usize == 0) {
                let both_now = atime_nsec == UTIME_NOW && mtime_nsec == UTIME_NOW;
                if !both_omit && !both_now {
                    return -1; // -EPERM
                }
            }

            if times_ptr.is_null() || times_ptr as usize == 0 {
                // NULL times: need write permission OR be owner OR CAP_DAC_OVERRIDE
                const W_OK: u32 = 2;
                if !has_dac && euid != stat.uid && !access_allowed(&stat, euid, _egid, W_OK) {
                    return -13; // -EACCES
                }
            } else if atime_nsec != UTIME_NOW && atime_nsec != UTIME_OMIT
                   || mtime_nsec != UTIME_NOW && mtime_nsec != UTIME_OMIT {
                // Explicit times: must be owner or CAP_FOWNER
                if !has_fowner && euid != stat.uid {
                    return -1; // -EPERM
                }
            } else {
                // UTIME_NOW: need write permission or be owner
                const W_OK: u32 = 2;
                if !has_fowner && euid != stat.uid && !has_dac && !access_allowed(&stat, euid, _egid, W_OK) {
                    return -13; // -EACCES
                }
            }
            let _ = inode.set_times(new_atime, new_mtime);
            crate::fs::dnotify::notify_file_path(&full_path, crate::fs::dnotify::DN_ATTRIB);
            crate::fs::fanotify::notify_file_path(&full_path, crate::fs::fanotify::FAN_ATTRIB);
            0
        }
        Err(crate::fs::FsError::NotDirectory) => -20, // -ENOTDIR
        Err(crate::fs::FsError::TooManyLinks) => -40, // -ELOOP
        Err(_) => -2, // -ENOENT
    }
}

/// futimens 语义：直接对 fd 设置时间戳。对应 Linux `do_utimes_fd` —
/// fd 无效 → EBADF。
fn utimens_on_fd(dirfd: isize, new_atime: Option<u64>, new_mtime: Option<u64>) -> isize {
    if dirfd < 0 {
        return -9; // -EBADF
    }
    let notify_path = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let proc = match table.as_ref()
            .and_then(|t| t.current.and_then(|pid| t.get_fd_process(pid))) {
            Some(p) => p,
            None => return -9,
        };
        match fd_table::get(proc, dirfd as usize) {
            Some(fd_obj) => {
                let _ = fd_obj.file.set_times(new_atime, new_mtime);
                fd_obj.path.clone()
            }
            None => return -9, // -EBADF
        }
    };
    if let Some(path) = notify_path.as_ref() {
        crate::fs::dnotify::notify_file_path(path, crate::fs::dnotify::DN_ATTRIB);
        crate::fs::fanotify::notify_file_path(path, crate::fs::fanotify::FAN_ATTRIB);
    }
    0
}
