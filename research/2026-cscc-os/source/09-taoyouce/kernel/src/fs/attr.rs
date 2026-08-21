use crate::syscall::user::read_c_string;

pub(crate) fn sys_fchmodat(dirfd: isize, path: *const u8, mode: u32, flags: u32) -> isize {
    const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
    if flags & !AT_SYMLINK_NOFOLLOW != 0 {
        return -22;
    }
    let path = match read_chmod_path(path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    // /proc/self/fd/<N> → delegate to sys_fchmod (musl fchmod fallback on riscv64)
    if let Some(rest) = path.strip_prefix("/proc/self/fd/") {
        if let Ok(fd) = rest.parse::<usize>() {
            return sys_fchmod(fd, mode);
        }
    }
    if path.is_empty() {
        return -2; // ENOENT
    }
    if chmod_path_name_too_long(&path) {
        return -36; // ENAMETOOLONG
    };
    let full_path = match crate::syscall::resolve_at_path(dirfd, &path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = chmod_check_search_prefix(&full_path) {
        return errno;
    }
    let inode = match crate::fs::lookup(&full_path) {
        Ok(inode) => inode,
        Err(err) => return chmod_lookup_errno(err),
    };
    if crate::fs::is_readonly_path(&full_path) {
        return -30; // EROFS
    }
    if let Err(errno) = chmod_check_owner_or_cap(&inode) {
        return errno;
    }
    let effective_mode = chmod_mode_after_linux_fsetid_rules(&inode, mode);
    match inode.set_mode(effective_mode) {
        Ok(()) => {}
        Err(crate::fs::FsError::PermissionDenied) => return -13,
        Err(_) => return -5,
    }
    let dir_bit = if inode.file_type() == crate::fs::FileType::Directory {
        crate::fs::inotify::IN_ISDIR
    } else {
        0
    };
    crate::fs::inotify::notify_file_path(&full_path, crate::fs::inotify::IN_ATTRIB | dir_bit);
    crate::fs::dnotify::notify_file_path(&full_path, crate::fs::dnotify::DN_ATTRIB);
    crate::fs::fanotify::notify_file_path(&full_path, crate::fs::fanotify::FAN_ATTRIB);
    0
}

pub(crate) fn sys_fchmod(fd: usize, mode: u32) -> isize {
    const O_PATH: u32 = 0o10000000;
    let fd_info = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get_fd_process(pid) {
                    crate::fs::fd::get(process, fd)
                        .map(|fd_obj| (fd_obj.flags, fd_obj.file.clone(), fd_obj.path.clone()))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    };
    let (flags, inode, path) = match fd_info {
        Some(info) => info,
        None => return -9,
    };
    if flags & O_PATH != 0 {
        return -9; // EBADF: O_PATH fds don't support fchmod
    }
    if let Some(path) = path.as_ref() {
        if crate::fs::is_readonly_path(path) {
            return -30; // EROFS
        }
    }
    if let Err(errno) = chmod_check_owner_or_cap(&inode) {
        return errno;
    }
    let effective_mode = chmod_mode_after_linux_fsetid_rules(&inode, mode);
    match inode.set_mode(effective_mode) {
        Ok(()) => {}
        Err(crate::fs::FsError::PermissionDenied) => return -13,
        Err(_) => return -5,
    }
    if let Some(path) = path.as_ref() {
        let dir_bit = if inode.file_type() == crate::fs::FileType::Directory {
            crate::fs::inotify::IN_ISDIR
        } else {
            0
        };
        crate::fs::inotify::notify_file_path(path, crate::fs::inotify::IN_ATTRIB | dir_bit);
        crate::fs::dnotify::notify_file_path(path, crate::fs::dnotify::DN_ATTRIB);
        crate::fs::fanotify::notify_file_path(path, crate::fs::fanotify::FAN_ATTRIB);
    }
    0
}

fn read_chmod_path(path: *const u8) -> Result<alloc::string::String, isize> {
    if path.is_null() {
        return Err(-14);
    }
    match read_c_string(path) {
        Ok(path) => Ok(path),
        Err("C string too long or not null-terminated") => Err(-36), // ENAMETOOLONG
        Err(_) => Err(-14),                                          // EFAULT
    }
}

fn chmod_path_name_too_long(path: &str) -> bool {
    const PATH_MAX: usize = 4096;
    const NAME_MAX: usize = 255;
    path.len() >= PATH_MAX || path.split('/').any(|component| component.len() > NAME_MAX)
}

fn chmod_lookup_errno(err: crate::fs::FsError) -> isize {
    match err {
        crate::fs::FsError::NotFound => -2,
        crate::fs::FsError::NotDirectory => -20,
        crate::fs::FsError::PermissionDenied => -13,
        crate::fs::FsError::OperationNotPermitted => -1,
        crate::fs::FsError::TooManyLinks => -40, // symlink-follow exhaustion → ELOOP
        crate::fs::FsError::NameTooLong => -36,
        crate::fs::FsError::InvalidArgument => -22,
        _ => -5,
    }
}

fn current_chmod_cred() -> (u32, u32, u64, alloc::vec::Vec<u32>) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)))
        .map(|p| {
            (
                p.euid,
                p.egid,
                p.cap_effective,
                p.supplementary_groups.clone(),
            )
        })
        .unwrap_or_else(|| (0, 0, u64::MAX, alloc::vec::Vec::new()))
}

fn chmod_gid_matches(egid: u32, supplementary_groups: &[u32], gid: u32) -> bool {
    egid == gid || supplementary_groups.iter().any(|&group| group == gid)
}

fn chmod_check_search_allowed(stat: &crate::fs::FileStat) -> bool {
    const CAP_DAC_OVERRIDE: u64 = 1u64 << 1;
    const CAP_DAC_READ_SEARCH: u64 = 1u64 << 2;
    let (euid, egid, caps, supplementary_groups) = current_chmod_cred();
    if euid == 0 || (caps & (CAP_DAC_OVERRIDE | CAP_DAC_READ_SEARCH)) != 0 {
        return true;
    }
    let bits = if euid == stat.uid {
        (stat.mode.0 >> 6) & 0o7
    } else if chmod_gid_matches(egid, &supplementary_groups, stat.gid) {
        (stat.mode.0 >> 3) & 0o7
    } else {
        stat.mode.0 & 0o7
    };
    bits & 0o1 != 0
}

fn chmod_check_search_prefix(full_path: &str) -> Result<(), isize> {
    let components: alloc::vec::Vec<&str> = full_path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect();
    if components.len() <= 1 {
        return Ok(());
    }

    let mut prefix = alloc::string::String::from("/");
    for component in components.iter().take(components.len() - 1) {
        if prefix != "/" {
            prefix.push('/');
        }
        prefix.push_str(component);

        let inode = crate::fs::lookup(&prefix).map_err(chmod_lookup_errno)?;
        if inode.file_type() != crate::fs::FileType::Directory {
            return Err(-20);
        }
        let stat = inode.stat().map_err(chmod_lookup_errno)?;
        if !chmod_check_search_allowed(&stat) {
            return Err(-13);
        }
    }
    Ok(())
}

fn chmod_check_owner_or_cap(inode: &alloc::sync::Arc<dyn crate::fs::Inode>) -> Result<(), isize> {
    const CAP_FOWNER: u64 = 1u64 << 3;
    let stat = inode.stat().map_err(chmod_lookup_errno)?;
    let (euid, _, caps, _) = current_chmod_cred();
    if euid == 0 || (caps & CAP_FOWNER) != 0 || euid == stat.uid {
        Ok(())
    } else {
        Err(-1) // EPERM
    }
}

fn chmod_mode_after_linux_fsetid_rules(
    inode: &alloc::sync::Arc<dyn crate::fs::Inode>,
    mode: u32,
) -> u32 {
    const S_ISGID: u32 = 0o2000;
    const CAP_FSETID: u64 = 1u64 << 4;

    if (mode & S_ISGID) == 0 {
        return mode;
    }

    let file_gid = match inode.stat() {
        Ok(stat) => stat.gid,
        Err(_) => return mode,
    };

    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(t) = table.as_ref() else {
        return mode;
    };
    let Some(pid) = t.current else {
        return mode;
    };
    let Some(p) = t.get(pid) else {
        return mode;
    };

    if p.euid == 0 || (p.cap_effective & CAP_FSETID) != 0 {
        return mode;
    }
    if p.egid == file_gid || p.supplementary_groups.iter().any(|&gid| gid == file_gid) {
        return mode;
    }

    mode & !S_ISGID
}

pub(crate) fn sys_fchown(fd: usize, owner: u32, group: u32) -> isize {
    const O_PATH: u32 = 0o10000000;
    let fd_info = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get_fd_process(pid) {
                    crate::fs::fd::get(process, fd)
                        .map(|fd_obj| (fd_obj.flags, fd_obj.file.clone(), fd_obj.path.clone()))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        }
    };
    let (flags, inode, path) = match fd_info {
        Some(info) => info,
        None => return -9,
    };
    if flags & O_PATH != 0 {
        return -9; // EBADF: O_PATH fds don't support fchown
    }
    if let Some(path) = path.as_ref() {
        if crate::fs::is_readonly_path(path) {
            return -30; // EROFS
        }
    }
    if let Err(errno) = chown_apply(&inode, owner, group) {
        return errno;
    }
    if let Some(path) = path.as_ref() {
        let dir_bit = if inode.file_type() == crate::fs::FileType::Directory {
            crate::fs::inotify::IN_ISDIR
        } else {
            0
        };
        crate::fs::inotify::notify_file_path(path, crate::fs::inotify::IN_ATTRIB | dir_bit);
        crate::fs::dnotify::notify_file_path(path, crate::fs::dnotify::DN_ATTRIB);
        crate::fs::fanotify::notify_file_path(path, crate::fs::fanotify::FAN_ATTRIB);
    }
    0
}

pub(crate) fn sys_fchownat(
    dirfd: isize,
    path: *const u8,
    owner: u32,
    group: u32,
    flags: u32,
) -> isize {
    const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
    const AT_EMPTY_PATH: u32 = 0x1000;
    if flags & !(AT_SYMLINK_NOFOLLOW | AT_EMPTY_PATH) != 0 {
        return -22;
    }
    let path = match read_chmod_path(path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };

    // /proc/self/fd/<N> → delegate to sys_fchown (musl fchown fallback on riscv64)
    if let Some(rest) = path.strip_prefix("/proc/self/fd/") {
        if let Ok(fd) = rest.parse::<usize>() {
            return sys_fchown(fd, owner, group);
        }
    }

    // AT_EMPTY_PATH: 当 path 为空字符串时，作用于 dirfd 自身（fchown(dirfd, ...)），
    // 与 Linux 5.x 的 fchownat(AT_EMPTY_PATH) 一致。
    if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 || dirfd < 0 {
            return -2;
        }
        return sys_fchown(dirfd as usize, owner, group);
    }
    if chmod_path_name_too_long(&path) {
        return -36;
    }

    let full_path = match crate::syscall::resolve_at_path(dirfd, &path) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    if let Err(errno) = chmod_check_search_prefix(&full_path) {
        return errno;
    }
    let lookup_result = if flags & AT_SYMLINK_NOFOLLOW != 0 {
        crate::fs::lookup_no_follow(&full_path)
    } else {
        crate::fs::lookup(&full_path)
    };
    let inode = match lookup_result {
        Ok(inode) => inode,
        Err(err) => return chmod_lookup_errno(err),
    };
    if crate::fs::is_readonly_path(&full_path) {
        return -30; // EROFS
    }
    if let Err(errno) = chown_apply(&inode, owner, group) {
        return errno;
    }
    let dir_bit = if inode.file_type() == crate::fs::FileType::Directory {
        crate::fs::inotify::IN_ISDIR
    } else {
        0
    };
    crate::fs::inotify::notify_file_path(&full_path, crate::fs::inotify::IN_ATTRIB | dir_bit);
    crate::fs::dnotify::notify_file_path(&full_path, crate::fs::dnotify::DN_ATTRIB);
    crate::fs::fanotify::notify_file_path(&full_path, crate::fs::fanotify::FAN_ATTRIB);
    0
}

fn chown_apply(
    inode: &alloc::sync::Arc<dyn crate::fs::Inode>,
    owner: u32,
    group: u32,
) -> Result<(), isize> {
    let uid = if owner == u32::MAX { None } else { Some(owner) };
    let gid = if group == u32::MAX { None } else { Some(group) };
    let stat = inode.stat().map_err(chmod_lookup_errno)?;
    let (euid, egid, caps, supplementary_groups) = current_chmod_cred();
    const CAP_CHOWN: u64 = 1u64 << 0;

    let privileged = euid == 0 || (caps & CAP_CHOWN) != 0;
    if !privileged {
        let owner_unchanged = uid.map_or(true, |new_uid| new_uid == stat.uid);
        let group_allowed = gid.map_or(true, |new_gid| {
            new_gid == stat.gid || chmod_gid_matches(egid, &supplementary_groups, new_gid)
        });
        if euid != stat.uid || !owner_unchanged || !group_allowed {
            return Err(-1); // EPERM
        }
    }

    match inode.set_owner(uid, gid) {
        Ok(()) => Ok(()),
        Err(crate::fs::FsError::PermissionDenied) => Err(-13),
        Err(crate::fs::FsError::OperationNotPermitted) => Err(-1),
        Err(err) => Err(chmod_lookup_errno(err)),
    }
}
