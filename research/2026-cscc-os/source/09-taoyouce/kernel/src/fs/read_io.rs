//! Read-side file I/O syscall helpers.

use crate::fs::{FileType, READINESS_READ};
use crate::syscall::user::{is_user_addr_valid, read_user_buffer, write_user_buffer, read_user_to_slice};
use crate::syscall::{has_other_runnable, inline_wfi_tick, SyscallError};

/// Per-hart static read buffer — avoids O(n) linked-list heap alloc/dealloc
/// for large reads (bw_file_rd uses 64KB blocks).  SMP=1, so a single
/// global buffer with a SpinLock is sufficient.
const STATIC_READ_BUF_SIZE: usize = 128 * 1024;
static STATIC_READ_BUF: crate::sync::SpinLock<[u8; STATIC_READ_BUF_SIZE]> =
    crate::sync::SpinLock::new([0u8; STATIC_READ_BUF_SIZE]);

/// readv — 分散读取
#[allow(dead_code)]
pub(crate) fn sys_readv(fd: usize, iov_ptr: usize, iovcnt: usize) -> isize {
    if iovcnt > 1024 {
        return SyscallError::InvalidArgument as isize;
    }
    if iovcnt == 0 {
        return 0;
    }

    let iov_size = iovcnt * 16;
    let iov_data = match read_user_buffer(iov_ptr as *const u8, iov_size) {
        Ok(d) => d,
        Err(_) => return -14,
    };

    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    if !fd_obj.readable() {
        return -9;
    }
    if fd_obj.ftype == FileType::Directory {
        return -21;
    }
    if let Some(errno) = crate::syscall::fd::fanotify_check_read_perm(&fd_obj) {
        return errno;
    }

    let is_storage = matches!(
        fd_obj.ftype,
        FileType::Regular | FileType::BlockDevice
    );
    let finish = |total: usize| -> isize {
        crate::sched::process::account_io_read(total, is_storage);
        total as isize
    };

    const SSIZE_MAX: usize = isize::MAX as usize;
    let mut total_len: usize = 0;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_data[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_data[off + 8..off + 16].try_into().unwrap());
        if len > SSIZE_MAX {
            return -22;
        }
        total_len = total_len.saturating_add(len);
        if total_len > SSIZE_MAX {
            return -22;
        }
        if len > 0 && base == 0 {
            return -14;
        }
        if len > 0 && !is_user_addr_valid(base, len) {
            return -14;
        }
    }

    let mut total: usize = 0;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_data[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_data[off + 8..off + 16].try_into().unwrap());
        if len == 0 {
            continue;
        }

        // Regular 文件大段直读用户页（同 sys_read 快路径）：musl fread 大块
        // 请求走 readv(iov[stdio_buf, user_buf])，通用 staging 路径对每段都是
        // kernel_buf memcpy + write_user_buffer 双拷贝。ramfs read_to_user
        // 直接拷进用户页，省一半 memcpy。
        if len > 1024 && fd_obj.ftype == FileType::Regular {
            if let Some(file) = fd_obj.file.as_file() {
                let offset = fd_obj.offset.load(core::sync::atomic::Ordering::Relaxed);
                match file.read_to_user(offset, base as *mut u8, len) {
                    Ok(n) => {
                        fd_obj
                            .offset
                            .fetch_add(n, core::sync::atomic::Ordering::Relaxed);
                        if n > 0 {
                            crate::syscall::fd::notify_fanotify_access(&fd_obj);
                        }
                        crate::syscall::fd::wake_non_stream_io_waiters(&fd_obj);
                        total += n;
                        if n < len {
                            return finish(total);
                        }
                        continue;
                    }
                    Err(crate::fs::FsError::NotSupported) => {}
                    Err(_) => return if total > 0 { finish(total) } else { -5 },
                }
            }
        }

        let buf_len = len.min(65536);
        let mut kernel_buf = alloc::vec![0u8; buf_len];
        let mut alone_retries = 0usize;
        loop {
            match fd_obj.read(&mut kernel_buf) {
                Ok(n) => {
                    if n == 0 {
                        return finish(total);
                    }
                    crate::syscall::fd::notify_fanotify_access(&fd_obj);
                    if write_user_buffer(base as *mut u8, &kernel_buf[..n]).is_err() {
                        return -14;
                    }
                    crate::syscall::fd::wake_non_stream_io_waiters(&fd_obj);
                    total += n;
                    break;
                }
                Err(crate::fs::FsError::WouldBlock) => {
                    if has_other_runnable() {
                        drop(kernel_buf);
                        drop(iov_data);
                        drop(fd_obj);
                        crate::sched::process::enter_wait_channel("pipe_wait");
                        crate::sched::process::suspend_current_and_run_next();
                    } else {
                        alone_retries += 1;
                        if alone_retries > 500 {
                            return finish(total);
                        }
                        inline_wfi_tick();
                        continue;
                    }
                }
                Err(_) => return -5,
            }
        }
    }

    finish(total)
}

/// 读文件
#[allow(dead_code)]
pub(crate) fn sys_read(fd: usize, buf: *mut u8, len: usize) -> isize {
    match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => {
            if !f.readable() {
                return -9;
            }
            // perf_event FD: intercept read to return counter value
            if let Some(ref path) = f.path {
                if path == "anon_inode:[perf_event]" {
                    if len < 8 {
                        return -22; // EINVAL
                    }
                    let mut counter_buf = [0u8; 8];
                    let result = crate::kernel::perf_event::perf_event_read(fd as i32, &mut counter_buf);
                    if result > 0 {
                        if write_user_buffer(buf, &counter_buf[..result as usize]).is_ok() {
                            return result;
                        } else {
                            return -14;
                        }
                    }
                    return result;
                }
            }
            // 热路径只做一次 dyn 分发，后续分支复用。
            let ftype = f.ftype;
            if ftype == FileType::Directory {
                return -21;
            }
            if let Some(errno) = crate::syscall::fd::fanotify_check_read_perm(&f) {
                return errno;
            }
            // O_NOATIME (0o1000000): the open(2) flag suppresses access-time
            // updates on read for this descriptor (open12 test_noatime).
            const O_NOATIME: u32 = 0o1000000;
            let noatime =
                f.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_NOATIME != 0;
            let buf_len = len.min(1024 * 1024);

            // Fast path: direct read into user pages (no kernel buffer copy).
            // Supported by ramfs — eliminates the double copy for bw_file_rd.
            if buf_len > 1024 && ftype == FileType::Regular {
                if let Some(file) = f.file.as_file() {
                    let offset = f.offset.load(core::sync::atomic::Ordering::Relaxed);
                    match file.read_to_user(offset, buf, buf_len) {
                        Ok(n) => {
                            f.offset.fetch_add(n, core::sync::atomic::Ordering::Relaxed);
                            if !noatime {
                                if let Some(ref p) = f.path {
                                    crate::fs::touch_atime(&f.file, p);
                                }
                            }
                            if n > 0 {
                                crate::syscall::fd::notify_fanotify_access(&f);
                            }
                            crate::syscall::fd::wake_non_stream_io_waiters(&f);
                            crate::sched::process::account_io_read(n, true);
                            return n as isize;
                        }
                        Err(crate::fs::FsError::NotSupported) => {}
                        Err(err) => return crate::syscall::fd::fs_error_to_errno(err),
                    }
                }
            }

            // Pipe zero-copy path: ring buffer is written straight into the
            // user pages (see PipeReadEnd::read_to_user), skipping the kernel
            // staging buffer's second full-size memcpy. Falls back to the
            // generic path on NotSupported (e.g. user page resolve failure).
            if buf_len > 1024
                && (ftype == FileType::Pipe || f.file.is_unix_socketpair())
            {
                if let Some(file) = f.file.as_file() {
                    loop {
                        match file.read_to_user(0, buf, buf_len) {
                            Ok(n) => {
                                if n > 0 {
                                    crate::syscall::fd::notify_fanotify_access(&f);
                                }
                                crate::syscall::fd::wake_non_stream_io_waiters(&f);
                                crate::sched::process::account_io_read(n, false);
                                return n as isize;
                            }
                            Err(crate::fs::FsError::NotSupported) => break,
                            Err(crate::fs::FsError::WouldBlock) => {
                                const O_NONBLOCK: u32 = 0x800;
                                if f.status_flags.load(core::sync::atomic::Ordering::Relaxed)
                                    & O_NONBLOCK
                                    != 0
                                {
                                    return -11;
                                }
                                if has_other_runnable() {
                                    let pid = crate::sched::process::HART_CURRENT_PID
                                        .load(core::sync::atomic::Ordering::Relaxed);
                                    if pid != 0 {
                                        file.register_waiter(READINESS_READ, pid, None);
                                    }
                                    crate::sched::process::enter_wait_channel("pipe_read");
                                    crate::sched::process::suspend_current_and_run_next();
                                } else {
                                    inline_wfi_tick();
                                    continue;
                                }
                            }
                            Err(err) => {
                                return crate::syscall::fd::fs_error_to_errno(err);
                            }
                        }
                    }
                }
            }

            // Socket 大块读快路径：直接从接收队列取走整段 Vec（drain 对整段
            // 命中是零拷贝 move），一次 write_user_buffer 写回用户页；跳过
            // 通用路径的 128KB 零初始化 staging 缓冲 + 额外一次全量 memcpy。
            if buf_len >= 4096 && ftype == FileType::Socket {
                if let Ok(sock) = crate::net::socket::socket_from_fd(fd as i32) {
                    loop {
                        match crate::net::socket::recvfrom_sock_coalesce(&sock, buf_len) {
                            Ok((data, _from)) => {
                                let n = data.len().min(buf_len);
                                let ok = write_user_buffer(buf, &data[..n]).is_ok();
                                crate::net::bufpool::put(data);
                                if !ok {
                                    return -14;
                                }
                                crate::syscall::fd::wake_non_stream_io_waiters(&f);
                                crate::sched::process::account_io_read(n, false);
                                return n as isize;
                            }
                            Err(crate::net::socket::SocketError::WouldBlock) => {
                                if crate::net::socket::socket_is_closed(&sock) {
                                    return 0;
                                }
                                const O_NONBLOCK: u32 = 0x800;
                                if f.status_flags.load(core::sync::atomic::Ordering::Relaxed)
                                    & O_NONBLOCK
                                    != 0
                                {
                                    return -11;
                                }
                                if has_other_runnable() {
                                    let pid = crate::sched::process::HART_CURRENT_PID
                                        .load(core::sync::atomic::Ordering::Relaxed);
                                    if pid != 0 {
                                        if let Some(file) = f.file.as_file() {
                                            file.register_waiter(READINESS_READ, pid, None);
                                        }
                                    }
                                    crate::sched::process::enter_wait_channel("pipe_read");
                                    crate::sched::process::suspend_current_and_run_next();
                                } else {
                                    inline_wfi_tick();
                                    continue;
                                }
                            }
                            Err(err) => {
                                return crate::syscall::fd::fs_error_to_errno(
                                    crate::net::socket::socket_error_to_fs_error(err),
                                );
                            }
                        }
                    }
                }
            }

            // Stack buffer for small reads avoids heap allocation on the
            // hot path.  After many test-groups the linked-list allocator's
            // free list grows long, making alloc/dealloc O(n) and
            // inflating lmbench read/write latency by 100-400×.
            const STACK_THRESHOLD: usize = 1024;
            // 未初始化栈缓冲：小读快路径只用到由 `f.read` 填入的 [..n] 前缀，
            // 每次系统调用零初始化整 1 KiB 是纯浪费（memset 曾主导 pipe/IO 热路径）。
            let mut stack_storage: core::mem::MaybeUninit<[u8; STACK_THRESHOLD]> =
                core::mem::MaybeUninit::uninit();

            if buf_len <= STACK_THRESHOLD {
                // SAFETY: 仅在 `f.read` 写入后访问 [..n]；u8 无非法位型，
                // 以 buf_len(≤STACK_THRESHOLD) 构造切片不越界。
                let kernel_buf = unsafe {
                    core::slice::from_raw_parts_mut(
                        stack_storage.as_mut_ptr() as *mut u8,
                        buf_len,
                    )
                };
                let mut alone_retries = 0usize;
                loop {
                    match f.read(kernel_buf) {
                        Ok(n) => {
                            if !noatime {
                                if let Some(ref p) = f.path {
                                    crate::fs::touch_atime(&f.file, p);
                                }
                            }
                            if n > 0 {
                                crate::syscall::fd::notify_fanotify_access(&f);
                            }
                            if write_user_buffer(buf, &kernel_buf[..n]).is_ok() {
                                crate::syscall::fd::wake_non_stream_io_waiters(&f);
                                let is_storage = matches!(
                                    ftype,
                                    FileType::Regular | FileType::BlockDevice
                                );
                                crate::sched::process::account_io_read(n, is_storage);
                                return n as isize;
                            } else {
                                return -14;
                            }
                        }
                        Err(crate::fs::FsError::WouldBlock) => {
                            const O_NONBLOCK: u32 = 0x800;
                            if f.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_NONBLOCK
                                != 0
                            {
                                return -11;
                            }
                            let is_stream_fd =
                                matches!(ftype, FileType::Pipe | FileType::Socket);
                            if has_other_runnable() {
                                // 管道 read() 返回 WouldBlock 前已把当前 pid 登记进
                                // read_waiters（见 PipeReadEnd::read），无需二次登记。
                                if ftype != FileType::Pipe {
                                    let pid = crate::sched::process::HART_CURRENT_PID
                                        .load(core::sync::atomic::Ordering::Relaxed);
                                    if pid != 0 {
                                        if let Some(file) = f.file.as_file() {
                                            file.register_waiter(READINESS_READ, pid, None);
                                        }
                                    }
                                }
                                drop(f);
                                crate::sched::process::enter_wait_channel("pipe_read");
                                crate::sched::process::suspend_current_and_run_next();
                            } else {
                                alone_retries += 1;
                                let is_timerfd =
                                    f.file.as_file().map_or(false, |file| file.is_timerfd());
                                if !is_stream_fd && !is_timerfd && alone_retries > 500 {
                                    return 0;
                                }
                                inline_wfi_tick();
                                continue;
                            }
                        }
                        Err(err) => {
                            return crate::syscall::fd::fs_error_to_errno(err);
                        }
                    }
                }
            } else if buf_len <= STATIC_READ_BUF_SIZE {
                let mut alone_retries = 0usize;
                loop {
                    let read_result = {
                        let mut static_buf = STATIC_READ_BUF.lock();
                        let slice = &mut static_buf[..buf_len];
                        match f.read(slice) {
                            Ok(n) => {
                                if crate::cmdline::wrdbg_enabled() && fd >= 3 {
                                    let cur_pid = crate::sched::process::PROCESS_TABLE
                                        .lock()
                                        .as_ref()
                                        .and_then(|t| t.current)
                                        .unwrap_or(0);
                                    crate::println!(
                                        "[wrdbg_read] pid={} fd={} path={:?} req={} n={} head={:02x?}",
                                        cur_pid,
                                        fd,
                                        f.path,
                                        buf_len,
                                        n,
                                        &slice[..n.min(16)],
                                    );
                                }
                                let write_ok = write_user_buffer(buf, &slice[..n]).is_ok();
                                Ok((n, write_ok))
                            }
                            Err(e) => Err(e),
                        }
                    };
                    match read_result {
                        Ok((n, write_ok)) => {
                            if !noatime {
                                if let Some(ref p) = f.path {
                                    crate::fs::touch_atime(&f.file, p);
                                }
                            }
                            if n > 0 {
                                crate::syscall::fd::notify_fanotify_access(&f);
                            }
                            if write_ok {
                                crate::syscall::fd::wake_non_stream_io_waiters(&f);
                                let is_storage = matches!(
                                    ftype,
                                    FileType::Regular | FileType::BlockDevice
                                );
                                crate::sched::process::account_io_read(n, is_storage);
                                return n as isize;
                            } else {
                                return -14;
                            }
                        }
                        Err(crate::fs::FsError::WouldBlock) => {
                            const O_NONBLOCK: u32 = 0x800;
                            if f.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_NONBLOCK
                                != 0
                            {
                                return -11;
                            }
                            let is_stream_fd =
                                matches!(ftype, FileType::Pipe | FileType::Socket);
                            let is_timerfd =
                                f.file.as_file().map_or(false, |file| file.is_timerfd());
                            if has_other_runnable() {
                                let pid = crate::sched::process::HART_CURRENT_PID
                                    .load(core::sync::atomic::Ordering::Relaxed);
                                if pid != 0 {
                                    if let Some(file) = f.file.as_file() {
                                        file.register_waiter(READINESS_READ, pid, None);
                                    }
                                }
                                drop(f);
                                crate::sched::process::enter_wait_channel("pipe_read");
                                crate::sched::process::suspend_current_and_run_next();
                            } else {
                                alone_retries += 1;
                                if !is_stream_fd && !is_timerfd && alone_retries > 500 {
                                    return 0;
                                }
                                inline_wfi_tick();
                                continue;
                            }
                        }
                        Err(err) => {
                            return crate::syscall::fd::fs_error_to_errno(err);
                        }
                    }
                }
            } else {
                let mut kernel_buf = alloc::vec![0u8; buf_len];
                let mut alone_retries = 0usize;
                loop {
                    match f.read(&mut kernel_buf) {
                        Ok(n) => {
                            if !noatime {
                                if let Some(ref p) = f.path {
                                    crate::fs::touch_atime(&f.file, p);
                                }
                            }
                            if n > 0 {
                                crate::syscall::fd::notify_fanotify_access(&f);
                            }
                            if write_user_buffer(buf, &kernel_buf[..n]).is_ok() {
                                crate::syscall::fd::wake_non_stream_io_waiters(&f);
                                let is_storage = matches!(
                                    ftype,
                                    FileType::Regular | FileType::BlockDevice
                                );
                                crate::sched::process::account_io_read(n, is_storage);
                                return n as isize;
                            } else {
                                return -14;
                            }
                        }
                        Err(crate::fs::FsError::WouldBlock) => {
                            const O_NONBLOCK: u32 = 0x800;
                            if f.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_NONBLOCK
                                != 0
                            {
                                drop(kernel_buf);
                                return -11;
                            }
                            let is_stream_fd =
                                matches!(ftype, FileType::Pipe | FileType::Socket);
                            let is_timerfd =
                                f.file.as_file().map_or(false, |file| file.is_timerfd());
                            if has_other_runnable() {
                                let pid = crate::sched::process::HART_CURRENT_PID
                                    .load(core::sync::atomic::Ordering::Relaxed);
                                if pid != 0 {
                                    if let Some(file) = f.file.as_file() {
                                        file.register_waiter(READINESS_READ, pid, None);
                                    }
                                }
                                drop(kernel_buf);
                                drop(f);
                                crate::sched::process::enter_wait_channel("pipe_read");
                                crate::sched::process::suspend_current_and_run_next();
                            } else {
                                alone_retries += 1;
                                if !is_stream_fd && !is_timerfd && alone_retries > 500 {
                                    return 0;
                                }
                                inline_wfi_tick();
                                continue;
                            }
                        }
                        Err(err) => {
                            return crate::syscall::fd::fs_error_to_errno(err);
                        }
                    }
                }
            }
        }
        None => {
            -9
        }
    }
}
