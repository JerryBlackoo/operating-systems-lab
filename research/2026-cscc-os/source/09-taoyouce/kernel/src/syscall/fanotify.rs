//! fanotify syscall wrappers — fanotify_init / fanotify_mark
//!
//! Extracted from fd.rs for low-coupling subsystem architecture.
//! The core fanotify implementation lives in `crate::fs::fanotify`.

use alloc::sync::Arc;

use crate::fs::fd as fd_table;
use crate::fs::{FileDescriptor, FileType};

fn get_fd_obj(fd: usize) -> Option<Arc<FileDescriptor>> {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_ref()?;
    let pid = t.current?;
    let process = t.get_fd_process(pid)?;
    fd_table::get(process, fd)
}

/// Effective uid of the current task. fanotify treats a non-zero euid (i.e. a
/// task lacking CAP_SYS_ADMIN) as an unprivileged listener.
fn current_euid() -> u32 {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)))
        .map(|p| p.euid)
        .unwrap_or(0)
}

pub(in crate::syscall) fn sys_fanotify_init(flags: usize, _event_f_flags: usize) -> isize {
    use crate::fs::fanotify::{FanotifyFile, FanotifyInstance};
    const FAN_CLOEXEC: usize = 0x00000001;
    const FAN_NONBLOCK: usize = 0x00000002;
    const FAN_CLASS_NOTIF: usize = 0x00000000;
    const FAN_CLASS_CONTENT: usize = 0x00000004;
    const FAN_CLASS_PRE_CONTENT: usize = 0x00000008;
    const FAN_UNLIMITED_QUEUE: usize = 0x00000010;
    const FAN_UNLIMITED_MARKS: usize = 0x00000020;
    const FAN_ENABLE_AUDIT: usize = 0x00000040;
    const FAN_REPORT_PIDFD: usize = 0x00000080;
    const FAN_REPORT_TID: usize = 0x00000100;
    const FAN_REPORT_FID: usize = 0x00000200;
    const FAN_REPORT_DIR_FID: usize = 0x00000400;
    const FAN_REPORT_NAME: usize = 0x00000800;
    const FAN_REPORT_TARGET_FID: usize = 0x00001000;
    let known = FAN_CLOEXEC | FAN_NONBLOCK | FAN_CLASS_NOTIF | FAN_CLASS_CONTENT
        | FAN_CLASS_PRE_CONTENT | FAN_UNLIMITED_QUEUE | FAN_UNLIMITED_MARKS
        | FAN_ENABLE_AUDIT | FAN_REPORT_PIDFD
        | FAN_REPORT_TID | FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME
        | FAN_REPORT_TARGET_FID;
    if flags & !known != 0 {
        return -22;
    }
    if flags & (FAN_CLASS_CONTENT | FAN_CLASS_PRE_CONTENT) == (FAN_CLASS_CONTENT | FAN_CLASS_PRE_CONTENT) {
        return -22;
    }
    // FAN_REPORT_PIDFD is mutually exclusive with FAN_REPORT_TID
    // (fs/notify/fanotify/fanotify_user.c).
    if flags & FAN_REPORT_PIDFD != 0 && flags & FAN_REPORT_TID != 0 {
        return -22;
    }
    // Unprivileged listener restrictions (lacks CAP_SYS_ADMIN, euid != 0). An
    // unprivileged group must request FAN_REPORT_FID and may not use any admin
    // init flag (content classes, unlimited queue/marks, audit, report-tid).
    // This is checked before the EINVAL class consistency checks so the
    // forbidden combinations surface as EPERM, matching Linux. (fanotify18)
    let unprivileged = current_euid() != 0;
    if unprivileged {
        const FAN_ADMIN_INIT_FLAGS: usize = FAN_CLASS_CONTENT
            | FAN_CLASS_PRE_CONTENT
            | FAN_UNLIMITED_QUEUE
            | FAN_UNLIMITED_MARKS
            | FAN_ENABLE_AUDIT
            | FAN_REPORT_TID;
        if flags & FAN_ADMIN_INIT_FLAGS != 0 || flags & FAN_REPORT_FID == 0 {
            return -1; // EPERM
        }
    }
    // Linux fanotify_init() consistency checks (fs/notify/fanotify/fanotify_user.c):
    //   * FID reporting (FAN_REPORT_FID/DIR_FID) is only valid with FAN_CLASS_NOTIF.
    //   * FAN_REPORT_NAME requires FAN_REPORT_DIR_FID.
    //   * FAN_REPORT_TARGET_FID requires FAN_REPORT_FID and FAN_REPORT_NAME.
    let class = flags & (FAN_CLASS_CONTENT | FAN_CLASS_PRE_CONTENT);
    let fid_mode = flags & (FAN_REPORT_FID | FAN_REPORT_DIR_FID);
    if fid_mode != 0 && class != FAN_CLASS_NOTIF {
        return -22;
    }
    if flags & FAN_REPORT_NAME != 0 && fid_mode & FAN_REPORT_DIR_FID == 0 {
        return -22;
    }
    if flags & FAN_REPORT_TARGET_FID != 0
        && (fid_mode & FAN_REPORT_FID == 0 || flags & FAN_REPORT_NAME == 0)
    {
        return -22;
    }
    // Enforce the per-user fanotify group limit (Linux default 128). When the
    // cap is reached fanotify_init reports EMFILE.
    if !FanotifyInstance::try_acquire_group() {
        return -24;
    }
    let instance = FanotifyInstance::new();
    let file = FanotifyFile::new(instance.clone());
    let mut open_flags: u32 = 0o2; // O_RDWR
    if flags & FAN_NONBLOCK != 0 {
        open_flags |= 0o4000; // O_NONBLOCK
    }
    let fd_obj = Arc::new(FileDescriptor::new(file, open_flags));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                let result = match fd_table::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        if flags & FAN_CLOEXEC != 0 {
                            let _ = fd_table::set_cloexec(process, fd, true);
                        }
                        instance.set_listener(pid, flags as u32);
                        instance.set_unprivileged(unprivileged);
                        fd as isize
                    }
                    Err(e) => e,
                };
                return result;
            }
        }
    }
    -22
}

/// fanotify_mark(fd, flags, mask, dirfd, pathname)
#[allow(dead_code)]
pub(in crate::syscall) fn sys_fanotify_mark(fd: usize, flags: usize, mask: u64, dirfd: isize, path: *const u8) -> isize {
    const FAN_MARK_MOUNT: u32 = 0x10;
    const FAN_MARK_IGNORED_SURV_MODIFY: u32 = 0x40;
    const FAN_MARK_FILESYSTEM: u32 = 0x100;
    const FAN_MARK_IGNORE: u32 = 0x400;
    const FAN_REPORT_FID: u32 = 0x200;
    const FAN_REPORT_DIR_FID: u32 = 0x400;
    const FAN_REPORT_NAME: u32 = 0x800;
    const FAN_REPORT_TARGET_FID: u32 = 0x1000;
    // Directory-entry events carried in the event mask.
    const FAN_MOVED_FROM: u64 = 0x40;
    const FAN_MOVED_TO: u64 = 0x80;
    const FAN_CREATE: u64 = 0x100;
    const FAN_DELETE: u64 = 0x200;
    const FAN_ONDIR: u64 = 0x4000_0000;
    const FAN_EVENT_ON_CHILD: u64 = 0x0800_0000;
    const FAN_RENAME: u64 = 0x1000_0000;
    const DIRENT_EVENTS: u64 = FAN_MOVED_FROM | FAN_MOVED_TO | FAN_CREATE | FAN_DELETE | FAN_RENAME;

    // Resolve the group first so mask validation can consult its init flags
    // (Linux do_fanotify_mark validates the mask/flags before path lookup).
    let fd_obj = match get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    let fa = match fd_obj.file.as_fanotify() {
        Some(fa) => fa,
        None => return -9,
    };
    let init_flags = fa.instance.init_flags();
    let fid_mode = init_flags & (FAN_REPORT_FID | FAN_REPORT_DIR_FID);
    let mark_flags = flags as u32;
    // Unprivileged listeners may only mark inodes (not mounts/filesystems) and
    // may not request permission events. (fanotify18)
    if fa.instance.is_unprivileged() {
        const FAN_ALL_PERM_EVENTS: u64 = 0x0001_0000 | 0x0002_0000 | 0x0004_0000;
        if mark_flags & (FAN_MARK_MOUNT | FAN_MARK_FILESYSTEM) != 0
            || mask & FAN_ALL_PERM_EVENTS != 0
        {
            return -1; // EPERM
        }
    }
    // Directory-entry events require an fid-reporting group; FAN_RENAME also
    // needs FAN_REPORT_NAME; and mount marks cannot watch dirent events.
    if mask & DIRENT_EVENTS != 0 && fid_mode == 0 {
        return -22;
    }
    if mask & FAN_RENAME != 0 && init_flags & FAN_REPORT_NAME == 0 {
        return -22;
    }
    if mark_flags & FAN_MARK_MOUNT != 0 && mask & DIRENT_EVENTS != 0 {
        return -22;
    }
    let is_inode_mark = mark_flags & (FAN_MARK_MOUNT | FAN_MARK_FILESYSTEM) == 0;
    // FAN_MARK_IGNORE without FAN_MARK_IGNORED_SURV_MODIFY (69562eb0bd3e): not
    // allowed on mount/filesystem marks at all (EINVAL); the inode-on-directory
    // case becomes EISDIR and is checked once the target is resolved below.
    let ignore_no_surv = mark_flags & FAN_MARK_IGNORE != 0
        && mark_flags & FAN_MARK_IGNORED_SURV_MODIFY == 0;
    if ignore_no_surv && !is_inode_mark {
        return -22;
    }
    // Mount/filesystem marks require a real backing superblock. An anonymous
    // pipe (AT_EMPTY_PATH with a pipe dirfd) has none → EINVAL.
    if !is_inode_mark && path.is_null() && dirfd >= 0 {
        if let Some(target) = get_fd_obj(dirfd as usize) {
            match target.file.file_type() {
                FileType::Pipe | FileType::Socket => return -22,
                _ => {}
            }
        }
    }

    let path_str = if path.is_null() {
        alloc::string::String::new()
    } else {
        match super::user::read_c_string(path) {
            Ok(s) => s,
            Err(_) => return -14,
        }
    };
    let abs = if path_str.is_empty() {
        alloc::string::String::from("/")
    } else {
        let resolved = match super::resolve_at_path(dirfd, &path_str) {
            Ok(p) => p,
            Err(e) => return e,
        };
        // Unless FAN_MARK_DONT_FOLLOW is set, a mark on a symlink follows it and
        // attaches to the target inode; the canonical path is the mark key so
        // that opens through the symlink (delivered on the canonical path) match
        // a followed mark but not a DONT_FOLLOW mark on the symlink (fanotify04).
        if mark_flags & crate::fs::fanotify::FAN_MARK_DONT_FOLLOW == 0 {
            crate::syscall::open::resolve_symlinks_for_path(&resolved)
        } else {
            resolved
        }
    };
    if mark_flags & crate::fs::fanotify::FAN_MARK_ONLYDIR != 0 {
        match crate::fs::lookup(&abs) {
            Ok(inode) if inode.file_type() == FileType::Directory => {}
            Ok(_) => return -20,
            Err(_) => return -2,
        }
    }
    // Non-dir inode mark restrictions (fanotify_events_supported):
    //   * dirent events on a non-directory inode → ENOTDIR (always).
    //   * FAN_ONDIR / FAN_EVENT_ON_CHILD on a non-directory inode → ENOTDIR only
    //     under "strict_dir_events": group has FAN_REPORT_TARGET_FID, or the mask
    //     contains FAN_RENAME, or the mark uses FAN_MARK_IGNORE.
    //   * FAN_MARK_IGNORE without SURV_MODIFY on a directory inode → EISDIR.
    let strict_dir_events = init_flags & FAN_REPORT_TARGET_FID != 0
        || mask & FAN_RENAME != 0
        || mark_flags & FAN_MARK_IGNORE != 0;
    let nondir_reject = DIRENT_EVENTS
        | if strict_dir_events { FAN_ONDIR | FAN_EVENT_ON_CHILD } else { 0 };
    if is_inode_mark && ((mask & nondir_reject != 0) || ignore_no_surv) {
        if let Ok(inode) = crate::fs::lookup(&abs) {
            let is_dir = inode.file_type() == FileType::Directory;
            if !is_dir && mask & nondir_reject != 0 {
                return -20; // ENOTDIR
            }
            if is_dir && ignore_no_surv {
                return -21; // EISDIR
            }
        }
    }
    if crate::cmdline::fandbg_enabled() {
        crate::println!("[FANDBG] mark path_str={} abs={} flags={:#x} mask={:#x}", path_str, abs, mark_flags, mask);
    }
    match fa.instance.mark(&abs, mask, mark_flags) {
        Ok(()) => 0,
        Err(e) => e as isize,
    }
}
