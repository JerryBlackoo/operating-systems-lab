//! 地址空间管理
//!
//! 实现虚拟内存区域（VMA）管理和写时复制（CoW）

#![allow(dead_code)]

use super::{VirtAddr, VirtPageNum, PAGE_SIZE};
use super::page_table::{PageTable, PageTableEntry, PTEFlags};
use super::frame::{frame_alloc, frame_alloc_order, frame_alloc_uninit, FrameTracker, SharedFrame};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::sync::{SpinLock, TicketSpinLock};

/// M9DBG swap 取证计数器（仅调试用，bootarg M9DBG=1 时由 memcg 打印）。
pub static DBG_SO_OK: AtomicUsize = AtomicUsize::new(0);
pub static DBG_SO_COW: AtomicUsize = AtomicUsize::new(0);
pub static DBG_SO_NOSLOT: AtomicUsize = AtomicUsize::new(0);
pub static DBG_SO_OTHER: AtomicUsize = AtomicUsize::new(0);
pub static DBG_SI: AtomicUsize = AtomicUsize::new(0);
pub static DBG_CAND: AtomicUsize = AtomicUsize::new(0);

/// 不变量计数：缺页时发现「`vma.frames` 已持有该页的帧、但页表里没有映射」
/// 的次数。非 0 说明存在会丢用户数据的窗口（修复后按重映射处理，不再重分配）。
pub static FRAMES_RESIDENT_UNMAPPED: AtomicUsize = AtomicUsize::new(0);

/// FRAMEPROBE 限量计数器（每类打印前 24 条，防止串口风暴）。
pub static FPDBG_CLONE: AtomicUsize = AtomicUsize::new(0);
pub static FPDBG_REL: AtomicUsize = AtomicUsize::new(0);
pub static FPDBG_COW: AtomicUsize = AtomicUsize::new(0);

pub fn dbg_swap_stats() -> (usize, usize, usize, usize, usize, usize) {
    (
        DBG_SO_OK.load(Ordering::Relaxed),
        DBG_SO_COW.load(Ordering::Relaxed),
        DBG_SO_NOSLOT.load(Ordering::Relaxed),
        DBG_SO_OTHER.load(Ordering::Relaxed),
        DBG_SI.load(Ordering::Relaxed),
        DBG_CAND.load(Ordering::Relaxed),
    )
}

/// Per-inode page cache for MAP_SHARED file-backed mappings. Keyed by
/// `(inode_id, page_index)` where `inode_id` is the **inode number**
/// (`Inode::ino()`) and `page_index` is the page-aligned file offset divided
/// by PAGE_SIZE.
///
/// 这里**绝不能**用 `Arc::as_ptr(inode)` 之类的对象地址作 key。Linux 的页缓存
/// key 是 `(struct address_space *, index)`，而 `address_space` 与 inode 同生命
/// 周期、并在 inode 回收时被 `truncate_inode_pages_final()` 清空，因此不存在
/// 地址复用命中旧页的可能。我们的 inode 是引用计数对象，堆地址会被回收复用：
/// 一旦某 inode 释放、新 inode 恰好落在同一地址，就会**命中前一个文件遗留的
/// 缓存页**，用户态随即读到另一个文件的内容（观测现象：rustc 从 `.rmeta` 读出
/// 的 prelude 项整体消失、堆结构里出现别处的指令字节/指针）。
///
/// 改用 inode 号后与私有文件映射走的 `super::page_cache`（同样以 ino 为 key）
/// 语义一致：同一文件的私有与共享路径不再落在两个不同的 key 空间。
static SHARED_PAGE_CACHE: TicketSpinLock<BTreeMap<(usize, usize), SharedFrame>> =
    TicketSpinLock::new(BTreeMap::new());

/// `ino == 0` 不是合法的文件身份（Linux 的有效 inode 号从 1 起；我们的若干
/// 伪文件/占位 inode 也返回 0）。**绝不能**让它进入缓存 key：那会把所有
/// `ino()==0` 的文件塌缩到同一个 `(0, page_index)` 上，互相读到彼此的页。
/// 这类映射直接退化为"每次分配私有帧、不参与共享"，语义上不共享总比读错文件安全。
#[inline]
fn shared_cache_key_valid(inode_id: usize) -> bool {
    inode_id != 0
}

/// Look up or allocate a page in the shared page cache for a MAP_SHARED
/// file-backed mapping. `inode_id` must be `Inode::ino()`.
pub fn shared_cache_get_or_insert(
    inode_id: usize,
    page_index: usize,
) -> Option<SharedFrame> {
    if !shared_cache_key_valid(inode_id) {
        // 无法可靠标识归属的文件：不入共享缓存，返回独立帧。
        return frame_alloc().map(SharedFrame::from_alloc);
    }
    let mut cache = SHARED_PAGE_CACHE.lock();
    if let Some(frame) = cache.get(&(inode_id, page_index)) {
        return Some(frame.clone());
    }
    let frame = frame_alloc()?;
    let sf = SharedFrame::from_alloc(frame);
    cache.insert((inode_id, page_index), sf.clone());
    Some(sf)
}

/// Look up-or-allocate **and populate** a MAP_SHARED page cache entry with the
/// 「锁内查 → 锁外读 → 锁内插」协议。
///
/// 竞态根因（2026-08-15 修复）：旧实现先 get_or_insert 出帧、在**锁外**用
/// `meta_ref() <= 2` 判断「我是首个用户」再填充。两个 hart 同时缺同一页时，
/// 第二个 get 的 clone 使 meta=3 → 判定「已填充」直接建 PTE——而第一个 hart
/// 可能仍在零化/读盘，用户随即把半页零读到内存（观测族：mmap 内容间歇错、
/// rustc 结构字段整页归零后近零地址解引用）。修复 = 先在锁外把本帧填充完整，
/// 再持锁插入；并发填充者若已插入同 key，丢弃本帧返回已发布帧。**填充期间
/// 绝不持 SHARED_PAGE_CACHE 锁**（file.read 会拿 ext4 fs 锁，而
/// punch_hole 等路径以 fs→shared-cache 反序拿锁，锁内填充会引入 ABBA）。
/// `populate` 返回 false = 读失败/短读：零尾帧不入缓存、不发布，随作用域归还。
pub fn shared_cache_get_or_insert_with<F>(
    inode_id: usize,
    page_index: usize,
    populate: F,
) -> Option<SharedFrame>
where
    F: FnOnce(&mut [u8]) -> bool,
{
    if !shared_cache_key_valid(inode_id) {
        // 无稳定身份的文件不参与共享缓存：返回独立帧，但仍必须走 populate
        // 填充内容（否则 MAP_SHARED 文件映射会拿到全零页——旧语义等价回归）。
        let frame = frame_alloc()?;
        let sf = SharedFrame::from_alloc(frame);
        let kva = sf.ppn.as_bytes_mut();
        for b in kva.iter_mut() {
            *b = 0;
        }
        if !populate(kva) {
            return None;
        }
        return Some(sf);
    }
    {
        let cache = SHARED_PAGE_CACHE.lock();
        if let Some(frame) = cache.get(&(inode_id, page_index)) {
            return Some(frame.clone());
        }
    }
    // 锁外填充：此时绝不持共享缓存锁（避免与 fs→shared-cache 顺序形成 ABBA）。
    let frame = frame_alloc()?;
    let sf = SharedFrame::from_alloc(frame);
    let kva = sf.ppn.as_bytes_mut();
    for b in kva.iter_mut() {
        *b = 0;
    }
    if !populate(kva) {
        return None;
    }
    // 锁内插入：并发填充者可能已插入同 key——用它的帧，丢弃本帧（重复读盘
    // 只在双 hart 同时首缺页时发生，正确性优先于那一次冗余 I/O）。
    let mut cache = SHARED_PAGE_CACHE.lock();
    if let Some(existing) = cache.get(&(inode_id, page_index)) {
        return Some(existing.clone());
    }
    cache.insert((inode_id, page_index), sf.clone());
    Some(sf)
}

/// Get an existing page from the shared cache (no allocation).
pub fn shared_cache_get(inode_id: usize, page_index: usize) -> Option<SharedFrame> {
    if !shared_cache_key_valid(inode_id) {
        return None;
    }
    let cache = SHARED_PAGE_CACHE.lock();
    cache.get(&(inode_id, page_index)).cloned()
}

/// Insert a frame into the shared page cache.
pub fn shared_cache_insert(inode_id: usize, page_index: usize, frame: SharedFrame) {
    if !shared_cache_key_valid(inode_id) {
        return;
    }
    let mut cache = SHARED_PAGE_CACHE.lock();
    cache.insert((inode_id, page_index), frame);
}

/// Evict the shared page-cache entries of inode `inode_id` for file page
/// indices `[start_idx, end_idx)`. Used by `msync(MS_INVALIDATE)` so the next
/// fault on the range re-reads the page from the backing file (a genuine major
/// fault) rather than silently reusing the still-cached frame as a near-zero
/// minor fault. Dropping the cache's `Arc` frees the physical page once no live
/// mapping still references it (a live mapping keeps its own `Arc` in
/// `Vma::frames`, so its PTE stays valid until it re-faults).
pub fn shared_cache_evict_range(inode_id: usize, start_idx: usize, end_idx: usize) {
    if start_idx >= end_idx {
        return;
    }
    let mut cache = SHARED_PAGE_CACHE.lock();
    let victims: Vec<(usize, usize)> = cache
        .range((inode_id, start_idx)..(inode_id, end_idx))
        .map(|(k, _)| *k)
        .collect();
    for k in victims {
        cache.remove(&k);
    }
}

/// 清空某 inode 的全部共享缓存页（对标 Linux `truncate_inode_pages_final()`：
/// inode 被回收前必须把它的页缓存全部丢掉，否则后续任何以同一身份命中的查找
/// 都会拿到陈旧数据）。
pub fn shared_cache_evict_ino(inode_id: usize) {
    shared_cache_evict_range(inode_id, 0, usize::MAX);
}

/// Per-inode shared huge-page cache for hugetlbfs file mappings. Keyed by
/// `(ino, huge_index)` where `ino` is the backing inode number and
/// `huge_index` is the huge-page-aligned file offset divided by the huge
/// page size. All MAP_SHARED mappings of the same hugetlbfs file share the
/// same physical huge frame (so two SHARED mappings see each other's writes,
/// hugemmap31), and a MAP_PRIVATE mapping copies from this cache on first
/// fault (COW from the shared page, hugemmap09/29). Entries persist across
/// munmap so the data survives unmap/remap while the file stays open
/// (hugemmap09). Each tracker owns one contiguous huge block.
static SHARED_HUGE_CACHE: TicketSpinLock<BTreeMap<(usize, usize), SharedFrame>> =
    TicketSpinLock::new(BTreeMap::new());

/// Look up or allocate a huge page in the per-inode shared huge cache.
/// `order` is log2(pages-per-huge). The allocator zero-fills new frames, so
/// the returned huge page is demand-zero on first allocation.
pub fn shared_huge_cache_get_or_insert(
    ino: usize,
    huge_index: usize,
    order: usize,
) -> Option<SharedFrame> {
    let mut cache = SHARED_HUGE_CACHE.lock();
    if let Some(frame) = cache.get(&(ino, huge_index)) {
        return Some(frame.clone());
    }
    let frame = match frame_alloc_order(order) {
        Some(f) => f,
        None => {
            // Under memory pressure, drop cache entries no longer referenced
            // by any live mapping (meta_ref == 1 means only the cache
            // holds it) and retry once.
            cache.retain(|_, f| f.meta_ref() > 1);
            frame_alloc_order(order)?
        }
    };
    let sf = SharedFrame::from_alloc(frame);
    cache.insert((ino, huge_index), sf.clone());
    Some(sf)
}

/// Get an existing huge page from the shared huge cache (no allocation).
pub fn shared_huge_cache_get(ino: usize, huge_index: usize) -> Option<SharedFrame> {
    let cache = SHARED_HUGE_CACHE.lock();
    cache.get(&(ino, huge_index)).cloned()
}

/// Evict cached shared huge pages of inode `ino` whose huge index is in
/// `[start_idx, end_idx)`. Called when a hugetlbfs file is truncated so the
/// cached frames are released in lockstep with the pool accounting in
/// `crate::mm::hugetlb_shared_release_range`. Dropping the cache's `Arc` frees
/// the physical huge page once no live mapping still references it.
pub fn shared_huge_cache_evict_range(ino: usize, start_idx: usize, end_idx: usize) {
    if start_idx >= end_idx {
        return;
    }
    let mut cache = SHARED_HUGE_CACHE.lock();
    let victims: Vec<(usize, usize)> = cache
        .range((ino, start_idx)..(ino, end_idx))
        .map(|(k, _)| *k)
        .collect();
    for k in victims {
        cache.remove(&k);
    }
}

/// Evict every cached shared huge page of inode `ino` (file deleted / inode
/// evicted), paired with `crate::mm::hugetlb_shared_drop`.
pub fn shared_huge_cache_evict_ino(ino: usize) {
    shared_huge_cache_evict_range(ino, 0, usize::MAX);
}

/// 虚拟内存区域类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmaType {
    /// 代码段
    Code,
    /// 数据段
    Data,
    /// BSS 段
    Bss,
    /// 堆
    Heap,
    /// 栈
    Stack,
    /// 内存映射文件
    Mmap,
    /// 共享内存
    Shared,
}

/// 虚拟内存区域权限
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmaPermission {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
    pub user: bool,
}

impl VmaPermission {
    pub fn new(read: bool, write: bool, execute: bool, user: bool) -> Self {
        Self { read, write, execute, user }
    }

    pub fn to_pte_flags(&self) -> PTEFlags {
        // 基本标志：Valid + Accessed + Dirty
        // RISC-V 规范要求：
        // - 如果 A 位为 0，任何访问都会触发页面错误
        // - 如果 D 位为 0，任何写操作都会触发页面错误
        // 为了避免不必要的页面错误，我们预先设置 A 和 D 位
        let mut flags = PTEFlags::V | PTEFlags::A | PTEFlags::D;
        if self.read { flags |= PTEFlags::R; }
        if self.write { flags |= PTEFlags::W; }
        if self.execute { flags |= PTEFlags::X; }
        if self.user { flags |= PTEFlags::U; }

        // A writable page must also be readable: RISC-V treats the W=1,R=0 leaf
        // encoding as *reserved* (any access page-faults forever), and on both
        // RISC-V and LoongArch Linux maps PROT_WRITE (write-only) to a readable
        // mapping. So fold read into write here (mprotect02 mprotect(PROT_WRITE)).
        if flags.contains(PTEFlags::W) {
            flags |= PTEFlags::R;
        }

        flags
    }
}

/// VMA 页帧表：两级 chunk 结构（512 页/块），提供 BTreeMap 兼容的最小 API。
///
/// 曾是按 vpn 排序的扁平 Vec：随机顺序缺页（demand paging 天然乱序）时
/// `Vec::insert` 中部插入每次 memmove 半表，minibuild pchist 实证其 memmove
/// 占内核 icount 第一热点（3.66e9 insns，96% 的全内核 memmove）。现改为
/// 按 chunk 基址（vpn>>9）排序的块表：块内槽位按 vpn 低 9 位直接寻址，
/// 插入/删除 O(1) 免搬移；块表本身条目极少（每 2MiB 虚址一条），增删便宜。
///
/// clone = 逐块 memcpy + 每帧 MetaSlot 引用 +1（fork 热路径）；drop 顺序减
/// 计数。条目类型为 [`SharedFrame`]：clone/drop 与 MetaSlot 引用计数严格
/// 配对（2026-08-14 P0 修复，见 frame.rs 模块注释）。
const FRAME_CHUNK_BITS: usize = 9;
const FRAME_CHUNK_SIZE: usize = 1 << FRAME_CHUNK_BITS;

type FrameSlot = Option<(VirtPageNum, SharedFrame)>;

#[derive(Clone)]
struct FrameChunk {
    /// vpn >> FRAME_CHUNK_BITS
    base: usize,
    /// 本块内已占用槽数
    used: usize,
    slots: alloc::boxed::Box<[FrameSlot; FRAME_CHUNK_SIZE]>,
}

impl FrameChunk {
    fn new(base: usize) -> Self {
        const NONE_SLOT: FrameSlot = None;
        Self {
            base,
            used: 0,
            slots: alloc::boxed::Box::new([NONE_SLOT; FRAME_CHUNK_SIZE]),
        }
    }
}

#[derive(Clone, Default)]
pub struct FrameMap {
    /// 按 base 升序
    chunks: Vec<FrameChunk>,
    len: usize,
}

impl FrameMap {
    pub const fn new() -> Self {
        Self { chunks: Vec::new(), len: 0 }
    }

    /// 由**已按 vpn 升序排序**的 Vec 构造。
    pub fn from_sorted_vec(entries: Vec<(VirtPageNum, SharedFrame)>) -> Self {
        debug_assert!(entries.windows(2).all(|w| w[0].0 < w[1].0));
        let mut m = Self::new();
        for (vpn, f) in entries {
            m.insert(vpn, f);
        }
        m
    }

    #[inline]
    fn chunk_pos(&self, cb: usize) -> Result<usize, usize> {
        self.chunks.binary_search_by(|c| c.base.cmp(&cb))
    }

    #[inline]
    fn slot_of(vpn: &VirtPageNum) -> (usize, usize) {
        (vpn.0 >> FRAME_CHUNK_BITS, vpn.0 & (FRAME_CHUNK_SIZE - 1))
    }

    pub fn get(&self, vpn: &VirtPageNum) -> Option<&SharedFrame> {
        let (cb, s) = Self::slot_of(vpn);
        let ci = self.chunk_pos(cb).ok()?;
        self.chunks[ci].slots[s].as_ref().map(|e| &e.1)
    }

    pub fn contains_key(&self, vpn: &VirtPageNum) -> bool {
        self.get(vpn).is_some()
    }

    pub fn insert(
        &mut self,
        vpn: VirtPageNum,
        frame: SharedFrame,
    ) -> Option<SharedFrame> {
        pte_site::record_frames_write(vpn.0, frame.ppn.0);
        let (cb, s) = Self::slot_of(&vpn);
        let ci = match self.chunk_pos(cb) {
            Ok(i) => i,
            Err(i) => {
                self.chunks.insert(i, FrameChunk::new(cb));
                i
            }
        };
        let c = &mut self.chunks[ci];
        let old = c.slots[s].replace((vpn, frame));
        match old {
            Some((_, f)) => Some(f),
            None => {
                c.used += 1;
                self.len += 1;
                None
            }
        }
    }

    pub fn remove(&mut self, vpn: &VirtPageNum) -> Option<SharedFrame> {
        let (cb, s) = Self::slot_of(vpn);
        let ci = self.chunk_pos(cb).ok()?;
        let c = &mut self.chunks[ci];
        let old = c.slots[s].take()?;
        c.used -= 1;
        self.len -= 1;
        if c.used == 0 {
            self.chunks.remove(ci);
        }
        Some(old.1)
    }

    /// 游标定位（兼容旧排序 Vec API）。本结构按 vpn 直接寻址，游标无实义，
    /// `at_key`/`insert_seq` 均以传入的 vpn 定位，返回值仅为满足旧调用形态。
    pub fn lower_bound(&self, _vpn: &VirtPageNum) -> usize {
        0
    }

    /// 游标处条目恰为 vpn 时返回其帧引用（按 vpn 直接寻址）。
    pub fn at_key(&self, _idx: usize, vpn: VirtPageNum) -> Option<&SharedFrame> {
        self.get(&vpn)
    }

    /// 升序游标插入/替换（按 vpn 直接寻址，O(1) 免 memmove）。
    pub fn insert_seq(
        &mut self,
        _idx: usize,
        vpn: VirtPageNum,
        frame: SharedFrame,
    ) -> usize {
        self.insert(vpn, frame);
        0
    }

    /// 一次性摘除半开区间 `[start, end)` 内的全部条目并按 vpn 升序返回。
    pub fn remove_range(
        &mut self,
        r: core::ops::Range<VirtPageNum>,
    ) -> Vec<(VirtPageNum, SharedFrame)> {
        if r.start.0 >= r.end.0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        let cb_lo = r.start.0 >> FRAME_CHUNK_BITS;
        let ci_lo = self.chunk_pos(cb_lo).unwrap_or_else(|i| i);
        for c in self.chunks[ci_lo..].iter_mut() {
            let chunk_first = c.base << FRAME_CHUNK_BITS;
            if chunk_first >= r.end.0 {
                break;
            }
            let s_lo = r.start.0.saturating_sub(chunk_first).min(FRAME_CHUNK_SIZE);
            let s_hi = (r.end.0 - chunk_first).min(FRAME_CHUNK_SIZE);
            for s in s_lo..s_hi {
                if let Some(e) = c.slots[s].take() {
                    c.used -= 1;
                    self.len -= 1;
                    out.push(e);
                }
            }
        }
        self.chunks.retain(|c| c.used != 0);
        out
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.chunks.clear();
        self.len = 0;
    }

    pub fn iter(&self) -> impl Iterator<Item = (&VirtPageNum, &SharedFrame)> {
        self.chunks
            .iter()
            .flat_map(|c| c.slots.iter())
            .filter_map(|s| s.as_ref().map(|e| (&e.0, &e.1)))
    }

    pub fn keys(&self) -> impl Iterator<Item = &VirtPageNum> {
        self.iter().map(|(k, _)| k)
    }

    /// 从 `start` 起连续页与帧指针逐帧相等的前缀长度（read(2) 别名映射
    /// 稳态重读热路径；按 vpn 直接寻址逐页比对，O(1)/页）。
    pub fn ptr_eq_prefix_ptrs(
        &self,
        start: VirtPageNum,
        ptrs: impl Iterator<Item = *const FrameTracker>,
    ) -> usize {
        let mut n = 0;
        for (k, p) in ptrs.enumerate() {
            match self.get(&VirtPageNum(start.0 + k)) {
                Some(ef) if ef.arc_ptr() == p => n += 1,
                _ => break,
            }
        }
        n
    }

    /// 半开区间 `[start, end)` 内的条目（与 BTreeMap::range(start..end) 等价）。
    pub fn range(
        &self,
        r: core::ops::Range<VirtPageNum>,
    ) -> impl Iterator<Item = (&VirtPageNum, &SharedFrame)> + '_ {
        let (lo, hi) = (r.start.0, r.end.0);
        let cb_lo = lo >> FRAME_CHUNK_BITS;
        let ci_lo = self.chunk_pos(cb_lo).unwrap_or_else(|i| i);
        self.chunks[ci_lo..]
            .iter()
            .take_while(move |c| (c.base << FRAME_CHUNK_BITS) < hi)
            .flat_map(|c| c.slots.iter())
            .filter_map(move |s| {
                s.as_ref()
                    .filter(|e| e.0 .0 >= lo && e.0 .0 < hi)
                    .map(|e| (&e.0, &e.1))
            })
    }
}


/// EXECDBG 用户页安装取证环：记录「哪条内核路径把哪个 ppn 装进了某个 vpn」。
/// 只有 `EXECDBG=1` 时写入，固定容量、无分配、无锁竞争之外的额外开销，
/// 供 fork 时发现 `frames`/PTE 失同步后回溯责任站点。
pub mod pte_site {
    use core::sync::atomic::{AtomicUsize, Ordering};

    pub const SITE_MAP_PAGE: usize = 1;
    pub const SITE_ZERO_PAGE: usize = 2;
    pub const SITE_COW_COPY: usize = 3;
    pub const SITE_COW_RESTORE: usize = 4;
    pub const SITE_ALIAS: usize = 5;
    pub const SITE_SWAP_IN: usize = 6;
    pub const SITE_DEMAND_FILE: usize = 7;
    pub const SITE_READAHEAD: usize = 8;
    pub const SITE_LAZY_AROUND: usize = 9;
    pub const SITE_PAGECACHE_COW: usize = 10;
    pub const SITE_SEGMENT_PERM: usize = 11;

    const CAP: usize = 16384;
    static ROOT: [AtomicUsize; CAP] = [const { AtomicUsize::new(0) }; CAP];
    static VPN: [AtomicUsize; CAP] = [const { AtomicUsize::new(0) }; CAP];
    static PPN: [AtomicUsize; CAP] = [const { AtomicUsize::new(0) }; CAP];
    static SITE: [AtomicUsize; CAP] = [const { AtomicUsize::new(0) }; CAP];
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    #[inline]
    pub fn record(root: usize, vpn: usize, ppn: usize, site: usize) {
        if !crate::cmdline::execdbg_enabled() && !crate::cmdline::ptesite_enabled() {
            return;
        }
        let i = NEXT.fetch_add(1, Ordering::Relaxed) % CAP;
        ROOT[i].store(root, Ordering::Relaxed);
        VPN[i].store(vpn, Ordering::Relaxed);
        PPN[i].store(ppn, Ordering::Relaxed);
        SITE[i].store(site, Ordering::Relaxed);
    }

    /// 回溯同一页表根下最近一次对 `vpn` 的安装记录（ppn, site）。
    pub fn last_for(root: usize, vpn: usize) -> Option<(usize, usize)> {
        let next = NEXT.load(Ordering::Relaxed);
        for back in 1..=CAP.min(next) {
            let i = (next - back) % CAP;
            if VPN[i].load(Ordering::Relaxed) == vpn && ROOT[i].load(Ordering::Relaxed) == root {
                return Some((PPN[i].load(Ordering::Relaxed), SITE[i].load(Ordering::Relaxed)));
            }
        }
        None
    }

    /// `frames` 写入站点偏移：与 PTE 安装站点共用环，靠 +100 区分命名空间。
    pub const FRAMES_BIAS: usize = 100;

    /// 泛化 frames 写入站点：由 `FrameMap` 内部统一记录，保证零遗漏。
    pub const SITE_FRAMES_GENERIC: usize = 99;

    /// `FrameMap` 内部统一记录：任何 vpn→frame 的写入都留痕（不带 root，
    /// 因 ppn 全局唯一，回溯时用 (vpn, ppn) 精确匹配）。
    #[inline]
    pub fn record_frames_write(vpn: usize, ppn: usize) {
        record(0, vpn, ppn, SITE_FRAMES_GENERIC + FRAMES_BIAS);
    }

    /// 回溯 (vpn, ppn) 对应的最近一条记录，返回 (序号, site)。序号越大越新，
    /// 用于判定「页表写入」与「frames 写入」谁最后生效。
    pub fn last_seq_by_ppn(vpn: usize, ppn: usize) -> Option<(usize, usize)> {
        let next = NEXT.load(Ordering::Relaxed);
        for back in 1..=CAP.min(next) {
            let i = (next - back) % CAP;
            if VPN[i].load(Ordering::Relaxed) == vpn && PPN[i].load(Ordering::Relaxed) == ppn {
                return Some((next - back, SITE[i].load(Ordering::Relaxed)));
            }
        }
        None
    }

    /// 回溯同一页表根下最近一次对 `vpn` 的 **frames 写入**记录（ppn, site）。
    pub fn last_frames_for(root: usize, vpn: usize) -> Option<(usize, usize)> {
        let next = NEXT.load(Ordering::Relaxed);
        for back in 1..=CAP.min(next) {
            let i = (next - back) % CAP;
            if VPN[i].load(Ordering::Relaxed) == vpn
                && ROOT[i].load(Ordering::Relaxed) == root
                && SITE[i].load(Ordering::Relaxed) >= FRAMES_BIAS
            {
                return Some((
                    PPN[i].load(Ordering::Relaxed),
                    SITE[i].load(Ordering::Relaxed) - FRAMES_BIAS,
                ));
            }
        }
        None
    }

    /// 回溯任意页表根下最近一次对 `vpn` 的安装记录（root, ppn, site）——
    /// 用于判断该 vpn 的最后一次安装是否发生在别的地址空间。
    pub fn last_any(vpn: usize) -> Option<(usize, usize, usize)> {
        let next = NEXT.load(Ordering::Relaxed);
        for back in 1..=CAP.min(next) {
            let i = (next - back) % CAP;
            if VPN[i].load(Ordering::Relaxed) == vpn {
                return Some((
                    ROOT[i].load(Ordering::Relaxed),
                    PPN[i].load(Ordering::Relaxed),
                    SITE[i].load(Ordering::Relaxed),
                ));
            }
        }
        None
    }
}

/// 虚拟内存区域
pub struct Vma {
    /// 起始虚拟地址
    pub start: VirtAddr,
    /// 结束虚拟地址
    pub end: VirtAddr,
    /// 区域类型
    pub vma_type: VmaType,
    /// 权限
    pub permission: VmaPermission,
    /// 是否为写时复制
    pub cow: bool,
    /// VM_LOCKED: pages pinned in RAM by mlock(2)/mlockall(2). Inherited by
    /// the child on fork (VMA is copied) and dropped when the address space
    /// is replaced at exec or torn down at exit. Consulted to account a
    /// process's locked bytes against RLIMIT_MEMLOCK.
    pub locked: bool,
    /// File-backed mapping (mmap of a regular file). False for anonymous
    /// mappings (MAP_ANONYMOUS, heap, stack). Inherited verbatim on fork and
    /// through VMA splits; consulted by madvise(2) to reject MADV_FREE /
    /// MADV_WIPEONFORK / MADV_REMOVE on non-private-anonymous regions.
    pub file_backed: bool,
    /// VM_MAYWRITE: whether this mapping is *allowed* to become writable via
    /// mprotect(2). True for anonymous and MAP_PRIVATE mappings (private COW is
    /// always permissible). False for a MAP_SHARED file mapping whose backing
    /// descriptor was opened read-only, so mprotect(PROT_WRITE) is EACCES
    /// (mprotect01 case 3). Inherited on fork and through VMA splits.
    pub may_write: bool,
    /// HugeTLB page size in bytes (0 = normal 4KiB mapping). Set by
    /// `mmap(MAP_HUGETLB)`; consulted by `munmap` to release the
    /// matching reservation in `crate::mm::hugetlb_release`.
    pub huge_page_size: usize,
    /// Whether this HugeTLB mapping holds a *reservation* in the global pool
    /// (`HugePages_Rsvd`) established at mmap time. True for MAP_SHARED (and a
    /// would-be private reservation); false for MAP_NORESERVE, whose pages are
    /// only instantiated — possibly as surplus — at fault time. Consulted by
    /// `munmap` to return the reservation correctly. Inherited on fork/splits.
    pub huge_reserved: bool,
    /// Backing inode for a file mapping (the object `mmap` read its initial
    /// contents from). Kept so a writable MAP_SHARED mapping can flush dirty
    /// pages back to the file on `msync(2)`/`munmap(2)` (msync01/02). `None`
    /// for anonymous mappings. Cloned (Arc) on fork and through VMA splits.
    pub backing: Option<Arc<dyn crate::fs::Inode>>,
    /// File path associated with this mapping, equivalent to Linux
    /// `vm_area_struct::vm_file` naming metadata. Shared by fork/split and used
    /// by procfs and fault diagnostics after the originating fd is closed.
    pub file_path: Option<Arc<str>>,
    /// File offset (bytes) corresponding to `start` for a file mapping. Used to
    /// place written-back pages. Adjusted by the split delta in
    /// `extract_segment` so each sub-VMA keeps the correct file offset.
    pub file_offset: usize,
    /// 映射的物理页帧
    frames: FrameMap,
    /// HugeTLB backing chunks. Keyed by huge-page-aligned base VPN. Each
    /// tracker owns one contiguous physical block (e.g. 512 pages for 2MiB).
    huge_frames: FrameMap,
    /// Shared backing store for an anonymous `MAP_SHARED` mapping (Linux's
    /// shmem object). `None` for everything else. When set, the mapping is
    /// demand-paged like any other VMA, but a fault resolves the frame through
    /// this map so that every process sharing the mapping (e.g. across fork)
    /// observes the same physical page even for pages first touched *after*
    /// fork. Cloned (Arc) on fork and through VMA splits.
    anon_shared: Option<Arc<SpinLock<BTreeMap<VirtPageNum, SharedFrame>>>>,
    /// `MADV_WIPEONFORK` (VM_WIPEONFORK): on the next fork the *child* must see
    /// a zero-filled copy of this range rather than inheriting the parent's
    /// pages. Only valid on private anonymous mappings. Persists in the child
    /// (so grand-children are wiped too) until cleared by `MADV_KEEPONFORK`.
    wipeonfork: bool,
    /// `MADV_DONTDUMP` (VM_DONTDUMP): exclude this range from core dumps. The
    /// PT_LOAD header is still emitted (so the address layout is preserved) but
    /// its contents are not written (`p_filesz = 0`). Cleared by `MADV_DODUMP`.
    /// Persists across fork and VMA splits. (LTP `madvise08`.)
    pub dontdump: bool,
    /// Protection key（pkey_mprotect 关联的 key，0 = 默认 key）。RV/LA 无
    /// 用户态 PKRU 寄存器，key 权限只能在 pkey_alloc 时设定，因此内核以
    /// “有效权限”软件仿真：PTE 权限 = prot 扣除 key 拒绝位，访问违规走普通
    /// 权限缺页 → SIGSEGV（对标 x86 PKU 可观测行为）。fork/split 继承。
    pub pkey: i32,
}

impl Vma {
    /// read(2) 零拷贝别名映射是否可用于本 VMA：只对 Heap/Mmap/Stack 匿名页
    /// 做（其帧一定在 vma.frames 里被追踪，替换后老帧 Arc drop 正常释放；
    /// Code/Data/Bss 的帧可能不入 frames）。
    #[inline]
    pub fn alias_map_eligible(&self) -> bool {
        matches!(self.vma_type, VmaType::Heap | VmaType::Mmap | VmaType::Stack)
            && self.anon_shared.is_none()
            && self.huge_page_size == 0
            && !self.file_backed
            && !self.locked
            && self.permission.read
            && self.permission.user
    }

    /// 创建新的 VMA
    pub fn new(
        start: VirtAddr,
        end: VirtAddr,
        vma_type: VmaType,
        permission: VmaPermission,
    ) -> Self {
        Self {
            start,
            end,
            vma_type,
            permission,
            cow: false,
            locked: false,
            file_backed: false,
            may_write: true,
            huge_page_size: 0,
            huge_reserved: false,
            backing: None,
            file_path: None,
            file_offset: 0,
            frames: FrameMap::new(),
            huge_frames: FrameMap::new(),
            anon_shared: None,
            wipeonfork: false,
            dontdump: false,
            pkey: 0,
        }
    }

    /// Mark this VMA as an anonymous `MAP_SHARED` mapping backed by a fresh
    /// (empty) shmem object, so its pages are demand-paged yet shared across
    /// fork. Called from `sys_mmap` right after the VMA is created.
    pub fn init_anon_shared(&mut self) {
        self.anon_shared = Some(Arc::new(SpinLock::new(BTreeMap::new())));
    }

    /// Flush dirty pages of a writable MAP_SHARED file mapping back to its
    /// backing inode for the `[range_start, range_end)` window (clamped to this
    /// VMA). A no-op for anonymous, read-only, or private mappings. Linux only
    /// writes back dirty pages; lacking per-page dirty tracking we write every
    /// present page, which is idempotent for ram-backed inodes.
    pub fn writeback(&self, range_start: VirtAddr, range_end: VirtAddr) {
        if !self.file_backed || self.vma_type != VmaType::Shared || !self.permission.write {
            return;
        }
        let inode = match &self.backing {
            Some(i) => i,
            None => return,
        };
        let file = match inode.as_file() {
            Some(f) => f,
            None => return,
        };
        let file_size = file.stat().map(|s| s.size).unwrap_or(0);
        let s = range_start.0.max(self.start.0) & !(PAGE_SIZE - 1);
        let e = range_end.0.min(self.end.0);
        let mut va = s;
        while va < e {
            let vpn = VirtPageNum(va >> 12);
            if let Some(frame) = self.frames.get(&vpn) {
                let foff = self.file_offset + (va - self.start.0);
                if foff < file_size {
                    let n = core::cmp::min(PAGE_SIZE, file_size - foff);
                    let bytes = &frame.ppn.as_bytes_mut()[..n];
                    let _ = file.write(foff, bytes);
                }
            }
            va += PAGE_SIZE;
        }
    }

    /// `msync(MS_INVALIDATE)`: drop the cached copy of a file mapping's pages
    /// and reload them from the backing inode, so modifications made to the
    /// file through another path (e.g. write(2) on the fd) become visible
    /// through the mapping (msync02). Without a unified page cache our mapping
    /// holds private frame copies, so we reload their contents here.
    pub fn invalidate(&self, range_start: VirtAddr, range_end: VirtAddr) {
        if !self.file_backed {
            return;
        }
        let inode = match &self.backing {
            Some(i) => i,
            None => return,
        };
        let file = match inode.as_file() {
            Some(f) => f,
            None => return,
        };
        let file_size = file.stat().map(|s| s.size).unwrap_or(0);
        let s = range_start.0.max(self.start.0) & !(PAGE_SIZE - 1);
        let e = range_end.0.min(self.end.0);
        let mut va = s;
        while va < e {
            let vpn = VirtPageNum(va >> 12);
            if let Some(frame) = self.frames.get(&vpn) {
                let foff = self.file_offset + (va - self.start.0);
                if foff < file_size {
                    let n = core::cmp::min(PAGE_SIZE, file_size - foff);
                    let dst = frame.ppn.as_bytes_mut();
                    let mut read_total = 0usize;
                    while read_total < n {
                        match file.read(foff + read_total, &mut dst[read_total..n]) {
                            Ok(0) => break,
                            Ok(r) => read_total += r,
                            Err(_) => break,
                        }
                    }
                }
            }
            va += PAGE_SIZE;
        }

        // MAP_SHARED file mapping: also drop the per-inode shared page-cache
        // entries covering this range. Linux's MS_INVALIDATE invalidates other
        // cached copies of the file so a subsequent access re-reads it; without
        // evicting here a remap+touch resolves against the still-cached frame as
        // a near-zero-cost minor fault, which collapses lat_pagefault's per-fault
        // timing (combined − mmap_only) into the mmap-loop noise band and yields
        // a negative result (0 score). Eviction forces the next fault to re-read
        // from the backing file (a genuine, deterministically-costed major
        // fault). A live mapping keeps its own Arc in `frames`, so this only
        // affects the *next* fault, not pages already resident.
        if self.vma_type == VmaType::Shared && e > s {
            let inode_id = inode.ino();
            let start_idx = (self.file_offset + (s - self.start.0)) / PAGE_SIZE;
            let end_idx =
                (self.file_offset + (e - self.start.0) + PAGE_SIZE - 1) / PAGE_SIZE;
            shared_cache_evict_range(inode_id, start_idx, end_idx);
        }

        // The demand-paging fault path (`try_resolve_fault` →
        // `NeedFileRead`) resolves file pages through the global page cache
        // keyed by `inode.ino()`, not the per-inode-pointer shared cache
        // above. Drop those entries too, so the invalidation actually reaches
        // the cache the next fault will consult; otherwise a remapped range
        // keeps resolving against stale cached frames and never re-reads the
        // backing file.
        if e > s {
            let ino = inode.ino();
            let start_idx = (self.file_offset + (s - self.start.0)) / PAGE_SIZE;
            let end_idx =
                (self.file_offset + (e - self.start.0) + PAGE_SIZE - 1) / PAGE_SIZE;
            for idx in start_idx..end_idx {
                super::page_cache::remove(ino, idx);
            }
        }
    }

    /// True when `va` falls in this file-backed mapping but on a page that lies
    /// entirely past the file's last page (offset >= round_up(file size)).
    /// Touching such a page must raise SIGBUS rather than instantiate an
    /// anonymous zero page (mmap13).
    pub fn is_file_page_beyond_eof(&self, va: VirtAddr) -> bool {
        if !self.file_backed {
            return false;
        }
        let inode = match &self.backing {
            Some(i) => i,
            None => return false,
        };
        let file = match inode.as_file() {
            Some(f) => f,
            None => return false,
        };
        let fsz = file.stat().map(|s| s.size).unwrap_or(0);
        let eof_page = (self.file_offset + fsz + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let foff = self.file_offset + (va.0 - self.start.0);
        foff >= eof_page
    }

    /// 标记为 HugeTLB VMA，记录每个大页字节数（典型为 2MiB / 1GiB）。
    pub fn mark_huge(&mut self, huge_page_size: usize) {
        self.huge_page_size = huge_page_size;
    }

    /// 本 VMA 当前驻留的物理页数（普通页帧数 + huge 块折算的 4KiB 页数）。
    /// 直接数 frames 集合，避免 RSS 统计逐页做三级页表遍历。
    pub fn resident_pages(&self) -> usize {
        let huge_pages: usize = if self.huge_page_size > 0 {
            self.huge_frames.len() * (self.huge_page_size / PAGE_SIZE)
        } else {
            self.huge_frames.len() * (0x20_0000 / PAGE_SIZE)
        };
        self.frames.len() + huge_pages
    }

    pub fn vpn_range(&self) -> core::ops::Range<usize> {
        self.start.floor().0..self.end.ceil().0
    }

    /// Insert an already-allocated frame into this VMA's frame map (used by
    /// the shared page cache path in sys_mmap for MAP_SHARED file mappings).
    pub fn insert_shared_frame(&mut self, vpn: VirtPageNum, frame: SharedFrame) {
        self.frames.insert(vpn, frame);
    }

    /// 分配并映射一个页
    pub fn map_page(&mut self, vpn: VirtPageNum, page_table: &mut PageTable) -> bool {
        // 检查页面是否已经映射
        if page_table.translate(vpn).is_some() {
            // 页面已经映射，跳过
            return true;
        }

        if self.huge_page_size != 0 {
            return self.map_huge_page_containing(vpn, page_table);
        }

        // 不变量：`frames` 里已有该页的帧 ⟹ 页内容有效，缺页只能**重新映射它**，
        // 绝不能新分配清零帧并覆盖 `frames`。否则任何「先 unmap PTE 但保留
        // frames」的路径（PROT_NONE 的 mprotect、回收扫描、区间部分 unmap）
        // 之后的首次访问都会把用户已写入的数据整页清零——观测现象正是 rustc
        // 的堆对象整页为 0、只剩最早写的几个字段，随后从中读出的"指针"是别处
        // 内容或垃圾，解引用即 SIGSEGV。
        //
        // Linux 的等价不变量：一个 VMA 内的页要么有页表项、要么在 swap 里、
        // 要么根本不存在；绝不会出现"内容仍在但被当作缺失从而重新分配"。
        if let Some(existing) = self.frames.get(&vpn) {
            // 不变量违规告警：到这里说明确实存在「PTE 已消失但 frames 仍持有
            // 内容」的窗口。修复后语义正确（重映射而非重分配），但仍要曝光
            // 是哪个区间在制造这个窗口，便于收口源头路径。
            if crate::cmdline::execdbg_enabled() {
                let n = FRAMES_RESIDENT_UNMAPPED.fetch_add(1, Ordering::Relaxed);
                if n < 24 {
                    crate::println!(
                        "[EXECDBG] frames-resident-unmapped #{} vpn={:#x} ppn={:#x} vma=[{:#x},{:#x}) type={:?} refs={} frame_ref={}",
                        n, vpn.0, existing.ppn.0, self.start.0, self.end.0, self.vma_type,
                        existing.strong(), existing.meta_ref()
                    );
                }
            }
            let ppn = existing.ppn;
            let mut flags = self.permission.to_pte_flags();
            // 私有映射且帧仍被共享：只能以只读重映射，写路径必须再经
            // handle_cow 复制断开（判据同 handle_cow，以帧引用计数为准）。
            let private = self.vma_type != VmaType::Shared && self.anon_shared.is_none();
            if private && (existing.strong() > 1 || existing.meta_ref() > 1) {
                flags.remove(PTEFlags::W);
                flags.remove(PTEFlags::D);
                self.cow = true;
            }
            if !page_table.try_map(vpn, ppn, flags) {
                return false;
            }
            pte_site::record(page_table.root_ppn().0, vpn.0, ppn.0, pte_site::SITE_MAP_PAGE);
            return true;
        }

        // Anonymous MAP_SHARED: resolve the frame through the shmem object so
        // every process sharing this mapping maps the *same* physical page.
        // A page first touched by any sharer is allocated once and recorded in
        // the shared map; subsequent faults (here or in another process) reuse
        // it. New pages are zero-filled, like all anonymous memory.
        if let Some(shared) = self.anon_shared.clone() {
            let flags = self.permission.to_pte_flags();
            let mut map = shared.lock();
            if let Some(existing) = map.get(&vpn) {
                let ppn = existing.ppn;
                if !page_table.try_map(vpn, ppn, flags) {
                    return false;
                }
                self.frames.insert(vpn, existing.clone());
                return true;
            }
            let Some(frame) = frame_alloc() else {
                return false;
            };
            // frame_alloc 已用 zero_frame 清零整页（见 mm::frame），此处无需再清。
            let ppn = frame.ppn;
            if !page_table.try_map(vpn, ppn, flags) {
                return false;
            }
            let frame = SharedFrame::from_alloc(frame);
            map.insert(vpn, frame.clone());
            self.frames.insert(vpn, frame);
            return true;
        }

        // File-backed MAP_SHARED: resolve the frame through the per-inode shared
        // page cache so every process mapping the same file region sees the same
        // physical frame (Linux page-cache coherence). Demand-paged on first
        // touch instead of eagerly at mmap, so a fresh remap genuinely faults
        // (lat_pagefault measures this) and msync/munmap only write back the
        // pages actually touched.
        if self.file_backed && self.vma_type == VmaType::Shared {
            let Some(ref inode) = self.backing else {
                return false;
            };
            let inode_id = inode.ino();
            let page_va = vpn.0 * PAGE_SIZE;
            let vma_offset = page_va.saturating_sub(self.start.0);
            let file_off = self.file_offset + vma_offset;
            let page_index = file_off / PAGE_SIZE;
            // 填充必须在共享缓存锁内完成（见 shared_cache_get_or_insert_with 注释）。
            let Some(frame_arc) = shared_cache_get_or_insert_with(inode_id, page_index, |kva| {
                let Some(file) = inode.as_file() else {
                    return false;
                };
                let Ok(st) = file.stat() else {
                    return false;
                };
                if file_off < st.size {
                    let avail = core::cmp::min(PAGE_SIZE, st.size - file_off);
                    let mut total = 0usize;
                    while total < avail {
                        match file.read(file_off + total, &mut kva[total..avail]) {
                            Ok(0) => break,
                            Ok(n) => total += n,
                            Err(_) => break,
                        }
                    }
                    // 短读/读失败：零尾页不能进入共享缓存或 PTE。
                    total == avail
                } else {
                    true // 末页尾部的零填充部分，无需读盘
                }
            }) else {
                return false;
            };
            let ppn = frame_arc.ppn;
            let flags = self.permission.to_pte_flags();
            if !page_table.try_map(vpn, ppn, flags) {
                return false;
            }
            self.frames.insert(vpn, frame_arc);
            return true;
        }

        let Some(frame) = frame_alloc() else {
            return false;
        };
        let ppn = frame.ppn;

        // File-backed demand paging: populate the page from the backing file.
        // This is the lazy counterpart to the eager pre-read in sys_mmap —
        // MAP_PRIVATE file mappings no longer pre-allocate all pages; instead
        // each page is read from the file on first access (page fault).
        if self.file_backed {
            // frame_alloc 已清零整页；只需把文件内容覆盖到 [0,avail)，尾部留零即可。
            let kva = ppn.as_bytes_mut();
            if let Some(ref inode) = self.backing {
                let page_va = vpn.0 * PAGE_SIZE;
                let vma_offset = page_va.saturating_sub(self.start.0);
                let file_off = self.file_offset + vma_offset;
                // Private file mapping: route the clean read through the
                // global page cache (like the trap-handler fault path) so
                // sibling sharers — and mincore — observe the page as
                // resident even when they have not faulted their own copy
                // (mincore04: a forked child mlock's the range). Then copy
                // the clean cache page into this private frame.
                let inode_id = inode.ino();
                let page_index = file_off / PAGE_SIZE;
                let inode_for_read = inode.clone();
                // ino==0 无法唯一标识文件，绝不能作为页缓存 key（会让所有这类
                // 文件塌缩到同一 key 上互相读到彼此的页）；此时直接从文件读入
                // 本帧，不参与页缓存共享。
                let cached = if inode_id == 0 {
                    None
                } else {
                super::page_cache::get_or_populate(inode_id, page_index, || {
                    let f = frame_alloc()?;
                    // frame_alloc 已清零整页，无需再逐字节清零。
                    let ckva = f.ppn.as_bytes_mut();
                    if let Some(file) = inode_for_read.as_file() {
                        let st = file.stat().ok()?;
                        if file_off < st.size {
                            let avail = core::cmp::min(PAGE_SIZE, st.size - file_off);
                            let mut total = 0usize;
                            while total < avail {
                                match file.read(file_off + total, &mut ckva[total..avail]) {
                                    Ok(0) => break,
                                    Ok(n) => total += n,
                                    Err(_) => break,
                                }
                            }
                            // 短读/读失败：不把零尾页插入页缓存（否则所有后续
                            // 私有映射都会复制到一份被截断的"文件内容"）。
                            if total < avail {
                                return None;
                            }
                        }
                    }
                    Some(SharedFrame::from_alloc(f))
                })
                };
                if let Some(cached) = cached {
                    kva.copy_from_slice(cached.ppn.as_bytes_mut());
                } else {
                    // 未走页缓存（ino 不可用或缓存填充失败）：直接把文件内容
                    // 读进本帧。frame_alloc 已清零；读不满必须整体失败，
                    // 绝不把零尾页当文件内容映射给用户。
                    let Some(file) = inode.as_file() else {
                        return false;
                    };
                    let Ok(st) = file.stat() else {
                        return false;
                    };
                    if file_off < st.size {
                        let avail = core::cmp::min(PAGE_SIZE, st.size - file_off);
                        let mut total = 0usize;
                        while total < avail {
                            match file.read(file_off + total, &mut kva[total..avail]) {
                                Ok(0) => break,
                                Ok(n) => total += n,
                                Err(_) => break,
                            }
                        }
                        if total < avail {
                            return false;
                        }
                    }
                }
            }
        }

        let flags = self.permission.to_pte_flags();

        // 用可失败版映射：若中间级页表也 OOM，则放弃本次映射（frame 在此处
        // 离开作用域时自动释放物理页），返回 false，由 trap 层终止当前进程而非
        // halt 内核。
        if !page_table.try_map(vpn, ppn, flags) {
            return false;
        }
        pte_site::record(page_table.root_ppn().0, vpn.0, ppn.0, pte_site::SITE_MAP_PAGE);
        self.frames.insert(vpn, SharedFrame::from_alloc(frame));
        pte_site::record(
            page_table.root_ppn().0,
            vpn.0,
            ppn.0,
            pte_site::SITE_MAP_PAGE + pte_site::FRAMES_BIAS,
        );
        true
    }

    /// 私有匿名映射的**读**缺页映射全局共享零页（Linux `ZERO_PAGE` 语义）：
    /// 不分配真实帧，PTE 去 W/D 只读映射；写入时经 CoW 复制断开（零页由
    /// 静态持有一份 Arc，引用计数恒 >1，`handle_cow` 必走复制分支）。
    /// 零页 OOM 时回退真实分配。
    pub fn map_zero_page(&mut self, vpn: VirtPageNum, page_table: &mut PageTable) -> bool {
        if page_table.translate(vpn).is_some() {
            return true;
        }
        let Some(zp) = crate::mm::frame::zero_page_frame() else {
            return self.map_page(vpn, page_table);
        };
        let mut flags = self.permission.to_pte_flags();
        flags.remove(PTEFlags::W);
        flags.remove(PTEFlags::D);
        if !page_table.try_map(vpn, zp.ppn, flags) {
            return false;
        }
        let zp_ppn = zp.ppn.0;
        pte_site::record(page_table.root_ppn().0, vpn.0, zp_ppn, pte_site::SITE_ZERO_PAGE);
        self.frames.insert(vpn, zp);
        pte_site::record(
            page_table.root_ppn().0,
            vpn.0,
            zp_ppn,
            pte_site::SITE_ZERO_PAGE + pte_site::FRAMES_BIAS,
        );
        self.cow = true;
        true
    }

    fn map_huge_page_containing(&mut self, vpn: VirtPageNum, page_table: &mut PageTable) -> bool {
        let pages_per_huge = self.huge_page_size / PAGE_SIZE;
        if pages_per_huge == 0 || !pages_per_huge.is_power_of_two() {
            return false;
        }
        let base_vpn = VirtPageNum((vpn.0 / pages_per_huge) * pages_per_huge);
        if self.huge_frames.contains_key(&base_vpn) {
            return page_table.translate(vpn).is_some();
        }
        let order = pages_per_huge.trailing_zeros() as usize;

        // Fault-time commit against the global hugetlb pool (Linux
        // `hugetlb_fault` / `alloc_huge_page`). hugetlbfs file mappings
        // (self.backing set) route through the per-inode shared huge cache so
        // MAP_SHARED mappings of the same file share one physical huge page and
        // MAP_PRIVATE copies-on-fault from it; anonymous MAP_HUGETLB (backing ==
        // None) stays demand-zero.
        //
        // MAP_SHARED hugetlbfs is *inode-owned* (Linux resv_map): the pool is
        // committed only on a cache *miss* (first instantiation of this file
        // page). A cache hit means the page is already resident — re-mapping /
        // re-faulting the same file region must NOT touch the pool again, or
        // Free/Rsvd get over-decremented (hugemmap08/23). Everything else keeps
        // the VMA-driven commit/undo accounting: a reserved mapping consumes its
        // reservation (free--, rsvd--); a non-reserved one instantiates a free
        // or overcommit-surplus page and SIGBUSes when the pool + quota are spent.
        let frame: SharedFrame = if let Some(ref inode) = self.backing {
            let ino = inode.ino();
            let base_va = base_vpn.0 * PAGE_SIZE;
            let file_off = self.file_offset + base_va.saturating_sub(self.start.0);
            let huge_index = file_off / self.huge_page_size;
            if self.vma_type == VmaType::Shared {
                if let Some(f) = shared_huge_cache_get(ino, huge_index) {
                    // Already resident — the inode owns it; no pool commit.
                    f
                } else {
                    let surplus = match crate::mm::hugetlb_shared_commit(
                        ino,
                        huge_index,
                        self.huge_reserved,
                    ) {
                        Some(s) => s,
                        None => return false, // SIGBUS: pool + overcommit exhausted
                    };
                    match shared_huge_cache_get_or_insert(ino, huge_index, order) {
                        Some(f) => f,
                        None => {
                            crate::mm::hugetlb_shared_commit_undo(
                                ino,
                                huge_index,
                                self.huge_reserved,
                                surplus,
                            );
                            return false;
                        }
                    }
                }
            } else {
                // MAP_PRIVATE file: VMA-driven commit; allocate a private huge
                // frame and copy the shared cache contents into it if the page
                // already exists (Linux COW from the page cache), else demand-zero.
                let (committed_reserved, committed_surplus) = if self.huge_reserved {
                    crate::mm::hugetlb_commit_reserved();
                    (true, false)
                } else {
                    match crate::mm::hugetlb_commit_unreserved() {
                        Some(surplus) => (false, surplus),
                        None => return false,
                    }
                };
                let Some(priv_frame) = frame_alloc_order(order) else {
                    crate::mm::hugetlb_commit_undo(committed_reserved, committed_surplus);
                    return false;
                };
                if let Some(src) = shared_huge_cache_get(ino, huge_index) {
                    for i in 0..pages_per_huge {
                        let s = super::PhysPageNum(src.ppn.0 + i).as_bytes_mut();
                        let d = super::PhysPageNum(priv_frame.ppn.0 + i).as_bytes_mut();
                        d.copy_from_slice(s);
                    }
                }
                SharedFrame::from_alloc(priv_frame)
            }
        } else {
            // Anonymous MAP_HUGETLB: VMA-driven commit, demand-zero frame.
            let (committed_reserved, committed_surplus) = if self.huge_reserved {
                crate::mm::hugetlb_commit_reserved();
                (true, false)
            } else {
                match crate::mm::hugetlb_commit_unreserved() {
                    Some(surplus) => (false, surplus),
                    None => return false,
                }
            };
            let Some(f) = frame_alloc_order(order) else {
                crate::mm::hugetlb_commit_undo(committed_reserved, committed_surplus);
                return false;
            };
            SharedFrame::from_alloc(f)
        };
        let base_ppn = frame.ppn;
        let flags = self.permission.to_pte_flags();
        for i in 0..pages_per_huge {
            let sub_vpn = VirtPageNum(base_vpn.0 + i);
            if sub_vpn.0 < self.start.floor().0 || sub_vpn.0 >= self.end.ceil().0 {
                continue;
            }
            if page_table.translate(sub_vpn).is_none() {
                page_table.map(sub_vpn, super::PhysPageNum(base_ppn.0 + i), flags);
            }
        }
        self.huge_frames.insert(base_vpn, frame);
        page_table.translate(vpn).is_some()
    }

    /// 取消映射一个页（若未实际映射则跳过，适配按需分页场景）
    pub fn unmap_page(&mut self, vpn: VirtPageNum, page_table: &mut PageTable) {
        if page_table.translate(vpn).is_some() {
            page_table.unmap(vpn);
        }
        self.frames.remove(&vpn);
        if self.huge_page_size != 0 {
            let pages_per_huge = self.huge_page_size / PAGE_SIZE;
            if pages_per_huge != 0 {
                let base_vpn = VirtPageNum((vpn.0 / pages_per_huge) * pages_per_huge);
                self.huge_frames.remove(&base_vpn);
            }
        }
    }

    /// 处理写时复制
    ///
    /// 当进程尝试写入一个 CoW 页面时调用此函数。
    /// 如果页面被多个进程共享（引用计数 > 1），则复制页面。
    /// 如果页面只被当前进程使用（引用计数 = 1），则直接恢复写权限。
    ///
    /// 返回 `Ok(Some(old))`：发生复制，PTE 已指向新帧、旧帧已从 frames 摘除；
    /// **调用方必须先把旧帧隔离（retired_frames / 传回 trap 层），在完成跨核
    /// TLB 失效（drain_shootdown）后才能 drop**——否则其它 hart 可经陈旧 TLB
    /// 读写已被 buddy 复用的旧帧（BuildStorm rustc SIGSEGV 根因家族）。
    /// 返回 `Ok(None)`：就地恢复写权限或懒补页，无帧被摘除。
    pub fn handle_cow(&mut self, vpn: VirtPageNum, page_table: &mut PageTable) -> Result<Option<SharedFrame>, ()> {
        if !self.cow {
            return Err(());
        }

        if let Some(frame) = self.frames.get(&vpn) {
            // 共享判据必须是**物理帧的真实引用计数**（MetaSlot），不能只看
            // `Arc<FrameTracker>` 的克隆数。fork 的稀疏快路径会给父子各自
            // 重建独立的 `Arc<FrameTracker>`（共享事实只记在 MetaSlot 里），
            // 此时父侧 `Arc::strong_count == 1`；若据此判定“独占”并就地恢复
            // 写权限，父就会**原地写一个仍被子进程共享的物理帧**，CoW 语义
            // 彻底失效（表现为对方地址空间里的结构被写花、解出指向 .text 的
            // 假指针后 SIGSEGV）。Linux `do_wp_page` 同样以页的引用/映射计数
            // （`page_count`/`PageAnonExclusive`）为准，而非任何内核侧智能指针。
            // 两个计数取保守并集：任一 > 1 都必须复制。（SharedFrame 修复后
            // MetaSlot 即为全部持有者数，strong 为同一 Arc 实例的克隆数，
            // 二者一致；保留并集是防御性保守。）
            let ref_count = core::cmp::max(
                frame.strong(),
                frame.meta_ref() as usize,
            );

            if ref_count > 1 {
                // 页面被多个进程共享，需要复制。
                // OOM 时不 panic、不 halt 内核：返回 Err，由 trap 层按「缺页处理失败」
                // 终止触发本次写时复制的当前进程并回收其内存，对标 Linux OOM 杀进程语义，
                // 让判题继续往下跑。
                let Some(new_frame) = frame_alloc_uninit() else {
                    return Err(());
                };
                let old_ppn = frame.ppn;
                let new_ppn = new_frame.ppn;

                // 复制数据
                let src = old_ppn.as_bytes_mut();
                let dst = new_ppn.as_bytes_mut();
                dst.copy_from_slice(src);

                // 使用 remap 更新映射（恢复写权限）
                page_table.remap(vpn, new_ppn, self.permission.to_pte_flags());
                pte_site::record(page_table.root_ppn().0, vpn.0, new_ppn.0, pte_site::SITE_COW_COPY);
                // FRAMEPROBE 取证：insert 前快照（insert 会摘除旧句柄，
                // 此处引用 frame 的借用必须先结束）。
                let fp_old_ppn = frame.ppn;
                let fp_old_meta = frame.meta_ref();
                let fp_old_strong = frame.strong();
                let retired = self.frames.insert(vpn, SharedFrame::from_alloc(new_frame));
                pte_site::record(
                    page_table.root_ppn().0,
                    vpn.0,
                    new_ppn.0,
                    pte_site::SITE_COW_COPY + pte_site::FRAMES_BIAS,
                );
                if crate::cmdline::frameprobe_enabled() && FPDBG_COW.fetch_add(1, Ordering::Relaxed) < 24 {
                    crate::println!(
                        "[FPDBG] cow vpn={:#x} old_ppn={:#x} old_meta={} old_strong={} new_ppn={:#x}",
                        vpn.0, fp_old_ppn.0, fp_old_meta, fp_old_strong, new_ppn.0,
                    );
                }

                // 刷新 TLB（异步广播；复用前强序由调用方 drain 后 drop 旧帧保证）
                crate::arch::asm_helpers::flush_tlb_va(vpn.0 << 12);

                return Ok(retired);
            } else {
                // 页面只被当前进程使用，直接恢复写权限
                let ppn = frame.ppn;

                // 使用 remap 恢复写权限
                page_table.remap(vpn, ppn, self.permission.to_pte_flags());
                pte_site::record(page_table.root_ppn().0, vpn.0, ppn.0, pte_site::SITE_COW_RESTORE);

                // 同 ppn 仅加 W 位（权限放宽）：其它 hart 的陈旧 RO 条目会
                // 再次写 fault 自愈，免跨核 shootdown。
                crate::arch::asm_helpers::flush_tlb_va_local(vpn.0 << 12);

                return Ok(None);
            }
        }

        if let Some(old_ppn) = page_table.translate(vpn) {
            // 同上：OOM 时返回 Err，让 trap 层终止当前进程而非 halt 内核。
            let Some(new_frame) = frame_alloc_uninit() else {
                return Err(());
            };
            let new_ppn = new_frame.ppn;
            let src = old_ppn.as_bytes_mut();
            let dst = new_ppn.as_bytes_mut();
            dst.copy_from_slice(src);
            page_table.remap(vpn, new_ppn, self.permission.to_pte_flags());
            pte_site::record(page_table.root_ppn().0, vpn.0, new_ppn.0, pte_site::SITE_COW_COPY);
            let retired = self.frames.insert(vpn, SharedFrame::from_alloc(new_frame));
            crate::arch::asm_helpers::flush_tlb_va(vpn.0 << 12);
            return Ok(retired);
        }
        if !self.map_page(vpn, page_table) {
            return Err(());
        }
        // fresh map（无效→有效）：其它 hart 无有效陈旧条目，免跨核 shootdown。
        crate::arch::asm_helpers::flush_tlb_va_local(vpn.0 << 12);
        Ok(None)
    }

    /// 克隆 VMA（用于 fork）
    pub fn clone_cow(&self) -> Self {
        Self {
            start: self.start,
            end: self.end,
            vma_type: self.vma_type,
            permission: self.permission,
            cow: true,
            locked: self.locked,
            file_backed: self.file_backed,
            may_write: self.may_write,
            huge_page_size: self.huge_page_size,
            huge_reserved: self.huge_reserved,
            backing: self.backing.clone(),
            file_path: self.file_path.clone(),
            file_offset: self.file_offset,
            frames: self.frames.clone(), // Arc 引用计数增加
            huge_frames: self.huge_frames.clone(),
            anon_shared: self.anon_shared.clone(),
            wipeonfork: self.wipeonfork,
            dontdump: self.dontdump,
            pkey: self.pkey,
        }
    }
}

impl core::fmt::Debug for Vma {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Vma")
            .field("start", &self.start)
            .field("end", &self.end)
            .field("vma_type", &self.vma_type)
            .field("permission", &self.permission)
            .field("cow", &self.cow)
            .field("locked", &self.locked)
            .field("file_backed", &self.file_backed)
            .field("may_write", &self.may_write)
            .field("huge_page_size", &self.huge_page_size)
            .field("file_path", &self.file_path.as_deref())
            .field("file_offset", &self.file_offset)
            .field("backing", &self.backing.is_some())
            .finish()
    }
}

/// `LAVMADBG=1` 门控的 VMA 生命周期追踪。
///
/// BuildStorm 的线程栈故障表现为「SP 落在 VMA 之上的空洞里」，光看故障瞬间的
/// VMA 表分不清是建立时就短、还是事后被 unmap/mprotect 切走。这里在每个会改变
/// VMA 集合的入口打一行，配合 `dump_all_vmas` 即可复原该地址区间的完整历史。
/// 默认关闭，正式跑分零开销。
#[inline]
fn vmaop_trace(op: &str, start: usize, end: usize, kind: Option<VmaType>) {
    if !crate::cmdline::vmaopdbg_enabled() {
        return;
    }
    let pid = crate::sched::process_sched::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed);
    let hart = crate::arch::asm_helpers::read_cpu_id();
    match kind {
        Some(k) => crate::println!(
            "[VMAOP] {} pid={} hart={} [{:#x},{:#x}) type={:?}",
            op, pid, hart, start, end, k
        ),
        None => crate::println!(
            "[VMAOP] {} pid={} hart={} [{:#x},{:#x})",
            op, pid, hart, start, end
        ),
    }
}

/// 地址空间
pub struct AddressSpace {
    /// 页表
    page_table: PageTable,
    /// 虚拟内存区域列表
    // Box 化：Vma 结构 ~250B，Vec 插入/删除/整表重建的 O(N) 挪移在 VMA 数
    // 上千（libc-bench 2500 线程栈）时是 memmove 首要热点；指针挪移便宜 30×。
    vmas: Vec<Box<Vma>>,
    /// `MADV_FREE` 惰性回收候选页（VM_FREE / lazy-free）。对标 Linux：
    /// `madvise(MADV_FREE)` 不立即释放，而是标记页「干净可丢弃」，仅在内存
    /// 压力（reclaim）时才丢弃；丢弃后下次访问得到清零页。一旦该页在丢弃前
    /// 被**写入**（写错误故障）即取消标记、内容保留。由于本内核 `to_pte_flags`
    /// 预置 D 位、不依赖硬件脏位，改用「清除 PTE 写位 → 写故障侦测重新弄脏」
    /// 实现，写位侦测路径见 `try_resolve_fault`/`handle_page_fault`。
    lazy_free: BTreeSet<VirtPageNum>,
    /// 本地址空间当前换出到 swap 的页数（VmSwap 计账）。`swap_out_page` +1，
    /// `swap_in` -1，换出页随 munmap/`MADV_DONTNEED`/teardown 释放时 -1，fork
    /// 继承 swap entry 时子空间 +1。`/proc/[pid]/status` 的 `VmSwap` 读取它。
    swapped_pages: usize,
    /// reclaim clock 算法的扫描游标（私有匿名候选页的环形指针，按 vpn 升序）。
    /// 每次 `reclaim` 从上次停留处继续，配合 PTE 访问位实现二次机会（second
    /// chance）：访问位置位的页清位放过，未访问的冷页换出。对标 Linux 的
    /// inactive LRU + 访问位老化，避免反复换出活跃工作集（抖动）。
    reclaim_cursor: VirtPageNum,
    /// mmap 自动分配的 bump 游标（高水位）：记录所有 VMA 末端中落在 mmap 窗口
    /// 内的最大值，`add_vma` 同步抬升。其语义不变式是「`mmap_bump` 之上的窗口
    /// 区间内一定没有任何 VMA」，于是 `auto_alloc` 可直接从此处 O(1) 取址，无需
    /// 对成千上万个 VMA 反复全扫+排序（max_map_count 一次 flood 6.5w 映射，旧
    /// 全扫为 O(N²) 必超时）。游标触及窗口上界时回退到全扫，复用 munmap 留下的
    /// 低地址空洞，保证不会因「只增不减」而提前 ENOMEM（正确性兜底）。
    mmap_bump: usize,
    /// `release_user_frames` 已执行的标记：exit 路径（mmput → 0）已清空全部
    /// 用户叶子 PTE 并刷过 TLB，且此后本地址空间永不再建立用户映射（见
    /// `release_user_frames` 注释）。`Drop` 据此跳过第二次整表 walk + TLB 刷新
    /// —— 该安全网仅为 release 未被调用的边角场景保留。
    user_frames_released: bool,
    /// 已摘除 PTE、但尚未归还 buddy 的用户帧（lazy-TLB quarantine）。
    ///
    /// munmap/MADV_DONTNEED 热路径不能每次同步等全核 TLB ack（pthread 栈频繁
    /// unmap，pchist 实测 `unmap_region` 23.7G 指令）；先把帧隔离在本 mm，
    /// 对端即使用陈旧 TLB 也只能命中仍属本 mm 的旧帧，不会踩到新用途。累计到
    /// 阈值或 mm exit 时一次 drain_shootdown 后批量 drop 归还 buddy。
    retired_frames: Vec<SharedFrame>,
    /// pkey 分配位图（per-mm，对标 Linux mm->context pkey allocation）。
    /// bit0 = 默认 key，恒占用；共 16 个 key（与 x86 PKU 一致）。fork 继承，
    /// exec 重置（新建地址空间）。
    pub pkey_alloc_mask: u32,
    /// 每个 key 的访问权限（bit0=PKEY_DISABLE_ACCESS，bit1=PKEY_DISABLE_WRITE）。
    /// RV/LA 无用户态 PKRU，只能由 pkey_alloc 的 init access 设定。
    pub pkey_rights: [u8; 16],
}

/// 区域取消映射的统计信息（用于上层做后置账务，如 HugeTLB 释放）。
#[derive(Debug, Default, Clone, Copy)]
pub struct UnmapStats {
    /// 被释放的 HugeTLB VMA 总字节数（含未实例化页，按 span 计）。
    pub hugetlb_bytes_released: usize,
    /// 上次释放的 HugeTLB 单页大小（字节）。当 `hugetlb_bytes_released == 0`
    /// 时无意义。混合大页大小目前不会发生（mmap 不允许）。
    pub huge_page_size: usize,
    /// 被释放的 HugeTLB 大页中**已实例化**（缺页提交过）的页数，用于 munmap
    /// 时按 Linux 账本归还（surplus 优先退回 buddy，否则回 free）。
    pub huge_touched_pages: usize,
    /// 被释放的 HugeTLB 映射是否在 mmap 时建立了预留（决定未触碰页的归账）。
    pub huge_reserved: bool,
}

/// Result of the first phase of demand-paging fault resolution.
///
/// Modeled after Linux's `vm_fault_t` return values from `handle_pte_fault`.
/// The trap handler uses this to decide whether to release the process table
/// lock and perform file I/O (phase 2) before re-acquiring and installing
/// the page (phase 3).
pub enum FaultAction {
    /// Fault fully handled (anonymous zero-page, CoW, TLB refresh, etc.).
    /// No further action needed by the trap handler.
    Handled,
    /// Fault handled, but the PTE change was CoW/permission-critical and must
    /// become visible on every hart before returning to userspace. The trap
    /// handler calls `drain_shootdown()` only after dropping mm/table locks.
    HandledNeedsTlbDrain,
    /// Fault requires file I/O that must NOT be done under the process table
    /// lock. The trap handler should:
    ///   1. Release PROCESS_TABLE lock
    ///   2. Read file data using the info below (+ readahead into page cache)
    ///   3. Re-acquire PROCESS_TABLE lock
    ///   4. Call `install_demand_pages` to map the frame(s)
    NeedFileRead(FileReadInfo),
    /// Fault cannot be handled (bad address, permission denied, etc.).
    /// The i32 is the signal to deliver (11 = SIGSEGV, 7 = SIGBUS).
    Failed(i32),
}

/// Metadata extracted under lock for a file-backed demand page fault.
/// Contains everything needed to perform file I/O without holding
/// PROCESS_TABLE.
pub struct FileReadInfo {
    /// The backing inode (Arc-cloned, survives lock release).
    pub inode: Arc<dyn crate::fs::Inode>,
    /// Stable inode identity for page cache keying (Arc::as_ptr cast).
    pub inode_id: usize,
    /// File offset of the faulting page.
    pub file_offset: usize,
    /// File size (to bound reads and detect EOF).
    pub file_size: usize,
    /// The faulting virtual page number.
    pub vpn: VirtPageNum,
    /// VMA start address (for offset calculations).
    pub vma_start: VirtAddr,
    /// VMA end address (for readahead bounds).
    pub vma_end: VirtAddr,
    /// VMA file_offset base.
    pub vma_file_offset: usize,
    /// PTE flags to use when mapping.
    pub pte_flags: PTEFlags,
    /// VMA index in the vmas array (for install phase).
    pub vma_idx: usize,
    /// Whether the VMA is a shared mapping (MAP_SHARED).
    pub is_shared: bool,
}

impl AddressSpace {
    // A single faulting access may sit far below the current stack low edge
    // when a function reserves a large frame in one `addi sp, sp, -N`
    // (e.g. writetest's ~768 KiB on-stack buffer, rustc's multi-MiB frames).
    // Linux grows the stack for any access within RLIMIT_STACK. BuildStorm's
    // rustc prints "increase RUST_MIN_STACK=16777216" then SIGSEGVs at
    // 0x7fffff0000 when the grow window is only 8 MiB — one `addi sp, -16M`
    // lands outside the window and never expands. Use 32 MiB to cover rustc
    // and leave headroom for larger rlimits.
    const STACK_AUTO_GROW_WINDOW: usize = 32 * 1024 * 1024 / PAGE_SIZE * PAGE_SIZE;
    /// Linux `stack_guard_gap` (default `256 * PAGE_SIZE`): a growable stack
    /// mapping is not allowed to auto-grow its low edge to within this many
    /// bytes of a lower, non-growable mapping. Touching the guard gap then
    /// faults (SIGSEGV) instead of silently growing into it (LTP `mmap18`
    /// `grow_stack_fail`).
    const STACK_GUARD_GAP: usize = 256 * PAGE_SIZE;

    /// True when extending a stack's low edge down to `new_start` would land
    /// within `STACK_GUARD_GAP` of the top of some other mapping that sits
    /// below it. Another growable stack does not count as a blocker (Linux:
    /// `!(prev->vm_flags & VM_GROWSDOWN)`).
    fn stack_grow_hits_guard_gap(&self, new_start: VirtAddr, self_idx: usize) -> bool {
        self.vmas.iter().enumerate().any(|(i, other)| {
            i != self_idx
                && other.vma_type != VmaType::Stack
                && other.end.0 <= new_start.0
                && new_start.0 - other.end.0 < Self::STACK_GUARD_GAP
        })
    }

    fn permission_has_access(permission: VmaPermission) -> bool {
        permission.read || permission.write || permission.execute
    }

    fn h23dbg() -> bool {
        crate::cmdline::h23dbg_enabled()
    }

    /// fork 懒建子 PTE 的缺页补建 + fault-around：把 vpn 所在页与同 VMA 内其后
    /// 最多 `FAULT_AROUND-1` 个已有帧（frames 中存在且尚未映射、且不是 swap
    /// entry）一并建 PTE，摊薄逐页 trap 的进出内核开销（对标 Linux fault-around）。
    /// cow VMA 一律补只读（写路径由 handle_cow 拦截后单页升级）。
    /// 返回触发页是否成功建映射。
    fn map_lazy_frames_around(&mut self, idx: usize, vpn: VirtPageNum, is_cow: bool) -> bool {
        const FAULT_AROUND: usize = 32;
        let mut flags = self.vmas[idx].permission.to_pte_flags();
        if is_cow {
            flags.remove(PTEFlags::W);
            flags.remove(PTEFlags::D);
        }
        let batch: alloc::vec::Vec<(VirtPageNum, super::PhysPageNum)> = self.vmas[idx]
            .frames
            .range(vpn..VirtPageNum(vpn.0 + FAULT_AROUND))
            .map(|(v, f)| (*v, f.ppn))
            .collect();
        let mut hit = false;
        for (v, ppn) in batch {
            if v == vpn {
                hit = self.page_table.try_map(v, ppn, flags);
                if hit {
                    pte_site::record(self.page_table.root_ppn().0, v.0, ppn.0, pte_site::SITE_LAZY_AROUND);
                }
            } else if self.page_table.translate(v).is_none()
                && self.page_table.get_swap(v).is_none()
            {
                if self.page_table.try_map(v, ppn, flags) {
                    pte_site::record(self.page_table.root_ppn().0, v.0, ppn.0, pte_site::SITE_LAZY_AROUND);
                }
            }
        }
        hit
    }

    fn extract_segment(vma: &mut Vma, start: VirtAddr, end: VirtAddr, permission: VmaPermission) -> Vma {
        vmaop_trace("split", start.0, end.0, Some(vma.vma_type));
        let mut segment = Vma::new(start, end, vma.vma_type, permission);
        // cow 标记跨 mprotect 保持：即使段暂时不可写，其帧仍可能与兄弟进程/
        // 全局零页共享，后续 mprotect 升回可写时写路径必须仍经 handle_cow 复制。
        segment.cow = vma.cow;
        segment.locked = vma.locked;
        segment.file_backed = vma.file_backed;
        segment.may_write = vma.may_write;
        segment.huge_page_size = vma.huge_page_size;
        segment.huge_reserved = vma.huge_reserved;
        segment.backing = vma.backing.clone();
        segment.file_path = vma.file_path.clone();
        // Both halves of a split anonymous-shared mapping keep referring to the
        // same shmem object so they continue to share faulted-in pages.
        segment.anon_shared = vma.anon_shared.clone();
        // A split preserves MADV_WIPEONFORK on both halves.
        segment.wipeonfork = vma.wipeonfork;
        // A split preserves MADV_DONTDUMP on both halves.
        segment.dontdump = vma.dontdump;
        // A split preserves the protection key on both halves.
        segment.pkey = vma.pkey;
        // Shift the file offset by how far this segment starts past the VMA's
        // base so each split keeps the correct file position for writeback.
        segment.file_offset = vma.file_offset + start.0.saturating_sub(vma.start.0);

        let start_vpn = start.floor();
        let end_vpn = end.ceil();
        let keys: Vec<VirtPageNum> = vma
            .frames
            .range(start_vpn..end_vpn)
            .map(|(vpn, _)| *vpn)
            .collect();
        for vpn in keys {
            if let Some(frame) = vma.frames.remove(&vpn) {
                segment.frames.insert(vpn, frame);
            }
        }
        if vma.huge_page_size != 0 {
            let pages_per_huge = vma.huge_page_size / PAGE_SIZE;
            if pages_per_huge != 0 {
                let start_base = (start_vpn.0 / pages_per_huge) * pages_per_huge;
                let end_base = ((end_vpn.0 + pages_per_huge - 1) / pages_per_huge) * pages_per_huge;
                let huge_keys: Vec<VirtPageNum> = vma
                    .huge_frames
                    .range(VirtPageNum(start_base)..VirtPageNum(end_base))
                    .map(|(vpn, _)| *vpn)
                    .collect();
                for vpn in huge_keys {
                    if let Some(frame) = vma.huge_frames.remove(&vpn) {
                        segment.huge_frames.insert(vpn, frame);
                    }
                }
            }
        }

        segment
    }

    fn apply_segment_permissions(page_table: &mut PageTable, vma: &Vma) {
        let allow_access = Self::permission_has_access(vma.permission);
        // HugeTLB 段保持按范围处理：页表项以 huge 粒度存在、frames 用 huge_frames
        // 记账，且 hugetlb mprotect 罕见，不是热路径。
        if vma.huge_page_size != 0 {
            let dbg = Self::h23dbg();
            for vpn_num in vma.vpn_range() {
                let vpn = VirtPageNum(vpn_num);
                if allow_access {
                    if let Some(ppn) = page_table.translate(vpn) {
                        page_table.remap(vpn, ppn, vma.permission.to_pte_flags());
                        if dbg && vpn_num == vma.start.floor().0 {
                            crate::println!("[h23] apply remap vpn={:#x} -> flags={:#x}",
                                vpn.0, vma.permission.to_pte_flags().bits());
                        }
                    } else if let Some(frame) = vma.frames.get(&vpn) {
                        page_table.map(vpn, frame.ppn, vma.permission.to_pte_flags());
                    } else if dbg && vpn_num == vma.start.floor().0 {
                        crate::println!("[h23] apply: vpn={:#x} NOT translatable, no frame (huge_frames={})",
                            vpn.0, vma.huge_frames.len());
                    }
                } else if page_table.translate(vpn).is_some() {
                    page_table.unmap(vpn);
                }
            }
            return;
        }
        // 普通段：只遍历实际映射的页（vma.frames，稀疏），不再逐 VA 扫整个声明范围。
        // 未 fault 的页没有 PTE/frame，权限已记录在 vma.permission，缺页时按新权限
        // 映射即可——glibc 创建线程时对整段 8MiB 栈 mprotect（页尚未 fault，frames
        // 为空），旧版每次空扫 ~2048 个 VPN，是 libc-bench pthread create 簇的首要
        // 热点（§1.13① 消除热路径 O(声明范围)）。
        let flags = vma.permission.to_pte_flags();
        // 私有映射里 Arc 被多方持有的帧（fork CoW 共享 / 全局零页）不得直授
        // W/D：写路径必须经 handle_cow 复制断开，否则 mprotect 会把共享帧
        // 直接改成可写（写穿兄弟进程/零页）。共享映射（MAP_SHARED）的多方
        // 持有是其语义本身，不受此限。
        let private = vma.vma_type != VmaType::Shared && vma.anon_shared.is_none();
        for (&vpn, frame) in vma.frames.iter() {
            if allow_access {
                let mut f = flags;
                // 同 `handle_cow`：共享事实以 MetaSlot 帧引用计数为准，fork
                // 后父子各持独立句柄时 strong 为 1，只看它会让 mprotect
                // 把仍共享的帧直接改成可写。
                let shared = frame.strong() > 1 || frame.meta_ref() > 1;
                if private && shared {
                    f.remove(PTEFlags::W);
                    f.remove(PTEFlags::D);
                }
                if page_table.translate(vpn).is_some() {
                    page_table.remap(vpn, frame.ppn, f);
                } else {
                    page_table.map(vpn, frame.ppn, f);
                }
            } else if page_table.translate(vpn).is_some() {
                page_table.unmap(vpn);
            }
        }
    }

    /// 创建新的地址空间
    ///
    /// 新创建的地址空间会映射内核空间，以便在用户态和内核态之间切换时
    /// 内核代码仍然可以访问。
    pub fn new() -> Self {
        let mut page_table = PageTable::new();

        // 映射内核空间（恒等映射）
        // 内核代码和数据位于 0x8020_0000 - MEMORY_END
        // 使用恒等映射，虚拟地址 = 物理地址
        Self::map_kernel_space(&mut page_table);

        Self {
            page_table,
            vmas: Vec::new(),
            lazy_free: BTreeSet::new(),
            swapped_pages: 0,
            reclaim_cursor: VirtPageNum(0),
            mmap_bump: 0,
            user_frames_released: false,
            retired_frames: Vec::new(),
            pkey_alloc_mask: 1,
            pkey_rights: [0; 16],
        }
    }

    /// 映射内核空间到用户页表
    ///
    /// 使用 Sv39 大页（gigapage / megapage）直接在根页表中设置条目：
    /// - root[2]: 1GB gigapage，恒等映射 0x8000_0000 – 0xBFFF_FFFF（内核代码、数据、堆、物理内存）
    /// - root[0] 下的 4KB 页映射 UART 和 VirtIO MMIO
    ///
    /// 每个地址空间独立创建 MMIO 的 L1/L0 页表页，
    /// 避免共享导致跨地址空间页表污染（fork/exec/exit 后 L0 页被释放导致父进程映射丢失）。
    fn map_kernel_space(page_table: &mut PageTable) {
        use super::page_table::{PTEFlags, PageTableEntry};
        use super::{VirtPageNum, PhysPageNum, PAGE_SIZE};

        #[cfg(target_arch = "riscv64")]
        {
            // RISC-V64: root[2..2+n] 1GB gigapage 恒等映射 [0x8000_0000,
            // kernel_ident_end)。n 由 boot 时实测 RAM 决定（-m 1G → 1 个，
            // 与历史行为一致；-m 8G → 8 个）。trap 全程运行在用户页表上，
            // 内核任何可分配帧都必须被这些 gigapage 覆盖。
            let root = page_table.root_ppn().get_pte_array();
            for i in 0..crate::mm::kernel_ident_gigapages() {
                root[2 + i] = PageTableEntry::new(
                    PhysPageNum(0x80000 + (i << 18)),
                    PTEFlags::R | PTEFlags::W | PTEFlags::X | PTEFlags::V | PTEFlags::A | PTEFlags::D,
                );
            }

            // UART: 0x1000_0000
            for va in (0x1000_0000usize..0x1000_1000).step_by(PAGE_SIZE) {
                let vpn = VirtPageNum(va >> 12);
                let ppn = PhysPageNum(va >> 12);
                page_table.map(vpn, ppn, PTEFlags::R | PTEFlags::W | PTEFlags::V | PTEFlags::A | PTEFlags::D);
            }
            // VirtIO MMIO: 0x1000_1000 – 0x1000_9000
            for va in (0x1000_1000usize..0x1000_9000).step_by(PAGE_SIZE) {
                let vpn = VirtPageNum(va >> 12);
                let ppn = PhysPageNum(va >> 12);
                page_table.map(vpn, ppn, PTEFlags::R | PTEFlags::W | PTEFlags::V | PTEFlags::A | PTEFlags::D);
            }
        }

        #[cfg(target_arch = "loongarch64")]
        {
            // LoongArch64: 内核通过 DMW 直接映射窗口访问，不需要页表映射
            // 用户页表只需要映射用户空间的页
            // DMW0 (CSR 0x180): VA[63:60]=0x9 → PA (Coherent Cached)
            // 内核代码/数据/MMIO 都通过 DMW 访问，不经过 TLB
            //
            // 但需要映射 UART 和 PCI MMIO 到用户可见的地址空间（如果需要）
            // 目前用户态不直接访问 MMIO，所以不需要映射
            let _ = page_table; // suppress unused warning
        }
    }

    /// Fallible variant of `map_kernel_space`: returns `false` instead of
    /// panicking when intermediate page-table nodes cannot be allocated (OOM).
    /// Used by the fork path so an out-of-memory fork fails gracefully.
    fn try_map_kernel_space(page_table: &mut PageTable) -> bool {
        use super::page_table::{PTEFlags, PageTableEntry};
        use super::{VirtPageNum, PhysPageNum, PAGE_SIZE};

        #[cfg(target_arch = "riscv64")]
        {
            // 与 map_kernel_space 相同：root[2..2+n] gigapage 覆盖实测 RAM。
            let root = page_table.root_ppn().get_pte_array();
            for i in 0..crate::mm::kernel_ident_gigapages() {
                root[2 + i] = PageTableEntry::new(
                    PhysPageNum(0x80000 + (i << 18)),
                    PTEFlags::R | PTEFlags::W | PTEFlags::X | PTEFlags::V | PTEFlags::A | PTEFlags::D,
                );
            }

            for va in (0x1000_0000usize..0x1000_1000).step_by(PAGE_SIZE) {
                let vpn = VirtPageNum(va >> 12);
                let ppn = PhysPageNum(va >> 12);
                if !page_table.try_map(vpn, ppn, PTEFlags::R | PTEFlags::W | PTEFlags::V | PTEFlags::A | PTEFlags::D) {
                    return false;
                }
            }
            for va in (0x1000_1000usize..0x1000_9000).step_by(PAGE_SIZE) {
                let vpn = VirtPageNum(va >> 12);
                let ppn = PhysPageNum(va >> 12);
                if !page_table.try_map(vpn, ppn, PTEFlags::R | PTEFlags::W | PTEFlags::V | PTEFlags::A | PTEFlags::D) {
                    return false;
                }
            }
        }

        #[cfg(target_arch = "loongarch64")]
        {
            let _ = page_table;
        }

        true
    }

    /// Fallible variant of `new`: returns `None` instead of panicking when the
    /// address space cannot be allocated (OOM). Used by the fork path so an
    /// out-of-memory fork fails gracefully (-ENOMEM).
    pub fn try_new() -> Option<Self> {
        let mut page_table = PageTable::try_new()?;
        if !Self::try_map_kernel_space(&mut page_table) {
            return None;
        }
        Some(Self {
            page_table,
            vmas: Vec::new(),
            lazy_free: BTreeSet::new(),
            swapped_pages: 0,
            reclaim_cursor: VirtPageNum(0),
            mmap_bump: 0,
            user_frames_released: false,
            retired_frames: Vec::new(),
            pkey_alloc_mask: 1,
            pkey_rights: [0; 16],
        })
    }

    /// 获取页表
    pub fn page_table(&self) -> &PageTable {
        &self.page_table
    }

    /// 获取可变页表
    pub fn page_table_mut(&mut self) -> &mut PageTable {
        &mut self.page_table
    }

    /// 获取 SATP 值
    pub fn satp(&self) -> usize {
        self.page_table.satp()
    }

    /// VMA 不重叠自检（仅 EXECDBG 开启时生效，且只在违例时打印）。
    /// Linux 的 VMA 集合恒不重叠，任何重叠都意味着后续缺页/CoW 只会命中其中
    /// 一条、另一条的 frames 变陈旧，fork/exec 便会传播错误内存。
    pub fn debug_assert_no_overlap(&self, tag: &str) {
        if !crate::cmdline::execdbg_enabled() {
            return;
        }
        for i in 0..self.vmas.len() {
            let a = &self.vmas[i];
            if let Some(b) = self.vmas.get(i + 1) {
                if a.end.0 > b.start.0 {
                    crate::println!(
                        "[EXECDBG] overlap-after-{} a=[{:#x},{:#x}) type={:?} b=[{:#x},{:#x}) type={:?}",
                        tag, a.start.0, a.end.0, a.vma_type, b.start.0, b.end.0, b.vma_type
                    );
                }
            }
        }
    }

    /// 添加 VMA
    pub fn add_vma(&mut self, vma: Vma) {
        vmaop_trace("add", vma.start.0, vma.end.0, Some(vma.vma_type));
        self.raise_mmap_bump_for(&vma);
        self.insert_vma_sorted(Box::new(vma));
        self.debug_assert_no_overlap("add_vma");
    }

    /// 按 start 升序插入，维持「vmas 全程按地址有序且互不重叠」不变式，
    /// 使 find_vma / protect_region 可用二分定位（sys_mprotect 线性全扫
    /// 是 pchist TOP 热点 ~103M 指令）。
    fn insert_vma_sorted(&mut self, vma: Box<Vma>) {
        let idx = self.vmas.partition_point(|v| v.start < vma.start);
        self.vmas.insert(idx, vma);
    }

    /// 维护 mmap bump 不变式：bump 始终 >= 所有落在 mmap 窗口内的 VMA 末端，
    /// 使 `auto_alloc` 的 fast-path 从 bump 取址恒不与现有 VMA 冲突。窗口外
    /// （如高地址用户栈）的 VMA 不抬升 bump，避免把 mmap 自动区无谓推高。
    ///
    /// **任何**把 VMA 落入 mmap 自动分配窗口的插入路径都必须调用本方法，否则
    /// 快路径会把新映射分配到既有 VMA 之上。典型坑：LA 用户程序加载基址
    /// =0x2000_0000 恰落在窗口内，ELF 段 / heap 走 `map_region` 直接 push 而
    /// 不抬 bump 时，flood mmap 会覆盖程序自身的代码/数据段，随后程序写自身
    /// 数据触发写时异常（LA 上为 PME）无限循环 hang。
    fn raise_mmap_bump_for(&mut self, vma: &Vma) {
        let (base, end) = Self::mmap_window();
        if vma.start.0 < end && vma.end.0 > base {
            // Heap 之上留 1 页隔离带：heap 自动扩展会把紧邻 heap end 的写故障
            // 当 musl 边界写处理（含 PROT_NONE 守卫页挖除），mmap(NULL) 若紧贴
            // heap 尾部分配会被该启发式误吞（mprotect04 PROT_NONE 页被写穿）。
            let guard = if vma.vma_type == VmaType::Heap { PAGE_SIZE } else { 0 };
            let e = (vma.end.0 + guard).min(end);
            if e > self.mmap_bump {
                self.mmap_bump = e;
            }
        }
    }

    /// mmap 自动分配窗口 [base, end)。与 `auto_alloc_mmap_addr_aligned` 保持一致。
    #[inline]
    pub fn mmap_window() -> (usize, usize) {
        // mmap 自动分配窗口。上界统一 0x38_0000_0000（RV Sv39 / LA 3 级页表用户
        // VA 均达 256~512GiB，栈在其上方），使反复持有大匿名映射的负载
        // （cve-2017-17052 每轮保活 ~224×16MiB）不再耗尽 VA 误报 ENOMEM。
        //   RISC-V64: 程序链接在 0x10000、heap 很低，基址 0x2000_0000 与其隔离；
        //             内核 gigapage [0x8000_0000, 0xc000_0000) 由搜索合成占用段跳过。
        //   LoongArch64: 程序链接在 0x1_2000_0000 附近、heap 紧随其后 brk 增长，
        //             基址抬到 0x4_0000_0000（heap 需增长 >11GiB 才会触及），
        //             避免 mmap(NULL) 紧贴 heap 尾部阻断 brk 扩展（brk01/sbrk01）
        //             或被 heap 边界写启发式误吞（mprotect04）。
        #[cfg(target_arch = "riscv64")]
        const MMAP_BASE: usize = 0x2000_0000;
        #[cfg(not(target_arch = "riscv64"))]
        const MMAP_BASE: usize = 0x4_0000_0000;
        const MMAP_END: usize = 0x38_0000_0000;
        (MMAP_BASE, MMAP_END)
    }

    /// 当前 mmap bump 高水位（auto_alloc fast-path 用）。
    #[inline]
    pub fn mmap_bump(&self) -> usize {
        self.mmap_bump
    }

    /// 推进 mmap bump 高水位（auto_alloc fast-path 成功取址后调用）。
    #[inline]
    pub fn set_mmap_bump(&mut self, v: usize) {
        if v > self.mmap_bump {
            self.mmap_bump = v;
        }
    }

    /// 返回 VMA 数量（调试用）
    pub fn vma_count(&self) -> usize {
        self.vmas.len()
    }

    pub fn mapped_frame_count(&self) -> usize {
        self.vmas.iter().map(|vma| vma.frames.len()).sum()
    }

    /// UAF 探针：返回本地址空间中「即将被真正释放」的页帧 (vpn, ppn)。
    /// 仅收集 `meta_ref == 1` 的帧——这些句柄 drop 后会归还 buddy。
    /// 若另一存活进程的页表仍映射同一 ppn，则那是悬空 PTE（跨进程 UAF）。
    pub fn frames_will_free_dbg(&self) -> Vec<(VirtPageNum, super::PhysPageNum)> {
        let mut out = Vec::new();
        for vma in &self.vmas {
            for (vpn, frame) in vma.frames.iter() {
                if frame.meta_ref() == 1 {
                    out.push((*vpn, frame.ppn));
                }
            }
        }
        out
    }

    /// UAF 探针：本地址空间页表是否把 `vpn` 映射到 `ppn`。
    pub fn maps_vpn_to_dbg(&self, vpn: VirtPageNum, ppn: super::PhysPageNum) -> bool {
        self.page_table.translate(vpn) == Some(ppn)
    }

    /// UAF 探针：诊断 `vpn` 的状态——返回 (PTE 映射的 ppn, 是否被某 VMA 的
    /// frames 追踪即持有 Arc<FrameTracker>)。PTE 有映射但无 Arc = 悬空映射。
    pub fn diagnose_vpn_dbg(&self, vpn: VirtPageNum) -> (Option<super::PhysPageNum>, bool) {
        let ppn = self.page_table.translate(vpn);
        let tracked = self.vmas.iter().any(|v| v.frames.contains_key(&vpn));
        (ppn, tracked)
    }

    /// UAF 探针（扩展）：返回 (PTE ppn, vma.frames 中该 vpn 的共享持有数)。
    pub fn diagnose_vpn_full_dbg(&self, vpn: VirtPageNum) -> (Option<super::PhysPageNum>, Option<usize>) {
        let ppn = self.page_table.translate(vpn);
        let sc = self
            .vmas
            .iter()
            .find_map(|v| v.frames.get(&vpn).map(|f| f.strong()));
        (ppn, sc)
    }

    /// 诊断：返回 vma.frames 中该 vpn 登记的物理帧（若无记录返回 None）。
    /// 用于对比「页表 PTE 实际指向的帧」与「frames 登记的所有权帧」是否一致。
    pub fn frames_ppn_for_dbg(&self, vpn: VirtPageNum) -> Option<super::PhysPageNum> {
        self.vmas
            .iter()
            .find_map(|v| v.frames.get(&vpn).map(|f| f.ppn))
    }

    /// 诊断：返回 vpn 对应的 VMA frames 中的共享持有数。
    pub fn vma_strong_count_dbg(&self, vpn: VirtPageNum) -> Option<usize> {
        self.vmas.iter().find_map(|v| v.frames.get(&vpn).map(|f| f.strong()))
    }

    /// 诊断：vpn 是否落在任何 VMA 的 [start, end) 范围内。
    pub fn vma_containing_dbg(&self, vpn: VirtPageNum) -> bool {
        let va = super::VirtAddr(vpn.0 << 12);
        self.vmas.iter().any(|v| va >= v.start && va < v.end)
    }

    /// 诊断：打印所有 VMA 的边界和类型。
    pub fn dump_vmas_dbg(&self, prefix: &str) {
        for (i, vma) in self.vmas.iter().enumerate() {
            crate::println!(
                "{} vma[{}] {:#x}-{:#x} type={:?} perm=r{}{}x{} frames={}",
                prefix, i, vma.start.0, vma.end.0, vma.vma_type,
                if vma.permission.read { 'r' } else { '-' },
                if vma.permission.write { 'w' } else { '-' },
                if vma.permission.execute { 'x' } else { '-' },
                vma.frames.len()
            );
        }
    }

    /// UAF 探针：全量反查——本地址空间页表里映射到 `ppns` 集合中任一 ppn 的
    /// 所有 (vpn, ppn)。用于在某进程释放帧时检测是否有其它存活进程仍映射同帧。
    pub fn vpns_mapping_ppns_dbg(
        &self,
        ppns: &alloc::collections::BTreeSet<usize>,
    ) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        for vma in &self.vmas {
            for vpn_num in vma.vpn_range() {
                let vpn = VirtPageNum(vpn_num);
                if let Some(ppn) = self.page_table.translate(vpn) {
                    if ppns.contains(&ppn.0) {
                        out.push((vpn_num, ppn.0));
                    }
                }
            }
        }
        out
    }

    /// 返回 VMA 迭代器（只读，调试用）
    pub fn vmas_iter(&self) -> impl Iterator<Item = &Vma> {
        self.vmas.iter().map(|v| &**v)
    }

    /// LAVMADBG 取证：把整张 VMA 表倒出来。
    ///
    /// `vmas_near` 只给故障地址两侧各 k 条，看不出「该地址曾属于哪条 VMA、被谁
    /// 切走」；配合 `[VMAOP]` 生命周期日志才能把「建时就短」与「事后被切」分开。
    pub fn dump_all_vmas(&self, tag: &str) {
        crate::println!("[VMADUMP] {} count={}", tag, self.vmas.len());
        for (i, v) in self.vmas.iter().enumerate() {
            if i >= 96 {
                crate::println!("[VMADUMP] ... truncated at 96");
                break;
            }
            crate::println!(
                "[VMADUMP] [{:#x},{:#x}) type={:?} perm={}{}{} frames={}",
                v.start.0,
                v.end.0,
                v.vma_type,
                if v.permission.read { 'r' } else { '-' },
                if v.permission.write { 'w' } else { '-' },
                if v.permission.execute { 'x' } else { '-' },
                v.frames.len(),
            );
        }
    }

    /// 取 `va` 附近各 `k` 条 VMA 的摘要（LAVMADBG 取证用）：
    /// `(start, end, type, r, w, x)`。vmas 有序，直接按 partition_point 开窗。
    pub fn vmas_near(
        &self,
        va: VirtAddr,
        k: usize,
    ) -> alloc::vec::Vec<(usize, usize, VmaType, bool, bool, bool)> {
        let idx = self.vmas.partition_point(|vma| vma.start <= va);
        let lo = idx.saturating_sub(k);
        let hi = (idx + k).min(self.vmas.len());
        self.vmas[lo..hi]
            .iter()
            .map(|v| {
                (
                    v.start.0,
                    v.end.0,
                    v.vma_type,
                    v.permission.read,
                    v.permission.write,
                    v.permission.execute,
                )
            })
            .collect()
    }

    /// UAF 探针：本地址空间是否有任一 VMA frames 持有物理页 `ppn`（FrameTracker
    /// 级，含 page-cache 共享页）。返回首个映射到它的 vpn。用于判定「内核执行流
    /// 的 sp 是否落在某进程仍映射的用户帧内」。
    pub fn frame_owner_vpn_dbg(&self, ppn: usize) -> Option<VirtPageNum> {
        for vma in &self.vmas {
            for (vpn, frame) in vma.frames.iter() {
                if frame.ppn.0 == ppn {
                    return Some(*vpn);
                }
            }
        }
        None
    }

    /// 用一组**已就绪的共享物理帧**直接建立映射（exec 只读段缓存命中路径）：
    /// 不分配、不清零、不读文件，逐页 try_map 到缓存帧并 Arc 共享。
    /// `frames` 必须恰好按 vpn 升序覆盖 [start, end) 的每一页。
    /// VMA 标记 cow=true：若将来 mprotect 加写后写入，会触发写时复制，
    /// 绝不污染全局缓存帧。
    pub fn map_region_shared_frames(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        vma_type: VmaType,
        permission: VmaPermission,
        frames: &[SharedFrame],
    ) -> bool {
        let mut vma = Vma::new(start, end, vma_type, permission);
        vma.cow = true;
        let mut flags = permission.to_pte_flags();
        flags.remove(PTEFlags::W);
        flags.remove(PTEFlags::D);
        let start_vpn = start.floor().0;
        let mut entries: Vec<(VirtPageNum, SharedFrame)> = Vec::with_capacity(frames.len());
        for (i, frame) in frames.iter().enumerate() {
            let vpn = VirtPageNum(start_vpn + i);
            if self.page_table.translate(vpn).is_some() {
                continue;
            }
            if !self.page_table.try_map(vpn, frame.ppn, flags) {
                return false;
            }
            entries.push((vpn, frame.clone()));
        }
        vma.frames = FrameMap::from_sorted_vec(entries);
        self.raise_mmap_bump_for(&vma);
        self.insert_vma_sorted(Box::new(vma));
        true
    }

    /// exec 只读段缓存回填用：取起始地址为 `start` 的 VMA 的物理帧，按 vpn
    /// 升序克隆返回（克隆 = 各帧 MetaSlot +1，缓存与 VMA 各自持有）。仅当该
    /// VMA 恰好映满 `npages` 页（每页一帧、连续覆盖、无洞）时返回 Some，
    /// 避免把不完整的段写进全局缓存。
    pub fn vma_frames_if_full(&self, start: VirtAddr, npages: usize) -> Option<Vec<SharedFrame>> {
        let vma = self.find_vma(start)?;
        if vma.start != start || vma.frames.len() != npages {
            return None;
        }
        Some(vma.frames.iter().map(|(_, f)| f.clone()).collect())
    }

    /// 映射区域
    ///
    /// 维持「VMA 集合恒不重叠」不变式（Linux `vma_link`/`find_vma_intersection`）：
    /// ELF 的相邻 PT_LOAD 常常共享同一页（前段尾部与后段头部落在同一页），若为
    /// 两个段各建一条覆盖该页的 VMA，就会出现重叠 VMA——之后缺页/CoW 只会命中
    /// 其中一条，另一条的 `frames` 停留在旧帧，fork 便按旧帧给子进程建映射，
    /// 子进程读到「另一个时刻的内存」。这里对已被覆盖的页不再重复建 VMA，只把
    /// 覆盖它的既有 VMA 权限取并集（该页确实同时属于两个段），数据仍由调用方
    /// 经页表写入，不做 unmap，避免丢失前段已复制的字节。
    pub fn map_region(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        vma_type: VmaType,
        permission: VmaPermission,
    ) {
        let mut cur = start.0;
        while cur < end.0 {
            let covered_end = self
                .vmas
                .iter()
                .find(|v| v.start.0 <= cur && cur < v.end.0)
                .map(|v| v.end.0);
            if let Some(covered_end) = covered_end {
                // 该段起点落在既有 VMA 内：合并权限后跳过这段区间。
                let seg_end = covered_end.min(end.0);
                if let Some(v) = self
                    .vmas
                    .iter_mut()
                    .find(|v| v.start.0 <= cur && cur < v.end.0)
                {
                    let merged = VmaPermission::new(
                        v.permission.read || permission.read,
                        v.permission.write || permission.write,
                        v.permission.execute || permission.execute,
                        v.permission.user || permission.user,
                    );
                    let changed = merged.read != v.permission.read
                        || merged.write != v.permission.write
                        || merged.execute != v.permission.execute
                        || merged.user != v.permission.user;
                    if changed {
                        v.permission = merged;
                        let flags = merged.to_pte_flags();
                        let (vs, ve) = (v.start.0, v.end.0);
                        for vpn_num in (vs >> 12)..((ve + PAGE_SIZE - 1) >> 12) {
                            let vpn = VirtPageNum(vpn_num);
                            if let Some(ppn) = self.page_table.translate(vpn) {
                                self.page_table.remap(vpn, ppn, flags);
                            }
                        }
                        crate::arch::asm_helpers::flush_tlb_all();
                    }
                }
                cur = seg_end;
                continue;
            }
            // 下一个既有 VMA 的起点即本段可独占的上界。
            let next_start = self
                .vmas
                .iter()
                .filter(|v| v.start.0 > cur)
                .map(|v| v.start.0)
                .min()
                .unwrap_or(end.0);
            let seg_end = next_start.min(end.0);
            let mut vma = Vma::new(VirtAddr(cur), VirtAddr(seg_end), vma_type, permission);
            for vpn_num in vma.vpn_range() {
                vma.map_page(VirtPageNum(vpn_num), &mut self.page_table);
            }
            self.raise_mmap_bump_for(&vma);
            self.insert_vma_sorted(Box::new(vma));
            cur = seg_end;
        }
    }

    /// 取消映射区域（支持 VMA 分割）
    ///
    /// 如果 unmap 范围部分覆盖一个 VMA，会将 VMA 拆分为保留部分和移除部分。
    /// Flush dirty pages of every writable MAP_SHARED file mapping overlapping
    /// `[start, end)` back to its backing inode (msync MS_SYNC/MS_ASYNC), then
    /// clear those pages' PTE dirty bits so `/proc/kpageflags` reports them
    /// clean afterwards (msync04: KPF_DIRTY must drop after a successful
    /// `msync(MS_SYNC)`). A later store re-faults and `try_resolve_fault`'s
    /// present-page branch remaps with D set again, so writes still work.
    pub fn writeback_range(&mut self, start: VirtAddr, end: VirtAddr) {
        let mut to_clean: Vec<VirtPageNum> = Vec::new();
        for vma in &self.vmas {
            if start.0 < vma.end.0 && end.0 > vma.start.0 {
                vma.writeback(start, end);
                if vma.file_backed
                    && vma.vma_type == VmaType::Shared
                    && vma.permission.write
                {
                    let s = start.0.max(vma.start.0) & !(PAGE_SIZE - 1);
                    let e = end.0.min(vma.end.0);
                    let mut va = s;
                    while va < e {
                        let vpn = VirtPageNum(va >> 12);
                        if vma.frames.contains_key(&vpn) {
                            to_clean.push(vpn);
                        }
                        va += PAGE_SIZE;
                    }
                }
            }
        }
        let mut cleaned = false;
        for vpn in to_clean {
            if self.page_table.clear_dirty(vpn) {
                cleaned = true;
            }
        }
        if cleaned {
            crate::arch::asm_helpers::flush_tlb_all();
        }
    }

    /// `/proc/kpageflags` 反查：给定物理页帧号 (PFN)，在本地址空间内找到映射它
    /// 的虚拟页并返回其 PTE 脏位（KPF_DIRTY 来源）。未映射该 PFN 返回 `None`。
    /// kpageflags 全局按 PFN 索引，本内核无全局反向映射，故按读取者（当前进程）
    /// 的地址空间扫描其 VMA 帧表；msync04 在自身上下文读取，足以满足语义。
    pub fn pfn_dirty(&self, pfn: usize) -> Option<bool> {
        for vma in &self.vmas {
            for (vpn, frame) in vma.frames.iter() {
                if frame.ppn.0 == pfn {
                    return self.page_table.pte_dirty(*vpn);
                }
            }
        }
        None
    }

    /// Decide which signal an *unhandled* user page fault at `va` should raise:
    /// SIGBUS (7) when `va` lies inside a file-backed mapping but on a page
    /// entirely past the file's last page (mmap13), otherwise SIGSEGV (11).
    pub fn fault_signal(&self, va: VirtAddr) -> i32 {
        for vma in &self.vmas {
            if va >= vma.start && va < vma.end {
                if vma.is_file_page_beyond_eof(va) {
                    return 7;
                }
                break;
            }
        }
        11
    }

    /// Reload file-backed mapping pages from their backing inode (msync
    /// MS_INVALIDATE) so changes made to the file out-of-band are visible.
    pub fn invalidate_range(&self, start: VirtAddr, end: VirtAddr) {
        for vma in &self.vmas {
            if start.0 < vma.end.0 && end.0 > vma.start.0 {
                vma.invalidate(start, end);
            }
        }
    }

    /// 批量回收 lazy-TLB quarantine。16K 普通页≈64MiB；8G BuildStorm 下足够
    /// 大以摊薄全核 drain，又足够小以避免长进程无限吃内存。
    fn maybe_reap_retired_frames(&mut self) {
        const RETIRED_FRAME_BATCH: usize = 16 * 1024;
        if self.retired_frames.len() < RETIRED_FRAME_BATCH {
            return;
        }
        crate::mm::tlb::drain_shootdown();
        self.retired_frames.clear();
    }

    /// 无条件回收 quarantine 帧。**调用方必须已执行 drain_shootdown**（或确认
    /// 无跨核陈旧 TLB 风险）——否则帧可能经陈旧 TLB 被其它 hart 命中后归还 buddy。
    pub fn reap_retired_frames(&mut self) {
        self.retired_frames.clear();
    }

    /// CoW 等路径摘除帧后的复用前强序（供 trap 层在**全部锁释放后**调用）：
    /// 先同步等所有对端完成挂起的 TLB 失效，再回收本进程 quarantine 的帧
    /// （此时归还 buddy 才是安全的——任何 hart 都不再可能经陈旧 TLB 命中它们）。
    pub fn drain_and_reap_current_mm() {
        crate::mm::tlb::drain_shootdown();
        let mm = {
            let guard = crate::sched::process::PROCESS_TABLE.lock();
            let t = match guard.as_ref() {
                Some(t) => t,
                None => return,
            };
            let pid = match t.current {
                Some(p) => p,
                None => return,
            };
            let as_pid = t
                .get(pid)
                .map(|p| if p.mm.is_some() { pid } else { p.tgid })
                .unwrap_or(pid);
            match t.get(as_pid).and_then(|p| p.mm.as_ref()) {
                Some(m) => m.clone(),
                None => return,
            }
        };
        mm.lock().retired_frames.clear();
    }

    pub fn unmap_region(&mut self, start: VirtAddr, end: VirtAddr) -> UnmapStats {
        let mut stats = UnmapStats::default();
        vmaop_trace("unmap", start.0, end.0, None);
        if start >= end {
            return stats;
        }
        let mut new_vmas = Vec::with_capacity(self.vmas.len() + 1);
        let old_vmas = core::mem::take(&mut self.vmas);
        let mut any_huge = false;
        // mmu_gather 语义（对标 Linux tlb_finish_mmu）：先摘 PTE、收集被丢弃的
        // 中间段，**在 flush + drain_shootdown 之后再统一 drop 它们的 FrameTracker**
        // 归还 buddy。否则帧先于跨核失效完成回到 buddy 被下一进程复用，而对端仍
        // 持陈旧 TLB → 写坏新进程内存（BuildStorm rustc 间歇 SIGSEGV 根因）。
        let mut dropped: Vec<Box<Vma>> = Vec::new();

        for mut vma in old_vmas {
            if end <= vma.start || start >= vma.end {
                // 完全不在范围内，保留
                new_vmas.push(vma);
                continue;
            }

            // vfork 子栈取证：谁在删除 Stack 类 VMA（BSCRASH 门控，窄条件）。
            if crate::cmdline::bscrash_enabled() && vma.vma_type == VmaType::Stack {
                let cur = crate::sched::process_sched::HART_CURRENT_PID
                    .load(core::sync::atomic::Ordering::Relaxed);
                crate::println!(
                    "[UNMAPDBG] pid={} unmap=[{:#x},{:#x}) hits Stack vma=[{:#x},{:#x})",
                    cur, start.0, end.0, vma.start.0, vma.end.0
                );
            }

            // VMA 与 unmap 范围有重叠
            any_huge |= vma.huge_page_size != 0;
            let vma_start = vma.start;
            let vma_end = vma.end;
            let vma_perm = vma.permission;
            let overlap_start = if start > vma_start { start } else { vma_start };
            let overlap_end = if end < vma_end { end } else { vma_end };

            // 拆出左段 [vma.start, overlap_start)（如果有）
            if overlap_start > vma_start {
                let left = Self::extract_segment(&mut vma, vma_start, overlap_start, vma_perm);
                new_vmas.push(Box::new(left));
            }

            // 拆出右段 [overlap_end, vma.end)（如果有）
            if overlap_end < vma_end {
                let right = Self::extract_segment(&mut vma, overlap_end, vma_end, vma_perm);
                new_vmas.push(Box::new(right));
            }

            // Track hugetlb pages released for the middle (dropped) span: both
            // the total span and how many huge pages were actually instantiated
            // (touched), so the caller can return the reservation per the Linux
            // accounting model (surplus first to buddy, else back to free).
            //
            // MAP_SHARED hugetlbfs file mappings are *inode-owned*: their pages
            // and reservation are released only when the file is truncated or the
            // inode is evicted (ftruncate / last close), NOT on munmap. So they
            // are deliberately excluded here — `crate::mm::hugetlb_shared_*`
            // owns their accounting (hugemmap08/10/23). Anonymous MAP_HUGETLB and
            // MAP_PRIVATE file huge mappings stay VMA-driven.
            let huge_inode_owned =
                vma.vma_type == VmaType::Shared && vma.backing.is_some();
            if vma.huge_page_size != 0 && !huge_inode_owned {
                let span_bytes = overlap_end.0 - overlap_start.0;
                stats.hugetlb_bytes_released = stats
                    .hugetlb_bytes_released
                    .saturating_add(span_bytes);
                stats.huge_page_size = vma.huge_page_size;
                // fork 后父子 VMA 共持同一帧：只有最后一个持有者（meta_ref == 1）
                // 才把已实例化页计入归还，否则同一物理大页被多个进程重复
                // free++，池账本出现 Free > Total。
                let touched = vma
                    .huge_frames
                    .iter()
                    .filter(|(base, frame)| {
                        let va = base.0 * PAGE_SIZE;
                        va >= overlap_start.0
                            && va < overlap_end.0
                            && frame.meta_ref() == 1
                    })
                    .count();
                stats.huge_touched_pages = stats.huge_touched_pages.saturating_add(touched);
                stats.huge_reserved = vma.huge_reserved;
            }

            // Decrement writable shared mmap count for F_SEAL_WRITE tracking
            if vma.vma_type == VmaType::Shared && vma.file_backed && vma.permission.write {
                if let Some(ref backing) = vma.backing {
                    backing.dec_writable_maps();
                }
            }

            // 中间重叠部分：先把脏的共享文件页写回 backing inode（munmap 隐式
            // 触发 MAP_SHARED 写回，msync02 再次映射时数据必须仍在文件里），
            // 然后再 unmap 页面并释放 frame。
            vma.writeback(overlap_start, overlap_end);

            // 中间重叠部分：unmap 页面。extract 左右段后，`vma.frames` 即中间
            // （被丢弃）段的实际映射页——只遍历它，而不是逐 VA 扫整个声明范围
            // （glibc 8MiB 线程栈仅 commit 数页，旧版退化为 O(声明范围)，是
            // libc-bench pthread create/join 簇热点，§1.13① 消除热路径 O(N)）。
            let unmap_start_vpn = overlap_start.floor().0;
            let unmap_end_vpn = overlap_end.ceil().0;
            let mapped: Vec<VirtPageNum> = vma.frames.keys().copied().collect();
            for vpn in mapped {
                if self.page_table.translate(vpn).is_some() {
                    self.page_table.unmap(vpn);
                }
                self.lazy_free.remove(&vpn);
            }
            // SysV SHM attaches map raw PTEs whose physical pages are owned by
            // SHM_TABLE, so the VMA carries no `frames` — walk the overlap
            // range and clear those PTEs too, or a write after shmdt would
            // still hit the stale mapping (shmdt01). Only reached for a
            // frame-less Shared anon VMA, so it never regresses the O(frames)
            // fast path above.
            if vma.frames.is_empty()
                && vma.huge_frames.is_empty()
                && vma.vma_type == VmaType::Shared
                && vma.backing.is_none()
            {
                for vpn_num in unmap_start_vpn..unmap_end_vpn {
                    let vpn = VirtPageNum(vpn_num);
                    if self.page_table.translate(vpn).is_some() {
                        self.page_table.unmap(vpn);
                    }
                    self.lazy_free.remove(&vpn);
                }
            }
            // 换出页（不在 frames 中）：仅当存在换出页时才按范围回收。
            if self.swapped_pages > 0 {
                for vpn_num in unmap_start_vpn..unmap_end_vpn {
                    let vpn = VirtPageNum(vpn_num);
                    if let Some(entry) = self.page_table.get_swap(vpn) {
                        super::swap::free_slot(super::swap::swp_offset(entry));
                        self.page_table.clear_swap(vpn);
                        self.swapped_pages = self.swapped_pages.saturating_sub(1);
                    }
                }
            }
            // 剩余的 vma (覆盖区) 暂存，等 flush + drain 后再释放其 frames。
            dropped.push(vma);
        }

        // 刷新 TLB，确保后续映射使用新的页表条目。小范围（如 musl 线程栈
        // munmap，pthread create/join 热路径）按页在当前 ASID 内精确失效，
        // 免得整机 TLB 全清后其余映射逐条重走页表（QEMU softmmu 下全清
        // 代价极高）。unmap 的调用方都在操作调用者自身的地址空间，故当前
        // CSR ASID 即所属 ASID（与 mprotect/protect_region 同款依据）；唯一
        // 的跨进程调用方是 hugetlb 共享截断，huge 映射一律走全量刷兜底。
        if !crate::DIAG_SKIP_UNMAP_FLUSH {
            use crate::mm::PAGE_SIZE;
            let pages = (end.ceil().0).saturating_sub(start.floor().0);
            if pages <= 64 && !any_huge {
                let e = end.ceil().0 * PAGE_SIZE;
                crate::arch::asm_helpers::flush_tlb_range_current_asid(start.0, e);
            } else {
                crate::arch::asm_helpers::flush_tlb_all();
            }
        }

        // 把已摘 PTE 的中间段帧转入本 mm 的 lazy-TLB quarantine，暂不归 buddy。
        // 对端若尚有陈旧 TLB，命中的仍是本 mm 自己的旧帧，不会踩新用途。
        for mut vma in dropped {
            self.retired_frames.extend(
                vma.frames
                    .remove_range(VirtPageNum(0)..VirtPageNum(usize::MAX))
                    .into_iter()
                    .map(|(_, frame)| frame),
            );
            self.retired_frames.extend(
                vma.huge_frames
                    .remove_range(VirtPageNum(0)..VirtPageNum(usize::MAX))
                    .into_iter()
                    .map(|(_, frame)| frame),
            );
        }
        self.maybe_reap_retired_frames();

        self.vmas = new_vmas;
        stats
    }

    /// Set (`on=true`, `MADV_WIPEONFORK`) or clear (`on=false`,
    /// `MADV_KEEPONFORK`) the wipe-on-fork attribute over `[start, end)`,
    /// splitting any partially-covered VMA so the flag is page-accurate. Does
    /// not touch page tables — the wipe only takes effect at the next fork.
    pub fn set_wipeonfork_range(&mut self, start: VirtAddr, end: VirtAddr, on: bool) {
        if start >= end {
            return;
        }
        let mut new_vmas = Vec::with_capacity(self.vmas.len() + 2);
        let old_vmas = core::mem::take(&mut self.vmas);
        for mut vma in old_vmas {
            if end <= vma.start || start >= vma.end {
                new_vmas.push(vma);
                continue;
            }
            let vma_start = vma.start;
            let vma_end = vma.end;
            let vma_perm = vma.permission;
            let overlap_start = if start > vma_start { start } else { vma_start };
            let overlap_end = if end < vma_end { end } else { vma_end };
            // Left part outside the range keeps its current flag.
            if overlap_start > vma_start {
                let left = Self::extract_segment(&mut vma, vma_start, overlap_start, vma_perm);
                new_vmas.push(Box::new(left));
            }
            // Right part outside the range keeps its current flag.
            if overlap_end < vma_end {
                let right = Self::extract_segment(&mut vma, overlap_end, vma_end, vma_perm);
                new_vmas.push(Box::new(right));
            }
            // The remaining `vma` now holds exactly the overlap (its outer
            // frames were moved into the split-off segments above); set the
            // flag on it and keep it.
            vma.start = overlap_start;
            vma.end = overlap_end;
            vma.wipeonfork = on;
            new_vmas.push(vma);
        }
        self.vmas = new_vmas;
    }

    /// Set (`on=true`, `MADV_DONTDUMP`) or clear (`on=false`, `MADV_DODUMP`) the
    /// exclude-from-coredump attribute over `[start, end)`, splitting any
    /// partially-covered VMA so the flag is page-accurate. (LTP `madvise08`.)
    pub fn set_dontdump_range(&mut self, start: VirtAddr, end: VirtAddr, on: bool) {
        if start >= end {
            return;
        }
        let mut new_vmas = Vec::with_capacity(self.vmas.len() + 2);
        let old_vmas = core::mem::take(&mut self.vmas);
        for mut vma in old_vmas {
            if end <= vma.start || start >= vma.end {
                new_vmas.push(vma);
                continue;
            }
            let vma_start = vma.start;
            let vma_end = vma.end;
            let vma_perm = vma.permission;
            let overlap_start = if start > vma_start { start } else { vma_start };
            let overlap_end = if end < vma_end { end } else { vma_end };
            if overlap_start > vma_start {
                let left = Self::extract_segment(&mut vma, vma_start, overlap_start, vma_perm);
                new_vmas.push(Box::new(left));
            }
            if overlap_end < vma_end {
                let right = Self::extract_segment(&mut vma, overlap_end, vma_end, vma_perm);
                new_vmas.push(Box::new(right));
            }
            vma.start = overlap_start;
            vma.end = overlap_end;
            vma.dontdump = on;
            new_vmas.push(vma);
        }
        self.vmas = new_vmas;
    }

    /// `remap_file_pages(2)` (legacy, emulated like modern Linux): re-point the
    /// file pages backing `[start, end)` of a single `MAP_SHARED` file mapping
    /// so the page at `start` maps file page `new_file_offset / PAGE_SIZE`, the
    /// next maps the following file page, and so on (a linear remap to a new
    /// pgoff — sufficient for the non-linear API's surviving callers since
    /// 3.16). Splits the covering VMA so a sub-VMA spans exactly `[start, end)`,
    /// retargets its `file_offset`, then drops and re-faults the pages from the
    /// shared page cache at the new offset. Returns 0 on success or `-errno`.
    pub fn remap_file_pages_range(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        new_file_offset: usize,
    ) -> isize {
        if start >= end {
            return -22; // -EINVAL
        }
        // The whole range must lie within one VMA that is a MAP_SHARED file
        // mapping with a backing inode (Linux: VM_SHARED && vma->vm_file).
        let (inode, perm) = {
            let vma = match self.vmas.iter().find(|v| start >= v.start && end <= v.end) {
                Some(v) => v,
                None => return -22,
            };
            if vma.vma_type != VmaType::Shared || !vma.file_backed {
                return -22;
            }
            match vma.backing {
                Some(ref i) => (i.clone(), vma.permission),
                None => return -22,
            }
        };

        // Split so a sub-VMA covers exactly [start, end) and carries the new
        // file offset. The split-off left/right halves keep their original
        // linear file offsets (extract_segment shifts by the start delta).
        {
            let mut new_vmas = Vec::with_capacity(self.vmas.len() + 2);
            let old_vmas = core::mem::take(&mut self.vmas);
            for mut vma in old_vmas {
                if end <= vma.start || start >= vma.end {
                    new_vmas.push(vma);
                    continue;
                }
                let vma_start = vma.start;
                let vma_end = vma.end;
                let vma_perm = vma.permission;
                let overlap_start = if start > vma_start { start } else { vma_start };
                let overlap_end = if end < vma_end { end } else { vma_end };
                if overlap_start > vma_start {
                    let left = Self::extract_segment(&mut vma, vma_start, overlap_start, vma_perm);
                    new_vmas.push(Box::new(left));
                }
                if overlap_end < vma_end {
                    let right = Self::extract_segment(&mut vma, overlap_end, vma_end, vma_perm);
                    new_vmas.push(Box::new(right));
                }
                vma.start = overlap_start;
                vma.end = overlap_end;
                vma.file_offset = new_file_offset;
                new_vmas.push(vma);
            }
            self.vmas = new_vmas;
        }

        // Re-fault each page from the shared page cache at the new offset.
        let inode_id = inode.ino();
        let flags = perm.to_pte_flags();
        let start_vpn = start.floor().0;
        let end_vpn = end.ceil().0;
        let mut mapped: Vec<(VirtPageNum, SharedFrame)> = Vec::new();
        for vpn_num in start_vpn..end_vpn {
            let vpn = VirtPageNum(vpn_num);
            if self.page_table.translate(vpn).is_some() {
                self.page_table.unmap(vpn);
            }
            let page_index = (new_file_offset + (vpn_num - start_vpn) * PAGE_SIZE) / PAGE_SIZE;
            // 填充在共享缓存锁内完成（竞态根因见 shared_cache_get_or_insert_with）。
            let frame = match shared_cache_get_or_insert_with(inode_id, page_index, |kva| {
                let Some(file) = inode.as_file() else {
                    return false;
                };
                let file_off = page_index * PAGE_SIZE;
                let Ok(st) = file.stat() else {
                    return false; // 拿不到文件大小，不能发布零页
                };
                if file_off < st.size {
                    let avail = core::cmp::min(PAGE_SIZE, st.size - file_off);
                    let mut total = 0usize;
                    while total < avail {
                        match file.read(file_off + total, &mut kva[total..avail]) {
                            Ok(0) => break,
                            Ok(n) => total += n,
                            Err(_) => break,
                        }
                    }
                    // 短读/读失败：零尾页不能进共享缓存，更不能进 PTE。
                    total == avail
                } else {
                    true // 末页尾部的零填充部分
                }
            }) {
                Some(f) => f,
                None => return -5, // -EIO（读失败）或 -ENOMEM 均以失败返回
            };
            let ppn = frame.ppn;
            self.page_table.map(vpn, ppn, flags);
            mapped.push((vpn, frame));
        }

        // Record the frames in the sub-VMA so they stay alive and flush on
        // msync(2)/munmap(2).
        if let Some(vma) = self.vmas.iter_mut().find(|v| v.start == start && v.end == end) {
            for (vpn, frame) in mapped {
                vma.insert_shared_frame(vpn, frame);
            }
        }
        0
    }

    /// `MADV_DONTNEED` / `MADV_FREE` / `MADV_REMOVE` 真页帧释放。
    ///
    /// Linux 语义（man madvise）：
    /// * **MADV_DONTNEED**：unmap 对应 PTE + drop 物理页帧 *引用*，**保留 VMA**。
    ///   下次访问该范围会重新 page fault：匿名映射会拿到清零新页，文件映射
    ///   会重新从 backing inode 读。当前 VMA 标志（perm/cow/type）不变。
    /// * **MADV_FREE**：Linux 中是 lazy free（标 PTE 为 dirty=0，内存压力时回收）。
    ///   我们没 reclaim 子系统，按 DONTNEED 立刻释放等价（POSIX 容许）。
    /// * **MADV_REMOVE**：file-backed 时 punch hole（释放 backing storage 字节
    ///   并清零）；匿名映射等同 DONTNEED。我们 ramfs 没真 hole 概念，
    ///   按 DONTNEED 处理（数据下次读为 0，与 punch hole 在用户视角等价）。
    ///
    /// 返回真释放的 frame 数（≥0）。范围内未映射的页静默跳过 — 与 Linux
    /// `madvise_dontneed_single_vma` 行为一致。
    /// —— 见下方 `madvise_dontneed`。
    ///
    /// MADV_REMOVE 的 hole punch：对范围内 MAP_SHARED 文件映射，把 backing
    /// 文件对应字节清零并驱逐 SHARED_PAGE_CACHE，使后续缺页读到 hole（全零）。
    /// 对标 Linux `madvise_remove` → `vfs_fallocate(PUNCH_HOLE|KEEP_SIZE)`。
    /// 匿名/私有映射不在此处理（调用方随后统一走 DONTNEED）。
    pub fn punch_hole_file_range(&mut self, start: VirtAddr, end: VirtAddr) {
        if start >= end { return; }
        for vma in self.vmas.iter() {
            if end <= vma.start || start >= vma.end { continue; }
            if vma.vma_type != VmaType::Shared { continue; }
            let Some(ref inode) = vma.backing else { continue };
            let Some(file) = inode.as_file() else { continue };
            let s = if start > vma.start { start } else { vma.start };
            let e = if end < vma.end { end } else { vma.end };
            let off_s = vma.file_offset + (s.0 - vma.start.0);
            let off_e = vma.file_offset + (e.0 - vma.start.0);
            let fsize = file.stat().map(|st| st.size as usize).unwrap_or(0);
            let zero_end = off_e.min(fsize);
            const ZERO_CHUNK: usize = 4096;
            let zeros = [0u8; ZERO_CHUNK];
            let mut cur = off_s;
            while cur < zero_end {
                let chunk = (zero_end - cur).min(ZERO_CHUNK);
                match file.write(cur, &zeros[..chunk]) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => cur += n,
                }
            }
            let inode_id = inode.ino();
            shared_cache_evict_range(
                inode_id,
                off_s / PAGE_SIZE,
                (off_e + PAGE_SIZE - 1) / PAGE_SIZE,
            );
        }
    }

    pub fn madvise_dontneed(&mut self, start: VirtAddr, end: VirtAddr) -> usize {
        if start >= end { return 0; }
        let mut released = 0usize;
        // mmu_gather：摘出的 frame 暂存，flush + drain_shootdown 后再统一 drop
        // 归还 buddy（复用前强序，见 unmap_region）。
        let mut gathered: Vec<SharedFrame> = Vec::new();
        // 仅对范围内「已 map」的页执行 unmap + frame drop；VMA 结构保留。
        // 关键：按 `vma.frames`（实际映射的稀疏页，BTreeMap range）迭代，而不是
        // 逐 VA 遍历整个声明范围——glibc 线程栈声明 8MiB（2048 页）却仅 commit
        // 数页，旧版每次 madvise(MADV_DONTNEED) 都要空扫 2048 个 VPN（translate +
        // 两次 BTree 查找），使其退化为 O(声明范围)，是 libc-bench pthread
        // create/join 簇的首要热点（§1.13① 消除热路径 O(N)）。
        for vma in self.vmas.iter_mut() {
            if end <= vma.start || start >= vma.end { continue; }
            let overlap_s = if start > vma.start { start } else { vma.start };
            let overlap_e = if end < vma.end { end } else { vma.end };
            let vpn_s = overlap_s.floor();
            let vpn_e = overlap_e.ceil();
            // 单次 drain 摘除区间（排序 Vec 中连续），避免逐 vpn remove 的
            // O(页数 × 表长) 重复挪移（大堆 VMA 上万条目）。
            // mmu_gather：摘出的 frame 暂存 gathered，flush + drain_shootdown 后
            // 再统一 drop 归还 buddy（复用前强序，见 unmap_region 同款注释）。
            for (vpn, frame) in vma.frames.remove_range(vpn_s..vpn_e) {
                if self.page_table.translate(vpn).is_some() {
                    self.page_table.unmap(vpn);
                }
                self.lazy_free.remove(&vpn);
                released += 1;
                gathered.push(frame);
            }
        }
        // 换出页（不在 frames 中）：仅当确实存在换出页时才按范围回收，避免
        // 常见（无 swap）路径退化回 O(声明范围)。
        if self.swapped_pages > 0 {
            let vpn_s = start.floor().0;
            let vpn_e = end.ceil().0;
            for vpn_num in vpn_s..vpn_e {
                let vpn = VirtPageNum(vpn_num);
                if let Some(entry) = self.page_table.get_swap(vpn) {
                    // 换出页：DONTNEED 丢弃换出内容（下次访问重新缺页拿清零页）。
                    super::swap::free_slot(super::swap::swp_offset(entry));
                    self.page_table.clear_swap(vpn);
                    self.swapped_pages = self.swapped_pages.saturating_sub(1);
                    released += 1;
                }
            }
        }
        // 兜底清理范围内残留的 lazy_free 标记（MADV_FREE 候选页若已被回收则不在
        // frames 中）；lazy_free 通常为空，仅在非空时付出 O(范围内标记数)。
        if !self.lazy_free.is_empty() {
            let lf: alloc::vec::Vec<VirtPageNum> =
                self.lazy_free.range(start.floor()..end.ceil()).copied().collect();
            for vpn in lf {
                self.lazy_free.remove(&vpn);
            }
        }
        crate::arch::asm_helpers::flush_tlb_all();
        // 转入 lazy-TLB quarantine，批量 drain 后才归 buddy（同 unmap_region）。
        self.retired_frames.extend(gathered);
        self.maybe_reap_retired_frames();
        released
    }

    /// 修改指定地址范围的权限。
    ///
    /// 当前实现支持把一个 VMA 拆分为最多三段，覆盖 pthread 栈保护页这类
    /// `mmap(PROT_NONE) + mprotect(subrange, PROT_READ|PROT_WRITE)` 场景。
    pub fn protect_region(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        permission: VmaPermission,
    ) -> bool {
        self.protect_region_pkey(start, end, permission, None).is_some()
    }

    /// mprotect / pkey_mprotect 共用体。`pkey = Some(k)` 把区间内 VMA 改挂到
    /// key k；`None`（普通 mprotect）保留各 VMA 现有 key（Linux
    /// do_mprotect_pkey(pkey=-1) 语义）。落盘到 PTE 的是“有效权限”：
    /// prot 扣除 key 的拒绝位（DISABLE_ACCESS 去 R+W，DISABLE_WRITE 去 W），
    /// 使被 key 拒绝的访问走普通权限缺页 → SIGSEGV（软件仿真 x86 PKU）。
    /// 返回 `None` = 区间未映射（ENOMEM）；`Some(reduced)` 中 `reduced`
    /// 表示是否有任一 VMA 的权限被**收紧**（旧有 R/W/X 而新无）——只有
    /// 收紧才需同步等待跨 hart shootdown；纯放宽/无变化时陈旧 TLB 只会
    /// 多一次缺页，不破坏安全（Linux change_protection 同义）。
    pub fn protect_region_pkey(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        permission: VmaPermission,
        pkey: Option<i32>,
    ) -> Option<bool> {
        vmaop_trace("protect", start.0, end.0, None);
        if start >= end {
            return None;
        }
        let rights = self.pkey_rights;
        let effective = |key: i32| -> VmaPermission {
            let mut p = permission;
            if (1..16).contains(&key) {
                let r = rights[key as usize];
                if r & 0x1 != 0 {
                    // PKEY_DISABLE_ACCESS：禁止一切数据访问（读+写），不禁取指
                    p.read = false;
                    p.write = false;
                }
                if r & 0x2 != 0 {
                    p.write = false;
                }
            }
            p
        };
        if Self::h23dbg() {
            crate::println!("[h23] protect_region start={:#x} end={:#x} perm(r={} w={} x={})",
                start.0, end.0, permission.read, permission.write, permission.execute);
        }

        // 原地修改：旧实现每次 mprotect 都 take + 重建整个 vmas Vec（逐个 move
        // 大 Vma 结构体 → memcpy），VMA 多时是 libc-bench pthread 簇的首要
        // memcpy 热点（musl pthread_create 每次建栈都 mprotect guard）。这里
        // 只对真正重叠的 VMA 操作：完整覆盖走免拷贝快路径，需切分时才
        // remove+insert（Vec::insert 保持原相对顺序，与旧版排序结果一致）。
        // vmas 有序且互不重叠 ⇒ 按 end 也有序：二分跳到首个可能重叠的 VMA，
        // 一旦越过 [start, end) 即提前终止，无需全扫。
        let mut changed = false;
        let mut perms_modified = false;
        let mut reduced = false;
        let reduces = |old: VmaPermission, new: VmaPermission| -> bool {
            (old.read && !new.read) || (old.write && !new.write) || (old.execute && !new.execute)
        };
        let mut i = self.vmas.partition_point(|v| v.end <= start);
        while i < self.vmas.len() {
            let (vs, ve) = (self.vmas[i].start, self.vmas[i].end);
            if vs >= end {
                break;
            }
            if end <= vs || start >= ve {
                i += 1;
                continue;
            }
            changed = true;
            let middle_start = if start > vs { start } else { vs };
            let middle_end = if end < ve { end } else { ve };

            let key = pkey.unwrap_or(self.vmas[i].pkey);
            let eff = effective(key);

            if middle_start == vs && middle_end == ve {
                // 完整覆盖：仅改权限位并回刷 PTE，不动 Vec。cow 标记保持
                // （共享帧的写断开不因权限往返而失效）。
                // 权限+key 均无变化时跳过 PTE 回刷与 TLB 失效（幂等
                // mprotect，如分配器反复对同段设相同保护）。
                if self.vmas[i].pkey == key && self.vmas[i].permission == eff {
                    i += 1;
                    continue;
                }
                reduced |= reduces(self.vmas[i].permission, eff);
                perms_modified = true;
                self.vmas[i].pkey = key;
                self.vmas[i].permission = eff;
                Self::apply_segment_permissions(&mut self.page_table, &self.vmas[i]);
                i += 1;
                continue;
            }
            reduced |= reduces(self.vmas[i].permission, eff);
            perms_modified = true;

            let mut vma = self.vmas.remove(i);
            let original_start = vma.start;
            let original_end = vma.end;
            let original_permission = vma.permission;
            let mut insert_at = i;

            if middle_start > original_start {
                let left = Self::extract_segment(&mut vma, original_start, middle_start, original_permission);
                Self::apply_segment_permissions(&mut self.page_table, &left);
                self.vmas.insert(insert_at, Box::new(left));
                insert_at += 1;
            }

            let mut middle = Self::extract_segment(&mut vma, middle_start, middle_end, eff);
            middle.pkey = key;
            Self::apply_segment_permissions(&mut self.page_table, &middle);
            self.vmas.insert(insert_at, Box::new(middle));
            insert_at += 1;

            if middle_end < original_end {
                let right = Self::extract_segment(&mut vma, middle_end, original_end, original_permission);
                Self::apply_segment_permissions(&mut self.page_table, &right);
                self.vmas.insert(insert_at, Box::new(right));
                insert_at += 1;
            }
            i = insert_at;
        }

        if !changed {
            return None;
        }
        if !perms_modified {
            // 全部 VMA 权限无变化：无 PTE 改动，无需任何 TLB 失效。
            return Some(false);
        }

        // Only `[start, end)` had its permissions (PTEs) changed — the left/right
        // remainders keep their original perms. Scope the TLB invalidation to that
        // range (in the *current* address space's ASID) instead of nuking the whole
        // TLB on every mprotect. musl `pthread_create` mprotects the stack guard on
        // every thread spawn; a full flush there evicts every other still-valid
        // entry and forces the next accesses to re-walk the page tables. mprotect
        // only ever modifies the calling process's own address space, so the live
        // CSR ASID is the owning ASID (RV: `sfence.vma va`; LA: `invtlb 0x6,asid,va`,
        // matching the page-fault / context-switch paths). Cap the per-page loop and
        // fall back to a single full flush for very large ranges.
        {
            use crate::mm::PAGE_SIZE;
            let pages = (end.0 - start.0) / PAGE_SIZE;
            if pages <= 64 {
                crate::arch::asm_helpers::flush_tlb_range_current_asid(start.0, end.0);
            } else {
                crate::arch::asm_helpers::flush_tlb_all();
            }
        }

        Some(reduced)
    }

    /// 查找包含地址的 VMA（vmas 按 start 有序 → 二分）
    pub fn find_vma(&self, va: VirtAddr) -> Option<&Vma> {
        let idx = self.vmas.partition_point(|vma| vma.start <= va);
        let vma = self.vmas.get(idx.checked_sub(1)?)?;
        if va < vma.end { Some(&**vma) } else { None }
    }

    /// 查找包含地址的可变 VMA（vmas 按 start 有序 → 二分）
    pub fn find_vma_mut(&mut self, va: VirtAddr) -> Option<&mut Vma> {
        let idx = self.vmas.partition_point(|vma| vma.start <= va);
        let vma = self.vmas.get_mut(idx.checked_sub(1)?)?;
        if va < vma.end { Some(&mut **vma) } else { None }
    }

    /// read(2) 零拷贝快路径：把文件数据页帧直接别名映射进用户页表（只读 +
    /// VMA 级 COW），替代 4 KiB memcpy。用户随后写该页会触发写缺页 →
    /// handle_cow 按 Arc 引用计数复制/升级，read 快照语义不破。
    /// 仅对私有匿名类 VMA 安全（共享映射替换帧会破坏共享语义）；不满足
    /// 条件返回 false，调用方退回逐字节拷贝。
    pub fn alias_map_user_page(&mut self, va: VirtAddr, frame: SharedFrame) -> bool {
        self.alias_map_user_range(va, core::slice::from_ref(&frame)) == 1
    }

    /// 稳态重读零分配短路：从 `va` 起连续页与 `ptrs` 逐帧指针相等的
    /// 前缀长度（帧已别名在位 → PTE/TLB/frames 均无需动）。调用方只需
    /// 对剩余尾部构造 `Vec<Arc>` 走 `alias_map_user_range`。
    pub fn alias_steady_prefix(
        &self,
        va: VirtAddr,
        ptrs: impl Iterator<Item = *const FrameTracker>,
    ) -> usize {
        let idx = self.vmas.partition_point(|vma| vma.start <= va);
        let Some(i) = idx.checked_sub(1) else { return 0 };
        let Some(vma) = self.vmas.get(i) else { return 0 };
        if va >= vma.end || !vma.alias_map_eligible() {
            return 0;
        }
        let end_vpn = vma.end.ceil().0;
        let base = va.floor();
        let cap = end_vpn.saturating_sub(base.0);
        vma.frames.ptr_eq_prefix_ptrs(base, ptrs.take(cap))
    }

    /// 批量版：把 `frames` 依次别名映射到从 `va`（页对齐）起的连续用户页。
    /// 返回成功映射的页数（前缀）。一次 VMA 查找摊薄到整个 range；
    /// 稳态（同帧已映射，is_same_arc）逐页 O(BTreeMap get) 短路，
    /// 不走页表也不 flush TLB。
    pub fn alias_map_user_range(
        &mut self,
        va: VirtAddr,
        frames: &[SharedFrame],
    ) -> usize {
        self.alias_map_user_iter(va, frames.iter())
    }

    /// 同 `alias_map_user_range`，但接受 `&SharedFrame` 迭代器 —— 调用方（ramfs
    /// read_to_user）直接从页缓存区间迭代，免预构造 `Vec`（每读
    /// 16 页×克隆/析构 + 堆分配是稳态滑窗重读的可观测开销）。
    pub fn alias_map_user_iter<'a>(
        &mut self,
        va: VirtAddr,
        frames: impl Iterator<Item = &'a SharedFrame>,
    ) -> usize {
        let idx = self.vmas.partition_point(|vma| vma.start <= va);
        let Some(i) = idx.checked_sub(1) else { return 0 };
        if self.vmas.get(i).map_or(true, |v| va >= v.end) {
            return 0;
        }
        let vma = &mut self.vmas[i];
        if !vma.alias_map_eligible() {
            return 0;
        }
        let end_vpn = vma.end.ceil().0;
        let mut flags = PTEFlags::V | PTEFlags::A | PTEFlags::R | PTEFlags::U;
        if vma.permission.execute {
            flags |= PTEFlags::X;
        }
        let base_vpn = va.floor().0;
        let pt_root = self.page_table.root_ppn().0;
        let mut done = 0;
        // 升序批量：FrameMap 一次二分后游标推进（稳态同帧页由游标处
        // ptr_eq 直接短路）；页表按 512 页块缓存 L0 表（bw_file_rd 滑窗
        // 重读 = 每 read 全 16 页换帧，逐页 translate+remap 双走表与逐页
        // 二分是首要热点）。
        let mut cur = vma.frames.lower_bound(&VirtPageNum(base_vpn));
        let mut blk_cache: (usize, Result<crate::mm::PhysPageNum, ()>) = (usize::MAX, Err(()));
        // 换帧页的 TLB 失效批量化：记区间、循环后一次性失效。判题 QEMU
        // （10.0.2）LA 任何单页 invtlb 都清空整个 softmmu TLB，滑窗重读每
        // read 换 16 帧 = 16 次全清；改用 flush_tlb_range_current_asid（LA
        // 多页单次 invtlb 0x4 按 ASID 清）压到 1 次。本函数只被 read 路径
        // 对调用者自身地址空间使用，满足 current-ASID 约束。
        let mut inv_lo = usize::MAX;
        let mut inv_hi = 0usize;
        for (k, frame) in frames.enumerate() {
            let vpn = VirtPageNum(base_vpn + k);
            if vpn.0 >= end_vpn {
                break;
            }
            // 稳态短路：该页已别名到同一帧，PTE/TLB 都无需动。
            if let Some(existing) = vma.frames.at_key(cur, vpn) {
                if existing.is_same_arc(frame) {
                    cur += 1;
                    done += 1;
                    continue;
                }
            }
            let pt = &mut self.page_table;
            let block = vpn.0 >> 9;
            if blk_cache.0 != block {
                blk_cache = (
                    block,
                    match pt.l0_table_lookup(vpn) {
                        Ok(Some(l0)) => Ok(l0),
                        Ok(None) => pt.l0_table_create(vpn).ok_or(()),
                        Err(()) => Err(()), // 大页叶子途中：回退逐页慢路径
                    },
                );
            }
            match blk_cache.1 {
                Ok(l0) => {
                    let pte = &mut l0.get_pte_array()[vpn.0 & 0x1ff];
                    if pte.is_valid() {
                        if pte.ppn() != frame.ppn {
                            pt.replace_leaf_pte(pte, frame.ppn, flags);
                            pte_site::record(pt.root_ppn().0, vpn.0, frame.ppn.0, pte_site::SITE_ALIAS);
                            inv_lo = inv_lo.min(vpn.0 << 12);
                            inv_hi = inv_hi.max((vpn.0 << 12) + crate::mm::PAGE_SIZE);
                        }
                    } else {
                        pt.install_leaf_pte(pte, frame.ppn, flags);
                        pte_site::record(pt.root_ppn().0, vpn.0, frame.ppn.0, pte_site::SITE_ALIAS);
                    }
                }
                Err(()) => match pt.translate(vpn) {
                    Some(ppn) if ppn == frame.ppn => {}
                    Some(_) => {
                        pt.remap(vpn, frame.ppn, flags);
                        inv_lo = inv_lo.min(vpn.0 << 12);
                        inv_hi = inv_hi.max((vpn.0 << 12) + crate::mm::PAGE_SIZE);
                    }
                    None => pt.map(vpn, frame.ppn, flags),
                },
            }
            cur = vma.frames.insert_seq(cur, vpn, frame.clone());
            pte_site::record(
                pt_root,
                vpn.0,
                frame.ppn.0,
                pte_site::SITE_ALIAS + pte_site::FRAMES_BIAS,
            );
            vma.cow = true;
            done += 1;
        }
        if inv_lo < inv_hi {
            crate::arch::asm_helpers::flush_tlb_range_current_asid(inv_lo, inv_hi);
        }
        done
    }

    /// Is every page in `[start, end)` backed by some VMA? mlock(2)/munlock(2)
    /// require the whole range to correspond to mapped pages, otherwise ENOMEM.
    pub fn range_fully_mapped(&self, start: VirtAddr, end: VirtAddr) -> bool {
        let mut vpn = start.floor().0;
        let end_vpn = end.ceil().0;
        while vpn < end_vpn {
            let va = VirtAddr(vpn * PAGE_SIZE);
            // A page counts as mapped if it belongs to a managed VMA *or* has a
            // live page-table entry. SysV shm attachments install PTEs without
            // a managed VMA, yet they are genuinely mapped memory.
            if let Some(vma) = self.find_vma(va) {
                // 命中 VMA 时整段跳到 VMA 末尾，避免对同一 VMA 内的
                // 每一页都重复 find_vma + 三级页表遍历。
                vpn = (vma.end.ceil().0).min(end_vpn).max(vpn + 1);
                continue;
            }
            if self.page_table.translate(VirtPageNum(vpn)).is_none() {
                return false;
            }
            vpn += 1;
        }
        true
    }

    /// Is any page in `[start, end)` currently backed by a live page-table
    /// entry? Used by mremap(2) to detect that an in-place expansion would
    /// collide with already-mapped memory (e.g. a neighbouring shm page) even
    /// when that memory has no managed VMA.
    pub fn range_has_mapped_page(&self, start: VirtAddr, end: VirtAddr) -> bool {
        let mut vpn = start.floor().0;
        let end_vpn = end.ceil().0;
        while vpn < end_vpn {
            if self.page_table.translate(VirtPageNum(vpn)).is_some() {
                return true;
            }
            vpn += 1;
        }
        false
    }

    /// Total bytes currently locked (sum of VM_LOCKED VMA sizes), used to
    /// account a process against RLIMIT_MEMLOCK.
    pub fn locked_vm_bytes(&self) -> u64 {
        self.vmas
            .iter()
            .filter(|v| v.locked)
            .map(|v| (v.end.0 - v.start.0) as u64)
            .sum()
    }

    /// Total mapped bytes across all VMAs (the process's current VM size),
    /// used by mlockall(MCL_CURRENT) to account against RLIMIT_MEMLOCK.
    pub fn total_vm_bytes(&self) -> u64 {
        self.vmas.iter().map(|v| (v.end.0 - v.start.0) as u64).sum()
    }

    /// Set/clear VM_LOCKED on every VMA (mlockall/munlockall MCL_CURRENT).
    pub fn set_all_locked(&mut self, locked: bool) {
        for v in self.vmas.iter_mut() {
            v.locked = locked;
        }
    }

    /// Is the page containing `va` currently resident (present in the page
    /// table)? Used by mincore(2) to report per-page residency.
    pub fn page_present(&self, va: VirtAddr) -> bool {
        self.page_table.translate(va.floor()).is_some()
    }

    /// Is the page containing `va` resident for mincore(2) purposes? A live
    /// local PTE always counts. For an anonymous `MAP_SHARED` mapping the
    /// backing pages live in a shared shmem object, so a page another sharer
    /// faulted in (e.g. a child that mlock'd the range) is resident even when
    /// this address space has not faulted its own PTE yet — mincore on shared
    /// memory reports page-cache residency, not per-task PTE state (mincore04).
    pub fn page_resident(&self, va: VirtAddr) -> bool {
        if self.page_table.translate(va.floor()).is_some() {
            return true;
        }
        let vpn = va.floor();
        if let Some(vma) = self.find_vma(va) {
            if let Some(ref shared) = vma.anon_shared {
                if shared.lock().contains_key(&vpn) {
                    return true;
                }
            }
            // File-backed mapping: a page counts as resident when it is present
            // in the file's page cache, even if this address space has not
            // faulted its own (possibly private) copy yet — another sharer
            // (e.g. a forked child that mlock'd the range) may have populated
            // the cache. mincore04 forks a child that faults a MAP_PRIVATE file
            // mapping and expects the parent's mincore to see the pages.
            if vma.file_backed {
                if let Some(ref inode) = vma.backing {
                    let page_va = vpn.0 * PAGE_SIZE;
                    let file_offset = vma.file_offset + page_va.saturating_sub(vma.start.0);
                    let page_index = file_offset / PAGE_SIZE;
                    if super::page_cache::lookup(inode.ino(), page_index).is_some() {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// 页帧号（PFN）—— `/proc/[pid]/pagemap` 需要返回每个已驻留虚拟页对应的
    /// 物理页号（present 时 bit63=1，bits0..54=pfn）。未映射返回 None。
    pub fn page_pfn(&self, va: VirtAddr) -> Option<usize> {
        self.page_table.translate(va.floor()).map(|ppn| ppn.0)
    }

    /// mlock(2)/munlock(2)/mlock2(2) core. Split the VMAs at the `[start, end)`
    /// boundaries and set VM_LOCKED on exactly the covered sub-range (so
    /// /proc/PID/status VmLck reflects the precise locked size). For a
    /// populating lock (plain mlock / mlock2 without MLOCK_ONFAULT) every page
    /// in the range is faulted into RAM; MLOCK_ONFAULT skips population and
    /// only locks already-present pages. Returns false (ENOMEM) if the range
    /// is not fully backed by VMAs.
    pub fn lock_region(
        &mut self,
        start: VirtAddr,
        end: VirtAddr,
        locked: bool,
        populate: bool,
    ) -> bool {
        if start >= end {
            return true;
        }
        if !self.range_fully_mapped(start, end) {
            return false;
        }

        let mut new_vmas = Vec::with_capacity(self.vmas.len() + 2);
        let old_vmas = core::mem::take(&mut self.vmas);
        for mut vma in old_vmas {
            if end <= vma.start || start >= vma.end {
                new_vmas.push(vma);
                continue;
            }
            let original_start = vma.start;
            let original_end = vma.end;
            let perm = vma.permission;
            let middle_start = if start > vma.start { start } else { vma.start };
            let middle_end = if end < original_end { end } else { original_end };

            if middle_start > original_start {
                let left = Self::extract_segment(&mut vma, original_start, middle_start, perm);
                new_vmas.push(Box::new(left));
            }
            let mut middle = Self::extract_segment(&mut vma, middle_start, middle_end, perm);
            middle.locked = locked;
            if locked && populate {
                for vpn_num in middle.vpn_range() {
                    middle.map_page(VirtPageNum(vpn_num), &mut self.page_table);
                }
            }
            new_vmas.push(Box::new(middle));
            if middle_end < original_end {
                let right = Self::extract_segment(&mut vma, middle_end, original_end, perm);
                new_vmas.push(Box::new(right));
            }
        }
        new_vmas.sort_by_key(|vma| vma.start.0);
        self.vmas = new_vmas;
        crate::arch::asm_helpers::flush_tlb_all();
        self.debug_assert_no_overlap("protect_region");
        true
    }

    /// 扩展 Heap VMA 并预映射新增页面
    ///
    /// 用于 brk 系统调用扩展 heap 时，避免 lazy allocation 在 LoongArch64 上的 TLB 问题。
    pub fn expand_heap_vma(&mut self, heap_start: VirtAddr, new_end: VirtAddr) {
        let old_end = self.find_vma(heap_start).map(|v| v.end).unwrap_or(heap_start);

        // Linux 的 VMA 集合恒不重叠（`find_vma_intersection` 守门）。堆扩展若
        // 直接改 `end`，会与上方既有 VMA 重叠；重叠后缺页只会命中其中一个
        // VMA（按 start 二分的前驱），它的 CoW 换帧不会反映到另一个重叠 VMA 的
        // frames 上，fork 便按陈旧 frames 给子进程建映射 → 子进程读到别的时刻
        // 的内存。因此这里必须维持不重叠：把扩展夹到上方最近 VMA 的起点，
        // 并吸收完全落在扩展范围内的相邻 Heap VMA（连同其帧所有权）。
        let Some(mut anchor_idx) = self
            .vmas
            .iter()
            .position(|v| heap_start >= v.start && heap_start < v.end)
        else {
            return;
        };
        let anchor_start = self.vmas[anchor_idx].start.0;
        let mut desired_end = new_end.0.max(old_end.0);

        // 1) 夹取：上方任何「不能被吸收」的 VMA 都是硬边界。
        let mut absorb: Vec<usize> = Vec::new();
        loop {
            let mut grew = false;
            let mut boundary = usize::MAX;
            for (i, v) in self.vmas.iter().enumerate() {
                if i == anchor_idx || absorb.contains(&i) {
                    continue;
                }
                if v.start.0 < desired_end && v.end.0 > anchor_start {
                    let absorbable = v.vma_type == VmaType::Heap
                        && v.start.0 >= anchor_start
                        && v.end.0 <= desired_end;
                    if absorbable {
                        absorb.push(i);
                        grew = true;
                    } else if v.start.0 > anchor_start {
                        boundary = boundary.min(v.start.0);
                    }
                }
            }
            if boundary != usize::MAX {
                let clamped = desired_end.min(boundary).max(old_end.0);
                if clamped != desired_end {
                    desired_end = clamped;
                    // 夹取后原本「可吸收」的判定可能失效，重算一轮。
                    absorb.clear();
                    continue;
                }
            }
            if !grew {
                break;
            }
        }

        // 2) 吸收：把被合并 Heap VMA 的帧所有权转移给锚点 VMA，避免帧泄漏与
        //    页表里出现无主映射。
        let mut moved: Vec<(VirtPageNum, SharedFrame)> = Vec::new();
        absorb.sort_unstable();
        for &i in absorb.iter().rev() {
            let mut other = self.vmas.remove(i);
            moved.extend(
                other
                    .frames
                    .remove_range(VirtPageNum(0)..VirtPageNum(usize::MAX)),
            );
            if i < anchor_idx {
                anchor_idx -= 1;
            }
        }

        let anchor = &mut self.vmas[anchor_idx];
        anchor.end = VirtAddr(desired_end.max(anchor.end.0));
        for (vpn, frame) in moved {
            anchor.frames.insert(vpn, frame);
        }
        let new_end = anchor.end;
        // brk 增长把 Heap VMA 推进 mmap 自动分配窗口时必须同步抬 bump（见
        // raise_mmap_bump_for 的不变式），否则后续 mmap(NULL) 会把匿名映射
        // 分配到刚扩出的 heap 页上（Linux: mmap 绝不与既有 brk 区重叠）。
        {
            let (base, end) = Self::mmap_window();
            if heap_start.0 < end && new_end.0 > base {
                // 同 raise_mmap_bump_for：heap 尾部之上留 1 页隔离带。
                let e = (new_end.0 + PAGE_SIZE).min(end);
                if e > self.mmap_bump {
                    self.mmap_bump = e;
                }
            }
        }

        // 预映射新增的页面
        if new_end > old_end {
            let start_vpn = old_end.floor().0;
            let end_vpn = new_end.ceil().0;
            for vpn_num in start_vpn..end_vpn {
                let vpn = VirtPageNum(vpn_num);
                if let Some(vma) = self.vmas.iter_mut().find(|v| heap_start >= v.start && heap_start < v.end) {
                    vma.map_page(vpn, &mut self.page_table);
                }
            }
        }
    }

    /// 处理页错误
    pub fn handle_page_fault(&mut self, va: VirtAddr, is_write: bool) -> bool {
        let vpn = va.floor();

        // 0. 快路径：vmas 按 start 有序 → 二分定位前驱 VMA。绝大多数 fault
        //    （demand paging/CoW/swap-in）落在一个能直接满足本次访问的 VMA 内，
        //    无需下面的堆增长探测与全表评分扫描（两者都是 O(VMA 数)，线程栈/
        //    映射数上千时每次 fault 数千次循环是 trap 路径首要热点）。命中且
        //    权限可满足（写→可写或 CoW，读→可读）才走捷径；否则回退完整慢
        //    路径，保留 PROT_NONE 守护页/堆边界扩展/栈向下增长的全部语义。
        let fast_idx: Option<usize> = {
            let idx = self.vmas.partition_point(|vma| vma.start <= va);
            match idx.checked_sub(1).and_then(|i| self.vmas.get(i)) {
                Some(vma)
                    if va < vma.end
                        && (if is_write {
                            vma.permission.write || vma.cow
                        } else {
                            vma.permission.read
                        }) =>
                {
                    Some(idx - 1)
                }
                _ => None,
            }
        };
        if let Some(idx) = fast_idx {
            return self.handle_page_fault_in_vma(va, vpn, idx, is_write);
        }

        // 1. 先尝试 Heap 自动扩展：写访问且 fault 地址恰在某 Heap VMA end 后的 1 页内 →
        //    扩展 Heap 覆盖此页。优先于普通 VMA 查找，避免 PROT_NONE Mmap 守护页"先入为主"
        //    把合法的 musl malloc 边界写吞掉（libc-bench BADV=0x12002c000/0x12002d000）。
        //    特殊处理：如果该页被 PROT_NONE Mmap 占用（musl 的 stack/arena 守护页），
        //    从 Mmap 中"挖出"该页给 Heap 用——musl 仅用这块作守护，不会真访问它，
        //    但 musl malloc 内部会写到 Heap 边界外一页，必须让 Heap 拥有该页。
        if is_write {
            let grow_idx = self.vmas.iter().position(|vma| {
                vma.vma_type == VmaType::Heap
                    && vma.permission.write
                    && va >= vma.end
                    && va.0.saturating_sub(vma.end.0) < PAGE_SIZE
            });
            if let Some(idx) = grow_idx {
                let new_end = VirtAddr(((va.0 + PAGE_SIZE) >> 12) << 12);
                let heap_start = self.vmas[idx].start;
                let page_start = VirtAddr(va.0 & !(PAGE_SIZE - 1));
                let page_end = VirtAddr(page_start.0 + PAGE_SIZE);

                // 查 fault 页是否被某 PROT_NONE **匿名** Mmap 占用（musl malloc 的
                // stack/arena 守护页一定是匿名的）。**绝不**挖除文件映射页：用户用
                // `mmap(addr, PROT_NONE, MAP_PRIVATE|MAP_FILE)` 在 heap 边界放置一段
                // 合法的 PROT_NONE 文件映射时，若被当成守护页挖给可写 heap，则内核经
                // read(2) 往该缓冲区写入会"成功"而非按 PROT_NONE 投递 EFAULT
                // （mmapstress02：read 进 PROT_NONE 映射须 EFAULT）。
                let blocking_mmap_idx = self.vmas.iter().enumerate().position(|(i, other)| {
                    i != idx
                        && other.vma_type == VmaType::Mmap
                        && !other.file_backed
                        && !other.permission.read
                        && !other.permission.write
                        && !other.permission.execute
                        && page_start >= other.start
                        && page_start < other.end
                });

                // fault 页是否被其它（不可挖除的）VMA 占用？例如紧贴 heap 上方的
                // SysV shm `Shared` 预留。Heap 绝不能把它吞并，否则可写 heap VMA
                // 会盖住只读 shm attach，写入永不 fault（shmat01 SHM_RDONLY/shmdt01）。
                let occupied_by_other = self.vmas.iter().enumerate().any(|(i, other)| {
                    i != idx
                        && Some(i) != blocking_mmap_idx
                        && page_start >= other.start
                        && page_start < other.end
                });

                if let Some(mmap_idx) = blocking_mmap_idx {
                    // 从 PROT_NONE Mmap 中挖掉 fault 页：用 unmap_region 让它做 split 处理
                    self.unmap_region(page_start, page_end);
                    // 注意：unmap_region 用 Vec::take/重组，可能改变 idx。重新查找 Heap。
                    if let Some(new_idx) = self.vmas.iter().position(|vma|
                        vma.vma_type == VmaType::Heap
                            && vma.permission.write
                            && vma.start == heap_start
                    ) {
                        self.vmas[new_idx].end = new_end;
                    }
                    let _ = mmap_idx;
                } else if !occupied_by_other {
                    // 普通情形：直接扩展 Heap end
                    self.vmas[idx].end = new_end;
                }
            }
        }

        // 2. 查找覆盖 va 的 VMA。当多个 VMA 重叠（例如 Mmap PROT_NONE 守护页与 Heap）时,
        //    优先选择可以满足本次访问的 VMA：
        //      * 写访问 → 优先 permission.write=true 的 VMA
        //      * 读访问 → 优先 permission.read=true 的 VMA
        //    评分还偏好 Heap/Stack 类型，避免 PROT_NONE Mmap "先入为主"。
        let mut vma_idx: Option<usize> = {
            let prefer_writable = is_write;
            let mut best: Option<(usize, u8)> = None;
            for (i, vma) in self.vmas.iter().enumerate() {
                if !(va >= vma.start && va < vma.end) {
                    continue;
                }
                let mut score: u8 = 0;
                if prefer_writable && vma.permission.write { score += 8; }
                if !prefer_writable && vma.permission.read { score += 8; }
                match vma.vma_type {
                    VmaType::Heap | VmaType::Stack => score += 4,
                    VmaType::Bss | VmaType::Data | VmaType::Code => score += 2,
                    _ => {}
                }
                match best {
                    None => best = Some((i, score)),
                    Some((_, s)) if score > s => best = Some((i, score)),
                    _ => {}
                }
            }
            best.map(|(i, _)| i)
        };

        if vma_idx.is_none() {
            // Stack 向下增长：栈底之下窗口内的 fault 视为合法增长
            let grow_idx = self.vmas.iter().position(|vma| {
                vma.vma_type == VmaType::Stack
                    && va < vma.start
                    && vma.start.0.saturating_sub(va.0) <= Self::STACK_AUTO_GROW_WINDOW
                    && va < vma.end
            });

            if let Some(idx) = grow_idx {
                let new_start = VirtAddr(vpn.0 << 12);
                let stack_end = self.vmas[idx].end;
                let overlaps = self.vmas.iter().enumerate().any(|(other_idx, other)| {
                    other_idx != idx && new_start < other.end && stack_end > other.start
                });
                if !overlaps && !self.stack_grow_hits_guard_gap(new_start, idx) {
                    self.vmas[idx].start = new_start;
                    vma_idx = Some(idx);
                }
            }
        }

        if let Some(idx) = vma_idx {
            return self.handle_page_fault_in_vma(va, vpn, idx, is_write);
        }

        false
    }

    /// 已定位到覆盖 fault 地址的 VMA 后的公共处理：权限校验、swap-in、CoW、
    /// 延迟补页、PTE 升级。快路径（二分命中）与慢路径（评分扫描）共用。
    fn handle_page_fault_in_vma(
        &mut self,
        va: VirtAddr,
        vpn: VirtPageNum,
        idx: usize,
        is_write: bool,
    ) -> bool {
        {
            let vma = &self.vmas[idx];
            let is_cow = vma.cow;
            let can_write = vma.permission.write;

            // 读访问（load fault，非写）要求该区域具备 R 权限。PROT_NONE（无 R）
            // 或 PROT_EXEC-only（仅 X、无 R，RISC-V/LA 的 execute-only 页）被读时，
            // 必须投递 SIGSEGV，而不能按需补页——否则补出的 PTE 仍缺 R 权限，load
            // 指令重试后再次 fault，陷入死循环（mmap05 的 PROT_NONE、mmap03 老版
            // 读 PROT_EXEC 映射，之前都会 hang/超时）。
            // 注：取指 fault 同样走此路径，但正常代码段 VMA 均为 R+X（read=true），
            // 不受影响；execute-only 的"懒映射后跳转执行"在 LTP 基础用例中不存在。
            if !is_write && !vma.permission.read {
                return false;
            }

            // 换出页：PTE 是 swap entry（V=0，translate 看不到）→ 先换回。
            // 必须先于 CoW/已映射判断，否则会误当未映射去补零页。
            if self.page_table.get_swap(vpn).is_some() {
                return self.swap_in(vpn, idx);
            }

            // 写故障先清 MADV_FREE 惰性标记（见 resolve_fault_in_vma 同路径）。
            if is_write {
                self.lazy_free.remove(&vpn);
            }

            if is_write && is_cow && can_write {
                // 写时复制 - 需要分开处理（can_write 门：RO cow 被写 → SIGSEGV）
                let vma = &mut self.vmas[idx];
                return match vma.handle_cow(vpn, &mut self.page_table) {
                    Ok(retired) => {
                        if let Some(old) = retired {
                            self.retired_frames.push(old);
                        }
                        true
                    }
                    Err(()) => false,
                };
            }

            // 检查权限
            if is_write && !can_write {
                return false;
            }

            // 延迟分配
            if self.page_table.translate(vpn).is_none() {
                // fork 懒建的子 PTE：帧已在本 VMA frames 中（与父 Arc 共享），
                // 直接补映射即可——cow 页补只读（写路径已被上面的 handle_cow
                // 分支拦截），非 cow 页按 VMA 权限映射。fault-around 批量补页。
                if self.vmas[idx].frames.contains_key(&vpn) {
                    return self.map_lazy_frames_around(idx, vpn, is_cow);
                }
                // 文件映射中完全落在文件末页之后的页：不能按需补一张匿名零页，
                // 必须让访问产生 SIGBUS（mmap13）。返回 false 由 trap 层据
                // fault_signal 投递 SIGBUS。
                if self.vmas[idx].is_file_page_beyond_eof(va) {
                    return false;
                }
                let vma = &mut self.vmas[idx];
                // 私有匿名读缺页 → 全局共享零页（见 resolve_fault_in_vma 同路径）。
                if !is_write
                    && vma.huge_page_size == 0
                    && !vma.file_backed
                    && vma.backing.is_none()
                    && vma.anon_shared.is_none()
                    && !vma.locked
                {
                    return vma.map_zero_page(vpn, &mut self.page_table);
                }
                return vma.map_page(vpn, &mut self.page_table);
            }

            // MADV_FREE 惰性回收页被**写入**：取消惰性标记、恢复可写映射（见 try_resolve_fault）。
            if is_write {
                self.lazy_free.remove(&vpn);
            }

            // 页面已映射但仍触发写错误：PTE 缺少 W/D 位（例如 MAP_SHARED 文件页先
            // 以只读建立、再被 mprotect 升级为可写，或 clone 出的只读副本）。
            // 对可写且非 CoW 的 VMA，直接把 PTE 重映射为可写+脏，避免无限 store 重试。
            // (mprotect02 第二阶段：对 PROT_WRITE 的共享文件映射写入。)
            if is_write && can_write {
                if let Some(ppn) = self.page_table.translate(vpn) {
                    let flags = self.vmas[idx].permission.to_pte_flags();
                    self.page_table.remap(vpn, ppn, flags);
                    // 同 ppn 权限放宽（补 W/D 位）：陈旧 RO 条目写 fault 自愈。
                    crate::arch::asm_helpers::flush_tlb_va_local(va.0);
                    return true;
                }
            }

            // 页面已映射但仍触发页错误 — 可能是 TLB 中残留旧条目
            // 返回 true 让 trap 处理器刷新 TLB 后重试
            true
        }
    }

    /// FOLL_FORCE 写：经 `/proc/<pid>/mem` 写入时，即便目标 VMA 不可写（PROT_READ）
    /// 也强制落盘——Linux `access_remote_vm` 带 `FOLL_FORCE|FOLL_WRITE`，对私有匿名
    /// 只读映射执行写时复制（COW break）后写入私有副本，**不**改变 VMA 权限、**不**
    /// 污染其它映射（thp04 / Huge Dirty COW：写只读 THP 须破 COW 而非改写全局零页）。
    ///
    /// 语义：① 缺页则先按需补页（读 fault，零填充私有帧）；② 若该帧被多方共享
    /// （fork CoW，refcount>1）则复制出私有副本并重映射；③ 直接写入私有物理帧。
    /// 越界/无覆盖 VMA/OOM 返回 false（调用方据此投递 EIO）。
    pub fn force_write(&mut self, va: VirtAddr, data: &[u8]) -> bool {
        let mut copied = 0usize;
        while copied < data.len() {
            let cur = VirtAddr(va.0 + copied);
            let vpn = cur.floor();
            let off = cur.page_offset();
            let n = (PAGE_SIZE - off).min(data.len() - copied);

            let idx = match self.vmas.iter().position(|v| cur >= v.start && cur < v.end) {
                Some(i) => i,
                None => return false,
            };

            // ① 补页（含换出页换回）。读语义即可：私有匿名零页 / 文件页。
            if self.page_table.translate(vpn).is_none() {
                if self.page_table.get_swap(vpn).is_some() {
                    if !self.swap_in(vpn, idx) {
                        return false;
                    }
                } else if !self.vmas[idx].map_page(vpn, &mut self.page_table) {
                    return false;
                }
            }

            // ② COW break：帧被多方共享时复制私有副本。
            {
                let perm_flags = self.vmas[idx].permission.to_pte_flags();
                let vma = &mut self.vmas[idx];
                if let Some(frame) = vma.frames.get(&vpn) {
                    // 共享判据同 `handle_cow`：以 MetaSlot 帧引用计数为准。
                    if frame.strong() > 1 || frame.meta_ref() > 1 {
                        let Some(new_frame) = frame_alloc() else {
                            return false;
                        };
                        let old_ppn = frame.ppn;
                        let new_ppn = new_frame.ppn;
                        new_ppn
                            .as_bytes_mut()
                            .copy_from_slice(old_ppn.as_bytes_mut());
                        self.page_table.remap(vpn, new_ppn, perm_flags);
                        if let Some(retired) = vma.frames.insert(vpn, SharedFrame::from_alloc(new_frame)) {
                            // 旧帧隔离：其它 hart 的陈旧 TLB 在 drain 前仍可
                            // 命中原帧，绝不能就地归还 buddy。
                            self.retired_frames.push(retired);
                        }
                        crate::arch::asm_helpers::flush_tlb_va(vpn.0 << 12);
                    }
                }
            }

            // ③ 直接写入私有物理帧（绕过 PTE 的 W 位，VMA 权限保持不变）。
            let ppn = match self.page_table.translate(vpn) {
                Some(p) => p,
                None => return false,
            };
            let dst = ppn.as_bytes_mut();
            dst[off..off + n].copy_from_slice(&data[copied..copied + n]);
            copied += n;
        }
        true
    }

    /// 换入：`vpn` 处 PTE 是 swap entry → 分配新帧、从 slot 读回内容、重建映射、
    /// 释放该进程对 slot 的一次引用。对标 Linux `do_swap_page`。
    ///
    /// 注意：每次换入都 `read_slot` 到**独立新帧**，故即便是 fork CoW 共享的
    /// swap entry（refcount>1），父子各自换入也得到私有副本——天然满足 CoW 隔离，
    /// 因此换入后可直接按 VMA 权限（含可写）建立映射。
    fn swap_in(&mut self, vpn: VirtPageNum, idx: usize) -> bool {
        let entry = match self.page_table.get_swap(vpn) {
            Some(e) => e,
            None => return false,
        };
        let off = super::swap::swp_offset(entry);
        let frame = match frame_alloc() {
            Some(f) => f,
            None => return false,
        };
        let ppn = frame.ppn;
        if !super::swap::read_slot(off, ppn.as_bytes_mut()) {
            return false;
        }
        let flags = self.vmas[idx].permission.to_pte_flags();
        // swap PTE 的 V=0，try_map 的 `!is_valid` 断言成立，直接覆盖建立有效映射。
        if !self.page_table.try_map(vpn, ppn, flags) {
            return false;
        }
        pte_site::record(self.page_table.root_ppn().0, vpn.0, ppn.0, pte_site::SITE_SWAP_IN);
        self.vmas[idx].frames.insert(vpn, SharedFrame::from_alloc(frame));
        pte_site::record(
            self.page_table.root_ppn().0,
            vpn.0,
            ppn.0,
            pte_site::SITE_SWAP_IN + pte_site::FRAMES_BIAS,
        );
        super::swap::free_slot(off); // 本进程对该 slot 的引用 -1
        self.swapped_pages = self.swapped_pages.saturating_sub(1);
        DBG_SI.fetch_add(1, Ordering::Relaxed);
        crate::arch::asm_helpers::flush_tlb_va(vpn.addr().0);
        true
    }

    /// 换出：把 `vpn` 处的**私有匿名**页写入 swap slot、PTE 改 swap entry、
    /// 释放物理帧。对标 Linux 匿名页 reclaim（`try_to_unmap` + `pageout`）。
    ///
    /// 只换“可安全换出”的页：私有匿名（非 file-backed / 非 shmem / 非 huge /
    /// 非 mlock）、且该帧仅本地址空间唯一持有（非 CoW 跨进程共享）。其余跳过。
    /// 返回是否成功换出。
    pub fn swap_out_page(&mut self, vpn: VirtPageNum) -> bool {
        self.swap_out_page_inner(vpn, true)
    }

    /// `swap_out_page` 的内部实现，`flush` 控制是否在换出后刷 TLB。批量回收
    /// （`reclaim` clock 循环）传 `false`，在扫描结束后统一 `flush_tlb_all`
    /// 一次，避免每页一次 `sfence.vma`（TCG 下尤其昂贵）造成的吞吐瓶颈。
    fn swap_out_page_inner(&mut self, vpn: VirtPageNum, flush: bool) -> bool {
        let idx = match self
            .vmas
            .iter()
            .position(|v| vpn.addr() >= v.start && vpn.addr() < v.end)
        {
            Some(i) => i,
            None => return false,
        };
        {
            let v = &self.vmas[idx];
            if v.file_backed
                || v.anon_shared.is_some()
                || v.huge_page_size != 0
                || v.locked
                || v.vma_type == VmaType::Shared
            {
                return false;
            }
        }
        // 必须当前 resident（有有效叶子 PTE 且有 frame）。
        let ppn = match self.page_table.translate(vpn) {
            Some(p) => p,
            None => return false,
        };
        let frame = match self.vmas[idx].frames.get(&vpn) {
            Some(f) => f.clone(),
            None => return false,
        };
        // 唯一持有：map 里 1 个 + 刚 clone 的 1 个 = 2。>2 说明 CoW 跨进程共享，
        // 跳过。（SharedFrame 的 clone 已把 MetaSlot +1，meta_ref 即全部持有者
        // 数；fork 修复分支的独立 Arc 实例也在其内，比旧 strong_count 更全。）
        if frame.meta_ref() != 2 {
            DBG_SO_COW.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let off = match super::swap::alloc_slot() {
            Some(o) => o,
            None => {
                DBG_SO_NOSLOT.fetch_add(1, Ordering::Relaxed);
                return false;
            }
        };
        if !super::swap::write_slot(off, ppn.as_bytes_mut()) {
            super::swap::free_slot(off);
            DBG_SO_OTHER.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let entry = super::swap::swp_entry(0, off);
        // 先改 PTE（dec map_count），再移除 Arc（drop 释放物理帧，断言 map_count==0）。
        if !self.page_table.set_swap(vpn, entry) {
            super::swap::free_slot(off);
            DBG_SO_OTHER.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        drop(frame); // 释放本地 clone
        if let Some(retired) = self.vmas[idx].frames.remove(&vpn) {
            // 最后一个持有者 → 本该归还 buddy；先隔离进 quarantine，
            // 由操作边界（reclaim 出口 drain / mm exit）统一归还。
            self.retired_frames.push(retired);
        }
        self.lazy_free.remove(&vpn);
        self.swapped_pages += 1;
        DBG_SO_OK.fetch_add(1, Ordering::Relaxed);
        if flush {
            crate::arch::asm_helpers::flush_tlb_va(vpn.addr().0);
        }
        true
    }

    /// 预读：把 `[start, end)` 范围内已换出（swap entry）的页提前换回 RAM。
    /// 对标 Linux `MADV_WILLNEED` 对 swap 页的 swap readahead（把页拉进 swap
    /// cache）。返回实际换入的页数。
    pub fn swap_in_range(&mut self, start: VirtAddr, end: VirtAddr) -> usize {
        if start >= end {
            return 0;
        }
        let mut done = 0usize;
        let s = start.floor().0;
        let e = end.ceil().0;
        for vpn_num in s..e {
            let vpn = VirtPageNum(vpn_num);
            if self.page_table.get_swap(vpn).is_none() {
                continue;
            }
            let va = vpn.addr();
            let idx = match self
                .vmas
                .iter()
                .position(|v| va >= v.start && va < v.end)
            {
                Some(i) => i,
                None => continue,
            };
            if self.swap_in(vpn, idx) {
                done += 1;
            }
        }
        done
    }

    /// 对 `[start, end)` 范围内所有可换出的私有匿名页执行换出，最多换 `max` 页。
    /// 返回实际换出的页数。供 `MADV_PAGEOUT` 与 cgroup 直接回收调用。
    pub fn swap_out_range(&mut self, start: VirtAddr, end: VirtAddr, max: usize) -> usize {
        if start >= end || max == 0 {
            return 0;
        }
        let mut done = 0usize;
        let s = start.floor().0;
        let e = end.ceil().0;
        for vpn_num in s..e {
            if done >= max {
                break;
            }
            if self.swap_out_page(VirtPageNum(vpn_num)) {
                done += 1;
            }
        }
        done
    }

    /// `MADV_FREE`（lazy free，对标 Linux `madvise_free_pte_range`）。
    ///
    /// 对范围内**已 resident 的私有匿名页**：清除 PTE 的写位（改只读）并把 vpn 记入
    /// `lazy_free` 集合，但**保留物理帧与内容**。语义：
    /// * 之后**读**该页 → 不缺页、返回原内容（'a'）——故 `MADV_FREE` 后立即读不为 0；
    /// * 之后**写**该页 → 写错误 → 缺页处理清除惰性标记并恢复可写（内容更新、不再丢弃）；
    /// * 内存压力（cgroup reclaim）下，仍为惰性的干净页被丢弃（帧释放、PTE 清空），
    ///   下次访问缺页拿到清零页。
    ///
    /// 仅对私有匿名（非 file-backed / 非 shmem / 非 huge）页生效。返回标记的页数。
    pub fn madvise_free(&mut self, start: VirtAddr, end: VirtAddr) -> usize {
        if start >= end {
            return 0;
        }
        let mut marked = 0usize;
        for idx in 0..self.vmas.len() {
            {
                let v = &self.vmas[idx];
                if end <= v.start || start >= v.end {
                    continue;
                }
                if v.file_backed
                    || v.anon_shared.is_some()
                    || v.huge_page_size != 0
                    || v.vma_type == VmaType::Shared
                {
                    continue;
                }
            }
            let vs = self.vmas[idx].start;
            let ve = self.vmas[idx].end;
            let overlap_s = if start > vs { start } else { vs };
            let overlap_e = if end < ve { end } else { ve };
            let ro_flags = {
                let mut f = self.vmas[idx].permission.to_pte_flags();
                f.remove(PTEFlags::W);
                f.remove(PTEFlags::D);
                f
            };
            for vpn_num in overlap_s.floor().0..overlap_e.ceil().0 {
                let vpn = VirtPageNum(vpn_num);
                // 仅处理已 resident 的页（有 frame + 有效叶子 PTE）。
                let ppn = match self.page_table.translate(vpn) {
                    Some(p) => p,
                    None => continue,
                };
                if !self.vmas[idx].frames.contains_key(&vpn) {
                    continue;
                }
                self.page_table.remap(vpn, ppn, ro_flags);
                self.lazy_free.insert(vpn);
                marked += 1;
            }
        }
        crate::arch::asm_helpers::flush_tlb_all();
        marked
    }

    /// 当前 resident（驻留 RAM）的页数 —— 各 VMA frames 计数之和。用于 cgroup
    /// memory 控制器计算 `memory.current`（对标 Linux `mem_cgroup` 的 RSS 计账）。
    pub fn resident_pages(&self) -> usize {
        self.vmas.iter().map(|v| v.frames.len()).sum()
    }

    /// 当前换出到 swap 的页数（`/proc/[pid]/status` 的 `VmSwap`）。
    pub fn swapped_pages(&self) -> usize {
        self.swapped_pages
    }

    /// 当前 `MADV_FREE` 惰性页数量（调试用）。
    pub fn lazy_free_pages(&self) -> usize {
        self.lazy_free.len()
    }

    /// cgroup 内存回收：尽量回收 `want` 页 RAM。对标 Linux
    /// `try_to_free_mem_cgroup_pages`：优先丢弃干净的 `MADV_FREE` 惰性页（无需写
    /// swap），不足再换出脏的私有匿名页到 swap。返回实际回收（释放出 RAM）的页数。
    pub fn reclaim(&mut self, want: usize) -> usize {
        if want == 0 {
            return 0;
        }
        let mut reclaimed = 0usize;

        // Pass 1：丢弃干净的 MADV_FREE 惰性页（帧释放、PTE 清空，下次访问得清零页）。
        let lazy: Vec<VirtPageNum> = self.lazy_free.iter().copied().collect();
        for vpn in lazy {
            if reclaimed >= want {
                break;
            }
            if self.page_table.translate(vpn).is_none() {
                self.lazy_free.remove(&vpn);
                continue;
            }
            let idx = match self
                .vmas
                .iter()
                .position(|v| vpn.addr() >= v.start && vpn.addr() < v.end)
            {
                Some(i) => i,
                None => {
                    self.lazy_free.remove(&vpn);
                    continue;
                }
            };
            self.page_table.unmap(vpn);
            if let Some(retired) = self.vmas[idx].frames.remove(&vpn) {
                self.retired_frames.push(retired);
            }
            self.lazy_free.remove(&vpn);
            reclaimed += 1;
        }
        if reclaimed >= want {
            crate::arch::asm_helpers::flush_tlb_all();
            return reclaimed;
        }

        // Pass 2：用「访问位时钟（clock / second-chance）」换出**非活跃**的私有
        // 匿名页到 swap。对标 Linux：reclaim 不是简单丢页，而是基于 PTE 访问位老化
        // 区分活跃/非活跃——活跃工作集（代码恰好不在此列：文件页被排除；栈被排除）
        // 被反复访问，访问位由硬件不断置位，应被放过；只有"分配后不再访问"的冷页
        // 才换出。这避免了对正在执行的工作集反复换出→立即换回的抖动（活锁）。
        //
        // 算法：把所有可换出的私有匿名 resident 页按 vpn 升序排成环，从 `reclaim_
        // cursor` 处继续扫描：
        //   - 访问位=1：清访问位（下次访问由硬件重新置位）并放过（二次机会）。
        //   - 访问位=0：自上次清位以来未被访问 → 冷页 → 换出。
        // 每次调用只扫一段（预算 = want*4 + 64），游标跨调用持久推进，分摊代价。
        //
        // **不碰栈**（`VmaType::Stack`）：栈是当前执行上下文，换出必立即换回。
        let need = want - reclaimed;
        let mut elig: Vec<VirtPageNum> = Vec::new();
        for v in &self.vmas {
            if v.file_backed
                || v.anon_shared.is_some()
                || v.huge_page_size != 0
                || v.locked
                || v.vma_type == VmaType::Shared
                || v.vma_type == VmaType::Stack
            {
                continue;
            }
            for vpn in v.frames.keys() {
                if self.lazy_free.contains(vpn) {
                    continue;
                }
                elig.push(*vpn);
            }
        }
        if elig.is_empty() {
            return reclaimed;
        }
        elig.sort_unstable();
        DBG_CAND.fetch_add(elig.len(), Ordering::Relaxed);

        let total = elig.len();
        // 从游标处（首个 >= cursor 的页）开始环形扫描。
        let start = match elig.binary_search(&self.reclaim_cursor) {
            Ok(i) => i,
            Err(i) => i % total,
        };
        // 预算：足够多以保证跨调用扫遍全表，但单次有界避免长时间持锁。
        let budget = (need * 4 + 64).min(total * 2);
        let mut scanned = 0usize;
        let mut pos = start;
        let mut touched = false;
        while reclaimed < want && scanned < budget {
            let vpn = elig[pos % total];
            pos += 1;
            scanned += 1;
            match self.page_table.pte_accessed(vpn) {
                Some(true) => {
                    // 活跃：清访问位，二次机会（不逐页刷 TLB，循环末统一刷）。
                    self.page_table.clear_accessed(vpn);
                    touched = true;
                }
                Some(false) => {
                    // 非活跃冷页：换出（不逐页刷 TLB）。
                    if self.swap_out_page_inner(vpn, false) {
                        reclaimed += 1;
                        touched = true;
                    }
                }
                None => {}
            }
        }
        // 统一刷一次 TLB：使被清访问位的页下次访问由硬件重新置位，被换出的页
        // 下次访问触发缺页换入。比逐页 `sfence.vma` 在 TCG 下快几个数量级。
        if touched {
            crate::arch::asm_helpers::flush_tlb_all();
        }
        // 记录下一个待扫页作为游标。
        self.reclaim_cursor = elig[pos % total];
        reclaimed
    }

    /// Two-phase page fault resolution — Phase 1 (under PROCESS_TABLE lock).
    ///
    /// Handles faults that don't require file I/O: anonymous zero-pages,
    /// CoW, TLB refresh, permission checks, heap/stack growth. For
    /// file-backed demand paging, returns `FaultAction::NeedFileRead` with
    /// all metadata needed to perform I/O outside the lock.
    ///
    /// This matches Linux's `handle_mm_fault` → `handle_pte_fault` dispatch:
    /// - PTE=0, anonymous → `do_anonymous_page` (allocate zero page inline)
    /// - PTE=0, file-backed → `do_fault` → return info for `filemap_fault`
    /// - PTE present, write to RO → `do_wp_page` (CoW inline)
    pub fn try_resolve_fault(&mut self, va: VirtAddr, is_write: bool) -> FaultAction {
        let vpn = va.floor();

        // --- 0. 快路径：vmas 按 start 有序 → 二分定位前驱 VMA ---
        // 绝大多数 fault（demand paging/CoW/swap-in）落在一个能直接满足本次
        // 访问的 VMA 内：命中且权限可满足（写→可写或 CoW，读→可读）时无需
        // 堆增长探测与全表评分扫描（均 O(VMA 数)，映射上千时是 fault 热路径
        // 首要开销）。未命中/权限不满足 → 回退完整慢路径，堆边界扩展、
        // PROT_NONE 守护页挖除、栈向下增长语义全部保留。
        {
            let pidx = self.vmas.partition_point(|vma| vma.start <= va);
            if let Some(vma) = pidx.checked_sub(1).and_then(|i| self.vmas.get(i)) {
                let satisfies = if is_write {
                    vma.permission.write || vma.cow
                } else {
                    vma.permission.read
                };
                if va < vma.end && satisfies {
                    return self.resolve_fault_in_vma(va, vpn, pidx - 1, is_write);
                }
            }
        }

        // --- 单趟扫描：同时找 heap 自动增长候选 与 覆盖 va 的最佳 VMA ---
        // （旧实现为两趟独立全表扫描，缺页热路径 TB profile 占全场 ~3.4%）
        let mut heap_grow_idx: Option<usize> = None;
        let mut fused_best: Option<(usize, u8)> = None;
        for (i, vma) in self.vmas.iter().enumerate() {
            if is_write
                && heap_grow_idx.is_none()
                && vma.vma_type == VmaType::Heap
                && vma.permission.write
                && va >= vma.end
                && va.0.saturating_sub(vma.end.0) < PAGE_SIZE
            {
                heap_grow_idx = Some(i);
            }
            if va >= vma.start && va < vma.end {
                let mut score: u8 = 0;
                if is_write && vma.permission.write { score += 8; }
                if !is_write && vma.permission.read { score += 8; }
                match vma.vma_type {
                    VmaType::Heap | VmaType::Stack => score += 4,
                    VmaType::Bss | VmaType::Data | VmaType::Code => score += 2,
                    _ => {}
                }
                match fused_best {
                    None => fused_best = Some((i, score)),
                    Some((_, s)) if score > s => fused_best = Some((i, score)),
                    _ => {}
                }
            }
        }

        // --- Heap auto-grow (same logic as handle_page_fault) ---
        if is_write {
            let grow_idx = heap_grow_idx;
            if let Some(idx) = grow_idx {
                fused_best = None; // 增长会改 vmas，下方重新 lookup
                let new_end = VirtAddr(((va.0 + PAGE_SIZE) >> 12) << 12);
                let heap_start = self.vmas[idx].start;
                let page_start = VirtAddr(va.0 & !(PAGE_SIZE - 1));
                let page_end = VirtAddr(page_start.0 + PAGE_SIZE);

                let blocking_mmap_idx = self.vmas.iter().enumerate().position(|(i, other)| {
                    i != idx
                        && other.vma_type == VmaType::Mmap
                        && !other.permission.read
                        && !other.permission.write
                        && !other.permission.execute
                        && page_start >= other.start
                        && page_start < other.end
                });

                // Is the fault page owned by some *other*, non-carveable VMA
                // (e.g. a SysV shm `Shared` reservation sitting just above the
                // heap)? The heap must not silently absorb it — otherwise a
                // writable heap VMA would shadow a read-only shm attach and a
                // write would never fault (shmat01 SHM_RDONLY / shmdt01).
                let occupied_by_other = self.vmas.iter().enumerate().any(|(i, other)| {
                    i != idx
                        && Some(i) != blocking_mmap_idx
                        && page_start >= other.start
                        && page_start < other.end
                });

                if let Some(_mmap_idx) = blocking_mmap_idx {
                    self.unmap_region(page_start, page_end);
                    if let Some(new_idx) = self.vmas.iter().position(|vma|
                        vma.vma_type == VmaType::Heap
                            && vma.permission.write
                            && vma.start == heap_start
                    ) {
                        self.vmas[new_idx].end = new_end;
                    }
                } else if !occupied_by_other {
                    self.vmas[idx].end = new_end;
                }
            }
        }

        // --- VMA lookup (same scoring as handle_page_fault) ---
        // 快路径：复用上方单趟扫描结果；heap 增长改过 vmas 时重扫（罕见）。
        let mut vma_idx: Option<usize> = match fused_best {
            Some((i, _)) => Some(i),
            None => {
                let mut best: Option<(usize, u8)> = None;
                for (i, vma) in self.vmas.iter().enumerate() {
                    if !(va >= vma.start && va < vma.end) {
                        continue;
                    }
                    let mut score: u8 = 0;
                    if is_write && vma.permission.write { score += 8; }
                    if !is_write && vma.permission.read { score += 8; }
                    match vma.vma_type {
                        VmaType::Heap | VmaType::Stack => score += 4,
                        VmaType::Bss | VmaType::Data | VmaType::Code => score += 2,
                        _ => {}
                    }
                    match best {
                        None => best = Some((i, score)),
                        Some((_, s)) if score > s => best = Some((i, score)),
                        _ => {}
                    }
                }
                best.map(|(i, _)| i)
            }
        };

        // Stack auto-grow
        if vma_idx.is_none() {
            let grow_idx = self.vmas.iter().position(|vma| {
                vma.vma_type == VmaType::Stack
                    && va < vma.start
                    && vma.start.0.saturating_sub(va.0) <= Self::STACK_AUTO_GROW_WINDOW
                    && va < vma.end
            });
            if let Some(idx) = grow_idx {
                let new_start = VirtAddr(vpn.0 << 12);
                let stack_end = self.vmas[idx].end;
                let overlaps = self.vmas.iter().enumerate().any(|(other_idx, other)| {
                    other_idx != idx && new_start < other.end && stack_end > other.start
                });
                if !overlaps && !self.stack_grow_hits_guard_gap(new_start, idx) {
                    self.vmas[idx].start = new_start;
                    vma_idx = Some(idx);
                }
            }
        }

        let idx = match vma_idx {
            Some(i) => i,
            None => return FaultAction::Failed(11), // SIGSEGV
        };

        self.resolve_fault_in_vma(va, vpn, idx, is_write)
    }

    /// 已定位到覆盖 fault 地址的 VMA 后的公共处理：权限校验、swap-in、CoW、
    /// 延迟补页、文件映射两阶段 I/O 调度。快路径（二分命中）与慢路径
    /// （评分扫描）共用。
    fn resolve_fault_in_vma(
        &mut self,
        va: VirtAddr,
        vpn: VirtPageNum,
        idx: usize,
        is_write: bool,
    ) -> FaultAction {
        let vma = &self.vmas[idx];
        let is_cow = vma.cow;
        let can_write = vma.permission.write;

        if Self::h23dbg() {
            crate::println!("[h23] fault va={:#x} write={} idx={} vma[{:#x},{:#x}) r={} w={} x={} cow={} huge={} translated={}",
                va.0, is_write, idx, vma.start.0, vma.end.0,
                vma.permission.read, vma.permission.write, vma.permission.execute,
                is_cow, vma.huge_page_size,
                self.page_table.translate(vpn).is_some());
        }

        // Permission check: read access requires R permission
        if !is_write && !vma.permission.read {
            return FaultAction::Failed(11); // SIGSEGV
        }

        // Swapped-out page: read it back in (do_swap_page). Must precede the
        // CoW/present checks — a swap PTE is non-present so translate() misses it.
        if self.page_table.get_swap(vpn).is_some() {
            return if self.swap_in(vpn, idx) {
                FaultAction::Handled
            } else {
                FaultAction::Failed(11)
            };
        }

        // 任何写故障都清除 MADV_FREE 惰性标记（页重新变脏，不再可丢弃）——
        // 必须先于 CoW 分支：cow VMA 里的 lazyfree 页被写时走 handle_cow，
        // 若不在此清标记，脏页会被后续 reclaim 误丢（madvise09）。
        if is_write {
            self.lazy_free.remove(&vpn);
        }

        // CoW: handle inline (no file I/O needed). can_write 门：RO 的 cow VMA
        // 被写必须 SIGSEGV，不能走 handle_cow 只读重映射后无限缺页循环。
        if is_write && is_cow && can_write {
            let vma = &mut self.vmas[idx];
            return match vma.handle_cow(vpn, &mut self.page_table) {
                Ok(retired) => {
                    if let Some(old) = retired {
                        // 旧帧隔离进 quarantine：trap 层在锁外 drain_shootdown
                        // 之后经 drain_and_reap_current_mm 统一归还 buddy。
                        self.retired_frames.push(old);
                    }
                    FaultAction::HandledNeedsTlbDrain
                }
                Err(()) => FaultAction::Failed(11),
            };
        }

        // Write permission check
        if is_write && !can_write {
            return FaultAction::Failed(11);
        }

        // Page already mapped?
        if self.page_table.translate(vpn).is_some() {
            if Self::h23dbg() {
                crate::println!("[h23] mapped-fault vpn={:#x} pte_flags={:?}",
                    vpn.0, self.page_table.get_pte_flags(vpn));
            }
            // MADV_FREE 惰性回收页被**写入**：取消惰性标记（页重新变脏，不再可丢弃），
            // 恢复可写映射。对标 Linux：写 MADV_FREE 页清除其 lazyfree 状态。
            if is_write {
                self.lazy_free.remove(&vpn);
            }
            // Mapped but still faulting on write: PTE missing W/D bits
            if is_write && can_write {
                if let Some(ppn) = self.page_table.translate(vpn) {
                    let flags = self.vmas[idx].permission.to_pte_flags();
                    self.page_table.remap(vpn, ppn, flags);
                    crate::arch::asm_helpers::flush_tlb_va(va.0);
                }
            }
            return FaultAction::Handled;
        }

        // Page not mapped — demand allocation needed
        // fork 懒建的子 PTE：帧已在本 VMA frames 中（与父 Arc 共享），直接补
        // 映射即可——cow 页补只读（写路径已被上面的 handle_cow 分支拦截），
        // 非 cow 页按 VMA 权限映射。fault-around 批量补附近页减少 trap 次数。
        if self.vmas[idx].frames.contains_key(&vpn) {
            return if self.map_lazy_frames_around(idx, vpn, is_cow) {
                FaultAction::Handled
            } else {
                FaultAction::Failed(11)
            };
        }

        // Check for SIGBUS (file page beyond EOF)
        if self.vmas[idx].is_file_page_beyond_eof(va) {
            return FaultAction::Failed(7); // SIGBUS (file page beyond EOF)
        }

        let vma = &self.vmas[idx];

        // File-backed mapping: return NeedFileRead to do I/O outside lock
        if vma.file_backed {
            if let Some(ref inode) = vma.backing {
                let inode_id = inode.ino();
                let page_va = vpn.0 * PAGE_SIZE;
                let vma_offset = page_va.saturating_sub(vma.start.0);
                let file_offset = vma.file_offset + vma_offset;
                let file_size = inode.as_file()
                    .and_then(|f| f.stat().ok())
                    .map(|s| s.size)
                    .unwrap_or(0);
                return FaultAction::NeedFileRead(FileReadInfo {
                    inode: inode.clone(),
                    inode_id,
                    file_offset,
                    file_size,
                    vpn,
                    vma_start: vma.start,
                    vma_end: vma.end,
                    vma_file_offset: vma.file_offset,
                    pte_flags: vma.permission.to_pte_flags(),
                    vma_idx: idx,
                    is_shared: vma.vma_type == VmaType::Shared,
                });
            }
        }

        // Anonymous mapping (incl. HugeTLB): allocate the page inline. A
        // HugeTLB fault that cannot be satisfied — the global pool plus the
        // overcommit quota are exhausted — raises SIGBUS, matching Linux
        // hugetlb fault semantics (hugemmap27); an ordinary anonymous OOM is
        // a SIGSEGV.
        let is_huge = self.vmas[idx].huge_page_size != 0;
        let vma = &mut self.vmas[idx];
        // 私有匿名映射的读缺页：映射全局共享零页（零内存消耗，写入时 CoW
        // 断开），对标 Linux do_anonymous_page 的 ZERO_PAGE 路径。fault-around
        // 同样用零页别名，零分配成本。
        if !is_write
            && !is_huge
            && !vma.file_backed
            && vma.backing.is_none()
            && vma.anon_shared.is_none()
            && !vma.locked
        {
            if vma.map_zero_page(vpn, &mut self.page_table) {
                const FAULT_AROUND_PAGES: usize = 15;
                let end_vpn = VirtAddr(vma.end.0).floor().0
                    + if vma.end.0 % PAGE_SIZE == 0 { 0 } else { 1 };
                let limit = core::cmp::min(vpn.0 + 1 + FAULT_AROUND_PAGES, end_vpn);
                for v in (vpn.0 + 1)..limit {
                    let nvpn = VirtPageNum(v);
                    if vma.frames.contains_key(&nvpn)
                        || self.page_table.translate(nvpn).is_some()
                        || self.page_table.get_swap(nvpn).is_some()
                    {
                        break;
                    }
                    if !vma.map_zero_page(nvpn, &mut self.page_table) {
                        break;
                    }
                }
                return FaultAction::Handled;
            }
            return FaultAction::Failed(11);
        }
        if vma.map_page(vpn, &mut self.page_table) {
            // 匿名 fault-around：demand-zero 缺页时顺带把同 VMA 内紧随其后的
            // 若干未映射页一并补零映射，成批砍掉后续 trap（QEMU TCG 下每次
            // guest 异常成本高）。仅限普通匿名私有映射——文件/huge/locked/
            // shared 走各自语义路径，不做 around。
            if !is_huge
                && !vma.file_backed
                && vma.backing.is_none()
                && vma.anon_shared.is_none()
                && !vma.locked
            {
                const FAULT_AROUND_PAGES: usize = 15;
                let end_vpn = VirtAddr(vma.end.0).floor().0
                    + if vma.end.0 % PAGE_SIZE == 0 { 0 } else { 1 };
                let limit = core::cmp::min(vpn.0 + 1 + FAULT_AROUND_PAGES, end_vpn);
                for v in (vpn.0 + 1)..limit {
                    let nvpn = VirtPageNum(v);
                    if vma.frames.contains_key(&nvpn)
                        || self.page_table.translate(nvpn).is_some()
                        || self.page_table.get_swap(nvpn).is_some()
                    {
                        break;
                    }
                    if !vma.map_page(nvpn, &mut self.page_table) {
                        break;
                    }
                }
            }
            FaultAction::Handled
        } else if is_huge {
            FaultAction::Failed(7) // SIGBUS: hugetlb pool exhausted
        } else {
            FaultAction::Failed(11)
        }
    }

    /// Two-phase page fault resolution — Phase 3 (re-acquire lock, install page).
    ///
    /// Called after file I/O is complete. Installs the pre-populated frame
    /// into the process's page table and VMA frame map. Also installs any
    /// readahead pages that were pre-read into the page cache.
    ///
    /// `vpn`: the faulting virtual page number
    /// `frame`: the physical frame with file data already read into it
    /// `vma_idx`: the VMA index from FileReadInfo (may have shifted if VMA
    ///            list was modified between phases — we re-validate)
    /// `pte_flags`: the PTE flags to use
    /// `vma_start`: expected VMA start for validation
    ///
    /// Returns true if the page was successfully installed.
    pub fn install_demand_page(
        &mut self,
        vpn: VirtPageNum,
        frame: SharedFrame,
        vma_idx: usize,
        pte_flags: PTEFlags,
        vma_start: VirtAddr,
    ) -> bool {
        // Re-validate: the VMA might have been modified between phase 1 and 3.
        // Check that vma_idx is still valid and points to the right VMA.
        let idx = if vma_idx < self.vmas.len() && self.vmas[vma_idx].start == vma_start {
            vma_idx
        } else {
            // VMA list shifted; search for the VMA covering this VPN
            match self.vmas.iter().position(|v| {
                let va = VirtAddr(vpn.0 * PAGE_SIZE);
                va >= v.start && va < v.end
            }) {
                Some(i) => i,
                None => return false, // VMA was removed
            }
        };

        // Don't double-map: another fault handler may have installed it already
        if self.page_table.translate(vpn).is_some() {
            return true;
        }

        let ppn = frame.ppn;
        if !self.page_table.try_map(vpn, ppn, pte_flags) {
            return false;
        }
        pte_site::record(self.page_table.root_ppn().0, vpn.0, ppn.0, pte_site::SITE_DEMAND_FILE);
        self.vmas[idx].frames.insert(vpn, frame);
        pte_site::record(
            self.page_table.root_ppn().0,
            vpn.0,
            ppn.0,
            pte_site::SITE_DEMAND_FILE + pte_site::FRAMES_BIAS,
        );
        true
    }

    /// Install readahead pages from the page cache into the page table.
    /// Only installs pages that are in the cache and whose VPN falls within
    /// the VMA bounds. Skips pages already mapped.
    pub fn install_readahead_pages(
        &mut self,
        inode_id: usize,
        vma_idx: usize,
        vma_start: VirtAddr,
        vma_end: VirtAddr,
        vma_file_offset: usize,
        pte_flags: PTEFlags,
        start_page_index: usize,
        count: usize,
        is_shared: bool,
    ) {
        // Re-validate VMA index
        let idx = if vma_idx < self.vmas.len() && self.vmas[vma_idx].start == vma_start {
            vma_idx
        } else {
            match self.vmas.iter().position(|v| v.start == vma_start && v.end == vma_end) {
                Some(i) => i,
                None => return,
            }
        };

        for i in 0..count {
            let page_index = start_page_index + i;
            let file_off = page_index * PAGE_SIZE;
            // Compute the virtual address for this file page
            if file_off < vma_file_offset {
                continue;
            }
            let vma_offset = file_off - vma_file_offset;
            let va = vma_start.0 + vma_offset;
            if va >= vma_end.0 {
                break;
            }
            let vpn = VirtPageNum(va / PAGE_SIZE);
            // Skip already-mapped pages
            if self.page_table.translate(vpn).is_some() {
                continue;
            }
            // Look up in page cache
            if let Some(cached_frame) = super::page_cache::lookup(inode_id, page_index) {
                // MAP_SHARED / MAP_PRIVATE 只读段直接共享缓存帧：不可写页面不会
                // 被用户写，复制一遍纯属浪费（rustc 每个进程都 mmap 上百 MB
                // 代码/只读数据，旧逻辑对每个只读页也做一次 4K memcpy）。
                let direct = is_shared || !pte_flags.contains(PTEFlags::W);
                if direct {
                    let ppn = cached_frame.ppn;
                    pte_site::record(self.page_table.root_ppn().0, vpn.0, ppn.0, pte_site::SITE_READAHEAD);
                    if self.page_table.try_map(vpn, ppn, pte_flags) {
                        self.vmas[idx].frames.insert(vpn, cached_frame);
                    }
                } else {
                    // MAP_PRIVATE writable: copy from cache to a new private frame
                    if let Some(private_frame) = frame_alloc_uninit() {
                        let src = cached_frame.ppn.as_bytes_mut();
                        let dst = private_frame.ppn.as_bytes_mut();
                        dst.copy_from_slice(src);
                        let ppn = private_frame.ppn;
                        if self.page_table.try_map(vpn, ppn, pte_flags) {
                            self.vmas[idx].frames.insert(vpn, SharedFrame::from_alloc(private_frame));
                        }
                    }
                }
            }
        }
    }

    /// 克隆地址空间（用于 fork，使用写时复制 CoW）
    ///
    /// 父子共享物理页，通过去除写权限实现 CoW。
    /// 写入时触发 page fault → handle_cow 分配新页并恢复写权限。
    pub fn clone_cow(&mut self) -> Option<Self> {
        if crate::cmdline::execdbg_enabled() {
            for i in 0..self.vmas.len() {
                for j in (i + 1)..self.vmas.len() {
                    let (a, b) = (&self.vmas[i], &self.vmas[j]);
                    if a.start.0 < b.end.0 && b.start.0 < a.end.0 {
                        crate::println!(
                            "[EXECDBG] vma-overlap i={} [{:#x},{:#x}) type={:?} frames={} | j={} [{:#x},{:#x}) type={:?} frames={}",
                            i, a.start.0, a.end.0, a.vma_type, a.frames.len(),
                            j, b.start.0, b.end.0, b.vma_type, b.frames.len()
                        );
                    }
                }
            }
        }
        let mut new_space = Self::try_new()?;
        // pkey 分配与权限随 mm 继承（Linux：key 分配 per-mm，PKRU 随 fork 继承）。
        new_space.pkey_alloc_mask = self.pkey_alloc_mask;
        new_space.pkey_rights = self.pkey_rights;

        for vma in &mut self.vmas {
            // 对于可写 VMA，需要将父的 PTE 也去掉 W 位以实现 CoW。
            // MADV_WIPEONFORK 的 VMA 例外：父进程保留自己的页且不改权限，
            // 子进程不继承任何页（下面整段跳过），首次访问时缺页换入清零页。
            let need_cow =
                vma.permission.write && vma.vma_type != VmaType::Shared && !vma.wipeonfork;

            // 子 VMA 共享父的 frames（Arc clone 增加引用计数）
            let mut child_vma = Vma {
                start: vma.start,
                end: vma.end,
                vma_type: vma.vma_type,
                permission: vma.permission,
                cow: need_cow,
                locked: vma.locked,
                file_backed: vma.file_backed,
                may_write: vma.may_write,
                huge_page_size: vma.huge_page_size,
                huge_reserved: vma.huge_reserved,
                backing: vma.backing.clone(),
                file_path: vma.file_path.clone(),
                file_offset: vma.file_offset,
                frames: FrameMap::new(),
                huge_frames: FrameMap::new(),
                // Share the shmem object so the child sees the same physical
                // pages for an anonymous MAP_SHARED mapping, including pages
                // first faulted in after the fork.
                anon_shared: vma.anon_shared.clone(),
                // MADV_WIPEONFORK persists across fork so grand-children are
                // wiped too; the page-copy loop below is skipped for it.
                wipeonfork: vma.wipeonfork,
                dontdump: vma.dontdump,
                pkey: vma.pkey,
            };
            child_vma.huge_frames = vma.huge_frames.clone();

            // 计算只读 flags（去掉 W 位）
            let cow_flags = {
                let mut f = vma.permission.to_pte_flags();
                f.remove(PTEFlags::W);
                f.remove(PTEFlags::D);
                f
            };

            // MADV_WIPEONFORK：子进程必须看到清零内存——不拷贝、不映射父页，
            // 让缺页处理在子进程首次访问时分配清零匿名页（madvise10）。
            if vma.wipeonfork {
                new_space.vmas.push(Box::new(child_vma));
                continue;
            }

            // 只有与已拷贝的前序 VMA 地址重叠时，子页表才可能已映射本区间的
            // vpn；不重叠则每页的子侧 translate/get_swap 去重检查必为空，整段
            // 跳过——省掉每页一次 3 级全走表（fork 热路径）。
            let overlaps_prev = new_space
                .vmas
                .iter()
                .any(|prev| prev.start.0 < vma.end.0 && prev.end.0 > vma.start.0);

            // 子 frames 先收集进有序 Vec，尾部一次性 bulk 构建 BTreeMap：
            // vpn 升序且唯一，FromIterator 走 O(n) 批量建树，取代逐页
            // insert 的 O(n log n) 再平衡。
            let mut child_frames: Vec<(VirtPageNum, SharedFrame)> =
                Vec::with_capacity(vma.frames.len());

            // 父 frames 有序游标：vpn 升序遍历与 frames 键升序同序，锻步推进代替
            // 逐页 BTreeMap::get 的 O(log n) 下钻，整段摊销 O(n)。
            let mut pframes = vma.frames.iter().peekable();

            // L0 表块级缓存：vpn 升序扫描下同一 512 页块只做一次 L2/L1 下钻，
            // 取代每页 find_pte_mut / try_map 的 3 级全走表（fork 热路径）。
            let mut pl0_cache: (usize, Result<Option<super::PhysPageNum>, ()>) =
                (usize::MAX, Ok(None));
            let mut cl0_cache: (usize, Option<super::PhysPageNum>) = (usize::MAX, None);

            // 实测结论（Batch 19）：子页表懒建（缺页时从 frames 补 PTE）对
            // lat_proc fork 是负优化——eager 批量建 PTE 经 L0 缓存后每页仅几十
            // 条指令，而懒建把开销转移到每页数千条指令的 trap（791→852µs）。
            // 保持 eager；缺页侧的 frames 补页分支作为健壮性兑底保留。
            let lazy_child_pte = false;

            // 稀疏快路径：私有匿名 Heap/Mmap/Stack VMA 的全部映射页必在
            // vma.frames 中（demand 分配 / CoW / 零页 / alias 映射都入账；
            // Code/Data/Bss 的帧可能不入 frames，Shared/shm 有裸 PTE，均走
            // 慢路径），且无 swap 项时页表里也没有 frames 之外的用户态信息
            // ——只遍历 frames（O(mapped)），跳过 vpn_range 的 O(声明范围)
            // 扫描。fork14 在 224GiB mmap 窗口下保活 224×1GiB 匿名映射，
            // 逐 VPN 扫描是每次 fork 5800 万次循环的灾难。
            let sparse_ok = self.swapped_pages == 0
                && !overlaps_prev
                && matches!(vma.vma_type, VmaType::Heap | VmaType::Mmap | VmaType::Stack)
                && vma.anon_shared.is_none()
                && !vma.file_backed
                && vma.backing.is_none()
                && vma.huge_page_size == 0;
            if sparse_ok {
                let parent_root = self.page_table.root_ppn().0;
                // 页表是进程实际可见映射的唯一真相（Linux `copy_page_range` 就是
                // 逐 PTE 复制）。`frames` 只是帧所有权登记表，可能落后于页表
                // （例如本地址空间的 CoW 换帧发生在 `frames` 快照之后）。fork 时
                // 一旦发现两者不一致，必须以 PTE 为准把子进程映射到真实帧，并就地
                // 修复父进程的所有权登记，否则子进程会继承一份「别的时刻的内存」。
                let mut frames_repairs: Vec<(VirtPageNum, SharedFrame)> = Vec::new();
                for (&vpn, frame) in vma.frames.iter() {
                    let mut ppn = frame.ppn;
                    let mut owner = frame.clone();
                    if crate::cmdline::frameprobe_enabled() && FPDBG_CLONE.fetch_add(1, Ordering::Relaxed) < 24 {
                        crate::println!(
                            "[FPDBG] clone-fast vpn={:#x} ppn={:#x} meta={} strong={}",
                            vpn.0, frame.ppn.0, frame.meta_ref(),
                            owner.strong(),
                        );
                    }
                    let flags = if need_cow { cow_flags } else { vma.permission.to_pte_flags() };
                    if need_cow {
                        if let Some(pte) = self
                            .page_table
                            .find_pte_mut_cached(vpn, &mut pl0_cache)
                            .filter(|pte| pte.is_valid())
                        {
                            if crate::cmdline::execdbg_enabled() && pte.ppn().0 != ppn.0 {
                                let pte_ppn = pte.ppn().0;
                                let owner_vpn = vma
                                    .frames
                                    .iter()
                                    .find(|(_, f)| f.ppn.0 == pte_ppn)
                                    .map(|(v, _)| v.0);
                                let root = parent_root;
                                let site_same = pte_site::last_for(root, vpn.0);
                                let site_any = pte_site::last_any(vpn.0);
                                let frames_site = pte_site::last_frames_for(root, vpn.0);
                                // 序号比较：谁最后一次把该 ppn 与该 vpn 关联。
                                let seq_frames = pte_site::last_seq_by_ppn(vpn.0, ppn.0);
                                let seq_pte = pte_site::last_seq_by_ppn(vpn.0, pte_ppn);
                                // 另一独立假设：FrameMap 失序/重复会让二分查找与
                                // 线性遍历给出不同结果，同轮一起验证。
                                let bsearch_ppn = vma.frames.get(&vpn).map(|f| f.ppn.0);
                                let dup_count = vma.frames.iter().filter(|(v, _)| **v == vpn).count();
                                crate::println!(
                                    "[EXECDBG] forkmap-stale root={:#x} vpn={:#x} frames_ppn={:#x} bsearch={:?} dup={} refs={} pte_ppn={:#x} owner={:?} pte_site={:?} any={:?} frames_site={:?} seq_frames={:?} seq_pte={:?} vma=[{:#x},{:#x}) type={:?}",
                                    root, vpn.0, ppn.0, bsearch_ppn, dup_count,
                                    frame.strong(), pte_ppn, owner_vpn,
                                    site_same, site_any, frames_site, seq_frames, seq_pte,
                                    vma.start.0, vma.end.0, vma.vma_type
                                );
                            }
                            if pte.ppn().0 != ppn.0 {
                                // PTE 为准：为真实帧再增加一个所有者（引用计数 +1），
                                // 子进程与父进程都登记该帧；旧的陈旧句柄在替换后
                                // 释放本地址空间对它的所有权（它已不被本页表映射）。
                                let truth = pte.ppn();
                                owner = SharedFrame::from_ppn_inc(truth, 0);
                                ppn = truth;
                                frames_repairs.push((vpn, owner.clone()));
                            }
                            *pte = PageTableEntry::new(pte.ppn(), cow_flags | PTEFlags::V | PTEFlags::A);
                        }
                    }
                    child_frames.push((vpn, owner));
                    if !lazy_child_pte
                        && !new_space.page_table.try_map_cached(vpn, ppn, flags, &mut cl0_cache)
                    {
                        return None;
                    }
                }
                // 稀疏快路径不使用父 frames 游标，提前释放其不可变借用，
                // 以便就地修复父进程的帧所有权登记。
                drop(pframes);
                for (vpn, owner) in frames_repairs {
                    if let Some(stale) = vma.frames.insert(vpn, owner) {
                        // 陈旧的父 frames 条目被替换：其帧可能仍被其它 hart
                        // 的陈旧 TLB 命中（父 PTE 已改指向 truth），先隔离。
                        self.retired_frames.push(stale);
                    }
                }
                child_vma.frames = FrameMap::from_sorted_vec(child_frames);
                if need_cow {
                    vma.cow = true;
                }
                new_space.vmas.push(Box::new(child_vma));
                continue;
            }

            for vpn_num in vma.vpn_range() {
                let vpn = VirtPageNum(vpn_num);

                // 跳过内核 gigapage 保留区。
                // RISC-V64: 内核 gigapage 占 root[2..2+n] = [0x8000_0000,
                //   kernel_ident_end)，n 随实测 RAM 变化（-m 1G → 0xc000_0000，
                //   -m 8G → 0x2_8000_0000）。其上方直到用户 mmap 窗口
                //   (VPN < 0x4000000) 是合法用户 VA，可被大映射占用
                //   （hugemmap17），fork 必须照常拷贝/继承，不能整段跳过。
                // LoongArch64: 内核通过 DMW 窗口访问，不在用户页表中，
                //              但 Sv39 风格页表最大支持 512GB (VPN < 0x8000000)
                #[cfg(target_arch = "riscv64")]
                if (0x80000..(crate::mm::kernel_ident_end() >> 12)).contains(&vpn.0) {
                    continue;
                }

                // 父页已换出（swap entry）：子继承同一 swap entry，slot refcount++。
                // 各自换入时 read_slot 到独立帧 → 天然 CoW 隔离（见 swap_in 注释）。
                // swapped_pages==0 时整个地址空间无 swap entry，跳过这次全深度走表。
                if let Some(entry) = if self.swapped_pages > 0 {
                    self.page_table.get_swap(vpn)
                } else {
                    None
                } {
                    if overlaps_prev
                        && (new_space.page_table.translate(vpn).is_some()
                            || new_space.page_table.get_swap(vpn).is_some())
                    {
                        continue;
                    }
                    if !new_space.page_table.set_swap(vpn, entry) {
                        return None;
                    }
                    super::swap::dup_slot(super::swap::swp_offset(entry));
                    new_space.swapped_pages += 1;
                    continue;
                }

                // 一次走表拿到父叶子 PTE 的可变引用：既读 ppn 又（需要时）就地把
                // 父页转 CoW 只读，取代旧 translate()(读走表)+try_remap()(写走表)
                // 的二次走表 —— fork 的可写数据/堆/栈页是热路径，省一次 3 级走表/页。
                let parent_pte = self
                    .page_table
                    .find_pte_mut_cached(vpn, &mut pl0_cache)
                    .filter(|pte| pte.is_valid());
                if parent_pte.is_none() {
                    // 父 PTE 缺失/无效：父自身可能是懒建 PTE 的 fork 子进程，帧仍
                    // 在 frames 中（Arc 共享）——子必须照常继承，否则内容丢失。
                    while pframes.peek().is_some_and(|(k, _)| **k < vpn) {
                        pframes.next();
                    }
                    if let Some(frame_arc) = pframes
                        .peek()
                        .and_then(|(k, v)| (**k == vpn).then(|| (*v).clone()))
                    {
                        if overlaps_prev && new_space.page_table.translate(vpn).is_some() {
                            continue;
                        }
                        let ppn = frame_arc.ppn;
                        if crate::cmdline::frameprobe_enabled() && FPDBG_CLONE.fetch_add(1, Ordering::Relaxed) < 24 {
                            crate::println!(
                                "[FPDBG] clone-slow-nopte vpn={:#x} ppn={:#x} meta={} strong={}",
                                vpn.0, frame_arc.ppn.0, frame_arc.meta_ref(),
                                frame_arc.strong(),
                            );
                        }
                        child_frames.push((vpn, frame_arc));
                        if !lazy_child_pte {
                            let flags = if need_cow { cow_flags } else { vma.permission.to_pte_flags() };
                            if !new_space.page_table.try_map_cached(vpn, ppn, flags, &mut cl0_cache) {
                                return None;
                            }
                        }
                    }
                    continue;
                }
                if let Some(parent_pte) = parent_pte {
                    let ppn = parent_pte.ppn();
                    // 跳过子进程页表中已映射的（多 VMA 重叠）
                    if overlaps_prev && new_space.page_table.translate(vpn).is_some() {
                        continue;
                    }

                    while pframes.peek().is_some_and(|(k, _)| **k < vpn) {
                        pframes.next();
                    }
                    let frame_hit = pframes
                        .peek()
                        .and_then(|(k, v)| (**k == vpn).then(|| (*v).clone()));
                    if let Some(frame_arc) = frame_hit {
                        // 与快路径同理：登记的所有权必须与 PTE 实际映射的帧一致，
                        // 否则子进程登记的是「别的帧」，既错误共享又会错误释放。
                        let frame_arc = if frame_arc.ppn.0 != ppn.0 {
                            SharedFrame::from_ppn_inc(ppn, 0)
                        } else {
                            frame_arc
                        };
                        if crate::cmdline::frameprobe_enabled() && FPDBG_CLONE.fetch_add(1, Ordering::Relaxed) < 24 {
                            crate::println!(
                                "[FPDBG] clone-slow-hit vpn={:#x} ppn={:#x} meta={} strong={}",
                                vpn.0, frame_arc.ppn.0, frame_arc.meta_ref(),
                                frame_arc.strong(),
                            );
                        }
                        // 子进程共享同一物理页
                        child_frames.push((vpn, frame_arc));

                        if need_cow {
                            // 父 PTE 就地去掉 W/D（触发写时 CoW）——与 try_remap 等效：
                            // PageTableEntry::new 会补回 V|A，cow_flags 已去 W、D。
                            *parent_pte = PageTableEntry::new(ppn, cow_flags | PTEFlags::V | PTEFlags::A);
                            // 子 PTE 懒建：缺页时从 frames 补只读映射
                            if !lazy_child_pte
                                && !new_space.page_table.try_map_cached(vpn, ppn, cow_flags, &mut cl0_cache)
                            {
                                return None;
                            }
                        } else {
                            // 只读段直接共享，子 PTE 同样懒建
                            if !lazy_child_pte
                                && !new_space.page_table.try_map_cached(vpn, ppn, vma.permission.to_pte_flags(), &mut cl0_cache)
                            {
                                return None;
                            }
                        }
                    } else {
                        // MAP_SHARED / SysV SHM：页帧归段（SHM_TABLE / 页缓存）
                        // 所有，不在 vma.frames。Linux fork 对共享映射直接共享
                        // 物理页（copy_page_range 不复制、不去写权限）——子进程
                        // 映射同一 ppn、保留父 PTE 权限（含 SHM_RDONLY 去 W）。
                        // 深拷贝会破坏父子共享语义，且对 hugetlb 段整段复制
                        // 直接耗尽内存（hugeshmctl01 fork ENOMEM）。
                        if vma.vma_type == VmaType::Shared {
                            let shared_flags = parent_pte.flags()
                                & (PTEFlags::R | PTEFlags::W | PTEFlags::X | PTEFlags::U);
                            if !new_space.page_table.try_map_cached(
                                vpn,
                                ppn,
                                shared_flags,
                                &mut cl0_cache,
                            ) {
                                return None;
                            }
                            continue;
                        }
                        // 页帧不在当前 VMA 的 frames 中（可能是延迟分配的、或其他 VMA 管理的）
                        // 深拷贝此页作为兜底
                        let Some(new_frame) = frame_alloc_uninit() else {
                            return None;
                        };
                        let new_ppn = new_frame.ppn;
                        let src = ppn.as_bytes_mut();
                        let dst = new_ppn.as_bytes_mut();
                        dst.copy_from_slice(src);
                        if !new_space.page_table.try_map_cached(vpn, new_ppn, vma.permission.to_pte_flags(), &mut cl0_cache) {
                            return None;
                        }
                        child_frames.push((vpn, SharedFrame::from_alloc(new_frame)));
                    }
                }
            }

            child_vma.frames = FrameMap::from_sorted_vec(child_frames);

            // 父 VMA 也标记 cow
            if need_cow {
                vma.cow = true;
            }

            new_space.vmas.push(Box::new(child_vma));
        }

        // 重建子进程 mmap_bump：上面逐 VMA 直接 push 绕过了 add_vma 的 bump
        // 维护，若不重算则子进程 bump=0，auto_alloc 快路径会从 MMAP_BASE 撞上
        // 继承来的程序段（LA 程序基址 0x2000_0000 落在窗口内时触发写时 PME
        // 无限循环 hang；RV 基址在窗口外故不暴露）。
        {
            let (base, end) = Self::mmap_window();
            let mut bump = new_space.mmap_bump;
            for vma in &new_space.vmas {
                if vma.start.0 < end && vma.end.0 > base {
                    bump = bump.max(vma.end.0.min(end));
                }
            }
            new_space.mmap_bump = bump;
        }

        // 刷新父进程 TLB（PTE 权限已改变）
        crate::arch::asm_helpers::flush_tlb_all();

        Some(new_space)
    }

    /// 提前释放本地址空间持有的全部用户数据页帧（对标 Linux `do_exit` 早期的
    /// `exit_mm` → `mmput` → `exit_mmap`：进程一退出、在变成僵尸之前就把用户
    /// 内存还给系统，僵尸只保留 task 元数据）。
    ///
    /// 实现上仅 drop 各 VMA 持有的 `Arc<FrameTracker>`（清空 `frames` /
    /// `huge_frames`），由引用计数决定物理页命运：
    /// - 私有匿名页（唯一持有者）→ 物理页立即回收，帧池马上可复用；
    /// - 与父进程 CoW 共享、或 `SHARED_PAGE_CACHE` / `MAP_SHARED` 文件页（仍被
    ///   他处引用）→ 因 `Arc` 计数 > 1 而保留，不会误删别人还在用的页。
    ///
    /// 页表结构页（`PageTable::frames`）**不**在此释放——它们随 `AddressSpace`
    /// 在进程被 reap（`processes.remove`）时一并回收，彼时 `satp`/`pgdl` 已切走，
    /// 不存在 use-after-free；且本地址空间此后永不再被翻译（僵尸不会被调度，
    /// `wait` 读取的是父进程地址空间）。
    ///
    /// 语义与 reap 时 `Drop` 走的路径完全一致（同样是 drop 这些 `Arc`、同样不
    /// 做 hugetlb 预留账目回收），只是提早执行；故不引入任何新的账目偏差。
    /// 本方法幂等：`frames`/`huge_frames` 清空后重复调用为 no-op。
    pub fn release_user_frames(&mut self) {
        // 释放物理帧前，必须先清除指向它们的页表项并刷新 TLB。物理帧一旦归还
        // buddy 即可被内核堆/页表/其他进程复用；若本地址空间的 PTE 仍指向它，
        // 复用方写入的数据会经这条残留映射污染——表现为用户代码页被写入内核
        // 指针、随后取指得到非法指令（嵌套 trap → hang）。对标 Linux
        // `exit_mmap`：unmap 与 free 必须成对、原子完成，绝不留下悬空映射。
        //
        // 曾经的实现仅遍历 vma.frames.keys()，遗漏了 page table 中已映射但
        // 未在 VMA frames 中追踪的 PTE（例如 clone_cow 深拷贝兜底路径留在父
        // 地址空间的孤立映射）。这里按 **VMA vpn 区间** 清除：孤立映射同样落
        // 在其所属 VMA 的区间内（它们都是为 vma 范围内的缺页建立的），而
        // munmap/mremap 移除 VMA 时已同步清 PTE，故用户叶子 PTE 必在现存
        // VMA 区间并集之内。区间化后代价 = O(Σ区间页数)，不再对每张 L0 表
        // 全扫 512 项（整表 walk 曾占 fork+exit 循环 ~13% 指令数）。
        for (start, end) in self
            .vmas
            .iter()
            .map(|vma| (vma.start.floor(), vma.end.ceil()))
            .collect::<alloc::vec::Vec<_>>()
        {
            self.page_table.clear_user_range(start, end);
        }
        // sigreturn trampoline（Linux sigpage 对标）是固定虚地址的内核映射、
        // 不走 VMA：退出时显式清除其 PTE，保证地址空间销毁时无任何残留用户
        // 映射（此前 FRAMEDBG 全表扫描把它误报为「VMA 区间外泄漏」）。
        {
            let tramp_vpn = VirtAddr(crate::kernel::signal::sigreturn_trampoline_va()).floor();
            if self.page_table.translate(tramp_vpn).is_some() {
                self.page_table.unmap(tramp_vpn);
            }
        }
        // FRAME_DBG 取证：区间清除后整表复扫必须为 0，否则存在 VMA 区间外的
        // 用户映射（潜在 UAF 源头），当场曝光。
        if crate::mm::frame::frame_dbg() {
            let leaked = self.page_table.clear_all_user_leaf_ptes();
            if leaked != 0 {
                crate::println!(
                    "[VMA-RANGE-CLEAR] {} user PTEs outside all VMA ranges!",
                    leaked
                );
            }
        }
        crate::arch::asm_helpers::flush_tlb_all();
        // 页复用强序（对标 Linux tlb_finish_mmu）：flush_tlb_all 现为异步
        // shootdown，仅送达失效请求。这里的 vma.frames.clear() 会把用户帧归还
        // buddy，随后立刻被下一个进程（BuildStorm 的 fork/exec 风暴）复用；若某
        // 对端 hart 仍持有指向这些帧的陈旧 TLB，就会写坏新进程内存（实测 rustc
        // 新进程被踩 → SIGSEGV）。故在归还 buddy 前同步等所有对端完成失效。
        // 安全性：mm 锁为不关中断 SpinLock，兄弟 hart 等锁时 IRQ 开着可应答
        // shootdown IPI；exit 路径 de_thread 已先收尾兄弟线程。
        crate::mm::tlb::drain_shootdown();
        self.retired_frames.clear();
        self.user_frames_released = true;
        for vma in self.vmas.iter_mut() {
            // HugeTLB accounting on exit — Linux `exit_mmap` unmaps every VMA,
            // returning each mapping's pool reservation/commit exactly like
            // munmap. VMA-driven huge mappings (anonymous MAP_HUGETLB and
            // MAP_PRIVATE file) settle here; MAP_SHARED hugetlbfs mappings are
            // inode-owned and settle via `hugetlb_shared_*` on truncate/evict.
            let huge_inode_owned =
                vma.vma_type == VmaType::Shared && vma.backing.is_some();
            if vma.huge_page_size != 0 && !huge_inode_owned {
                let hpages = (vma.end.0 - vma.start.0) / vma.huge_page_size;
                // 同 munmap 路径：fork 后共持帧只由最后一个持有者归还计数。
                let touched = vma
                    .huge_frames
                    .iter()
                    .filter(|(_, frame)| frame.meta_ref() == 1)
                    .count();
                crate::mm::hugetlb_unmap(hpages, touched, vma.huge_reserved);
                vma.huge_page_size = 0;
            }
            if crate::cmdline::frameprobe_enabled() && FPDBG_REL.fetch_add(1, Ordering::Relaxed) < 24 {
                let mut shown = 0usize;
                for (vpn, f) in vma.frames.iter() {
                    if shown >= 3 {
                        break;
                    }
                    crate::println!(
                        "[FPDBG] release vpn={:#x} ppn={:#x} meta={} strong={}",
                        vpn.0, f.ppn.0, f.meta_ref(), f.strong(),
                    );
                    shown += 1;
                }
            }
            vma.frames.clear();
            vma.huge_frames.clear();
        }
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // 安全网：确保所有 PTE 在物理帧归还 buddy 前被清除。
        // 正常路径由 mmput → release_user_frames 处理；此处兜底防御
        // release_user_frames 未被调用的边角场景。已释放过则无需再整表
        // walk + 全局 TLB 刷新。
        if !self.user_frames_released {
            self.page_table.clear_all_user_leaf_ptes();
            crate::arch::asm_helpers::flush_tlb_all();
        }
        // 复用前强序（exec 丢弃旧地址空间 / 最后一个 mm_users 归零走这里）：
        // 帧随后由 Rust 自动 drop 归还 buddy 并立刻被复用；drop 不在 mm 锁下
        // 运行（Arc 归零触发），故此处 drain 无锁环风险，安全等所有对端完成失效。
        // 这是 `rustc -vV` 新进程被踩（exec 旧 AS 帧复用）的强序兜底。
        crate::mm::tlb::drain_shootdown();
        // VMA 中的 FrameTracker 在此之后由 Rust 自动 drop 释放物理页
    }
}
