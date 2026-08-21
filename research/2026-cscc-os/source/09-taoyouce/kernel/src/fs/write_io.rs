//! Write-side file I/O syscall helpers.

use crate::fs::FileType;
use crate::syscall::user::{read_user_buffer, read_user_to_slice};
use crate::syscall::{has_other_runnable, inline_wfi_tick, SyscallError};

/// 写文件
#[allow(dead_code)]
pub(crate) fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    // 写端直递重放入口：本进程在该 pipe 上有未完成直递（被 deadline/信号唤醒
    // 重放）→ 消费登记、按「阻塞前已写 + 读者已代写」总字节数返回（读者已代写
    // 部分已入环被消费，绝不能由写者再写一遍——否则重复）。
    if let Some(f) = crate::syscall::fd::get_fd_obj(fd) {
        if f.ftype == FileType::Pipe || f.file.is_unix_socketpair() {
            let pid = crate::sched::process::HART_CURRENT_PID
                .load(core::sync::atomic::Ordering::Relaxed);
            if pid != 0 {
                if let Some(written) = f
                    .file
                    .as_file()
                    .and_then(|file| file.take_write_handoff(pid))
                {
                    if crate::cmdline::wrdbg_enabled() {
                        static B: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(64);
                        if B.fetch_update(core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed, |n| if n==0 {None} else {Some(n-1)}).is_ok() {
                            crate::println!("[WRHDBG] write-replay fd={} pid={} -> {}", fd, pid, written);
                        }
                    }
                    crate::sched::process::account_io_write(written, false);
                    return written as isize;
                }
            }
        }
    }

    // Regular files must transfer the *entire* user buffer in a single
    // write(2): POSIX/Linux only short-write a regular file on signal,
    // ENOSPC, or RLIMIT_FSIZE. The kernel-internal staging buffer is bounded
    // to 1 MiB, so loop the request in <=1 MiB chunks and accumulate the full
    // count. Pipes/sockets/char devices fall through to the single-shot path
    // below, where a short write is legitimate POSIX behavior.
    const CHUNK: usize = 1024 * 1024;
    if len > CHUNK {
        if let Some(f) = crate::syscall::fd::get_fd_obj(fd) {
            if f.ftype == FileType::Regular {
                if !f.writable() {
                    return -9;
                }
                const O_APPEND: u32 = 0x400;
                let start_pos = if f.status_flags.load(core::sync::atomic::Ordering::Relaxed)
                    & O_APPEND
                    != 0
                {
                    f.file.stat().map(|s| s.size).unwrap_or(0)
                } else {
                    f.offset.load(core::sync::atomic::Ordering::Relaxed)
                };
                let total_allowed = match crate::fs::file_limits::rlimit_fsize_clamp(start_pos, len)
                {
                    Ok(a) => a,
                    Err(e) => return e,
                };
                let mut total = 0usize;
                while total < total_allowed {
                    let chunk = (total_allowed - total).min(CHUNK);
                    let src = unsafe { buf.add(total) };
                    let data = match read_user_buffer(src, chunk) {
                        Ok(d) => d,
                        Err(_) => {
                            if total > 0 {
                                break;
                            }
                            return -14;
                        }
                    };
                    match f.write(&data) {
                        Ok(n) => {
                            total += n;
                            if n < chunk {
                                break;
                            }
                        }
                        Err(crate::fs::FsError::InvalidArgument) => {
                            if total > 0 {
                                break;
                            }
                            return -22;
                        }
                        Err(crate::fs::FsError::AccessDenied) => {
                            if total > 0 {
                                break;
                            }
                            return -13;
                        }
                        Err(crate::fs::FsError::PermissionDenied)
                        | Err(crate::fs::FsError::OperationNotPermitted) => {
                            if total > 0 {
                                break;
                            }
                            return -1;
                        }
                        Err(_) => {
                            if total > 0 {
                                break;
                            }
                            return -5;
                        }
                    }
                }
                if total > 0 {
                    crate::syscall::fd::notify_inotify_modify(&f);
                }
                crate::syscall::fd::wake_non_stream_io_waiters(&f);
                crate::sched::process::account_io_write(total, true);
                return total as isize;
            }
        }
    }

    // Socket 大块写快路径：staging Vec 所有权直接 move 进对端接收队列
    // （sendto_owned），跳过 File::write → backend::sendto 的第二次全量
    // to_vec 拷贝+堆分配。iperf/netperf TCP 每 write 128KB，省一半 memcpy。
    const SOCK_OWNED_MIN: usize = 4096;
    if len >= SOCK_OWNED_MIN {
        if let Some(f) = crate::syscall::fd::get_fd_obj(fd) {
            if f.ftype == FileType::Socket {
                if !f.writable() {
                    return -9;
                }
                if let Ok(sock) = crate::net::socket::socket_from_fd(fd as i32) {
                    let write_len = len.min(1024 * 1024);
                    // 直接 read_user_buffer 拿 Vec（单次 user→kernel 拷贝）；
                    // read_user_to_slice 对 >4 页请求会退化成 Vec 中转+二次拷贝。
                    let v = match read_user_buffer(buf, write_len) {
                        Ok(v) => v,
                        Err(_) => return -14,
                    };
                    const O_NONBLOCK: u32 = 0o4000;
                    let nonblock = f.status_flags.load(core::sync::atomic::Ordering::Relaxed)
                        & O_NONBLOCK
                        != 0;
                    let mut payload = v;
                    loop {
                        match crate::net::socket::sendto_sock_owned(&sock, payload, None) {
                            Ok(n) => return n as isize,
                            Err((crate::net::socket::SocketError::WouldBlock, buf_back)) => {
                                payload = buf_back;
                                if nonblock {
                                    return -11;
                                }
                                let pid = crate::sched::process::HART_CURRENT_PID
                                    .load(core::sync::atomic::Ordering::Relaxed);
                                if pid != 0 {
                                    crate::net::socket::register_write_waiter_sock_after_wouldblock(
                                        &sock, pid, None,
                                    );
                                }
                                match crate::net::socket::sendto_sock_owned(&sock, payload, None) {
                                    Ok(n) => return n as isize,
                                    Err((crate::net::socket::SocketError::WouldBlock, buf_back)) => {
                                        payload = buf_back
                                    }
                                    Err((err, _)) => {
                                        return crate::syscall::socket::socket_errno(err)
                                    }
                                }
                                crate::sched::process::enter_wait_channel("tcp_sendmsg");
                                crate::sched::process::suspend_current_and_run_next();
                            }
                            Err((err, _)) => return crate::syscall::socket::socket_errno(err),
                        }
                    }
                }
            }
        }
    }

    // Regular 文件中等大小写（1 KiB..1 MiB）零拷贝快路径：不再把用户缓冲
    // staging 到内核堆 Vec（一次 heap alloc + 一次全量 memcpy）再交给
    // File::write 拷进文件页缓存，而是把用户页直接解析成内核直通只读段，
    // 逐段 write 进文件（单次拷贝）。lat_fs 1k/4k/10k、iozone 顺序写热路径
    // 的 memcpy 总量减半。ramfs/ext4 write 均为纯内存拷贝、不阻塞不调度，
    // 段指针在本次系统调用内保持有效。失败（未映射等）回退 staging 慢路径。
    const ZC_MIN: usize = 513;
    const ZC_MAX: usize = 1024 * 1024;
    if (ZC_MIN..=ZC_MAX).contains(&len) {
        if let Some(f) = crate::syscall::fd::get_fd_obj(fd) {
            if f.ftype == FileType::Regular {
                if !f.writable() {
                    return -9;
                }
                if let Ok(segs) = crate::syscall::user::user_readable_segments(buf, len) {
                    const O_APPEND: u32 = 0x400;
                    let pos = if f.status_flags.load(core::sync::atomic::Ordering::Relaxed)
                        & O_APPEND
                        != 0
                    {
                        f.file.stat().map(|s| s.size).unwrap_or(0)
                    } else {
                        f.offset.load(core::sync::atomic::Ordering::Relaxed)
                    };
                    let allowed = match crate::fs::file_limits::rlimit_fsize_clamp(pos, len) {
                        Ok(a) => a,
                        Err(e) => return e,
                    };
                    let mut total = 0usize;
                    let mut err: Option<crate::fs::FsError> = None;
                    'seg: for (ptr, seg_len) in segs {
                        if total >= allowed {
                            break;
                        }
                        let take = seg_len.min(allowed - total);
                        let slice = unsafe { core::slice::from_raw_parts(ptr, take) };
                        match f.write(slice) {
                            Ok(n) => {
                                total += n;
                                if n < take {
                                    break 'seg;
                                }
                            }
                            Err(e) => {
                                err = Some(e);
                                break 'seg;
                            }
                        }
                    }
                    if total == 0 {
                        if let Some(e) = err {
                            return match e {
                                crate::fs::FsError::InvalidArgument => -22,
                                crate::fs::FsError::OutOfRange => -34,
                                crate::fs::FsError::AccessDenied => -13,
                                crate::fs::FsError::PermissionDenied
                                | crate::fs::FsError::OperationNotPermitted => -1,
                                crate::fs::FsError::NoSpace => -28,
                                _ => -5,
                            };
                        }
                    }
                    if total > 0 {
                        crate::syscall::fd::notify_inotify_modify(&f);
                    }
                    crate::syscall::fd::wake_non_stream_io_waiters(&f);
                    crate::sched::process::account_io_write(total, true);
                    return total as isize;
                }
                // segments 解析失败（如缓冲区部分未映射）→ 回退 staging 慢路径，
                // 保证 EFAULT 语义与原路径一致。
            }
        }
    }

    // Pipe/socketpair 大块写（>PIPE_BUF，Linux 本就不保证原子性）零拷贝快
    // 路径：用户页解析成内核直通只读段后逐段写进环形缓冲（单次拷贝），
    // 跳过 staging Vec 的堆分配 + 全量 memcpy。环满未写入任何字节时回退
    // staging 慢路径走原有阻塞逻辑（suspend 会重启系统调用，仅在零进度
    // 时安全）；有进度后遇环满则回退通用路径走写端直递（见下）。
    let mut pipe_fast_partial = 0usize;
    if len > 4096 {
        if let Some(f) = crate::syscall::fd::get_fd_obj(fd) {
            if f.ftype == FileType::Pipe || f.file.is_unix_socketpair() {
                if !f.writable() {
                    return -9;
                }
                if let Ok(segs) = crate::syscall::user::user_readable_segments(buf, len) {
                    let mut total = 0usize;
                    let mut fell_back = false;
                    'pipe_seg: for (ptr, seg_len) in segs {
                        let slice = unsafe { core::slice::from_raw_parts(ptr, seg_len) };
                        match f.write(slice) {
                            Ok(n) => {
                                total += n;
                                if n < seg_len {
                                    // 环满且有进度：回退通用路径做写端直递
                                    // （写者不回放、读者代写剩余数据）。
                                    fell_back = true;
                                    break 'pipe_seg;
                                }
                            }
                            Err(crate::fs::FsError::WouldBlock) => {
                                if total == 0 {
                                    // 零进度阻塞：交还通用路径处理（阻塞/重启安全）。
                                    fell_back = true;
                                }
                                break 'pipe_seg;
                            }
                            Err(crate::fs::FsError::BrokenPipe) => {
                                if total > 0 {
                                    break 'pipe_seg;
                                }
                                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                                if let Some(ref mut t) = *table {
                                    if let Some(pid) = t.current {
                                        if let Some(p) = t.get_mut(pid) {
                                            let mask = 1u64 << (13 - 1);
                                            if p.signal_mask & mask == 0 {
                                                p.pending_signals |= mask;
                                                crate::sched::process::note_signal_hint();
                                            }
                                        }
                                    }
                                }
                                return -32;
                            }
                            Err(crate::fs::FsError::InvalidArgument) => return -22,
                            Err(_) => {
                                if total > 0 {
                                    break 'pipe_seg;
                                }
                                return -5;
                            }
                        }
                    }
                    if !fell_back {
                        if total > 0 {
                            crate::syscall::fd::wake_non_stream_io_waiters(&f);
                        }
                        crate::sched::process::account_io_write(total, false);
                        return total as isize;
                    }
                    pipe_fast_partial = total;
                }
                // segments 解析失败（部分未映射）或零进度阻塞 → staging 慢路径。
            }
        }
    }

    // Stack buffer for small writes avoids heap allocation on the hot
    // path (same rationale as sys_read — linked-list allocator O(n)
    // fragmentation after many test-groups).
    const STACK_THRESHOLD: usize = 1024;
    // 未初始化栈缓冲：小写快路径只用到由 `read_user_to_slice` 填入的 [..write_len]
    // 前缀，每次系统调用零初始化整 1 KiB 是纯浪费（memset 曾主导 pipe/IO 热路径）。
    let mut stack_storage: core::mem::MaybeUninit<[u8; STACK_THRESHOLD]> =
        core::mem::MaybeUninit::uninit();
    let write_len = len.min(1024 * 1024);

    let data: alloc::vec::Vec<u8>;
    let data_slice: &[u8];
    if write_len <= STACK_THRESHOLD {
        // SAFETY: 先由 read_user_to_slice 填满 [..write_len] 再以同范围读取；
        // u8 无非法位型，write_len(≤STACK_THRESHOLD) 构造切片不越界。
        let stack_buf = unsafe {
            core::slice::from_raw_parts_mut(
                stack_storage.as_mut_ptr() as *mut u8,
                write_len,
            )
        };
        match read_user_to_slice(buf, stack_buf) {
            Ok(_n) => {
                data_slice = unsafe {
                    core::slice::from_raw_parts(
                        stack_storage.as_ptr() as *const u8,
                        write_len,
                    )
                };
            }
            Err(_) => return -14,
        }
        data = alloc::vec::Vec::new(); // unused, zero-cost empty Vec
    } else {
        match read_user_buffer(buf, write_len) {
            Ok(d) => { data = d; data_slice = &data; }
            Err(_) => return -14,
        }
    }

    write_debug_trace(fd, data_slice);

    let fd_obj = crate::syscall::fd::get_fd_obj(fd);
    match fd_obj {
        Some(f) => {
            if !f.writable() {
                return -9;
            }
            let mut effective_data = data_slice;
            let mut truncated_vec: alloc::vec::Vec<u8>;
            if f.ftype == FileType::Regular {
                const O_APPEND: u32 = 0x400;
                let pos =
                    if f.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_APPEND != 0 {
                        f.file.stat().map(|s| s.size).unwrap_or(0)
                    } else {
                        f.offset.load(core::sync::atomic::Ordering::Relaxed)
                    };
                match crate::fs::file_limits::rlimit_fsize_clamp(pos, effective_data.len()) {
                    Ok(allowed) => {
                        if allowed < effective_data.len() {
                            truncated_vec = effective_data[..allowed].to_vec();
                            effective_data = &truncated_vec;
                        }
                    }
                    Err(e) => return e,
                }
            }
            let mut alone_retries = 0usize;
            // 写端直递累进计数：pipe 已由快路径写入的字节从 written 起续写，
            // 其余 fd 类型保持单次写语义（POSIX 允许短写）。
            let mut written = pipe_fast_partial;
            loop {
                if written >= effective_data.len() {
                    if written > 0 {
                        crate::syscall::fd::notify_inotify_modify(&f);
                    }
                    crate::syscall::fd::wake_non_stream_io_waiters(&f);
                    crate::sched::process::account_io_write(written, false);
                    return written as isize;
                }
                let is_pipe = f.ftype == FileType::Pipe || f.file.is_unix_socketpair();
                match f.write(&effective_data[written..]) {
                    Ok(n) => {
                        written += n;
                        if !is_pipe {
                            if n > 0 {
                                crate::syscall::fd::notify_inotify_modify(&f);
                            }
                            crate::syscall::fd::wake_non_stream_io_waiters(&f);
                            let is_storage = matches!(
                                f.ftype,
                                FileType::Regular | FileType::BlockDevice
                            );
                            crate::sched::process::account_io_write(n, is_storage);
                            return n as isize;
                        }
                        if written >= effective_data.len() {
                            crate::syscall::fd::notify_inotify_modify(&f);
                            crate::syscall::fd::wake_non_stream_io_waiters(&f);
                            crate::sched::process::account_io_write(written, false);
                            return written as isize;
                        }
                        // 环满部分写入：下一轮 write 即 WouldBlock → 直递分支。
                    }
                    Err(crate::fs::FsError::WouldBlock) => {
                        const O_NONBLOCK: u32 = 0o4000;
                        if f.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_NONBLOCK
                            != 0
                        {
                            return if written > 0 { written as isize } else { -11 };
                        }
                        if written > 0 {
                            if crate::cmdline::pipe_handoff_enabled() {
                                // 写端直递：登记剩余工作，读者消费后替本写者续写完成
                                // （Linux pipe_write 阻塞语义：写者不回放、不重复）。
                                let registered = f
                                    .file
                                    .as_file()
                                    .map_or(false, |file| {
                                        file.register_write_handoff(
                                            effective_data[written..].to_vec(),
                                            written,
                                        )
                                    });
                                if registered {
                                    drop(f);
                                    drop(data);
                                    crate::sched::process::enter_wait_channel("pipe_write");
                                    crate::sched::process::suspend_current_for_io(
                                        crate::arch::asm_helpers::rdtime()
                                            .saturating_add(10_000_000),
                                    );
                                }
                            }
                            // 有进度时必须短写返回：suspend_current_for_io 唤醒后会
                            // 重放整个系统调用，带着 written>0 进度去阻塞会把已写入
                            // 的字节整段重写（管道对端收到重复数据，cargo metadata
                            // 的 JSON 流即因此损坏）。调用方 write_all 会续写余量。
                            crate::syscall::fd::notify_inotify_modify(&f);
                            crate::syscall::fd::wake_non_stream_io_waiters(&f);
                            crate::sched::process::account_io_write(written, false);
                            return written as isize;
                        }
                        if has_other_runnable() {
                            if f.file.as_file().map_or(false, |file| file.poll_write()) {
                                continue;
                            }
                            // suspend_current_for_io 发散（-> !）并在唤醒后重启整个
                            // 系统调用，本栈帧被直接丢弃、Drop 不会运行。若不在此显式
                            // 释放，写入大缓冲区（>1024B）时阻塞在满管道上会泄漏 `data`
                            // 这块 64KB 堆内存（lmbench bw_pipe 累积上万次 → 堆 OOM）。
                            // 与 sys_read 阻塞前 drop(kernel_buf) 对称。
                            drop(f);
                            drop(data);
                            crate::sched::process::enter_wait_channel("pipe_write");
                            crate::sched::process::suspend_current_for_io(
                                crate::arch::asm_helpers::rdtime().saturating_add(10_000_000),
                            );
                        } else {
                            alone_retries += 1;
                            if alone_retries > 500 {
                                return -4;
                            }
                            inline_wfi_tick();
                            continue;
                        }
                    }
                    Err(crate::fs::FsError::BrokenPipe) => {
                        let mut table = crate::sched::process::PROCESS_TABLE.lock();
                        if let Some(ref mut t) = *table {
                            if let Some(pid) = t.current {
                                if let Some(p) = t.get_mut(pid) {
                                    let mask = 1u64 << (13 - 1);
                                    if p.signal_mask & mask == 0 {
                                        p.pending_signals |= mask; crate::sched::process::note_signal_hint();
                                    }
                                }
                            }
                        }
                        return -32;
                    }
                    Err(crate::fs::FsError::InvalidArgument) => return -22,
                    Err(crate::fs::FsError::OutOfRange) => return -34,
                    Err(crate::fs::FsError::AccessDenied) => return -13,
                    Err(crate::fs::FsError::PermissionDenied)
                    | Err(crate::fs::FsError::OperationNotPermitted) => return -1,
                    Err(_e) => return -5,
                }
            }
        }
        None => -9,
    }
}

/// writev — 写入多个缓冲区（musl 的 printf 最终走这条路）
#[allow(dead_code)]
pub(crate) fn sys_writev(fd: usize, iov_ptr: usize, iovcnt: usize) -> isize {
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

    writev_debug_trace(fd, iovcnt, &iov_data);

    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    if !fd_obj.writable() {
        return -9;
    }

    let is_storage = matches!(
        fd_obj.ftype,
        FileType::Regular | FileType::BlockDevice
    );
    let finish = |total: usize| -> isize {
        crate::sched::process::account_io_write(total, is_storage);
        total as isize
    };

    // 管道写端直递重放入口（与 sys_write 对称）：deadline/信号唤醒重放时消费
    // 登记、按「阻塞前已写 + 读者已代写」返回（不重写不重复）。
    if fd_obj.ftype == FileType::Pipe || fd_obj.file.is_unix_socketpair() {
        let pid = crate::sched::process::HART_CURRENT_PID
            .load(core::sync::atomic::Ordering::Relaxed);
        if pid != 0 {
            if let Some(written) = fd_obj
                .file
                .as_file()
                .and_then(|file| file.take_write_handoff(pid))
            {
                if crate::cmdline::wrdbg_enabled() {
                    static B: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(64);
                    if B.fetch_update(core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed, |n| if n==0 {None} else {Some(n-1)}).is_ok() {
                        crate::println!("[WRHDBG] writev-replay fd={} pid={} -> {}", fd, pid, written);
                    }
                }
                return finish(written);
            }
        }
    }

    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_data[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_data[off + 8..off + 16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        if len > 1024 * 1024 {
            return -22;
        }
        if !crate::syscall::user::is_user_addr_valid(base, len) {
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
        if len > 1024 * 1024 {
            return -22;
        }

        // Regular 文件段直写（同 sys_write 零拷贝快路径）：musl fwrite 大块
        // 走 writev(iov[stdio_buf, data])，通用路径每段 staging Vec 全量
        // memcpy 后再拷进页缓存。用户页解析成直通只读段后单次拷贝。
        // ramfs/ext4 write 纯内存拷贝不阻塞，段指针在本 syscall 内有效。
        if len >= 513 && fd_obj.ftype == FileType::Regular {
            if let Ok(segs) = crate::syscall::user::user_readable_segments(base as *const u8, len)
            {
                let mut seg_total = 0usize;
                let mut seg_err: Option<crate::fs::FsError> = None;
                for (ptr, seg_len) in segs {
                    let slice = unsafe { core::slice::from_raw_parts(ptr, seg_len) };
                    match fd_obj.write(slice) {
                        Ok(n) => {
                            seg_total += n;
                            if n < seg_len {
                                break;
                            }
                        }
                        Err(e) => {
                            seg_err = Some(e);
                            break;
                        }
                    }
                }
                if seg_total > 0 {
                    crate::syscall::fd::notify_inotify_modify(&fd_obj);
                    crate::syscall::fd::wake_non_stream_io_waiters(&fd_obj);
                    total += seg_total;
                }
                if let Some(e) = seg_err {
                    if total > 0 {
                        return finish(total);
                    }
                    return match e {
                        crate::fs::FsError::InvalidArgument => -22,
                        crate::fs::FsError::OutOfRange => -34,
                        crate::fs::FsError::AccessDenied => -13,
                        crate::fs::FsError::PermissionDenied
                        | crate::fs::FsError::OperationNotPermitted => -1,
                        crate::fs::FsError::NoSpace => -28,
                        _ => -5,
                    };
                }
                continue;
            }
        }

        match read_user_buffer(base as *const u8, len) {
            Ok(buf) => {
                let mut alone_retries = 0usize;
                loop {
                    match fd_obj.write(&buf) {
                        Ok(n) => {
                            if n > 0 {
                                crate::syscall::fd::notify_inotify_modify(&fd_obj);
                            }
                            crate::syscall::fd::wake_non_stream_io_waiters(&fd_obj);
                            total += n;
                            let is_pipe = fd_obj.ftype == FileType::Pipe
                                || fd_obj.file.is_unix_socketpair();
                            if is_pipe && n < buf.len() {
                                if crate::cmdline::pipe_handoff_enabled() {
                                    // 环满部分写入：stage 剩余（本 iov 余部 + 后续
                                    // iovs）→ 写端直递（读者代写完成，a0=全部字节，
                                    // 防旧重放整段重写的重复写入）。
                                    if pipe_writev_stage_and_register(
                                        &fd_obj, &buf, n, &iov_data, i, total,
                                    ) {
                                        drop(buf);
                                        drop(iov_data);
                                        drop(fd_obj);
                                        crate::sched::process::enter_wait_channel("pipe_write");
                                        crate::sched::process::suspend_current_for_io(
                                            crate::arch::asm_helpers::rdtime()
                                                .saturating_add(10_000_000),
                                        );
                                    }
                                }
                                // 部分写必须立即短写返回（不能 break 去写下一个
                                // iov：会把本 iov 尾部漏掉造成字节流乱序/丢失）。
                                return finish(total);
                            }
                            break;
                        }
                        Err(crate::fs::FsError::WouldBlock) => {
                            const O_NONBLOCK: u32 = 0o4000;
                            if fd_obj
                                .status_flags
                                .load(core::sync::atomic::Ordering::Relaxed)
                                & O_NONBLOCK
                                != 0
                            {
                                return if total > 0 { finish(total) } else { -11 };
                            }
                            let is_pipe = fd_obj.ftype == FileType::Pipe
                                || fd_obj.file.is_unix_socketpair();
                            if is_pipe && total > 0 {
                                if crate::cmdline::pipe_handoff_enabled() {
                                    // 前序 iov 已有进度：stage 剩余 → 直递（防旧
                                    // 重放把前序 iov 整段重写的重复写入）。
                                    if pipe_writev_stage_and_register(
                                        &fd_obj, &buf, 0, &iov_data, i, total,
                                    ) {
                                        drop(buf);
                                        drop(iov_data);
                                        drop(fd_obj);
                                        crate::sched::process::enter_wait_channel("pipe_write");
                                        crate::sched::process::suspend_current_for_io(
                                            crate::arch::asm_helpers::rdtime()
                                                .saturating_add(10_000_000),
                                        );
                                    }
                                }
                                // 有进度时短写返回：suspend 唤醒会重放整个 writev，
                                // 把前序 iov 整段重写（管道对端收到重复数据）。
                                return finish(total);
                            }
                            if has_other_runnable() {
                                if fd_obj
                                    .file
                                    .as_file()
                                    .map_or(false, |file| file.poll_write())
                                {
                                    continue;
                                }
                                drop(buf);
                                drop(iov_data);
                                drop(fd_obj);
                                crate::sched::process::enter_wait_channel("pipe_write");
                                crate::sched::process::suspend_current_for_io(
                                    crate::arch::asm_helpers::rdtime().saturating_add(10_000_000),
                                );
                            } else {
                                alone_retries += 1;
                                if alone_retries > 500 {
                                    return if total > 0 { finish(total) } else { -4 };
                                }
                                inline_wfi_tick();
                                continue;
                            }
                        }
                        Err(crate::fs::FsError::BrokenPipe) => {
                            let mut table = crate::sched::process::PROCESS_TABLE.lock();
                            if let Some(ref mut t) = *table {
                                if let Some(pid) = t.current {
                                    if let Some(p) = t.get_mut(pid) {
                                        let mask = 1u64 << (13 - 1);
                                        if p.signal_mask & mask == 0 {
                                            p.pending_signals |= mask; crate::sched::process::note_signal_hint();
                                        }
                                    }
                                }
                            }
                            return if total > 0 { finish(total) } else { -32 };
                        }
                        Err(err) => return crate::syscall::fd::fs_error_to_errno(err),
                    }
                }
            }
            // Linux ≥4.8 语义（writev07）：iovec 中途 EFAULT 时若已写入若干
            // 字节则返回已写字节数（短写），仅在一个字节都没写时返回 EFAULT。
            Err(_) => return if total > 0 { finish(total) } else { -14 },
        }
    }
    finish(total)
}

/// writev 管道阻塞点：把「当前 iov 余部 + 后续 iov」stage 成单一内核缓冲并
/// 登记写端直递（a0_base = 已写入字节数）；读者代写完成后 a0 = a0_base + 全部
/// 余量。超过 1MiB 的余量截断（与 write(2) staging 上限一致）；后续 iov 坏地址
/// 时只直递已 stage 部分（完成后 a0 即部分字节，对齐 Linux 部分写 + EFAULT
/// 语义）。返回是否登记成功（staging 所有权随成功移交 pipe）。
fn pipe_writev_stage_and_register(
    fd_obj: &alloc::sync::Arc<crate::fs::FileDescriptor>,
    cur: &[u8],
    n: usize,
    iov_data: &[u8],
    i: usize,
    a0_base: usize,
) -> bool {
    const CAP: usize = 1024 * 1024;
    let mut staged: alloc::vec::Vec<u8> = alloc::vec::Vec::with_capacity((cur.len() - n).min(CAP));
    staged.extend_from_slice(&cur[n..]);
    let mut idx = i + 1;
    while staged.len() < CAP && idx * 16 + 16 <= iov_data.len() {
        let off = idx * 16;
        let base = usize::from_le_bytes(iov_data[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_data[off + 8..off + 16].try_into().unwrap());
        idx += 1;
        if len == 0 {
            continue;
        }
        let take = (CAP - staged.len()).min(len);
        match read_user_buffer(base as *const u8, take) {
            Ok(d) => {
                let dlen = d.len();
                staged.extend_from_slice(&d);
                if dlen < take {
                    break; // 坏 iov 余部：只直递已 stage 部分
                }
            }
            Err(_) => break, // 坏 iov：只直递已 stage 部分
        }
    }
    fd_obj
        .file
        .as_file()
        .map_or(false, |file| file.register_write_handoff(staged, a0_base))
}

fn write_debug_trace(fd: usize, data: &[u8]) {
    if !crate::cmdline::wrdbg_enabled() || !(fd == 1 || fd == 2) || data.is_empty() {
        return;
    }
    let cur_pid = crate::sched::process::PROCESS_TABLE
        .lock()
        .as_ref()
        .and_then(|t| t.current)
        .unwrap_or(0);
    let mut s = alloc::string::String::new();
    for &b in &data[..core::cmp::min(data.len(), 120)] {
        if b == b'\n' {
            s.push_str("\\n");
        } else if (32..127).contains(&b) {
            s.push(b as char);
        } else {
            use core::fmt::Write;
            let _ = write!(&mut s, "\\x{:02x}", b);
        }
    }
    crate::println!(
        "[wrdbg_write] pid={} fd={} len={} text=\"{}\"",
        cur_pid,
        fd,
        data.len(),
        s,
    );
}

fn writev_debug_trace(fd: usize, iovcnt: usize, iov_data: &[u8]) {
    if !crate::cmdline::wrdbg_enabled()
        || !(fd == 1 || fd == 2)
    {
        return;
    }

    let cur_pid = crate::sched::process::PROCESS_TABLE
        .lock()
        .as_ref()
        .and_then(|t| t.current)
        .unwrap_or(0);
    let mut stack_smash_seen = false;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_data[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_data[off + 8..off + 16].try_into().unwrap());
        let preview_len = core::cmp::min(len, 120);
        let preview = if preview_len > 0 {
            read_user_buffer(base as *const u8, preview_len).unwrap_or_default()
        } else {
            alloc::vec::Vec::new()
        };
        let mut s = alloc::string::String::new();
        for &b in &preview {
            if b == b'\n' {
                s.push_str("\\n");
            } else if (32..127).contains(&b) {
                s.push(b as char);
            } else {
                use core::fmt::Write;
                let _ = write!(&mut s, "\\x{:02x}", b);
            }
        }
        crate::println!(
            "[wrdbg] pid={} fd={} iov[{}] base={:#x} len={} text=\"{}\"",
            cur_pid,
            fd,
            i,
            base,
            len,
            s,
        );
        if s.contains("stack smashing detected") {
            stack_smash_seen = true;
        }
    }
    if stack_smash_seen {
        writev_debug_context(cur_pid);
    }
}

fn writev_debug_context(cur_pid: usize) {
    let (sepc, ra, sp, tp, s0, s1, s2, s3, a0, a7, stack_words) = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_ref() {
            if let Some(pid) = t.current {
                if let Some(p) = t.get(pid) {
                    let cx = unsafe { &*(p.trap_context_ptr as *const crate::arch::TrapContext) };
                    let entries: alloc::vec::Vec<(usize, Option<u64>)> =
                        if let Some(ref mm) = p.mm {
                            let addr_space = mm.lock();
                            (0..768usize)
                                .step_by(8)
                                .map(|off| {
                                    let va = crate::mm::VirtAddr(cx.x[2] + off);
                                    let vpn = va.floor();
                                    let val = addr_space.page_table().translate(vpn).map(|ppn| {
                                        let kva = crate::mm::phys_to_kernel_vaddr(
                                            ppn.0 * 4096 + va.page_offset(),
                                        );
                                        unsafe { *(kva as *const u64) }
                                    });
                                    (off, val)
                                })
                                .collect()
                        } else {
                            alloc::vec::Vec::new()
                        };
                    (
                        cx.sepc, cx.x[1], cx.x[2], cx.x[4], cx.x[8], cx.x[9], cx.x[18], cx.x[19],
                        cx.x[10], cx.x[17], entries,
                    )
                } else {
                    (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, alloc::vec::Vec::new())
                }
            } else {
                (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, alloc::vec::Vec::new())
            }
        } else {
            (0, 0, 0, 0, 0, 0, 0, 0, 0, 0, alloc::vec::Vec::new())
        }
    };
    crate::println!(
        "[wrdbg_ctx] pid={} sepc={:#x} ra={:#x} sp={:#x} tp={:#x} s0={:#x} s1={:#x} s2={:#x} s3={:#x} a0={:#x} a7={:#x}",
        cur_pid,
        sepc,
        ra,
        sp,
        tp,
        s0,
        s1,
        s2,
        s3,
        a0,
        a7,
    );
    for (off, val) in stack_words {
        match val {
            Some(v) => crate::println!("[wrdbg_stack]   +{:#x}: {:#018x}", off, v),
            None => crate::println!("[wrdbg_stack]   +{:#x}: <unmapped>", off),
        }
    }
}
