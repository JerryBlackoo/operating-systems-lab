//! Working directory syscalls: getcwd, chdir, fchdir, chroot.
//!
//! Split from `path.rs` to align with Linux `fs/open.c` cwd operations.

use super::user::{read_c_string, UserSlice};
use super::path::{access_allowed, current_access_ids, access_fs_errno};

/// 获取当前工作目录
#[allow(dead_code)]
pub(in crate::syscall) fn sys_getcwd(buf: *mut u8, size: usize) -> isize {
    use crate::sched::process::PROCESS_TABLE;

    // Linux raw syscall ABI differs from libc `getcwd(3)`: the kernel returns
    // the number of bytes copied, including the trailing NUL.  Returning the
    // user pointer here is a libc-wrapper semantic and breaks glibc callers
    // that use the raw result as a length while constructing temporary paths.
    // Linux `sys_getcwd` checks the buffer length (ERANGE) *before* touching the
    // user buffer: a too-small size returns ERANGE even when buf is NULL/bad
    // (getcwd01 size=0 / size=1 / NULL+1 cases). Only when the path fits is the
    // copy attempted, and a bad/NULL destination then yields EFAULT.
    let pair = {
        let table = PROCESS_TABLE.lock();
        table.as_ref().and_then(|t| {
            t.current.and_then(|pid| t.get(pid)).map(|p| {
                let fs = p.fs.lock();
                (fs.cwd.clone(), fs.root.clone())
            })
        })
    };
    let (cwd, root) = match pair {
        Some(pair) => pair,
        None => return -1,
    };
    // Linux getcwd 以进程 root 为基准输出路径；cwd 在 chroot root 之外时
    // 前缀 "(unreachable)"（libc 检查首字符非 '/' 后转 ENOENT，realpath01）。
    let cwd = if root != "/" {
        if cwd == root {
            alloc::string::String::from("/")
        } else if cwd.starts_with(&root) && cwd.as_bytes().get(root.len()) == Some(&b'/') {
            alloc::string::String::from(&cwd[root.len()..])
        } else {
            alloc::format!("(unreachable){}", cwd)
        }
    } else {
        cwd
    };

    let copied_len = cwd.len().saturating_add(1);
    if copied_len > size {
        return -34; // -ERANGE
    }
    if buf.is_null() {
        return -14; // -EFAULT (path fits but no destination)
    }

    let mut out = alloc::vec::Vec::with_capacity(copied_len);
    out.extend_from_slice(cwd.as_bytes());
    out.push(0);
    match UserSlice::<u8>::new_mut(buf, out.len()).write_bytes(&out) {
        Ok(()) => copied_len as isize,
        Err(_) => -14,
    }
}

/// 改变当前工作目录
#[allow(dead_code)]
pub(in crate::syscall) fn sys_chdir(path_ptr: *const u8) -> isize {
    use crate::sched::process::PROCESS_TABLE;

    // Linux chdir(2) path ingestion keeps ENAMETOOLONG distinct from EFAULT:
    // an unterminated string at PATH_MAX or an overlong component must not be
    // collapsed into ENOENT/EFAULT.  LTP chdir04 checks the NAME_MAX branch.
    let path = match chdir_read_path(path_ptr) {
        Ok(s) => s,
        Err(errno) => return errno,
    };
    if path.is_empty() {
        return -2; // -ENOENT
    }

    // 解析路径
    let full_path = super::resolve_path(&path);
    if chdir_path_name_too_long(&full_path) {
        return -36; // -ENAMETOOLONG
    }
    let full_path_dbg = if crate::cmdline::execdbg_enabled() {
        full_path.clone()
    } else {
        alloc::string::String::new()
    };

    // 验证目录存在
    match crate::fs::lookup(&full_path) {
        Ok(inode) => {
            // 检查是否是目录
            if inode.file_type() != crate::fs::FileType::Directory {
                return -20; // -ENOTDIR
            }
            if let Err(errno) = chdir_check_search_permissions(&full_path, &inode) {
                return errno;
            }

            // Linux stores the *physical* directory (all symlinks resolved) as
            // the cwd, so getcwd(2) reaches the real path even when chdir went
            // through a symlink (getcwd03). Fall back to the lexical path if
            // canonicalisation fails for any reason.
            let canon = crate::fs::canonicalize(&full_path).unwrap_or(full_path);
            // 更新进程的 cwd
            let mut table = PROCESS_TABLE.lock();
            if let Some(table) = table.as_mut() {
                if let Some(current_pid) = table.current {
                    if let Some(process) = table.get_mut(current_pid) {
                        process.fs.lock().cwd = canon;
                        return 0;
                    }
                }
            }
            -22 // -EINVAL
        }
        Err(err) => {
            let errno = chdir_fs_errno(err);
            if crate::cmdline::execdbg_enabled() {
                let (pid, tgid) = {
                    let table = PROCESS_TABLE.lock();
                    table
                        .as_ref()
                        .and_then(|t| t.current.map(|pid| (pid, t.get(pid).map(|p| p.tgid).unwrap_or(pid))))
                        .unwrap_or((0, 0))
                };
                let (parent_pid, cur_mm_ptr, cur_pfn, par_mm_ptr, par_pfn) = {
                    let table = PROCESS_TABLE.lock();
                    let mut out = (0usize, 0usize, 0usize, 0usize, 0usize);
                    if let Some(t) = table.as_ref() {
                        let va = crate::mm::VirtAddr(path_ptr as usize);
                        if let Some(p) = t.get(pid) {
                            out.0 = p.parent_pid.unwrap_or(0);
                            if let Some(ref mm) = p.mm {
                                out.1 = alloc::sync::Arc::as_ptr(mm) as *const () as usize;
                                out.2 = mm.lock().page_pfn(va).unwrap_or(0);
                            }
                        }
                        if out.0 != 0 {
                            if let Some(pp) = t.get(out.0) {
                                if let Some(ref mm) = pp.mm {
                                    out.3 = alloc::sync::Arc::as_ptr(mm) as *const () as usize;
                                    out.4 = mm.lock().page_pfn(va).unwrap_or(0);
                                }
                            }
                        }
                    }
                    out
                };
                crate::println!(
                    "[EXECDBG] chdir pid={} tgid={} parent={} uptr={:#x} raw={} full={} errno={} cur_mm={:#x} cur_pfn={:#x} par_mm={:#x} par_pfn={:#x}",
                    pid, tgid, parent_pid, path_ptr as usize, path, full_path_dbg, errno,
                    cur_mm_ptr, cur_pfn, par_mm_ptr, par_pfn
                );
                let off = (path_ptr as usize) & (crate::arch::consts::PAGE_SIZE - 1);
                for (label, pfn) in [("cur", cur_pfn), ("par", par_pfn)] {
                    if pfn == 0 {
                        continue;
                    }
                    let bytes = crate::mm::PhysPageNum(pfn).as_bytes_mut();
                    let end = core::cmp::min(off + 64, bytes.len());
                    let mut ascii = alloc::string::String::new();
                    for &b in &bytes[off..end] {
                        if (0x20..0x7f).contains(&b) {
                            ascii.push(b as char);
                        } else if b == 0 {
                            ascii.push('.');
                        } else {
                            ascii.push('?');
                        }
                    }
                    crate::println!("[EXECDBG] chdir page {} pfn={:#x} off={:#x} ascii={}", label, pfn, off, ascii);
                }
            }
            errno
        }
    }
}

/// fchdir(fd) — change the working directory to the directory referenced by an
/// open file descriptor. Linux `ksys_fchdir`:
/// - bad fd            -> EBADF
/// - fd not a directory-> ENOTDIR
/// - lacks MAY_EXEC on the target inode -> EACCES (checked at call time, so an
///   already-open fd to a now-unsearchable dir still fails; fchdir03)
pub(in crate::syscall) fn sys_fchdir(fd: usize) -> isize {
    use crate::sched::process::PROCESS_TABLE;
    let fd_obj = match super::fd::get_fd_obj(fd) {
        Some(o) => o,
        None => return -9, // -EBADF
    };
    if fd_obj.file.file_type() != crate::fs::FileType::Directory {
        return -20; // -ENOTDIR
    }
    let path = match fd_obj.path.clone() {
        Some(p) if !p.is_empty() => p,
        _ => return -9, // -EBADF: anonymous dir inode without a path
    };
    // Linux checks MAY_EXEC on the target inode only (the fd already resolved
    // the path), not the whole ancestry.
    match fd_obj.file.stat() {
        Ok(stat) => {
            if !chdir_search_allowed(&stat) {
                return -13; // -EACCES
            }
        }
        Err(err) => return chdir_fs_errno(err),
    }
    let mut table = PROCESS_TABLE.lock();
    if let Some(table) = table.as_mut() {
        if let Some(current_pid) = table.current {
            if let Some(process) = table.get_mut(current_pid) {
                process.fs.lock().cwd = path;
                return 0;
            }
        }
    }
    -22 // -EINVAL
}

pub(in crate::syscall) fn sys_chroot(path_ptr: *const u8) -> isize {
    const CAP_SYS_CHROOT: u64 = 1u64 << 18;
    let path = match chdir_read_path(path_ptr) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if path.is_empty() {
        return -2;
    }

    let full_path = super::resolve_path(&path);
    if chdir_path_name_too_long(&full_path) {
        return -36;
    }
    match crate::fs::lookup(&full_path) {
        Ok(inode) => {
            if inode.file_type() != crate::fs::FileType::Directory {
                return -20;
            }
            if let Err(errno) = chdir_check_search_permissions(&full_path, &inode) {
                return errno;
            }
            let (euid, _, caps, _) = current_chdir_cred();
            if euid != 0 && (caps & CAP_SYS_CHROOT) == 0 {
                return -1;
            }
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            let Some(process_table) = table.as_mut() else { return -1; };
            let Some(pid) = process_table.current else { return -1; };
            let Some(process) = process_table.get_mut(pid) else { return -1; };
            process.fs.lock().root = full_path;
            0
        }
        Err(err) => chdir_fs_errno(err),
    }
}

fn chdir_read_path(path_ptr: *const u8) -> Result<alloc::string::String, isize> {
    if path_ptr.is_null() {
        return Err(-14); // EFAULT
    }
    match read_c_string(path_ptr) {
        Ok(path) => {
            if chdir_path_name_too_long(&path) {
                Err(-36) // ENAMETOOLONG
            } else {
                Ok(path)
            }
        }
        Err("C string too long or not null-terminated") => Err(-36),
        Err(_) => Err(-14),
    }
}

fn chdir_path_name_too_long(path: &str) -> bool {
    const PATH_MAX: usize = 4096;
    const NAME_MAX: usize = 255;
    path.len() >= PATH_MAX || path.split('/').any(|component| component.len() > NAME_MAX)
}

fn chdir_fs_errno(err: crate::fs::FsError) -> isize {
    match err {
        crate::fs::FsError::NotFound => -2,
        crate::fs::FsError::NotDirectory => -20,
        crate::fs::FsError::PermissionDenied => -13,
        crate::fs::FsError::OperationNotPermitted => -1,
        // VFS uses InvalidArgument for namei symlink-follow exhaustion.
        // User-visible Linux errno is ELOOP.
        crate::fs::FsError::TooManyLinks => -40,
        crate::fs::FsError::NameTooLong => -36,
        crate::fs::FsError::InvalidArgument => -22,
        _ => -5,
    }
}

fn current_chdir_cred() -> (u32, u32, u64, alloc::vec::Vec<u32>) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| {
            t.current.and_then(|pid| {
                t.get(pid)
                    .map(|p| (p.euid, p.egid, p.cap_effective, p.supplementary_groups.clone()))
            })
        })
        .unwrap_or_else(|| (0, 0, u64::MAX, alloc::vec::Vec::new()))
}

fn chdir_gid_matches(egid: u32, supplementary_groups: &[u32], gid: u32) -> bool {
    egid == gid || supplementary_groups.iter().any(|&group| group == gid)
}

fn chdir_search_allowed(stat: &crate::fs::FileStat) -> bool {
    const CAP_DAC_OVERRIDE: u64 = 1u64 << 1;
    const CAP_DAC_READ_SEARCH: u64 = 1u64 << 2;
    let (euid, egid, caps, supplementary_groups) = current_chdir_cred();
    if euid == 0 || (caps & (CAP_DAC_OVERRIDE | CAP_DAC_READ_SEARCH)) != 0 {
        return true;
    }
    let bits = if euid == stat.uid {
        (stat.mode.0 >> 6) & 0o7
    } else if chdir_gid_matches(egid, &supplementary_groups, stat.gid) {
        (stat.mode.0 >> 3) & 0o7
    } else {
        stat.mode.0 & 0o7
    };
    bits & 0o1 != 0
}

fn chdir_check_search_permissions(
    full_path: &str,
    final_inode: &alloc::sync::Arc<dyn crate::fs::Inode>,
) -> Result<(), isize> {
    let components: alloc::vec::Vec<&str> = full_path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect();
    if components.is_empty() {
        let stat = final_inode.stat().map_err(chdir_fs_errno)?;
        return if chdir_search_allowed(&stat) { Ok(()) } else { Err(-13) };
    }

    let mut prefix = alloc::string::String::from("/");
    for (idx, component) in components.iter().enumerate() {
        if prefix != "/" {
            prefix.push('/');
        }
        prefix.push_str(component);

        let inode = if idx + 1 == components.len() {
            final_inode.clone()
        } else {
            crate::fs::lookup(&prefix).map_err(chdir_fs_errno)?
        };
        if inode.file_type() != crate::fs::FileType::Directory {
            return Err(-20);
        }
        let stat = inode.stat().map_err(chdir_fs_errno)?;
        if !chdir_search_allowed(&stat) {
            return Err(-13);
        }
    }
    Ok(())
}

