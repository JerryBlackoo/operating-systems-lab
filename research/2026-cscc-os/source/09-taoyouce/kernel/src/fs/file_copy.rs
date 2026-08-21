//! File-copy syscall helpers.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use crate::fs::FileType;
use crate::syscall::user::{read_user_buffer, write_user_buffer};

/// copy_file_range(fd_in, off_in, fd_out, off_out, len, flags)
pub(crate) fn sys_copy_file_range(
    fd_in: i32,
    off_in: usize,
    fd_out: i32,
    off_out: usize,
    len: usize,
    flags: u32,
) -> isize {
    if flags != 0 {
        return -22;
    }
    if len == 0 {
        return 0;
    }

    let fd_in_obj = match crate::syscall::fd::get_fd_obj(fd_in as usize) {
        Some(f) => f,
        None => return -9,
    };
    let fd_out_obj = match crate::syscall::fd::get_fd_obj(fd_out as usize) {
        Some(f) => f,
        None => return -9,
    };

    let in_ty = fd_in_obj.file.file_type();
    let out_ty = fd_out_obj.file.file_type();

    if matches!(out_ty, FileType::Directory) {
        return -21;
    }

    if !matches!(in_ty, FileType::Regular) || !matches!(out_ty, FileType::Regular) {
        return -22;
    }

    if !fd_in_obj.readable() || !fd_out_obj.writable() {
        return -9;
    }

    if fd_out_obj.file.is_immutable() {
        return -1;
    }
    if crate::mm::swap::swapfile_ino() == Some(fd_out_obj.file.ino()) {
        return -26;
    }

    const O_APPEND: u32 = 0x400;
    if fd_out_obj.status_flags.load(Ordering::Relaxed) & O_APPEND != 0 {
        return -9;
    }

    let src_offset = if off_in != 0 {
        match read_user_buffer(off_in as *const u8, 8) {
            Ok(buf) => u64::from_le_bytes(buf[..8].try_into().unwrap()) as usize,
            Err(_) => return -14,
        }
    } else {
        fd_in_obj.offset.load(Ordering::Relaxed)
    };

    let dst_offset = if off_out != 0 {
        match read_user_buffer(off_out as *const u8, 8) {
            Ok(buf) => u64::from_le_bytes(buf[..8].try_into().unwrap()) as usize,
            Err(_) => return -14,
        }
    } else {
        fd_out_obj.offset.load(Ordering::Relaxed)
    };

    let same_file = fd_in_obj.file.ino() == fd_out_obj.file.ino()
        || fd_in_obj.path.as_ref() == fd_out_obj.path.as_ref();
    if same_file {
        let src_end = src_offset.saturating_add(len);
        let dst_end = dst_offset.saturating_add(len);
        if src_offset < dst_end && dst_offset < src_end {
            return -22;
        }
    }

    if src_offset.checked_add(len).is_none() || dst_offset.checked_add(len).is_none() {
        return -75;
    }

    const MAX_REGULAR_FILE_SIZE: usize = 0x0001_0000_0000_0000;
    if dst_offset >= MAX_REGULAR_FILE_SIZE
        || dst_offset
            .checked_add(len)
            .map_or(true, |end| end > MAX_REGULAR_FILE_SIZE)
    {
        return -27;
    }

    let src_file = match fd_in_obj.file.as_file() {
        Some(f) => f,
        None => return -22,
    };
    let dst_file = match fd_out_obj.file.as_file() {
        Some(f) => f,
        None => return -22,
    };

    let mut total_copied = 0usize;
    let mut cur_src = src_offset;
    let mut cur_dst = dst_offset;
    let mut remaining = len;
    while remaining > 0 {
        let chunk = remaining.min(65536);
        let mut buf = alloc::vec![0u8; chunk];

        let n_read = match src_file.read(cur_src, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) => {
                if total_copied > 0 {
                    break;
                }
                return crate::syscall::fd::fs_error_to_errno(err);
            }
        };
        crate::sched::process::account_io_read(n_read, true);

        let n_written = match dst_file.write(cur_dst, &buf[..n_read]) {
            Ok(n) => n,
            Err(err) => {
                if total_copied > 0 {
                    break;
                }
                return crate::syscall::fd::fs_error_to_errno(err);
            }
        };
        crate::sched::process::account_io_write(n_written, true);

        cur_src += n_read;
        cur_dst += n_written;
        total_copied += n_written;
        remaining -= n_written;

        if n_written < n_read {
            break;
        }
    }

    if off_in != 0 {
        let new_off = (src_offset + total_copied) as u64;
        let _ = write_user_buffer(off_in as *mut u8, &new_off.to_le_bytes());
    } else {
        fd_in_obj.offset.fetch_add(total_copied, Ordering::Relaxed);
    }

    if off_out != 0 {
        let new_off = (dst_offset + total_copied) as u64;
        let _ = write_user_buffer(off_out as *mut u8, &new_off.to_le_bytes());
    } else {
        fd_out_obj.offset.fetch_add(total_copied, Ordering::Relaxed);
    }

    total_copied as isize
}
