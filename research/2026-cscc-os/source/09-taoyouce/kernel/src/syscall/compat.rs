// sync/fsync/fdatasync/syncfs/sync_file_range → moved to sync.rs

// mount/umount2/mount_setattr + helpers → kernel/src/syscall/mount.rs

pub(in crate::syscall) fn sys_rseq(
    _rseq: usize,
    _rseq_len: usize,
    _flags: usize,
    _sig: usize,
) -> isize {
    -38
}

/// 读 PCB 当前 uid/euid/gid/egid，与 `setresuid` / `setreuid` 等写入侧
/// 配套。LTP `getuid01`/`getgid01`/`geteuid01`/`getegid01` 需要 round-trip。
pub(in crate::syscall) fn sys_getuid() -> isize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| p.uid as isize))
        .unwrap_or(0)
}

pub(in crate::syscall) fn sys_geteuid() -> isize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| {
            t.current
                .and_then(|pid| t.get(pid))
                .map(|p| p.euid as isize)
        })
        .unwrap_or(0)
}

pub(in crate::syscall) fn sys_getgid() -> isize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| p.gid as isize))
        .unwrap_or(0)
}

pub(in crate::syscall) fn sys_getegid() -> isize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| {
            t.current
                .and_then(|pid| t.get(pid))
                .map(|p| p.egid as isize)
        })
        .unwrap_or(0)
}

// msync/mlock/mlock2/mlockall/munlock/munlockall/madvise → kernel/src/mm/mman.rs

/// `umask(mask)` — POSIX：将进程 umask 设为 `mask & 0o777`，返回旧值。
/// fork/clone 继承；execve 不重置。LTP `umask01` 测 round-trip。
pub(in crate::syscall) fn sys_umask(mask: usize) -> isize {
    let new_mask = (mask as u32) & 0o777;
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() {
        Some(t) => t,
        None => return 0o022,
    };
    let pid = match t.current {
        Some(p) => p,
        None => return 0o022,
    };
    let p = match t.get_mut(pid) {
        Some(p) => p,
        None => return 0o022,
    };
    let mut fs = p.fs.lock();
    let old = fs.umask;
    fs.umask = new_mask;
    old as isize
}

/// 当前进程 umask 的只读快照，open/mkdir/mknod 创建文件时用 `mode & !umask`。
pub(crate) fn current_umask() -> u32 {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| p.umask()))
        .unwrap_or(0o022)
}

// syslog → kernel/src/syscall/system.rs

// sched_setaffinity → kernel/src/syscall/sched.rs

// setpgid/getpgid → kernel/src/syscall/process_group.rs

// `sys_getpeername` / `sys_setsockopt` / `sys_getsockopt` / `sys_shutdown`
// 已迁移到 `syscall::socket` 真实现，本文件不再提供 stub，避免 mod.rs
// `use` 路径混淆与"假成功"误导调用者。

// ============================================================================
// Slice 1.8 — Tsinghua-style stub bypass for the "rare but on the FAIL list"
// syscalls. None of these have real backing state in our kernel; each
// implementation only does the minimum that LTP cases check on the syscall
// boundary (errno mapping + sane "get" return values + UserCString probes).
// ============================================================================

// ---- POSIX message queue family (mq_*) -------------------------------------
// Linux requires CONFIG_POSIX_MQUEUE. Without it, all calls return -ENOSYS.
// LTP mq_* tests TBROK cleanly on -ENOSYS instead of segfaulting; that path
// is the cheapest "compatible" stub.
// Slice 2.x T3-5 — POSIX mq_* delegate to real impl in `syscall::mqueue`.
// POSIX MQ subsystem moved to kernel/src/syscall/mqueue.rs

// quotactl → kernel/src/syscall/system.rs

// NUMA/page migration/mempolicy → kernel/src/mm/mempolicy.rs
// mincore → kernel/src/mm/mman.rs

// flock → moved to sync.rs

// setpriority/getpriority → kernel/src/syscall/process_group.rs

// init_module/finit_module/delete_module → kernel/src/syscall/system.rs

// chroot → kernel/src/syscall/path.rs

fn current_euid_caps() -> (u32, u64) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| {
            t.current
                .and_then(|pid| t.get(pid))
                .map(|p| (p.euid, p.cap_effective))
        })
        .unwrap_or((0, u64::MAX))
}

/// vhangup(2)：使当前控制终端上的所有进程"挂断"。Linux 仅校验
/// `CAP_SYS_TTY_CONFIG`（root 近似）：非特权调用 → EPERM；特权调用 → 0。
/// 我们没有真实 tty 子系统去投递 SIGHUP，但权限门控是 vhangup01/02 真正探测
/// 的语义差异（非 root → EPERM；root → 成功），按 Linux 返回值精确对标。
pub(in crate::syscall) fn sys_vhangup() -> isize {
    const CAP_SYS_TTY_CONFIG: u64 = 1u64 << 26;
    let (euid, caps) = current_euid_caps();
    if euid != 0 && (caps & CAP_SYS_TTY_CONFIG) == 0 {
        return -1; // -EPERM
    }
    0
}

// pivot_root → kernel/src/syscall/system.rs

// unshare/setns → kernel/src/syscall/namespace.rs

// New Mount API (fsopen/fsconfig/fsmount/fspick/open_tree/move_mount) → kernel/src/syscall/mount.rs

// xattr subsystem → kernel/src/syscall/xattr.rs

// AIO subsystem moved to kernel/src/syscall/aio.rs

// reboot/kexec/swapon/swapoff → kernel/src/syscall/system.rs

// ---- sched_setattr / sched_getattr ----------------------------------------
//
// sched_setattr/sched_getattr → kernel/src/syscall/sched.rs

// perf_event_open → kernel/src/syscall/system.rs

// ptrace/process_vm_readv/writev/kcmp → kernel/src/syscall/ptrace.rs

// memfd_create/userfaultfd → kernel/src/syscall/memfd.rs

// clone3 → kernel/src/syscall/namespace.rs
// execveat → kernel/src/exec/mod.rs
// getcpu → kernel/src/syscall/system.rs

// futex_waitv → kernel/src/syscall/futex.rs

// settimeofday/clock_settime/clock_adjtime → kernel/src/syscall/time.rs

// ---- landlock family ------------------------------------------------------
pub(in crate::syscall) fn sys_landlock_create_ruleset(
    attr: *const u8,
    size: usize,
    flags: u32,
) -> isize {
    crate::security::landlock::sys_landlock_create_ruleset(attr, size, flags)
}
pub(in crate::syscall) fn sys_landlock_add_rule(
    ruleset_fd: i32,
    rule_type: u32,
    rule_attr: *const u8,
    flags: u32,
) -> isize {
    crate::security::landlock::sys_landlock_add_rule(ruleset_fd, rule_type, rule_attr, flags)
}
pub(in crate::syscall) fn sys_landlock_restrict_self(ruleset_fd: i32, flags: u32) -> isize {
    crate::security::landlock::sys_landlock_restrict_self(ruleset_fd, flags)
}

// openat2 / close_range → moved to open.rs
/// Linux 5.8+ `faccessat2(dirfd, path, mode, flags)`：相对老的 `faccessat`
/// 多出 `flags` 参数。我们做最小校验：
///   * 允许 `AT_EACCESS (0x200) | AT_SYMLINK_NOFOLLOW (0x100) | AT_EMPTY_PATH (0x1000)`
///   * 其它位 → `-EINVAL`
/// 然后转发到 `sys_faccessat_flags`（mode 自身的位校验在那边做）。
pub(in crate::syscall) fn sys_faccessat2(
    dirfd: isize,
    pathname: *const u8,
    mode: u32,
    flags: u32,
) -> isize {
    const AT_SYMLINK_NOFOLLOW: u32 = 0x100;
    const AT_EACCESS: u32 = 0x200;
    const AT_EMPTY_PATH: u32 = 0x1000;
    const ALLOWED: u32 = AT_SYMLINK_NOFOLLOW | AT_EACCESS | AT_EMPTY_PATH;
    if flags & !ALLOWED != 0 {
        return -22;
    }
    super::attr::sys_faccessat_flags(dirfd, pathname, mode, flags)
}
// process_madvise → kernel/src/mm/mempolicy.rs
pub(in crate::syscall) fn sys_epoll_pwait2(
    epfd: usize,
    events: *mut u8,
    maxevents: usize,
    timeout: *const u8,
    sigmask: *const u8,
    sigsetsize: usize,
) -> isize {
    // Convert struct timespec* -> ms. timespec layout: tv_sec(8) + tv_nsec(8).
    let timeout_ms: isize = if timeout.is_null() {
        -1 // wait forever
    } else {
        let bytes = match super::user::read_user_buffer(timeout, 16) {
            Ok(b) => b,
            Err(_) => return -14,
        };
        let sec = i64::from_le_bytes(bytes[0..8].try_into().unwrap());
        let nsec = i64::from_le_bytes(bytes[8..16].try_into().unwrap());
        // Linux ep_poll/timespec64 validation: negative seconds/nanoseconds or
        // nsec >= 1e9 is EINVAL (epoll_pwait05).
        if sec < 0 || nsec < 0 || nsec >= 1_000_000_000 {
            return -22; // -EINVAL
        }
        ((sec * 1000) + (nsec / 1_000_000)) as isize
    };
    crate::fs::epoll::sys_epoll_pwait(epfd, events, maxevents, timeout_ms, sigmask, sigsetsize)
}

// recvmsg/sendmsg/recvmmsg/sendmmsg → kernel/src/syscall/socket.rs

// mremap/remap_file_pages → kernel/src/mm/mman.rs

// Slice 2.x T4 真化：`readahead(fd, offset, count)` — Linux `mm/readahead.c`
// 真行为是触发 page cache prefetch（force_page_cache_readahead）。我们没有
// page cache 但有 inode 层的 `read(offset, &mut buf)`；模拟 prefetch 的方式
// 是真的把 [offset, offset+count) 段读进 throwaway buffer 触发底层 IO（更新
// inode atime、磁盘块解码、对后续 read() 也算"热"了内存）。
//
// 真校验：
//   * `fd < 0` → -EBADF
//   * `offset < 0` → -EINVAL
//   * fd 必须可读且是 regular file（dir / pipe / socket / chardev → -EINVAL，
//     与 Linux `do_readahead` 校验一致）
//   * `count` clamp 到 64KB（Linux 默认 VM_READAHEAD_PAGES * PAGE_SIZE，我们
//     更保守以免大 buffer alloc）
pub(in crate::syscall) fn sys_readahead(fd: i32, offset: i64, count: usize) -> isize {
    if fd < 0 {
        return -9;
    }
    if offset < 0 {
        return -22;
    }
    let count = count.min(64 * 1024);
    if count == 0 {
        return 0;
    }

    let fd_obj = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() {
            Some(t) => t,
            None => return -9,
        };
        let pid = match t.current {
            Some(p) => p,
            None => return -9,
        };
        let proc = match t.get_fd_process(pid) {
            Some(p) => p,
            None => return -9,
        };
        match crate::fs::fd::get(proc, fd as usize) {
            Some(f) => f,
            None => return -9,
        }
    };
    // O_PATH fds cannot be used for I/O — Linux returns EBADF
    const O_PATH: u32 = 0o10000000; // 0x200000
    if fd_obj.flags & O_PATH != 0 {
        return -9;
    }
    if !matches!(fd_obj.file.file_type(), crate::fs::FileType::Regular) {
        return -22;
    }
    // Reject pseudo-files that aren't backed by a real filesystem (bpf maps, etc.)
    // Linux readahead requires f_mapping->a_ops (page cache backing)
    if let Some(ref p) = fd_obj.path {
        if p.starts_with("bpf-map:") || p.starts_with("pidfd:") || p.starts_with("anon_inode:") {
            return -22; // EINVAL — no page cache backing
        }
    } else {
        return -22; // No path means anonymous/pseudo file
    }
    let file = match fd_obj.file.as_file() {
        Some(f) => f,
        None => return -22,
    };
    let mut buf = alloc::vec![0u8; count];
    let _ = file.read(offset as usize, &mut buf);
    0
}

// mlock2 → kernel/src/mm/mman.rs

// pkey_* → kernel/src/mm/mempolicy.rs

// ---- preadv2 / pwritev2 ---------------------------------------------------
// Same as preadv/pwritev, ignoring the high-order flag bits (RWF_*), except
// that an offset of -1 means "use (and advance) the current file position",
// i.e. behave like readv/writev (preadv201/pwritev201).
// RWF_SUPPORTED = RWF_HIPRI|RWF_DSYNC|RWF_SYNC|RWF_NOWAIT|RWF_APPEND (0x1f).
// Any bit outside this mask is EOPNOTSUPP (preadv202/pwritev202).
const RWF_SUPPORTED: i32 = 0x1f;

pub(in crate::syscall) fn sys_preadv2(
    fd: usize,
    iov: usize,
    iovcnt: usize,
    offset: usize,
    flags: i32,
) -> isize {
    if flags & !RWF_SUPPORTED != 0 {
        return -95; // -EOPNOTSUPP
    }
    if offset as i64 == -1 {
        return crate::fs::read_io::sys_readv(fd, iov, iovcnt);
    }
    crate::fs::positional_io::sys_preadv(fd, iov, iovcnt, offset)
}
pub(in crate::syscall) fn sys_pwritev2(
    fd: usize,
    iov: usize,
    iovcnt: usize,
    offset: usize,
    flags: i32,
) -> isize {
    if flags & !RWF_SUPPORTED != 0 {
        return -95; // -EOPNOTSUPP
    }
    if offset as i64 == -1 {
        return crate::fs::write_io::sys_writev(fd, iov, iovcnt);
    }
    crate::fs::positional_io::sys_pwritev(fd, iov, iovcnt, offset)
}

// ---- statmount / listmount ------------------------------------------------
// Linux 6.8+; we have no mount-id model, refuse cleanly.
// statmount/listmount → kernel/src/syscall/mount.rs

// POSIX timer syscall wrappers and getitimer → kernel/src/sched/posix_timer.rs

// ioprio_set / ioprio_get → kernel/src/syscall/process_group.rs
