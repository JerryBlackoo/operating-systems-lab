//! Link-related syscalls: mknodat, linkat, readlinkat, symlinkat.
//!
//! Split from `path.rs` to align with Linux `fs/namei.c` link operations.

use super::user::{UserSlice, UserCString};
use super::SyscallError;
use crate::fs::fd as fd_table;
use super::path::{
    notify_inotify_create, check_prefix_search_effective, access_allowed,
    current_access_ids, resolve_proc_fd_inode_pub, resolve_proc_fd_link,
    access_fs_errno,
};

/// mknodat(dirfd, pathname, mode, dev) — POSIX mknod 真实现 (Slice 1.9)。
///
/// 按 `mode` 高位 S_IFMT 区分节点类型并在父目录创建对应 inode：
///   - S_IFREG (0o0000) -> Regular
///   - S_IFCHR (0o020000) -> CharDevice (dev 编码忽略，与 /dev/null 同行为)
///   - S_IFBLK (0o060000) -> BlockDevice
///   - S_IFIFO (0o010000) -> Pipe (FIFO，不持有 reader/writer，等首次 open)
///   - S_IFSOCK (0o140000) -> Socket (用作 UNIX domain socket bind 落地)
///   - 其他 -> -EINVAL
///
/// 不需要 4 KiB 栈缓冲（避免 listen01 stack-pressure regression）。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_mknodat(
    dirfd: isize,
    pathname: *const u8,
    mode: u32,
    dev: u64,
) -> isize {
    const S_IFMT:  u32 = 0o170000;
    const S_IFREG: u32 = 0o100000;
    const S_IFCHR: u32 = 0o020000;
    const S_IFBLK: u32 = 0o060000;
    const S_IFIFO: u32 = 0o010000;
    const S_IFSOCK: u32 = 0o140000;
    // getname semantics: EFAULT for a bad pointer, ENAMETOOLONG for an over-long
    // pathname/component (mknod06); an empty pathname is ENOENT (mknod06).
    let path = match super::user::read_path(pathname) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if path.is_empty() {
        return -2; // -ENOENT
    }
    let fmt = mode & S_IFMT;
    // POSIX: when no type bits set, default to Regular (matches Linux).
    let file_type = match fmt {
        0 | S_IFREG => crate::fs::FileType::Regular,
        S_IFCHR => crate::fs::FileType::CharDevice,
        S_IFBLK => crate::fs::FileType::BlockDevice,
        S_IFIFO => crate::fs::FileType::Pipe,
        S_IFSOCK => crate::fs::FileType::Socket,
        _ => return -22, // -EINVAL
    };
    let full = match super::resolve_at_path(dirfd, &path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // namei on the path prefix: EACCES (non-searchable ancestor, mknod07),
    // ENOTDIR (file component, mknod06), ENOENT (missing component, mknod06),
    // ELOOP (symlink cycle, mknod07).
    if let Err(e) = check_prefix_search_effective(&full) {
        return e;
    }
    if crate::fs::lookup(&full).is_ok() { return -17; }
    // mnt_want_write: creating a node on a read-only mount fails with EROFS,
    // and Linux checks this before the parent-directory permission and CAP_MKNOD
    // gates (mknod07/mknodat02 EROFS subcases on a needs_rofs mount).
    if crate::fs::is_readonly_path(&full) {
        return -30; // -EROFS
    }
    let (parent_path, name) = match full.rfind('/') {
        Some(0) => ("/", &full[1..]),
        Some(p) => (&full[..p], &full[p + 1..]),
        None => return -22,
    };
    if name.is_empty() { return -22; }
    // CAP_MKNOD: only a privileged caller may create character/block device
    // special files; an unprivileged caller gets EPERM (mknod07).
    if matches!(
        file_type,
        crate::fs::FileType::CharDevice | crate::fs::FileType::BlockDevice
    ) {
        let (euid, _) = current_access_ids(true);
        if euid != 0 {
            return -1; // -EPERM
        }
    }
    let parent_inode = match crate::fs::lookup(parent_path) {
        Ok(i) => i,
        Err(crate::fs::FsError::NotDirectory) => return -20,
        Err(crate::fs::FsError::TooManyLinks) => return -40,
        Err(_) => return -2,
    };
    let dir = match parent_inode.as_dir() {
        Some(d) => d,
        None => return -20,
    };
    // Creating a new entry requires write permission on the parent directory
    // (mknod07 EACCES on a 0500 directory owned by the caller).
    {
        const W_OK: u32 = 2;
        let (euid, egid) = current_access_ids(true);
        if let Ok(pstat) = parent_inode.stat() {
            if !access_allowed(&pstat, euid, egid, W_OK) {
                return -13; // -EACCES
            }
        }
    }
    match dir.create(name, file_type) {
        Ok(inode) => {
            // 字符/块设备节点记录 rdev：用 Linux new_decode_dev 拆分（与 glibc
            // gnu_dev_makedev 互逆），statx(2)/stat(2) 才能回报 rdev_major/minor。
            if matches!(
                file_type,
                crate::fs::FileType::CharDevice | crate::fs::FileType::BlockDevice
            ) {
                let d = dev as u32;
                let major = (d & 0x000f_ff00) >> 8;
                let minor = (d & 0xff) | ((d >> 12) & 0xfff00);
                let _ = inode.set_device_id(major, minor);
            }
            let create_mode =
                (mode & 0o7777) & !crate::syscall::compat::current_umask();
            if let Err(err) = inode.set_mode(create_mode) {
                return match err {
                    crate::fs::FsError::PermissionDenied => -13,
                    crate::fs::FsError::InvalidArgument => -22,
                    _ => -5,
                };
            }
            crate::fs::dentry_cache_insert(&full, inode);
            notify_inotify_create(parent_path, name, file_type);
            0
        }
        Err(crate::fs::FsError::AlreadyExists) => -17,
        Err(crate::fs::FsError::PermissionDenied) => -13,
        Err(crate::fs::FsError::InvalidArgument) => -22,
        Err(_) => -5,
    }
}

/// linkat(olddirfd, oldpath, newdirfd, newpath, flags) — 真硬链接（Slice 2.x 真化）。
///
/// 之前的 "create+复制内容" 实现是简化（注释里坦言 "不是真硬链接：nlink
/// 不增、unlink 任一不影响另一"，会让 LTP `link02..08` / `linkat02..04`
/// 检 nlink/unlink 语义的子项 FAIL）。现在走真 `Directory::link` 路径：
///
/// 1. dirfd-aware 解析 old / new 双路径；
/// 2. lookup 源 `Arc<dyn Inode>`；directory 类型 → -EPERM（Linux 行为）；
/// 3. lookup 目标父目录 → 调 `Directory::link(name, &src)` —— ramfs
///    override 真做：把 **同一个 Arc** 插入 children，nlink+=1；
/// 4. 跨 fs / 不支持 link 的 fs 默认返 -EXDEV；
/// 5. `flags & AT_SYMLINK_FOLLOW`：默认不 follow symlink（POSIX/Linux 默认）。
///
/// Linux 错误码对齐：
///   * 源是 directory → -EPERM(-1)
///   * 目标已存在 → -EEXIST(-17)
///   * 跨 fs → -EXDEV(-18)
///   * 路径不存在 → -ENOENT(-2)
///   * 父非目录 → -ENOTDIR(-20)
#[allow(dead_code)]
pub(in crate::syscall) fn sys_linkat(
    olddirfd: isize,
    oldpath: *const u8,
    newdirfd: isize,
    newpath: *const u8,
    _flags: u32,
) -> isize {
    const AT_SYMLINK_FOLLOW: u32 = 0x400;
    const AT_EMPTY_PATH: u32 = 0x1000;
    // linkat(2) only accepts AT_SYMLINK_FOLLOW and AT_EMPTY_PATH; any other bit
    // is rejected with EINVAL (linkat01 case 22, flags=1).
    if _flags & !(AT_SYMLINK_FOLLOW | AT_EMPTY_PATH) != 0 {
        return -22; // -EINVAL
    }
    let old = match super::user::read_path(oldpath) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let new_str = match super::user::read_path(newpath) {
        Ok(s) => s,
        Err(e) => return e,
    };
    // Empty pathname is ENOENT unless linkat() requests AT_EMPTY_PATH (old side
    // only). newpath has no such relaxation. (link04 empty-string cases.)
    if old.is_empty() && _flags & AT_EMPTY_PATH == 0 {
        return -2; // -ENOENT
    }
    if new_str.is_empty() {
        return -2; // -ENOENT
    }
    // Resolve the source inode, handling AT_EMPTY_PATH and /proc/self/fd/N
    let full_new = match super::resolve_at_path(newdirfd, &new_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let (src_inode, resolved_old) = if old.is_empty() && _flags & AT_EMPTY_PATH != 0 {
        // AT_EMPTY_PATH: old="" → use olddirfd's inode directly
        if olddirfd < 0 { return -9; } // EBADF
        let (inode, path) = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_ref() { Some(t) => t, None => return -3 };
            let pid = match t.current { Some(p) => p, None => return -3 };
            let process = match t.get_fd_process(pid) { Some(p) => p, None => return -3 };
            let fd_obj = match fd_table::get(process, olddirfd as usize) { Some(f) => f, None => return -9 };
            (fd_obj.file.clone(), fd_obj.path.clone().unwrap_or_default())
        };
        (inode, path)
    } else {
        let full_old = match super::resolve_at_path(olddirfd, &old) {
            Ok(p) => p,
            Err(e) => return e,
        };
        // Try /proc/self/fd/N → get inode directly (handles O_TMPFILE anonymous fds)
        if let Some(inode) = resolve_proc_fd_inode_pub(&full_old) {
            let path = resolve_proc_fd_link(&full_old).unwrap_or(full_old.clone());
            // Use resolved fd path for mount check; if it's anon, use dest path
            // so cross-device check passes (same-fs enforced by fs link() itself).
            let effective_path = if path.starts_with('/') && !path.starts_with("/proc") {
                path
            } else {
                full_new.clone()
            };
            (inode, effective_path)
        } else {
            let inode = match crate::fs::lookup_no_follow(&full_old) {
                Ok(i) => i,
                Err(e) => return super::fd::fs_error_to_errno(e),
            };
            (inode, full_old)
        }
    };
    // 真 Linux: hardlink 不能指向目录 → -EPERM
    if src_inode.file_type() == crate::fs::FileType::Directory {
        return -1; // -EPERM
    }
    if crate::fs::lookup(&full_new).is_ok() {
        return -17; // -EEXIST
    }
    // mnt_want_write on the destination: creating a link on a read-only mount
    // fails with EROFS (link08 EROFS subcase).
    if crate::fs::is_readonly_path(&full_new) {
        return -30; // -EROFS
    }
    let (new_parent_path, new_name) = match full_new.rfind('/') {
        Some(0) => ("/", &full_new[1..]),
        Some(p) => (&full_new[..p], &full_new[p + 1..]),
        None => return -22,
    };
    if new_name.is_empty() { return -22; }
    let new_parent = match crate::fs::lookup(new_parent_path) {
        Ok(p) => p,
        Err(e) => return super::fd::fs_error_to_errno(e),
    };
    let new_dir = match new_parent.as_dir() {
        Some(d) => d,
        None => return -20,
    };
    // Linux namei: MAY_EXEC on every ancestor of both operands, and
    // may_create(new_parent) requires MAY_WRITE | MAY_EXEC on the destination
    // directory. A non-root caller lacking these gets EACCES (link04 last two
    // "write/search access directory" cases). Root bypasses via access_allowed.
    if let Err(e) = check_prefix_search_effective(&resolved_old) {
        return e;
    }
    if let Err(e) = check_prefix_search_effective(&full_new) {
        return e;
    }
    {
        const W_OK: u32 = 2;
        const X_OK: u32 = 1;
        let (euid, egid) = current_access_ids(true);
        if let Ok(pstat) = new_parent.stat() {
            if !access_allowed(&pstat, euid, egid, W_OK | X_OK) {
                return -13; // -EACCES
            }
        }
    }
    // cross-device link check: source and destination must be on the same mount
    {
        let src_mid = crate::fs::mount_id_for_path(&resolved_old);
        let dst_mid = crate::fs::mount_id_for_path(new_parent_path);
        if src_mid != dst_mid { return -18; } // -EXDEV
    }
    // 真硬链接：让 fs 把同一 Arc 挂到第二个 dirent，nlink+=1
    match new_dir.link(new_name, &src_inode) {
        Ok(()) => {
            notify_inotify_create(new_parent_path, new_name, src_inode.file_type());
            0
        }
        Err(crate::fs::FsError::AlreadyExists) => -17,
        // 不支持 link 的 fs (procfs/devfs/ext4 shim) → 跨 fs 等价 EXDEV
        Err(crate::fs::FsError::PermissionDenied) => -18, // -EXDEV
        Err(_) => -5, // -EIO
    }
}

/// readlinkat(78) — 读取符号链接目标。
///
/// Slice 2.x TIER 6 真化（去 `-EINVAL` 占位）：lookup 路径找到 inode；若
/// `file_type() == Symlink`，通过 `as_file().read()` 读出 target 内容（即
/// `sys_symlinkat` 写入的字节），拷贝最多 `bufsiz` 字节到 user buf，返回
/// 真实拷贝数（**不**含 NUL 终止符，与 Linux `readlink(2)` 行为一致）。
///
/// 特殊路径：`/proc/self/exe` → 当前进程的 `exec_path`（与历史行为一致）。
///
/// 错误码：
/// * `bufsiz == 0` → `-EINVAL(22)`
/// * 路径不存在 → `-ENOENT(2)`
/// * 不是 symlink → `-EINVAL(22)`（man readlink(2)）
/// * read 失败 → `-EIO(5)`
/// * 拷给 user 失败 → `-EFAULT(14)`
#[allow(dead_code)]
pub(in crate::syscall) fn sys_readlinkat(dirfd: isize, pathname: *const u8, buf: *mut u8, bufsiz: usize) -> isize {
    let path = match super::user::read_path(pathname) {
        Ok(s) => s,
        Err(e) => return e, // -EFAULT or -ENAMETOOLONG
    };
    if bufsiz == 0 { return -22; }
    // Linux AT_EMPTY_PATH semantic: readlinkat(fd, "") reads the symlink
    // target of the file that fd refers to (opened with O_PATH|O_NOFOLLOW).
    const AT_FDCWD: isize = -100;
    if path.is_empty() && dirfd != AT_FDCWD {
        if dirfd < 0 { return -9; } // EBADF
        let inode = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let table = match table.as_ref() { Some(t) => t, None => return -3 };
            let pid = match table.current { Some(p) => p, None => return -3 };
            let process = match table.get_fd_process(pid) { Some(p) => p, None => return -3 };
            let fd = match fd_table::get(process, dirfd as usize) { Some(f) => f, None => return -9 };
            fd.file.clone()
        };
        if inode.file_type() != crate::fs::FileType::Symlink {
            return -22; // EINVAL: not a symlink
        }
        let file = match inode.as_file() { Some(f) => f, None => return -22 };
        let target_len = file.stat().map(|s| s.size).unwrap_or(4096).min(4096);
        let read_len = target_len.min(bufsiz);
        let mut tmp = alloc::vec![0u8; read_len];
        let n = match file.read(0, &mut tmp) { Ok(n) => n, Err(_) => return -5 };
        if UserSlice::<u8>::new_mut(buf, n).write_bytes(&tmp[..n]).is_err() { return -14; }
        return n as isize;
    }
    if path.is_empty() { return -2; } // ENOENT: empty path with AT_FDCWD
    // 统一 dirfd 解析
    let full_path = match super::resolve_at_path(dirfd, &path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // 特殊路径: /proc/self/exe → 返回当前进程真实可执行文件路径
    if full_path == "/proc/self/exe" {
        if let Some(exec_path) = super::current_process_exec_path() {
            let resolved = super::resolve_path(&exec_path);
            let bytes = resolved.as_bytes();
            let copy_len = bytes.len().min(bufsiz);
            if UserSlice::<u8>::new_mut(buf, copy_len).write_bytes(&bytes[..copy_len]).is_ok() {
                return copy_len as isize;
            }
            return -14;
        }
    }
    // /proc/self/fd/N or /proc/[pid]/fd/N → path of the open file descriptor
    if let Some(fd_path) = resolve_proc_fd_link(&full_path) {
        let bytes = fd_path.as_bytes();
        let copy_len = bytes.len().min(bufsiz);
        if UserSlice::<u8>::new_mut(buf, copy_len).write_bytes(&bytes[..copy_len]).is_ok() {
            return copy_len as isize;
        }
        return -14;
    }
    // /proc/<pid|self>/ns/<name> → nsfs magic symlink target "name:[inode]"
    if let Some(target) = super::path::resolve_proc_ns_link(&full_path) {
        let bytes = target.as_bytes();
        let copy_len = bytes.len().min(bufsiz);
        if UserSlice::<u8>::new_mut(buf, copy_len).write_bytes(&bytes[..copy_len]).is_ok() {
            return copy_len as isize;
        }
        return -14;
    }
    // procfs 魔法符号链接 /<proc-mount>/self、/<proc-mount>/thread-self：
    // Linux procfs 把它们实现为指向 "<pid>" / "<tgid>/<tid>" 的符号链接, readlink
    // 返回该 pid 字符串。固定 /proc 路径之外, 用户也可把 proc 挂到任意点(pidns03
    // 把全新 proc 挂到临时目录后 readlink self), 故按 fstype 判定而非路径前缀。
    // pid 须按当前进程所在 PID namespace 翻译(与 getpid 一致)。
    {
        let base = full_path.rsplit('/').next().unwrap_or("");
        if (base == "self" || base == "thread-self")
            && crate::fs::mount::fstype_for_path(&full_path).as_deref() == Some("proc")
        {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some((t, cur)) = table.as_ref().and_then(|t| t.current.map(|c| (t, c))) {
                let p = t.get(cur);
                let root = p.map(|p| p.pidns_root).unwrap_or(0);
                let tgid = p.map(|p| if p.is_thread { p.tgid } else { cur }).unwrap_or(cur);
                let vnr = |g: usize| if root == 0 || g <= root { g } else { g - root };
                let target = if base == "self" {
                    alloc::format!("{}", vnr(tgid))
                } else {
                    alloc::format!("{}/{}", vnr(tgid), vnr(cur))
                };
                drop(table);
                let bytes = target.as_bytes();
                let copy_len = bytes.len().min(bufsiz);
                if UserSlice::<u8>::new_mut(buf, copy_len).write_bytes(&bytes[..copy_len]).is_ok() {
                    return copy_len as isize;
                }
                return -14;
            }
        }
    }
    // 真 symlink：lookup → 校验类型 → 读 target 字节。
    // namei 错误码须区分：中间分量是普通文件 → ENOTDIR（readlinkat02 的
    // "test_file/test_file" 子项期望 ENOTDIR，旧实现一律 ENOENT）；符号链接
    // 成环 → ELOOP。
    // Check ENAMETOOLONG: full path or any component
    {
        const PATH_MAX: usize = 4096;
        const NAME_MAX: usize = 255;
        if full_path.len() >= PATH_MAX || full_path.split('/').any(|c| c.len() > NAME_MAX) {
            return -36; // ENAMETOOLONG
        }
    }
    // Linux namei: MAY_EXEC on every ancestor directory (readlink03 EACCES test)
    if let Err(e) = check_prefix_search_effective(&full_path) {
        return e;
    }
    let inode = match crate::fs::lookup_no_follow(&full_path) {
        Ok(i) => i,
        Err(crate::fs::FsError::NotDirectory) => return -20, // ENOTDIR
        Err(crate::fs::FsError::TooManyLinks) => return -40, // ELOOP
        Err(_) => return -2, // ENOENT
    };
    if inode.file_type() != crate::fs::FileType::Symlink {
        return -22;
    }
    let file = match inode.as_file() {
        Some(f) => f,
        None => return -22,
    };
    // 先用 stat.size 决定读多少；fallback 到 PATH_MAX(4096)
    let target_len = file.stat().map(|s| s.size).unwrap_or(4096).min(4096);
    let read_len = target_len.min(bufsiz);
    let mut tmp = alloc::vec![0u8; read_len];
    let n = match file.read(0, &mut tmp) {
        Ok(n) => n,
        Err(_) => return -5,
    };
    if UserSlice::<u8>::new_mut(buf, n).write_bytes(&tmp[..n]).is_err() {
        return -14;
    }
    n as isize
}

pub(in crate::syscall) fn sys_symlinkat(target_ptr: *const u8, newdirfd: isize, linkpath_ptr: *const u8) -> isize {
    let target = match super::user::read_path(target_ptr) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let linkpath = match super::user::read_path(linkpath_ptr) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if target.is_empty() || linkpath.is_empty() {
        return -2;
    }
    let full_path = match super::resolve_at_path(newdirfd, &linkpath) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let (parent_path, name) = match full_path.rfind('/') {
        Some(pos) if pos > 0 => (&full_path[..pos], &full_path[pos + 1..]),
        Some(_) => ("/", &full_path[1..]),
        None => return -22,
    };
    if name.is_empty() {
        return -2;
    }
    let parent = match crate::fs::lookup(parent_path) {
        Ok(inode) => inode,
        Err(_) => return -2,
    };
    // Write permission on parent directory is required to create a symlink.
    {
        const W_OK: u32 = 2;
        let (euid, egid) = current_access_ids(true);
        if let Ok(pstat) = parent.stat() {
            if !access_allowed(&pstat, euid, egid, W_OK) {
                return -13; // EACCES
            }
        }
    }
    let dir = match parent.as_dir() {
        Some(dir) => dir,
        None => return -20,
    };
    let inode = match dir.create(name, crate::fs::FileType::Symlink) {
        Ok(inode) => inode,
        Err(crate::fs::FsError::AlreadyExists) => return -17,
        Err(crate::fs::FsError::NotDirectory) => return -20,
        Err(crate::fs::FsError::PermissionDenied) => return -13,
        Err(_) => return -5,
    };
    match inode.as_file() {
        Some(file) => match file.write(0, target.as_bytes()) {
            Ok(_) => {
                crate::fs::dentry_cache_insert(&full_path, inode.clone());
                notify_inotify_create(parent_path, name, crate::fs::FileType::Symlink);
                0
            }
            Err(_) => -5,
        },
        None => -5,
    }
}
