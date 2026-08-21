//! File operation syscall entry points split out of syscall/fd.rs.

/// ftruncate(fd, length)
#[allow(dead_code)]
pub fn sys_ftruncate(fd: usize, length: usize) -> isize {
    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    let inode_id = fd_obj.file.ino();
    if let Err(errno) = crate::fs::file_lease::break_lease_blocking(inode_id, 1, true) {
        return -(errno as isize);
    }
    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    if !fd_obj.writable() {
        return -22;
    }
    if fd_obj.file.file_type() != crate::fs::FileType::Regular {
        return -22;
    }
    let file = match fd_obj.file.as_file() {
        Some(f) => f,
        None => return -22,
    };
    if (length as i64) < 0 {
        return -22;
    }
    if let Some(e) = crate::fs::file_limits::rlimit_fsize_exceeded(length) {
        return e;
    }
    // For a hugetlbfs file, truncate releases the inode-owned huge pages and
    // reservations of every whole huge page at index >= round_up(length) back
    // to the global pool (Linux `hugetlb_vmtruncate` removes inode pages from
    // that page offset onward). This is what frees MAP_SHARED huge pages after
    // munmap (hugemmap08/23) — munmap alone keeps them charged to the inode.
    //
    // The release is keyed on the inode's own huge accounting set, NOT on i_size:
    // an mmap'd+touched hugetlbfs file does not grow i_size, and the file may be
    // unlinked (tst_creat_unlinked) so a path-based fstype probe is unreliable.
    // We therefore release unconditionally — for a non-hugetlbfs inode there is
    // no `HUGE_INODE_ACCT` entry and no cached huge page, so both calls are
    // cheap no-ops.
    let hpage = crate::mm::huge_page_size_kb() * 1024;
    let huge_truncate = if hpage != 0 {
        let start_idx = (length + hpage - 1) / hpage;
        Some((fd_obj.file.ino(), start_idx, usize::MAX))
    } else {
        None
    };
    match file.truncate(length) {
        Ok(()) => {
            if let Some((ino, start_idx, end_idx)) = huge_truncate {
                crate::mm::address_space::shared_huge_cache_evict_range(ino, start_idx, end_idx);
                crate::mm::hugetlb_shared_release_range(ino, start_idx, end_idx);
            }
            let current = fd_obj.offset.load(core::sync::atomic::Ordering::Relaxed);
            if current > length {
                fd_obj.offset.store(length, core::sync::atomic::Ordering::Relaxed);
            }
            crate::syscall::fd::notify_inotify_modify(&fd_obj);
            0
        }
        Err(crate::fs::FsError::InvalidArgument) => -22,
        Err(crate::fs::FsError::PermissionDenied) => -13,
        Err(_) => -5,
    }
}

/// truncate(path, length) — path-based file truncation.
#[allow(dead_code)]
pub fn sys_truncate(path_ptr: *const u8, length: usize) -> isize {
    let path = match crate::syscall::user::read_path(path_ptr) {
        Ok(s) => s,
        Err(e) => return e,
    };
    if path.is_empty() {
        return -2;
    }
    if (length as i64) < 0 {
        return -22;
    }
    let full_path = match crate::syscall::open::resolve_at_path(-100isize, &path) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let inode = match crate::fs::lookup(&full_path) {
        Ok(i) => i,
        Err(err) => return crate::syscall::fd::fs_error_to_errno(err),
    };
    if inode.file_type() == crate::fs::FileType::Directory {
        return -21;
    }
    let stat = match inode.stat() {
        Ok(s) => s,
        Err(err) => return crate::syscall::fd::fs_error_to_errno(err),
    };
    if !truncate_write_allowed(&stat) {
        return -13;
    }
    if stat.file_type == crate::fs::FileType::Regular
        && crate::fs::is_write_denied(&full_path)
    {
        return -26;
    }
    let file = match inode.as_file() {
        Some(f) => f,
        None => return -22,
    };
    if let Some(e) = crate::fs::file_limits::rlimit_fsize_exceeded(length) {
        return e;
    }
    match file.truncate(length) {
        Ok(()) => {
            if crate::syscall::path::any_fsnotify_active() {
                crate::fs::inotify::notify_file_path(&full_path, crate::fs::inotify::IN_MODIFY);
                crate::fs::dnotify::notify_file_path(&full_path, crate::fs::dnotify::DN_MODIFY);
                crate::fs::fanotify::notify_file_path(&full_path, crate::fs::fanotify::FAN_MODIFY);
            }
            0
        }
        Err(crate::fs::FsError::InvalidArgument) => -22,
        Err(crate::fs::FsError::PermissionDenied) => -13,
        Err(_) => -5,
    }
}

fn truncate_write_allowed(stat: &crate::fs::FileStat) -> bool {
    const CAP_DAC_OVERRIDE: u64 = 1u64 << 1;
    let (euid, egid, caps, groups) = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table
            .as_ref()
            .and_then(|t| {
                t.current.and_then(|pid| {
                    t.get(pid)
                        .map(|p| (p.euid, p.egid, p.cap_effective, p.supplementary_groups.clone()))
                })
            })
            .unwrap_or((0, 0, u64::MAX, alloc::vec::Vec::new()))
    };
    if euid == 0 || (caps & CAP_DAC_OVERRIDE) != 0 {
        return true;
    }
    let bits = stat.mode.0 & 0o777;
    let granted = if euid == stat.uid {
        (bits >> 6) & 0o7
    } else if egid == stat.gid || groups.iter().any(|&g| g == stat.gid) {
        (bits >> 3) & 0o7
    } else {
        bits & 0o7
    };
    granted & 0o2 != 0
}

/// fallocate(fd, mode, offset, len) - preallocate/manage file space.
#[allow(dead_code)]
pub fn sys_fallocate(fd: usize, mode: usize, offset: usize, len: usize) -> isize {
    const FALLOC_FL_KEEP_SIZE:      usize = 0x01;
    const FALLOC_FL_PUNCH_HOLE:     usize = 0x02;
    const FALLOC_FL_COLLAPSE_RANGE: usize = 0x08;
    const FALLOC_FL_ZERO_RANGE:     usize = 0x10;
    const FALLOC_FL_INSERT_RANGE:   usize = 0x20;
    const FALLOC_FL_UNSHARE_RANGE:  usize = 0x40;

    if (offset as isize) < 0 || (len as isize) <= 0 {
        return -22;
    }
    let end = match offset.checked_add(len) {
        Some(v) => v,
        None => return -27,
    };
    const MAX_FILE_SIZE: usize = i64::MAX as usize;
    if end > MAX_FILE_SIZE {
        return -27;
    }

    let unsupported = FALLOC_FL_COLLAPSE_RANGE | FALLOC_FL_INSERT_RANGE;
    if mode & unsupported != 0 {
        return -95;
    }
    let need_zero = mode & (FALLOC_FL_ZERO_RANGE | FALLOC_FL_PUNCH_HOLE) != 0;
    let _ = FALLOC_FL_UNSHARE_RANGE;

    if mode & FALLOC_FL_PUNCH_HOLE != 0 && mode & FALLOC_FL_KEEP_SIZE == 0 {
        return -22;
    }

    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    if !fd_obj.writable() {
        return -9;
    }
    let inode_id = fd_obj.file.ino();
    if let Err(errno) = crate::fs::file_lease::break_lease_blocking(inode_id, 1, false) {
        return -(errno as isize);
    }
    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    let file = match fd_obj.file.as_file() {
        Some(f) => f,
        None => return -22,
    };

    let current_size = match file.stat() {
        Ok(s) => s.size,
        Err(_) => return -5,
    };

    // hugetlbfs: fallocate operates on *whole huge pages*, not bytes, and every
    // (de)allocation must move pages in the global huge page pool (HugePages_Free
    // in /proc/meminfo). Preallocate rounds the range UP to huge-page boundaries
    // (offset down, offset+len up); FALLOC_FL_PUNCH_HOLE rounds it DOWN (offset
    // up, offset+len down) so only whole pages fully inside the range move — this
    // is exactly the alignment behaviour hugefallocate01/02 probe.
    let is_hugetlb = fd_obj
        .path
        .as_deref()
        .map(|p| crate::fs::mount::fstype_for_path(p).as_deref() == Some("hugetlbfs"))
        .unwrap_or(false);
    if is_hugetlb {
        let hpage = crate::mm::huge_page_size_kb() * 1024;
        if hpage == 0 {
            return -22;
        }
        let key = alloc::sync::Arc::as_ptr(&fd_obj.file) as *const () as usize;
        if mode & FALLOC_FL_PUNCH_HOLE != 0 {
            let aligned_start = (offset + hpage - 1) & !(hpage - 1);
            let aligned_end = end & !(hpage - 1);
            if aligned_end > aligned_start {
                crate::mm::hugetlb_file_punch(key, aligned_start / hpage, aligned_end / hpage);
            }
        } else {
            let aligned_start = offset & !(hpage - 1);
            let aligned_end = (end + hpage - 1) & !(hpage - 1);
            if crate::mm::hugetlb_file_alloc(key, aligned_start / hpage, aligned_end / hpage).is_err() {
                return -28; // -ENOSPC: huge page pool exhausted
            }
            if mode & FALLOC_FL_KEEP_SIZE == 0 && end > current_size {
                let _ = file.truncate(end);
            }
        }
        crate::syscall::fd::notify_inotify_modify(&fd_obj);
        return 0;
    }

    let keep_size = mode & (FALLOC_FL_KEEP_SIZE | FALLOC_FL_PUNCH_HOLE) != 0;
    let target_size = if keep_size {
        current_size
    } else {
        end.max(current_size)
    };

    if target_size > current_size {
        match file.truncate(target_size) {
            Ok(()) => {}
            Err(crate::fs::FsError::InvalidArgument) => return -22,
            Err(crate::fs::FsError::PermissionDenied) => return -13,
            Err(crate::fs::FsError::NoSpace) => return -28,
            Err(_) => return -5,
        }
    }

    // Preallocation (mode 0 / KEEP_SIZE / ZERO_RANGE): reserve backing pages
    // so later writes into the range cannot fail with ENOSPC (fallocate06).
    if mode & FALLOC_FL_PUNCH_HOLE == 0 {
        match file.allocate(offset, len) {
            Ok(()) => {}
            Err(crate::fs::FsError::NoSpace) => return -28,
            Err(_) => return -5,
        }
    }

    if need_zero {
        let zero_end = if mode & FALLOC_FL_PUNCH_HOLE != 0 {
            end.min(target_size)
        } else {
            end
        };
        if zero_end > offset {
            const ZERO_CHUNK: usize = 8 * 1024;
            let zeros = [0u8; ZERO_CHUNK];
            let mut cur = offset;
            while cur < zero_end {
                let chunk = (zero_end - cur).min(ZERO_CHUNK);
                match file.write(cur, &zeros[..chunk]) {
                    Ok(0) => return -28,
                    Ok(n) => cur += n,
                    Err(crate::fs::FsError::PermissionDenied) => return -13,
                    Err(crate::fs::FsError::NoSpace) => return -28,
                    Err(_) => return -5,
                }
            }
        }
    }

    crate::syscall::fd::notify_inotify_modify(&fd_obj);
    0
}
