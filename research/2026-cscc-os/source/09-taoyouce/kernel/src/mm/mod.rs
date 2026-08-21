//! 内存管理子系统
//! 
//! 包含物理内存分配、页表管理、地址空间管理
//!
//! # Per-CPU 缓存集成
//! 小对象分配优先使用 Per-CPU 缓存，减少锁竞争

pub mod page_meta;
pub mod typed_frame;
pub mod mm_struct;
pub mod gup;
mod frame;
mod heap;
pub mod page_table;
pub mod swap;
pub mod memcg;
mod address;
pub mod address_space;
pub mod page_cache;
pub mod mman;
pub mod mempolicy;
pub mod pmem;
pub mod percpu_cache;
pub mod slab;
mod pcp;  // per-CPU 页帧缓存（Linux per_cpu_pages 对标）
pub mod tlb;
pub mod asid;

#[cfg(test)]
mod percpu_cache_tests;
#[cfg(test)]
mod tlb_tests;

#[cfg(target_arch = "riscv64")]
pub use frame::enable_zicboz;
pub use frame::{
    frame_alloc, frame_alloc_uninit, frame_alloc_order, alloc_frame, dealloc_frame, zero_pool_refill,
    alloc_frames, alloc_frames_uninit, dealloc_frames, get_frame_ref, inc_frame_ref,
    dec_frame_ref,
    dealloc_frame_force, FrameTracker, SharedFrame, zero_page_frame,
    get_available_memory,
    // 帧调试取证开关（bootarg `FRAMEDBG=1`）
    set_frame_dbg,
    // 旧诊断接口（空操作占位，防止未清理的调用方编译失败）
    uaf_dbg_enabled, is_in_buddy_free_dbg, ft_live_count_dbg,
    target_log_dump_dbg, kstk_record_freed_dbg, kstk_running_on_freed_dbg,
    prov_dump_dbg, frame_ownership_dump_dbg,
    slab_owned_mark_dbg, slab_owned_clear_dbg, slab_slot_user_check_dbg,
};
pub use page_meta::{MetaSlot, FrameOwner, meta_of, try_meta_of};
pub use typed_frame::{
    FrameMeta, UniqueFrame, Frame, Segment,
    UserPageMeta, KernelStackMeta, SlabMeta, PageTableMeta,
    PageCacheMeta, HeapMeta, DmaMeta, SharedMemMeta, HypervisorMeta,
};
pub use mm_struct::{MmStruct, mmput_arc, mmget_clone};
#[allow(unused_imports)]
pub use heap::{init_heap, heap_stats, heap_size_bins};
pub use pcp::pcp_stats;
pub use page_table::{PageTable, PTEFlags, flush_tlb};
pub use address::{PhysAddr, VirtAddr, PhysPageNum, VirtPageNum, phys_to_kernel_vaddr};
pub use address_space::{AddressSpace, UnmapStats, Vma, VmaType, VmaPermission, FaultAction, FileReadInfo};

use crate::sync::SpinLock;

/// 默认 HugeTLB 池大小（每个 2 MiB 页）—— 对标真机 Linux：开机 `nr_hugepages`
/// 默认 **0**，巨页池由测试在 setup 阶段经 `/proc/sys/vm/nr_hugepages` 显式预留。
/// 这里必须为 0：LTP `tst_request_hugepages`（`lib/tst_hugepage.c:51-52`）对
/// `TST_NEEDS` 用例做的是「请求页数 + 当前 HugePages_Total」**累加**，若默认非 0
/// 会被加进去（例如默认 512 + 请求 2 = 预留 514 页），使「按 free 页数整池映射」
/// 的用例（hugemmap27 等）首个 mmap 就因 VA/池被撑满而 ENOMEM。
const DEFAULT_HUGEPAGES_TOTAL: usize = 0;
const DEFAULT_HUGEPAGE_KB: usize = 2048;

/// 整个 hugetlb 账本受一把 SpinLock 保护：在 `set_total` 时需要原子地
/// 调整 total 与 free 两个数，避免与并发的 reserve/release 出现负数。
/// HugeTLB 账本对标 Linux `mm/hugetlb.c`：
/// - `total`：池中大页总数（HugePages_Total），含 surplus。
/// - `free`：尚未实例化（未缺页提交）的大页数（HugePages_Free）。
/// - `rsvd`：已被映射「预留」但尚未实例化的大页数（HugePages_Rsvd）。
/// - `surp`：溢出（overcommit）分配出来的大页数（HugePages_Surp）。
/// - `overcommit`：`nr_overcommit_hugepages` 上限。
///
/// 关键语义（与 eager 模型相反）：MAP_SHARED / 私有预留映射在 mmap 时只增 `rsvd`
/// （不足时配额内增 surplus），`free` 不变；真正缺页（touch）时才 commit——`free--`
/// 且预留映射 `rsvd--`。这样 /proc/meminfo 的四个计数与 LTP hugemmap 系列对账。
struct HugeTlbBook {
    total: usize,
    free: usize,
    rsvd: usize,
    surp: usize,
    overcommit: usize,
    page_size_kb: usize,
}

static HUGETLB_BOOK: SpinLock<HugeTlbBook> = SpinLock::new(HugeTlbBook {
    total: DEFAULT_HUGEPAGES_TOTAL,
    free: DEFAULT_HUGEPAGES_TOTAL,
    rsvd: 0,
    surp: 0,
    overcommit: 0,
    page_size_kb: DEFAULT_HUGEPAGE_KB,
});

/// `hugetlb_reserve(N)`：N 个大页可用就预留并返回 true，否则原子失败。
/// eager 语义（`free--`、`rsvd++`），供 SysV shm / hugetlbfs fallocate 使用——
/// 这些路径在分配时即实例化大页，与 mmap 的惰性预留模型相互独立、不混用。
pub fn hugetlb_reserve(pages: usize) -> bool {
    if pages == 0 {
        return true;
    }
    let mut book = HUGETLB_BOOK.lock();
    if book.free < pages {
        return false;
    }
    book.free -= pages;
    book.rsvd += pages;
    true
}

/// `hugetlb_release(N)`：归还 N 个大页（来自 unmap / 文件 truncate）。
pub fn hugetlb_release(pages: usize) {
    if pages == 0 {
        return;
    }
    let mut book = HUGETLB_BOOK.lock();
    let give = pages.min(book.rsvd);
    book.rsvd -= give;
    book.free += give;
}

/// mmap 时为 MAP_SHARED（或支持私有预留时的 MAP_PRIVATE）映射建立 `pages` 个
/// 大页的预留：池内可预留页（`free - rsvd`）够则只增 `rsvd`；不足部分按
/// `nr_overcommit_hugepages` 配额申请 surplus（`total++ free++ surp++`）。配额不
/// 足返回 false（mmap → ENOMEM）。对标 `hugetlb_reserve_pages`/`alloc_surplus`。
pub fn hugetlb_reserve_map(pages: usize) -> bool {
    if pages == 0 {
        return true;
    }
    let mut book = HUGETLB_BOOK.lock();
    book_reserve(&mut book, pages)
}

/// 池侧预留 `pages` 个大页的内部实现（调用方持有 `HUGETLB_BOOK`）：池内可预留页
/// （`free - rsvd`）够则只增 `rsvd`；不足部分按 `nr_overcommit_hugepages` 配额申请
/// surplus（`total++ free++ surp++`）；配额不足返回 false（不做部分预留）。
fn book_reserve(book: &mut HugeTlbBook, pages: usize) -> bool {
    if pages == 0 {
        return true;
    }
    let avail = book.free.saturating_sub(book.rsvd);
    if avail >= pages {
        book.rsvd += pages;
        return true;
    }
    let shortfall = pages - avail;
    if book.surp + shortfall > book.overcommit {
        return false;
    }
    book.total += shortfall;
    book.free += shortfall;
    book.surp += shortfall;
    book.rsvd += pages;
    true
}

/// 缺页实例化一个「已预留」的大页（MAP_SHARED / 私有预留 touch）：`free--`、
/// `rsvd--`。预留已在 mmap 时保证，必定成功。
pub fn hugetlb_commit_reserved() {
    let mut book = HUGETLB_BOOK.lock();
    if book.free > 0 {
        book.free -= 1;
    }
    if book.rsvd > 0 {
        book.rsvd -= 1;
    }
}

/// 缺页实例化一个「未预留」的大页（MAP_NORESERVE / 不支持私有预留的 MAP_PRIVATE
/// touch）：优先用池内非预留空闲页（`free--`）；否则按配额申请 surplus
/// （`total++ surp++`，立即消费故 `free` 不变）。配额不足返回 `None`（→ SIGBUS），
/// 绝不挪用为别的映射预留的页。返回 `Some(true)` 表示用了 surplus 页。
pub fn hugetlb_commit_unreserved() -> Option<bool> {
    let mut book = HUGETLB_BOOK.lock();
    let avail = book.free.saturating_sub(book.rsvd);
    if avail >= 1 {
        book.free -= 1;
        return Some(false);
    }
    if book.surp + 1 > book.overcommit {
        return None;
    }
    book.total += 1;
    book.surp += 1;
    Some(true)
}

/// 缺页在物理帧分配失败时回滚一次 commit（极少触发；保证账本不漂移）。
pub fn hugetlb_commit_undo(reserved: bool, was_surplus: bool) {
    let mut book = HUGETLB_BOOK.lock();
    if reserved {
        book.free += 1;
        book.rsvd += 1;
    } else if was_surplus {
        book.total -= 1;
        book.surp -= 1;
    } else {
        book.free += 1;
    }
}

/// munmap 一个大页映射时归账，对标 LTP hugemmap10 的 UNMAP 模型与 `mm/hugetlb.c`：
/// - `hpages`：映射的大页数；`touched`：其中已实例化（缺页）的页数；
/// - `reserved`：该映射是否在 mmap 时建立了预留（MAP_SHARED / 私有预留）。
///
/// 已 touch 的页：池处于 surplus 状态时先把 surplus 退回 buddy（`total-- surp--`），
/// 否则回到池（`free++`）。预留映射里未被 touch 的预留页释放预留，并把为其分配的
/// 未使用 surplus 退回。
pub fn hugetlb_unmap(hpages: usize, touched: usize, reserved: bool) {
    let mut book = HUGETLB_BOOK.lock();
    let touched = touched.min(hpages);
    for _ in 0..touched {
        if book.surp > 0 {
            book.total = book.total.saturating_sub(1);
            book.surp -= 1;
        } else {
            book.free += 1;
        }
    }
    if reserved {
        let untouched = hpages - touched;
        let unused_surplus = untouched.min(book.surp);
        book.total = book.total.saturating_sub(unused_surplus);
        book.free = book.free.saturating_sub(unused_surplus);
        book.surp -= unused_surplus;
        book.rsvd = book.rsvd.saturating_sub(untouched);
    }
}

/// 设置 `nr_overcommit_hugepages`（surplus 配额上限）。
pub fn hugetlb_set_overcommit(n: usize) {
    HUGETLB_BOOK.lock().overcommit = n;
}

/// 读取当前 `nr_overcommit_hugepages`。
pub fn hugetlb_overcommit() -> usize {
    HUGETLB_BOOK.lock().overcommit
}

/// `hugetlb_snapshot()` → (total, free, rsvd, surp, page_size_kb)。
pub fn hugetlb_snapshot() -> (usize, usize, usize, usize, usize) {
    let book = HUGETLB_BOOK.lock();
    (book.total, book.free, book.rsvd, book.surp, book.page_size_kb)
}

/// `set_hugepages_total(n)`：将 hugetlb 池大小调整到 n，逐字对标
/// `mm/hugetlb.c:set_max_huge_pages`：增长先消化 surplus 再分配新页；收缩先退回
/// 空闲页（保住预留与已实例化），再把欠下的转成 surplus。返回最终 total。
pub fn set_hugepages_total(n: usize) -> usize {
    let mut book = HUGETLB_BOOK.lock();
    let count = n as i64;
    let mut es = book.surp as i64;
    let mut et = book.total as i64;
    let mut ef = book.free as i64;
    let er = book.rsvd as i64;

    // 增长：先把 surplus 转为常驻页，再分配新页补足差额。
    while es > 0 && count > et - es {
        es -= 1;
    }
    while count > et - es {
        et += 1;
        ef += 1;
    }
    if count < et - es {
        // 收缩：先把空闲页退回 buddy（保留满足预留所需），再把欠下的转 surplus。
        let min_size = core::cmp::max(count, er + et - ef);
        while min_size < et - es {
            ef -= 1;
            et -= 1;
        }
        while count < et - es {
            es += 1;
        }
    }

    book.total = et.max(0) as usize;
    book.free = ef.max(0) as usize;
    book.surp = es.max(0) as usize;
    book.total
}

/// `hugepages_total()`：`/proc/sys/vm/nr_hugepages` 报告的「持久池」大小，即
/// 不含 surplus 的常驻大页数（对标 Linux `nr_huge_pages - surplus_huge_pages`）。
pub fn hugepages_total() -> usize {
    let book = HUGETLB_BOOK.lock();
    book.total.saturating_sub(book.surp)
}

/// `huge_page_size_kb()`：单页大小（KiB）。
pub fn huge_page_size_kb() -> usize {
    HUGETLB_BOOK.lock().page_size_kb
}

/// 每个 hugetlbfs 文件已预留的大页索引集合，按 backing inode 的数据指针地址
/// 索引。`fallocate()` 在 hugetlbfs 上以「整页」为单位预留（范围向上取整），
/// `FALLOC_FL_PUNCH_HOLE` 释放（范围向下取整）；两者都必须反映到全局大页池
/// （/proc/meminfo 的 HugePages_Free），且对同一页的重复操作必须幂等——因此
/// 需要按文件记录已分配的页索引集合（对标 Linux hugetlbfs 的 reservation map）。
static HUGETLB_FILE_PAGES: SpinLock<alloc::collections::BTreeMap<usize, alloc::collections::BTreeSet<usize>>> =
    SpinLock::new(alloc::collections::BTreeMap::new());

/// hugetlbfs 文件 fallocate 预留：把 `[start_idx, end_idx)` 中尚未 backing 的
/// 大页一次性从全局池预留。池不足时返回 `Err(())`（调用方映射 -ENOSPC），且
/// 不保留任何部分预留。已 backing 的页幂等跳过。
pub fn hugetlb_file_alloc(key: usize, start_idx: usize, end_idx: usize) -> Result<(), ()> {
    if start_idx >= end_idx {
        return Ok(());
    }
    let mut map = HUGETLB_FILE_PAGES.lock();
    let set = map.entry(key).or_default();
    let mut to_add = alloc::vec::Vec::new();
    for idx in start_idx..end_idx {
        if !set.contains(&idx) {
            to_add.push(idx);
        }
    }
    if !hugetlb_reserve(to_add.len()) {
        return Err(());
    }
    for idx in to_add {
        set.insert(idx);
    }
    Ok(())
}

/// hugetlbfs 文件 `FALLOC_FL_PUNCH_HOLE`：释放 `[start_idx, end_idx)` 中所有已
/// backing 的整页，归还到全局池。未 backing 的页幂等跳过。
pub fn hugetlb_file_punch(key: usize, start_idx: usize, end_idx: usize) {
    if start_idx >= end_idx {
        return;
    }
    let mut map = HUGETLB_FILE_PAGES.lock();
    if let Some(set) = map.get_mut(&key) {
        let victims: alloc::vec::Vec<usize> = set.range(start_idx..end_idx).copied().collect();
        let freed = victims.len();
        for idx in victims {
            set.remove(&idx);
        }
        if set.is_empty() {
            map.remove(&key);
        }
        hugetlb_release(freed);
    }
}

/// hugetlbfs 文件 inode 销毁时归还其仍持有的全部大页（最后一个引用关闭）。
/// 没有记录时为廉价的 no-op，故可无条件挂到 ramfs inode 的 Drop 上。
pub fn hugetlb_file_drop(key: usize) {
    let mut map = HUGETLB_FILE_PAGES.lock();
    if let Some(set) = map.remove(&key) {
        let n = set.len();
        hugetlb_release(n);
    }
}

// ── MAP_SHARED hugetlbfs 文件映射的 inode 自有预留模型 ──────────────────────
// 对标 Linux `mm/hugetlb.c` 的 `resv_map`：MAP_SHARED hugetlbfs 映射的预留与大页
// 归 **inode（文件）所有**，而非归某个 VMA。语义要点：
//   * mmap：对该 inode 的 `[start,end)` 区间「按需」建立预留——只对尚未预留/实例化
//     的页扣预留（避免对同一 fd 区间重复 mmap 造成重复预留，hugemmap23）。
//   * 缺页 touch（cache miss）：把对应页从 `resv` 迁到 `inst`，并向池 commit
//     （free--/rsvd--）；若该页已实例化（cache hit）则**不再 commit**——这是
//     hugemmap08/23 的关键修复（旧模型每次缺页都 commit，重映射时把 Free 扣穿）。
//   * munmap：**不归还**任何页（inode 仍持有），见 `unmap_region` 跳过共享文件大页。
//   * ftruncate / inode 关闭：一次性把该 inode（或被截掉区间）的 `inst`+`resv` 归还
//     到池（touched 退 free / surplus 退 buddy，未触碰预留退 rsvd），对标 Linux
//     hugetlbfs 在 truncate / inode evict 时释放页与预留。
struct HugeInodeAcct {
    /// 已预留（mmap 建立）但尚未实例化的大页索引，计入 HugePages_Rsvd。
    resv: alloc::collections::BTreeSet<usize>,
    /// 已实例化（缺页提交、常驻）的大页索引，对应共享 huge cache 中的物理页。
    inst: alloc::collections::BTreeSet<usize>,
}

impl Default for HugeInodeAcct {
    fn default() -> Self {
        Self {
            resv: alloc::collections::BTreeSet::new(),
            inst: alloc::collections::BTreeSet::new(),
        }
    }
}

/// 按 backing inode 号索引的 MAP_SHARED hugetlbfs 自有账本。锁序固定为
/// `HUGE_INODE_ACCT → HUGETLB_BOOK`（本文件中所有同时触及两者的函数均如此），
/// 不存在反向加锁，故无死锁。
static HUGE_INODE_ACCT: SpinLock<alloc::collections::BTreeMap<usize, HugeInodeAcct>> =
    SpinLock::new(alloc::collections::BTreeMap::new());

/// MAP_SHARED hugetlbfs：为 inode `ino` 的 `[start_idx, end_idx)` 区间按需建立预留，
/// 只对尚未预留/实例化的页扣预留。配额不足返回 false（mmap → ENOMEM），不做部分
/// 预留。对同一区间重复 mmap 幂等（已预留/已实例化的页不再扣，hugemmap23）。
pub fn hugetlb_shared_reserve_map(ino: usize, start_idx: usize, end_idx: usize) -> bool {
    if start_idx >= end_idx {
        return true;
    }
    let mut acct = HUGE_INODE_ACCT.lock();
    let entry = acct.entry(ino).or_default();
    let mut fresh = alloc::vec::Vec::new();
    for idx in start_idx..end_idx {
        if !entry.resv.contains(&idx) && !entry.inst.contains(&idx) {
            fresh.push(idx);
        }
    }
    if fresh.is_empty() {
        return true;
    }
    {
        let mut book = HUGETLB_BOOK.lock();
        if !book_reserve(&mut book, fresh.len()) {
            return false;
        }
    }
    for idx in fresh {
        entry.resv.insert(idx);
    }
    true
}

/// MAP_SHARED hugetlbfs 缺页（cache miss）：实例化 inode `ino` 的第 `idx` 个大页。
/// `reserved` 表示该映射是否带预留（非 MAP_NORESERVE）。返回：
///   * `Some(false)`：已实例化（不重复 commit），或预留映射的提交；
///   * `Some(true)`：未预留映射用了 overcommit surplus 页；
///   * `None`：未预留映射且池+配额耗尽 → SIGBUS（绝不挪用他人预留）。
pub fn hugetlb_shared_commit(ino: usize, idx: usize, reserved: bool) -> Option<bool> {
    let mut acct = HUGE_INODE_ACCT.lock();
    let entry = acct.entry(ino).or_default();
    if entry.inst.contains(&idx) {
        return Some(false);
    }
    if reserved {
        entry.resv.remove(&idx);
        entry.inst.insert(idx);
        hugetlb_commit_reserved();
        Some(false)
    } else {
        match hugetlb_commit_unreserved() {
            Some(surplus) => {
                entry.inst.insert(idx);
                Some(surplus)
            }
            None => None,
        }
    }
}

/// 回滚一次 `hugetlb_shared_commit`（物理帧分配失败的极少数场景）：把页从 `inst`
/// 退回 `resv`（预留映射）或丢弃（未预留），并按 `was_surplus` 还原池计数。
pub fn hugetlb_shared_commit_undo(ino: usize, idx: usize, reserved: bool, was_surplus: bool) {
    let mut acct = HUGE_INODE_ACCT.lock();
    if let Some(entry) = acct.get_mut(&ino) {
        if entry.inst.remove(&idx) && reserved {
            entry.resv.insert(idx);
        }
    }
    hugetlb_commit_undo(reserved, was_surplus);
}

/// 释放 inode `ino` 在 `[start_idx, end_idx)` 区间内自有的全部 MAP_SHARED 大页
/// （ftruncate 截断 / 文件被删）：已实例化页按 touched 归账（surplus 优先退 buddy，
/// 否则退 free），未触碰的预留页退还 rsvd。调用方须同步驱逐共享 huge cache。
pub fn hugetlb_shared_release_range(ino: usize, start_idx: usize, end_idx: usize) {
    if start_idx >= end_idx {
        return;
    }
    let (inst_n, resv_n) = {
        let mut acct = HUGE_INODE_ACCT.lock();
        let Some(entry) = acct.get_mut(&ino) else {
            return;
        };
        let inst_victims: alloc::vec::Vec<usize> =
            entry.inst.range(start_idx..end_idx).copied().collect();
        let resv_victims: alloc::vec::Vec<usize> =
            entry.resv.range(start_idx..end_idx).copied().collect();
        for idx in &inst_victims {
            entry.inst.remove(idx);
        }
        for idx in &resv_victims {
            entry.resv.remove(idx);
        }
        if entry.inst.is_empty() && entry.resv.is_empty() {
            acct.remove(&ino);
        }
        (inst_victims.len(), resv_victims.len())
    };
    if inst_n + resv_n > 0 {
        hugetlb_unmap(inst_n + resv_n, inst_n, true);
    }
}

/// inode 销毁（最后一个 fd 关闭未链接文件 / 文件被删除）时，归还其自有的全部
/// MAP_SHARED 大页与预留。无记录时为廉价 no-op，可无条件挂到 ramfs inode Drop。
pub fn hugetlb_shared_drop(ino: usize) {
    let (inst_n, resv_n) = {
        let mut acct = HUGE_INODE_ACCT.lock();
        match acct.remove(&ino) {
            Some(entry) => (entry.inst.len(), entry.resv.len()),
            None => return,
        }
    };
    if inst_n + resv_n > 0 {
        hugetlb_unmap(inst_n + resv_n, inst_n, true);
    }
}

/// 获取内存信息（总内存，空闲内存）单位：字节
pub fn get_memory_info() -> (u64, u64) {
    let allocator = frame::FRAME_ALLOCATOR.lock();
    let total_pages = allocator.total();
    let allocated_pages = allocator.allocated();
    let free_pages = total_pages.saturating_sub(allocated_pages);
    (total_pages as u64 * 4096, free_pages as u64 * 4096)
}

/// Whether the *current* process's address space has a VMA covering `va`.
///
/// Used by the page-fault trap path to pick the correct fault `si_code`: a bad
/// access inside a mapped region is a protection violation (`SEGV_ACCERR`),
/// while one to an unmapped address is `SEGV_MAPERR` — matching Linux's
/// `__do_page_fault`. Must be called with no `PROCESS_TABLE`/mm lock held; it
/// takes `PROCESS_TABLE` then the mm lock (the canonical order used elsewhere).
pub fn current_user_vma_present(va: usize) -> bool {
    use crate::sched::process::PROCESS_TABLE;
    let table = PROCESS_TABLE.lock();
    let t = match table.as_ref() {
        Some(t) => t,
        None => return false,
    };
    let pid = match t.current {
        Some(p) => p,
        None => return false,
    };
    let as_pid = t
        .get(pid)
        .map(|p| if p.mm.is_some() { pid } else { p.tgid })
        .unwrap_or(pid);
    let mm = match t.get(as_pid).and_then(|p| p.mm.as_ref()).cloned() {
        Some(m) => m,
        None => return false,
    };
    let addr_space = mm.lock();
    addr_space.find_vma(VirtAddr(va)).is_some()
}

#[cfg(target_arch = "riscv64")]
use crate::arch::consts::MEMORY_END;
pub use crate::arch::consts::PAGE_SIZE;

/// RISC-V：内核恒等映射（用户页表 gigapage 区）的运行时终点。
///
/// boot 时由 `mm::init` 按 FDT 实测 RAM 末尾向上取整到 1 GiB 边界写入；
/// `-m 1G` 时保持 0xC000_0000（与历史行为逐位一致），`-m 8G` 时为
/// 0x2_8000_0000。所有「内核保留区」判定（mmap 合成占用段、fork 跳过段、
/// hint 重定位）一律读这里，禁止再硬编码 0xC000_0000。
static KERNEL_IDENT_END: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0xC000_0000);

/// 读取内核恒等映射终点（见 [`KERNEL_IDENT_END`]）。
#[inline]
pub fn kernel_ident_end() -> usize {
    KERNEL_IDENT_END.load(core::sync::atomic::Ordering::Relaxed)
}

/// 内核恒等映射区包含的 1 GiB gigapage 数（用户页表 root[2..2+n]）。
#[cfg(target_arch = "riscv64")]
#[inline]
pub fn kernel_ident_gigapages() -> usize {
    (kernel_ident_end() - 0x8000_0000) >> 30
}

// 内核结束地址（由链接脚本定义）
extern "C" {
    fn ekernel();
}

/// 初始化内存管理
pub fn init(dtb: usize) {
    // 首先初始化内核堆（使用静态内存）
    // 这样后续的 Vec 等数据结构才能使用
    heap::init_heap();
    crate::println!("[mm] Heap allocator initialized");
    
    // 初始化物理页帧分配器
    // ekernel 是链接地址：
    //   RISC-V64: 物理地址 (0x802x_xxxx)
    //   LoongArch64: DMW0 窗口地址 (0x9000_0000_002x_xxxx)
    // 需要转换为物理地址
    let ekernel_va = ekernel as usize;
    #[cfg(target_arch = "riscv64")]
    let start = ekernel_va; // RISC-V64: 恒等映射
    #[cfg(target_arch = "loongarch64")]
    let start = ekernel_va & 0x0000_FFFF_FFFF_FFFF; // 去掉 DMW0 前缀
    let detected_ram_end = crate::cmdline::detect_ram_end(dtb);

    #[cfg(target_arch = "riscv64")]
    {
        // FDT 探测失败时保守回退 1 GiB（历史行为），绝不按编译期上限
        // MEMORY_END(8G) 捏造不存在的内存。
        const FALLBACK_END: usize = 0xC000_0000;
        let end = match detected_ram_end {
            Some(e) => core::cmp::min(e, MEMORY_END),
            None => FALLBACK_END,
        };

        // 内核恒等映射终点 = RAM 末尾向上取整到 1 GiB（gigapage 粒度）。
        // 用户页表的 root[2..2+n] gigapage、mmap/fork 的内核保留区判定
        // 全部以此为准（kernel_ident_end）。
        let ident_end = (end + (1 << 30) - 1) & !((1 << 30) - 1);
        KERNEL_IDENT_END.store(ident_end, core::sync::atomic::Ordering::Relaxed);

        crate::println!(
            "[mm] Physical memory: {:#x} - {:#x} ({} KB), kernel ident map to {:#x} ({} gigapages)",
            start, end, (end - start) / 1024, ident_end, (ident_end - 0x8000_0000) >> 30
        );

        frame::init(start, end);
    }

    // QEMU LoongArch virt splits RAM into two discontiguous banks:
    //   low  [ekernel, 0x1000_0000)             -- kernel image + heap live here
    //   high [0x8000_0000, 0xb000_0000) (768MB) -- previously completely unused
    // Add BOTH banks so LA can use ~920MB instead of ~155MB.
    #[cfg(target_arch = "loongarch64")]
    {
        use crate::arch::consts::{RAM_HIGH_BASE, RAM_HIGH_END, RAM_HIGH_FALLBACK_END, RAM_LOW_END};
        frame::init(start, RAM_LOW_END);
        // detect_ram_end 取 FDT 全部 /memory 节点的最大 base+size（LA 是双 bank，
        // 高 bank 才是大头）。探测失败或只看见低 bank 时保守回退 -m 1G 布局
        // （0xb000_0000），绝不按编译期上限捏造内存。
        let high_end = match detected_ram_end {
            Some(e) if e > RAM_HIGH_BASE => core::cmp::min(e, RAM_HIGH_END),
            _ => RAM_HIGH_FALLBACK_END,
        };
        frame::add_region(RAM_HIGH_BASE, high_end);
        crate::println!(
            "[mm] Physical memory: low {:#x}-{:#x} + high {:#x}-{:#x}",
            start, RAM_LOW_END, RAM_HIGH_BASE, high_end
        );
    }

    // 真化（Slice 2.x D2）：之前写死 `num_cpus = 4` 与 cmdline/DTS 解出的
    // 真 CPU 数不一致，会导致：
    //   * percpu_cache 给出/收回的桶超出真活跃 CPU 范围 → 缓存利用率虚高、
    //     远端 CPU 桶永远不用；
    //   * tlb::init 申请的 CpuMask 容量与 SMP IPI bitmap 不匹配，
    //     `flush_tlb_all_cpus` 会漏 shootdown。
    // 改用 `config::NUM_CPUS`（boot 早期由 main.rs 从设备树或 -smp 解析
    // 后 store）。clamp 到 ≥1 防止极早期 (NUM_CPUS=0 默认) 把 bitmap 算空。
    let num_cpus = core::cmp::max(
        1,
        crate::config::NUM_CPUS.load(core::sync::atomic::Ordering::Relaxed),
    );
    percpu_cache::init(num_cpus);

    // 初始化 SLUB 分配器（在帧分配器和 percpu_cache 之后）
    slab::init();

    // 初始化 TLB 管理器
    tlb::init(num_cpus);

    // 启用 swap 子系统（zram 式 RAM-backed）。对标 Linux 发行版 init 启动时
    // `swapon -a`：系统一上电即有 swap 区可用，sysinfo/`/proc/meminfo` 报告
    // 真实 SwapTotal/SwapFree，匿名页可经 MADV_PAGEOUT / cgroup 回收换出换入。
    if swap::swap_on(swap::SWAP_NR_SLOTS) {
        crate::println!(
            "[mm] Swap enabled: {} MiB (zram-backed)",
            swap::SWAP_NR_SLOTS * PAGE_SIZE / (1024 * 1024)
        );
    } else {
        crate::println!("[mm] Swap init failed (continuing without swap)");
    }
}

/// 页对齐（向下）
pub const fn page_floor(addr: usize) -> usize {
    addr & !(PAGE_SIZE - 1)
}

/// 页对齐（向上）
pub const fn page_ceil(addr: usize) -> usize {
    (addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)
}

// ============ Per-CPU 缓存集成的分配接口 ============

/// 使用 Per-CPU 缓存分配小对象
///
/// 真化（Slice 2.x D2）：之前 miss 路径直接返 `None`，等价于本地缓存为空
/// 时分配完全失败（与 Linux slab/SLUB 在 fastpath miss 时落 slowpath 取
/// partial-slab 完全不符）。改为 fallback 到全局堆分配器
/// `crate::mm::heap` 真分配，与 Linux `kmem_cache_alloc_slowpath` 等价。
/// 注：我们没有 cpu-local refill batch（需要 slab 结构），所以 miss 时
/// 直接走全局堆是当前最贴近 Linux 的"slowpath 但功能完整"实现，不留
/// `None` 占位。
pub fn alloc_small(size: usize) -> Option<*mut u8> {
    let cpu_id = percpu_cache::current_cpu_id();
    let cache = percpu_cache::get_cache();

    // 1. fastpath：本地 per-cpu cache
    if let Some(ptr) = cache.alloc(cpu_id, size) {
        return Some(ptr);
    }

    // 2. slowpath：fall back 到全局堆（`#[global_allocator]` 注册的
    //    `heap::LockedHeap`，等价于 Linux `kmem_cache_alloc_slowpath`）。
    use core::alloc::Layout;
    let layout = Layout::from_size_align(size, core::mem::align_of::<usize>()).ok()?;
    // SAFETY: 走 alloc::alloc::alloc → 全局分配器；调用方必须用配对
    // `dealloc_small(ptr, size)` 释放，layout 必须匹配（we 用相同 align）。
    let ptr = unsafe { alloc::alloc::alloc(layout) };
    if ptr.is_null() { None } else { Some(ptr) }
}

/// 使用 Per-CPU 缓存释放小对象
///
/// 真化（Slice 2.x D2）：本地缓存已满路径之前返 `false` 等价于内存泄漏
/// （上层会把这视作"未释放"但实际上对象已与 caller 解耦）。
/// 改为 fallback 到全局堆 dealloc，对应 Linux SLUB `kfree_full_slab_path`。
pub fn dealloc_small(ptr: *mut u8, size: usize) -> bool {
    let cpu_id = percpu_cache::current_cpu_id();
    let cache = percpu_cache::get_cache();

    if cache.dealloc(cpu_id, ptr, size) {
        return true;
    }

    use core::alloc::Layout;
    let layout = match Layout::from_size_align(size, core::mem::align_of::<usize>()) {
        Ok(l) => l,
        Err(_) => return false,
    };
    unsafe { alloc::alloc::dealloc(ptr, layout); }
    true
}

/// 获取内存统计信息
pub fn memory_stats() -> MemoryStats {
    let cache_stats = percpu_cache::get_cache().total_stats();
    let tlb_stats = tlb::get_manager().stats();
    
    MemoryStats {
        cache_local_hits: cache_stats.local_hits.load(core::sync::atomic::Ordering::Relaxed),
        cache_local_misses: cache_stats.local_misses.load(core::sync::atomic::Ordering::Relaxed),
        cache_hit_rate: cache_stats.hit_rate(),
        tlb_shootdowns: tlb_stats.remote_shootdowns.load(core::sync::atomic::Ordering::Relaxed),
        tlb_ipis_sent: tlb_stats.ipi_sent.load(core::sync::atomic::Ordering::Relaxed),
    }
}

/// 内存统计信息
pub struct MemoryStats {
    /// Per-CPU 缓存本地命中次数
    pub cache_local_hits: u64,
    /// Per-CPU 缓存本地未命中次数
    pub cache_local_misses: u64,
    /// Per-CPU 缓存命中率
    pub cache_hit_rate: f64,
    /// TLB Shootdown 次数
    pub tlb_shootdowns: u64,
    /// TLB IPI 发送次数
    pub tlb_ipis_sent: u64,
}

/// 详细内存统计（供 ProcFS 使用）
#[derive(Default)]
pub struct DetailedMemoryStats {
    /// 总页数
    pub total_pages: usize,
    /// 空闲页数
    pub free_pages: usize,
    /// 已用页数
    pub used_pages: usize,
    /// 缓存页数
    pub cached_pages: usize,
    /// Slab 页数
    pub slab_pages: usize,
    /// 页表页数
    pub page_table_pages: usize,
}

/// 获取详细内存统计（供 ProcFS 使用）
pub fn get_memory_stats() -> DetailedMemoryStats {
    // 从物理页帧分配器获取统计
    let frame_stats = frame::get_stats();
    
    DetailedMemoryStats {
        total_pages: frame_stats.total,
        free_pages: frame_stats.free,
        used_pages: frame_stats.used,
        cached_pages: frame_stats.cached,
        slab_pages: frame_stats.slab,
        page_table_pages: frame_stats.page_tables,
    }
}

/// 内存管理子系统自测试
/// 
/// 测试所有内存管理功能，确保代码被实际使用
pub fn self_test() {
    crate::println!("[mm] Running self-test...");
    
    // 测试页对齐函数
    let addr = 0x1234;
    let floor = page_floor(addr);
    let ceil = page_ceil(addr);
    assert!(floor <= addr);
    assert!(ceil >= addr);
    assert!(floor % PAGE_SIZE == 0);
    assert!(ceil % PAGE_SIZE == 0);
    
    // 测试物理页帧分配
    if let Some(frame) = frame_alloc() {
        let ppn = frame.ppn;
        crate::println!("[mm] Allocated frame at {:?}", ppn);
        // frame 会在作用域结束时自动释放
    }
    
    // 测试多页分配 (order=2 表示 4 个连续页)
    if let Some(ppn) = frame::alloc_frames(2) {
        crate::println!("[mm] Allocated 4 contiguous frames at {:?}", ppn);
        frame::dealloc_frames(ppn, 2);
    }
    
    // 测试 Per-CPU 缓存
    if let Some(ptr) = alloc_small(64) {
        crate::println!("[mm] Per-CPU cache alloc: {:p}", ptr);
        dealloc_small(ptr, 64);
    }
    
    // 测试 Per-CPU 缓存详细功能
    percpu_cache::self_test();
    
    // 测试内存统计
    let stats = memory_stats();
    crate::println!("[mm] Cache hit rate: {:.2}%", stats.cache_hit_rate * 100.0);
    crate::println!("[mm] TLB shootdowns: {}", stats.tlb_shootdowns);
    
    // 测试详细统计
    let detailed = get_memory_stats();
    crate::println!("[mm] Total pages: {}, Free: {}, Used: {}", 
        detailed.total_pages, detailed.free_pages, detailed.used_pages);
    
    // 测试 TLB 管理
    tlb::self_test();
    
    // 测试页表操作
    let mut pt = PageTable::new();
    let vpn = VirtPageNum(0x1000);
    let ppn = PhysPageNum(0x2000);
    let flags = PTEFlags::V | PTEFlags::R | PTEFlags::W;
    pt.map(vpn, ppn, flags);
    if let Some(pte) = pt.translate(vpn) {
        crate::println!("[mm] Page table mapping verified: VPN {:?} -> PPN {:?}", vpn, pte);
    }
    pt.unmap(vpn);
    
    // 测试地址空间
    let mut addr_space = AddressSpace::new();
    let vma = Vma::new(
        VirtAddr(0x1000_0000),
        VirtAddr(0x1001_0000),
        VmaType::Data,
        VmaPermission { read: true, write: true, execute: false, user: true },
    );
    addr_space.add_vma(vma);
    crate::println!("[mm] Address space created");
    
    // 测试 flush_tlb
    flush_tlb();
    
    crate::println!("[mm] Self-test passed!");
}
