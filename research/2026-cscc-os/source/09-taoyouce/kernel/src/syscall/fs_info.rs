use super::user::{UserCString, UserSlice};
use crate::fs::fd as fd_table;

#[allow(dead_code)]
pub(in crate::syscall) fn sys_statfs(path_ptr: *const u8, buf: *mut u8) -> isize {
    // getname semantics: EFAULT for a bad pointer, ENAMETOOLONG for a pathname
    // (or component) that is too long (statfs02).
    let path = match super::user::read_path(path_ptr) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let full_path = match super::open::resolve_at_path(-100, &path) {
        Ok(p) => p,
        Err(e) => return e, // -ENAMETOOLONG for an over-long component
    };
    // namei on the path prefix: a non-searchable ancestor is EACCES (statfs03),
    // a file component is ENOTDIR, a missing one is ENOENT (statfs02).
    if let Err(e) = super::path::check_prefix_search_effective(&full_path) {
        return e;
    }
    // The target itself must exist and resolve (following symlinks -> ELOOP).
    match crate::fs::lookup(&full_path) {
        Ok(_) => {}
        Err(crate::fs::FsError::NotDirectory) => return -20,
        Err(crate::fs::FsError::TooManyLinks) => return -40,
        Err(crate::fs::FsError::NotFound) => return -2,
        Err(_) => return -2,
    }
    fill_statfs(buf, &full_path)
}

#[allow(dead_code)]
pub(in crate::syscall) fn sys_fstatfs(fd: usize, buf: *mut u8) -> isize {
    let path = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let table = match table.as_ref() { Some(t) => t, None => return -1 };
        let pid = match table.current { Some(p) => p, None => return -1 };
        let process = match table.get_fd_process(pid) { Some(p) => p, None => return -1 };
        match fd_table::get(process, fd) {
            Some(fd_obj) => fd_obj.path.clone().unwrap_or_else(|| "/".into()),
            None => return -9,
        }
    };
    fill_statfs(buf, &path)
}

/// 获取文件状态
#[allow(dead_code)]
pub(in crate::syscall) fn sys_fstat(fd: usize, stat_buf: *mut u8) -> isize {
    // 通过 fd 获取文件描述符
    let fd_obj = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get_fd_process(pid) {
                    fd_table::get(process, fd)
                } else { None }
            } else { None }
        } else { None }
    };

    let f = match fd_obj {
        Some(f) => f,
        None => return -9, // -EBADF
    };

    match f.file.stat() {
        Ok(stat) => {
            // A `/proc/<pid>` directory fd is bound to the process *instance*
            // that occupied the PID at open time (recorded in pidfd_instance).
            // Mix that binding into st_ino so a recycled PID yields a distinct
            // inode number for the new occupant, matching Linux where each
            // process gets its own procfs inode.
            let mut ino = f.file.ino() as u64;
            let iid = f
                .pidfd_instance
                .load(core::sync::atomic::Ordering::SeqCst);
            if iid != 0 {
                ino ^= iid
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    .rotate_left(17);
            }
            let buf = build_stat_buf(ino, &stat, f.file.device_id());

            if let Err(_) = UserSlice::<u8>::new_mut(stat_buf, buf.len()).write_bytes(&buf) {
                return -14; // -EFAULT
            }
            0
        }
        Err(_) => -5, // -EIO
    }
}

#[allow(dead_code)]
pub(in crate::syscall) fn sys_stat(path_ptr: *const u8, stat_buf: *mut u8) -> isize {
    // 从用户空间读取路径（null-terminated C 字符串）
    let path = match UserCString::new(path_ptr).read() {
        Ok(s) => s,
        Err(_) => return -14, // -EFAULT
    };

    // 解析路径
    let full_path = super::resolve_path(&path);

    // 查找文件
    match crate::fs::lookup(&full_path) {
        Ok(inode) => {
            match inode.stat() {
                Ok(stat) => {
                    let buf = build_stat_buf(inode.ino() as u64, &stat, inode.device_id());

                    if let Err(_) = UserSlice::<u8>::new_mut(stat_buf, buf.len()).write_bytes(&buf) {
                        return super::SyscallError::InvalidArgument as isize;
                    }

                    0
                }
                Err(_) => super::SyscallError::IoError as isize,
            }
        }
        Err(_) => super::SyscallError::NotFound as isize,
    }
}

/// fstatat(dirfd, pathname, statbuf, flags)
/// 当 flags 含 AT_EMPTY_PATH 且 pathname 为空时，对 dirfd 本身做 stat
/// 当 pathname 为绝对路径或 dirfd==AT_FDCWD 时，行为与 stat 一致
#[allow(dead_code)]
pub(in crate::syscall) fn sys_newfstatat(dirfd: isize, path_ptr: *const u8, stat_buf: *mut u8, flags: u32) -> isize {
    const AT_SYMLINK_NOFOLLOW: u32 = 0x0100;
    const AT_NO_AUTOMOUNT: u32 = 0x0800;
    const AT_EMPTY_PATH: u32 = 0x1000;
    const KNOWN_FLAGS: u32 = AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT | AT_EMPTY_PATH;
    const AT_FDCWD: isize = -100;
    if flags & !KNOWN_FLAGS != 0 {
        return -22;
    }

    // getname() semantics: EFAULT for unmapped pointer, ENAMETOOLONG for an
    // overlong path/component (lstat02/stat path-error cases). Empty string is
    // returned as Ok("") and handled by the AT_EMPTY_PATH branch below.
    let path = match super::user::read_path(path_ptr) {
        Ok(s) => s,
        Err(e) => return e, // -EFAULT / -ENAMETOOLONG
    };

    // AT_EMPTY_PATH + 空路径 → 对 dirfd 本身做 stat；dirfd=AT_FDCWD 时
    // Linux 语义是当前工作目录。
    if path.is_empty() {
        if flags & AT_EMPTY_PATH == 0 {
            return -2;
        }
        if dirfd == AT_FDCWD {
            let cwd = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                table
                    .as_ref()
                    .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| p.cwd()))
                    .unwrap_or_else(|| "/".into())
            };
            return stat_path_to_user(&cwd, stat_buf, false);
        }
        if dirfd < 0 {
            return -9;
        }
        return sys_fstat(dirfd as usize, stat_buf);
    }

    // 解析完整路径（统一 dirfd 支持）
    let full_path = match super::resolve_at_path(dirfd, &path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    // namei requires MAY_EXEC on every ancestor directory (lstat02 EACCES case).
    if let Err(e) = super::path::check_prefix_search_effective(&full_path) {
        return e;
    }
    let nofollow = flags & AT_SYMLINK_NOFOLLOW != 0;
    stat_path_to_user(&full_path, stat_buf, nofollow)
}

fn fs_info_errno(err: crate::fs::FsError) -> isize {
    match err {
        crate::fs::FsError::NotFound => -2,
        crate::fs::FsError::NotDirectory => -20,
        crate::fs::FsError::PermissionDenied => -13,
        crate::fs::FsError::OperationNotPermitted => -1,
        crate::fs::FsError::TooManyLinks => -40,
        crate::fs::FsError::NameTooLong => -36,
        crate::fs::FsError::InvalidArgument => -22,
        _ => -5,
    }
}

fn stat_path_to_user(path: &str, stat_buf: *mut u8, nofollow: bool) -> isize {
    let lookup = if nofollow {
        crate::fs::lookup_no_follow(path)
    } else {
        crate::fs::lookup(path)
    };
    let inode = match lookup {
        Ok(inode) => inode,
        Err(err) => return fs_info_errno(err),
    };
    match inode.stat() {
        Ok(stat) => {
            let buf = build_stat_buf(inode.ino() as u64, &stat, inode.device_id());
            if UserSlice::<u8>::new_mut(stat_buf, buf.len()).write_bytes(&buf).is_err() {
                return -14;
            }
            0
        }
        Err(err) => fs_info_errno(err),
    }
}

fn fill_statfs(buf: *mut u8, path: &str) -> isize {
    if buf.is_null() { return -14; }
    let (f_type, f_bsize, f_blocks, f_bfree, f_bavail, f_files, f_ffree, f_namelen) =
        if path.starts_with("/proc") {
            (0x9fa0i64, 4096i64, 0i64, 0i64, 0i64, 0i64, 0i64, 255i64)
        } else if path.starts_with("/dev") {
            // /dev (devtmpfs) and /dev/shm (tmpfs) report TMPFS_MAGIC. /tmp is
            // NOT a separate mount — it lives on the ext4 root, so it must fall
            // through to the ext4 branch below (statfs("/tmp").f_type == EXT4),
            // otherwise LTP's tst_fs_type() sees tmpfs and dio/O_DIRECT tests
            // TCONF with "tmpfs is not supported by the test".
            (0x01021994i64, 4096i64, 16384i64, 16384i64, 16384i64, 0i64, 0i64, 255i64)
        } else if path.starts_with("/sys") {
            (0x62656572i64, 4096i64, 0i64, 0i64, 0i64, 0i64, 0i64, 255i64)
        } else {
            // ext4 root on the 4GB sdcard. Writes are served by a RAM-backed
            // COW overlay (see fs/ext4.rs), so the *writable* free space is
            // bounded by free physical memory, NOT by the 4 GiB image. Report
            // f_bavail/f_bfree as min(nominal image free, free RAM): this is
            // the honest capacity (an LTP test that needs more than we can
            // actually store then TCONF-skips exactly like mainline Linux on a
            // memory-constrained box — e.g. dma_thread_diotest's 1.3 GiB
            // precondition) while still clearing the smaller preconditions of
            // dio_append / growfiles when RAM is plentiful.
            const NOMINAL_FREE_BLOCKS: i64 = 786432; // 3 GiB @ 4 KiB blocks
            let avail_blocks = (crate::mm::get_available_memory() / 4096) as i64;
            let free_blocks = avail_blocks.min(NOMINAL_FREE_BLOCKS).max(0);
            (0xEF53i64, 4096i64, 1048576i64, free_blocks, free_blocks, 262144i64, 200000i64, 255i64)
        };
    // Retrieve mount flags for this path from MOUNT_INFO_TABLE.
    // Linux statfs returns f_flags with the mount's MS_* flags (ST_RDONLY,
    // ST_NOSUID, ST_NODEV, ST_NOEXEC, ST_NODIRATIME, ST_NOSYMFOLLOW, etc.)
    // at offset 80 of struct statfs. Linux always sets ST_VALID (0x0020)
    // to indicate the flags field is meaningful.
    const ST_VALID: i64 = 0x0020;
    const MS_NOSYMFOLLOW: i64 = 0x100;
    const ST_NOSYMFOLLOW: i64 = 0x2000;
    let f_flags: i64 = {
        let table = crate::fs::MOUNT_INFO_TABLE.lock();
        let normalized = if path.is_empty() { "/" } else { path };
        let mount_flags = table.iter()
            .filter(|m| {
                if m.target == "/" { return true; }
                if normalized == m.target { return true; }
                normalized.len() > m.target.len()
                    && normalized.as_bytes().get(m.target.len()) == Some(&b'/')
                    && normalized.starts_with(&m.target)
            })
            .max_by_key(|m| m.target.len())
            .map(|m| m.flags as i64)
            .unwrap_or(0);
        // Convert MS_NOSYMFOLLOW (0x100) to ST_NOSYMFOLLOW (0x2000) for userspace.
        let mut flags_out = (mount_flags & !MS_NOSYMFOLLOW) | ST_VALID;
        if mount_flags & MS_NOSYMFOLLOW != 0 {
            flags_out |= ST_NOSYMFOLLOW;
        }
        flags_out
    };
    let mut data = [0u8; 120];
    data[0..8].copy_from_slice(&f_type.to_le_bytes());
    data[8..16].copy_from_slice(&f_bsize.to_le_bytes());
    data[16..24].copy_from_slice(&f_blocks.to_le_bytes());
    data[24..32].copy_from_slice(&f_bfree.to_le_bytes());
    data[32..40].copy_from_slice(&f_bavail.to_le_bytes());
    data[40..48].copy_from_slice(&f_files.to_le_bytes());
    data[48..56].copy_from_slice(&f_ffree.to_le_bytes());
    data[64..72].copy_from_slice(&f_namelen.to_le_bytes());       // f_namelen
    data[72..80].copy_from_slice(&f_bsize.to_le_bytes());         // f_frsize
    data[80..88].copy_from_slice(&f_flags.to_le_bytes());         // f_flags
    if UserSlice::<u8>::new_mut(buf, data.len()).write_bytes(&data).is_err() {
        return -14;
    }
    0
}

fn encode_dev(major: u32, minor: u32) -> u64 {
    // Linux new_encode_dev(): 12-bit major + 20-bit minor in a 64-bit dev_t.
    ((minor & 0xff) as u64)
        | (((major & 0xfff) as u64) << 8)
        | (((minor & !0xff) as u64) << 12)
}

fn build_stat_buf(ino: u64, stat: &crate::fs::FileStat, rdev: Option<(u32, u32)>) -> [u8; 128] {
    let mut buf = [0u8; 128];
    let put_u64 = |b: &mut [u8], off: usize, v: u64| {
        b[off..off+8].copy_from_slice(&v.to_le_bytes());
    };
    let put_u32 = |b: &mut [u8], off: usize, v: u32| {
        b[off..off+4].copy_from_slice(&v.to_le_bytes());
    };
    let put_i64 = |b: &mut [u8], off: usize, v: i64| {
        b[off..off+8].copy_from_slice(&v.to_le_bytes());
    };
    let put_i32 = |b: &mut [u8], off: usize, v: i32| {
        b[off..off+4].copy_from_slice(&v.to_le_bytes());
    };
    put_u64(&mut buf, 0, 0);                        // st_dev
    put_u64(&mut buf, 8, ino);                      // st_ino
    put_u32(&mut buf, 16, stat.mode.0);             // st_mode
    put_u32(&mut buf, 20, stat.nlink as u32);       // st_nlink
    put_u32(&mut buf, 24, stat.uid);                // st_uid
    put_u32(&mut buf, 28, stat.gid);                // st_gid
    let rdev = rdev.map(|(major, minor)| encode_dev(major, minor)).unwrap_or(0);
    put_u64(&mut buf, 32, rdev);                    // st_rdev
    put_u64(&mut buf, 40, 0);                       // __pad1
    put_i64(&mut buf, 48, stat.size as i64);        // st_size
    put_i32(&mut buf, 56, 4096);                    // st_blksize
    put_i32(&mut buf, 60, 0);                       // __pad2
    put_i64(&mut buf, 64, (stat.size as i64 + 511) / 512); // st_blocks
    put_i64(&mut buf, 72, stat.atime as i64);       // st_atime
    put_u64(&mut buf, 80, 0);                       // st_atime_nsec
    put_i64(&mut buf, 88, stat.mtime as i64);       // st_mtime
    put_u64(&mut buf, 96, 0);                       // st_mtime_nsec
    put_i64(&mut buf, 104, stat.ctime as i64);      // st_ctime
    put_u64(&mut buf, 112, 0);                      // st_ctime_nsec
    buf
}

/// statx — 正确实现 struct statx 布局
///
/// struct statx 与 struct stat 布局完全不同，不可混用
pub(in crate::syscall) fn sys_statx(dirfd: isize, pathname: *const u8, flags: u32, mask: u32, statxbuf: *mut u8) -> isize {
    const AT_EMPTY_PATH:        u32 = 0x1000;
    const AT_SYMLINK_NOFOLLOW:  u32 = 0x0100;
    const AT_NO_AUTOMOUNT:      u32 = 0x0800;
    const AT_STATX_SYNC_TYPE:   u32 = 0x6000;
    const KNOWN_FLAGS: u32 = AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW | AT_NO_AUTOMOUNT | AT_STATX_SYNC_TYPE;
    if flags & !KNOWN_FLAGS != 0 {
        return -22;
    }
    // Linux statx(): reject a mask that sets the reserved bit STATX__RESERVED
    // (bit 31). Other unknown mask bits are silently ignored, but the reserved
    // bit is explicitly rejected with EINVAL (statx03 mask=-1 subcase).
    const STATX_RESERVED: u32 = 0x8000_0000;
    if mask & STATX_RESERVED != 0 {
        return -22; // -EINVAL
    }

    // STATX_* bits — what userspace asked for
    const STATX_TYPE:        u32 = 0x0001;
    const STATX_MODE:        u32 = 0x0002;
    const STATX_NLINK:       u32 = 0x0004;
    const STATX_UID:         u32 = 0x0008;
    const STATX_GID:         u32 = 0x0010;
    const STATX_ATIME:       u32 = 0x0020;
    const STATX_MTIME:       u32 = 0x0040;
    const STATX_CTIME:       u32 = 0x0080;
    const STATX_INO:         u32 = 0x0100;
    const STATX_SIZE:        u32 = 0x0200;
    const STATX_BLOCKS:      u32 = 0x0400;
    const STATX_BTIME:       u32 = 0x0800;
    const STATX_MNT_ID:      u32 = 0x1000;
    const STATX_BASIC_STATS: u32 = STATX_TYPE | STATX_MODE | STATX_NLINK
        | STATX_UID | STATX_GID | STATX_ATIME | STATX_MTIME | STATX_CTIME
        | STATX_INO | STATX_SIZE | STATX_BLOCKS;

    // getname() semantics (statx is the lstat/stat backend on LoongArch musl):
    // EFAULT for a bad pointer, ENAMETOOLONG for an overlong path/component.
    let path = match super::user::read_path(pathname) {
        Ok(s) => s,
        Err(e) => return e,
    };

    // A `/proc/<pid>` directory fd is bound to the process *instance* that held
    // the PID at open time (recorded in pidfd_instance). When statx backs an
    // fstat(fd) via AT_EMPTY_PATH (the LoongArch musl path), carry that binding
    // so a recycled PID yields a distinct stx_ino for the new occupant.
    let mut pidfd_iid: u64 = 0;
    let target: Option<(alloc::sync::Arc<dyn crate::fs::Inode>, bool)> =
        if (flags & AT_EMPTY_PATH) != 0 && path.is_empty() {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref t) = *table {
                if let Some(pid) = t.current {
                    if let Some(process) = t.get_fd_process(pid) {
                        let fd = dirfd as usize;
                        fd_table::get(process, fd).map(|fobj| {
                            pidfd_iid = fobj
                                .pidfd_instance
                                .load(core::sync::atomic::Ordering::SeqCst);
                            (fobj.file.clone(), false)
                        })
                    } else { None }
                } else { None }
            } else { None }
        } else if path.is_empty() {
            return -2;
        } else {
            let full_path = match super::resolve_at_path(dirfd, &path) {
                Ok(p) => p,
                Err(e) => return e,
            };
            // namei MAY_EXEC on every ancestor directory (lstat02 EACCES case).
            if let Err(e) = super::path::check_prefix_search_effective(&full_path) {
                return e;
            }
            let lookup = if flags & AT_SYMLINK_NOFOLLOW != 0 {
                crate::fs::lookup_no_follow(&full_path)
            } else {
                crate::fs::lookup(&full_path)
            };
            match lookup {
                Ok(inode) => Some((inode, false)),
                Err(err) => return fs_info_errno(err),
            }
        };

    let (inode, force_regular_exec) = match target {
        Some(target) => target,
        None => return -9,
    };

    let mut stat = match inode.stat() {
        Ok(s) => s,
        Err(_) => return -5,
    };
    if force_regular_exec {
        stat.file_type = crate::fs::FileType::Regular;
        stat.mode.0 = 0o100755;
    }

    // POSIX S_IF* type bits in the high half of mode.
    use crate::fs::FileType;
    let ifmt: u32 = match stat.file_type {
        FileType::Regular    => 0o100000,
        FileType::Directory  => 0o040000,
        FileType::Symlink    => 0o120000,
        FileType::CharDevice => 0o020000,
        FileType::BlockDevice=> 0o060000,
        FileType::Pipe       => 0o010000,
        FileType::Socket     => 0o140000,
    };
    let full_mode: u32 = (stat.mode.0 & 0o7777) | ifmt;

    // struct statx layout (256 B) — see <linux/stat.h>.
    let mut buf = [0u8; 256];
    let put_u16 = |b: &mut [u8], off: usize, v: u16| {
        b[off..off+2].copy_from_slice(&v.to_le_bytes());
    };
    let put_u32 = |b: &mut [u8], off: usize, v: u32| {
        b[off..off+4].copy_from_slice(&v.to_le_bytes());
    };
    let put_u64 = |b: &mut [u8], off: usize, v: u64| {
        b[off..off+8].copy_from_slice(&v.to_le_bytes());
    };
    let put_i64 = |b: &mut [u8], off: usize, v: i64| {
        b[off..off+8].copy_from_slice(&v.to_le_bytes());
    };
    let put_ts = |b: &mut [u8], off: usize, sec: i64| {
        put_i64(b, off, sec);
        // tv_nsec = 0, __reserved = 0
    };

    // stx_blksize and stx_attributes_mask are always informative.
    put_u32(&mut buf, 4, 4096);              // stx_blksize
    // stx_attributes: map inode flags to STATX_ATTR_* (Linux semantics)
    let iflags = inode.get_inode_flags();
    let mut stx_attrs: u64 = 0;
    const FS_COMPR_FL: u32     = 0x04;
    const FS_IMMUTABLE_FL: u32 = 0x10;
    const FS_APPEND_FL: u32    = 0x20;
    const FS_NODUMP_FL: u32    = 0x40;
    const STATX_ATTR_COMPRESSED: u64 = 0x0004;
    const STATX_ATTR_IMMUTABLE: u64  = 0x0010;
    const STATX_ATTR_APPEND: u64     = 0x0020;
    const STATX_ATTR_NODUMP: u64     = 0x0040;
    if iflags & FS_COMPR_FL != 0     { stx_attrs |= STATX_ATTR_COMPRESSED; }
    if iflags & FS_IMMUTABLE_FL != 0 { stx_attrs |= STATX_ATTR_IMMUTABLE; }
    if iflags & FS_APPEND_FL != 0    { stx_attrs |= STATX_ATTR_APPEND; }
    if iflags & FS_NODUMP_FL != 0    { stx_attrs |= STATX_ATTR_NODUMP; }
    put_u64(&mut buf, 8, stx_attrs);         // stx_attributes
    // stx_attributes_mask: which attrs we support
    put_u64(&mut buf, 56, STATX_ATTR_COMPRESSED | STATX_ATTR_IMMUTABLE | STATX_ATTR_APPEND | STATX_ATTR_NODUMP);

    let mut filled: u32 = 0;

    // Linux always fills the basic fields whether requested or not, then
    // reports actually-filled bits in stx_mask. We do the same selection
    // based on the request mask to make LTP `statx0[4-8]` mask checks PASS.
    let want = mask | STATX_BASIC_STATS; // always provide basic stats
    if want & STATX_NLINK != 0 {
        put_u32(&mut buf, 16, stat.nlink as u32);
        filled |= STATX_NLINK;
    }
    if want & STATX_UID != 0 {
        put_u32(&mut buf, 20, stat.uid);
        filled |= STATX_UID;
    }
    if want & STATX_GID != 0 {
        put_u32(&mut buf, 24, stat.gid);
        filled |= STATX_GID;
    }
    if want & (STATX_MODE | STATX_TYPE) != 0 {
        // stx_mode is __u16 at offset 28; high bits convey file type.
        put_u16(&mut buf, 28, (full_mode & 0xFFFF) as u16);
        if want & STATX_MODE != 0 { filled |= STATX_MODE; }
        if want & STATX_TYPE != 0 { filled |= STATX_TYPE; }
    }
    if want & STATX_INO != 0 {
        let mut ino = inode.ino() as u64;
        if pidfd_iid != 0 {
            ino ^= pidfd_iid.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(17);
        }
        put_u64(&mut buf, 32, ino);
        filled |= STATX_INO;
    }
    if want & STATX_SIZE != 0 {
        put_u64(&mut buf, 40, stat.size as u64);
        filled |= STATX_SIZE;
    }
    if want & STATX_BLOCKS != 0 {
        put_u64(&mut buf, 48, ((stat.size as u64) + 511) / 512);
        filled |= STATX_BLOCKS;
    }
    if want & STATX_ATIME != 0 {
        put_ts(&mut buf, 64, stat.atime as i64);
        filled |= STATX_ATIME;
    }
    if mask & STATX_BTIME != 0 {
        // We do not track btime — leave at 0 and DO NOT set the filled bit
        // (POSIX: stx_mask only includes fields we actually computed).
    }
    if want & STATX_CTIME != 0 {
        put_ts(&mut buf, 96, stat.ctime as i64);
        filled |= STATX_CTIME;
    }
    if want & STATX_MTIME != 0 {
        put_ts(&mut buf, 112, stat.mtime as i64);
        filled |= STATX_MTIME;
    }

    // stx_rdev_major/minor and stx_dev_major/minor.
    let (rdev_major, rdev_minor) = inode.device_id().unwrap_or((0, 0));
    put_u32(&mut buf, 128, rdev_major); // stx_rdev_major
    put_u32(&mut buf, 132, rdev_minor); // stx_rdev_minor
    put_u32(&mut buf, 136, 0);     // stx_dev_major
    put_u32(&mut buf, 140, 1);     // stx_dev_minor

    if mask & STATX_MNT_ID != 0 {
        // stx_mnt_id at offset 144 (kernel >= 5.8). We have a single mount id.
        put_u64(&mut buf, 144, 1);
        filled |= STATX_MNT_ID;
    }

    put_u32(&mut buf, 0, filled);

    if let Err(_) = UserSlice::<u8>::new_mut(statxbuf, buf.len()).write_bytes(&buf) {
        return -14;
    }
    0
}
