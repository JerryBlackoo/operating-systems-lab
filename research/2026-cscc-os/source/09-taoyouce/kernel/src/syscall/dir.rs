//! Directory modification syscalls: mkdir, mkdirat, unlink, unlinkat.
//!
//! Split from `path.rs` to align with Linux `fs/namei.c` directory operations.

use super::user::UserCString;
use super::SyscallError;
use super::path::{
    notify_inotify_create, notify_inotify_delete, dir_setgid_inherit,
    access_allowed, current_access_ids, may_delete_errno_with_flags,
    access_fs_errno,
};

/// 创建目录
#[allow(dead_code)]
pub(in crate::syscall) fn sys_mkdir(path_ptr: *const u8, mode: usize) -> isize {
    // 从用户空间读取路径（null-terminated C 字符串）
    let path = match UserCString::new(path_ptr).read() {
        Ok(s) => s,
        Err(_) => return -14, // -EFAULT
    };

    // 解析路径，获取父目录和新目录名
    let full_path = super::resolve_path(&path);
    let (parent_path, dir_name) = match full_path.rfind('/') {
        Some(pos) if pos > 0 => (&full_path[..pos], &full_path[pos + 1..]),
        Some(_) => ("/", &full_path[1..]), // 根目录下创建
        None => return SyscallError::InvalidArgument as isize,
    };

    if dir_name.is_empty() {
        return SyscallError::InvalidArgument as isize;
    }

    // 查找父目录
    match crate::fs::lookup(parent_path) {
        Ok(parent_inode) => {
            if let Some(parent_dir) = parent_inode.as_dir() {
                // 创建目录
                match parent_dir.create(dir_name, crate::fs::FileType::Directory) {
                    Ok(inode) => {
                        let create_mode =
                            ((mode as u32) & 0o7777) & !crate::syscall::compat::current_umask();
                        let create_mode = dir_setgid_inherit(&parent_inode, create_mode, &inode);
                        if let Err(err) = inode.set_mode(create_mode) {
                            return match err {
                                crate::fs::FsError::PermissionDenied => -13,
                                crate::fs::FsError::InvalidArgument => -22,
                                _ => -5,
                            };
                        }
                        crate::fs::dentry_cache_insert(&full_path, inode);
                        notify_inotify_create(parent_path, dir_name, crate::fs::FileType::Directory);
                        0
                    }
                    Err(crate::fs::FsError::AlreadyExists) => {
                        SyscallError::AlreadyExists as isize
                    }
                    Err(_) => SyscallError::IoError as isize,
                }
            } else {
                SyscallError::NotFound as isize
            }
        }
        Err(_) => SyscallError::NotFound as isize,
    }
}

/// 删除文件
#[allow(dead_code)]
pub(in crate::syscall) fn sys_unlink(path_ptr: *const u8, _flags: usize) -> isize {
    // 从用户空间读取路径：read_path 区分 EFAULT(-14) 与 ENAMETOOLONG(-36)
    // （unlink07 longpathname 子项要求 ENAMETOOLONG 而非 EFAULT）。
    let path = match super::user::read_path(path_ptr) {
        Ok(s) => s,
        Err(e) => return e, // -EFAULT / -ENAMETOOLONG
    };
    // Linux: unlink("") 返回 ENOENT（空路径），而不是误删 cwd。
    if path.is_empty() {
        return -2; // -ENOENT
    }

    // 解析路径，获取父目录和文件名
    let full_path = super::resolve_path(&path);
    let (parent_path, file_name) = match full_path.rfind('/') {
        Some(pos) if pos > 0 => (&full_path[..pos], &full_path[pos + 1..]),
        Some(_) => ("/", &full_path[1..]), // 根目录下删除
        None => return SyscallError::InvalidArgument as isize,
    };

    if file_name.is_empty() {
        return SyscallError::InvalidArgument as isize;
    }
    // 查找父目录
    match crate::fs::lookup(parent_path) {
        Ok(parent_inode) => {
            if let Some(parent_dir) = parent_inode.as_dir() {
                // 热路径减指令：victim 走父目录 dirent 表查找，避免对 full_path
                // 另做两次完整路径遍历（见 sys_unlinkat 同款优化注释）。Directory::
                // lookup 不跟随末级 symlink，等价原 lookup_no_follow 语义。
                let victim = parent_dir.lookup(file_name).ok();
                // Linux may_delete(2)：先校验父目录写/搜索权限(EACCES)，再拒绝
                // 删除目录(EISDIR)。仅在目标存在时校验，缺失时交给 unlink 返回
                // ENOENT。（unlink08 unwritable/unsearchable-dir 与 directory 子项）
                if let Some(ref victim) = victim {
                    if let Ok(vstat) = victim.stat() {
                        let viflags = victim.get_inode_flags();
                        if let Some(err) = may_delete_errno_with_flags(&parent_inode, &vstat, viflags) {
                            return err; // -EACCES / -EPERM
                        }
                    }
                    if victim.file_type() == crate::fs::FileType::Directory {
                        return -21; // -EISDIR（unlink 不能删目录）
                    }
                    // Read-only filesystem check (EROFS).
                    if crate::fs::is_readonly_path(&full_path) {
                        return -30; // -EROFS
                    }
                }
                let removed_type = victim.as_ref().map(|v| v.file_type());
                // 删除文件
                match parent_dir.unlink(file_name) {
                    Ok(_) => {
                        // 普通文件无子路径，删单条目即可（O(log N)）；prefix
                        // 整树扫描（remove_tree 的 O(缓存大小) retain）仅 rmdir
                        // 目录时才需要。lat_fs 删海量小文件时这是 O(N^2) 主因。
                        crate::fs::dentry_cache_remove(&full_path);
                        // Clear xattr entries for the unlinked file
                        super::xattr::xattr_clear_path(&full_path);
                        if let Some(file_type) = removed_type {
                            let vino = victim.as_ref().map(|v| v.ino() as u64).unwrap_or(0);
                            notify_inotify_delete(parent_path, &full_path, file_name, file_type, vino);
                        }
                        0
                    }
                    Err(crate::fs::FsError::NotFound) => {
                        if crate::net::socket::unlink_unix_pathname(&full_path) {
                            notify_inotify_delete(parent_path, &full_path, file_name, crate::fs::FileType::Socket, 0);
                            crate::fs::dentry_cache_remove(&full_path);
                            0
                        } else {
                            SyscallError::NotFound as isize
                        }
                    }
                    Err(crate::fs::FsError::IsDirectory) => {
                        SyscallError::InvalidArgument as isize
                    }
                    Err(_) => SyscallError::IoError as isize,
                }
            } else {
                // 父路径分量不是目录 → ENOTDIR（unlink07 "file/file" 子项）。
                -20
            }
        }
        // 父路径查找失败：按精确 errno 映射（ELOOP/ENOTDIR/ENAMETOOLONG/...）
        // 而非一律 ENOENT。
        Err(e) => access_fs_errno(e),
    }
}

/// mkdirat — Linux 标准接口 (C 字符串, AT_FDCWD)
#[allow(dead_code)]
pub(in crate::syscall) fn sys_mkdirat(dirfd: isize, path_ptr: *const u8, mode: u32) -> isize {
    let path = match super::user::read_path(path_ptr) {
        Ok(s) => s,
        Err(e) => return e, // -EFAULT / -ENAMETOOLONG
    };
    let full_path = match super::resolve_at_path(dirfd, &path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // Linux: mkdir("/") (and any path resolving to the root) fails with
    // EEXIST — the root directory always exists. busybox `mkdir -p /a/b`
    // probes each prefix including "/" and only tolerates EEXIST.
    if full_path == "/" {
        return -17; // -EEXIST
    }
    // Landlock: check MAKE_DIR permission
    if !crate::security::landlock::check_access(&full_path, crate::security::landlock::LANDLOCK_ACCESS_FS_MAKE_DIR) {
        return -13; // -EACCES
    }
    let (parent_path, dir_name) = match full_path.rfind('/') {
        Some(pos) if pos > 0 => (&full_path[..pos], &full_path[pos + 1..]),
        Some(_) => ("/", &full_path[1..]),
        None => return -22, // -EINVAL
    };
    if dir_name.is_empty() {
        return -22;
    }
    match crate::fs::lookup(parent_path) {
        Ok(parent_inode) => {
            // Linux filename_create(): if the final component already exists the
            // call fails with EEXIST *before* any write/EROFS/permission check.
            // Use a no-follow lookup so an existing symlink, fifo, or device node
            // (e.g. mkdir03's /dev/null subcase) is detected as present rather
            // than being routed into create() and surfacing as EIO.
            if crate::fs::lookup_no_follow(&full_path).is_ok() {
                return -17; // -EEXIST
            }
            // Target does not exist yet → this will be a real creation.
            // 只读挂载上创建新目录违反 EROFS（mkdir03 rofs 子项）。
            // is_readonly_path 默认空，普通可写 fs 不受影响。
            if crate::fs::is_readonly_path(&full_path) {
                return -30; // -EROFS
            }
            // Linux may_create(): creating a new directory entry requires both
            // write and search (execute) permission on the parent directory. A
            // non-root user lacking these gets EACCES.
            {
                const W_OK: u32 = 2;
                const X_OK: u32 = 1;
                let (euid, egid) = current_access_ids(true);
                if let Ok(pstat) = parent_inode.stat() {
                    if !access_allowed(&pstat, euid, egid, W_OK | X_OK) {
                        return -13; // -EACCES
                    }
                }
            }
            if let Some(parent_dir) = parent_inode.as_dir() {
                match parent_dir.create(dir_name, crate::fs::FileType::Directory) {
                    Ok(inode) => {
                        let create_mode =
                            (mode & 0o7777) & !crate::syscall::compat::current_umask();
                        let create_mode = dir_setgid_inherit(&parent_inode, create_mode, &inode);
                        if let Err(err) = inode.set_mode(create_mode) {
                            return match err {
                                crate::fs::FsError::PermissionDenied => -13,
                                crate::fs::FsError::InvalidArgument => -22,
                                _ => -5,
                            };
                        }
                        crate::fs::dentry_cache_insert(&full_path, inode);
                        notify_inotify_create(parent_path, dir_name, crate::fs::FileType::Directory);
                        0
                    }
                    Err(crate::fs::FsError::AlreadyExists) => -17, // -EEXIST
                    Err(_) => -5, // -EIO
                }
            } else {
                -20 // -ENOTDIR
            }
        }
        // Map the parent path-walk failure to the precise Linux errno instead of
        // collapsing everything to ENOENT: a symlink loop in the parent prefix
        // must surface as ELOOP (mkdir03 loop_dir subcase), a non-directory
        // component as ENOTDIR, an overlong component as ENAMETOOLONG, etc.
        Err(e) => access_fs_errno(e),
    }
}

/// unlinkat — Linux 标准接口 (C 字符串, AT_FDCWD, flags)
#[allow(dead_code)]
pub(in crate::syscall) fn sys_unlinkat(dirfd: isize, path_ptr: *const u8, flags: u32) -> isize {
    let at_removedir: u32 = 0x200;
    // Linux unlinkat(2)：AT_REMOVEDIR 是唯一合法 flag，其余位置位 → EINVAL
    // （unlinkat01 flag=9999 子项，含 AT_REMOVEDIR 位但还有非法位）。
    if flags & !at_removedir != 0 {
        return -22; // -EINVAL
    }
    // read_path 区分 EFAULT(-14) 与 ENAMETOOLONG(-36)（rmdir02/unlink07 长路径子项）；
    // 与凭证读取合并为一次 PROCESS_TABLE 锁（lat_fs remove 热路径）。
    // 路径读进栈缓冲（对标 Linux getname 定长槽）：常见路径全程零堆分配；
    // 免初始化（read_path_and_cred 只消费已写入前缀），省 256B memset。
    let mut path_buf = [core::mem::MaybeUninit::<u8>::uninit(); 256];
    let (rp, euid, _egid, root_is_plain) =
        match super::user::read_path_and_cred(path_ptr, &mut path_buf) {
            Ok(t) => t,
            Err(e) => return e,
        };
    let path = rp.as_str();
    if path.is_empty() {
        return -2; // -ENOENT（空路径）
    }
    // rmdir 分支要用**用户原始路径**的末分量判 "."/".."（归一化会丢失）。
    let raw_last_component: u8 = if flags & at_removedir != 0 {
        match path.trim_end_matches('/').rsplit('/').next().unwrap_or("") {
            "." => 1,
            ".." => 2,
            _ => 0,
        }
    } else {
        0
    };
    // 绝大多数调用（lat_fs 删海量文件）传入的已是规范绝对路径：
    // 借用快路径直接以栈缓冲 &str 作 full_path，全程零 String 分配；
    // 非规范/相对/chroot 路径才落入 owned 慢路径。
    // 父目录 MRU 前缀直配：整条路径一次前缀比较即完成规范性判定 +
    // 父子切分 + 父目录解析（键即已验证的规范绝对父路径，末分量就地
    // 校验）。命中免去 resolve_path_borrowed_ok 扫描 / rfind / 哈希查找。
    let mru_hit: Option<(usize, alloc::sync::Arc<dyn crate::fs::Inode>)> =
        if root_is_plain && path.as_bytes().first() == Some(&b'/') {
            crate::fs::dentry_parent_mru_match(path)
        } else {
            None
        };
    let full_owned: alloc::string::String;
    let full_path: &str = if mru_hit.is_some() {
        path
    } else {
        match super::path::resolve_path_borrowed_ok(path, root_is_plain) {
        Err(e) => return e,
        Ok(true) => path,
        Ok(false) => {
            let fs = match super::user::current_fs() {
                Ok(f) => f,
                Err(e) => return e,
            };
            let owned = alloc::string::String::from(path);
            full_owned =
                match super::path::resolve_at_path_owned_fs(dirfd, owned, root_is_plain, &fs) {
                    Ok(p) => p,
                    Err(e) => return e,
                };
            &full_owned
        }
        }
    };
    // Landlock: check REMOVE_FILE or REMOVE_DIR permission
    {
        let at_removedir: u32 = 0x200;
        let access = if flags & at_removedir != 0 {
            crate::security::landlock::LANDLOCK_ACCESS_FS_REMOVE_DIR
        } else {
            crate::security::landlock::LANDLOCK_ACCESS_FS_REMOVE_FILE
        };
        if !crate::security::landlock::check_access(&full_path, access) {
            return -13; // -EACCES
        }
    }
    let (parent_path, file_name) = if let Some((pos, _)) = &mru_hit {
        (&full_path[..*pos], &full_path[*pos + 1..])
    } else {
        match full_path.rfind('/') {
            Some(pos) if pos > 0 => (&full_path[..pos], &full_path[pos + 1..]),
            Some(_) => ("/", &full_path[1..]),
            None => return -22,
        }
    };
    if file_name.is_empty() {
        return -22;
    }

    // rmdir(2) 语义（unlinkat AT_REMOVEDIR）：在执行删除前做完整的 may_delete 校验。
    if flags & at_removedir != 0 {
        // 1. 末分量为 "."/".." → EINVAL/ENOTEMPTY（rmdir02 "." 子项）。
        if raw_last_component == 1 {
            return -22; // -EINVAL
        }
        if raw_last_component == 2 {
            return -39; // -ENOTEMPTY
        }
        // 2. 删除挂载点本身 → EBUSY（rmdir02 mntpoint 子项）。
        if crate::fs::is_mount_point(&full_path) {
            return -16; // -EBUSY
        }
        // 3. 只读挂载 → EROFS。Linux do_rmdir() 在解析到父目录后即 mnt_want_write，
        // 早于目标 dentry 查找，因此即使目标不存在，RO 挂载上的删除也先返回 EROFS
        //（rmdir02 mntpoint/dir 子项：dir 并不真实存在于只读挂载里）。
        if crate::fs::is_readonly_path(&full_path) {
            return -30; // -EROFS
        }
        // 4. 目标必须存在且为目录（不跟随末级 symlink）。
        let target = match crate::fs::lookup_no_follow(&full_path) {
            Ok(t) => t,
            Err(e) => return access_fs_errno(e), // ENOENT/ELOOP/ENOTDIR/...
        };
        if target.file_type() != crate::fs::FileType::Directory {
            return -20; // -ENOTDIR
        }
        // 5. 取父目录并做 may_delete 权限/sticky 校验（rmdir03 子项）。
        let parent_inode = match crate::fs::lookup(parent_path) {
            Ok(p) => p,
            Err(e) => return access_fs_errno(e),
        };
        if let Ok(vstat) = target.stat() {
            let viflags = target.get_inode_flags();
            if let Some(err) = may_delete_errno_with_flags(&parent_inode, &vstat, viflags) {
                return err; // -EACCES / -EPERM
            }
        }
        // 6. 非空目录 → ENOTEMPTY（rmdir02 testdir 子项）。
        if let Some(tdir) = target.as_dir() {
            if let Ok(entries) = tdir.read_dir() {
                if entries.iter().any(|e| e.name != "." && e.name != "..") {
                    return -39; // -ENOTEMPTY
                }
            }
        }
        // 7. 删除空目录。
        return match parent_inode.as_dir() {
            Some(parent_dir) => match parent_dir.unlink(file_name) {
                Ok(_) => {
                    // Linux vfs_rmdir sets S_DEAD on the removed directory inode
                    // so an fd still open to it fails getdents with ENOENT.
                    target.mark_dead();
                    // rmdir 成功 ⇒ 目录已空：其子路径的缓存条目在各删除路径
                    //（unlink/rename/rmdir/mount 全清）时已逐条移除，只需删
                    // 目录自身单条目；remove_tree 的 O(全表) prefix retain 是
                    // lat_fs 阶段性 rmdir 的主要开销（每次 ~万级指令）。
                    crate::fs::dentry_cache_remove(&full_path);
                    notify_inotify_delete(
                        parent_path,
                        &full_path,
                        file_name,
                        crate::fs::FileType::Directory,
                        target.ino() as u64,
                    );
                    0
                }
                Err(crate::fs::FsError::NotEmpty) => -39,
                Err(crate::fs::FsError::NotFound) => -2,
                Err(e) => access_fs_errno(e),
            },
            None => -20,
        };
    }

    // EROFS: read-only filesystem prohibits unlink (unlink09 test).
    // Linux do_unlinkat checks mnt_want_write before proceeding.
    if crate::fs::is_readonly_path(&full_path) {
        return -30; // -EROFS
    }
    // dentry 树式解析：非目录末级不入全局 dcache，只取父目录缓存
    //（MRU 单槽优先）；未命中再退回完整 VFS lookup。只有 symlink
    // 末级可能持有缓存条目，成功摘除后按类型补删。
    let cached_parent = match mru_hit {
        Some((_, inode)) => Some(inode),
        None => crate::fs::dentry_cache_parent_get(parent_path),
    };
    let parent_lookup = match cached_parent {
        Some(p) => Ok(p),
        None => crate::fs::lookup(parent_path),
    };
    match parent_lookup {
        Ok(parent_inode) => {
            if let Some(parent_dir) = parent_inode.as_dir() {
                // Linux may_delete(2)：unlink(2)/unlinkat(no flag) 先校验父目录
                // 写/搜索权限(EACCES)，再拒绝删除目录(EISDIR)。unlink() 在 glibc/musl
                // 下走 unlinkat(AT_FDCWD,path,0)，故权限/EISDIR 校验必须在此分支。
                // （unlink08 unwritable/unsearchable-dir 与 directory 子项）
                const FS_IMMUTABLE_FL: u32 = 0x10;
                const FS_APPEND_FL: u32 = 0x20;
                if euid == 0 {
                    // root(euid==0) 凭 CAP_DAC_OVERRIDE 对父目录恒有写/搜索权，
                    // sticky 也免检；仅需 immutable/append iflags 检查（对 root 同样
                    // 生效）。热路径：unlink_take 单次目录项探测完成
                    // 「查 victim + 拒删目录 + 摘除」；victim iflags 事后校验，
                    // 极罕见的 immutable/append 命中时 link 回插（nlink 净不变）。
                    let piflags = parent_inode.get_inode_flags();
                    if piflags & (FS_IMMUTABLE_FL | FS_APPEND_FL) != 0 {
                        return -1; // -EPERM
                    }
                    match parent_dir.unlink_take(file_name, true, FS_IMMUTABLE_FL | FS_APPEND_FL) {
                        Ok((victim, ft)) => {
                            if ft == crate::fs::FileType::Symlink {
                                crate::fs::dentry_cache_remove(&full_path);
                            }
                            if super::path::any_fsnotify_active() {
                                notify_inotify_delete(parent_path, &full_path, file_name, ft, victim.ino() as u64);
                            }
                            crate::fs::inode_reap_defer(victim);
                            0
                        }
                        Err(crate::fs::FsError::PermissionDenied) => -1, // -EPERM
                        Err(crate::fs::FsError::IsDirectory) => -21,     // -EISDIR
                        Err(crate::fs::FsError::NotFound) => {
                            if crate::net::socket::unlink_unix_pathname(&full_path) {
                                notify_inotify_delete(parent_path, &full_path, file_name, crate::fs::FileType::Socket, 0);
                                0
                            } else { -2 }
                        }
                        Err(_) => -5,
                    }
                } else {
                    // 非 root：完整 may_delete（sticky/写权限）须在摘除前校验，
                    // 保持原两段式（lookup + unlink）。
                    let victim = parent_dir.lookup(file_name).ok();
                    if let Some(ref victim) = victim {
                        if let Ok(vstat) = victim.stat() {
                            let viflags = victim.get_inode_flags();
                            if let Some(err) = may_delete_errno_with_flags(&parent_inode, &vstat, viflags) {
                                return err; // -EACCES / -EPERM
                            }
                        }
                        if victim.file_type() == crate::fs::FileType::Directory {
                            return -21; // -EISDIR
                        }
                    }
                    let removed_type = victim.as_ref().map(|v| v.file_type());
                    match parent_dir.unlink(file_name) {
                        Ok(_) => {
                            // 非目录末级中只有 symlink 可能持有 dcache 条目。
                            if removed_type == Some(crate::fs::FileType::Symlink) {
                                crate::fs::dentry_cache_remove(&full_path);
                            }
                            if let Some(file_type) = removed_type {
                                let vino = victim.as_ref().map(|v| v.ino() as u64).unwrap_or(0);
                                notify_inotify_delete(parent_path, &full_path, file_name, file_type, vino);
                            }
                            0
                        }
                        Err(crate::fs::FsError::NotFound) => {
                            if crate::net::socket::unlink_unix_pathname(&full_path) {
                                notify_inotify_delete(parent_path, &full_path, file_name, crate::fs::FileType::Socket, 0);
                                0
                            } else { -2 }
                        }
                        Err(crate::fs::FsError::IsDirectory) => -21,
                        Err(_) => -5,
                    }
                }
            } else {
                -20
            }
        }
        Err(_) => {
            if crate::net::socket::unlink_unix_pathname(&full_path) {
                crate::fs::dentry_cache_remove(&full_path);
                notify_inotify_delete(parent_path, &full_path, file_name, crate::fs::FileType::Socket, 0);
                0
            } else { -2 }
        }
    }
}
