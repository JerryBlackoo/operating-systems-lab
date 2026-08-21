//! Extended attribute (xattr) subsystem — per Linux fs/xattr.c
//!
//! Each file/path maintains a BTreeMap<String, Vec<u8>> as xattr table.
//! set/get/list/remove go through global XATTR_TABLE indexed by absolute path.
//! l*/f* variants resolve path from symlink-nofollow / fd respectively.
//!
//! Linux semantics implemented:
//! * Namespace validation: user., trusted., security., system. prefixes
//! * XATTR_CREATE / XATTR_REPLACE flags with proper EEXIST/ENODATA errors
//! * XATTR_NAME_MAX (255) and XATTR_SIZE_MAX (65536) size limits
//! * Immutable/append-only file checks (FS_IMMUTABLE_FL | FS_APPEND_FL)
//! * user.* xattrs only on regular files and directories
//! * Path component length validation (NAME_MAX = 255 → ENAMETOOLONG)
//! * fd-backed operations with O_PATH rejection

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::sync::SpinLock;

static XATTR_TABLE: SpinLock<BTreeMap<String, BTreeMap<String, Vec<u8>>>> =
    SpinLock::new(BTreeMap::new());

/// 当前持有 xattr 的 path 条目数。绝大多数负载（lmbench/iozone 等）
/// 从不 setxattr，unlink/create 清理路径可据此零锁短路（同 inotify ACTIVE 手法）。
/// 用活跃计数而非 ever-set 布尔：boot 期一次 setxattr 后删光时门重新关闭，
/// 不让 lat_fs 每次 unlink 永久付一次 BTreeMap remove（全路径 memcmp）。
static XATTR_PATH_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
use core::sync::atomic::AtomicUsize;

/// Expose the xattr table for file capability lookup during execve.
pub fn get_xattr_table() -> &'static SpinLock<BTreeMap<String, BTreeMap<String, Vec<u8>>>> {
    &XATTR_TABLE
}

// ============================================================================
// Path resolution helpers
// ============================================================================

fn xattr_path_of_fd(fd: i32) -> Option<String> {
    const O_PATH: u32 = 0o10000000;
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let pid = table.as_ref().and_then(|t| t.current)?;
    let proc = table.as_ref()?.get_fd_process(pid)?;
    let fdo = crate::fs::fd::get(proc, fd as usize)?;
    if fdo.flags & O_PATH != 0 {
        return None;
    }
    Some(fdo.path.clone().unwrap_or_else(|| {
        use alloc::format;
        format!("__anon_fd_{}", fd)
    }))
}

fn xattr_validate_name(name: &str) -> Result<(), isize> {
    if name.is_empty() { return Err(-34); } // ERANGE
    const PREFIXES: &[&str] = &["user.", "trusted.", "security.", "system."];
    for pfx in PREFIXES {
        if let Some(suffix) = name.strip_prefix(pfx) {
            if suffix.is_empty() { return Err(-22); } // "user." alone invalid
            return Ok(());
        }
    }
    Err(-95) // -EOPNOTSUPP: no recognized prefix
}

fn xattr_path_component_too_long(path: &str) -> bool {
    path.split('/').any(|c| c.len() > 255)
}

fn xattr_validate_path(path: &str) -> Result<(), isize> {
    if xattr_path_component_too_long(path) { return Err(-36); } // ENAMETOOLONG
    Ok(())
}

fn xattr_check_existence(path: &str, nofollow: bool) -> Result<(), isize> {
    if path.is_empty() { return Err(-2); }
    if XATTR_TABLE.lock().contains_key(path) { return Ok(()); }
    let found = if nofollow {
        crate::fs::lookup_no_follow(path).is_ok()
    } else {
        crate::fs::lookup(path).is_ok()
    };
    if !found {
        return Err(-2); // -ENOENT
    }
    Ok(())
}

fn xattr_resolve_path(raw: &str) -> Result<String, isize> {
    if raw.is_empty() { return Ok(String::new()); }
    if raw.starts_with('/') { return Ok(raw.into()); }
    const AT_FDCWD: isize = -100;
    super::open::resolve_at_path(AT_FDCWD, raw)
}

// ============================================================================
// Core xattr operations
// ============================================================================

fn xattr_set(path: &str, name: &str, value: &[u8], flags: i32) -> isize {
    if let Err(e) = xattr_validate_name(name) { return e; }
    const XATTR_CREATE:  i32 = 1;
    const XATTR_REPLACE: i32 = 2;
    const XATTR_NAME_MAX: usize = 255;
    const XATTR_SIZE_MAX: usize = 65536;
    const FS_IMMUTABLE_FL: u32 = 0x10;
    const FS_APPEND_FL: u32 = 0x20;

    if name.len() > XATTR_NAME_MAX { return -34; } // ERANGE
    if value.len() > XATTR_SIZE_MAX { return -7; } // E2BIG
    if flags & !(XATTR_CREATE | XATTR_REPLACE) != 0 { return -22; }
    if flags & XATTR_CREATE != 0 && flags & XATTR_REPLACE != 0 { return -22; }

    if let Ok(inode) = crate::fs::lookup(path) {
        let iflags = inode.get_inode_flags();
        if iflags & (FS_IMMUTABLE_FL | FS_APPEND_FL) != 0 {
            return -1; // -EPERM
        }
        if name.starts_with("user.") {
            let ft = inode.file_type();
            match ft {
                crate::fs::FileType::Regular | crate::fs::FileType::Directory => {},
                _ => return -1, // -EPERM
            }
        }
    }

    let mut tbl = XATTR_TABLE.lock();
    let is_new_path = !tbl.contains_key(path);
    let entry = tbl.entry(path.into()).or_insert_with(BTreeMap::new);
    if is_new_path {
        XATTR_PATH_COUNT.fetch_add(1, core::sync::atomic::Ordering::Release);
    }
    let exists = entry.contains_key(name);
    if flags & XATTR_CREATE != 0 && exists { return -17; } // EEXIST
    if flags & XATTR_REPLACE != 0 && !exists { return -61; } // ENODATA
    entry.insert(name.into(), value.to_vec());
    0
}

fn xattr_get(path: &str, name: &str, value: *mut u8, size: usize) -> isize {
    xattr_get_inner(path, name, value, size, true)
}

fn xattr_get_inner(path: &str, name: &str, value: *mut u8, size: usize, fd_backed: bool) -> isize {
    if let Err(e) = xattr_validate_name(name) { return e; }
    let tbl = XATTR_TABLE.lock();
    if let Some(entry) = tbl.get(path) {
        let v = match entry.get(name) { Some(v) => v, None => return -61 };
        if size == 0 { return v.len() as isize; }
        if v.len() > size { return -34; } // ERANGE
        if super::user::write_user_buffer(value, v).is_err() { return -14; }
        return v.len() as isize;
    }
    drop(tbl);
    if fd_backed { return -61; }
    if path.is_empty() || crate::fs::lookup(path).is_err() {
        -2
    } else {
        -61
    }
}

fn xattr_list(path: &str, list: *mut u8, size: usize) -> isize {
    let tbl = XATTR_TABLE.lock();
    let entry = match tbl.get(path) { Some(e) => e, None => return 0 };
    let mut buf = Vec::new();
    for k in entry.keys() {
        buf.extend_from_slice(k.as_bytes());
        buf.push(0);
    }
    if size == 0 { return buf.len() as isize; }
    if buf.len() > size { return -34; }
    if super::user::write_user_buffer(list, &buf).is_err() { return -14; }
    buf.len() as isize
}

fn xattr_remove(path: &str, name: &str, nofollow: bool) -> isize {
    if let Err(e) = xattr_validate_name(name) { return e; }
    if let Ok(inode) = crate::fs::lookup(path) {
        let iflags = inode.get_inode_flags();
        if iflags & 0x30 != 0 { return -1; }
    }
    let mut tbl = XATTR_TABLE.lock();
    if let Some(entry) = tbl.get_mut(path) {
        let removed = entry.remove(name).is_some();
        if removed && entry.is_empty() {
            tbl.remove(path);
            XATTR_PATH_COUNT.fetch_sub(1, core::sync::atomic::Ordering::Release);
        }
        return if removed { 0 } else { -61 };
    }
    drop(tbl);
    let found = if nofollow {
        crate::fs::lookup_no_follow(path).is_ok()
    } else {
        crate::fs::lookup(path).is_ok()
    };
    if path.is_empty() || !found {
        -2
    } else {
        -61
    }
}

// ============================================================================
// Public API — called from cleanup paths
// ============================================================================

pub(in crate::syscall) fn xattr_clear_path(path: &str) {
    if XATTR_PATH_COUNT.load(core::sync::atomic::Ordering::Acquire) == 0 {
        return;
    }
    if XATTR_TABLE.lock().remove(path).is_some() {
        XATTR_PATH_COUNT.fetch_sub(1, core::sync::atomic::Ordering::Release);
    }
}

// ============================================================================
// Syscall entry points — per Linux setxattr(2)/getxattr(2)/listxattr(2)/removexattr(2)
// ============================================================================

pub(in crate::syscall) fn sys_setxattr(path: *const u8, name: *const u8, value: *const u8, size: usize, flags: i32) -> isize {
    let raw = match super::user::read_path(path) { Ok(s) => s, Err(e) => return e };
    let p = match xattr_resolve_path(&raw) { Ok(s) => s, Err(e) => return e };
    if let Err(e) = xattr_validate_path(&p) { return e; }
    let n = match super::user::read_c_string(name) { Ok(s) => s, Err(_) => return -14 };
    let v = match super::user::read_user_buffer(value, size) { Ok(b) => b, Err(_) => return -14 };
    xattr_set(&p, &n, &v, flags)
}

pub(in crate::syscall) fn sys_lsetxattr(path: *const u8, name: *const u8, value: *const u8, size: usize, flags: i32) -> isize {
    sys_setxattr(path, name, value, size, flags)
}

pub(in crate::syscall) fn sys_fsetxattr(fd: i32, name: *const u8, value: *const u8, size: usize, flags: i32) -> isize {
    let p = match xattr_path_of_fd(fd) { Some(p) => p, None => return -9 };
    let n = match super::user::read_c_string(name) { Ok(s) => s, Err(_) => return -14 };
    let v = match super::user::read_user_buffer(value, size) { Ok(b) => b, Err(_) => return -14 };
    xattr_set(&p, &n, &v, flags)
}

fn getxattr_common(path: *const u8, name: *const u8, value: *mut u8, size: usize, nofollow: bool) -> isize {
    let raw = match super::user::read_path(path) { Ok(s) => s, Err(e) => return e };
    let p = match xattr_resolve_path(&raw) { Ok(s) => s, Err(e) => return e };
    if let Err(e) = xattr_validate_path(&p) { return e; }
    if let Err(e) = xattr_check_existence(&p, nofollow) { return e; }
    let n = match super::user::read_c_string(name) { Ok(s) => s, Err(_) => return -14 };
    xattr_get(&p, &n, value, size)
}

pub(in crate::syscall) fn sys_getxattr(path: *const u8, name: *const u8, value: *mut u8, size: usize) -> isize {
    getxattr_common(path, name, value, size, false)
}

// The l* variants operate on the symlink itself: existence is checked without
// following the final component, so a link whose (relative) target does not
// resolve still carries its own xattrs (lremovexattr01 stores a link target
// relative to CWD which is dangling when resolved relative to the link's dir).
pub(in crate::syscall) fn sys_lgetxattr(path: *const u8, name: *const u8, value: *mut u8, size: usize) -> isize {
    getxattr_common(path, name, value, size, true)
}

pub(in crate::syscall) fn sys_fgetxattr(fd: i32, name: *const u8, value: *mut u8, size: usize) -> isize {
    let p = match xattr_path_of_fd(fd) { Some(p) => p, None => return -9 };
    let n = match super::user::read_c_string(name) { Ok(s) => s, Err(_) => return -14 };
    xattr_get_inner(&p, &n, value, size, true)
}

fn listxattr_common(path: *const u8, list: *mut u8, size: usize, nofollow: bool) -> isize {
    let raw = match super::user::read_path(path) { Ok(s) => s, Err(e) => return e };
    let p = match xattr_resolve_path(&raw) { Ok(s) => s, Err(e) => return e };
    if let Err(e) = xattr_validate_path(&p) { return e; }
    if let Err(e) = xattr_check_existence(&p, nofollow) { return e; }
    xattr_list(&p, list, size)
}

pub(in crate::syscall) fn sys_listxattr(path: *const u8, list: *mut u8, size: usize) -> isize {
    listxattr_common(path, list, size, false)
}

pub(in crate::syscall) fn sys_llistxattr(path: *const u8, list: *mut u8, size: usize) -> isize {
    listxattr_common(path, list, size, true)
}

pub(in crate::syscall) fn sys_flistxattr(fd: i32, list: *mut u8, size: usize) -> isize {
    let p = match xattr_path_of_fd(fd) { Some(p) => p, None => return -9 };
    xattr_list(&p, list, size)
}

fn removexattr_common(path: *const u8, name: *const u8, nofollow: bool) -> isize {
    let raw = match super::user::read_path(path) { Ok(s) => s, Err(e) => return e };
    let p = match xattr_resolve_path(&raw) { Ok(s) => s, Err(e) => return e };
    if let Err(e) = xattr_validate_path(&p) { return e; }
    let n = match super::user::read_c_string(name) { Ok(s) => s, Err(_) => return -14 };
    xattr_remove(&p, &n, nofollow)
}

pub(in crate::syscall) fn sys_removexattr(path: *const u8, name: *const u8) -> isize {
    removexattr_common(path, name, false)
}

pub(in crate::syscall) fn sys_lremovexattr(path: *const u8, name: *const u8) -> isize {
    removexattr_common(path, name, true)
}

pub(in crate::syscall) fn sys_fremovexattr(fd: i32, name: *const u8) -> isize {
    let p = match xattr_path_of_fd(fd) { Some(p) => p, None => return -9 };
    let n = match super::user::read_c_string(name) { Ok(s) => s, Err(_) => return -14 };
    xattr_remove(&p, &n, false)
}
