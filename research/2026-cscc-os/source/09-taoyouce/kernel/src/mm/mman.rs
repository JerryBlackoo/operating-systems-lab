use crate::mm::{VirtAddr, PAGE_SIZE};

/// `msync(addr, len, flags)` — POSIX flush a memory mapping.
///
/// 我们的 VFS 是 ram-backed，没有 dirty page-cache 概念，做完边界校验后
/// 直接返回 0。校验：
///   * `addr` 必须 page-aligned，否则 `-EINVAL`
///   * `flags` 必须是 `{MS_ASYNC=1, MS_SYNC=4, MS_INVALIDATE=2}` 的子集
///   * `MS_SYNC` 和 `MS_ASYNC` 互斥
///   * `addr` + `len` 不能溢出
pub(crate) fn sys_msync(addr: usize, len: usize, flags: usize) -> isize {
    const MS_ASYNC: usize = 0x01;
    const MS_INVALIDATE: usize = 0x02;
    const MS_SYNC: usize = 0x04;
    const MS_ALL: usize = MS_ASYNC | MS_INVALIDATE | MS_SYNC;
    if flags & !MS_ALL != 0 {
        return -22;
    }
    if (flags & MS_SYNC != 0) && (flags & MS_ASYNC != 0) {
        return -22;
    }
    if addr & 0xfff != 0 {
        return -22;
    }
    if addr.checked_add(len).is_none() {
        return -22;
    }

    let aligned_len = (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let end = addr.saturating_add(aligned_len);

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let table = match table.as_mut() {
        Some(t) => t,
        None => return -1,
    };
    let pid = match table.current {
        Some(p) => p,
        None => return -1,
    };
    let as_pid = {
        let p = match table.get(pid) {
            Some(p) => p,
            None => return -1,
        };
        if p.mm.is_some() {
            pid
        } else {
            p.tgid
        }
    };
    let mm = match table.get(as_pid).and_then(|p| p.mm.as_ref()) {
        Some(m) => m.clone(),
        None => return -1,
    };
    let mut addr_space = mm.lock();

    // Walk the range like Linux mm/msync.c: a hole (no VMA covering the next
    // byte) is ENOMEM (msync03 addr4); with MS_INVALIDATE, a VM_LOCKED VMA in
    // the range is EBUSY (msync03 addr1 mapped MAP_LOCKED).
    let mut cur = addr;
    while cur < end {
        match addr_space.find_vma(VirtAddr(cur)) {
            Some(v) if v.start.0 <= cur => {
                if flags & MS_INVALIDATE != 0 && v.locked {
                    return -16; // -EBUSY
                }
                cur = v.end.0;
            }
            _ => return -12, // -ENOMEM: unmapped hole
        }
    }

    // MS_SYNC / MS_ASYNC: flush dirty pages of writable MAP_SHARED file
    // mappings back to their backing inode so a subsequent read()/re-mmap sees
    // the modifications (msync01/02). MS_INVALIDATE alone has nothing to write.
    if flags & (MS_SYNC | MS_ASYNC) != 0 {
        addr_space.writeback_range(VirtAddr(addr), VirtAddr(end));
    }
    // MS_INVALIDATE: drop cached pages and reload from the backing file so
    // out-of-band file changes become visible through the mapping (msync02).
    // Linux orders writeback before invalidation when both are requested.
    if flags & MS_INVALIDATE != 0 {
        addr_space.invalidate_range(VirtAddr(addr), VirtAddr(end));
    }
    0
}

const RLIMIT_MEMLOCK: usize = 8;
const CAP_IPC_LOCK: u64 = 1u64 << 14;

/// Common worker for `mlock(2)`/`mlock2(2)`/`munlock(2)`. Linux rounds the
/// start address down and the end up to page boundaries (the address need
/// not be page-aligned). The whole resulting range must correspond to mapped
/// pages (else ENOMEM). For an unprivileged caller (no CAP_IPC_LOCK) the
/// locked size is charged against RLIMIT_MEMLOCK: a zero limit is EPERM and
/// exceeding a non-zero limit is ENOMEM.
fn mlock_range(addr: usize, len: usize, lock: bool, populate: bool) -> isize {
    if len == 0 {
        return 0;
    }
    let start = addr & !(PAGE_SIZE - 1);
    let end = match addr.checked_add(len) {
        Some(e) => match e.checked_add(PAGE_SIZE - 1) {
            Some(r) => r & !(PAGE_SIZE - 1),
            None => return -22, // -EINVAL (overflow)
        },
        None => return -22, // -EINVAL (overflow)
    };
    let nr_bytes = (end - start) as u64;

    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() {
        Some(t) => t,
        None => return -1,
    };
    let pid = match table.current {
        Some(p) => p,
        None => return -1,
    };
    let (euid, cap_eff, memlock_limit, as_pid) = match table.get(pid) {
        Some(p) => (
            p.euid,
            p.cap_effective,
            p.rlimits[RLIMIT_MEMLOCK].0,
            if p.mm.is_some() {
                pid
            } else {
                p.tgid
            },
        ),
        None => return -1,
    };
    let mm = match table.get(as_pid).and_then(|p| p.mm.as_ref()) {
        Some(m) => m.clone(),
        None => return -1,
    };
    let mut addr_space = mm.lock();

    if !addr_space.range_fully_mapped(VirtAddr(start), VirtAddr(end)) {
        return -12; // -ENOMEM: range not fully mapped
    }

    let privileged = euid == 0 || (cap_eff & CAP_IPC_LOCK) != 0;
    if lock && !privileged {
        if memlock_limit == 0 {
            return -1; // -EPERM
        }
        let already = addr_space.locked_vm_bytes();
        if already.saturating_add(nr_bytes) > memlock_limit {
            return -12; // -ENOMEM: would exceed RLIMIT_MEMLOCK
        }
    }
    if addr_space.lock_region(VirtAddr(start), VirtAddr(end), lock, populate) {
        0
    } else {
        -12 // -ENOMEM: range not fully mapped (re-checked under split)
    }
}

/// `mlock(addr, len)` — pin a range in RAM and fault every page in. We never
/// page out, but the VM_LOCKED/VmLck accounting and errno semantics are real.
pub(crate) fn sys_mlock(addr: usize, len: usize) -> isize {
    mlock_range(addr, len, true, true)
}

/// Linux 5.x `mlock2(addr, len, flags)`：扩展 `mlock`，可以指定
/// `MLOCK_ONFAULT = 1` 让映射页只在首次 page fault 时锁住。我们没有
/// 真正的换出，所以行为与 `mlock` 等价；这里负责校验 flag 与边界。
pub(crate) fn sys_mlock2(addr: usize, len: usize, flags: u32) -> isize {
    const MLOCK_ONFAULT: u32 = 1;
    if flags & !MLOCK_ONFAULT != 0 {
        return -22;
    }
    // MLOCK_ONFAULT locks only already-present pages and does not fault the
    // rest in; plain mlock2 populates the whole range (like mlock(2)).
    let populate = flags & MLOCK_ONFAULT == 0;
    mlock_range(addr, len, true, populate)
}

/// `munlock(addr, len)` — unpin a range; same range/ENOMEM checks as mlock.
pub(crate) fn sys_munlock(addr: usize, len: usize) -> isize {
    mlock_range(addr, len, false, false)
}

/// `mlockall(flags)` — pin the whole address space. Validate the flag bitmask,
/// then (for MCL_CURRENT) charge the current VM size against RLIMIT_MEMLOCK for
/// an unprivileged caller, matching mlockall03's ENOMEM/EPERM/EINVAL cases.
pub(crate) fn sys_mlockall(flags: usize) -> isize {
    const MCL_CURRENT: usize = 1;
    const MCL_FUTURE: usize = 2;
    const MCL_ONFAULT: usize = 4;
    const MCL_ALL: usize = MCL_CURRENT | MCL_FUTURE | MCL_ONFAULT;
    if flags == 0 {
        return -22;
    }
    if flags & !MCL_ALL != 0 {
        return -22;
    }
    // MCL_ONFAULT is only meaningful together with MCL_CURRENT or MCL_FUTURE.
    if flags & MCL_ONFAULT != 0 && flags & (MCL_CURRENT | MCL_FUTURE) == 0 {
        return -22;
    }

    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() {
        Some(t) => t,
        None => return -1,
    };
    let pid = match table.current {
        Some(p) => p,
        None => return -1,
    };
    let (euid, cap_eff, memlock_limit, as_pid) = match table.get(pid) {
        Some(p) => (
            p.euid,
            p.cap_effective,
            p.rlimits[RLIMIT_MEMLOCK].0,
            if p.mm.is_some() {
                pid
            } else {
                p.tgid
            },
        ),
        None => return -1,
    };
    let mm = match table.get(as_pid).and_then(|p| p.mm.as_ref()) {
        Some(m) => m.clone(),
        None => return -1,
    };
    let mut addr_space = mm.lock();

    let privileged = euid == 0 || (cap_eff & CAP_IPC_LOCK) != 0;
    if !privileged {
        if memlock_limit == 0 {
            return -1; // -EPERM
        }
        if flags & MCL_CURRENT != 0 && addr_space.total_vm_bytes() > memlock_limit {
            return -12; // -ENOMEM
        }
    }
    if flags & MCL_CURRENT != 0 {
        addr_space.set_all_locked(true);
    }
    0
}

/// `munlockall()` — drop all VA pins.
pub(crate) fn sys_munlockall() -> isize {
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() {
        Some(t) => t,
        None => return -1,
    };
    let pid = match table.current {
        Some(p) => p,
        None => return -1,
    };
    let as_pid = match table.get(pid) {
        Some(p) => {
            if p.mm.is_some() {
                pid
            } else {
                p.tgid
            }
        }
        None => return -1,
    };
    if let Some(mm) = table.get(as_pid).and_then(|p| p.mm.as_ref()).cloned() {
        let mut space = mm.lock();
        space.set_all_locked(false);
    }
    0
}

/// madvise(addr, len, advice) — Slice 2.x T3-10 真化（去清零简化）。
///
/// Linux 真实现：
/// * **MADV_DONTNEED (4)**：unmap PTE + drop 物理页 *引用*，**保留 VMA**。
///   下次访问范围内地址会触发 page fault → 重新分配清零页（anon）或
///   重读 backing file（file-mapped）。
/// * **MADV_FREE (8)**：lazy free，标 PTE dirty=0；我们没 reclaim 子系统，
///   直接走 DONTNEED 路径（POSIX 允许更早释放，与 Linux 在内存压力下行为
///   等价）。
/// * **MADV_REMOVE (9)**：file-backed 时 punch hole；anon 等同 DONTNEED。
///   ramfs 没 hole 概念 → 按 DONTNEED 处理。
/// * **MADV_NORMAL/RANDOM/SEQUENTIAL/WILLNEED/HUGEPAGE/NOHUGEPAGE/MERGEABLE/
///   UNMERGEABLE/DONTDUMP/DODUMP/WIPEONFORK/KEEPONFORK/COLD/PAGEOUT** ——
///   readahead/policy hint 类，Linux 不强制行为；我们维持成功返回 0。
/// * 未知 advice → `-EINVAL(22)` 对齐 Linux `madvise(2)`。
///
/// 参数校验：
/// * `addr` 必须 page-aligned（`-EINVAL`）。
/// * `addr + len` 溢出 / 范围与现有 VMA 完全无交集 → 仍返 0（DONTNEED 容许
///   无效页静默跳过），但 `len==0` 直接返 0（Linux 行为）。
pub(crate) fn sys_madvise(addr: usize, len: usize, advice: usize) -> isize {
    const MADV_NORMAL: usize = 0;
    const MADV_RANDOM: usize = 1;
    const MADV_SEQUENTIAL: usize = 2;
    const MADV_WILLNEED: usize = 3;
    const MADV_DONTNEED: usize = 4;
    const MADV_FREE: usize = 8;
    const MADV_REMOVE: usize = 9;
    const MADV_DONTFORK: usize = 10;
    const MADV_DOFORK: usize = 11;
    const MADV_MERGEABLE: usize = 12;
    const MADV_UNMERGEABLE: usize = 13;
    const MADV_HUGEPAGE: usize = 14;
    const MADV_NOHUGEPAGE: usize = 15;
    const MADV_DONTDUMP: usize = 16;
    const MADV_DODUMP: usize = 17;
    const MADV_WIPEONFORK: usize = 18;
    const MADV_KEEPONFORK: usize = 19;
    const MADV_COLD: usize = 20;
    const MADV_PAGEOUT: usize = 21;
    const MADV_POPULATE_READ: usize = 22;
    const MADV_POPULATE_WRITE: usize = 23;
    const MADV_HWPOISON: usize = 100;

    use crate::mm::VmaType;

    if addr & 0xFFF != 0 {
        return -22;
    } // -EINVAL: start not page-aligned
    if len == 0 {
        return 0;
    }
    // Linux `do_madvise`: `end = start + PAGE_ALIGN(len)` — the length is
    // rounded up to a whole page so advice always applies to entire pages
    // (and per-VMA flags like VM_DONTDUMP/VM_WIPEONFORK never split a VMA
    // mid-page, which would otherwise leave a sub-page tail still carrying the
    // page's contents into a core dump — LTP madvise08).
    let aligned_len = match len.checked_add(PAGE_SIZE - 1) {
        Some(v) => v & !(PAGE_SIZE - 1),
        None => return -22,
    };
    let end = match addr.checked_add(aligned_len) {
        Some(e) => e,
        None => return -22,
    };

    // Any advice value we do not recognise is EINVAL (matches madvise02; not
    // a silent accept). MADV_MERGEABLE / MADV_UNMERGEABLE (CONFIG_KSM) follow
    // Linux ksm_madvise: mark/unmark the VMA, no-op success on incompatible
    // VMAs — never EINVAL (we expose /sys/kernel/mm/ksm so madvise02 skips
    // its CONFIG_KSM=n EINVAL probes, same as a KSM-enabled Linux).
    let recognized = matches!(
        advice,
        MADV_NORMAL
            | MADV_RANDOM
            | MADV_SEQUENTIAL
            | MADV_WILLNEED
            | MADV_DONTNEED
            | MADV_FREE
            | MADV_REMOVE
            | MADV_DONTFORK
            | MADV_DOFORK
            | MADV_MERGEABLE
            | MADV_UNMERGEABLE
            | MADV_HUGEPAGE
            | MADV_NOHUGEPAGE
            | MADV_DONTDUMP
            | MADV_DODUMP
            | MADV_WIPEONFORK
            | MADV_KEEPONFORK
            | MADV_COLD
            | MADV_PAGEOUT
            | MADV_POPULATE_READ
            | MADV_POPULATE_WRITE
            | MADV_HWPOISON
    );
    // MADV_HWPOISON (CONFIG_MEMORY_FAILURE)：错误注入。Linux madvise_inject_error
    // 先要求 capable(CAP_SYS_ADMIN)，随后对范围内每页 memory_failure() —— 页被
    // unmap 并从 page cache / anon 摘除。我们等价为「特权校验 + 丢弃该范围物理
    // 页」（后续访问重新缺页，而非 SIGBUS——足够覆盖注入接口语义本身）。
    if advice == MADV_HWPOISON && crate::kernel::security::current_euid() != 0 {
        return -1; // -EPERM
    }
    if !recognized {
        return -22;
    }

    // 只在短临界区里解析 mm；PROCESS_TABLE 锁绝不能带进下方主体——
    // DONTNEED 出口要做同步 drain_shootdown，若彼时仍持表锁，其它 hart 的
    // 缺页处理（都要表锁）无法返回用户态 ack，8 核互等成死锁（GDB 实证：
    // CPU4 drain 自旋 + CPU0-3 全堵在 handle_page_fault_trap 等表锁）。
    let mm = {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_mut() {
            Some(t) => t,
            None => return -22,
        };
        let pid = match t.current {
            Some(p) => p,
            None => return -22,
        };
        let as_pid = match t.get(pid) {
            Some(p) => {
                if p.mm.is_some() {
                    pid
                } else {
                    p.tgid
                }
            }
            None => return -22,
        };
        match t.get(as_pid).and_then(|p| p.mm.as_ref()) {
            Some(m) => m.clone(),
            None => return -22,
        }
    };
    let mut addr_space = mm.lock();

    // Per-VMA advice restrictions are evaluated against the VMA at `start`
    // BEFORE the whole-range ENOMEM check, matching Linux's madvise walk
    // (which reports a per-VMA EINVAL before noticing an unmapped tail). E.g.
    // madvise02 calls MADV_WIPEONFORK over a 1-page shared-anon mapping with a
    // multi-page length: the shared VMA must yield EINVAL, not ENOMEM.
    if let Some(v) = addr_space.find_vma(VirtAddr(addr)) {
        let (vtype, locked, file_backed) = (v.vma_type, v.locked, v.file_backed);
        // "private anonymous" = neither a shared mapping nor file-backed.
        let private_anon = vtype != VmaType::Shared && !file_backed;
        match advice {
            // MADV_FREE / MADV_WIPEONFORK only apply to private anon pages.
            MADV_FREE | MADV_WIPEONFORK if !private_anon => return -22,
            // MADV_DONTNEED cannot release locked (VM_LOCKED) pages.
            MADV_DONTNEED if locked => return -22,
            // MADV_REMOVE hole-punch requires a shared mapping whose backing
            // store supports fallocate(PUNCH_HOLE)（shmem / 常规文件均可）；
            // 私有映射为 EINVAL（Linux madvise_remove 对 VM_PRIVATE 拒绝）。
            // Linux madvise_remove additionally rejects VM_LOCKED (can't
            // punch storage out from under pinned pages) — madvise02 mlocks
            // file1 and expects EINVAL.
            MADV_REMOVE if vtype != VmaType::Shared || locked => return -22,
            _ => {}
        }
    }

    // The whole range must be backed by VMAs, otherwise ENOMEM (madvise02's
    // file2 has its last page unmapped).
    if !addr_space.range_fully_mapped(VirtAddr(addr), VirtAddr(end)) {
        return -12; // -ENOMEM
    }

    match advice {
        MADV_HWPOISON => {
            let _ = addr_space.madvise_dontneed(VirtAddr(addr), VirtAddr(end));
            drop(addr_space);
            crate::mm::address_space::AddressSpace::drain_and_reap_current_mm();
            0
        }
        MADV_DONTNEED | MADV_REMOVE => {
            if advice == MADV_REMOVE {
                addr_space.punch_hole_file_range(VirtAddr(addr), VirtAddr(end));
            }
            let _ = addr_space.madvise_dontneed(VirtAddr(addr), VirtAddr(end));
            // MADV_DONTNEED 的 Linux 语义：调用返回后，本 mm 任何线程的后续
            // 读必须看到零页/重新缺页。帧仅进 quarantine（防跨进程复用踩踏）
            // 不够——同 mm 的兄弟线程在其它 hart 上仍握着陈旧 TLB，会继续读
            // 到旧内容，破坏 jemalloc/allocator 的"purge 后必为零"假设
            // （BuildStorm rustc codegen 线程用户态死循环 20min+ 的根因）。
            // 出 mm 锁后同步等待所有对端完成失效并回收 quarantine 再返回用户态。
            drop(addr_space);
            crate::mm::address_space::AddressSpace::drain_and_reap_current_mm();
            0
        }
        // MADV_FREE: lazy free（对标 Linux）。标记页可回收但保留内容，仅在内存
        // 压力（cgroup reclaim）下才丢弃干净页；期间被写入则取消标记。
        MADV_FREE => {
            let _ = addr_space.madvise_free(VirtAddr(addr), VirtAddr(end));
            0
        }
        // MADV_WIPEONFORK: child must see zero-filled memory after the next
        // fork; MADV_KEEPONFORK undoes it. Recorded per-VMA, applied in
        // clone_cow (madvise10).
        MADV_WIPEONFORK => {
            addr_space.set_wipeonfork_range(VirtAddr(addr), VirtAddr(end), true);
            0
        }
        MADV_KEEPONFORK => {
            addr_space.set_wipeonfork_range(VirtAddr(addr), VirtAddr(end), false);
            0
        }
        // MADV_DONTDUMP: exclude the range from core dumps; MADV_DODUMP undoes
        // it. Recorded per-VMA, consulted by the core dumper (madvise08).
        MADV_DONTDUMP => {
            addr_space.set_dontdump_range(VirtAddr(addr), VirtAddr(end), true);
            0
        }
        MADV_DODUMP => {
            addr_space.set_dontdump_range(VirtAddr(addr), VirtAddr(end), false);
            0
        }
        // MADV_PAGEOUT: reclaim (swap out) the pages in the range now. 对标
        // Linux `madvise_pageout` → 把匿名页写入 swap、PTE 改 swap entry、释放
        // 物理帧。只换可安全换出的私有匿名页（见 swap_out_page）。
        MADV_PAGEOUT => {
            let pages = (end - addr) / PAGE_SIZE;
            let _ = addr_space.swap_out_range(VirtAddr(addr), VirtAddr(end), pages);
            0
        }
        // MADV_WILLNEED: readahead。对 swap 页做 swap readahead——把已换出的页
        // 提前换回 RAM（madvise06 正是验证该路径：WILLNEED 后访问无 major fault）。
        MADV_WILLNEED => {
            let _ = addr_space.swap_in_range(VirtAddr(addr), VirtAddr(end));
            0
        }
        // Remaining advices (NORMAL/RANDOM/SEQUENTIAL/COLD/HUGEPAGE/...) are
        // hints over an already-validated mapped range; honoured as no-ops.
        _ => 0,
    }
}

/// mincore(start, len, vec) — Slice 2.x 真实现。
///
/// 按 LTP `mincore01..04` 测，逐页生成 1 字节 (bit0=1 表示 in-core)，
/// 我们没有 swap，所以全部置 1。同时验证：
///   - start 必须页对齐 → -EINVAL
///   - vec 是用户指针 → -EFAULT 时返
///   - len > 0
pub(crate) fn sys_mincore(start: usize, len: usize, vec: *mut u8) -> isize {
    if start & (PAGE_SIZE - 1) != 0 {
        return -22;
    } // -EINVAL: unaligned addr
    if len == 0 {
        return 0;
    }
    if vec.is_null() {
        return -14; // -EFAULT
    }
    let pages = (len + PAGE_SIZE - 1) / PAGE_SIZE;
    let end = match start.checked_add(pages * PAGE_SIZE) {
        Some(e) => e,
        None => return -12, // -ENOMEM: range wraps the address space
    };

    // Resolve the calling process's address space, build the residency vector
    // (bit0 = page resident), then drop the lock before copying to user space.
    let buf = {
        let mm = match crate::syscall::user::current_mm() {
            Ok(m) => m,
            Err(_) => return -14,
        };
        let mut addr_space = mm.lock();
        // mincore(2): ENOMEM if the range is not all mapped.
        if !addr_space.range_fully_mapped(VirtAddr(start), VirtAddr(end)) {
            return -12;
        }
        let mut buf = alloc::vec![0u8; pages];
        for (i, slot) in buf.iter_mut().enumerate() {
            let va = VirtAddr(start + i * PAGE_SIZE);
            if addr_space.page_resident(va) {
                *slot = 1;
            }
        }
        buf
    };
    if crate::syscall::user::write_user_buffer(vec, &buf).is_err() {
        return -14; // -EFAULT
    }
    0
}

// ---- mremap / remap_file_pages — Slice 2.x 真扩展 -------------------------
//
// mremap(old_addr, old_size, new_size, flags, new_addr)：
//   - new_size <= old_size: 等同截断（unmap [old + new_size, old + old_size)）→ 返 old。
//   - new_size > old_size 且 MREMAP_MAYMOVE：在 process address_space 找一段
//     new_size 的空闲区域，分配物理页+映射 → 返 new_va；旧映射保留为 hole 由
//     调用者后续 munmap 释放。
//   - 否则：-ENOMEM。
pub(crate) fn sys_mremap(
    old: usize,
    old_size: usize,
    new_size: usize,
    flags: u32,
    new_addr: usize,
) -> isize {
    const MREMAP_MAYMOVE: u32 = 1;
    const MREMAP_FIXED: u32 = 2;
    const MREMAP_DONTUNMAP: u32 = 4;
    const KNOWN: u32 = MREMAP_MAYMOVE | MREMAP_FIXED | MREMAP_DONTUNMAP;
    if flags & !KNOWN != 0 {
        return -22;
    }
    // FIXED implies MAYMOVE.
    if flags & MREMAP_FIXED != 0 && flags & MREMAP_MAYMOVE == 0 {
        return -22;
    }
    // DONTUNMAP requires MAYMOVE and only makes sense for size-preserving move.
    if flags & MREMAP_DONTUNMAP != 0 {
        if flags & MREMAP_MAYMOVE == 0 {
            return -22;
        }
        if new_size != old_size {
            return -22;
        }
    }
    if old == 0 || old & 0xFFF != 0 {
        return -22;
    }
    if new_size == 0 {
        return -22;
    }
    if flags & MREMAP_FIXED != 0 && (new_addr == 0 || new_addr & 0xFFF != 0) {
        return -22;
    }

    let page_align = |x: usize| (x + 0xFFF) & !0xFFF;
    let aligned_old_size = page_align(old_size);
    let aligned_new_size = page_align(new_size);

    // The old range must be currently mapped, otherwise EFAULT (mremap03 uses
    // tst_get_bad_addr() as the source). MREMAP_FIXED with an old/new range
    // that overlaps is EINVAL (mremap05 case 3).
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                let as_pid = t
                    .get(pid)
                    .map(|p| {
                        if p.mm.is_some() {
                            pid
                        } else {
                            p.tgid
                        }
                    })
                    .unwrap_or(pid);
                if let Some(mm) = t.get(as_pid).and_then(|p| p.mm.as_ref()).cloned() {
                    let addr_space = mm.lock();
                    if !addr_space
                        .range_fully_mapped(VirtAddr(old), VirtAddr(old + aligned_old_size))
                    {
                        return -14; // -EFAULT
                    }
                }
            }
        }
    }
    if flags & MREMAP_FIXED != 0 {
        let old_end = old + aligned_old_size;
        let new_end = new_addr + aligned_new_size;
        if new_addr < old_end && old < new_end {
            return -22; // -EINVAL: old/new ranges must not overlap
        }
    }

    // ---- in-place shrink: release tail pages now ----
    if aligned_new_size < aligned_old_size && flags & MREMAP_FIXED == 0 {
        let tail_start = VirtAddr(old + aligned_new_size);
        let tail_end = VirtAddr(old + aligned_old_size);
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                let as_pid = t
                    .get(pid)
                    .map(|p| {
                        if p.mm.is_some() {
                            pid
                        } else {
                            p.tgid
                        }
                    })
                    .unwrap_or(pid);
                if let Some(mm) = t.get(as_pid).and_then(|p| p.mm.as_ref()).cloned() {
                    let mut addr_space = mm.lock();
                    {
                        let stats = addr_space.unmap_region(tail_start, tail_end);
                        if stats.hugetlb_bytes_released != 0 && stats.huge_page_size != 0 {
                            let pages = stats.hugetlb_bytes_released / stats.huge_page_size;
                            crate::mm::hugetlb_release(pages);
                        }
                    }
                }
            }
        }
        drop(table);
        // mremap 收缩与 munmap 同语义：返回前同步失效全 mm 的陈旧 TLB，
        // 否则兄弟线程仍可经旧条目触到已 retire 的尾页（allocator 元数据
        // 损坏 → cargo "no entry found for key" 级随机 panic）。
        crate::mm::tlb::drain_shootdown();
        return old as isize;
    }

    // ---- in-place same size, non-FIXED: noop ----
    if aligned_new_size == aligned_old_size && flags & MREMAP_FIXED == 0 {
        return old as isize;
    }

    // ---- grow: try in-place first when MAYMOVE not requested ----
    if aligned_new_size > aligned_old_size && flags & (MREMAP_MAYMOVE | MREMAP_FIXED) == 0 {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let can_expand = if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                let as_pid = t
                    .get(pid)
                    .map(|p| {
                        if p.mm.is_some() {
                            pid
                        } else {
                            p.tgid
                        }
                    })
                    .unwrap_or(pid);
                t.get(as_pid)
                    .and_then(|p| p.mm.as_ref())
                    .map(|mm| {
                        let addr_space = mm.lock();
                        let expand_start = old + aligned_old_size;
                        let expand_end = old + aligned_new_size;
                        let vma_overlap = addr_space
                            .vmas_iter()
                            .any(|v| expand_start < v.end.0 && expand_end > v.start.0);
                        // Also reject when a live PTE (e.g. a neighbouring shm
                        // page) already occupies the target range, so growing a
                        // shm attachment in place without MREMAP_MAYMOVE fails
                        // with ENOMEM as Linux does.
                        let pte_overlap = addr_space.range_has_mapped_page(
                            VirtAddr(expand_start),
                            VirtAddr(expand_end),
                        );
                        !vma_overlap && !pte_overlap
                    })
                    .unwrap_or(false)
            } else {
                false
            }
        } else {
            false
        };
        drop(table);
        if can_expand {
            const PROT_READ: usize = 1;
            const PROT_WRITE: usize = 2;
            const MAP_PRIVATE: usize = 0x02;
            const MAP_ANONYMOUS: usize = 0x20;
            const MAP_FIXED: usize = 0x10;
            let tail_va = old + aligned_old_size;
            let tail_len = aligned_new_size - aligned_old_size;
            let rc = crate::syscall::memory::sys_mmap(
                tail_va,
                tail_len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED,
                -1isize as usize,
                0,
            );
            if rc == tail_va as isize {
                return old as isize;
            }
            // mmap rejected → fall through to MAYMOVE failure path below
        }
        return -12; // -ENOMEM
    }

    // ---- move (MAYMOVE/FIXED) ----
    if flags & MREMAP_MAYMOVE == 0 {
        return -12;
    }

    const PROT_READ: usize = 1;
    const PROT_WRITE: usize = 2;
    const MAP_PRIVATE: usize = 0x02;
    const MAP_ANONYMOUS: usize = 0x20;
    const MAP_FIXED: usize = 0x10;

    let (target_addr, map_flags) = if flags & MREMAP_FIXED != 0 {
        (new_addr, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED)
    } else {
        (0, MAP_PRIVATE | MAP_ANONYMOUS)
    };
    let new_va = crate::syscall::memory::sys_mmap(
        target_addr,
        new_size,
        PROT_READ | PROT_WRITE,
        map_flags,
        -1isize as usize,
        0,
    );
    if new_va < 0 {
        return new_va;
    }

    // Copy old content into the new mapping. read_user_buffer caps a single
    // request at 1 MiB, so a large region (e.g. LTP thp02 remaps ~8 MiB) must
    // be copied in chunks; a single oversized read silently returned Err and
    // left the destination zero-filled, which the test detected as "mremap bug".
    let copy_len = old_size.min(new_size);
    const CHUNK: usize = 1024 * 1024;
    let mut off = 0usize;
    while off < copy_len {
        let n = CHUNK.min(copy_len - off);
        match crate::syscall::user::read_user_buffer((old + off) as *const u8, n) {
            Ok(buf) => {
                let _ = crate::syscall::user::write_user_buffer(
                    (new_va as usize + off) as *mut u8,
                    &buf,
                );
            }
            Err(_) => break,
        }
        off += n;
    }

    // Unless DONTUNMAP, release the old range.
    if flags & MREMAP_DONTUNMAP == 0 {
        let _ = crate::syscall::memory::sys_munmap(old, aligned_old_size);
    }
    new_va
}

/// `remap_file_pages(addr, size, prot, pgoff, flags)` — the legacy non-linear
/// file-mapping syscall. Linux deprecated the true non-linear behavior in 3.16
/// and now emulates it: the call must target an existing `MAP_SHARED` file VMA,
/// `prot` must be 0, and it re-points `[addr, addr+size)` so the first page
/// maps file page `pgoff`. We honor exactly that (LTP `remap_file_pages01`).
pub(crate) fn sys_remap_file_pages(
    addr: usize,
    size: usize,
    prot: usize,
    pgoff: usize,
    _flags: usize,
) -> isize {
    use crate::mm::{VirtAddr, PAGE_SIZE};

    // prot must be 0 (the API never carried a real protection change; Linux
    // returns EINVAL otherwise). addr must be page-aligned; size must be
    // non-zero. pgoff is in page units.
    if prot != 0 {
        return -22; // -EINVAL
    }
    if addr & (PAGE_SIZE - 1) != 0 {
        return -22;
    }
    if size == 0 {
        return -22;
    }
    let aligned_size = match size.checked_add(PAGE_SIZE - 1) {
        Some(s) => s & !(PAGE_SIZE - 1),
        None => return -22,
    };
    let end = match addr.checked_add(aligned_size) {
        Some(e) => e,
        None => return -22,
    };
    let new_file_offset = match pgoff.checked_mul(PAGE_SIZE) {
        Some(o) => o,
        None => return -22,
    };

    let mm = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() {
            Some(t) => t,
            None => return -22,
        };
        let pid = match t.current {
            Some(p) => p,
            None => return -22,
        };
        let as_pid = match t.get(pid) {
            Some(p) => {
                if p.mm.is_some() {
                    pid
                } else {
                    p.tgid
                }
            }
            None => return -22,
        };
        match t.get(as_pid).and_then(|p| p.mm.as_ref()) {
            Some(m) => m.clone(),
            None => return -22,
        }
    };
    let mut addr_space = mm.lock();
    addr_space.remap_file_pages_range(VirtAddr(addr), VirtAddr(end), new_file_offset)
}
