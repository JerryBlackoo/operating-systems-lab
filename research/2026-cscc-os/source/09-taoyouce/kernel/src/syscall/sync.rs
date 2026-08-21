// kernel/src/syscall/sync.rs — file synchronization & advisory locking syscalls
//
// Extracted from compat.rs (CLUSTER 48) to follow Linux fs/sync.c + fs/locks.c
// one-concept-per-file principle.
//
// Contains: sync, fsync, fdatasync, syncfs, sync_file_range, flock.

/// `sync(2)` — flush all in-memory FS state to backing store.
/// Per Linux fs/sync.c ksys_sync(): iterates all mounted filesystems and
/// calls their sync_filesystem(), which for ext4 triggers jbd2 journal commit.
pub(in crate::syscall) fn sys_sync() -> isize {
    // Commit the ext4 journal to flush all dirty COW data to disk.
    let _ = crate::fs::ext4::journal_commit();
    0
}

/// `fsync(fd)` — flush a single file's pages.
/// 真实化要点：必须先验证 `fd` 在调用进程 fd 表内存在，否则返回 `-EBADF`，
/// 否则 LTP `fsync01..04` 会 TFAIL。落盘部分对 ramfs 是 no-op。
pub(in crate::syscall) fn sys_fsync(fd: usize) -> isize {
    match crate::syscall::fd::fd_file_type(fd) {
        None => -9, // -EBADF
        Some(ft) if !fsync_supported(ft) => -22, // -EINVAL
        Some(_) => {
            // Per Linux vfs_fsync_range(): call file->f_op->fsync().
            // For ext4 files, this triggers journal commit for the file's data.
            if let Some(fd_obj) = super::fd::get_fd_obj(fd) {
                let _ = fd_obj.file.as_file().map(|f| f.sync());
            }
            0
        }
    }
}

/// `fdatasync(fd)` — same as fsync without metadata; same checks suffice.
pub(in crate::syscall) fn sys_fdatasync(fd: usize) -> isize {
    match crate::syscall::fd::fd_file_type(fd) {
        None => -9, // -EBADF
        Some(ft) if !fsync_supported(ft) => -22, // -EINVAL
        Some(_) => {
            // Per Linux fdatasync(2): same as fsync but without metadata.
            if let Some(fd_obj) = super::fd::get_fd_obj(fd) {
                let _ = fd_obj.file.as_file().map(|f| f.sync());
            }
            0
        }
    }
}

/// fsync/fdatasync 仅对有 `f_op->fsync` 的文件类型有效：普通文件、目录。
/// 管道/套接字/字符设备/块设备等 → EINVAL。
fn fsync_supported(ft: crate::fs::FileType) -> bool {
    matches!(ft, crate::fs::FileType::Regular | crate::fs::FileType::Directory)
}

/// `syncfs(fd)` — sync the entire filesystem containing `fd`.
/// Per Linux fs/sync.c: triggers sync_filesystem() for the fs that fd belongs to.
pub(in crate::syscall) fn sys_syncfs(fd: usize) -> isize {
    if !crate::syscall::fd::fd_exists(fd) { return -9; }
    // Commit the ext4 journal (covers the main mounted filesystem).
    let _ = crate::fs::ext4::journal_commit();
    0
}

/// `sync_file_range(fd, offset, nbytes, flags)` — flush a byte range.
/// Linux defines flags ⊆ {SYNC_FILE_RANGE_WAIT_BEFORE=1, WRITE=2, WAIT_AFTER=4}.
/// Unknown flags → -EINVAL; negative offset/nbytes → -EINVAL; bad fd → -EBADF.
pub(in crate::syscall) fn sys_sync_file_range(
    fd: usize,
    offset: i64,
    nbytes: i64,
    flags: u32,
) -> isize {
    const SFR_WAIT_BEFORE: u32 = 1;
    const SFR_WRITE:        u32 = 2;
    const SFR_WAIT_AFTER:   u32 = 4;
    const SFR_ALL: u32 = SFR_WAIT_BEFORE | SFR_WRITE | SFR_WAIT_AFTER;
    if flags & !SFR_ALL != 0 { return -22; }
    // Linux ksys_sync_file_range：flags → offset<0 → endbyte<0 均 EINVAL，
    // 再 fdget(EBADF)，最后按 inode 类型分流。
    if offset < 0 || nbytes < 0 { return -22; }
    let endbyte = offset.checked_add(nbytes);
    if endbyte.map(|e| e < 0).unwrap_or(true) { return -22; }
    match crate::syscall::fd::fd_file_type(fd) {
        None => -9, // -EBADF
        // Linux：仅 S_ISREG/S_ISBLK/S_ISDIR/S_ISLNK 允许，其余 → ESPIPE
        // （sync_file_range01：sfd=/dev/null 字符设备期望 ESPIPE）。
        Some(ft) if !sync_file_range_supported(ft) => -29, // -ESPIPE
        Some(_) => 0,
    }
}

/// sync_file_range 允许的 inode 类型：普通文件/块设备/目录/符号链接。
fn sync_file_range_supported(ft: crate::fs::FileType) -> bool {
    matches!(
        ft,
        crate::fs::FileType::Regular
            | crate::fs::FileType::BlockDevice
            | crate::fs::FileType::Directory
            | crate::fs::FileType::Symlink
    )
}

// ---- flock(2) — BSD advisory whole-file locks (Linux fs/locks.c) -----------
//
// Per-inode 一张 `Vec<FlockEntry { fd_ino: usize, kind: u8 }>`（kind 1=SH, 2=EX）；
// `fd_ino` 取自 `Arc<FileDescriptor>` 指针的稳定 hash，等价 BSD 中 "per
// open file" 的 owner — `dup` 出来的 fd 共享同一 fd_obj，自动同一 owner。
//
// * `LOCK_SH`：与现有 EX 冲突；与 SH 共存。
// * `LOCK_EX`：与任何其它 owner 的锁冲突。
// * `LOCK_UN`：清掉本 fd_obj 在该 inode 的全部 flock 记录。
// * `LOCK_NB`：冲突时返 `-EWOULDBLOCK(11)`；不带 NB 时（阻塞）我们让出 CPU
//   重试，被 pending unblocked signal 打断 → `-EINTR(4)`。
// * 重复加锁（同 fd_obj 再 `LOCK_*`）替换原 entry，对应 Linux 的 "atomic
//   upgrade/downgrade"。
// * fd 关闭时由 `finalize_closed_fd` 调 `release_flock_for_fd_obj` 清表。
pub(in crate::syscall) fn sys_flock(fd: i32, operation: i32) -> isize {
    const LOCK_SH: i32 = 1;
    const LOCK_EX: i32 = 2;
    const LOCK_NB: i32 = 4;
    const LOCK_UN: i32 = 8;
    let nonblock = operation & LOCK_NB != 0;
    let op = operation & !LOCK_NB;
    if !matches!(op, LOCK_SH | LOCK_EX | LOCK_UN) { return -22; }
    if fd < 0 { return -9; }

    let fd_obj = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -9 };
        let pid = match t.current { Some(p) => p, None => return -9 };
        let proc = match t.get_fd_process(pid) { Some(p) => p, None => return -9 };
        match crate::fs::fd::get(proc, fd as usize) { Some(f) => f, None => return -9 }
    };
    let inode_id = fd_obj.file.ino();
    let fd_owner = alloc::sync::Arc::as_ptr(&fd_obj) as usize;

    if op == LOCK_UN {
        crate::fs::file_lock::flock_release(inode_id, fd_owner);
        return 0;
    }
    let kind: u8 = if op == LOCK_SH { 1 } else { 2 };

    // 阻塞语义说明：`suspend_current_and_run_next()` 标注 `-> !` — 唤醒时
    // 会被调度器从 ecall 处重新进入 syscall，再次跑到这里。所以这个 loop
    // 实际上 *最多* 走一次 try + 一次 suspend；suspend 之后整 syscall 会被
    // 重新 dispatch（与 `file_lock::set_lock_w` 同一模式）。
    let pid_for_signal = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table.as_ref().and_then(|t| t.current).unwrap_or(0)
    };
    // 进入前先检查 pending unblocked signal — 因为 suspend 后重新 dispatch
    // 仍会回到这条路径，等价于"唤醒后第二次 signal_check"。
    let pending_now = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table.as_ref().map_or(false, |t| {
            crate::kernel::signal::pending_interrupting_signal_for_process(t, pid_for_signal, 0)
        })
    };
    if pending_now && !crate::fs::file_lock::flock_try_set(inode_id, fd_owner, kind) {
        // 已挂起 + signal pending → -EINTR（仅在 try 仍失败时；try 成功照常返 0）
        return -4;
    }
    if crate::fs::file_lock::flock_try_set(inode_id, fd_owner, kind) {
        return 0;
    }
    if nonblock { return -11; } // -EWOULDBLOCK
    // 阻塞挂起 — 永不返回；唤醒后 syscall 从 ecall 重新进入。
    crate::sched::process::enter_wait_channel("flock_lock_inode");
    crate::sched::process::suspend_current_and_run_next();
}
