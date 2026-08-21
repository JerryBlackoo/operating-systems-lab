//! Shared path utilities: proc-fd resolution, inotify notifications,
//! permission checking, openat2 RESOLVE_* helpers.
//!
//! Syscall entry points have been split into domain-specific modules:
//! - `link.rs`: mknodat, linkat, readlinkat, symlinkat
//! - `rename.rs`: renameat2
//! - `chdir.rs`: getcwd, chdir, fchdir, chroot
//! - `dir.rs`: mkdir, mkdirat, unlink, unlinkat
//! - `attr.rs`: faccessat, faccessat_flags, utimensat

use super::user::{read_c_string, UserSlice};
use crate::fs::fd as fd_table;

/// Parse a `/proc/self/fd/N` or `/proc/[pid]/fd/N` path into a (pid, fd_num).
fn parse_proc_fd_path(full_path: &str) -> Option<(usize, usize)> {
    let rest = full_path.strip_prefix("/proc/")?;
    let (pid_val, fd_part) = {
        if let Some(r) = rest.strip_prefix("self/") {
            let my_pid = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                let t = table.as_ref()?;
                t.current?
            };
            (my_pid, r)
        } else if let Some(r) = rest.strip_prefix("thread-self/") {
            let my_pid = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                let t = table.as_ref()?;
                t.current?
            };
            (my_pid, r)
        } else {
            let slash = rest.find('/')?;
            let pid: usize = rest[..slash].parse().ok()?;
            (pid, &rest[slash + 1..])
        }
    };
    let fd_str = fd_part.strip_prefix("fd/")?;
    let fd_num: usize = fd_str.parse().ok()?;
    Some((pid_val, fd_num))
}

/// Parse `/proc/<pid|self|thread-self>/mem` into the target global pid.
/// `self`/`thread-self` 解析为当前进程；数字 pid 按调用者 PID namespace 翻译
/// 为全局 pid（与 kill 入口一致）。非 mem 路径返回 None。
pub(in crate::syscall) fn parse_proc_mem_path(full_path: &str) -> Option<usize> {
    let rest = full_path.strip_prefix("/proc/")?;
    let (pid_val, tail) = if let Some(r) = rest.strip_prefix("self/") {
        let my = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = table.as_ref()?;
            t.current?
        };
        (my, r)
    } else if let Some(r) = rest.strip_prefix("thread-self/") {
        let my = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = table.as_ref()?;
            t.current?
        };
        (my, r)
    } else {
        let slash = rest.find('/')?;
        let vpid: isize = rest[..slash].parse().ok()?;
        let g = crate::syscall::process_info::ns_translate_incoming_pid(vpid);
        if g <= 0 {
            return None;
        }
        (g as usize, &rest[slash + 1..])
    };
    if tail != "mem" {
        return None;
    }
    Some(pid_val)
}

/// Resolve `/proc/self/fd/N` or `/proc/[pid]/fd/N` to the path of the open fd.
/// Returns None if the path doesn't match or the fd/pid is invalid.
pub(in crate::syscall) fn resolve_proc_fd_link(full_path: &str) -> Option<alloc::string::String> {
    let (pid_val, fd_num) = parse_proc_fd_path(full_path)?;
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_ref()?;
    let process = t.get_fd_process(pid_val)?;
    let fd_obj = fd_table::get(process, fd_num)?;
    let path = fd_obj.path.clone();
    drop(table);
    match path {
        Some(p) if !p.is_empty() => Some(p),
        _ => Some(alloc::format!("anon_inode:[fd:{}]", fd_num)),
    }
}

/// Resolve a `/proc/<pid|self>/ns/<name>` magic nsfs symlink to its readlink
/// target string `name:[<ns-inode>]` (e.g. `user:[4026531837]`). Linux exposes
/// these as nsfs symlinks; we keep the procfs node a regular file so `open(2)`
/// still yields an fd usable by setns(2), and synthesize the symlink target
/// here for readlink(2) (userns05's getusernsidbypid).
pub(in crate::syscall) fn resolve_proc_ns_link(full_path: &str) -> Option<alloc::string::String> {
    let rest = full_path.strip_prefix("/proc/")?;
    let (pid_val, name_part) = if let Some(r) = rest.strip_prefix("self/") {
        let my_pid = { let table = crate::sched::process::PROCESS_TABLE.lock(); let t = table.as_ref()?; t.current? };
        (my_pid, r)
    } else if let Some(r) = rest.strip_prefix("thread-self/") {
        let my_pid = { let table = crate::sched::process::PROCESS_TABLE.lock(); let t = table.as_ref()?; t.current? };
        (my_pid, r)
    } else {
        let slash = rest.find('/')?;
        let pid: usize = rest[..slash].parse().ok()?;
        (pid, &rest[slash + 1..])
    };
    let name = name_part.strip_prefix("ns/")?;
    if name.is_empty() || name.contains('/') { return None; }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_ref()?;
    let p = t.get(pid_val)?;
    let ns = p.nsproxy.lock();
    let (prefix, id) = match name {
        "cgroup" => ("cgroup", ns.cgroup_ns.id.0),
        "ipc" => ("ipc", ns.ipc_ns.id.0),
        "mnt" => ("mnt", ns.mnt_ns.id.0),
        "net" => ("net", ns.net_ns.id.0),
        "pid" | "pid_for_children" => ("pid", ns.pid_ns.id.0),
        "time" | "time_for_children" => ("time", ns.time_ns.id.0),
        "user" => ("user", ns.user_ns.id.0),
        "uts" => ("uts", ns.uts_ns.id.0),
        _ => return None,
    };
    Some(alloc::format!("{}:[{}]", prefix, id))
}

/// Resolve `/proc/self/fd/N` to the underlying inode Arc (for linkat O_TMPFILE / open).
pub(in crate::syscall) fn resolve_proc_fd_inode_pub(full_path: &str) -> Option<alloc::sync::Arc<dyn crate::fs::Inode>> {
    let (pid_val, fd_num) = parse_proc_fd_path(full_path)?;
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_ref()?;
    let process = t.get_fd_process(pid_val)?;
    let fd_obj = fd_table::get(process, fd_num)?;
    Some(fd_obj.file.clone())
}

fn inotify_dir_bit(file_type: crate::fs::FileType) -> u32 {
    if matches!(file_type, crate::fs::FileType::Directory) {
        crate::fs::inotify::IN_ISDIR
    } else {
        0
    }
}

/// 三家 fsnotify（inotify/dnotify/fanotify）合并活跃门：全部无订阅时
/// 热路径（create/delete/modify 每次文件操作）只付 3 次原子读。
#[inline]
pub(crate) fn any_fsnotify_active() -> bool {
    crate::fs::inotify::watches_active()
        || crate::fs::dnotify::watches_active()
        || crate::fs::fanotify::has_active_marks()
}

pub(in crate::syscall) fn notify_inotify_create(parent_path: &str, name: &str, file_type: crate::fs::FileType) {
    if !any_fsnotify_active() {
        return;
    }
    crate::fs::inotify::notify_path(
        parent_path,
        crate::fs::inotify::IN_CREATE | inotify_dir_bit(file_type),
        name,
    );
    crate::fs::dnotify::notify_path(parent_path, crate::fs::dnotify::DN_CREATE);
    if crate::fs::fanotify::has_active_marks() {
        let full_path = if parent_path == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", parent_path, name)
        };
        crate::fs::fanotify::notify_file_path(&full_path, crate::fs::fanotify::FAN_CREATE);
    }
}

pub(in crate::syscall) fn notify_inotify_delete(parent_path: &str, full_path: &str, name: &str, file_type: crate::fs::FileType, victim_ino: u64) {
    if !any_fsnotify_active() {
        return;
    }
    let dir_bit = inotify_dir_bit(file_type);
    // Linux: IN_ISDIR is set on the parent's IN_DELETE event (child is dir),
    // but NOT on IN_DELETE_SELF (self-events never carry IN_ISDIR).
    crate::fs::inotify::notify_path(parent_path, crate::fs::inotify::IN_DELETE | dir_bit, name);
    // Linux: vfs_unlink (non-directory) calls fsnotify_link_count → IN_ATTRIB
    // on the inode BEFORE IN_DELETE_SELF. vfs_rmdir does NOT do this.
    if file_type != crate::fs::FileType::Directory {
        crate::fs::inotify::notify_path(full_path, crate::fs::inotify::IN_ATTRIB, "");
    }
    crate::fs::inotify::notify_path(full_path, crate::fs::inotify::IN_DELETE_SELF, "");
    crate::fs::dnotify::notify_path(parent_path, crate::fs::dnotify::DN_DELETE);
    crate::fs::fanotify::notify_file_path_deleted(
        full_path,
        crate::fs::fanotify::FAN_DELETE | crate::fs::fanotify::FAN_DELETE_SELF,
        victim_ino,
        file_type == crate::fs::FileType::Directory,
    );
}

pub(in crate::syscall) fn next_inotify_rename_cookie() -> u32 {
    static RENAME_COOKIE: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(1);
    RENAME_COOKIE.fetch_add(1, core::sync::atomic::Ordering::Relaxed).max(1)
}

pub(in crate::syscall) fn notify_inotify_move(
    old_parent_path: &str,
    old_full_path: &str,
    old_name: &str,
    file_type: crate::fs::FileType,
    new_parent_path: &str,
    new_name: &str,
    cookie: u32,
) {
    let dir_bit = inotify_dir_bit(file_type);
    crate::fs::inotify::notify_path_with_cookie(
        old_parent_path,
        crate::fs::inotify::IN_MOVED_FROM | dir_bit,
        cookie,
        old_name,
    );
    crate::fs::inotify::notify_path_with_cookie(
        new_parent_path,
        crate::fs::inotify::IN_MOVED_TO | dir_bit,
        cookie,
        new_name,
    );
    // Linux: IN_MOVE_SELF never carries IN_ISDIR (self-events).
    crate::fs::inotify::notify_path(old_full_path, crate::fs::inotify::IN_MOVE_SELF, "");
    let new_full_path = if new_parent_path == "/" {
        alloc::format!("/{}", new_name)
    } else {
        alloc::format!("{}/{}", new_parent_path, new_name)
    };
    // The old path no longer resolves after the rename: seed the moved
    // object's identity (ino via the new path, is_dir from the syscall) so
    // FID events and dirent-class merging stay correct.
    let is_dir = file_type == crate::fs::FileType::Directory;
    let moved_ino = crate::fs::lookup(&new_full_path)
        .map(|i| i.ino() as u64)
        .unwrap_or(0);
    // Linux fsnotify_move emits FS_RENAME before the MOVED_FROM/MOVED_TO
    // dirent events, so a FAN_RENAME event is queued ahead of MOVED_TO.
    crate::fs::fanotify::notify_rename(old_full_path, &new_full_path, moved_ino, is_dir);
    crate::fs::fanotify::notify_file_path_deleted(
        old_full_path,
        crate::fs::fanotify::FAN_MOVED_FROM,
        moved_ino,
        is_dir,
    );
    crate::fs::fanotify::notify_file_path_typed(&new_full_path, crate::fs::fanotify::FAN_MOVED_TO, is_dir);
    // Linux fsnotify_move emits FS_MOVE_SELF last, after both dirent events.
    crate::fs::fanotify::notify_file_path_deleted(
        old_full_path,
        crate::fs::fanotify::FAN_MOVE_SELF,
        moved_ino,
        is_dir,
    );
    crate::fs::dnotify::notify_rename(old_parent_path, new_parent_path);
    crate::fs::relocate_open_fd_paths(old_full_path, &new_full_path);
}

pub(in crate::syscall) fn dir_setgid_inherit(
    parent: &alloc::sync::Arc<dyn crate::fs::Inode>,
    base_mode: u32,
    new_inode: &alloc::sync::Arc<dyn crate::fs::Inode>,
) -> u32 {
    const S_ISGID: u32 = 0o2000;
    if let Ok(pstat) = parent.stat() {
        if pstat.mode.0 & S_ISGID != 0 {
            let _ = new_inode.set_owner(None, Some(pstat.gid));
            return base_mode | S_ISGID;
        }
    }
    base_mode
}

pub(in crate::syscall) fn access_path_read(path_ptr: *const u8) -> Result<alloc::string::String, isize> {
    if path_ptr.is_null() {
        return Err(-14);
    }
    match read_c_string(path_ptr) {
        Ok(path) => {
            if path.len() >= 4096 {
                Err(-36)
            } else {
                Ok(path)
            }
        }
        Err("C string too long or not null-terminated") => Err(-36),
        Err(_) => Err(-14),
    }
}

pub(in crate::syscall) fn access_fs_errno(err: crate::fs::FsError) -> isize {
    match err {
        crate::fs::FsError::NotFound => -2,
        crate::fs::FsError::NotDirectory => -20,
        crate::fs::FsError::TooManyLinks => -40,
        crate::fs::FsError::NameTooLong => -36,
        crate::fs::FsError::InvalidArgument => -22,
        crate::fs::FsError::PermissionDenied => -13,
        _ => -2,
    }
}

pub(in crate::syscall) fn current_access_ids_with_caps() -> (u32, u32, u64) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| {
            t.current.and_then(|pid| {
                t.get(pid).map(|p| (p.euid, p.egid, p.cap_effective))
            })
        })
        .unwrap_or((0, 0, u64::MAX))
}

pub(in crate::syscall) fn current_access_ids(use_effective: bool) -> (u32, u32) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| {
            t.current.and_then(|pid| {
                t.get(pid).map(|p| {
                    if use_effective {
                        (p.euid, p.egid)
                    } else {
                        (p.uid, p.gid)
                    }
                })
            })
        })
        .unwrap_or((0, 0))
}

/// Linux `may_delete()` permission gate for unlink(2)/rmdir(2): the parent
/// directory must grant write+search to the caller (else EACCES), and if the
/// parent has the sticky bit (S_ISVTX) set, only the owner of the victim, the
/// owner of the directory, or root (CAP_FOWNER) may delete it (else EPERM).
/// Returns Some(errno) on denial, None if the delete is allowed.
/// (unlink08 unwritable/unsearchable-dir, rmdir03 sticky/no-search subcases.)
pub(in crate::syscall) fn may_delete_errno(
    parent: &alloc::sync::Arc<dyn crate::fs::Inode>,
    victim_stat: &crate::fs::FileStat,
) -> Option<isize> {
    may_delete_errno_with_flags(parent, victim_stat, 0)
}

pub(in crate::syscall) fn may_delete_errno_with_flags(
    parent: &alloc::sync::Arc<dyn crate::fs::Inode>,
    victim_stat: &crate::fs::FileStat,
    victim_iflags: u32,
) -> Option<isize> {
    const X_OK: u32 = 1;
    const W_OK: u32 = 2;
    const S_ISVTX: u32 = 0o1000;
    const FS_IMMUTABLE_FL: u32 = 0x10;
    const FS_APPEND_FL: u32 = 0x20;
    let (euid, egid) = current_access_ids(true);
    let pstat = parent.stat().ok()?;
    if !access_allowed(&pstat, euid, egid, W_OK | X_OK) {
        return Some(-13); // -EACCES
    }
    // Parent directory immutable or append-only → cannot delete within it.
    let parent_iflags = parent.get_inode_flags();
    if parent_iflags & FS_IMMUTABLE_FL != 0 || parent_iflags & FS_APPEND_FL != 0 {
        return Some(-1); // -EPERM
    }
    // Victim file immutable → cannot unlink.
    if victim_iflags & FS_IMMUTABLE_FL != 0 {
        return Some(-1); // -EPERM
    }
    // Victim file append-only → cannot unlink.
    if victim_iflags & FS_APPEND_FL != 0 {
        return Some(-1); // -EPERM
    }
    if euid != 0 && (pstat.mode.0 & S_ISVTX) != 0 {
        if euid != victim_stat.uid && euid != pstat.uid {
            return Some(-1); // -EPERM (sticky directory)
        }
    }
    None
}

/// Linux `may_open()` access-mode permission for an existing file: O_RDONLY
/// needs R, O_WRONLY needs W, O_RDWR needs R|W. Uses the caller's effective
/// credentials; root (or CAP_DAC_OVERRIDE via access_allowed) bypasses. Used by
/// the open(2) path to surface EACCES on a file the caller cannot access in the
/// requested mode (open08).
pub(in crate::syscall) fn open_mode_allowed(stat: &crate::fs::FileStat, flags: u32) -> bool {
    const O_ACCMODE: u32 = 0o3;
    const O_WRONLY: u32 = 0o1;
    const O_RDWR: u32 = 0o2;
    const X_OK: u32 = 1;
    const W_OK: u32 = 2;
    const R_OK: u32 = 4;
    let need = match flags & O_ACCMODE {
        O_WRONLY => W_OK,
        O_RDWR => R_OK | W_OK,
        _ => R_OK,
    };
    let _ = X_OK;
    let (euid, egid) = current_access_ids(true);
    access_allowed(stat, euid, egid, need)
}

/// Linux `execve(2)` file checks performed after path resolution (do_open_execat
/// + inode_permission(MAY_EXEC) + exec_binprm format probe). The final target
/// (symlinks followed) must be a regular file with execute permission, and an
/// empty file with execute permission yields ENOEXEC. Path-resolution failures
/// surface through `access_fs_errno` (ENOENT/ENOTDIR/ENAMETOOLONG/ELOOP/EACCES).
/// Returns `Some(errno)` to fail the exec, `None` to allow it.
/// (execve02 EACCES; execve03 ENAMETOOLONG/ENOTDIR/EACCES/ENOEXEC subcases.)
pub(crate) fn may_exec_errno(path: &str) -> Option<isize> {
    const X_OK: u32 = 1;
    let inode = match crate::fs::lookup(path) {
        Ok(i) => i,
        Err(e) => return Some(access_fs_errno(e)),
    };
    let stat = inode.stat().ok()?;
    // execve target must be a regular file; a directory or special file -> EACCES.
    if stat.file_type != crate::fs::FileType::Regular {
        return Some(-13); // -EACCES
    }
    // MS_NOEXEC: deny execution on a noexec mount (mount03).
    if crate::fs::is_noexec_path(path) {
        return Some(-13); // -EACCES
    }
    // MAY_EXEC: need an execute bit reachable by the caller (root still needs
    // at least one x bit set on a non-directory, handled by access_allowed).
    let (euid, egid) = current_access_ids(true);
    if !access_allowed(&stat, euid, egid, X_OK) {
        return Some(-13); // -EACCES
    }
    // A zero-length file with execute permission is not a recognised binary.
    if stat.size == 0 {
        return Some(-8); // -ENOEXEC
    }
    None
}

pub(in crate::syscall) fn access_allowed(stat: &crate::fs::FileStat, uid: u32, gid: u32, mode: u32) -> bool {
    const X_OK: u32 = 1;
    const W_OK: u32 = 2;
    const R_OK: u32 = 4;
    let need = ((mode & R_OK) >> 2) << 2
        | (mode & W_OK)
        | (mode & X_OK);
    if need == 0 {
        return true;
    }
    let bits = stat.mode.0 & 0o777;
    if uid == 0 {
        if need & X_OK != 0
            && stat.file_type != crate::fs::FileType::Directory
            && bits & 0o111 == 0
        {
            return false;
        }
        return true;
    }
    let granted = if uid == stat.uid {
        (bits >> 6) & 0o7
    } else if gid == stat.gid {
        (bits >> 3) & 0o7
    } else {
        bits & 0o7
    };
    granted & need == need
}

pub(in crate::syscall) fn access_lookup_target(
    dirfd: isize,
    path: &str,
    flags: u32,
) -> Result<(alloc::string::String, alloc::sync::Arc<dyn crate::fs::Inode>), isize> {
    const AT_FDCWD: isize = -100;
    const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
    const AT_EMPTY_PATH: u32 = 0x1000;

    if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return Err(-2);
        }
        if dirfd == AT_FDCWD {
            let cwd = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                table
                    .as_ref()
                    .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| p.cwd()))
                    .unwrap_or_else(|| "/".into())
            };
            let inode = crate::fs::lookup(&cwd).map_err(access_fs_errno)?;
            return Ok((cwd, inode));
        }
        if dirfd < 0 {
            return Err(-9);
        }
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = table.as_ref().ok_or(-9isize)?;
        let current = t.current.ok_or(-9isize)?;
        let process = t.get_fd_process(current).ok_or(-9isize)?;
        let fd = fd_table::get(process, dirfd as usize).ok_or(-9isize)?;
        let path = fd.path.clone().unwrap_or_default();
        return Ok((path, fd.file.clone()));
    }

    let full_path = resolve_at_path(dirfd, path)?;
    if full_path.len() >= 4096 {
        return Err(-36);
    }
    let lookup = if flags & AT_SYMLINK_NOFOLLOW != 0 {
        crate::fs::lookup_no_follow(&full_path)
    } else {
        crate::fs::lookup(&full_path)
    };
    match lookup {
        Ok(inode) => Ok((full_path, inode)),
        Err(err) => Err(access_fs_errno(err)),
    }
}

/// Enforce search (execute) permission on every intermediate directory of a
/// fully-resolved path, matching Linux path-walk semantics. `access()` /
/// `faccessat()` resolve the whole path, so a missing search bit on any ancestor
/// directory must surface as `EACCES` even when the final target is otherwise
/// reachable. The credential pair must be the same (real vs effective) one used
/// for the final-target access check.
/// Enforce Linux namei `MAY_EXEC` on every directory in the path prefix using
/// the caller's *effective* credentials. Shared by path-based syscalls
/// (stat/lstat/link/…) so a non-root caller lacking search permission on an
/// intermediate directory gets EACCES, matching `link_path_walk`.
pub(in crate::syscall) fn check_prefix_search_effective(full_path: &str) -> Result<(), isize> {
    let (euid, egid) = current_access_ids(true);
    // root（euid==0）凭 CAP_DAC_OVERRIDE/CAP_DAC_READ_SEARCH 对任何目录恒有 search
    // 权限，故此前缀检查对 root 唯一可能的非 EACCES 结果（祖先非目录 ENOTDIR /
    // 不存在 ENOENT）会被各调用方紧随其后的真实路径解析等价地产生（如 open 的
    // 主 lookup 返回 NotDirectory→ENOTDIR、O_CREAT 父目录解析失败→ENOENT）。
    // 因此对 root 跳过这趟 O(depth²) 的额外前缀重走是纯减冗余、语义等价
    //（EACCES 子测例以非 root 运行、euid!=0 仍会走完整检查）。§1.13①
    if euid == 0 {
        return Ok(());
    }
    access_check_search(full_path, euid, egid)
}

pub(in crate::syscall) fn access_check_search(full_path: &str, uid: u32, gid: u32) -> Result<(), isize> {
    const X_OK: u32 = 1;
    let components: alloc::vec::Vec<&str> = full_path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect();
    if components.len() <= 1 {
        return Ok(());
    }
    let mut prefix = alloc::string::String::from("/");
    for component in &components[..components.len() - 1] {
        if prefix != "/" {
            prefix.push('/');
        }
        prefix.push_str(component);
        let inode = crate::fs::lookup(&prefix).map_err(access_fs_errno)?;
        if inode.file_type() != crate::fs::FileType::Directory {
            return Err(-20);
        }
        let stat = inode.stat().map_err(access_fs_errno)?;
        if !access_allowed(&stat, uid, gid, X_OK) {
            return Err(-13);
        }
    }
    Ok(())
}

// ---- Path resolution (shared by all *at syscalls) ----

/// `resolve_path` 的 owned 变体：主流情形零/单分配——绝对规范路径原样
/// 返回；相对规范路径若入参 String 头部有足够备用容量（read_path_and_cred
/// 预留 32B）则原地前插 cwd 前缀，免新分配+旧串释放。语义与
/// `resolve_path` 严格一致。
pub(crate) fn resolve_path_owned(path: alloc::string::String) -> alloc::string::String {
    use crate::sched::process::PROCESS_TABLE;
    {
        let table = PROCESS_TABLE.lock();
        let proc = table
            .as_ref()
            .and_then(|t| t.current.and_then(|pid| t.get(pid)));
        if let Some(p) = proc {
            let fs = p.fs.lock();
            if path.starts_with('/') {
                if fs.root == "/" && abs_path_is_normalized(&path) {
                    return path;
                }
            } else if rel_path_is_normalized(&path) && path_is_normalized(&fs.cwd) {
                let cwd = &fs.cwd;
                let prefix_len = if cwd.ends_with('/') { cwd.len() } else { cwd.len() + 1 };
                let mut v = path.into_bytes();
                let old = v.len();
                v.reserve(prefix_len);
                v.resize(old + prefix_len, 0);
                v.copy_within(0..old, prefix_len);
                v[..cwd.len()].copy_from_slice(cwd.as_bytes());
                if prefix_len > cwd.len() {
                    v[cwd.len()] = b'/';
                }
                // 入参与 cwd 均为合法 UTF-8，拼接不破坏编码。
                return unsafe { alloc::string::String::from_utf8_unchecked(v) };
            }
        }
    }
    resolve_path(&path)
}

/// 解析路径（处理相对路径和 . ..）
pub(crate) fn resolve_path(path: &str) -> alloc::string::String {
    use crate::sched::process::PROCESS_TABLE;

    // 绝对路径 + 无 chroot 的主流情形在锁内只读判定、零 clone：fs_root/cwd
    // 的 String clone（两次堆分配+memcpy）在每个 path syscall 上都是纯开销，
    // 只有真正需要拼接/归一化的少数分支才付。§1.13① 减分配。
    let (fs_root, cwd) = {
        let table = PROCESS_TABLE.lock();
        let proc = table
            .as_ref()
            .and_then(|t| t.current.and_then(|pid| t.get(pid)));
        if let Some(p) = proc {
            let fs = p.fs.lock();
            if path.starts_with('/') {
                if fs.root == "/" && abs_path_is_normalized(path) {
                    return alloc::string::String::from(path);
                }
            } else {
                // 相对路径快路径：cwd 恒维护为规范形；相对分量若也无 ""/"."/".."，
                // 拼接结果必已规范 —— 锁内单次分配拼出，免 cwd clone + format! +
                // normalize 的三次分配/拷贝。
                if rel_path_is_normalized(path) && path_is_normalized(&fs.cwd) {
                    let cwd = &fs.cwd;
                    let mut full =
                        alloc::string::String::with_capacity(cwd.len() + 1 + path.len());
                    full.push_str(cwd);
                    if !cwd.ends_with('/') {
                        full.push('/');
                    }
                    full.push_str(path);
                    return full;
                }
            }
            (fs.root.clone(), fs.cwd.clone())
        } else {
            if path.starts_with('/') && abs_path_is_normalized(path) {
                return alloc::string::String::from(path);
            }
            (
                alloc::string::String::from("/"),
                alloc::string::String::from("/"),
            )
        }
    };

    if path.starts_with('/') {
        normalize_path_at_root(&fs_root, path)
    } else {
        let full = if cwd.ends_with('/') {
            alloc::format!("{}{}", cwd, path)
        } else {
            alloc::format!("{}/{}", cwd, path)
        };
        normalize_path(&full)
    }
}

/// 快速判定绝对 `path` 是否已是 `normalize_path` 的输出形（绝对、无尾斜杠、
/// 无空/"."/".." 分量），可据此跳过重归一化分配。规则与 normalize_path 严格一致。
fn abs_path_is_normalized(path: &str) -> bool {
    path_is_normalized(path)
}

/// 规范化路径（处理 . 和 ..）
/// 已规范形（绝对路径、无尾斜杠、无 ""/"."/".."" 分量）判定：逐字节单遍
/// 扫描，避免热路径上 split 的通用 CharSearcher 逐分量非内联调用。
fn path_is_normalized(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes == b"/" {
        return true;
    }
    if bytes.first() != Some(&b'/') || bytes.last() == Some(&b'/') {
        return false;
    }
    let mut comp_start = 1;
    let mut i = 1;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'/' {
            let comp = &bytes[comp_start..i];
            if comp.is_empty() || comp == b"." || comp == b".." {
                return false;
            }
            comp_start = i + 1;
        }
        i += 1;
    }
    true
}

/// 相对路径版规范形判定：非空、不以 '/' 开头/结尾、无 ""/"."/".." 分量。
/// 与 `path_is_normalized` 对绝对路径的规则一致（拼到规范 cwd 后仍规范）。
fn rel_path_is_normalized(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes.first() == Some(&b'/') || bytes.last() == Some(&b'/') {
        return false;
    }
    let mut comp_start = 0;
    let mut i = 0;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'/' {
            let comp = &bytes[comp_start..i];
            if comp.is_empty() || comp == b"." || comp == b".." {
                return false;
            }
            comp_start = i + 1;
        }
        i += 1;
    }
    true
}

pub(crate) fn normalize_path(path: &str) -> alloc::string::String {
    // 快路径：入参已是规范形（syscall 入口拼出的绝对路径绝大多数如此），
    // 单次拷贝返回，免 Vec 物化 + 逐组件重建（全局锁分配器很贵）。
    if path_is_normalized(path) {
        return alloc::string::String::from(path);
    }
    let mut components: alloc::vec::Vec<&str> = alloc::vec::Vec::new();

    for component in path.split('/') {
        match component {
            "" | "." => continue,
            ".." => {
                components.pop();
            }
            _ => {
                components.push(component);
            }
        }
    }

    if components.is_empty() {
        alloc::string::String::from("/")
    } else {
        let mut result = alloc::string::String::new();
        for c in components {
            result.push('/');
            result.push_str(c);
        }
        result
    }
}

fn normalize_path_at_root(root: &str, path_from_root: &str) -> alloc::string::String {
    let mut components: alloc::vec::Vec<&str> = alloc::vec::Vec::new();
    let root_norm = normalize_path(root);
    for component in root_norm.split('/').filter(|component| !component.is_empty()) {
        components.push(component);
    }
    let root_depth = components.len();

    for component in path_from_root.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if components.len() > root_depth {
                    components.pop();
                }
            }
            other => components.push(other),
        }
    }

    if components.is_empty() {
        alloc::string::String::from("/")
    } else {
        let mut result = alloc::string::String::new();
        for c in components {
            result.push('/');
            result.push_str(c);
        }
        result
    }
}

/// 统一 dirfd-relative 路径解析（所有 *at 系列 syscall 公用）
///
/// - 绝对路径 → normalize
/// - dirfd == AT_FDCWD → cwd + relative
/// - 其他 → fd 对应目录路径 + relative
///
/// 返回 Ok(full_path) 或 Err(负 errno)
/// 单遍逐字节扫描：NAME_MAX(255) 分量长度校验 + 规范形判定（绝对、无尾斜杠、
/// 无 ""/"."/".." 分量）二合一。返回 (any_component_too_long, is_normalized_abs)。
/// 热路径（unlink/openat 等每 syscall 一次）避免 split 的 CharSearcher 两遍扫。
fn scan_namemax_normalized(path: &str) -> (bool, bool) {
    let bytes = path.as_bytes();
    let n = bytes.len();
    if bytes == b"/" {
        return (false, true);
    }
    // 短路径快路径：总长 ≤255（含首 '/'）时任何分量都不可能 >255，
    // 免逐分量长度统计；规范形只需判「无空分量、无 "."/".." 分量、
    // 无尾斜杠」——等价于每个 '/' 的后邻字节不是 '/'、不是路径尾、
    // 且不是构成 "."/".." 分量的 '.'（"/.hidden" 合法）。
    if n <= 255 && bytes.first() == Some(&b'/') {
        let mut normalized = bytes[n - 1] != b'/';
        let mut i = 1usize;
        while normalized {
            // 检查当前分量首字节（i 指向 '/' 的下一位）
            if bytes[i] == b'.'
                && (i + 1 == n
                    || bytes[i + 1] == b'/'
                    || (bytes[i + 1] == b'.' && (i + 2 == n || bytes[i + 2] == b'/')))
            {
                normalized = false;
                break;
            }
            match memchr_slash(&bytes[i..]) {
                Some(p) => {
                    let j = i + p + 1;
                    if j >= n || bytes[j] == b'/' {
                        normalized = false;
                        break;
                    }
                    i = j;
                }
                None => break,
            }
        }
        return (false, normalized);
    }
    let mut too_long = false;
    let mut normalized = bytes.first() == Some(&b'/') && bytes.last() != Some(&b'/');
    let mut comp_start = if bytes.first() == Some(&b'/') { 1 } else { 0 };
    // 分量间用 memchr 跳跃（word-at-a-time），替代逐字节多分支循环。
    loop {
        let rest = &bytes[comp_start..];
        let end = match memchr_slash(rest) {
            Some(p) => comp_start + p,
            None => bytes.len(),
        };
        let comp = &bytes[comp_start..end];
        if comp.len() > 255 {
            too_long = true;
        }
        if comp.is_empty() || comp == b"." || comp == b".." {
            normalized = false;
        }
        if end == bytes.len() {
            break;
        }
        comp_start = end + 1;
        if comp_start > bytes.len() {
            break;
        }
    }
    (too_long, normalized)
}

/// word-at-a-time 找 b'/'（SWAR，8 字节/迭代），路径分量扫描热路径用。
#[inline]
fn memchr_slash(haystack: &[u8]) -> Option<usize> {
    const LO: u64 = 0x0101_0101_0101_0101;
    const HI: u64 = 0x8080_8080_8080_8080;
    const NEEDLE: u64 = 0x2f2f_2f2f_2f2f_2f2f; // b'/' 广播
    let len = haystack.len();
    let mut i = 0usize;
    while i + 8 <= len {
        // 未对齐 8 字节读安全：来自同一 slice 内部
        let word = u64::from_le_bytes(haystack[i..i + 8].try_into().unwrap());
        let x = word ^ NEEDLE;
        let found = x.wrapping_sub(LO) & !x & HI;
        if found != 0 {
            return Some(i + (found.trailing_zeros() / 8) as usize);
        }
        i += 8;
    }
    while i < len {
        if haystack[i] == b'/' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// 借用版路径解析快判：绝对+已规范+无 chroot 时入参 &str 可直接作为
/// full_path 使用（零堆分配，unlink 等热路径配合栈路径缓冲）。
/// Ok(true)=可直接借用；Ok(false)=需走 owned 慢路径（慢路径会重做
/// NAME_MAX/规范化检查）；Err(-36)=分量超 NAME_MAX。
pub(crate) fn resolve_path_borrowed_ok(path: &str, root_is_plain: bool) -> Result<bool, isize> {
    const PATH_MAX: usize = 4096;
    if path.len() >= PATH_MAX {
        return Err(-36); // -ENAMETOOLONG
    }
    if !root_is_plain || path.as_bytes().first() != Some(&b'/') {
        return Ok(false);
    }
    let (too_long, normalized) = scan_namemax_normalized(path);
    if too_long {
        return Err(-36);
    }
    Ok(normalized)
}

/// 所有权版路径解析快路径：绝对+已规范+无 chroot 时直接 move 入参 String，
/// 零 clone 零重归一化（unlink/lat_fs 删海量文件热路径每次省 2 次堆分配）。
pub(crate) fn resolve_at_path_owned(
    dirfd: isize,
    path: alloc::string::String,
) -> Result<alloc::string::String, isize> {
    resolve_at_path_owned_hint(dirfd, path, None)
}

/// 同 `resolve_at_path_owned_hint`，但调用方额外传入已 clone 的进程 fs
/// 结构（read_path_and_cred 顺带取出）：主流 AT_FDCWD/绝对路径分支完全
/// 不再碰 PROCESS_TABLE 锁，直接锁 fs 取 root/cwd。
pub(crate) fn resolve_at_path_owned_fs(
    dirfd: isize,
    path: alloc::string::String,
    root_is_plain: bool,
    fs: &alloc::sync::Arc<crate::sync::SpinLock<crate::sched::process::FsStruct>>,
) -> Result<alloc::string::String, isize> {
    const PATH_MAX: usize = 4096;
    if path.len() >= PATH_MAX {
        return Err(-36); // -ENAMETOOLONG
    }
    let is_abs = path.as_bytes().first() == Some(&b'/');
    // 相对短路径（≤255B）任何分量都不可能超 NAME_MAX：单遍 rel 规范形
    // 扫描同时充当 NAME_MAX 检查，免 scan_namemax + rel_path 双扫描。
    let (too_long, normalized) = if is_abs || path.len() > 255 {
        scan_namemax_normalized(&path)
    } else {
        (false, rel_path_is_normalized(&path))
    };
    if too_long {
        return Err(-36);
    }
    if normalized && is_abs && root_is_plain {
        return Ok(path);
    }
    const AT_FDCWD: isize = -100;
    if dirfd == AT_FDCWD || is_abs {
        {
            let fs = fs.lock();
            if is_abs {
                if fs.root == "/" && abs_path_is_normalized(&path) {
                    return Ok(path);
                }
            } else if (normalized || (path.len() > 255 && rel_path_is_normalized(&path)))
                && path_is_normalized(&fs.cwd)
            {
                let cwd = &fs.cwd;
                let prefix_len = if cwd.ends_with('/') { cwd.len() } else { cwd.len() + 1 };
                let mut v = path.into_bytes();
                let old = v.len();
                v.reserve(prefix_len);
                v.resize(old + prefix_len, 0);
                v.copy_within(0..old, prefix_len);
                v[..cwd.len()].copy_from_slice(cwd.as_bytes());
                if prefix_len > cwd.len() {
                    v[cwd.len()] = b'/';
                }
                // 入参与 cwd 均为合法 UTF-8，拼接不破坏编码。
                return Ok(unsafe { alloc::string::String::from_utf8_unchecked(v) });
            }
        }
        return Ok(resolve_path(&path));
    }
    resolve_at_path(dirfd, &path)
}

/// 同 `resolve_at_path_owned`，但调用方可传入已取到的 `fs_root == "/"`
/// 判定，免在此再锁一次进程表（unlink 等热路径与凭证读取合并为一把锁）。
pub(crate) fn resolve_at_path_owned_hint(
    dirfd: isize,
    path: alloc::string::String,
    root_is_plain_hint: Option<bool>,
) -> Result<alloc::string::String, isize> {
    const PATH_MAX: usize = 4096;
    if path.len() >= PATH_MAX {
        return Err(-36); // -ENAMETOOLONG
    }
    let (too_long, normalized) = scan_namemax_normalized(&path);
    if too_long {
        return Err(-36);
    }
    if normalized && path.as_bytes().first() == Some(&b'/') {
        let root_is_plain = root_is_plain_hint.unwrap_or_else(|| {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table
                .as_ref()
                .and_then(|t| t.current.and_then(|pid| t.get(pid)))
                .map(|p| p.fs_root_is_plain())
                .unwrap_or(true)
        });
        if root_is_plain {
            return Ok(path);
        }
    }
    // AT_FDCWD/绝对路径直接进 resolve_path：resolve_at_path 只会重复
    // PATH_MAX/NAME_MAX 扫描（本函数已做）再转发同一分支。
    const AT_FDCWD: isize = -100;
    if dirfd == AT_FDCWD || path.as_bytes().first() == Some(&b'/') {
        return Ok(resolve_path_owned(path));
    }
    resolve_at_path(dirfd, &path)
}

pub(crate) fn resolve_at_path(dirfd: isize, path: &str) -> Result<alloc::string::String, isize> {
    const AT_FDCWD: isize = -100;
    use crate::sched::process::PROCESS_TABLE;

    const PATH_MAX: usize = 4096;
    if path.len() >= PATH_MAX {
        return Err(-36); // -ENAMETOOLONG
    }
    let (too_long, _) = scan_namemax_normalized(path);
    if too_long {
        return Err(-36); // -ENAMETOOLONG
    }

    if path.starts_with('/') {
        return Ok(resolve_path(path));
    }
    if dirfd == AT_FDCWD {
        return Ok(resolve_path(path));
    }
    if dirfd < 0 {
        return Err(-9); // -EBADF
    }
    let base_path = {
        let table = PROCESS_TABLE.lock();
        let table = table.as_ref().ok_or(-1isize)?;
        let current_pid = table.current.ok_or(-1isize)?;
        let process = table.get_fd_process(current_pid).ok_or(-1isize)?;
        let fd = fd_table::get(process, dirfd as usize).ok_or(-9isize)?; // -EBADF
        if fd.file.file_type() != crate::fs::FileType::Directory {
            return Err(-20); // -ENOTDIR
        }
        fd.path.clone().ok_or(-2isize)? // -ENOENT
    };
    let joined = if base_path == "/" {
        alloc::format!("/{}", path)
    } else if base_path.ends_with('/') {
        alloc::format!("{}{}", base_path, path)
    } else {
        alloc::format!("{}/{}", base_path, path)
    };
    Ok(normalize_path(&joined))
}

// ---- openat2 RESOLVE_* helpers ----

/// Check if a path traverses any /proc magic symlinks:
/// /proc/self, /proc/thread-self, /proc/<pid>/exe, /proc/<pid>/cwd,
/// /proc/<pid>/root, /proc/<pid>/fd/N — all are magic symlinks.
pub(in crate::syscall) fn is_proc_fd_path(full_path: &str) -> bool {
    if let Some(rest) = full_path.strip_prefix("/proc/") {
        // /proc/self/* and /proc/thread-self/* are magic symlinks
        if rest.starts_with("self/") || rest.starts_with("thread-self/") {
            return true;
        }
        // /proc/<pid>/exe, /proc/<pid>/cwd, /proc/<pid>/root, /proc/<pid>/fd/*
        if let Some(slash) = rest.find('/') {
            if rest[..slash].chars().all(|c| c.is_ascii_digit()) {
                let after = &rest[slash + 1..];
                if after.starts_with("exe") || after.starts_with("cwd")
                    || after.starts_with("root") || after.starts_with("fd/")
                {
                    return true;
                }
            }
        }
    }
    false
}

/// Check if any component (including the final one) of the path is a symlink.
/// Covers both real ramfs symlinks and /proc magic symlinks.
pub(in crate::syscall) fn has_symlink_component(full_path: &str) -> bool {
    // /proc/self and /proc/thread-self are magic symlinks
    if full_path.starts_with("/proc/self/") || full_path.starts_with("/proc/thread-self/") {
        return true;
    }
    // /lib is typically a symlink to /{libc}/lib
    if full_path.starts_with("/lib/") || full_path == "/lib" {
        return true;
    }
    // Check all path components (including the final one) for symlink type
    // Use lookup_no_follow to see the actual symlink inode, not its target
    let parts: alloc::vec::Vec<&str> = full_path.split('/').filter(|s| !s.is_empty()).collect();
    let mut prefix = alloc::string::String::new();
    for part in parts.iter() {
        prefix.push('/');
        prefix.push_str(part);
        if let Ok(inode) = crate::fs::lookup_no_follow(&prefix) {
            if inode.file_type() == crate::fs::FileType::Symlink {
                return true;
            }
        }
    }
    false
}

/// Check if a path crosses mount-point boundaries (different filesystem).
/// In our system, /proc, /dev, /sys, /tmp are different "mounts".
pub(in crate::syscall) fn crosses_mount_point(full_path: &str, dirfd: isize) -> bool {
    const AT_FDCWD: isize = -100;

    // Determine the "mount" of the starting point
    let start_mount = if dirfd == AT_FDCWD {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_ref() {
            if let Some(pid) = t.current {
                if let Some(p) = t.get(pid) {
                    get_mount_id(&p.fs.lock().cwd)
                } else { 0 }
            } else { 0 }
        } else { 0 }
    } else {
        // Get path from dirfd
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_ref() {
            if let Some(pid) = t.current {
                if let Some(p) = t.get(pid) {
                    if let Some(fd_obj) = p.fd_table.get(dirfd as usize).and_then(|x| x.as_ref()) {
                        if let Some(ref path) = fd_obj.path {
                            get_mount_id(path)
                        } else { 0 }
                    } else { 0 }
                } else { 0 }
            } else { 0 }
        } else { 0 }
    };

    let target_mount = get_mount_id(full_path);
    start_mount != target_mount
}

/// Simple mount-point identification: top-level directory determines "mount".
fn get_mount_id(path: &str) -> u32 {
    if path.starts_with("/proc") { 1 }
    else if path.starts_with("/dev") { 2 }
    else if path.starts_with("/sys") { 3 }
    else if path.starts_with("/tmp") { 4 }
    else { 0 } // root ramfs / ext4
}

/// Check if a relative path escapes upward via ".." beyond its starting directory.
pub(in crate::syscall) fn escapes_beneath(path: &str) -> bool {
    let mut depth: i32 = 0;
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => {
                depth += 1;
            }
        }
    }
    false
}
