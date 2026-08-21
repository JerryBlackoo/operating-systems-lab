use alloc::sync::Arc;

use super::{FileDescriptor, FileType, READINESS_READ, READINESS_WRITE};
use crate::syscall::user::{read_user_buffer, write_user_buffer, UserSlice};

fn get_current_fd_obj(fd: usize) -> Option<Arc<FileDescriptor>> {
    let table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = table_guard.as_ref()?;
    let pid = table.current?;
    let process = table.get_fd_process(pid)?;
    crate::fs::fd::get(process, fd)
}

fn fs_error_to_errno(err: crate::fs::FsError) -> isize {
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

/// 判定一个 fd 当前是否 backed by storage（Regular / BlockDevice）— pipe /
/// socket / char dev 都不算。Linux `task_io_accounting` 里 `read_bytes` /
/// `write_bytes` 只在 storage 端累加，sendfile / splice / copy_file_range
/// 都需要这个判定。
fn fd_is_storage_backed(fd: usize) -> bool {
    get_current_fd_obj(fd)
        .map(|fd_obj| {
            matches!(
                fd_obj.file.file_type(),
                crate::fs::FileType::Regular | crate::fs::FileType::BlockDevice
            )
        })
        .unwrap_or(false)
}

/// 在指定 offset 读 fd（regular file 才生效），**不计 IO** — sendfile 内
/// 部使用，避免与 `sys_pread64` 入口的计费重复（sendfile 自身在 read
/// 成功后立刻 per-iteration `account_io_read`）。pipe / socket / chardev
/// 没有随机访问语义 → 返 -EINVAL，与 Linux sendfile 对非 seekable 源的
/// 行为一致。
fn read_from_fd_at_direct(fd: usize, offset: usize, data: &mut [u8]) -> isize {
    let Some(fd_obj) = get_current_fd_obj(fd) else { return -9 };
    if !fd_obj.readable() {
        return -9;
    }
    if let Some(file) = fd_obj.file.as_file() {
        match file.read(offset, data) {
            Ok(n) => n as isize,
            Err(crate::fs::FsError::PermissionDenied) => -9,
            Err(crate::fs::FsError::InvalidArgument) => -22,
            Err(err) => fs_error_to_errno(err),
        }
    } else {
        -22
    }
}

/// Helper: write a kernel byte slice directly to an fd by invoking the
/// FileDescriptor's `write()` method (bypassing the user-pointer
/// validation done by sys_write).
fn write_to_fd_direct(fd: usize, data: &[u8]) -> isize {
    let Some(fd_obj) = get_current_fd_obj(fd) else { return -9 };
    if !fd_obj.writable() {
        return -9;
    }
    match fd_obj.write(data) {
        Ok(n) => n as isize,
        Err(crate::fs::FsError::PermissionDenied) => -9,
        Err(crate::fs::FsError::WouldBlock) => -11,
        Err(err) => fs_error_to_errno(err),
    }
}

/// Helper: read directly from an fd into a kernel buffer (FileDescriptor::read
/// tracks the per-fd offset internally for regular files, and reads streamed
/// bytes for pipes / sockets / chardev).
fn read_from_fd_direct(fd: usize, data: &mut [u8]) -> isize {
    let Some(fd_obj) = get_current_fd_obj(fd) else { return -9 };
    if !fd_obj.readable() {
        return -9;
    }
    match fd_obj.read(data) {
        Ok(n) => n as isize,
        Err(crate::fs::FsError::PermissionDenied) => -9,
        Err(crate::fs::FsError::WouldBlock) => -11,
        Err(err) => fs_error_to_errno(err),
    }
}

/// sendfile(out_fd, in_fd, offset, count) — efficient zero-copy from in_fd to out_fd.
///
/// We implement it as a simple bounce-buffer loop (8 KiB) which is what most
/// LTP `sendfile0[1-9]` cases require. The `offset` argument is a Linux
/// `loff_t *` (8-byte user pointer); when non-NULL we read the starting offset
/// from it, use pread on the source, and write the new offset back. When NULL
/// (offset == 0) we use the in_fd's current position via plain read.
///
/// Returns total bytes actually transferred (which may be less than `count`
/// on short reads / out_fd backpressure), or `-errno` on early failure.
pub(crate) fn sys_sendfile(
    out_fd: i32,
    in_fd: i32,
    offset_ptr: usize,
    count: usize,
) -> isize {
    let in_fd_obj = match get_current_fd_obj(in_fd as usize) {
        Some(fd) => fd,
        None => return -9,
    };
    let out_fd_obj = match get_current_fd_obj(out_fd as usize) {
        Some(fd) => fd,
        None => return -9,
    };
    if !in_fd_obj.readable() || !out_fd_obj.writable() {
        return -9;
    }

    if count == 0 { return 0; }
    let mut transferred: usize = 0;
    let chunk: usize = 8 * 1024;
    // Optional user offset (loff_t = i64)
    let use_offset = offset_ptr != 0;
    let mut cur_off: i64 = 0;
    if use_offset {
        let bytes = match read_user_buffer(offset_ptr as *const u8, 8) {
            Ok(b) => b,
            Err(_) => return -14,
        };
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&bytes[..8]);
        cur_off = i64::from_le_bytes(buf);
        if cur_off < 0 { return -22; }
        // Linux validates the offset pointer is writable upfront (copy_to_user
        // probe in do_sendfile). A PROT_READ-only page must cause EFAULT before
        // any data is transferred (sendfile04 protection=PROT_READ case).
        if write_user_buffer(offset_ptr as *mut u8, &buf).is_err() {
            return -14; // -EFAULT
        }
    }
    let mut tmp = alloc::vec![0u8; chunk];
    // Slice 2.4 IO 计费（规则 9 — per-iteration，禁聚合）：
    //
    // Linux `do_sendfile()` 内部对每一轮 `splice_direct_to_actor` 都调
    // `task_io_account_read(ret)` 与 `task_io_account_write(ret)`，并且
    // `vfs_read` / `vfs_write` 入口各 `syscr++` / `syscw++`。所以一次
    // sendfile 传 N 个 chunk 实际产生 N 次 syscr + N 次 syscw 计费，不
    // 是合并为末尾一次 total。
    //
    // 我们这里循环内每次成功读 / 写完即立刻 `account_io_read(n, ...)` /
    // `account_io_write(n, ...)`，并各 +1 syscr/syscw（helper 内做）。
    // `read_bytes` / `write_bytes` 只在对应端 backed by Regular /
    // BlockDevice 时累加，所以预先各查一次 is_storage 即可，避免每轮
    // 重复进 PROCESS_TABLE 锁。
    let in_is_storage = fd_is_storage_backed(in_fd as usize);
    let out_is_storage = fd_is_storage_backed(out_fd as usize);
    while transferred < count {
        let want = chunk.min(count - transferred);
        // 源端 read：走不带计费的内部 helper（`read_from_fd_at_direct` /
        // `read_from_fd_direct`），避免与 `sys_pread64` / `sys_read` 入口
        // 的计费重复 — sendfile 自身在 read 成功后立刻 account 一次。
        let nread = if use_offset {
            read_from_fd_at_direct(in_fd as usize, cur_off as usize, &mut tmp[..want])
        } else {
            read_from_fd_direct(in_fd as usize, &mut tmp[..want])
        };
        if nread < 0 {
            // 读端早 error：不计这轮，直接返回（与 Linux 一致：失败的
            // vfs_read 不进 task_io_account_read）。
            return if transferred > 0 { transferred as isize } else { nread };
        }
        let nread = nread as usize;
        if nread == 0 { break; } // EOF — 不计 (Linux 同：n=0 也算一次 syscr，
        // 但调用链通常已在外层算过；我们这里若 transferred==0 直接返回 0，
        // 不需要计；若 transferred>0 则前面循环已逐次计过)。
        // ✅ Linux: 每次内部 vfs_read 成功 → task_io_account_read + syscr++。
        crate::sched::process::account_io_read(nread, in_is_storage);

        // 目标端 write：同上走不计费 helper，写成功后立刻 account 一次。
        let written = write_to_fd_direct(out_fd as usize, &tmp[..nread]);
        if written < 0 {
            return if transferred > 0 { transferred as isize } else { written };
        }
        let written = written as usize;
        // ✅ Linux: 每次内部 vfs_write 成功 → task_io_account_write + syscw++。
        // 注意按 written 而非 nread 计 — Linux short write 时只算实际写出量。
        crate::sched::process::account_io_write(written, out_is_storage);

        transferred += written;
        if use_offset { cur_off += written as i64; }
        if written < nread { break; } // short write — stop here
    }
    if use_offset {
        let buf = (cur_off as i64).to_le_bytes();
        let _ = write_user_buffer(offset_ptr as *mut u8, &buf);
    }
    transferred as isize
}

pub(crate) fn sys_splice(fd_in: i32, off_in: usize, fd_out: i32, off_out: usize,
              len: usize, flags: u32) -> isize {
    const SPLICE_F_MOVE: u32 = 0x01;
    const SPLICE_F_NONBLOCK: u32 = 0x02;
    const SPLICE_F_MORE: u32 = 0x04;
    const SPLICE_F_GIFT: u32 = 0x08;
    const KNOWN_FLAGS: u32 = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT;
    if flags & !KNOWN_FLAGS != 0 {
        return -22;
    }
    if len == 0 {
        return 0;
    }

    let fd_in_obj = match get_current_fd_obj(fd_in as usize) {
        Some(f) => f,
        None => return -9, // -EBADF
    };
    let fd_out_obj = match get_current_fd_obj(fd_out as usize) {
        Some(f) => f,
        None => return -9, // -EBADF
    };

    if !fd_in_obj.readable() || !fd_out_obj.writable() {
        return -9; // -EBADF
    }

    let in_ty = fd_in_obj.file.file_type();
    let out_ty = fd_out_obj.file.file_type();
    let in_is_pipe = matches!(in_ty, FileType::Pipe);
    let out_is_pipe = matches!(out_ty, FileType::Pipe);
    let in_is_pipe_backed_socket = matches!(in_ty, FileType::Socket) && fd_in_obj.file.ino() == 0;
    let out_is_pipe_backed_socket = matches!(out_ty, FileType::Socket) && fd_out_obj.file.ino() == 0;
    if !in_is_pipe && !out_is_pipe {
        return -22;
    }

    const O_APPEND: u32 = 0x400;
    if fd_out_obj.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_APPEND != 0 {
        return -22; // -EINVAL: Linux splice rejects O_APPEND output
    }

    if (in_is_pipe && off_in != 0) || (out_is_pipe && off_out != 0) {
        return -29;
    }

    // Linux 在真正读空 pipe / 写目标之前，会先判定端点是否支持 splice。
    // 否则像 LTP splice07 的 "empty pipe read end -> epoll/eventfd/timerfd"
    // 会错误地阻塞等待数据，而不是立即返回 EINVAL/EBADF。这里按能力矩阵
    // 前置拒绝明显不支持 splice 的 anon/char fd：
    //   * pipe -> regular/block/socket 允许（pipe -> socket 覆盖 splice05）
    //   * regular/block/socket/devfs char -> pipe 允许尝试
    //   * epoll/eventfd/signalfd/timerfd/io_uring 等 anon char fd 立即 EINVAL
    let in_is_pseudo_regular = matches!(in_ty, FileType::Regular)
        && fd_in_obj.path.as_ref().map_or(false, |p| {
            p.starts_with("pidfd:")
                || p.starts_with("bpf-map:")
                || p.starts_with("anon_inode:")
        });
    let out_is_pseudo_regular = matches!(out_ty, FileType::Regular)
        && fd_out_obj.path.as_ref().map_or(false, |p| {
            p.starts_with("pidfd:")
                || p.starts_with("bpf-map:")
                || p.starts_with("anon_inode:")
        });

    if in_is_pipe && !out_is_pipe {
        let out_can_splice_from_pipe = matches!(
            out_ty,
            FileType::Regular | FileType::BlockDevice
        ) && !out_is_pseudo_regular
            || out_is_pipe_backed_socket;
        if !out_can_splice_from_pipe {
            return -22;
        }
    }
    if !in_is_pipe && out_is_pipe {
        let in_devfs_char = matches!(in_ty, FileType::CharDevice)
            && fd_in_obj.path.as_ref().map_or(false, |p| p.starts_with("/dev/"));
        let in_can_splice_to_pipe = matches!(
            in_ty,
            FileType::Regular | FileType::BlockDevice
        ) && !in_is_pseudo_regular
            || in_is_pipe_backed_socket
            || in_devfs_char;
        if !in_can_splice_to_pipe {
            return -22;
        }
    }

    let in_is_storage = matches!(
        in_ty,
        crate::fs::FileType::Regular | crate::fs::FileType::BlockDevice
    );
    let out_is_storage = matches!(
        out_ty,
        crate::fs::FileType::Regular | crate::fs::FileType::BlockDevice
    );
    const O_NONBLOCK: u32 = 0o4000;
    let nonblock = flags & SPLICE_F_NONBLOCK != 0
        || fd_in_obj.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_NONBLOCK != 0
        || fd_out_obj.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_NONBLOCK != 0;

    if fd_in_obj.file.as_file().is_none() || fd_out_obj.file.as_file().is_none() {
        return -22; // -EINVAL
    }

    // 确定源偏移（管道时 off_in 必须为 NULL）
    let src_offset = if off_in != 0 {
        match read_user_buffer(off_in as *const u8, 8) {
            Ok(buf) => {
                let off = i64::from_le_bytes(buf[..8].try_into().unwrap());
                if off < 0 {
                    return -22; // -EINVAL：负偏移
                }
                off as usize
            }
            Err(_) => return -14,
        }
    } else {
        fd_in_obj.offset.load(core::sync::atomic::Ordering::Relaxed)
    };

    let dst_offset = if off_out != 0 {
        match read_user_buffer(off_out as *const u8, 8) {
            Ok(buf) => {
                let off = i64::from_le_bytes(buf[..8].try_into().unwrap());
                if off < 0 {
                    return -22; // -EINVAL：负偏移
                }
                off as usize
            }
            Err(_) => return -14,
        }
    } else {
        fd_out_obj.offset.load(core::sync::atomic::Ordering::Relaxed)
    };

    let mut total = 0usize;
    let mut cur_src = src_offset;
    let mut cur_dst = dst_offset;
    let mut remaining = len;

    while remaining > 0 {
        let chunk = remaining.min(65536);

        if in_is_pipe && out_is_pipe {
            let splice_res = {
                let src_file = fd_in_obj.file.as_file().unwrap();
                let dst_file = fd_out_obj.file.as_file().unwrap();
                src_file.splice_to(dst_file, chunk)
            };
            match splice_res {
                Ok(0) => break,
                Ok(n) => {
                    crate::sched::process::account_io_read(n, false);
                    crate::sched::process::account_io_write(n, false);
                    total += n;
                    remaining -= n;
                    if n < chunk {
                        break;
                    }
                    continue;
                }
                Err(crate::fs::FsError::WouldBlock) => {
                    if total > 0 { break; }
                    if nonblock { return -11; }
                    if crate::syscall::has_other_runnable() {
                        let ready = {
                            let src_file = fd_in_obj.file.as_file().unwrap();
                            let dst_file = fd_out_obj.file.as_file().unwrap();
                            src_file.poll_read() && dst_file.poll_write()
                        };
                        if ready {
                            continue;
                        }
                        drop(fd_in_obj);
                        drop(fd_out_obj);
                        crate::sched::process::enter_wait_channel("pipe_wait");
                        crate::sched::process::suspend_current_and_run_next();
                        continue;
                    }
                    crate::syscall::inline_wfi_tick();
                    continue;
                }
                Err(err) => {
                    if total > 0 { break; }
                    return fs_error_to_errno(err);
                }
            }
        }

        let mut buf = alloc::vec![0u8; chunk];

        let n_read = match fd_in_obj.file.as_file().unwrap().read(if in_is_pipe { 0 } else { cur_src }, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(crate::fs::FsError::WouldBlock) => {
                if total > 0 { break; }
                if nonblock { return -11; }
                if crate::syscall::has_other_runnable() {
                    let current_pid = {
                        let table = crate::sched::process::PROCESS_TABLE.lock();
                        table.as_ref().and_then(|table| table.current)
                    };
                    if let Some(pid) = current_pid {
                        fd_in_obj.file.as_file().unwrap().register_waiter(READINESS_READ, pid, None);
                    }
                    if fd_in_obj.file.as_file().unwrap().poll_read() {
                        continue;
                    }
                    drop(buf);
                    drop(fd_in_obj);
                    drop(fd_out_obj);
                    crate::sched::process::enter_wait_channel("pipe_read");
                    crate::sched::process::suspend_current_and_run_next();
                    continue;
                } else {
                    crate::syscall::inline_wfi_tick();
                    continue;
                }
            }
            Err(err) => {
                if total > 0 { break; }
                return fs_error_to_errno(err);
            }
        };
        // Slice 2.4 IO 计费（规则 9，per-iteration，禁聚合）：
        // 对齐 Linux `do_splice` 内 `do_splice_to`/`do_splice_from` 每次
        // 内部 vfs_read / vfs_write 都触发 `task_io_account_*` + syscr/syscw++。
        crate::sched::process::account_io_read(n_read, in_is_storage);

        let n_written = match fd_out_obj.file.as_file().unwrap().write(if out_is_pipe { 0 } else { cur_dst }, &buf[..n_read]) {
            Ok(n) => n,
            Err(crate::fs::FsError::WouldBlock) => {
                if total > 0 { break; }
                if nonblock { return -11; }
                if crate::syscall::has_other_runnable() {
                    let current_pid = {
                        let table = crate::sched::process::PROCESS_TABLE.lock();
                        table.as_ref().and_then(|table| table.current)
                    };
                    if let Some(pid) = current_pid {
                        fd_out_obj.file.as_file().unwrap().register_waiter(READINESS_WRITE, pid, None);
                    }
                    if fd_out_obj.file.as_file().unwrap().poll_write() {
                        continue;
                    }
                    drop(buf);
                    drop(fd_in_obj);
                    drop(fd_out_obj);
                    crate::sched::process::enter_wait_channel("pipe_write");
                    crate::sched::process::suspend_current_and_run_next();
                    continue;
                } else {
                    crate::syscall::inline_wfi_tick();
                    continue;
                }
            }
            Err(err) => {
                if total > 0 { break; }
                return fs_error_to_errno(err);
            }
        };
        crate::sched::process::account_io_write(n_written, out_is_storage);

        if !in_is_pipe {
            cur_src += n_read;
        }
        if !out_is_pipe {
            cur_dst += n_written;
        }
        total += n_written;
        remaining -= n_written;

        if n_written < n_read {
            break;
        }
    }

    // 更新偏移
    if off_in != 0 {
        let new_off = (src_offset + total) as u64;
        let _ = write_user_buffer(off_in as *mut u8, &new_off.to_le_bytes());
    } else if !in_is_pipe {
        fd_in_obj.offset.fetch_add(total, core::sync::atomic::Ordering::Relaxed);
    }

    if off_out != 0 {
        let new_off = (dst_offset + total) as u64;
        let _ = write_user_buffer(off_out as *mut u8, &new_off.to_le_bytes());
    } else if !out_is_pipe {
        fd_out_obj.offset.fetch_add(total, core::sync::atomic::Ordering::Relaxed);
    }

    total as isize
}

pub(crate) fn sys_tee(fd_in: i32, fd_out: i32, len: usize, flags: u32) -> isize {
    const SPLICE_F_MOVE: u32 = 0x01;
    const SPLICE_F_NONBLOCK: u32 = 0x02;
    const SPLICE_F_MORE: u32 = 0x04;
    const SPLICE_F_GIFT: u32 = 0x08;
    const KNOWN_FLAGS: u32 = SPLICE_F_MOVE | SPLICE_F_NONBLOCK | SPLICE_F_MORE | SPLICE_F_GIFT;
    if flags & !KNOWN_FLAGS != 0 {
        return -22;
    }
    if len == 0 {
        return 0;
    }

    let fd_in_obj = match get_current_fd_obj(fd_in as usize) {
        Some(f) => f,
        None => return -9,
    };
    let fd_out_obj = match get_current_fd_obj(fd_out as usize) {
        Some(f) => f,
        None => return -9,
    };

    if !matches!(fd_in_obj.file.file_type(), FileType::Pipe)
        || !matches!(fd_out_obj.file.file_type(), FileType::Pipe)
    {
        return -22;
    }

    if fd_in_obj.file.as_file().is_none() || fd_out_obj.file.as_file().is_none() {
        return -22;
    }
    let same_pipe = {
        let src_file = fd_in_obj.file.as_file().unwrap();
        let dst_file = fd_out_obj.file.as_file().unwrap();
        src_file.pipe_buffer_id().is_some()
            && src_file.pipe_buffer_id() == dst_file.pipe_buffer_id()
    };
    if same_pipe {
        return -22;
    }

    let mut total = 0usize;
    let mut remaining = len;
    while remaining > 0 {
        let want = remaining.min(65536);
        let tee_res = {
            let src_file = fd_in_obj.file.as_file().unwrap();
            let dst_file = fd_out_obj.file.as_file().unwrap();
            src_file.tee_to(dst_file, want)
        };
        match tee_res {
            Ok(0) => break,
            Ok(n) => {
                crate::sched::process::account_io_read(n, false);
                crate::sched::process::account_io_write(n, false);
                total += n;
                remaining -= n;
                if n < want {
                    break;
                }
            }
            Err(crate::fs::FsError::WouldBlock) => {
                if total > 0 {
                    break;
                }
                if flags & SPLICE_F_NONBLOCK != 0 {
                    return -11;
                }
                if crate::syscall::has_other_runnable() {
                    let current_pid = {
                        let table = crate::sched::process::PROCESS_TABLE.lock();
                        table.as_ref().and_then(|table| table.current)
                    };
                    if let Some(pid) = current_pid {
                        fd_in_obj.file.as_file().unwrap().register_waiter(READINESS_READ, pid, None);
                        fd_out_obj.file.as_file().unwrap().register_waiter(READINESS_WRITE, pid, None);
                    }
                    let ready = {
                        let src_file = fd_in_obj.file.as_file().unwrap();
                        let dst_file = fd_out_obj.file.as_file().unwrap();
                        src_file.poll_read() && dst_file.poll_write()
                    };
                    if ready {
                        continue;
                    }
                    drop(fd_in_obj);
                    drop(fd_out_obj);
                    crate::sched::process::enter_wait_channel("pipe_wait");
                    crate::sched::process::suspend_current_and_run_next();
                } else {
                    crate::syscall::inline_wfi_tick();
                }
            }
            Err(err) => return fs_error_to_errno(err),
        }
    }
    total as isize
}

/// vmsplice(75) — splice user pages into a pipe.
///
/// Linux semantics: copies user memory described by iovec into the write end
/// of a pipe. Returns total bytes written. The fd must be a pipe write end.
pub(crate) fn sys_vmsplice(fd: i32, iov_ptr: usize, nr_segs: usize, flags: u32) -> isize {
    const SPLICE_F_NONBLOCK: u32 = 0x02;
    const SPLICE_F_GIFT: u32 = 0x08;
    const KNOWN_FLAGS: u32 = 0x01 | SPLICE_F_NONBLOCK | 0x04 | SPLICE_F_GIFT;
    if flags & !KNOWN_FLAGS != 0 {
        return -22; // EINVAL
    }
    if nr_segs == 0 {
        return 0;
    }
    if nr_segs > 1024 {
        return -22; // UIO_MAXIOV
    }

    let fd_obj = match get_current_fd_obj(fd as usize) {
        Some(f) => f,
        None => return -9, // EBADF
    };

    // Must be a pipe — Linux returns EBADF for non-pipe fds
    if fd_obj.file.file_type() != crate::fs::FileType::Pipe {
        return -9; // EBADF
    }

    // Read iovec array from user space (each iovec: base(8) + len(8) = 16 bytes)
    let iov_size = nr_segs * 16;
    let iov_buf = match UserSlice::<u8>::new(iov_ptr as *const u8, iov_size).read_bytes() {
        Ok(b) => b,
        Err(_) => return -14, // EFAULT
    };

    let is_write = fd_obj.writable();
    let is_read = fd_obj.readable();

    if !is_write && !is_read {
        return -9; // EBADF
    }

    let mut total: usize = 0;

    let nonblock = flags & SPLICE_F_NONBLOCK != 0;

    if is_write {
        // Write direction: user memory -> pipe
        for i in 0..nr_segs {
            let off = i * 16;
            let base = u64::from_le_bytes(iov_buf[off..off + 8].try_into().unwrap()) as usize;
            let len = u64::from_le_bytes(iov_buf[off + 8..off + 16].try_into().unwrap()) as usize;
            if len == 0 { continue; }
            let data = match UserSlice::<u8>::new(base as *const u8, len).read_bytes() {
                Ok(d) => d,
                Err(_) => return if total > 0 { total as isize } else { -14 },
            };
            let mut retries = 0usize;
            loop {
                match fd_obj.write(&data) {
                    Ok(n) => { total += n; break; }
                    Err(crate::fs::FsError::WouldBlock) => {
                        if nonblock {
                            return if total > 0 { total as isize } else { -11 }; // EAGAIN
                        }
                        if crate::syscall::has_other_runnable() {
                            if fd_obj.file.as_file().map_or(false, |f| f.poll_write()) {
                                continue;
                            }
                            crate::sched::process::enter_wait_channel("pipe_write");
                            crate::sched::process::suspend_current_and_run_next();
                        } else {
                            retries += 1;
                            if retries > 500 {
                                return if total > 0 { total as isize } else { -4 }; // EINTR
                            }
                            crate::syscall::inline_wfi_tick();
                        }
                    }
                    Err(crate::fs::FsError::BrokenPipe) => {
                        return if total > 0 { total as isize } else { -32 }; // EPIPE
                    }
                    Err(_) => {
                        return if total > 0 { total as isize } else { -32 }; // EPIPE
                    }
                }
            }
        }
    } else {
        // Read direction: pipe -> user memory
        for i in 0..nr_segs {
            let off = i * 16;
            let base = u64::from_le_bytes(iov_buf[off..off + 8].try_into().unwrap()) as usize;
            let len = u64::from_le_bytes(iov_buf[off + 8..off + 16].try_into().unwrap()) as usize;
            if len == 0 { continue; }
            let mut buf = alloc::vec![0u8; len];
            match fd_obj.read(&mut buf) {
                Ok(n) => {
                    if n == 0 { break; } // EOF
                    if UserSlice::<u8>::new_mut(base as *mut u8, n)
                        .write_bytes(&buf[..n])
                        .is_err()
                    {
                        return if total > 0 { total as isize } else { -14 };
                    }
                    total += n;
                }
                Err(_) => {
                    return if total > 0 { total as isize } else { -11 }; // EAGAIN
                }
            }
        }
    }

    total as isize
}
