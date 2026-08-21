//! memfd / userfaultfd subsystem — per Linux mm/memfd.c, fs/userfaultfd.c
//!
//! Extracted from compat.rs. Implements:
//! * memfd_create(2) — create anonymous memory-backed fd
//! * userfaultfd(2) — user-space page fault handling (stub)

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

// ============================================================================
// memfd_create(2) — per Linux mm/memfd.c
// ============================================================================

pub(in crate::syscall) fn sys_memfd_create(name: *const u8, flags: u32) -> isize {
    const MFD_CLOEXEC:       u32 = 0x0001;
    const MFD_ALLOW_SEALING: u32 = 0x0002;
    const MFD_HUGETLB:       u32 = 0x0004;
    const MFD_HUGE_MASK:     u32 = 0x3f << 26;
    const KNOWN_FLAGS: u32 = MFD_CLOEXEC | MFD_ALLOW_SEALING | MFD_HUGETLB | MFD_HUGE_MASK;
    if flags & !KNOWN_FLAGS != 0 {
        return -22;
    }
    if flags & MFD_HUGE_MASK != 0 && flags & MFD_HUGETLB == 0 {
        return -22;
    }
    let hugetlb_size: usize = if flags & MFD_HUGETLB != 0 {
        let log2 = (flags & MFD_HUGE_MASK) >> 26;
        let log2 = if log2 == 0 { 21 } else { log2 };
        // 一个合法编码但本机无对应 hugetlb 池的大页尺寸（如 MFD_HUGE_2GB/16GB,
        // log2 31/34）按 Linux hugetlb_file_setup 行为返 ENODEV——而非 EINVAL
        // (EINVAL 仅用于非法 flag 组合); memfd_create04 对缺池尺寸期望 ENODEV。
        if !(12..=30).contains(&log2) {
            return -19;
        }
        1usize << log2
    } else {
        0
    };

    if name.is_null() {
        return -14;
    }
    let raw_bytes: alloc::vec::Vec<u8> = {
        let mut bytes = alloc::vec::Vec::new();
        let mut terminated = false;
        for i in 0..250usize {
            match super::user::read_user_buffer(unsafe { name.add(i) }, 1) {
                Ok(b) => {
                    if b[0] == 0 {
                        terminated = true;
                        break;
                    }
                    bytes.push(b[0]);
                }
                Err(_) => return -14,
            }
        }
        if !terminated {
            return -22;
        }
        bytes
    };
    let raw_name = match alloc::string::String::from_utf8(raw_bytes) {
        Ok(s) => s,
        Err(_) => return -22,
    };
    let full_name = alloc::format!("/memfd:{}", raw_name);

    static MEMFD_INO: AtomicUsize = AtomicUsize::new(0x6000_0000);
    let ino = MEMFD_INO.fetch_add(1, Ordering::Relaxed);
    let allow_sealing = flags & MFD_ALLOW_SEALING != 0;
    let inode: Arc<dyn crate::fs::Inode> = Arc::new(
        crate::fs::ramfs::RamInode::new_memfd_sized(ino, allow_sealing, hugetlb_size),
    );
    let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
        inode,
        0o2,
        Some(full_name),
    ));

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                let cloexec = flags & MFD_CLOEXEC != 0;
                return match crate::fs::fd::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        if cloexec {
                            let _ = crate::fs::fd::set_cloexec(process, fd, true);
                        }
                        fd as isize
                    }
                    Err(e) => e,
                };
            }
        }
    }
    -22
}

// ============================================================================
// memfd_secret(2) — per Linux mm/secretmem.c
// ============================================================================

/// memfd_secret(flags) — anonymous secret-memory fd (Linux 5.14+, syscall 447).
///
/// Linux `SYSCALL_DEFINE1(memfd_secret, unsigned int, flags)`:
/// * only `O_CLOEXEC` is accepted; any other bit → EINVAL.
/// * returns an O_RDWR fd backed by secretmem — pages are removed from the
///   kernel direct map; the file supports mmap/ftruncate/fstat but has **no**
///   read_iter/write_iter/splice_read → read(2)/write(2)/splice(2)/
///   sendfile(2)/readahead(2) on it all fail EINVAL.
///
/// We back it with a memfd RamInode and mark the descriptor path as
/// `anon_inode:[secretmem]` — the splice/readahead pseudo-file guards key off
/// that prefix and reject it with EINVAL exactly like Linux's missing
/// splice_read/a_ops (LTP splice07 / readahead01 / accept03 via tst_fd).
pub(in crate::syscall) fn sys_memfd_secret(flags: u32) -> isize {
    const O_CLOEXEC: u32 = 0o2000000;
    if flags & !O_CLOEXEC != 0 {
        return -22;
    }

    static SECRETMEM_INO: AtomicUsize = AtomicUsize::new(0x6800_0000);
    let ino = SECRETMEM_INO.fetch_add(1, Ordering::Relaxed);
    let inode: Arc<dyn crate::fs::Inode> =
        Arc::new(crate::fs::ramfs::RamInode::new_memfd(ino, false));
    let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
        inode,
        0o2,
        Some(alloc::string::String::from("anon_inode:[secretmem]")),
    ));

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                return match crate::fs::fd::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        if flags & O_CLOEXEC != 0 {
                            let _ = crate::fs::fd::set_cloexec(process, fd, true);
                        }
                        fd as isize
                    }
                    Err(e) => e,
                };
            }
        }
    }
    -22
}

// ============================================================================
// userfaultfd(2) — per Linux fs/userfaultfd.c
// ============================================================================

pub(in crate::syscall) fn sys_userfaultfd(flags: u32) -> isize {
    crate::kernel::userfaultfd::create_userfaultfd(flags)
}
