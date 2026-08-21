use alloc::string::String;

use crate::fs::fd as fd_table;

/// Resolve symlinks in a path to produce a canonical path for /proc/self/fd/N.
/// Walks each component; if a component is a symlink, reads its target and
/// substitutes. Limits to 40 levels to avoid infinite loops.
pub(crate) fn resolve_symlinks_for_path(path: &str) -> String {
    // 目录前缀规范缓存快路径：目录的 symlink 展开形式只随目录结构变化
    //（rename/rmdir/mount 走 dentry 缓存同款失效），文件级 create/unlink
    // 不使其失效。命中后只需对末级分量做一次 no-follow 探测。
    if let Some((dir, name)) = path.rsplit_once('/') {
        if !dir.is_empty() && !name.is_empty() && name != "." && name != ".." {
            if let Some(cdir) = crate::fs::canonical_dir_get(dir) {
                let mut joined = String::with_capacity(cdir.len() + 1 + name.len());
                if cdir != "/" {
                    joined.push_str(&cdir);
                }
                joined.push('/');
                joined.push_str(name);
                let terminal_symlink = crate::fs::lookup_no_follow(&joined)
                    .map(|inode| inode.file_type() == crate::fs::FileType::Symlink)
                    .unwrap_or(false);
                if !terminal_symlink {
                    return joined;
                }
                // 末级是 symlink：罕见，退回逐分量慢路径处理替换逻辑。
            }
        }
    }
    let mut result = String::new();
    let mut remaining_depth = 40usize;
    let mut terminal_was_symlink = false;
    for component in path.split('/') {
        terminal_was_symlink = false;
        if component.is_empty() {
            if result.is_empty() { result.push('/'); }
            continue;
        }
        if component == "." { continue; }
        if component == ".." {
            if let Some(pos) = result[1..].rfind('/') {
                result.truncate(pos + 1);
            } else {
                result = String::from("/");
            }
            continue;
        }
        if !result.ends_with('/') { result.push('/'); }
        result.push_str(component);
        // Check if this component is a symlink
        if remaining_depth == 0 { break; }
        if let Ok(inode) = crate::fs::lookup_no_follow(&result) {
            if inode.file_type() == crate::fs::FileType::Symlink {
                terminal_was_symlink = true;
                remaining_depth -= 1;
                if let Some(file) = inode.as_file() {
                    let mut buf = alloc::vec![0u8; 4096];
                    if let Ok(n) = file.read(0, &mut buf) {
                        let target = core::str::from_utf8(&buf[..n]).unwrap_or("");
                        if target.starts_with('/') {
                            // Absolute symlink target
                            result = String::from(target);
                        } else {
                            // Relative symlink target: replace last component
                            if let Some(pos) = result.rfind('/') {
                                result.truncate(pos + 1);
                            }
                            result.push_str(target);
                        }
                        // Normalize the result (remove any // or trailing /)
                        // by recursively resolving... but we just continue the walk
                    }
                }
            }
        }
    }
    if result.is_empty() { result.push('/'); }
    // 慢路径收尾：末级非 symlink 时，把「原目录前缀 → 规范目录前缀」写入
    // 缓存，后续同目录的 open 全部走上面的快路径。
    if !terminal_was_symlink {
        if let (Some((dir, name)), Some(rpos)) = (path.rsplit_once('/'), result.rfind('/')) {
            if !dir.is_empty() && !name.is_empty() && name != "." && name != ".."
                && &result[rpos + 1..] == name
            {
                let cdir = if rpos == 0 { "/" } else { &result[..rpos] };
                crate::fs::canonical_dir_insert(dir, cdir);
            }
        }
    }
    result
}

fn inotify_dir_bit(file_type: crate::fs::FileType) -> u32 {
    if matches!(file_type, crate::fs::FileType::Directory) {
        crate::fs::inotify::IN_ISDIR
    } else {
        0
    }
}

fn notify_inotify_create(path: &str, file_type: crate::fs::FileType) {
    match path.rfind('/') {
        Some(pos) if pos > 0 && pos + 1 < path.len() => {
            crate::fs::inotify::notify_path(
                &path[..pos],
                crate::fs::inotify::IN_CREATE | inotify_dir_bit(file_type),
                &path[pos + 1..],
            );
        }
        Some(0) if path.len() > 1 => {
            crate::fs::inotify::notify_path(
                "/",
                crate::fs::inotify::IN_CREATE | inotify_dir_bit(file_type),
                &path[1..],
            );
        }
        _ => {}
    }
}

/// 打开文件 — Linux 标准 openat(2)
///
/// openat(dirfd, pathname, flags, mode)
/// - dirfd: 目录文件描述符（AT_FDCWD=-100 表示当前工作目录）
/// - pathname: null-terminated C 字符串
/// - flags: O_RDONLY(0), O_WRONLY(1), O_RDWR(2), O_CREAT(0x40), O_TRUNC(0x200), O_APPEND(0x400), O_DIRECTORY(0x10000)
/// - mode: 创建文件时的权限位
#[allow(dead_code)]
pub(in crate::syscall) fn sys_openat(dirfd: isize, path_ptr: *const u8, flags: u32, mode: u32) -> isize {
    use crate::sched::process::PROCESS_TABLE;
    use alloc::sync::Arc;

    const O_CREAT:     u32 = 0x40;
    const O_EXCL:      u32 = 0x80;
    const O_NOCTTY:    u32 = 0x100;
    const O_TRUNC:     u32 = 0x200;
    const O_APPEND:    u32 = 0x400;
    const O_NONBLOCK:  u32 = 0x800;
    const O_DSYNC:     u32 = 0x1000;
    const O_DIRECT:    u32 = 0x4000;     // bypass page cache; we never had one
    const O_LARGEFILE: u32 = 0x8000;
    const O_DIRECTORY: u32 = 0x10000;    // 0x200000 on some arches, RV64 = 0x10000
    const O_NOFOLLOW:  u32 = 0x20000;
    const O_NOATIME:   u32 = 0o1000000;
    const O_CLOEXEC:   u32 = 0o2000000;
    const O_SYNC:      u32 = 0o4010000;
    const O_PATH:      u32 = 0o10000000; // 0o10_000_000 = 0x200000
    const O_TMPFILE:   u32 = 0o20200000; // Linux: O_TMPFILE = __O_TMPFILE | O_DIRECTORY
    // O_APPEND / O_NONBLOCK are honored by FileDescriptor::write and
    // sys_read/sys_write WouldBlock branch (Slice 2.x T2-9 upgrade).
    // O_NOATIME is honored implicitly: ramfs/devfs do not update atime.
    let _ = (O_DSYNC, O_DIRECT, O_LARGEFILE, O_NOCTTY, O_NOATIME, O_SYNC);

    // 读取 null-terminated C 字符串（区分 EFAULT 与 ENAMETOOLONG，匹配
    // Linux getname()，creat06）。与凭证读取合并为一次 PROCESS_TABLE
    // 锁，路径读进栈缓冲（对标 Linux getname 定长槽）：常见路径全程
    // 零堆分配（与 unlinkat 同款 b117 单遍化路径入口）。
    let mut path_buf = [core::mem::MaybeUninit::<u8>::uninit(); 256];
    let (rp, cred_euid, cred_egid, root_is_plain) =
        match super::user::read_path_and_cred(path_ptr, &mut path_buf) {
            Ok(t) => t,
            Err(e) => return e,
        };
    let path = rp.as_str();
    if path.is_empty() {
        return -22; // -EINVAL
    }

    // 父目录 MRU 前缀直配：整条路径一次前缀比较即完成规范性判定 +
    // 父子切分 + 父目录解析（键即已验证的规范绝对父路径）。命中免去
    // resolve_path 规范化扫描 / rfind / lookup_parent 哈希查找。
    let mru_hit: Option<(usize, Arc<dyn crate::fs::Inode>)> =
        if root_is_plain && path.as_bytes().first() == Some(&b'/') {
            crate::fs::dentry_parent_mru_match(path)
        } else {
            None
        };
    // 解析路径（统一 dirfd-relative 路径）：规范绝对路径借用快路径
    // 零分配；非规范/相对/chroot 路径才落 owned 慢路径。
    let full_owned: String;
    let full_path: &str = if mru_hit.is_some() {
        path
    } else {
        match crate::syscall::path::resolve_path_borrowed_ok(path, root_is_plain) {
            Err(e) => return e,
            Ok(true) => path,
            Ok(false) => {
                let fs = match super::user::current_fs() {
                    Ok(f) => f,
                    Err(e) => return e,
                };
                let owned = String::from(path);
                full_owned = match crate::syscall::path::resolve_at_path_owned_fs(
                    dirfd, owned, root_is_plain, &fs,
                ) {
                    Ok(p) => p,
                    Err(e) => return e,
                };
                &full_owned
            }
        }
    };

    // Linux namei: enforce search (MAY_EXEC) permission and component types on
    // every ancestor directory of the target. A non-searchable ancestor is
    // EACCES (creat04, creat06 EACCES); an ancestor that is a file is ENOTDIR
    // (creat06 ENOTDIR). ENOENT is deferred to the normal lookup below so the
    // /lib -> /musl/lib fallback and O_CREAT-in-existing-dir still work.
    // root（euid==0）恒有 search 权限（同 check_prefix_search_effective），
    // 凭证已随路径一次锁取出，免再锁 PROCESS_TABLE。
    if cred_euid != 0 {
        match crate::syscall::path::access_check_search(full_path, cred_euid, cred_egid) {
            Ok(()) => {}
            Err(-2) => {} // -ENOENT: defer to lookup / O_CREAT / fallback
            Err(errno) => return errno,
        }
    }

    // Landlock access check: determine required access from open flags
    {
        use crate::security::landlock;
        let access_mode = flags & 0x3; // O_RDONLY=0, O_WRONLY=1, O_RDWR=2
        let mut required_access = 0u64;
        if access_mode == 0 || access_mode == 2 {
            required_access |= landlock::LANDLOCK_ACCESS_FS_READ_FILE;
        }
        if access_mode == 1 || access_mode == 2 {
            required_access |= landlock::LANDLOCK_ACCESS_FS_WRITE_FILE;
        }
        if flags & O_CREAT != 0 {
            required_access |= landlock::LANDLOCK_ACCESS_FS_MAKE_REG;
        }
        if required_access != 0 && !landlock::check_access(&full_path, required_access) {
            return -13; // -EACCES
        }
    }

    // /dev/ptmx: allocate a new PTY pair and return the master fd.
    // Each open() of ptmx creates a distinct PTY, so we must intercept here
    // before the normal lookup (which would return the shared device inode).
    if full_path == "/dev/ptmx" {
        match crate::fs::devfs::open_ptmx() {
            Ok(master_inode) => {
                let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
                    master_inode,
                    flags,
                    Some(alloc::string::String::from("/dev/ptmx")),
                ));
                let mut table = PROCESS_TABLE.lock();
                if let Some(table) = table.as_mut() {
                    if let Some(current_pid) = table.current {
                        if let Some(process) = table.get_fd_process_mut(current_pid) {
                            return match fd_table::alloc(process, fd_obj, 0, true) {
                                Ok(fd) => {
                                    if flags & O_CLOEXEC != 0 {
                                        let _ = fd_table::set_cloexec(process, fd, true);
                                    }
                                    fd as isize
                                }
                                Err(errno) => errno,
                            };
                        }
                    }
                }
                return -1;
            }
            Err(_) => return -28, // -ENOSPC (no free PTY slots)
        }
    }

    // /dev/pts/N: per-open slave file object so the master observes hangup
    // (EOF/POLLHUP) when the last slave open closes, per Linux tty semantics.
    if let Some(rest) = full_path.strip_prefix("/dev/pts/") {
        if let Ok(index) = rest.parse::<usize>() {
            if crate::fs::devfs::pty_slave_is_locked(index) {
                return -5; // -EIO (slave is locked)
            }
            match crate::fs::devfs::open_pty_slave(index) {
                Ok(slave_inode) => {
                    let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
                        slave_inode,
                        flags,
                        Some(alloc::string::String::from(&*full_path)),
                    ));
                    let mut table = PROCESS_TABLE.lock();
                    if let Some(table) = table.as_mut() {
                        if let Some(current_pid) = table.current {
                            if let Some(process) = table.get_fd_process_mut(current_pid) {
                                return match fd_table::alloc(process, fd_obj, 0, true) {
                                    Ok(fd) => {
                                        if flags & O_CLOEXEC != 0 {
                                            let _ = fd_table::set_cloexec(process, fd, true);
                                        }
                                        fd as isize
                                    }
                                    Err(errno) => errno,
                                };
                            }
                        }
                    }
                    return -1;
                }
                Err(_) => return -5, // -EIO (pty gone: master closed)
            }
        }
    }

    // /proc/self/fd/N or /proc/<pid>/fd/N: open the underlying fd's file
    // (Linux procfs magic symlink — dup-like semantics).
    if let Some(inode) = crate::syscall::path::resolve_proc_fd_inode_pub(&full_path) {
        // O_TRUNC: truncate the file to 0 (if not sealed)
        if flags & O_TRUNC != 0 {
            // F_SEAL_SHRINK: reject O_TRUNC with EPERM
            if let Some(shared_seals) = inode.shared_seals() {
                let cur = shared_seals.load(core::sync::atomic::Ordering::Acquire);
                if cur & 0x0002 != 0 { // F_SEAL_SHRINK
                    return -1; // -EPERM
                }
            }
            if let Some(file) = inode.as_file() {
                let _ = file.truncate(0);
            }
        }
        let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
            inode,
            flags,
            Some(String::from(full_path)),
        ));
        let mut table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_mut() {
            if let Some(current_pid) = table.current {
                if let Some(process) = table.get_fd_process_mut(current_pid) {
                    return match fd_table::alloc(process, fd_obj, 0, true) {
                        Ok(fd) => {
                            if flags & O_CLOEXEC != 0 {
                                if let Err(errno) = fd_table::set_cloexec(process, fd, true) {
                                    return errno;
                                }
                            }
                            fd as isize
                        }
                        Err(errno) => errno,
                    };
                }
            }
        }
        return -1;
    }

    // /proc/<pid|self>/mem: 进程地址空间字节流（thp04 经它读写堆区触发 THP
    // 拆分）。procfs 不把它作为常规 inode 暴露, 这里在 open 阶段直接构造一个
    // 访问当前地址空间的 ProcMemInode（仅 self / 同 tgid 线程可访问）。
    if let Some(target_pid) = crate::syscall::path::parse_proc_mem_path(&full_path) {
        let inode: Arc<dyn crate::fs::Inode> =
            Arc::new(crate::fs::proc::mem::ProcMemInode::new(target_pid));
        let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
            inode,
            flags,
            Some(String::from(full_path)),
        ));
        let mut table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_mut() {
            if let Some(current_pid) = table.current {
                if let Some(process) = table.get_fd_process_mut(current_pid) {
                    return match fd_table::alloc(process, fd_obj, 0, true) {
                        Ok(fd) => {
                            if flags & O_CLOEXEC != 0 {
                                if let Err(errno) = fd_table::set_cloexec(process, fd, true) {
                                    return errno;
                                }
                            }
                            fd as isize
                        }
                        Err(errno) => errno,
                    };
                }
            }
        }
        return -1;
    }

    // O_TMPFILE: pathname must refer to a *directory*; the kernel creates a
    // brand-new unnamed inode in it. Userspace can later call
    // linkat(AT_FDCWD, "/proc/self/fd/N", dst_dirfd, dst, AT_SYMLINK_FOLLOW)
    // to give it a name. We implement this by creating a temp entry with a
    // unique synthetic name, capturing the Arc, then immediately unlinking
    // the dirent. The fd holds the only reference; when it is closed the
    // inode is reclaimed via Arc-drop.
    if flags & O_TMPFILE == O_TMPFILE {
        let dir_inode = match crate::fs::lookup(&full_path) {
            Ok(i) => i,
            Err(_) => return -2,
        };
        let dir = match dir_inode.as_dir() {
            Some(d) => d,
            None => return -20,
        };
        use core::sync::atomic::{AtomicUsize, Ordering};
        static TMP_SEQ: AtomicUsize = AtomicUsize::new(1);
        let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let synth = alloc::format!(".tmpfile.{}.{}", seq, dir_inode.ino());
        let new_inode = match dir.create(&synth, crate::fs::FileType::Regular) {
            Ok(i) => i,
            Err(crate::fs::FsError::PermissionDenied) => return -13,
            Err(_) => return -5,
        };
        let mut create_mode = (mode & 0o7777) & !crate::syscall::compat::current_umask();
        // O_TMPFILE inode created in an S_ISGID directory inherits the parent
        // GID (handled in dir.create). Mirror creat09/openat04: strip the
        // requested S_ISGID unless the caller has CAP_FSETID or its egid is the
        // inherited group, matching Linux inode_init_owner().
        {
            const S_ISGID: u32 = 0o2000;
            const CAP_FSETID: u64 = 1u64 << 4;
            if create_mode & S_ISGID != 0 {
                if let Ok(parent_stat) = dir_inode.stat() {
                    if parent_stat.mode.0 & S_ISGID != 0 {
                        let (egid, caps) = {
                            let table = PROCESS_TABLE.lock();
                            table
                                .as_ref()
                                .and_then(|t| t.current.and_then(|pid| t.get(pid)))
                                .map(|p| (p.egid, p.cap_effective))
                                .unwrap_or((0, 0))
                        };
                        if (caps & CAP_FSETID) == 0 && egid != parent_stat.gid {
                            create_mode &= !S_ISGID;
                        }
                    }
                }
            }
        }
        if let Err(err) = new_inode.set_mode(create_mode) {
            return match err {
                crate::fs::FsError::PermissionDenied => -13,
                crate::fs::FsError::InvalidArgument => -22,
                _ => -5,
            };
        }
        // Detach the dirent so the file is anonymous; the Arc returned via
        // `new_inode` (and held by the FileDescriptor below) keeps the inode
        // alive even though it now has no name.
        let _ = dir.unlink(&synth);
        let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
            new_inode,
            flags,
            None, // anonymous — no path
        ));
        let mut table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_mut() {
            if let Some(current_pid) = table.current {
                if let Some(process) = table.get_fd_process_mut(current_pid) {
                    return match fd_table::alloc(process, fd_obj, 0, true) {
                        Ok(fd) => {
                            if flags & O_CLOEXEC != 0 {
                                if let Err(errno) = fd_table::set_cloexec(process, fd, true) {
                                    return errno;
                                }
                            }
                            fd as isize
                        }
                        Err(errno) => errno,
                    };
                }
            }
        }
        return -22;
    }

    let mut created_file = false;

    // 查找文件。O_NOFOLLOW 必须作用于最终 component：中间 symlink 仍按
    // Linux namei 规则解析，但 basename 是 symlink 时不能被普通 open()
    // 悄悄跟随。MS_NOSYMFOLLOW (mount07): mount-level prohibition of final
    // symlink traversal — equivalent to implicit O_NOFOLLOW. O_PATH opens
    // always get the entry itself and are not affected by NOSYMFOLLOW.
    let no_follow_final = flags & O_NOFOLLOW != 0
        || (flags & O_PATH == 0 && crate::fs::is_nosymfollow_path(&full_path));
    // O_CREAT 热路径（lmbench lat_fs 海量小文件创建）：新文件名对 full_path 的
    // dentry cache 必然 miss，直接 lookup 要走一次注定失败的完整 VFS walk
    //（mount 表扫描 + 逐分量目录查找 + normalize）。先取 parent（通常 dentry
    // cache O(1) 命中）再在父目录内单次 dirent 查找即可分辨"存在/仅缺末端"。
    // 末级是 symlink 或 parent 解析失败时回退慢路径，errno/follow 语义不变。
    let mut fast_lookup: Option<Result<Arc<dyn crate::fs::Inode>, crate::fs::FsError>> = None;
    // NotFound 快路径命中时缓存已解析的父目录，供下方 O_CREAT 创建分支
    // 复用，免重复一次 lookup(parent_path)。
    let mut fast_parent: Option<Arc<dyn crate::fs::Inode>> = None;
    // 末级已被证实非 symlink（fast 单级查找命中 / 新建文件）时，下方
    // stored_path 计算可免一次 lookup_no_follow 终判。
    let mut terminal_known_nonsymlink = false;
    if flags & O_CREAT != 0 && !no_follow_final {
        // MRU 父目录直配命中时免 lookup_parent（哈希查找 + 路径切分）。
        let mru_parent: Option<(Arc<dyn crate::fs::Inode>, &str)> = match &mru_hit {
            Some((pos, inode)) => Some((inode.clone(), &full_path[*pos + 1..])),
            None => crate::fs::lookup_parent(full_path)
                .ok()
                .map(|(i, n)| (i, n)),
        };
        if let Some((parent_inode, file_name)) = mru_parent {
            if let Some(parent_dir) = parent_inode.as_dir() {
                match parent_dir.lookup(file_name) {
                    Ok(child) if child.file_type() != crate::fs::FileType::Symlink => {
                        fast_lookup = Some(Ok(child));
                        terminal_known_nonsymlink = true;
                    }
                    Err(crate::fs::FsError::NotFound) => {
                        fast_lookup = Some(Err(crate::fs::FsError::NotFound));
                        fast_parent = Some(parent_inode);
                    }
                    _ => {} // symlink / 其它错误 → 慢路径
                }
            }
        }
    }
    let lookup_result = match fast_lookup {
        Some(r) => r,
        None if no_follow_final => crate::fs::lookup_no_follow(full_path),
        None => crate::fs::lookup(full_path),
    };
    let inode = match lookup_result {
        Ok(inode) => {
            // O_CREAT | O_EXCL: file must not exist; if it does, return -EEXIST
            // (LTP openat02_excl / open_create_o_excl_efault).
            if flags & O_CREAT != 0 && flags & O_EXCL != 0 {
                return -17; // -EEXIST
            }
            // O_NOFOLLOW: refuse to follow a terminal symlink. Slice 2.x T6
            // added Vfs lookup auto-follow of intermediate symlinks, but the
            // final component must still trigger -ELOOP when O_NOFOLLOW is set.
            // MS_NOSYMFOLLOW: mount-level prohibition of symlink traversal
            // (mount07). If the mount has NOSYMFOLLOW, any open of a symlink
            // returns -ELOOP regardless of O_NOFOLLOW flag.
            if inode.file_type() == crate::fs::FileType::Symlink
                && flags & O_PATH == 0
                && (flags & O_NOFOLLOW != 0 || crate::fs::is_nosymfollow_path(&full_path))
            {
                return -40; // -ELOOP
            }
            // 如果设置了 O_DIRECTORY，检查是否为目录
            if flags & O_DIRECTORY != 0 && inode.file_type() != crate::fs::FileType::Directory {
                return -20; // -ENOTDIR
            }
            // Opening a directory with write intent is EISDIR: O_WRONLY/O_RDWR,
            // O_TRUNC, or O_CREAT all imply modification and are rejected; a
            // plain O_RDONLY open of a directory is allowed (open11, creat06).
            if inode.file_type() == crate::fs::FileType::Directory {
                const O_ACCMODE: u32 = 0o3;
                if (flags & O_ACCMODE) != 0 || (flags & O_CREAT) != 0 || (flags & O_TRUNC) != 0 {
                    return -21; // -EISDIR
                }
            }
            // may_open() access-mode permission for an existing file: O_RDONLY
            // needs R, O_WRONLY needs W, O_RDWR needs R|W (open08). O_PATH only
            // wants a reference fd and skips the read/write permission check.
            if flags & O_PATH == 0 {
                if let Ok(st) = inode.stat() {
                    if !crate::syscall::path::open_mode_allowed(&st, flags) {
                        return -13; // -EACCES
                    }
                    // MS_NODEV: deny opening device files on nodev mount (mount03).
                    if (st.file_type == crate::fs::FileType::BlockDevice
                        || st.file_type == crate::fs::FileType::CharDevice)
                        && crate::fs::is_nodev_path(&full_path)
                    {
                        return -13; // -EACCES
                    }
                    // EROFS: deny writing to files on a read-only mount
                    // (mount03). Linux checks this in may_open() before
                    // ETXTBSY for O_WRONLY/O_RDWR/O_TRUNC on any file type.
                    const O_ACCMODE_MASK: u32 = 0o3;
                    let acc = flags & O_ACCMODE_MASK;
                    if (acc == 1 || acc == 2 || flags & O_TRUNC != 0)
                        && crate::fs::is_readonly_path(&full_path)
                    {
                        return -30; // -EROFS
                    }
                    // ETXTBSY: deny writing to a currently-executing binary
                    // (creat07, execve04). Linux checks deny_write_access in
                    // may_open() for O_WRONLY/O_RDWR/O_TRUNC on regular files.
                    if (acc == 1 || acc == 2 || flags & O_TRUNC != 0)
                        && st.file_type == crate::fs::FileType::Regular
                        && crate::fs::is_write_denied(&full_path)
                    {
                        return -26; // -ETXTBSY
                    }
                }
            }
            // O_PATH: caller only wants the path-fd for fstatat / linkat /
            // etc., and will not read/write. We still allocate a normal fd
            // because our FileDescriptor has no per-fd "access denied" bit;
            // userspace is responsible for not touching it.
            let _ = O_PATH;
            inode
        }
        Err(crate::fs::FsError::NotFound) if flags & O_CREAT == 0 => {
            // 路径 fallback: /lib/X → /musl/lib/X (musl dlopen 搜索路径)
            if full_path.starts_with("/lib/") {
                let fallback = alloc::format!("/musl{}", &full_path);
                match crate::fs::lookup(&fallback) {
                    Ok(inode) => inode,
                    Err(_) => return -2, // -ENOENT
                }
            } else {
                return -2; // -ENOENT
            }
        }
        Err(crate::fs::FsError::NotFound) if flags & O_CREAT != 0 => {
            // 文件不存在且设置了 O_CREAT → 创建文件。先判只读挂载：在只读
            // 文件系统上创建新文件违反 EROFS（creat06）。namei 错误（ENOENT/
            // ENOTDIR/ELOOP/ENAMETOOLONG）已在上面 lookup 阶段返回，此处路径
            // 解析成功仅缺末端文件，正是 Linux 报 EROFS 的时机。
            if crate::fs::is_readonly_path(&full_path) {
                return -30; // -EROFS
            }
            // 文件不存在且设置了 O_CREAT → 创建文件（MRU 命中时复用已切分位置）
            let (parent_path, file_name) = match mru_hit.as_ref().map(|(pos, _)| *pos) {
                Some(pos) if pos > 0 => (&full_path[..pos], &full_path[pos + 1..]),
                Some(_) => ("/", &full_path[1..]),
                None => match full_path.rfind('/') {
                    Some(pos) if pos > 0 => (&full_path[..pos], &full_path[pos + 1..]),
                    Some(_) => ("/", &full_path[1..]),
                    None => return -2, // -ENOENT (no parent)
                },
            };
            if file_name.is_empty() {
                return -22;
            }
            let parent_lookup = match fast_parent {
                Some(p) => Ok(p),
                None => crate::fs::lookup(parent_path),
            };
            match parent_lookup {
                Ok(parent_inode) => {
                    if let Some(parent_dir) = parent_inode.as_dir() {
                        match parent_dir.create(file_name, crate::fs::FileType::Regular) {
                            Ok(new_inode) => {
                                created_file = true;
                                terminal_known_nonsymlink = true;
                                // 路径此前可能缓存了负向 dentry（ENOENT 探测）；
                                // 新建成功后必须失效，否则下一次路径 stat/open
                                // 会命中旧负向项。
                                crate::fs::dentry_cache_remove(&full_path);
                                // 新分配点顺带增量回收延迟析构的 victim inode
                                //（unlink 临界路径挪来的页帧归还在此摊销）。
                                crate::fs::inode_reap_drain_some(2);
                                // Clear stale xattr entries for this path
                                super::xattr::xattr_clear_path(&full_path);
                                let mut create_mode =
                                    (mode & 0o7777) & !crate::syscall::compat::current_umask();
                                // Linux setgid inheritance: if parent has S_ISGID, the new
                                // file inherits the parent GID. Strip S_ISGID from the new
                                // file unless the process has CAP_FSETID or belongs to the
                                // parent's group (creat09).
                                const S_ISGID: u32 = 0o2000;
                                const CAP_FSETID: u64 = 1u64 << 4;
                                if let Ok(parent_stat) = parent_inode.stat() {
                                    if parent_stat.mode.0 & S_ISGID != 0 {
                                        // Inherit parent GID
                                        let _ = new_inode.set_owner(None, Some(parent_stat.gid));
                                        // Strip S_ISGID unless process has CAP_FSETID or
                                        // is in parent's group
                                        let (egid, caps) = {
                                            let table = PROCESS_TABLE.lock();
                                            table.as_ref()
                                                .and_then(|t| t.current.and_then(|pid| t.get(pid)))
                                                .map(|p| (p.egid, p.cap_effective))
                                                .unwrap_or((0, u64::MAX))
                                        };
                                        if (caps & CAP_FSETID) == 0 && egid != parent_stat.gid {
                                            create_mode &= !S_ISGID;
                                        }
                                    }
                                }
                                if let Err(err) = new_inode.set_mode(create_mode) {
                                    return match err {
                                        crate::fs::FsError::PermissionDenied => -13,
                                        crate::fs::FsError::InvalidArgument => -22,
                                        _ => -5,
                                    };
                                }
                                // 新建文件不预插 dentry cache：后续访问几乎都经
                                // 返回的 fd（write/fstat）而非路径；lat_fs 类
                                // create+unlink 负载下预插 = 白付一次 String 克隆
                                // + hash insert + unlink 时的 remove。真有路径
                                // 访问时首次 lookup miss 会自行回填。
                                new_inode
                            }
                            Err(_) => return -5, // -EIO
                        }
                    } else {
                        return -20; // -ENOTDIR
                    }
                }
                Err(_) => return -2, // -ENOENT
            }
        }
        Err(crate::fs::FsError::TooManyLinks) => return -40, // -ELOOP
        Err(crate::fs::FsError::NotDirectory) => return -20, // -ENOTDIR
        Err(_) => {
            // 路径 fallback（同上）
            if full_path.starts_with("/lib/") {
                let fallback = alloc::format!("/musl{}", &full_path);
                match crate::fs::lookup(&fallback) {
                    Ok(inode) => inode,
                    Err(_) => return -2,
                }
            } else {
                return -2; // -ENOENT
            }
        }
    };

    // O_NOATIME requires the caller to own the file or hold CAP_FOWNER
    // (Linux do_dentry_open -> inode_owner_or_capable). An unprivileged caller
    // opening another user's file with O_NOATIME gets EPERM (open02). Skip when
    // we just created the file (the caller is by definition the owner).
    if flags & O_NOATIME != 0 && !created_file {
        const CAP_FOWNER: u64 = 1u64 << 3;
        let (euid, caps) = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table
                .as_ref()
                .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| (p.euid, p.cap_effective)))
                .unwrap_or((0, u64::MAX))
        };
        if let Ok(st) = inode.stat() {
            if euid != 0 && (caps & CAP_FOWNER) == 0 && euid != st.uid {
                return -1; // -EPERM
            }
        }
    }

    // Lease break: conflicting open must wait for the existing lease holder to
    // downgrade/release. Newly created files have no lease yet so the call is a
    // no-op; for existing files we honour the inode's lease record.
    {
        const O_TRUNC_BIT: u32 = 0x200;
        let is_trunc = flags & O_TRUNC_BIT != 0;
        let inode_id_for_lease = inode.ino();
        if let Err(errno) = crate::fs::file_lease::break_lease_blocking(
            inode_id_for_lease,
            flags & 0o3,
            is_trunc,
        ) {
            return -(errno as isize);
        }
    }

    // fanotify permission event：在文件被真正暴露给用户态之前同步阻塞，
    // 等待任何 FAN_OPEN_PERM 监听者放行（ALLOW/超时）或拒绝（DENY → -EPERM）。
    // FAN_OPEN_EXEC_PERM 仅在 FMODE_EXEC 打开（execve 路径，见 exec/image.rs）
    // 时触发，与 Linux 一致；普通 open 可执行文件不产生 exec perm 事件。
    // Linux 默认 monitor 未响应时按 ALLOW 处理。O_PATH 打开（FMODE_PATH，
    // Linux do_o_path 不走 do_dentry_open）不产生任何 fsnotify open 事件。
    if flags & O_PATH == 0 && crate::fs::fanotify::has_active_marks() {
        if let Err(errno) = crate::fs::fanotify::notify_perm_path(
            &full_path,
            crate::fs::fanotify::FAN_OPEN_PERM,
        ) {
            return errno as isize;
        }
    }

    // Named FIFO open: when the inode is a Pipe (FileType::Pipe) created by
    // mknod/mknodat, swap it for a NamedFifoRead/Write/Rdwr depending on access mode.
    let inode = if inode.file_type() == crate::fs::FileType::Pipe {
        const O_ACCMODE: u32 = 0o3;
        const O_RDONLY:  u32 = 0o0;
        const O_WRONLY:  u32 = 0o1;
        const O_RDWR:    u32 = 0o2;
        let access = (flags as u32) & O_ACCMODE;
        let fifo_state = crate::fs::pipe::get_or_create_fifo(inode.ino());
        let ino = inode.ino();
        if access == O_WRONLY {
            // Linux fifo open：O_WRONLY|O_NONBLOCK 且无任何 reader → 立即 ENXIO
            const O_NONBLOCK: u32 = 0x800;
            if (flags as u32) & O_NONBLOCK != 0
                && fifo_state.reader_count.load(core::sync::atomic::Ordering::Acquire) == 0
            {
                return -6; // -ENXIO
            }
            crate::fs::pipe::NamedFifoWrite::new(fifo_state, ino) as alloc::sync::Arc<dyn crate::fs::Inode>
        } else if access == O_RDWR {
            crate::fs::pipe::NamedFifoRdwr::new(fifo_state, ino) as alloc::sync::Arc<dyn crate::fs::Inode>
        } else {
            let _ = O_RDONLY;
            crate::fs::pipe::NamedFifoRead::new(fifo_state, ino) as alloc::sync::Arc<dyn crate::fs::Inode>
        }
    } else {
        inode
    };
    // For /proc/self/fd/N readlink (used by musl realpath), store the
    // canonical (resolved) path. If we followed a symlink during lookup,
    // the stored path should be the symlink target, not the link itself.
    let stored_path: String = if no_follow_final {
        String::from(full_path)
    } else if terminal_known_nonsymlink {
        // 末级已被单级查找/新建证实非 symlink：目录前缀规范缓存命中即
        // 直接拼接，免 resolve_symlinks_for_path 内部那次 lookup_no_follow
        // 终判（一整次 VFS 查找）。未命中仍走原慢路径。
        match full_path.rsplit_once('/') {
            Some((dir, name))
                if !dir.is_empty() && !name.is_empty() && name != "." && name != ".." =>
            {
                match crate::fs::canonical_dir_get(dir) {
                    Some(cdir) => {
                        let mut joined = String::with_capacity(cdir.len() + 1 + name.len());
                        if cdir != "/" {
                            joined.push_str(&cdir);
                        }
                        joined.push('/');
                        joined.push_str(name);
                        joined
                    }
                    None => resolve_symlinks_for_path(full_path),
                }
            }
            _ => resolve_symlinks_for_path(full_path),
        }
    } else {
        // Might have followed symlinks. Resolve the full path to canonical.
        resolve_symlinks_for_path(full_path)
    };
    let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
        inode,
        flags,
        Some(stored_path),
    ));
    let stored_path: &str = fd_obj.path.as_deref().unwrap_or(full_path);
    // Record the opener's effective credentials on the open file description.
    // cgroupfs uses them to enforce open-time-credential migration permission
    // for cgroup.procs writes (CVE-2021-4197); other inodes ignore it.
    // 凭证已随路径一次锁取出（read_path_and_cred），免再锁 PROCESS_TABLE。
    fd_obj.file.note_open_cred(cred_euid, cred_egid);
    // Bind a `/proc/<pid>` directory fd to the target's process *instance*.
    // Linux backs such a fd with the task's `struct pid`, so pidfd_send_signal
    // via this fd must return ESRCH once the PID is reused by a different
    // process instance (LTP pidfd_send_signal03). We record the live instance
    // id at open time; pidfd_resolve_live compares it against the current
    // occupant of that PID.
    if let Some(pid) = stored_path
        .strip_prefix("/proc/")
        .and_then(|rest| rest.parse::<usize>().ok())
    {
        let iid = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table.as_ref().and_then(|t| t.get(pid)).map(|p| p.instance_id)
        };
        if let Some(iid) = iid {
            fd_obj
                .pidfd_instance
                .store(iid, core::sync::atomic::Ordering::SeqCst);
        }
    }
    // Linux do_open(): when the file was just created (FMODE_CREATED), O_TRUNC
    // is cleared (`open_flag &= ~O_TRUNC`) — a fresh file is already empty, so
    // no truncate and crucially NO IN_MODIFY is emitted. Only honor O_TRUNC for
    // a pre-existing file (inotify02: creat → IN_CREATE+IN_OPEN, never IN_MODIFY).
    //
    // O_TRUNC 仅对**普通文件**有意义：Linux `do_truncate` 只截断 regular file,
    // 对 FIFO / 设备 / socket 等特殊文件 O_TRUNC 一律静默忽略(不报错)。此前对
    // FIFO 调 truncate 失败误返 EACCES, 导致 `fopen(fifo, "w+")` 失败 (stream02
    // 的 pipe file block)。目录路径已在前面按 EISDIR 拒绝。
    if flags & O_TRUNC != 0 && !created_file
        && fd_obj.file.file_type() == crate::fs::FileType::Regular
    {
        if let Some(file) = fd_obj.file.as_file() {
            if let Err(_) = file.truncate(0) {
                return -13;
            }
            crate::fs::inotify::notify_file_path(&full_path, crate::fs::inotify::IN_MODIFY);
            crate::fs::dnotify::notify_file_path(&full_path, crate::fs::dnotify::DN_MODIFY);
            crate::fs::fanotify::notify_file_path_typed(&full_path, crate::fs::fanotify::FAN_MODIFY, false);
        } else {
            return -22;
        }
    }

    // 分配 fd
    let fd = {
        let mut table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_mut() {
            if let Some(current_pid) = table.current {
                if let Some(process) = table.get_fd_process_mut(current_pid) {
                    match fd_table::alloc(process, fd_obj.clone(), 0, true) {
                        Ok(fd) => {
                            if flags & O_CLOEXEC != 0 {
                                if let Err(errno) = fd_table::set_cloexec(process, fd, true) {
                                    return errno;
                                }
                            }
                            fd as isize
                        }
                        Err(errno) => errno,
                    }
                } else {
                    -22
                }
            } else {
                -22
            }
        } else {
            -22
        }
    };
    if fd >= 0 {
        let file_type = fd_obj.file.file_type();
        let is_dir = file_type == crate::fs::FileType::Directory;
        // ETXTBSY tracking: record that a writable fd exists for this inode.
        let acc = flags & 0o3;
        if (acc == 1 || acc == 2) && file_type == crate::fs::FileType::Regular {
            crate::fs::get_write_access(fd_obj.file.ino());
        }
        if created_file {
            notify_inotify_create(&full_path, file_type);
            crate::fs::dnotify::notify_file_path(&full_path, crate::fs::dnotify::DN_CREATE);
            crate::fs::fanotify::notify_file_path_typed(&full_path, crate::fs::fanotify::FAN_CREATE, is_dir);
        }
        // O_PATH 打开（FMODE_PATH）不产生 IN_OPEN / FAN_OPEN（Linux
        // path_openat 的 do_o_path 分支不经过 do_dentry_open/fsnotify_open）。
        if flags & O_PATH == 0 {
            crate::fs::inotify::notify_file_path(
                &full_path,
                crate::fs::inotify::IN_OPEN | inotify_dir_bit(file_type),
            );
            // Deliver the open event on the canonical (symlink-resolved) path so a
            // FAN_MARK_DONT_FOLLOW mark on the symlink does not see opens through it,
            // while a normal (followed) mark on the target does (fanotify04).
            if crate::cmdline::fandbg_enabled() {
                crate::println!("[FANDBG] open deliver full={} stored={} nff={}", full_path, stored_path, no_follow_final);
            }
            crate::fs::fanotify::notify_file_path_typed(&stored_path, crate::fs::fanotify::FAN_OPEN, is_dir);
        }
    }
    fd
}

/// 解析路径 — delegates to path module
pub(crate) fn resolve_path(path: &str) -> String {
    super::path::resolve_path(path)
}

/// 统一 dirfd-relative 路径解析 — delegates to path module
pub(crate) fn resolve_at_path(dirfd: isize, path: &str) -> Result<String, isize> {
    super::path::resolve_at_path(dirfd, path)
}

/// Like sys_openat but takes a kernel-side path string instead of a user pointer.
/// Used by openat2 RESOLVE_IN_ROOT to remap absolute paths.
pub(in crate::syscall) fn sys_openat_path(dirfd: isize, path: &str, flags: u32, mode: u32) -> isize {
    use crate::sched::process::PROCESS_TABLE;
    use alloc::sync::Arc;

    if path.is_empty() {
        return -22; // -EINVAL
    }

    let full_path = match resolve_at_path(dirfd, path) {
        Ok(p) => p,
        Err(e) => return e,
    };

    // Check prefix search permission
    match crate::syscall::path::check_prefix_search_effective(&full_path) {
        Ok(()) => {}
        Err(-2) => {} // defer
        Err(errno) => return errno,
    }

    // Look up the file
    match crate::fs::lookup(&full_path) {
        Ok(inode) => {
            let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
                inode,
                flags,
                Some(String::from(full_path)),
            ));
            let mut table = PROCESS_TABLE.lock();
            if let Some(table) = table.as_mut() {
                if let Some(current_pid) = table.current {
                    if let Some(process) = table.get_fd_process_mut(current_pid) {
                        return match crate::fs::fd::alloc(process, fd_obj, 0, true) {
                            Ok(fd) => {
                                const O_CLOEXEC: u32 = 0o2000000;
                                if flags & O_CLOEXEC != 0 {
                                    let _ = crate::fs::fd::set_cloexec(process, fd, true);
                                }
                                fd as isize
                            }
                            Err(errno) => errno,
                        };
                    }
                }
            }
            -1
        }
        Err(_) => -2, // -ENOENT
    }
}

// ---- openat2(2) ----
// Linux 5.6+ openat2 with full validation of struct open_how.
pub(in crate::syscall) fn sys_openat2(dirfd: isize, pathname: *const u8, how: *const u8, size: usize) -> isize {
    // struct open_how { u64 flags; u64 mode; u64 resolve; } = 24 bytes
    const OPEN_HOW_SIZE: usize = 24;
    if how.is_null() || size < OPEN_HOW_SIZE { return -22; } // EINVAL

    // If size > OPEN_HOW_SIZE, read extra bytes and check they're all zero
    if size > OPEN_HOW_SIZE {
        let bytes = match super::user::read_user_buffer(how, size) {
            Ok(b) => b,
            Err(_) => return -14, // EFAULT
        };
        // Check padding bytes are zero
        for &b in &bytes[OPEN_HOW_SIZE..] {
            if b != 0 {
                return -7; // -E2BIG: unknown extension fields are non-zero
            }
        }
        let flags = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let mode  = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let resolve = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        return openat2_validated(dirfd, pathname, flags, mode, resolve);
    }

    let bytes = match super::user::read_user_buffer(how, OPEN_HOW_SIZE) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let flags = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let mode  = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
    let resolve = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
    openat2_validated(dirfd, pathname, flags, mode, resolve)
}

fn openat2_validated(dirfd: isize, pathname: *const u8, flags: u64, mode: u64, resolve: u64) -> isize {
    const O_CREAT: u64 = 0o100;
    const O_TMPFILE: u64 = 0o20200000;
    // Known RESOLVE_* flags
    const RESOLVE_ALL: u64 = 0x01 | 0x02 | 0x04 | 0x08 | 0x10 | 0x20;

    // Validate: mode must be zero if neither O_CREAT nor O_TMPFILE
    if (flags & O_CREAT == 0) && (flags & O_TMPFILE != O_TMPFILE) {
        if mode != 0 {
            return -22; // EINVAL
        }
    }
    // Validate: mode has no bits beyond 07777
    if mode > 0o7777 {
        return -22; // EINVAL
    }
    // Validate: unknown resolve bits
    if resolve & !RESOLVE_ALL != 0 {
        return -22; // EINVAL
    }
    // Validate: unknown flag bits (Linux rejects flags it doesn't know about)
    // Known open flags (very broad set to avoid false positives):
    const KNOWN_FLAGS: u64 = 0o377_777_777; // all flags up to bit 23
    if flags & !KNOWN_FLAGS != 0 {
        return -22; // EINVAL
    }

    // Enforce RESOLVE_* flags before delegating to sys_openat.
    const RESOLVE_NO_XDEV:       u64 = 0x01;
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_NO_SYMLINKS:   u64 = 0x04;
    const RESOLVE_BENEATH:       u64 = 0x08;
    const RESOLVE_IN_ROOT:       u64 = 0x10;

    if resolve != 0 {
        // Read the pathname string for resolve checks
        let path_str = match super::user::read_path(pathname) {
            Ok(s) => s,
            Err(e) => return e,
        };

        // Resolve the full path for analysis
        let full_path = match resolve_at_path(dirfd, &path_str) {
            Ok(p) => p,
            Err(e) => return e,
        };

        // RESOLVE_NO_MAGICLINKS: reject /proc/self/fd/N and /proc/<pid>/fd/N
        if resolve & RESOLVE_NO_MAGICLINKS != 0 {
            if crate::syscall::path::is_proc_fd_path(&full_path) {
                return -40; // -ELOOP
            }
        }

        // RESOLVE_NO_SYMLINKS: reject if any component is a symlink
        if resolve & RESOLVE_NO_SYMLINKS != 0 {
            if crate::syscall::path::has_symlink_component(&full_path) {
                return -40; // -ELOOP
            }
        }

        // RESOLVE_NO_XDEV: reject cross-mount boundaries (e.g. ramfs -> procfs/devfs)
        if resolve & RESOLVE_NO_XDEV != 0 {
            if crate::syscall::path::crosses_mount_point(&full_path, dirfd) {
                return -18; // -EXDEV
            }
        }

        // RESOLVE_BENEATH: path must not escape the dirfd directory
        if resolve & RESOLVE_BENEATH != 0 {
            // absolute path or ".." escape → EXDEV
            if path_str.starts_with('/') {
                return -18; // -EXDEV
            }
            if crate::syscall::path::escapes_beneath(&path_str) {
                return -18; // -EXDEV
            }
        }

        // RESOLVE_IN_ROOT: absolute paths in pathname are resolved relative to dirfd.
        if resolve & RESOLVE_IN_ROOT != 0 {
            if path_str.starts_with('/') {
                // Strip leading '/' to make it relative, then resolve from dirfd/CWD
                let relative = path_str.trim_start_matches('/');
                if relative.is_empty() {
                    // Opening "/" as the dirfd directory itself
                    return sys_openat_path(dirfd, ".", flags as u32, mode as u32);
                }
                return sys_openat_path(dirfd, relative, flags as u32, mode as u32);
            }
            // ".." must not escape dirfd root
            if crate::syscall::path::escapes_beneath(&path_str) {
                return -18; // -EXDEV
            }
        }
    }

    sys_openat(dirfd, pathname, flags as u32, mode as u32)
}


