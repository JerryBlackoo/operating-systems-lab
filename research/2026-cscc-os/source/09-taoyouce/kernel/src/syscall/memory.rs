use alloc::vec::Vec;
use crate::fs::fd as fd_table;

/// mprotect — 修改现有 VMA 的访问权限
pub(crate) fn sys_mprotect(addr: usize, len: usize, prot: usize) -> isize {
    let mut reduced = false;
    let rc = do_mprotect(addr, len, prot, None, &mut reduced);
    // mprotect(2) 返回后新权限须对本 mm 全部线程立即生效：降权后兄弟线程
    // 不得凭陈旧可写 TLB 继续写（CoW/写屏障协议）。do_mprotect 内部仅本地
    // 失效 + 异步 shootdown，这里在锁全部释放后同步等待对端完成。
    // 仅权限**收紧**时才需同步：纯放宽/无变化下陈旧 TLB 只多一次缺页，
    // 不破坏正确性（Linux change_protection 同义）。
    if rc == 0 && reduced {
        crate::mm::tlb::drain_shootdown();
    }
    rc
}

/// mprotect / pkey_mprotect 共用体（Linux do_mprotect_pkey）。
/// `pkey = Some(k)`：区间改挂到 key k（须已在本 mm 分配，否则 EINVAL）；
/// `None`：保留各 VMA 现有 key。
pub(crate) fn do_mprotect(
    addr: usize,
    len: usize,
    prot: usize,
    pkey: Option<i32>,
    reduced: &mut bool,
) -> isize {
    use crate::mm::{VirtAddr, VmaPermission, PAGE_SIZE};

    if len == 0 || addr % PAGE_SIZE != 0 {
        return -22; // -EINVAL
    }

    if len > usize::MAX - PAGE_SIZE {
        return -12; // -ENOMEM
    }

    const PROT_READ: usize = 0x1;
    const PROT_WRITE: usize = 0x2;
    const PROT_EXEC: usize = 0x4;

    let aligned_len = (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let start = VirtAddr(addr);
    let end = VirtAddr(addr + aligned_len);
    let permission = VmaPermission::new(
        prot & PROT_READ != 0,
        prot & PROT_WRITE != 0,
        prot & PROT_EXEC != 0,
        true,
    );

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let mm = {
        let table = match table.as_mut() {
            Some(t) => t,
            None => return -1,
        };
        let pid = match table.current {
            Some(pid) => pid,
            None => return -1,
        };
        let as_pid = {
            let p = match table.get(pid) { Some(p) => p, None => return -1 };
            if p.mm.is_some() { pid } else { p.tgid }
        };
        let process = match table.get(as_pid) {
            Some(process) => process,
            None => return -1,
        };
        match process.mm.as_ref() {
            Some(mm) => mm.clone(),
            None => return -1,
        }
    }; // PT 锁在此释放：下面全部 mm 工作只在 mm 锁内做
    drop(table);
    let mut addr_space = mm.lock();

    // pkey_mprotect 的 key 必须已在本 mm 分配（key 0 恒存在），否则 EINVAL。
    if let Some(k) = pkey {
        if !(0..16).contains(&k) || addr_space.pkey_alloc_mask & (1u32 << k) == 0 {
            return -22; // -EINVAL
        }
    }

    // mprotect adding PROT_WRITE to a mapping that may not be written is EACCES.
    // This is the case for a MAP_SHARED file mapping whose backing fd was opened
    // read-only (VM_MAYWRITE cleared at mmap time) — mprotect01 case 3.
    if prot & PROT_WRITE != 0 {
        let mut cur = start;
        while cur < end {
            match addr_space.find_vma(cur) {
                Some(v) => {
                    if !v.may_write {
                        return -13; // -EACCES
                    }
                    cur = v.end;
                }
                None => break, // gap -> protect_region reports ENOMEM below
            }
        }
    }

    match addr_space.protect_region_pkey(start, end, permission, pkey) {
        Some(r) => {
            *reduced = r;
            0
        }
        None => -12, // -ENOMEM: 区间未映射
    }
}

/// 在 mmap 区域 [0x2000_0000, 0x4000_0000) 内查找连续 aligned_len 的空闲段。
///
/// 之前的实现只检查 candidate 起点和终点-1 是否落入某 VMA，会漏检"完全包含
/// 在 candidate 范围中段"的 VMA（musl mallocng 大量小 mmap 后会让候选地址
/// 中段被占用），导致返回的地址实际已被占用，用户访问时无 VMA 命中 →
/// PageInvalidLoad。这里改为扫所有 VMA 求并集，找完整的空隙。
fn auto_alloc_mmap_addr(
    addr_space: &mut crate::mm::AddressSpace,
    aligned_len: usize,
) -> Option<usize> {
    auto_alloc_mmap_addr_aligned(addr_space, aligned_len, crate::mm::PAGE_SIZE)
}

/// RISC-V64 内核占用 root[2..2+n] 的 1GB gigapage，恒等映射
/// [0x8000_0000, kernel_ident_end)（n 随 -m 实测 RAM 变化）。
/// 用户态映射若落在此区间，页表遍历会把内核 gigapage 叶子 PTE 当作中间级
/// 目录去读内核内存，造成 translate/unmap 死循环（hugemmap02 RV 挂死根因）。
/// LoongArch64 内核走 DMW 窗口，不在用户页表里，0xa0000000 等地址是合法用户 VA，
/// 故此函数仅在 riscv64 生效。
#[inline]
fn overlaps_kernel_reserved(start: usize, end: usize) -> bool {
    #[cfg(target_arch = "riscv64")]
    {
        const K_START: usize = 0x8000_0000;
        let k_end = crate::mm::kernel_ident_end();
        start < k_end && end > K_START
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        let _ = (start, end);
        false
    }
}

fn auto_alloc_mmap_addr_aligned(
    addr_space: &crate::mm::AddressSpace,
    aligned_len: usize,
    alignment: usize,
) -> Option<usize> {
    use crate::mm::PAGE_SIZE;
    // 窗口常量集中在 AddressSpace::mmap_window()，两处必须一致（bump 不变式）。
    let (mmap_base, mmap_end) = crate::mm::AddressSpace::mmap_window();
    if aligned_len == 0 || aligned_len > mmap_end - mmap_base {
        return None;
    }
    let align = alignment.max(PAGE_SIZE);
    // O(1) bump 快路径：`add_vma` 维护「mmap_bump 之上的窗口区间内一定没有任何
    // VMA」的不变式，故从 bump 取址恒不与现有映射冲突，无需对成千上万个 VMA 反复
    // 全扫+排序（max_map_count 一次 flood 6.5w 映射，旧全扫 O(N²) 必超时）。
    {
        let mut cand = (addr_space.mmap_bump().max(mmap_base) + align - 1) & !(align - 1);
        // RISC-V 内核保留 gigapage 不是 VMA、不抬 bump：候选段若跨入则越过它。
        if overlaps_kernel_reserved(cand, cand.saturating_add(aligned_len)) {
            #[cfg(target_arch = "riscv64")]
            {
                let k_end = crate::mm::kernel_ident_end();
                cand = (k_end + align - 1) & !(align - 1);
            }
        }
        if let Some(end) = cand.checked_add(aligned_len) {
            if cand >= mmap_base && end <= mmap_end && !overlaps_kernel_reserved(cand, end) {
                return Some(cand);
            }
        }
        // bump 触及窗口上界 → 回退到下面的全扫，复用 munmap 留下的低地址空洞。
    }
    // 收集 mmap 区间内的占用段并按 start 排序
    let mut occupied: alloc::vec::Vec<(usize, usize)> = addr_space
        .vmas_iter()
        .filter_map(|vma| {
            let s = vma.start.0;
            let e = vma.end.0;
            // 与 mmap 区间相交才记录
            if e > mmap_base && s < mmap_end {
                Some((s.max(mmap_base), e.min(mmap_end)))
            } else {
                None
            }
        })
        .collect();
    // RISC-V64: 内核 gigapage [0x8000_0000, kernel_ident_end) 不是用户 VMA，但页表
    // 遍历会把它的叶子 PTE 误当中间目录（hugemmap02 挂死根因）。作为合成占用段注入，
    // 使 gap 搜索跨过它，把大映射安全放到 kernel_ident_end 之上。
    #[cfg(target_arch = "riscv64")]
    {
        const K_START: usize = 0x8000_0000;
        let k_end = crate::mm::kernel_ident_end();
        if k_end > mmap_base && K_START < mmap_end {
            occupied.push((K_START.max(mmap_base), k_end.min(mmap_end)));
        }
    }
    occupied.sort_by_key(|&(s, _)| s);

    let mut candidate = (mmap_base + align - 1) & !(align - 1);
    for (s, e) in &occupied {
        if candidate + aligned_len <= *s {
            // 此 VMA 之前的空隙够大
            return Some(candidate);
        }
        // 跳到此 VMA 的 end，page 对齐
        let new_candidate = (*e + align - 1) & !(align - 1);
        if new_candidate > candidate {
            candidate = new_candidate;
        }
        if candidate + aligned_len > mmap_end {
            return None;
        }
    }
    if candidate + aligned_len <= mmap_end {
        Some(candidate)
    } else {
        None
    }
}

/// 内存映射
pub(crate) fn sys_mmap(
    addr: usize,
    len: usize,
    prot: usize,
    flags: usize,
    fd: usize,
    offset: usize,
) -> isize {
    use crate::mm::{VirtAddr, VmaType, VmaPermission, Vma, PAGE_SIZE};

    // --- file-descriptor resolution and flag-type validation (Linux order) ---
    // For a file mapping, Linux resolves the fd (fget) before checking len or
    // flags, so an invalid fd is EBADF even when len==0 (mmap08 passes
    // page_sz==0 with a closed fd). Capture the descriptor's read/write mode for
    // the EACCES checks taken once the file path is chosen.
    let (file_perm, file_path): (Option<(bool, bool)>, Option<alloc::sync::Arc<str>>) =
        if flags & 0x20 == 0 {
            let fdesc = {
                let tg = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(ref t) = *tg {
                    if let Some(pid) = t.current {
                        t.get_fd_process(pid)
                            .and_then(|process| fd_table::get(process, fd))
                    } else { None }
                } else { None }
            };
            match fdesc {
                Some(fobj) => {
                    // F_SEAL_WRITE (0x08): MAP_SHARED + PROT_WRITE → EPERM
                    if let Some(shared_seals) = fobj.file.shared_seals() {
                        let cur = shared_seals.load(core::sync::atomic::Ordering::Acquire);
                        const F_SEAL_WRITE: u32 = 0x0008;
                        const F_SEAL_FUTURE_WRITE: u32 = 0x0010;
                        if (cur & F_SEAL_WRITE != 0 || cur & F_SEAL_FUTURE_WRITE != 0)
                            && flags & 0x01 != 0
                            && prot & 0x2 != 0
                        {
                            return -1; // -EPERM
                        }
                    }
                    (
                        Some((fobj.readable(), fobj.writable())),
                        fobj.path.as_deref().map(alloc::sync::Arc::<str>::from),
                    )
                }
                None => return -9, // -EBADF
            }
        } else {
            (None, None)
        };

    if len == 0 {
        return -22; // -EINVAL
    }

    // Exactly one mapping type must be present. MAP_SHARED (0x1), MAP_PRIVATE
    // (0x2) and MAP_SHARED_VALIDATE (0x3) are the only valid values of the low
    // nibble; anything else (e.g. plain MAP_FILE==0) is EINVAL (mmap06 case 8).
    {
        let map_type = flags & 0x0f;
        if map_type != 0x01 && map_type != 0x02 && map_type != 0x03 {
            return -22; // -EINVAL
        }
        // MAP_SHARED_VALIDATE rejects any flag bit the kernel does not
        // understand with EOPNOTSUPP (mmap20), unlike plain MAP_SHARED which
        // silently ignores unknown legacy bits.
        if map_type == 0x03 {
            const MAP_KNOWN: usize = 0x01 | 0x02 | 0x10 | 0x20 | 0x0100 | 0x0800
                | 0x1000 | 0x2000 | 0x4000 | 0x8000 | 0x10000 | 0x20000 | 0x40000
                | 0x80000 | 0x100000 | 0x00FF_0000;
            if flags & !MAP_KNOWN != 0 {
                return -95; // -EOPNOTSUPP
            }
        }
    }

    // File mappings require read access to the underlying file; a write-only
    // descriptor is EACCES regardless of MAP_SHARED/MAP_PRIVATE (mmap06 cases
    // 1-6). A writable MAP_SHARED mapping additionally needs write access.
    if let Some((readable, writable)) = file_perm {
        if !readable {
            return -13; // -EACCES
        }
        if flags & 0x01 != 0 && prot & 0x2 != 0 && !writable {
            return -13; // -EACCES
        }
    }

    // 防止 len + PAGE_SIZE - 1 溢出
    if len > usize::MAX - PAGE_SIZE {
        return -12; // -ENOMEM
    }

    // Linux mmap flags (asm-generic/mman.h + arch-specific)
    const MAP_SHARED:           usize = 0x01;
    const MAP_PRIVATE:          usize = 0x02;
    const MAP_FIXED:            usize = 0x10;
    const MAP_ANONYMOUS:        usize = 0x20;
    const MAP_GROWSDOWN:        usize = 0x0100;
    const MAP_DENYWRITE:        usize = 0x0800;
    const MAP_EXECUTABLE:       usize = 0x1000;
    const MAP_LOCKED:           usize = 0x2000;
    const MAP_NORESERVE:        usize = 0x4000;
    const MAP_POPULATE:         usize = 0x8000;
    const MAP_NONBLOCK:         usize = 0x10000;
    const MAP_STACK:            usize = 0x20000;
    const MAP_HUGETLB:          usize = 0x40000;
    const MAP_SYNC:             usize = 0x80000;
    const MAP_FIXED_NOREPLACE:  usize = 0x100000;
    // Mask of flags we silently accept (treat as no-op or fall through to
    // the standard private/anon/file path). Anything outside this set we
    // refuse with -EINVAL so LTP `mmap16` / `mmap-flag` cases get clean
    // errno instead of corrupting our VMA tree.
    const KNOWN_FLAGS_MASK: usize = MAP_SHARED | MAP_PRIVATE | MAP_FIXED
        | MAP_ANONYMOUS | MAP_GROWSDOWN | MAP_DENYWRITE | MAP_EXECUTABLE
        | MAP_LOCKED | MAP_NORESERVE | MAP_POPULATE | MAP_NONBLOCK
        | MAP_STACK | MAP_HUGETLB | MAP_SYNC | MAP_FIXED_NOREPLACE
        | 0x00FF_0000; // huge-page-size selector bits (MAP_HUGE_2MB etc.)
    if flags & !KNOWN_FLAGS_MASK != 0 {
        return -22;
    }
    // prot flags
    const PROT_READ:  usize = 0x1;
    const PROT_WRITE: usize = 0x2;
    const PROT_EXEC:  usize = 0x4;
    const PROT_GROWSDOWN: usize = 0x01000000;
    const PROT_GROWSUP:   usize = 0x02000000;
    const KNOWN_PROT_MASK: usize = PROT_READ | PROT_WRITE | PROT_EXEC
        | PROT_GROWSDOWN | PROT_GROWSUP;
    if prot & !KNOWN_PROT_MASK != 0 {
        return -22;
    }

    // 目前支持匿名映射和文件映射
    let mut is_anonymous = flags & MAP_ANONYMOUS != 0;
    let is_shared = flags & MAP_SHARED != 0;

    // mmap(/dev/zero) 等同匿名内存（Linux 语义）：MAP_PRIVATE → 私有 demand-zero
    // 页，MAP_SHARED → 共享匿名页。字符设备没有 EOF，若按文件映射处理则每一页都
    // “越过 EOF”→ 首次访问即 SIGBUS（mmap10 用 /dev/zero 做匿名映射）。按 /dev/zero
    // 的 device id (major=1, minor=5) 识别，转为匿名映射，忽略 fd/offset。
    if !is_anonymous {
        let dev_id = {
            let tg = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref t) = *tg {
                if let Some(pid) = t.current {
                    t.get_fd_process(pid)
                        .and_then(|process| fd_table::get(process, fd))
                        .and_then(|fobj| fobj.file.device_id())
                } else { None }
            } else { None }
        };
        if dev_id == Some((1, 5)) {
            is_anonymous = true;
        }
    }

    // hugetlbfs 文件映射：测例先 `mount -t hugetlbfs none <dir>`，再在该目录下
    // open() 一个文件并 mmap（不带 MAP_HUGETLB）。这种映射在 Linux 上由大页池
    // 按需补大页、零填充；文件 i_size 起始为 0，但访问映射区不应 SIGBUS（与普通
    // 文件「越过 EOF→SIGBUS」相反）。识别 fd 路径所在挂载是否 hugetlbfs，若是
    // 则按匿名大页（demand-zero）处理：标 huge、忽略文件 backing 与 EOF 检查。
    // MFD_HUGETLB memfd 与 hugetlbfs 文件同治：memfd_hugetlb_size() 非 0 时
    // 映射同样按大页 demand-zero 处理（memfd_create03）。
    let mut memfd_huge_size = 0usize;
    let is_hugetlb_file = if !is_anonymous && flags & MAP_HUGETLB == 0 {
        let (path, inode_huge) = {
            let tg = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref t) = *tg {
                if let Some(pid) = t.current {
                    match t.get_fd_process(pid)
                        .and_then(|process| fd_table::get(process, fd))
                    {
                        Some(fobj) => {
                            (fobj.path.clone(), fobj.file.memfd_hugetlb_size())
                        }
                        None => (None, 0),
                    }
                } else { (None, 0) }
            } else { (None, 0) }
        };
        memfd_huge_size = inode_huge;
        inode_huge != 0
            || path
                .map(|p| {
                    crate::fs::mount::fstype_for_path(&p).as_deref()
                        == Some("hugetlbfs")
                })
                .unwrap_or(false)
    } else {
        false
    };
    // For a hugetlbfs file mapping we capture the backing inode (so the
    // per-inode shared huge cache can key on it for MAP_SHARED/MAP_PRIVATE
    // coherence) but still treat it as demand-zero (no eager file read,
    // no EOF→SIGBUS), so set is_anonymous to skip the file_data path below.
    let hugetlb_inode: Option<alloc::sync::Arc<dyn crate::fs::Inode>> = if is_hugetlb_file {
        let inode = {
            let tg = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref t) = *tg {
                if let Some(pid) = t.current {
                    t.get_fd_process(pid)
                        .and_then(|process| fd_table::get(process, fd))
                        .map(|fobj| fobj.file.clone())
                } else { None }
            } else { None }
        };
        is_anonymous = true;
        inode
    } else {
        None
    };
    // MAP_FIXED_NOREPLACE: like MAP_FIXED but fails if [addr, addr+len) is
    // already mapped. Honor it by scanning the VMA tree for overlap.
    if flags & MAP_FIXED_NOREPLACE != 0 && addr != 0 {
        let aligned_addr = addr & !(PAGE_SIZE - 1);
        let aligned_end  = aligned_addr + ((len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                let as_pid = match t.get(pid) {
                    Some(p) => if p.mm.is_some() { pid } else { p.tgid },
                    None => return -1,
                };
                if let Some(p) = t.get(as_pid) {
                    if let Some(ref mm) = p.mm {
                        let addr_space = mm.lock();
                        for vma in addr_space.vmas_iter() {
                            if aligned_addr < vma.end.0 && aligned_end > vma.start.0 {
                                return -17; // -EEXIST per MAP_FIXED_NOREPLACE
                            }
                        }
                    }
                }
            }
        }
    }

    // MAP_HUGETLB: validate size and alignment against the requested huge page
    // size. The high 6 bits of `flags` encode log2(page_size) when MAP_HUGE_*
    // is set (Linux MAP_HUGE_SHIFT = 26). Default is 2MB.
    let huge_page_size: usize = if flags & MAP_HUGETLB != 0 {
        let log2_size = (flags >> 26) & 0x3f;
        let log2 = if log2_size == 0 { 21 } else { log2_size }; // default 2MB
        if log2 < 12 || log2 > 30 {
            return -22;
        }
        1usize << log2
    } else if is_hugetlb_file {
        // hugetlbfs 默认大页（2MiB）；MFD_HUGETLB memfd 用创建时编码的尺寸。
        // 长度按大页向上取整（Linux 在 hugetlbfs 上把映射长度圆整到大页
        // 边界），故此处不做 len 对齐校验。
        if memfd_huge_size != 0 { memfd_huge_size } else { 2 * 1024 * 1024 }
    } else {
        0
    };
    // 大页映射（MAP_HUGETLB 匿名 或 hugetlbfs 文件）的对齐校验，Linux 语义：
    //   * offset 必须按大页对齐（hugetlbfs_file_mmap 检查 vm_pgoff & (HPAGE-1)）；
    //     bogus/未对齐 offset → EINVAL（hugemmap19）。
    //   * MAP_FIXED 时 addr 必须按大页对齐 → 否则 EINVAL（hugemmap18）。
    //   * 仅 MAP_HUGETLB 匿名大页额外要求 len 精确对齐；hugetlbfs 文件映射把 len
    //     向上圆整到大页边界，不做精确校验。
    if huge_page_size != 0 {
        if offset % huge_page_size != 0 {
            return -22;
        }
        if (flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0) && addr % huge_page_size != 0 {
            return -22;
        }
        if flags & MAP_HUGETLB != 0 && len % huge_page_size != 0 {
            return -22;
        }
    }

    let is_stack_growsdown = flags & (MAP_GROWSDOWN | MAP_STACK) != 0;
    let _ = (MAP_LOCKED, MAP_POPULATE,
             MAP_NONBLOCK, MAP_NORESERVE, MAP_SYNC, MAP_DENYWRITE, MAP_EXECUTABLE);

    // 对文件映射，保留 backing inode（Arc），供可写 MAP_SHARED 在
    // msync/munmap 时把脏页写回文件 (msync01/02)，以及 demand paging 时
    // 按需从文件读取页数据。
    // 仅 MAP_POPULATE 才在 mmap 时预读全部文件数据；MAP_SHARED 与
    // MAP_PRIVATE 文件映射都改为按需分页（demand paging）：page fault
    // handler 在首次访问时逐页拉入（MAP_SHARED 走 per-inode 共享页缓存，
    // MAP_PRIVATE 走私有副本）。这既对标 Linux（连 MAP_SHARED 也是 demand
    // -paged），又让 lat_pagefault 的 remap 循环真正缺页可测（避免预读把页
    // 提前驻留导致测得 0/负值），还大幅减少 mmap 时的内存分配和 I/O。
    let mut backing_inode: Option<alloc::sync::Arc<dyn crate::fs::Inode>> = None;
    let needs_eager_file_read = flags & 0x8000 != 0; // MAP_POPULATE only
    let file_data: Option<Vec<u8>> = if !is_anonymous {
        // 从 fd 获取 FileDescriptor
        let fd_obj = {
            let tg = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref t) = *tg {
                if let Some(pid) = t.current {
                    if let Some(process) = t.get_fd_process(pid) {
                        fd_table::get(process, fd).map(|fobj| fobj.file.clone())
                    } else { None }
                } else { None }
            } else { None }
        };
        match fd_obj {
            Some(inode) => {
                backing_inode = Some(inode.clone());
                if inode.as_file().is_none() {
                    return -22; // -EINVAL: not a file
                }
                if needs_eager_file_read {
                    // MAP_SHARED / MAP_POPULATE: pre-read file content for
                    // eager page allocation (shared page cache or explicit populate)
                    let file = inode.as_file().unwrap();
                    let stat = file.stat().map_err(|_| ()).ok();
                    let file_size = stat.map(|s| s.size).unwrap_or(0);
                    let available = file_size.saturating_sub(offset);
                    let read_len = core::cmp::min(len, available);
                    let mut buf = alloc::vec![0u8; read_len];
                    let mut read_total = 0usize;
                    while read_total < read_len {
                        match file.read(offset + read_total, &mut buf[read_total..]) {
                            Ok(0) => break,
                            Ok(n) => read_total += n,
                            Err(_) => return -5,
                        }
                    }
                    Some(buf)
                } else {
                    // MAP_PRIVATE: lazy — file data will be read on demand by
                    // page fault handler (demand paging via map_page)
                    None
                }
            }
            None => return -9, // -EBADF
        }
    } else {
        None
    };

    let _ = if is_anonymous { (fd, offset) } else { (0, 0) }; // 匿名映射忽略 fd 和 offset

    // 页对齐；大页映射把长度向上圆整到大页边界（Linux mmap_pgoff 对 hugetlbfs
    // 文件做 ALIGN(len, huge_page_size)）。否则 4K 长度的 VMA 与按大页粒度做的
    // 池预留失配：munmap 只覆盖 4K span → 预留永远不释放（hugemmap18 泄漏
    // HugePages_Rsvd）。
    let aligned_len = if huge_page_size != 0 {
        (len + huge_page_size - 1) & !(huge_page_size - 1)
    } else {
        (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
    };

    let perm = VmaPermission::new(
        prot & PROT_READ != 0,
        prot & PROT_WRITE != 0,
        prot & PROT_EXEC != 0,
        true,
    );

    // PT 只做快照（**块作用域**：guard 的析构按词法作用域在块尾执行——NLL 只
    // 提前结束借用，不会提前 drop guard；第一版误把遮蔽后以为已释放，审查 P0
    // 纠正）。旧实现 PT 横跨整个选址/建 VMA/mm 段，持 PT 等 mm 锁——BuildStorm
    // 8 核下 sys_mmap 单次持锁 2.86s（pt_lock max_hold 实测）根因。下方 mm 段
    // 独立持 mm 锁，TOCTOU 防护由「选址+插入同一次 mm 锁临界区」保证。
    let (lock_needed, lk_euid, lk_cap, lk_memlock, mm_arc) = {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let table = match table.as_mut() {
            Some(t) => t,
            None => return -1,
        };
        let pid = match table.current {
            Some(p) => p,
            None => return -1,
        };
        // Thread fallback: use tgid leader's address space
        let as_pid = match table.get(pid) {
            Some(p) => if p.mm.is_some() { pid } else { p.tgid },
            None => return -1,
        };
        let process = match table.get_mut(as_pid) {
            Some(p) => p,
            None => return -1,
        };
        // MAP_LOCKED locks the new mapping into RAM (like an mlock over the
        // range). Capture the caller's credentials/RLIMIT_MEMLOCK now, before
        // the mutable borrow of process.mm below (mmap14 expects VmLck to grow
        // by the mapping size). Index 8 == RLIMIT_MEMLOCK, bit 14 == CAP_IPC_LOCK.
        let lock_needed = flags & MAP_LOCKED != 0;
        let (lk_euid, lk_cap, lk_memlock) =
            (process.euid, process.cap_effective, process.rlimits[8].0);
        (lock_needed, lk_euid, lk_cap, lk_memlock, process.mm.clone())
    };
    // 语义对齐旧实现：无 mm 的调用者（内核线程形态）直接 -1，不得落到
    // 下方占位 map_addr=0 返回“成功”（审查 P2-1）。
    if mm_arc.is_none() {
        return -1;
    }
    // 地址方案只做纯参数决策；真正的选址必须与 add_vma 在**同一次** mm 锁
    // 临界区内完成。旧实现"锁内 auto_alloc → 解锁 → 再锁 add_vma"存在
    // TOCTOU：两个线程并发 mmap 会拿到同一空闲地址、插入两条完全重叠的
    // VMA；先完成的一方 munmap 时把对方的映射一并删除——BuildStorm cargo
    // posix_spawn 子栈随机 SEGV_MAPERR（进而 rustc SIGSEGV/SIGILL）的根因。
    enum AddrPlan {
        Fixed(usize),
        Hint(usize),
        Auto,
    }
    let addr_plan = if addr != 0 && flags & MAP_FIXED != 0 {
        AddrPlan::Fixed(addr & !(PAGE_SIZE - 1))
    } else if addr != 0 {
        let hint_alignment = if huge_page_size != 0 { huge_page_size } else { PAGE_SIZE };
        AddrPlan::Hint((addr + hint_alignment - 1) & !(hint_alignment - 1))
    } else {
        AddrPlan::Auto
    };
    // MAP_FIXED 的越界/保留区校验不依赖 vmas，可先行；Hint/Auto 的最终地址
    // 在锁内决定后回填。
    let mut map_addr = match addr_plan {
        AddrPlan::Fixed(a) => a,
        // 占位：真实值在下方锁内计算后回填。
        AddrPlan::Hint(a) => a,
        AddrPlan::Auto => 0,
    };

    // MAP_FIXED 要求 [map_addr, map_addr+aligned_len) 完整落在用户地址空间内。
    // 越界（含 addr+len 回绕，如 mmap15 用 (void*)-page_size）→ EINVAL/ENOMEM。
    // 用户区上界取栈顶之上的 0x80_0000_0000（512GiB），既覆盖正常程序/栈/mmap，
    // 又能拒绝内核/非规范地址（mmap15）。
    if flags & MAP_FIXED != 0 {
        const USER_VA_MAX: usize = 0x80_0000_0000;
        match map_addr.checked_add(aligned_len) {
            None => return -22,                 // 回绕 → EINVAL
            Some(e) if e > USER_VA_MAX => return -12, // 越过用户区 → ENOMEM
            // MAP_FIXED 强制地址若落入 RISC-V 内核保留区则无法映射 → ENOMEM
            Some(e) if overlaps_kernel_reserved(map_addr, e) => return -12,
            _ => {}
        }
    }

    // Linux 强制 `vm.max_map_count`（mmap_region）：VMA 数超上限 → ENOMEM。
    // 与 OLD 实现同序：**在 huge 预留之前**判定（短 mm 临界区读计数，不持 PT），
    // 越限直接返回、不留预留泄漏面（审查 P1-1）。
    if flags & MAP_FIXED == 0 {
        let over = mm_arc.as_ref().map_or(false, |mm| {
            mm.lock().vma_count() > crate::fs::proc::sys::max_map_count() as usize
        });
        if over {
            return -12; // ENOMEM
        }
    }

    // HugeTLB reservation follows Linux's lazy model. A MAP_NORESERVE huge
    // mapping reserves nothing at mmap — its pages (ordinary or overcommit
    // surplus) are instantiated at fault, SIGBUS when the pool and the
    // `nr_overcommit_hugepages` quota are both exhausted. Every other huge
    // mapping (MAP_SHARED or private) reserves `HugePages_Rsvd` up front
    // (Free unchanged until a fault commits); an insufficient pool + quota
    // fails the mmap with ENOMEM.
    let huge_noreserve = huge_page_size != 0 && flags & MAP_NORESERVE != 0;
    // A MAP_SHARED hugetlbfs file mapping reserves against the *inode* (Linux
    // resv_map): the reservation is charged once per inode region and survives
    // munmap, released only on ftruncate / inode eviction. Re-mmapping the same
    // file region is idempotent (no double reservation, hugemmap23). Anonymous
    // MAP_HUGETLB and MAP_PRIVATE huge mappings keep the VMA-driven reservation.
    let huge_shared_file = huge_page_size != 0 && is_shared && hugetlb_inode.is_some();
    let reserved_huge_pages = if huge_page_size != 0 && !huge_noreserve {
        let pages = (len + huge_page_size - 1) / huge_page_size;
        if huge_shared_file {
            let ino = hugetlb_inode.as_ref().unwrap().ino();
            let start_idx = offset / huge_page_size;
            if !crate::mm::hugetlb_shared_reserve_map(ino, start_idx, start_idx + pages) {
                return -12;
            }
            // The inode owns this reservation; keep `reserved_huge_pages == 0`
            // so the VMA-driven failure-cleanup below never double-frees it.
            0
        } else {
            if !crate::mm::hugetlb_reserve_map(pages) {
                return -12;
            }
            pages
        }
    } else {
        0
    };

    if let Some(mm) = mm_arc {
    let mut addr_space = mm.lock();
    // 终定映射地址：Hint 冲突重定位与 Auto 选址都在**本临界区**内完成，
    // 与下方 add_vma 原子；并发线程在同一锁上串行，不可能重复选中同一空洞。
    match addr_plan {
        AddrPlan::Fixed(_) => {}
        AddrPlan::Hint(aligned_hint) => {
            let hint_end = aligned_hint + aligned_len;
            let conflicts_with_heap = addr_space.vmas_iter().any(|vma| {
                vma.vma_type == VmaType::Heap
                    && aligned_hint < vma.end.0
                    && hint_end > vma.start.0
            });
            if conflicts_with_heap || overlaps_kernel_reserved(aligned_hint, hint_end) {
                let alignment =
                    if huge_page_size != 0 { huge_page_size } else { PAGE_SIZE };
                match auto_alloc_mmap_addr_aligned(&*addr_space, aligned_len, alignment) {
                    Some(c) => map_addr = c,
                    None => {
                        if reserved_huge_pages != 0 {
                            crate::mm::hugetlb_unmap(reserved_huge_pages, 0, true);
                        }
                        return -12;
                    }
                }
            }
        }
        AddrPlan::Auto => {
            let alignment = if huge_page_size != 0 { huge_page_size } else { PAGE_SIZE };
            match auto_alloc_mmap_addr_aligned(&*addr_space, aligned_len, alignment) {
                Some(c) => map_addr = c,
                None => {
                    if reserved_huge_pages != 0 {
                        crate::mm::hugetlb_unmap(reserved_huge_pages, 0, true);
                    }
                    if huge_page_size != 0 {
                        crate::println!(
                            "[hgdbg] auto_alloc FAIL aligned_len={:#x} align={:#x}",
                            aligned_len, alignment
                        );
                    }
                    return -12;
                }
            }
        }
    }
    {
        // MAP_FIXED: Linux 语义要求先取消旧映射
        if flags & MAP_FIXED != 0 {
            let stats = addr_space.unmap_region(VirtAddr(map_addr), VirtAddr(map_addr + aligned_len));
            if stats.hugetlb_bytes_released != 0 && stats.huge_page_size != 0 {
                let hpages = stats.hugetlb_bytes_released / stats.huge_page_size;
                crate::mm::hugetlb_unmap(hpages, stats.huge_touched_pages, stats.huge_reserved);
            }
        }
        if prot != 0 {
            // 有权限：立即映射物理页（eager mapping）
            let vma_kind = if is_stack_growsdown {
                VmaType::Stack
            } else if is_shared {
                VmaType::Shared
            } else {
                VmaType::Mmap
            };
            let mut vma = Vma::new(
                VirtAddr(map_addr),
                VirtAddr(map_addr + aligned_len),
                vma_kind,
                perm,
            );
            if huge_page_size != 0 {
                vma.mark_huge(huge_page_size);
                vma.huge_reserved = !huge_noreserve;
            }
            vma.file_backed = !is_anonymous;
            // VM_MAYWRITE: a MAP_SHARED file mapping can only be made writable
            // later (mprotect) if the backing fd is writable; everything else
            // (private/anon) is always permissible.
            // Also deny may_write when F_SEAL_WRITE is set on the backing file.
            let seal_deny_write = if !is_anonymous && is_shared {
                backing_inode.as_ref().and_then(|inode| {
                    inode.shared_seals().map(|s| {
                        let cur = s.load(core::sync::atomic::Ordering::Acquire);
                        cur & 0x0008 != 0 || cur & 0x0010 != 0 // F_SEAL_WRITE | F_SEAL_FUTURE_WRITE
                    })
                }).unwrap_or(false)
            } else {
                false
            };
            // Linux do_mmap：任何带 fd 的 MAP_SHARED（含 /dev/zero 转匿名的情形）
            // 若 fd 不可写则清 VM_MAYWRITE，后续 mprotect(PROT_WRITE) 必 EACCES。
            // 判据是 syscall 层是否传了真 fd（file_perm=Some），而非派生的
            // is_anonymous（mprotect01 case 3）。
            vma.may_write = !seal_deny_write
                && (!is_shared || file_perm.map(|(_, w)| w).unwrap_or(true));
            vma.backing = backing_inode.clone();
            vma.file_path = if backing_inode.is_some() {
                file_path.clone()
            } else {
                None
            };
            // hugetlbfs file mapping: attach the backing inode so the huge
            // fault path keys the per-inode shared huge cache on it, while
            // file_backed stays false (no writeback to the real file).
            if hugetlb_inode.is_some() {
                vma.backing = hugetlb_inode.clone();
                vma.file_path = file_path.clone();
            }
            vma.file_offset = offset;
            if is_anonymous && is_shared && huge_page_size == 0 {
                // Back the anonymous MAP_SHARED region with a shmem object and
                // demand-page it (Linux semantics): pages are allocated on first
                // touch, not eagerly, so mincore(2)/mlock2(2) observe the true
                // per-page residency, while fork still shares the same frames
                // through the shmem object.
                vma.init_anon_shared();
            }
            // 文件映射：将文件数据拷贝到映射区域
            // For MAP_SHARED file-backed mappings, use the per-inode page
            // cache so all processes mapping the same file share the same
            // physical frames (Linux page-cache semantics).
            if let Some(ref data) = file_data {
                let use_page_cache = is_shared && !is_anonymous && backing_inode.is_some();
                // 页缓存 key 必须是稳定的 inode 身份（inode 号），不能用 Arc 的
                // 对象地址：地址会随 inode 释放被复用，导致命中另一个文件遗留的
                // 缓存页（见 address_space::SHARED_PAGE_CACHE 注释）。
                let inode_id = backing_inode.as_ref().map(|i| i.ino()).unwrap_or(0);

                let first_vpn = VirtAddr(map_addr).floor().0;
                let map_end = map_addr + aligned_len;
                let last_vpn = VirtAddr(map_end - 1).floor().0;

                if use_page_cache {
                    // MAP_SHARED file-backed: use shared page cache.
                    // Only pre-map pages that overlap the file content.
                    // Pages entirely beyond EOF must remain unmapped so that
                    // accessing them triggers a page fault → SIGBUS (mmap13).
                    let file_size_from_offset = data.len(); // bytes of file content from offset
                    let eof_page_count = (file_size_from_offset + PAGE_SIZE - 1) / PAGE_SIZE;
                    let map_up_to_vpn = first_vpn + eof_page_count;
                    let effective_last = if map_up_to_vpn <= last_vpn { map_up_to_vpn.saturating_sub(1) } else { last_vpn };
                    for vpn_num in first_vpn..=effective_last {
                        let vpn = crate::mm::VirtPageNum(vpn_num);
                        let va_base = vpn_num * PAGE_SIZE;
                        let file_page_offset = offset + (va_base - map_addr);
                        let page_index = file_page_offset / PAGE_SIZE;

                        let frame_arc = match crate::mm::address_space::shared_cache_get_or_insert_with(
                            inode_id,
                            page_index,
                            |kva| {
                                // 填充在共享缓存锁内完成：两个 hart 同时 mmap 同一
                                // MAP_SHARED 页时，后到者必须拿到已填充帧，而不是
                                // meta_ref 判定竞态下映射到半零页。
                                let page_file_start = file_page_offset;
                                if page_file_start < data.len() + offset {
                                    let data_offset = page_file_start.saturating_sub(offset);
                                    let avail = data.len().saturating_sub(data_offset);
                                    let copy_len = avail.min(PAGE_SIZE);
                                    if copy_len > 0 {
                                        kva[..copy_len].copy_from_slice(
                                            &data[data_offset..data_offset + copy_len],
                                        );
                                    }
                                }
                                true
                            },
                        ) {
                            Some(f) => f,
                            None => {
                                if reserved_huge_pages != 0 {
                                    crate::mm::hugetlb_unmap(reserved_huge_pages, 0, true);
                                }
                                return -12;
                            }
                        };
                        let ppn = frame_arc.ppn;

                        // Map the shared frame into this process's page table
                        if addr_space.page_table().translate(vpn).is_none() {
                            addr_space.page_table_mut().map(vpn, ppn, perm.to_pte_flags());
                        }
                        vma.insert_shared_frame(vpn, frame_arc);
                    }
                } else {
                    // MAP_PRIVATE or anonymous: allocate private pages.
                    // For file-backed private mappings, only pre-map pages
                    // within the file content. Pages beyond EOF must remain
                    // unmapped to trigger SIGBUS on access (mmap13).
                    if !data.is_empty() {
                        let priv_eof_pages = (data.len() + PAGE_SIZE - 1) / PAGE_SIZE;
                        let priv_last = if !is_anonymous {
                            let limit = first_vpn + priv_eof_pages;
                            if limit <= last_vpn { limit.saturating_sub(1) } else { last_vpn }
                        } else {
                            last_vpn
                        };
                        for vpn_num in first_vpn..=priv_last {
                            if !vma.map_page(crate::mm::VirtPageNum(vpn_num), addr_space.page_table_mut()) {
                                if reserved_huge_pages != 0 {
                                    crate::mm::hugetlb_unmap(reserved_huge_pages, 0, true);
                                }
                                return -12;
                            }
                        }
                    }
                    let pt = addr_space.page_table();
                    let mut copied = 0usize;
                    while copied < data.len() {
                        let va = VirtAddr(map_addr + copied);
                        let vpn = va.floor();
                        let page_offset = va.page_offset();
                        let copy_len = (PAGE_SIZE - page_offset).min(data.len() - copied);
                        if let Some(ppn) = pt.translate(vpn) {
                            let kva = super::ppn_offset_to_kernel_ptr(ppn, page_offset) as *mut u8;
                            unsafe {
                                core::ptr::copy_nonoverlapping(
                                    data.as_ptr().add(copied),
                                    kva,
                                    copy_len,
                                );
                            }
                        }
                        copied += copy_len;
                    }
                }
            }
            // Track writable MAP_SHARED mappings for F_SEAL_WRITE enforcement
            if is_shared && !is_anonymous && prot & PROT_WRITE != 0 {
                if let Some(ref inode) = backing_inode {
                    inode.inc_writable_maps();
                }
            }
            // MAP_POPULATE: prefault the whole anonymous region so the pages are
            // resident immediately (advisory in Linux; we honour it for anon).
            if is_anonymous && flags & 0x8000 != 0 {
                let first_vpn = VirtAddr(map_addr).floor().0;
                let last_vpn = VirtAddr(map_addr + aligned_len - 1).floor().0;
                // 只读私有匿名 populate 走全局共享零页（Linux populate 即读故障，
                // do_anonymous_page 映射 ZERO_PAGE），零物理内存消耗；可写/共享
                // 段按写故障语义分真实帧。
                let zero_populate = prot & PROT_WRITE == 0
                    && !is_shared
                    && huge_page_size == 0
                    && !lock_needed;
                for vpn_num in first_vpn..=last_vpn {
                    let vpn = crate::mm::VirtPageNum(vpn_num);
                    if zero_populate {
                        vma.map_zero_page(vpn, addr_space.page_table_mut());
                    } else {
                        vma.map_page(vpn, addr_space.page_table_mut());
                    }
                }
            }
            addr_space.add_vma(vma);
            if lock_needed {
                let privileged = lk_euid == 0 || (lk_cap & (1u64 << 14)) != 0;
                let within_limit = privileged
                    || (lk_memlock != 0
                        && addr_space
                            .locked_vm_bytes()
                            .saturating_add(aligned_len as u64)
                            <= lk_memlock);
                if within_limit {
                    addr_space.lock_region(
                        VirtAddr(map_addr),
                        VirtAddr(map_addr + aligned_len),
                        true,
                        prot != 0,
                    );
                }
            }

        } else {
            // PROT_NONE：只创建 VMA，不映射物理页
            let vma_kind = if is_stack_growsdown {
                VmaType::Stack
            } else if is_shared {
                VmaType::Shared
            } else {
                VmaType::Mmap
            };
            let mut vma = Vma::new(
                VirtAddr(map_addr),
                VirtAddr(map_addr + aligned_len),
                vma_kind,
                perm,
            );
            if huge_page_size != 0 {
                vma.mark_huge(huge_page_size);
                vma.huge_reserved = !huge_noreserve;
            }
            vma.file_backed = !is_anonymous;
            vma.may_write = !is_shared || file_perm.map(|(_, w)| w).unwrap_or(true);
            vma.backing = backing_inode.clone();
            vma.file_path = if backing_inode.is_some() {
                file_path.clone()
            } else {
                None
            };
            if hugetlb_inode.is_some() {
                vma.backing = hugetlb_inode.clone();
                vma.file_path = file_path.clone();
            }
            vma.file_offset = offset;
            addr_space.add_vma(vma);
            if lock_needed {
                let privileged = lk_euid == 0 || (lk_cap & (1u64 << 14)) != 0;
                let within_limit = privileged
                    || (lk_memlock != 0
                        && addr_space
                            .locked_vm_bytes()
                            .saturating_add(aligned_len as u64)
                            <= lk_memlock);
                if within_limit {
                    addr_space.lock_region(
                        VirtAddr(map_addr),
                        VirtAddr(map_addr + aligned_len),
                        true,
                        false,
                    );
                }
            }
        }
        // 建立点自检：mmap 返回的区间必须真被单条 VMA 完整覆盖。BuildStorm 现场
        // 是线程栈只映射了开头一小段、SP 落在其上的空洞里；在这里比对请求区间与
        // 实际覆盖，即可区分「建立时就短」与「建立后被别的路径改小」。
        if crate::cmdline::lavmadbg_enabled() {
            let want_end = map_addr + aligned_len;
            let cur = crate::sched::process_sched::HART_CURRENT_PID
                .load(core::sync::atomic::Ordering::Relaxed);
            match addr_space.find_vma(VirtAddr(map_addr)) {
                Some(v) if v.start.0 <= map_addr && v.end.0 >= want_end => {}
                Some(v) => crate::println!(
                    "[MMAPCHK] short pid={} want=[{:#x},{:#x}) got=[{:#x},{:#x}) flags={:#x} prot={:#x}",
                    cur, map_addr, want_end, v.start.0, v.end.0, flags, prot,
                ),
                None => crate::println!(
                    "[MMAPCHK] missing pid={} want=[{:#x},{:#x}) flags={:#x} prot={:#x}",
                    cur, map_addr, want_end, flags, prot,
                ),
            }
        }
    }
    } else if reserved_huge_pages != 0 {
        // mmap failed after reserving: undo the pure (untouched) reservation,
        // returning any surplus allocated for it to the buddy allocator.
        crate::mm::hugetlb_unmap(reserved_huge_pages, 0, true);
        return -12;
    }
    // MAP_FIXED 覆盖了旧映射（unmap_region 只发异步 shootdown）：新映射
    // 建立后其它线程的陈旧 TLB 仍指旧帧，读写命中旧数据而非新内容
    // （ld.so/allocator 常用 MAP_FIXED 原地替换）。与 munmap 同语义，
    // 返回前在锁外同步收尾。
    if flags & MAP_FIXED != 0 {
        crate::mm::tlb::drain_shootdown();
    }
    map_addr as isize
}
pub(crate) fn sys_munmap(addr: usize, len: usize) -> isize {
    use crate::mm::{VirtAddr, PAGE_SIZE};
    if len == 0 || addr % PAGE_SIZE != 0 {
        return -22; // -EINVAL
    }
    let aligned_len = (len + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    // PT 只做 pid/mm 解析（与 sys_mmap 同款收窄）：持 PT 等 mm 锁会把
    // unmap_region（摘 PTE + 异步 shootdown + 批量回收）整段塞进全局临界区，
    // 8 核编译期所有 hart 的 syscall 全部排队。mm Arc 拷出后 PT 即释放。
    let mm_arc = {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let mut mm = None;
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                let as_pid = match t.get(pid) {
                    Some(p) => if p.mm.is_some() { pid } else { p.tgid },
                    None => return 0,
                };
                if let Some(process) = t.get(as_pid) {
                    mm = process.mm.clone();
                }
            }
        }
        mm
    };
    let stats = if let Some(mm) = mm_arc {
        let mut addr_space = mm.lock();
        // Linux hugetlb VMA 只能按大页边界切分（__split_vma →
        // hugetlb_vm_op_split）：munmap 范围若部分覆盖 huge VMA
        // 且切点不在大页边界 → EINVAL（memfd_create03）。
        let end = addr + aligned_len;
        for vma in addr_space.vmas_iter() {
            let hps = vma.huge_page_size;
            if hps == 0 || addr >= vma.end.0 || end <= vma.start.0 {
                continue;
            }
            let cut_start = addr > vma.start.0 && addr % hps != 0;
            let cut_end = end < vma.end.0 && end % hps != 0;
            if cut_start || cut_end {
                return -22;
            }
        }
        addr_space.unmap_region(VirtAddr(addr), VirtAddr(addr + aligned_len))
    } else {
        crate::mm::UnmapStats::default()
    };
    if stats.hugetlb_bytes_released != 0 && stats.huge_page_size != 0 {
        let hpages = stats.hugetlb_bytes_released / stats.huge_page_size;
        crate::mm::hugetlb_unmap(hpages, stats.huge_touched_pages, stats.huge_reserved);
    }
    // munmap(2) 返回后，本 mm 所有线程对该区间的访问必须立即 SIGSEGV，且
    // glibc 随后在同址 mmap 的新映射不得被旧 TLB 条目短路成旧帧内容。
    // unmap_region 内部只发异步 shootdown；此处（已出 PROCESS_TABLE/mm 锁）
    // 同步等待全部对端失效完成，对齐 Linux munmap 的同步 flush 语义。
    crate::mm::tlb::drain_shootdown();
    0
}

/// 调整堆大小（brk 系统调用）
/// Linux 语义：brk(0) 返回当前 brk 地址，brk(addr) 设置新 brk
/// 成功返回新 brk 地址，失败返回当前 brk 地址
pub(in crate::syscall) fn sys_brk(addr: usize) -> isize {
    use crate::mm::VirtAddr;

    // 辅助函数：读取当前 brk（线程回退到 tgid leader）
    let current_brk = || -> usize {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                if let Some(p) = t.get(pid) {
                    let as_pid = if p.mm.is_some() { pid } else { p.tgid };
                    if let Some(leader) = t.get(as_pid) {
                        return leader.heap_end.0;
                    }
                }
            }
        }
        0
    };

    let old_brk = current_brk();
    let result = if addr == 0 {
        old_brk as isize
    } else {
        match crate::sched::process::brk(VirtAddr(addr)) {
            Ok(new_brk) => new_brk.0 as isize,
            Err(_) => old_brk as isize,
        }
    };
    // brk 收缩释放了堆尾页（unmap_region 异步 shootdown）：malloc trim 后
    // 兄弟线程不得再经陈旧 TLB 触到已 retire 的页。与 munmap 同语义收尾。
    if addr != 0 && (result as usize) < old_brk {
        crate::mm::tlb::drain_shootdown();
    }
    result
}
