//! Linux getdents64(2) — read directory entries in linux_dirent64 format.

use super::user::write_user_buffer;
use super::SyscallError;
use crate::fs::fd as fd_table;

/// Linux dirent64 记录长度：d_ino + d_off + d_reclen + d_type + name + NUL，
/// 按 8 字节对齐。
#[inline]
fn dirent_reclen(name_len: usize) -> usize {
    (8 + 8 + 2 + 1 + name_len + 1 + 7) & !7
}

/// 只克隆「本 getdents 调用真正要写进用户缓冲」的目录项子集。
///
/// find/cargo 会以 32KiB 缓冲多次调用 getdents；旧实现每次调用都 `cloned()`
/// 整个目录（大 target 目录数千项），把 `find target` 变成 O(n²) 字符串克隆，
/// BuildStorm cargo 完成后光 find artifact 就数分钟不返回。
/// 返回 None 表示首个目录项都放不进 buf（Linux EINVAL 语义）。
fn select_dirents(
    entries: &[crate::fs::DirEntry],
    start: usize,
    buf_size: usize,
) -> Option<alloc::vec::Vec<crate::fs::DirEntry>> {
    if start >= entries.len() {
        return Some(alloc::vec::Vec::new());
    }
    let first_reclen = dirent_reclen(entries[start].name.len());
    if first_reclen > buf_size {
        return None;
    }
    let mut used = 0usize;
    let mut end = start;
    while end < entries.len() {
        let reclen = dirent_reclen(entries[end].name.len());
        if used + reclen > buf_size {
            break;
        }
        used += reclen;
        end += 1;
    }
    Some(entries[start..end].to_vec())
}

/// 读取目录项（linux_dirent64 格式）
#[allow(dead_code)]
pub(in crate::syscall) fn sys_getdents(fd: usize, buf: *mut u8, buf_size: usize) -> isize {
    use crate::sched::process::PROCESS_TABLE;
    use crate::fs::Directory;
    use core::sync::atomic::Ordering;

    let (dir_inode, entry_start, fd_obj, dir_path) = if fd == (-100isize) as usize {
        let path = {
            let table = PROCESS_TABLE.lock();
            if let Some(table) = table.as_ref() {
                if let Some(current_pid) = table.current {
                    if let Some(process) = table.get(current_pid) {
                        process.cwd()
                    } else {
                        return SyscallError::InvalidArgument as isize;
                    }
                } else {
                    return SyscallError::InvalidArgument as isize;
                }
            } else {
                return SyscallError::InvalidArgument as isize;
            }
        };
        match crate::fs::lookup(&path) {
            Ok(inode) => (inode, 0usize, None, Some(path)),
            Err(_) => return SyscallError::NotFound as isize,
        }
    } else {
        // 从 fd_table 获取目录 inode + 当前偏移（条目索引）
        let table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_ref() {
            if let Some(current_pid) = table.current {
                if let Some(process) = table.get_fd_process(current_pid) {
                    if let Some(file_desc) = fd_table::get(process, fd) {
                        let inode = file_desc.file.clone();
                        let start = file_desc.offset.load(Ordering::Relaxed);
                        let p = file_desc.path.clone();
                        (inode, start, Some(file_desc), p)
                    } else {
                        return -9; // -EBADF
                    }
                } else {
                    return SyscallError::InvalidArgument as isize;
                }
            } else {
                return SyscallError::InvalidArgument as isize;
            }
        } else {
            return SyscallError::InvalidArgument as isize;
        }
    };

    let dir = match dir_inode.as_dir() {
        Some(dir) => dir,
        None => return -20,
    };

    // Linux iterate_dir() rejects a directory removed via rmdir (S_DEAD) before
    // reading any entry: getdents on a still-open fd returns ENOENT (getdents02).
    if let Ok(st) = dir_inode.stat() {
        if st.nlink == 0 {
            return -2; // -ENOENT
        }
    }

    // Linux VFS always returns . and .. as the first two entries with real
    // inode numbers: "." is the directory itself, ".." its parent (self at a
    // filesystem root). glibc's getcwd fallback matches directory entries by
    // (dev,ino), so synthetic values here must be the true ones.
    let self_ino = dir_inode.ino();
    let parent_ino = dir_path
        .as_deref()
        .filter(|p| *p != "/")
        .and_then(|p| {
            let parent = match p.rfind('/') {
                Some(0) => "/",
                Some(idx) => &p[..idx],
                None => "/",
            };
            crate::fs::lookup(parent).ok().map(|i| i.ino())
        })
        .unwrap_or(self_ino);
    let prepend_dots = |entries: &mut alloc::vec::Vec<crate::fs::DirEntry>| {
        let has_dot = entries.iter().any(|e| e.name == ".");
        let has_dotdot = entries.iter().any(|e| e.name == "..");
        if !has_dotdot {
            entries.insert(0, crate::fs::DirEntry {
                name: alloc::string::String::from(".."),
                file_type: crate::fs::FileType::Directory,
                inode: parent_ino,
            });
        }
        if !has_dot {
            entries.insert(0, crate::fs::DirEntry {
                name: alloc::string::String::from("."),
                file_type: crate::fs::FileType::Directory,
                inode: self_ino,
            });
        }
    };
    let entries = if let Some(ref fd_obj) = fd_obj {
        let mut cache = fd_obj.dir_entries.lock();
        if cache.is_none() {
            match dir.read_dir() {
                Ok(mut entries) => {
                    prepend_dots(&mut entries);
                    *cache = Some(entries);
                }
                Err(_) => return SyscallError::IoError as isize,
            }
        }
        let all = cache.as_ref().unwrap();
        match select_dirents(all, entry_start, buf_size) {
            None => return -22, // -EINVAL
            Some(entries) => entries,
        }
    } else {
        match dir.read_dir() {
            Ok(mut entries) => {
                prepend_dots(&mut entries);
                match select_dirents(&entries, entry_start, buf_size) {
                    None => return -22, // -EINVAL
                    Some(selected) => selected,
                }
            }
            Err(_) => return SyscallError::IoError as isize,
        }
    };

    if entries.is_empty() {
        return 0;
    }

    let mut offset = 0usize;
    let mut entries_written = 0usize;

    for (i, entry) in entries.iter().enumerate() {
        let name_bytes = entry.name.as_bytes();
        let name_len = name_bytes.len();
        let reclen = dirent_reclen(name_len);

        let d_type: u8 = match entry.file_type {
            crate::fs::FileType::Regular => 8,
            crate::fs::FileType::Directory => 4,
            crate::fs::FileType::Symlink => 10,
            _ => 0,
        };

        let mut record = alloc::vec![0u8; reclen];
        record[0..8].copy_from_slice(&(entry.inode as u64).to_le_bytes());
        let next_off = (entry_start + i + 1) as u64;
        record[8..16].copy_from_slice(&next_off.to_le_bytes());
        record[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
        record[18] = d_type;
        let copy_len = name_len.min(reclen - 19 - 1);
        record[19..19 + copy_len].copy_from_slice(&name_bytes[..copy_len]);

        let entry_ptr = unsafe { buf.add(offset) };
        if write_user_buffer(entry_ptr, &record).is_err() {
            // Copying the first entry to a bad user buffer is EFAULT, matching
            // Linux copy_to_user failure in filldir64 (getdents02 EFAULT case).
            if entries_written == 0 {
                return -14; // -EFAULT
            }
            break;
        }

        offset += reclen;
        entries_written += 1;
    }

    // Linux touch_atime on directory read (getdents).
    if entries_written > 0 {
        if let Some(ref fd_o) = fd_obj {
            if let Some(ref p) = fd_o.path {
                crate::fs::touch_atime(&dir_inode, p);
            }
        }
    }

    if fd != (-100isize) as usize && entries_written > 0 {
        let table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_ref() {
            if let Some(current_pid) = table.current {
                if let Some(process) = table.get_fd_process(current_pid) {
                    if let Some(file_desc) = fd_table::get(process, fd) {
                        file_desc.offset.store(entry_start + entries_written, Ordering::Relaxed);
                    }
                }
            }
        }
    }

    offset as isize
}
