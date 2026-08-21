//! File-position syscall helpers.

use core::sync::atomic::Ordering;

/// lseek(fd, offset, whence)
pub(crate) fn sys_lseek(fd: usize, offset: isize, whence: usize) -> isize {
    const SEEK_SET: usize = 0;
    const SEEK_CUR: usize = 1;
    const SEEK_END: usize = 2;
    const SEEK_DATA: usize = 3;
    const SEEK_HOLE: usize = 4;

    let fd_obj = crate::syscall::fd::get_fd_obj(fd);

    let f = match fd_obj {
        Some(f) => f,
        None => return -9,
    };
    if matches!(
        f.file.file_type(),
        crate::fs::FileType::Pipe | crate::fs::FileType::Socket | crate::fs::FileType::CharDevice
    ) {
        return -29;
    }

    let current = f.offset.load(Ordering::Relaxed);

    let new_offset = match whence {
        SEEK_SET => {
            if offset < 0 {
                return -22;
            }
            offset as usize
        }
        SEEK_CUR => {
            if offset < 0 {
                let abs = (-offset) as usize;
                if abs > current {
                    return -22;
                }
                current - abs
            } else {
                current + offset as usize
            }
        }
        SEEK_END => {
            let size = match f.file.stat() {
                Ok(st) => st.size as usize,
                Err(_) => return -5,
            };
            if offset < 0 {
                let abs = (-offset) as usize;
                if abs > size {
                    return -22;
                }
                size - abs
            } else {
                size + offset as usize
            }
        }
        SEEK_DATA => {
            let size = match f.file.stat() {
                Ok(st) => st.size as usize,
                Err(_) => return -5,
            };
            if offset < 0 {
                return -22;
            }
            let o = offset as usize;
            if o >= size {
                return -6;
            }
            o
        }
        SEEK_HOLE => {
            let size = match f.file.stat() {
                Ok(st) => st.size as usize,
                Err(_) => return -5,
            };
            if offset < 0 {
                return -22;
            }
            let o = offset as usize;
            if o >= size {
                return -6;
            }
            size
        }
        _ => return -22,
    };

    f.offset.store(new_offset, Ordering::Relaxed);
    new_offset as isize
}
