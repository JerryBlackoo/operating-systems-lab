//! Per-inode page cache with true LRU eviction.
//!
//! This module implements a global page cache that stores physical pages
//! keyed by `(inode_id, page_index)`. It mirrors the Linux kernel's
//! `struct address_space` page cache, providing:
//!
//! - **Per-inode granularity**: Pages are associated with their backing inode,
//!   so multiple processes mapping the same file share cached pages.
//! - **True LRU eviction**: When memory pressure requires eviction, the least
//!   recently used pages are evicted first (not lowest sector number like the
//!   old sector cache).
//! - **Dynamic sizing**: The cache grows to use available free memory and
//!   shrinks under memory pressure, matching Linux's behavior where the page
//!   cache occupies all free RAM.
//! - **Readahead support**: The `readahead` method pre-populates multiple
//!   consecutive pages in a single I/O batch, amortizing disk access latency.
//!
//! # Integration
//!
//! - **Demand paging (MAP_PRIVATE)**: On page fault, the fault handler checks
//!   the page cache first. On hit, it copies the cached page to a new private
//!   frame (COW semantics for private mappings). On miss, it reads from disk,
//!   inserts into cache, then copies.
//! - **MAP_SHARED**: Shared mappings directly reference the cached frame (no
//!   copy). All processes mapping the same file share the same physical page.
//! - **Readahead**: When faulting page N, the handler also reads pages N+1
//!   through N+RA_PAGES into the cache, so subsequent faults are cache hits.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use crate::sync::TicketSpinLock;
use super::frame::{frame_alloc, SharedFrame};
use super::PAGE_SIZE;

/// Default readahead window: 32 pages = 128KB (matches Linux default).
///
/// 缺页时按此窗口批量读入并**预映射**后续连续文件页，把「每页一次 trap+
/// 单页 virtio 读」摊薄成「每窗口一次」。对顺序访问的 ELF/动态库加载收益巨大：
/// glibc `/bin/sh`(动态 busybox + ld.so + libc.so ~2MB) 原本每次启动要 ~512 次
/// 缺页+512 次单页磁盘读，是 la-glibc lmbench `lat_proc shell` 慢到拖死整组的
/// 主因。窗口内已映射/越界/EOF 页由 readahead/install_readahead_pages 自动跳过，
/// 不会越界或重复读。
pub const DEFAULT_READAHEAD_PAGES: usize = 32;

/// Minimum free pages before eviction kicks in. When free memory drops
/// below this threshold, the cache starts evicting LRU entries.
const MIN_FREE_PAGES_THRESHOLD: usize = 512; // 2MB minimum free

/// Maximum number of eviction attempts per insert to avoid infinite loops
/// when all cached pages are still referenced.
const MAX_EVICTION_ATTEMPTS: usize = 64;

/// A single cached page entry.
struct CacheEntry {
    /// The physical frame holding this page's data.
    frame: SharedFrame,
    /// Whether this page has been modified (for writeback on eviction).
    dirty: bool,
    /// Access sequence number — the entry's current position in the LRU
    /// index (`lru[seq] == key`). Bumped to a fresh value on every access.
    seq: u64,
}

/// Global page cache with lazy LRU eviction.
///
/// 命中路径只把单调 clock 写进 entry.seq（零额外树操作）；LRU 序仅在驱逐时
/// 需要，届时对 entries 做一次线性扫描选出最旧的一批。旧实现维护独立的
/// `seq → key` BTreeMap 索引，每次命中都要 remove+insert 两次 O(log n) 树操作
/// ——pchist 实证 BTreeMap insert/remove/bulk_steal 合计是 RV/LA minibuild
/// 内核前 10 热点（~2e8 insns），而驱逐只在内存压力下发生（罕见、可批量摊销）。
struct PageCacheInner {
    /// Key → cached frame mapping.
    entries: BTreeMap<(usize, usize), CacheEntry>,
    /// Monotonic access clock; each access assigns the next value.
    clock: u64,
    /// Statistics.
    hits: u64,
    misses: u64,
    evictions: u64,
    insertions: u64,
}

impl PageCacheInner {
    const fn new() -> Self {
        Self {
            entries: BTreeMap::new(),
            clock: 0,
            hits: 0,
            misses: 0,
            evictions: 0,
            insertions: 0,
        }
    }

    /// Assign a fresh sequence number. O(1)——LRU 序惰性维护，驱逐时才排序。
    fn bump_seq(&mut self) -> u64 {
        let seq = self.clock;
        self.clock = self.clock.wrapping_add(1);
        seq
    }

    /// 线性扫描选出最旧的至多 `k` 个 key（驱逐专用；k 小，简单插入选择）。
    fn oldest_keys(&self, k: usize) -> Vec<(usize, usize)> {
        let mut top: Vec<(u64, (usize, usize))> = Vec::with_capacity(k + 1);
        for (key, e) in self.entries.iter() {
            if top.len() < k {
                top.push((e.seq, *key));
                if top.len() == k {
                    top.sort_unstable_by_key(|t| core::cmp::Reverse(t.0));
                }
            } else if e.seq < top[0].0 {
                top[0] = (e.seq, *key);
                let mut i = 0;
                while i + 1 < top.len() && top[i].0 < top[i + 1].0 {
                    top.swap(i, i + 1);
                    i += 1;
                }
            }
        }
        top.into_iter().map(|t| t.1).collect()
    }

    /// Look up a page in the cache. Returns a cloned handle if found
    /// （clone = MetaSlot +1，新持有者计入引用）。
    /// Moves the entry to the back of LRU (most recently used).
    fn get(&mut self, inode_id: usize, page_index: usize) -> Option<SharedFrame> {
        let key = (inode_id, page_index);
        let seq = self.bump_seq();
        match self.entries.get_mut(&key) {
            Some(entry) => {
                entry.seq = seq;
                self.hits += 1;
                Some(entry.frame.clone())
            }
            None => {
                self.misses += 1;
                None
            }
        }
    }

    /// Insert or update a page in the cache.
    fn insert(&mut self, inode_id: usize, page_index: usize, frame: SharedFrame) {
        let key = (inode_id, page_index);
        let seq = self.bump_seq();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.frame = frame;
            entry.seq = seq;
            return;
        }
        self.maybe_evict();
        self.entries.insert(key, CacheEntry { frame, dirty: false, seq });
        self.insertions += 1;
    }

    /// Insert only if not already present. Returns the cached frame.
    fn get_or_insert_with<F>(&mut self, inode_id: usize, page_index: usize, f: F) -> Option<SharedFrame>
    where
        F: FnOnce() -> Option<SharedFrame>,
    {
        let key = (inode_id, page_index);
        let seq = self.bump_seq();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.seq = seq;
            self.hits += 1;
            return Some(entry.frame.clone());
        }
        self.misses += 1;
        let frame = f()?;
        self.maybe_evict();
        let shared = frame.clone();
        self.entries.insert(key, CacheEntry { frame, dirty: false, seq });
        self.insertions += 1;
        Some(shared)
    }

    /// Mark a cached page as dirty (for future writeback).
    fn mark_dirty(&mut self, inode_id: usize, page_index: usize) {
        let key = (inode_id, page_index);
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.dirty = true;
        }
    }

    /// Remove a specific page from the cache (e.g., on truncate or invalidate).
    fn remove(&mut self, inode_id: usize, page_index: usize) -> Option<SharedFrame> {
        let key = (inode_id, page_index);
        self.entries.remove(&key).map(|entry| entry.frame)
    }

    /// Remove all cached pages for a given inode (e.g., on file close/delete).
    fn invalidate_inode(&mut self, inode_id: usize) {
        let keys_to_remove: Vec<(usize, usize)> = self.entries
            .range((inode_id, 0)..=(inode_id, usize::MAX))
            .map(|(k, _)| *k)
            .collect();
        for key in &keys_to_remove {
            self.entries.remove(key);
        }
    }

    /// Mark key as most recently used. O(log n) 查找 + O(1) seq 更新。
    fn touch_lru(&mut self, key: (usize, usize)) {
        let seq = self.bump_seq();
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.seq = seq;
        }
    }

    /// Evict LRU entries if memory is tight.
    fn maybe_evict(&mut self) {
        let should_evict = self.should_evict();
        if !should_evict {
            return;
        }
        // 一次线性扫描选出最旧一批（把 O(n) 扫描摊到整批驱逐上）。
        for key in self.oldest_keys(MAX_EVICTION_ATTEMPTS) {
            if !self.should_evict() {
                break;
            }
            self.entries.remove(&key);
            self.evictions += 1;
        }
    }

    /// Determine whether eviction is needed based on free memory.
    fn should_evict(&self) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let (_, free_bytes) = super::get_memory_info();
        let free_pages = free_bytes as usize / PAGE_SIZE;
        free_pages < MIN_FREE_PAGES_THRESHOLD
    }

    /// Current number of cached pages.
    fn len(&self) -> usize {
        self.entries.len()
    }

    /// Cache statistics: (hits, misses, evictions, insertions, current_size).
    fn stats(&self) -> (u64, u64, u64, u64, usize) {
        (self.hits, self.misses, self.evictions, self.insertions, self.entries.len())
    }
}

/// The global page cache instance.
static PAGE_CACHE: TicketSpinLock<PageCacheInner> = TicketSpinLock::new(PageCacheInner::new());

// ============ Public API ============

/// Look up a cached page for the given inode and page index.
/// Returns a cloned handle on cache hit（clone = MetaSlot +1）。
pub fn lookup(inode_id: usize, page_index: usize) -> Option<SharedFrame> {
    PAGE_CACHE.lock().get(inode_id, page_index)
}

/// Insert a page into the cache.
pub fn insert(inode_id: usize, page_index: usize, frame: SharedFrame) {
    PAGE_CACHE.lock().insert(inode_id, page_index, frame);
}

/// Look up or create a cached page. On miss, calls `populate_fn` to create
/// and fill the page, then inserts it into the cache.
pub fn get_or_populate<F>(inode_id: usize, page_index: usize, populate_fn: F) -> Option<SharedFrame>
where
    F: FnOnce() -> Option<SharedFrame>,
{
    PAGE_CACHE.lock().get_or_insert_with(inode_id, page_index, populate_fn)
}

/// Mark a cached page as dirty.
pub fn mark_dirty(inode_id: usize, page_index: usize) {
    PAGE_CACHE.lock().mark_dirty(inode_id, page_index);
}

/// Remove a specific page from the cache.
pub fn remove(inode_id: usize, page_index: usize) -> Option<SharedFrame> {
    PAGE_CACHE.lock().remove(inode_id, page_index)
}

/// Remove all cached pages for a given inode.
pub fn invalidate_inode(inode_id: usize) {
    PAGE_CACHE.lock().invalidate_inode(inode_id);
}

/// Get cache statistics: (hits, misses, evictions, insertions, current_size).
pub fn stats() -> (u64, u64, u64, u64, usize) {
    PAGE_CACHE.lock().stats()
}

/// Current number of cached pages.
pub fn cached_page_count() -> usize {
    PAGE_CACHE.lock().len()
}

/// 内存压力应急回收：从 LRU 头部逐条驱逐页缓存直到释放出约 `target_pages`
/// 个可归还 buddy 的帧（仅统计无外部引用的条目）或缓存清空。
///
/// 供帧分配失败兜底路径调用（对标 Linux direct reclaim）：页缓存会长到
/// 吃满空闲内存，大块连续分配（hugetlb）或长跑批次末尾的普通分配必须
/// 能把它压回去。用 try_lock：若调用链已持有页缓存锁（get_or_populate 的
/// populate 闭包内分配失败）则直接返回 0，避免死锁。
pub fn emergency_reclaim(target_pages: usize) -> usize {
    let Some(mut cache) = PAGE_CACHE.try_lock() else {
        return 0;
    };
    let mut freed = 0usize;
    // 全量收集 (seq,key) 升序驱逐——应急路径罕见，O(n log n) 可接受。
    let mut order: Vec<(u64, (usize, usize))> =
        cache.entries.iter().map(|(k, e)| (e.seq, *k)).collect();
    order.sort_unstable_by_key(|t| t.0);
    for (_seq, key) in order {
        if freed >= target_pages {
            break;
        }
        if let Some(entry) = cache.entries.remove(&key) {
            cache.evictions += 1;
            if entry.frame.meta_ref() == 1 {
                freed += 1;
            }
        }
    }
    freed
}

/// Perform readahead: pre-populate `count` consecutive pages starting from
/// `start_page_index` for the given inode. Reads from the backing file
/// using the provided `read_fn` closure.
///
/// `read_fn(file_offset, buf)` should read `buf.len()` bytes from the file
/// at the given offset, returning the number of bytes actually read.
///
/// Returns the number of pages successfully read ahead.
pub fn readahead<F>(
    inode_id: usize,
    start_page_index: usize,
    count: usize,
    file_size: usize,
    read_fn: F,
) -> usize
where
    F: Fn(usize, &mut [u8]) -> usize,
{
    let mut pages_read = 0;
    for i in 0..count {
        let page_index = start_page_index + i;
        let file_offset = page_index * PAGE_SIZE;
        if file_offset >= file_size {
            break;
        }
        // Skip if already cached
        {
            let mut cache = PAGE_CACHE.lock();
            if cache.entries.contains_key(&(inode_id, page_index)) {
                cache.touch_lru((inode_id, page_index));
                pages_read += 1;
                continue;
            }
        }
        // Allocate frame and read from file (outside cache lock)
        let Some(frame) = frame_alloc() else {
            break;
        };
        // MetaSlot: 标记为 PageCache 用途
        crate::mm::page_meta::meta_of(frame.ppn).set_owner(crate::mm::page_meta::FrameOwner::PageCache);
        let kva = frame.ppn.as_bytes_mut();
        // Zero the page first (security: prevent data leakage)
        for b in kva.iter_mut() {
            *b = 0;
        }
        let avail = core::cmp::min(PAGE_SIZE, file_size - file_offset);
        let mut total_read = 0usize;
        while total_read < avail {
            let n = read_fn(file_offset + total_read, &mut kva[total_read..avail]);
            if n == 0 {
                break;
            }
            total_read += n;
        }
        // 短读/读失败：本页未完全 uptodate，**绝不能进页缓存**——否则后续
        // 任何缺页都会把零尾当文件内容映射给用户（观测形态：mmap 视图与
        // pread 间歇不一致、rustc parser 拿到被截断的源码）。对标 Linux
        // readpage 失败页不建立 uptodate 页；帧随作用域 drop 归还 buddy，
        // 并停止继续 readahead（后续页大概率同样失败）。
        if total_read < avail {
            break;
        }
        // [RAREAD] forensic probe (UAFDBG-gated): an all-zero page here means
        // the read produced a hole-like page; if this is a code page it
        // surfaces as executing zeros.
        if crate::mm::uaf_dbg_enabled() {
            let first = u32::from_le_bytes([kva[0], kva[1], kva[2], kva[3]]);
            if first == 0 {
                crate::println!(
                    "[RAREAD] inode={} pidx={} off={:#x} avail={} got={} first={:#x}",
                    inode_id, page_index, file_offset, avail, total_read, first
                );
            }
        }
        // Insert into cache
        let shared = SharedFrame::from_alloc(frame);
        PAGE_CACHE.lock().insert(inode_id, page_index, shared);
        pages_read += 1;
    }
    pages_read
}
