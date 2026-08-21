//! fcntl(2) syscall — file descriptor control operations.
//!
//! Covers all F_* commands defined in Linux uapi (fcntl.h / asm-generic/fcntl.h).

use alloc::sync::Arc;
use crate::fs::fd as fd_table;
use crate::fs::FileDescriptor;

/// Count the number of open file descriptions (across all processes) pointing
/// to the given inode. Used by `F_SETLEASE` to approximate the inode reference
/// count that Linux checks via `i_writecount` / `i_readcount`.
fn count_inode_open_descriptions(inode_id: usize) -> usize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let mut total = 0usize;
    if let Some(ref t) = *table {
        for pid in t.pids() {
            if let Some(process) = t.get(pid) {
                if process.is_thread {
                    continue;
                }
                for slot in process.fd_table.iter() {
                    if let Some(fd_obj) = slot {
                        if fd_obj.file.ino() == inode_id {
                            total += 1;
                        }
                    }
                }
            }
        }
    }
    total
}

fn pending_unblocked_signal(pid: usize) -> bool {
    crate::syscall::signal::pending_interrupting_signal(pid)
}

fn fd_lock_access_ok(fd_obj: &Arc<FileDescriptor>, l_type: i16) -> bool {
    const F_RDLCK: i16 = 0;
    const F_WRLCK: i16 = 1;
    const O_ACCMODE: u32 = 0o3;
    const O_RDONLY: u32 = 0o0;
    const O_WRONLY: u32 = 0o1;
    const O_PATH: u32 = 0o10000000;
    if fd_obj.flags & O_PATH != 0 {
        return false;
    }
    let accmode = fd_obj.flags & O_ACCMODE;
    match l_type {
        F_RDLCK => accmode != O_WRONLY,
        F_WRLCK => accmode != O_RDONLY,
        _ => true,
    }
}

fn refresh_async_owner(fd_obj: &Arc<FileDescriptor>) {
    use core::sync::atomic::Ordering;
    let owner_type = fd_obj.f_owner_type.load(Ordering::Relaxed);
    let owner_pid = fd_obj.f_owner_pid.load(Ordering::Relaxed);
    let signal = fd_obj.f_owner_signal.load(Ordering::Relaxed);
    let enabled = fd_obj.f_async_enabled.load(Ordering::Relaxed);
    if let Some(file) = fd_obj.file.as_file() {
        file.set_async_owner(owner_type, owner_pid, signal, enabled);
    }
}

pub(in crate::syscall) fn sys_fcntl(fd: usize, cmd: usize, arg: usize) -> isize {
    use crate::syscall::fd::{get_fd_obj, fs_error_to_errno};

    // Linux: include/uapi/linux/fcntl.h, include/uapi/asm-generic/fcntl.h
    const F_DUPFD:         usize = 0;
    const F_GETFD:         usize = 1;
    const F_SETFD:         usize = 2;
    const F_GETFL:         usize = 3;
    const F_SETFL:         usize = 4;
    const F_GETLK:         usize = 5;
    const F_SETLK:         usize = 6;
    const F_SETLKW:        usize = 7;
    const F_SETOWN:        usize = 8;
    const F_GETOWN:        usize = 9;
    const F_SETSIG:        usize = 10;
    const F_GETSIG:        usize = 11;
    const F_SETOWN_EX:     usize = 15;
    const F_GETOWN_EX:     usize = 16;
    const F_GETOWNER_UIDS: usize = 17;
    const F_OFD_GETLK:     usize = 36;
    const F_OFD_SETLK:     usize = 37;
    const F_OFD_SETLKW:    usize = 38;
    const F_DUPFD_CLOEXEC: usize = 1030;
    const F_SETPIPE_SZ:    usize = 1031;
    const F_GETPIPE_SZ:    usize = 1032;
    const F_ADD_SEALS:     usize = 1033;
    const F_GET_SEALS:     usize = 1034;
    const F_SETLEASE:      usize = 1024;
    const F_GETLEASE:      usize = 1025;
    const F_NOTIFY:        usize = 1026;

    const FD_CLOEXEC: usize = 1;
    // struct flock l_type values
    const F_UNLCK: i16 = 2;

    match cmd {
        F_DUPFD | F_DUPFD_CLOEXEC => {
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref mut t) = *table {
                if let Some(pid) = t.current {
                    if let Some(process) = t.get_fd_process_mut(pid) {
                        // POSIX: F_DUPFD must honor RLIMIT_NOFILE and return EMFILE
                        // when the per-process fd table is full (LTP fcntl12).
                        return match fd_table::duplicate(process, fd, arg, true) {
                            Ok(new_fd) => {
                                if cmd == F_DUPFD_CLOEXEC {
                                    if let Err(errno) = fd_table::set_cloexec(process, new_fd, true) {
                                        return errno;
                                    }
                                }
                                new_fd as isize
                            }
                            Err(errno) => errno,
                        };
                    }
                }
            }
            -9
        }
        F_GETFD => {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref t) = *table {
                if let Some(pid) = t.current {
                    if let Some(proc) = t.get_fd_process(pid) {
                        if fd_table::is_open(proc, fd) {
                            return if fd_table::is_cloexec(proc, fd) { FD_CLOEXEC as isize } else { 0 };
                        }
                    }
                }
            }
            -9
        }
        F_SETFD => {
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref mut t) = *table {
                if let Some(pid) = t.current {
                    if let Some(process) = t.get_fd_process_mut(pid) {
                        return match fd_table::set_cloexec(process, fd, (arg & FD_CLOEXEC) != 0) {
                            Ok(()) => 0,
                            Err(errno) => errno,
                        };
                    }
                }
            }
            -9
        }
        F_GETFL => {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref t) = *table {
                if let Some(pid) = t.current {
                    if let Some(proc) = t.get_fd_process(pid) {
                        if let Some(fd_obj) = fd_table::get(proc, fd) {
                            return (fd_obj.status_flags.load(core::sync::atomic::Ordering::Relaxed) & !0o2000000u32) as isize;
                        }
                    }
                }
            }
            -9 // EBADF
        }
        F_SETFL => {
            const O_ACCMODE: u32 = 0o3;
            const O_ASYNC: u32 = 0o20000;
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            let old = fd_obj.status_flags.load(core::sync::atomic::Ordering::Relaxed);
            let new_flags = (old & O_ACCMODE) | ((arg as u32) & !O_ACCMODE);
            fd_obj.status_flags.store(new_flags, core::sync::atomic::Ordering::Relaxed);
            fd_obj.f_async_enabled.store(new_flags & O_ASYNC != 0, core::sync::atomic::Ordering::Relaxed);
            refresh_async_owner(&fd_obj);
            0
        }

        // POSIX / OFD advisory file lock query: F_GETLK / F_OFD_GETLK
        F_GETLK | F_OFD_GETLK => {
            use crate::fs::file_lock::{
                F_UNLCK, F_RDLCK, F_WRLCK, LockOwner, get_lock, ofd_id_from_arc, posix_owner_for_pid,
                parse_flock, serialize_flock,
            };
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            let inode_id = fd_obj.file.ino();
            let mut user_bytes = match crate::syscall::user::read_user_buffer(arg as *const u8, 32) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let cur_off = fd_obj.offset.load(core::sync::atomic::Ordering::Relaxed) as i64;
            let file_size = fd_obj.file.stat().map(|s| s.size as i64).unwrap_or(0);
            let mut requested = match parse_flock(&user_bytes, cur_off, file_size) {
                Some(l) => l,
                None => return -22,
            };
            // l_type must be F_RDLCK or F_WRLCK on GETLK (POSIX).
            if requested.l_type != F_RDLCK && requested.l_type != F_WRLCK {
                return -22;
            }
            if cmd == F_OFD_GETLK {
                let l_pid = i32::from_le_bytes([user_bytes[24], user_bytes[25], user_bytes[26], user_bytes[27]]);
                if l_pid != 0 {
                    return -22;
                }
            }
            requested.owner = if cmd == F_OFD_GETLK {
                LockOwner::Ofd(ofd_id_from_arc(&fd_obj))
            } else {
                let pid = {
                    let t = crate::sched::process::PROCESS_TABLE.lock();
                    t.as_ref().and_then(|tt| tt.current).unwrap_or(0)
                };
                LockOwner::Pid(posix_owner_for_pid(pid))
            };
            let out = match get_lock(inode_id, &requested) {
                Some(conflict) => serialize_flock(&conflict).to_vec(),
                None => {
                    user_bytes[0..2].copy_from_slice(&F_UNLCK.to_le_bytes());
                    user_bytes
                }
            };
            if crate::syscall::user::write_user_buffer(arg as *mut u8, &out).is_err() {
                return -14;
            }
            0
        }

        // POSIX / OFD advisory file lock acquire/release: F_SETLK / F_SETLKW
        // and F_OFD_SETLK / F_OFD_SETLKW.
        F_SETLK | F_OFD_SETLK | F_SETLKW | F_OFD_SETLKW => {
            use crate::fs::file_lock::{
                F_RDLCK, F_UNLCK, F_WRLCK, LockOwner, ofd_id_from_arc, posix_owner_for_pid,
                parse_flock, set_lock_nb, set_lock_w,
            };
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            let inode_id = fd_obj.file.ino();
            let user_bytes = match crate::syscall::user::read_user_buffer(arg as *const u8, 32) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let cur_off = fd_obj.offset.load(core::sync::atomic::Ordering::Relaxed) as i64;
            let file_size = fd_obj.file.stat().map(|s| s.size as i64).unwrap_or(0);
            let mut requested = match parse_flock(&user_bytes, cur_off, file_size) {
                Some(l) => l,
                None => return -22,
            };
            // l_type must be one of RDLCK/WRLCK/UNLCK.
            if requested.l_type != F_RDLCK
                && requested.l_type != F_WRLCK
                && requested.l_type != F_UNLCK
            {
                return -22;
            }
            // F_OFD_SETLK / F_OFD_SETLKW require l_whence == SEEK_SET and a
            // non-negative absolute byte range.
            let is_ofd = matches!(cmd, F_OFD_SETLK | F_OFD_SETLKW);
            if is_ofd {
                let l_pid = i32::from_le_bytes([user_bytes[24], user_bytes[25], user_bytes[26], user_bytes[27]]);
                if l_pid != 0 {
                    return -22;
                }
            }
            if !fd_lock_access_ok(&fd_obj, requested.l_type) {
                return -9;
            }
            let pid = {
                let t = crate::sched::process::PROCESS_TABLE.lock();
                t.as_ref().and_then(|tt| tt.current).unwrap_or(0)
            };
            requested.owner = if is_ofd {
                LockOwner::Ofd(ofd_id_from_arc(&fd_obj))
            } else {
                LockOwner::Pid(posix_owner_for_pid(pid))
            };
            let blocking = matches!(cmd, F_SETLKW | F_OFD_SETLKW);
            let res = if blocking {
                drop(user_bytes);
                drop(fd_obj);
                set_lock_w(inode_id, requested, pid, || pending_unblocked_signal(pid))
            } else {
                set_lock_nb(inode_id, requested)
            };
            match res {
                Ok(()) => 0,
                Err(errno) => -(errno as isize),
            }
        }

        // F_GETOWN / F_SETOWN
        F_GETOWN => {
            use core::sync::atomic::Ordering;
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            let owner_type = fd_obj.f_owner_type.load(Ordering::Relaxed);
            let owner_pid = fd_obj.f_owner_pid.load(Ordering::Relaxed);
            if owner_type == 2 {
                -(owner_pid as isize)
            } else {
                owner_pid as isize
            }
        }
        F_SETOWN => {
            use core::sync::atomic::Ordering;
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            let owner = arg as isize as i32;
            if owner < 0 {
                fd_obj.f_owner_type.store(2, Ordering::Relaxed);
                fd_obj.f_owner_pid.store(owner.saturating_neg(), Ordering::Relaxed);
            } else {
                fd_obj.f_owner_type.store(1, Ordering::Relaxed);
                fd_obj.f_owner_pid.store(owner, Ordering::Relaxed);
            }
            refresh_async_owner(&fd_obj);
            0
        }
        F_GETSIG => {
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            fd_obj.f_owner_signal.load(core::sync::atomic::Ordering::Relaxed) as isize
        }
        F_SETSIG => {
            let sig = arg as i32;
            if !(0..=64).contains(&sig) {
                return -22;
            }
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            fd_obj.f_owner_signal.store(sig, core::sync::atomic::Ordering::Relaxed);
            refresh_async_owner(&fd_obj);
            0
        }
        F_GETOWN_EX => {
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            let mut out = [0u8; 8];
            let owner_type = fd_obj.f_owner_type.load(core::sync::atomic::Ordering::Relaxed);
            let owner_pid = fd_obj.f_owner_pid.load(core::sync::atomic::Ordering::Relaxed);
            out[0..4].copy_from_slice(&owner_type.to_le_bytes());
            out[4..8].copy_from_slice(&owner_pid.to_le_bytes());
            if crate::syscall::user::write_user_buffer(arg as *mut u8, &out).is_err() {
                return -14;
            }
            0
        }
        F_SETOWN_EX => {
            let buf = match crate::syscall::user::read_user_buffer(arg as *const u8, 8) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let owner_type = i32::from_le_bytes(buf[0..4].try_into().unwrap());
            let owner_pid = i32::from_le_bytes(buf[4..8].try_into().unwrap());
            if !(0..=2).contains(&owner_type) || owner_pid < 0 {
                return -22;
            }
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            fd_obj.f_owner_type.store(owner_type, core::sync::atomic::Ordering::Relaxed);
            fd_obj.f_owner_pid.store(owner_pid, core::sync::atomic::Ordering::Relaxed);
            refresh_async_owner(&fd_obj);
            0
        }
        F_GETOWNER_UIDS => {
            let out = [0u8; 8];
            if crate::syscall::user::write_user_buffer(arg as *mut u8, &out).is_err() {
                return -14;
            }
            0
        }
        F_NOTIFY => {
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            crate::fs::dnotify::set_watch(fd_obj, fd as i32, arg as u32)
        }

        F_GETPIPE_SZ => {
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            match fd_obj.file.as_file().and_then(|file| file.pipe_capacity().ok()) {
                Some(size) => size as isize,
                None => -9,
            }
        }
        F_SETPIPE_SZ => {
            if arg > i32::MAX as usize {
                return -22;
            }
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            match fd_obj.file.as_file() {
                Some(file) => match file.set_pipe_capacity(arg) {
                    Ok(size) => size as isize,
                    Err(err) => fs_error_to_errno(err),
                },
                None => -9,
            }
        }
        F_GETLEASE => {
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            let inode_id = fd_obj.file.ino();
            let ofd_id = Arc::as_ptr(&fd_obj) as usize;
            let kind = crate::fs::file_lease::get_lease(inode_id, ofd_id);
            fd_obj.lease_type.store(kind, core::sync::atomic::Ordering::Relaxed);
            kind as isize
        }
        F_SETLEASE => {
            let lease = arg as i32;
            if lease != crate::fs::file_lease::F_RDLCK
                && lease != crate::fs::file_lease::F_WRLCK
                && lease != crate::fs::file_lease::F_UNLCK
            {
                return -22;
            }
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            const O_ACCMODE: u32 = 0o3;
            let accmode = fd_obj.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_ACCMODE;
            let inode_id = fd_obj.file.ino();
            let ofd_id = Arc::as_ptr(&fd_obj) as usize;
            let refcount = count_inode_open_descriptions(inode_id);
            let owner_pid = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                table.as_ref().and_then(|t| t.current).unwrap_or(0)
            };
            let target_type = fd_obj.f_owner_type.load(core::sync::atomic::Ordering::Relaxed);
            let target_pid = fd_obj.f_owner_pid.load(core::sync::atomic::Ordering::Relaxed);
            let signal = fd_obj.f_owner_signal.load(core::sync::atomic::Ordering::Relaxed);
            match crate::fs::file_lease::fcntl_setlease(
                inode_id,
                refcount,
                accmode,
                ofd_id,
                owner_pid,
                target_type,
                target_pid,
                signal,
                lease,
            ) {
                Ok(()) => {
                    fd_obj.lease_type.store(lease, core::sync::atomic::Ordering::Relaxed);
                    0
                }
                Err(errno) => -(errno as isize),
            }
        }
        F_GET_SEALS => {
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            if let Some(shared) = fd_obj.file.shared_seals() {
                shared.load(core::sync::atomic::Ordering::Acquire) as isize
            } else {
                fd_obj.seals.load(core::sync::atomic::Ordering::Acquire) as isize
            }
        }
        F_ADD_SEALS => {
            let fd_obj = match get_fd_obj(fd) {
                Some(f) => f,
                None => return -9,
            };
            // Linux memfd defined seal bits: SEAL/SHRINK/GROW/WRITE/FUTURE_WRITE = 0x1f.
            const KNOWN_SEALS: u32 = 0x001f;
            let new_seals = arg as u32;
            if new_seals == 0 || new_seals & !KNOWN_SEALS != 0 {
                return -22; // -EINVAL
            }
            const O_ACCMODE: u32 = 0o3;
            const O_RDONLY: u32 = 0o0;
            if fd_obj.status_flags.load(core::sync::atomic::Ordering::Relaxed) & O_ACCMODE == O_RDONLY {
                return -1; // -EPERM
            }
            if let Some(shared) = fd_obj.file.shared_seals() {
                let cur = shared.load(core::sync::atomic::Ordering::Acquire);
                if cur & 0x0001 != 0 {
                    return -1; // -EPERM
                }
                const F_SEAL_WRITE_BIT: u32 = 0x0008;
                if new_seals & F_SEAL_WRITE_BIT != 0 && cur & F_SEAL_WRITE_BIT == 0 {
                    if fd_obj.file.writable_maps_count() > 0 {
                        return -16; // -EBUSY: writable mappings exist
                    }
                }
                let new = cur | new_seals;
                if shared
                    .compare_exchange(cur, new, core::sync::atomic::Ordering::AcqRel, core::sync::atomic::Ordering::Acquire)
                    .is_err()
                {
                    return -16; // -EBUSY
                }
                0
            } else {
                let old = fd_obj.seals.load(core::sync::atomic::Ordering::Acquire);
                if old & 0x0001 != 0 && new_seals & !old != 0 {
                    return -1;
                }
                fd_obj.seals.store(old | new_seals, core::sync::atomic::Ordering::Release);
                0
            }
        }

        _ => -22,
    }
}
