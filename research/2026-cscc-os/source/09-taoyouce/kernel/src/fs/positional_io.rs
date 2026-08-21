//! Positioned file I/O syscall helpers.

use crate::syscall::user::{read_user_buffer, write_user_buffer};
use crate::syscall::SyscallError;

/// pread64 — 在指定偏移读取，不改变 fd 位置
pub(crate) fn sys_pread64(fd: usize, buf: *mut u8, len: usize, offset: usize) -> isize {
    let fd_obj = crate::syscall::fd::get_fd_obj(fd);
    match fd_obj {
        Some(f) => {
            match f.file.file_type() {
                crate::fs::FileType::Directory => return -21,
                crate::fs::FileType::Pipe | crate::fs::FileType::Socket => return -29,
                _ => {}
            }
            if !f.readable() {
                return -9;
            }
            if (offset as i64) < 0 {
                return -22;
            }
            if let Some(errno) = crate::syscall::fd::fanotify_check_read_perm(&f) {
                return errno;
            }
            if let Some(file) = f.file.as_file() {
                // Read in <=1MiB chunks (read_user_buffer/write_user_buffer cap
                // single transfers at 1MiB) so a large pread completes fully
                // like Linux instead of returning a 1MiB short read.
                const CHUNK: usize = 1024 * 1024;
                let mut total: usize = 0;
                let mut notified = false;
                while total < len {
                    let this = (len - total).min(CHUNK);
                    let mut kernel_buf = alloc::vec![0u8; this];
                    match file.read(offset + total, &mut kernel_buf) {
                        Ok(n) => {
                            if n > 0 && !notified {
                                crate::syscall::fd::notify_fanotify_access(&f);
                                notified = true;
                            }
                            if write_user_buffer(unsafe { buf.add(total) }, &kernel_buf[..n]).is_err() {
                                if total > 0 { break; }
                                return -14;
                            }
                            total += n;
                            if n < this { break; } // EOF / short read
                        }
                        Err(err) => {
                            if total > 0 { break; }
                            return crate::syscall::fd::fs_error_to_errno(err);
                        }
                    }
                }
                crate::sched::process::account_io_read(total, true);
                total as isize
            } else {
                -22
            }
        }
        None => -9,
    }
}

/// pwrite64 — 在指定偏移写入，不改变 fd 位置
pub(crate) fn sys_pwrite64(fd: usize, buf: *const u8, len: usize, offset: usize) -> isize {
    // Linux checks the fd (EBADF) before copying from user; validate first.
    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    match fd_obj.file.file_type() {
        crate::fs::FileType::Directory => return -21,
        crate::fs::FileType::Pipe | crate::fs::FileType::Socket => return -29,
        _ => {}
    }
    if !fd_obj.writable() {
        return -9;
    }
    if (offset as i64) < 0 {
        return -22;
    }
    let file = match fd_obj.file.as_file() {
        Some(file) => file,
        None => return -22,
    };
    const O_APPEND: u32 = 0x400;
    let append =
        fd_obj.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_APPEND != 0;
    // Write in <=1MiB chunks (read_user_buffer caps single copies at 1MiB) so a
    // large pwrite completes fully like Linux instead of a 1MiB short write.
    const CHUNK: usize = 1024 * 1024;
    let mut total: usize = 0;
    let mut notified = false;
    while total < len {
        let this = (len - total).min(CHUNK);
        let data = match read_user_buffer(unsafe { buf.add(total) }, this) {
            Ok(d) => d,
            Err(_) => {
                if total > 0 { break; }
                return -14;
            }
        };
        // O_APPEND: Linux pwrite still appends at EOF regardless of offset.
        let eff_offset = if append {
            file.stat().map(|s| s.size).unwrap_or(offset + total)
        } else {
            offset + total
        };
        match file.write(eff_offset, &data) {
            Ok(n) => {
                if n > 0 && !notified {
                    crate::syscall::fd::notify_inotify_modify(&fd_obj);
                    notified = true;
                }
                total += n;
                if n < this { break; } // short write
            }
            Err(err) => {
                if total > 0 { break; }
                return crate::syscall::fd::fs_error_to_errno(err);
            }
        }
    }
    crate::sched::process::account_io_write(total, true);
    total as isize
}

#[allow(dead_code)]
pub(crate) fn sys_preadv(fd: usize, iov_ptr: usize, iovcnt: usize, offset: usize) -> isize {
    if iovcnt == 0 || iovcnt > 1024 {
        return SyscallError::InvalidArgument as isize;
    }
    let iov_size = iovcnt * 16;
    let iov_data = match read_user_buffer(iov_ptr as *const u8, iov_size) {
        Ok(d) => d,
        Err(_) => return -14,
    };
    if (offset as i64) < 0 {
        return -22;
    }
    if let Err(e) = validate_iovec_lengths(&iov_data, iovcnt) {
        return e;
    }
    let mut total: usize = 0;
    let mut cur_off = offset;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_data[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_data[off + 8..off + 16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let n = sys_pread64(fd, base as *mut u8, len, cur_off);
        if n < 0 {
            return if total > 0 { total as isize } else { n };
        }
        let n = n as usize;
        total += n;
        cur_off += n;
        if n < len {
            break;
        }
    }
    total as isize
}

#[allow(dead_code)]
pub(crate) fn sys_pwritev(fd: usize, iov_ptr: usize, iovcnt: usize, offset: usize) -> isize {
    if iovcnt == 0 || iovcnt > 1024 {
        return SyscallError::InvalidArgument as isize;
    }
    let iov_size = iovcnt * 16;
    let iov_data = match read_user_buffer(iov_ptr as *const u8, iov_size) {
        Ok(d) => d,
        Err(_) => return -14,
    };
    if (offset as i64) < 0 {
        return -22;
    }
    if let Err(e) = validate_iovec_lengths(&iov_data, iovcnt) {
        return e;
    }
    let mut total: usize = 0;
    let mut cur_off = offset;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_data[off..off + 8].try_into().unwrap());
        let len = usize::from_le_bytes(iov_data[off + 8..off + 16].try_into().unwrap());
        if len == 0 {
            continue;
        }
        let n = sys_pwrite64(fd, base as *const u8, len, cur_off);
        if n < 0 {
            return if total > 0 { total as isize } else { n };
        }
        let n = n as usize;
        total += n;
        cur_off += n;
        if n < len {
            break;
        }
    }
    total as isize
}

fn validate_iovec_lengths(iov_data: &[u8], iovcnt: usize) -> Result<(), isize> {
    const SSIZE_MAX: usize = isize::MAX as usize;
    let mut sum: usize = 0;
    for i in 0..iovcnt {
        let off = i * 16;
        let len = usize::from_le_bytes(iov_data[off + 8..off + 16].try_into().unwrap());
        if len > SSIZE_MAX {
            return Err(-22);
        }
        sum = match sum.checked_add(len) {
            Some(s) if s <= SSIZE_MAX => s,
            _ => return Err(-22),
        };
    }
    Ok(())
}
