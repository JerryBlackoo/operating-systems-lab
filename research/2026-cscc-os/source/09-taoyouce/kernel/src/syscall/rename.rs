//! Rename syscall: renameat2.
//!
//! Split from `path.rs` to align with Linux `fs/namei.c` rename operations.

use super::path::{
    notify_inotify_move, next_inotify_rename_cookie, notify_inotify_create,
    access_allowed, current_access_ids,
};

/// POSIX rename / renameat / renameat2 实现。
///
/// 此前的版本是 "create empty + unlink old" 拼凑，会丢失文件内容并触发
/// LTP rename01..14 大批 TIMEOUT；新的实现直接调用 VFS 的 `Directory::rename`
/// （同目录）或 `Directory::rename_to`（跨目录）trait 方法，保持 inode 不
/// 变、内容不丢、原子地移动 dirent。
///
/// `flags` 当前只处理 RENAME_NOREPLACE 与 0；RENAME_EXCHANGE / RENAME_WHITEOUT
/// 暂不支持。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_renameat2(
    olddirfd: isize,
    oldpath: *const u8,
    newdirfd: isize,
    newpath: *const u8,
    flags: u32,
) -> isize {
    // Linux: include/uapi/linux/fs.h
    const RENAME_NOREPLACE: u32 = 1 << 0;
    const RENAME_EXCHANGE:  u32 = 1 << 1;
    const RENAME_WHITEOUT:  u32 = 1 << 2;
    const KNOWN: u32 = RENAME_NOREPLACE | RENAME_EXCHANGE | RENAME_WHITEOUT;
    if flags & !KNOWN != 0 {
        return -22;
    }
    // EXCHANGE is mutually exclusive with NOREPLACE/WHITEOUT (per renameat2(2)).
    if flags & RENAME_EXCHANGE != 0 && flags & (RENAME_NOREPLACE | RENAME_WHITEOUT) != 0 {
        return -22;
    }

    let old = match super::user::read_path(oldpath) {
        Ok(s) => s,
        Err(e) => return e, // -EFAULT / -ENAMETOOLONG
    };
    let new_str = match super::user::read_path(newpath) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let full_old = match super::resolve_at_path(olddirfd, &old) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let full_new = match super::resolve_at_path(newdirfd, &new_str) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // 拆分 parent + name
    let (old_parent_path, old_name) = match full_old.rfind('/') {
        Some(pos) if pos > 0 => (&full_old[..pos], &full_old[pos + 1..]),
        Some(_) => ("/", &full_old[1..]),
        None => return -22,
    };
    let (new_parent_path, new_name) = match full_new.rfind('/') {
        Some(pos) if pos > 0 => (&full_new[..pos], &full_new[pos + 1..]),
        Some(_) => ("/", &full_new[1..]),
        None => return -22,
    };
    if old_name.is_empty() || new_name.is_empty()
        || old_name == "." || old_name == ".."
        || new_name == "." || new_name == ".."
    {
        return -22;
    }

    // Linux mnt_want_write: renaming on a read-only mount returns EROFS.
    // Check both old and new paths (rename11 EROFS subcase, renameat01 test 7).
    if crate::fs::is_readonly_path(&full_old) || crate::fs::is_readonly_path(&full_new) {
        return -30; // -EROFS
    }

    let old_type = match crate::fs::lookup(&full_old) {
        Ok(inode) => inode.file_type(),
        Err(e) => return super::fd::fs_error_to_errno(e),
    };

    // RENAME_NOREPLACE: 当 dst 已存在时返回 -EEXIST
    let dst_type = crate::fs::lookup(&full_new).ok().map(|inode| inode.file_type());
    let dst_exists = dst_type.is_some();
    if flags & RENAME_NOREPLACE != 0 && dst_exists {
        return -17; // -EEXIST
    }

    // src 与 dst 是同一路径的同一对象：POSIX 要求静默成功
    if full_old == full_new {
        return 0;
    }

    // 普通 rename（非 EXCHANGE）的 Linux 类型/包含约束（vfs_rename / may_delete）：
    // - 把目录移进自身子树 → EINVAL；
    // - rename(dir, 非目录) → ENOTDIR；
    // - rename(非目录, dir) → EISDIR。
    // EXCHANGE 允许两端类型不同，故不在此判定。
    if flags & RENAME_EXCHANGE == 0 {
        let old_is_dir = old_type == crate::fs::FileType::Directory;
        if old_is_dir {
            let prefix = alloc::format!("{}/", full_old);
            if full_new.starts_with(&prefix) {
                return -22; // -EINVAL: dst 在 src 子树内
            }
        }
        if let Some(dt) = dst_type {
            let dst_is_dir = dt == crate::fs::FileType::Directory;
            if old_is_dir && !dst_is_dir {
                return -20; // -ENOTDIR
            }
            if !old_is_dir && dst_is_dir {
                return -21; // -EISDIR
            }
        }
    }

    // Linux may_delete(old_parent) / may_create(new_parent): rename requires
    // write + search permission on both the source and destination parent
    // directories. A non-root user lacking these gets EACCES (root bypasses
    // via access_allowed). Checked before the rename is attempted.
    {
        const W_OK: u32 = 2;
        const X_OK: u32 = 1;
        let (euid, egid) = current_access_ids(true);
        for pdir in [old_parent_path, new_parent_path] {
            if let Ok(pinode) = crate::fs::lookup(pdir) {
                if let Ok(pstat) = pinode.stat() {
                    if !access_allowed(&pstat, euid, egid, W_OK | X_OK) {
                        return -13; // -EACCES
                    }
                }
            }
        }
    }

    let old_parent = match crate::fs::lookup(old_parent_path) {
        Ok(inode) => inode,
        Err(_) => return -2,
    };
    let old_dir = match old_parent.as_dir() {
        Some(dir) => dir,
        None => return -20,
    };

    // RENAME_EXCHANGE: 要求 dst 必须存在，原子交换两 dirent 指向的 inode。
    if flags & RENAME_EXCHANGE != 0 {
        if !dst_exists {
            return -2;
        }
        let rc = if old_parent_path == new_parent_path {
            match old_dir.rename_exchange(old_name, new_name) {
                Ok(()) => 0,
                Err(crate::fs::FsError::NotFound) => -2,
                Err(crate::fs::FsError::InvalidArgument) => -22,
                Err(crate::fs::FsError::PermissionDenied) => -18, // EXDEV
                Err(_) => -5,
            }
        } else {
            let new_parent = match crate::fs::lookup(new_parent_path) {
                Ok(inode) => inode,
                Err(_) => return -2,
            };
            let new_dir = match new_parent.as_dir() {
                Some(d) => d,
                None => return -20,
            };
            match old_dir.rename_exchange_to(old_name, new_dir, new_name) {
                Ok(()) => 0,
                Err(crate::fs::FsError::NotFound) => -2,
                Err(crate::fs::FsError::InvalidArgument) => -22,
                Err(crate::fs::FsError::PermissionDenied) => -18, // EXDEV
                Err(_) => -5,
            }
        };
        if rc == 0 {
            crate::fs::dentry_cache_remove_tree(&full_old);
            crate::fs::dentry_cache_remove_tree(&full_new);
            // EXCHANGE 两端都可能是目录：各自方向上让 cwd 跟随。
            if old_type == crate::fs::FileType::Directory {
                crate::sched::process::rewrite_cwd_after_dir_rename(&full_old, &full_new);
            }
            if dst_type == Some(crate::fs::FileType::Directory) {
                crate::sched::process::rewrite_cwd_after_dir_rename(&full_new, &full_old);
            }
            notify_inotify_move(
                old_parent_path,
                &full_old,
                old_name,
                old_type,
                new_parent_path,
                new_name,
                next_inotify_rename_cookie(),
            );
            if let Some(new_type) = dst_type {
                notify_inotify_move(
                    new_parent_path,
                    &full_new,
                    new_name,
                    new_type,
                    old_parent_path,
                    old_name,
                    next_inotify_rename_cookie(),
                );
            }
            // EXCHANGE swaps two subtrees: re-key watches on each side to the
            // other via a sentinel temp prefix so the two moves don't cancel.
            let tmp = "\u{1}__inotify_exchange_tmp__";
            crate::fs::inotify::INOTIFY_REGISTRY.rename_path(&full_old, tmp);
            crate::fs::inotify::INOTIFY_REGISTRY.rename_path(&full_new, &full_old);
            crate::fs::inotify::INOTIFY_REGISTRY.rename_path(tmp, &full_new);
        }
        return rc;
    }

    // RENAME_WHITEOUT: 等价于 (a) 普通 rename src->dst 后
    // (b) 在 src 位置放一个 char-dev(0,0) whiteout entry。
    if flags & RENAME_WHITEOUT != 0 {
        // 先执行 rename
        let rename_rc: isize = if old_parent_path == new_parent_path {
            match old_dir.rename(old_name, new_name) {
                Ok(()) => 0,
                Err(crate::fs::FsError::NotFound) => -2,
                Err(crate::fs::FsError::AlreadyExists) => -17,
                Err(crate::fs::FsError::NotEmpty) => -39,
                Err(crate::fs::FsError::InvalidArgument) => -22,
                Err(crate::fs::FsError::PermissionDenied) => -13,
                Err(crate::fs::FsError::NotDirectory) => -20,
                Err(_) => -5,
            }
        } else {
            let new_parent = match crate::fs::lookup(new_parent_path) {
                Ok(inode) => inode,
                Err(_) => return -2,
            };
            let new_dir = match new_parent.as_dir() {
                Some(d) => d,
                None => return -20,
            };
            match old_dir.rename_to(old_name, new_dir, new_name) {
                Ok(()) => 0,
                Err(crate::fs::FsError::NotFound) => -2,
                Err(crate::fs::FsError::AlreadyExists) => -17,
                Err(crate::fs::FsError::NotEmpty) => -39,
                Err(crate::fs::FsError::InvalidArgument) => -22,
                Err(crate::fs::FsError::PermissionDenied) => -18,
                Err(crate::fs::FsError::NotDirectory) => -20,
                Err(_) => -5,
            }
        };
        if rename_rc != 0 {
            return rename_rc;
        }
        let whiteout_rc = match old_dir.whiteout(old_name) {
            Ok(()) => 0,
            Err(crate::fs::FsError::PermissionDenied) => -1, // EPERM
            Err(crate::fs::FsError::AlreadyExists) => -17,
            Err(_) => -5,
        };
        if whiteout_rc == 0 {
            crate::fs::dentry_cache_remove_tree(&full_old);
            crate::fs::dentry_cache_remove_tree(&full_new);
            if old_type == crate::fs::FileType::Directory {
                crate::sched::process::rewrite_cwd_after_dir_rename(&full_old, &full_new);
            }
            notify_inotify_move(
                old_parent_path,
                &full_old,
                old_name,
                old_type,
                new_parent_path,
                new_name,
                next_inotify_rename_cookie(),
            );
            crate::fs::inotify::INOTIFY_REGISTRY.rename_path(&full_old, &full_new);
            notify_inotify_create(old_parent_path, old_name, crate::fs::FileType::CharDevice);
        }
        return whiteout_rc;
    }

    // 同 parent 路径快路径：直接 same-dir rename
    if old_parent_path == new_parent_path {
        let rc = match old_dir.rename(old_name, new_name) {
            Ok(()) => 0,
            Err(crate::fs::FsError::NotFound) => -2,
            Err(crate::fs::FsError::AlreadyExists) => -17,
            Err(crate::fs::FsError::NotEmpty) => -39, // -ENOTEMPTY
            Err(crate::fs::FsError::InvalidArgument) => -22,
            Err(crate::fs::FsError::PermissionDenied) => -13,
            Err(crate::fs::FsError::NotDirectory) => -20,
            Err(_) => -5,
        };
        if rc == 0 {
            crate::fs::dentry_cache_remove_tree(&full_old);
            crate::fs::dentry_cache_remove_tree(&full_new);
            if old_type == crate::fs::FileType::Directory {
                crate::sched::process::rewrite_cwd_after_dir_rename(&full_old, &full_new);
            }
            notify_inotify_move(
                old_parent_path,
                &full_old,
                old_name,
                old_type,
                new_parent_path,
                new_name,
                next_inotify_rename_cookie(),
            );
            crate::fs::inotify::INOTIFY_REGISTRY.rename_path(&full_old, &full_new);
        }
        return rc;
    }

    // 跨目录路径：拿到 new_dir trait 引用调用 rename_to
    let new_parent = match crate::fs::lookup(new_parent_path) {
        Ok(inode) => inode,
        Err(_) => return -2,
    };
    let new_dir = match new_parent.as_dir() {
        Some(dir) => dir,
        None => return -20,
    };
    let rc = match old_dir.rename_to(old_name, new_dir, new_name) {
        Ok(()) => 0,
        Err(crate::fs::FsError::NotFound) => -2,
        Err(crate::fs::FsError::AlreadyExists) => -17,
        Err(crate::fs::FsError::NotEmpty) => -39,
        Err(crate::fs::FsError::InvalidArgument) => -22,
        // 跨 fs 类型当前回退为 EXDEV，让 libc 转 copy+unlink 兜底
        Err(crate::fs::FsError::PermissionDenied) => -18, // -EXDEV
        Err(crate::fs::FsError::NotDirectory) => -20,
        Err(_) => -5,
    };
    if rc == 0 {
        crate::fs::dentry_cache_remove_tree(&full_old);
        crate::fs::dentry_cache_remove_tree(&full_new);
        if old_type == crate::fs::FileType::Directory {
            crate::sched::process::rewrite_cwd_after_dir_rename(&full_old, &full_new);
        }
        notify_inotify_move(
            old_parent_path,
            &full_old,
            old_name,
            old_type,
            new_parent_path,
            new_name,
            next_inotify_rename_cookie(),
        );
        crate::fs::inotify::INOTIFY_REGISTRY.rename_path(&full_old, &full_new);
    }
    rc
}
