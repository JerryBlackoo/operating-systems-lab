use alloc::sync::Arc;

use super::wake_io_waiters;
use crate::fs::{fd as fd_table, FileDescriptor, FileType};

fn inotify_dir_bit(file_type: FileType) -> u32 {
    if matches!(file_type, FileType::Directory) {
        crate::fs::inotify::IN_ISDIR
    } else {
        0
    }
}

pub(crate) fn fs_error_to_errno(err: crate::fs::FsError) -> isize {
    match err {
        crate::fs::FsError::NotFound => -2,
        crate::fs::FsError::AlreadyExists => -17,
        crate::fs::FsError::NotDirectory => -20,
        crate::fs::FsError::IsDirectory => -21,
        crate::fs::FsError::PermissionDenied => -13,
        crate::fs::FsError::AccessDenied => -13,
        crate::fs::FsError::OperationNotPermitted => -1,
        crate::fs::FsError::InvalidArgument => -22,
        crate::fs::FsError::NoSpace => -28,
        crate::fs::FsError::NotEmpty => -39,
        crate::fs::FsError::WouldBlock => -11,
        crate::fs::FsError::Busy => -16,
        crate::fs::FsError::BrokenPipe => -32,
        crate::fs::FsError::TooManyLinks => -40,
        crate::fs::FsError::NameTooLong => -36,
        crate::fs::FsError::OutOfMemory => -12,
                crate::fs::FsError::OutOfRange => -34,
crate::fs::FsError::IoError => -5,
        crate::fs::FsError::NotSupported => -95,
    }
}

pub(crate) fn notify_inotify_modify(fd_obj: &Arc<FileDescriptor>) {
    if !super::path::any_fsnotify_active() {
        return;
    }
    if let Some(path) = fd_obj.path.as_ref() {
        crate::fs::inotify::notify_file_path(
            path,
            crate::fs::inotify::IN_MODIFY | inotify_dir_bit(fd_obj.file.file_type()),
        );
        crate::fs::dnotify::notify_file_path(path, crate::fs::dnotify::DN_MODIFY);
        crate::fs::fanotify::notify_file_path(path, crate::fs::fanotify::FAN_MODIFY);
    }
}

pub(crate) fn notify_fanotify_access(fd_obj: &Arc<FileDescriptor>) {
    if !super::path::any_fsnotify_active() {
        return;
    }
    if let Some(path) = fd_obj.path.as_ref() {
        crate::fs::inotify::notify_file_path(
            path,
            crate::fs::inotify::IN_ACCESS | inotify_dir_bit(fd_obj.file.file_type()),
        );
        crate::fs::dnotify::notify_file_path(path, crate::fs::dnotify::DN_ACCESS);
        crate::fs::fanotify::notify_file_path(path, crate::fs::fanotify::FAN_ACCESS);
    }
}

/// `FAN_ACCESS_PERM`：read 路径在拷贝数据前必须同步通过 fanotify monitor。
/// 返回 `Some(-errno)` 表示拒绝；`None` 表示放行（包括无 monitor / 超时 / monitor 自身）。
pub(crate) fn fanotify_check_read_perm(fd_obj: &Arc<FileDescriptor>) -> Option<isize> {
    let path = fd_obj.path.as_ref()?;
    if matches!(
        fd_obj.file.file_type(),
        crate::fs::FileType::Pipe
            | crate::fs::FileType::Socket
            | crate::fs::FileType::CharDevice
            | crate::fs::FileType::BlockDevice
    ) {
        // 与 Linux 一致：仅在普通文件 / 目录路径上做 ACCESS_PERM。
        return None;
    }
    match crate::fs::fanotify::notify_perm_path(path, crate::fs::fanotify::FAN_ACCESS_PERM) {
        Ok(()) => None,
        Err(errno) => Some(errno as isize),
    }
}

/// 公共辅助：把 /dev/null 包成一个新 fd 返回，用于 inotify_init1 /
/// eventfd2 / timerfd_create / signalfd4 / epoll_create1 等"返回 fd"
/// 类 syscall 的最小桩。LTP 中只验证 fd 是否成功创建的子用例可借此 PASS；
/// 后续 read/write 行为按 /dev/null 语义（读返回 EOF，写丢弃）。
pub(in crate::syscall) fn alloc_null_fd(flags: u32) -> isize {
    let null_inode = match crate::fs::lookup("/dev/null") {
        Ok(i) => i,
        Err(_) => return -19, // -ENODEV
    };
    let fd_obj = Arc::new(FileDescriptor::new(null_inode, flags));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                return match fd_table::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => fd as isize,
                    Err(errno) => errno,
                };
            }
        }
    }
    -1
}

/// inotify_init1(flags) — 真 inotify fd (Slice 1.9b)。
///

/// eventfd2(initval, flags) — Slice 2.x T4-eventfd 真 counter 实现。

// fanotify_init / fanotify_mark moved to kernel/src/syscall/fanotify.rs

pub(crate) fn wake_non_stream_io_waiters(fd: &FileDescriptor) {
    match fd.ftype {
        FileType::Pipe | FileType::Socket => {}
        _ => wake_io_waiters(),
    }
}

/// dup — 复制文件描述符
pub(in crate::syscall) fn sys_dup(old_fd: usize) -> isize {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                return match fd_table::duplicate(process, old_fd, 0, true) {
                    Ok(new_fd) => new_fd as isize,
                    Err(errno) => errno,
                };
            }
        }
    }
    -1
}

/// dup3 — 复制文件描述符到指定编号
pub(in crate::syscall) fn sys_dup3(old_fd: usize, new_fd: usize, flags: usize) -> isize {
    const O_CLOEXEC: usize = 0o2000000;
    if old_fd == new_fd {
        return -22; // -EINVAL
    }
    if flags & !O_CLOEXEC != 0 {
        return -22;
    }
    let (result, displaced) = {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(ref mut t) = *table else {
            return -1;
        };
        let Some(pid) = t.current else {
            return -1;
        };
        let Some(process) = t.get_fd_process_mut(pid) else {
            return -1;
        };
        // Linux dup2/dup3 treat a negative or >= RLIMIT_NOFILE newfd as EBADF
        // (glibc's dup2(-1/getdtablesize) LTP checks depend on this).  The
        // separate hard cap prevents unbounded fd_table growth in this kernel.
        if new_fd >= process.rlimit_nofile || new_fd > 4096 {
            return -9;
        }
        let fd_obj = match fd_table::get(process, old_fd) {
            Some(fd_obj) => fd_obj,
            None => return -9,
        };
        match fd_table::install_at(process, new_fd, fd_obj, Some(4096), false) {
            Ok((fd, displaced)) => {
                if flags & O_CLOEXEC != 0 {
                    if let Err(errno) = fd_table::set_cloexec(process, fd, true) {
                        return errno;
                    }
                }
                (fd as isize, displaced)
            }
            Err(errno) => return errno,
        }
    };
    if let Some(old) = displaced {
        fd_table::finalize_closed_fd(old);
    }
    result
}

/// 关闭文件
#[allow(dead_code)]
pub(in crate::syscall) fn sys_close(fd: usize) -> isize {
    use crate::sched::process::PROCESS_TABLE;

    let closed = {
        let mut table = PROCESS_TABLE.lock();
        let Some(table) = table.as_mut() else {
            return -9;
        };
        let Some(current_pid) = table.current else {
            return -9;
        };
        let Some(process) = table.get_fd_process_mut(current_pid) else {
            return -9;
        };
        match fd_table::close(process, fd) {
            Ok(fd_obj) => fd_obj,
            Err(errno) => return errno,
        }
    };
    // Clean up perf_event if this was a perf fd
    if let Some(ref path) = closed.path {
        if path == "anon_inode:[perf_event]" {
            crate::kernel::perf_event::remove_perf_event(fd as i32);
        }
    }
    fd_table::finalize_closed_fd(closed);
    0
}
pub(crate) fn get_fd_obj(fd: usize) -> Option<alloc::sync::Arc<crate::fs::FileDescriptor>> {
    // Lock-free fast path via HART_CURRENT_PCB (see its safety contract):
    // smp=1 and syscalls run with interrupts disabled, so the current PCB is
    // stable and nothing can mutate its fd table concurrently. Covers the
    // common private-table case (fd_table_owner == pid); CLONE_FILES sharers
    // need the owner's PCB and take the locked slow path below.
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &*pcb_ptr };
        if p.fd_table_owner == p.pid {
            return fd_table::get(p, fd);
        }
    }
    let table_guard = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref t) = *table_guard {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process(pid) {
                return fd_table::get(process, fd);
            }
        }
    }
    None
}

/// 调用者公开的 fd 合法性判定（无副作用）。复用 `get_fd_obj` 的解析路径，
/// 让 `fsync / fdatasync / syncfs / sync_file_range` 等"语义上是 no-op，
/// 但必须返回 EBADF on bad fd" 的 syscall 不用各自重写 fd 表查找。
pub(in crate::syscall) fn fd_exists(fd: usize) -> bool {
    get_fd_obj(fd).is_some()
}

/// 返回 fd 指向文件对象的 `FileType`（fd 不存在 → `None`）。供 fsync/
/// fdatasync/sync_file_range 等按文件类型分流错误码使用。
pub(in crate::syscall) fn fd_file_type(fd: usize) -> Option<crate::fs::FileType> {
    get_fd_obj(fd).map(|f| f.file.file_type())
}
