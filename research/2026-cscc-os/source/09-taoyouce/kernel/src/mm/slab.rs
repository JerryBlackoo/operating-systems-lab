//! SLUB 分配器 — 对标 Linux SLUB 的内核堆快速路径
//!
//! 设计要点（对标 Linux mm/slub.c）：
//!   - 每个 size class 有独立的 `SlabCache`，管理 partial slab 链表
//!   - 每个 slab 是一整页（4KiB），页首嵌入 `SlabPageHeader` 元数据
//!   - 空闲 slot 通过嵌入式 freelist（指针存在空闲 slot 自身内）O(1) 弹出/压入
//!   - 整页空闲时归还帧分配器（真回收，非只增不减）
//!   - 分配路径：percpu_cache O(1) → slab partial O(1) → new page O(1)
//!   - 释放路径：percpu_cache → slab freelist → 整页回收
//!
//! 锁序：SLUB → FRAME_ALLOCATOR（SLUB 内部需要取帧），不与 HEAP 嵌套。

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use crate::sync::{LocalIrqCell, SpinLock, TicketSpinLock};

/// SLUB 管理的大小类（字节），≤ 2048 走 slab 页
/// 更大的对象直接走老链表堆或帧分配器
const NUM_SLAB_CLASSES: usize = 8;
const SLAB_SIZE_CLASSES: [usize; NUM_SLAB_CLASSES] = [
    16, 32, 64, 128, 256, 512, 1024, 2048,
];

/// 最大 slab 管理的对象大小
pub const MAX_SLAB_OBJ_SIZE: usize = 2048;

/// Slab 页魔数，用于 dealloc 时识别 slab 页 vs 老堆页
const SLAB_MAGIC: u32 = 0x534C_5542; // "SLUB" ASCII

/// 页大小，与 arch 一致
const PAGE_SIZE: usize = 4096;

/// 缓存行大小（对标 Linux SLUB cache coloring）
const CACHE_LINE_SIZE: usize = 64;

/// 全局 SLUB 是否已初始化
pub static SLUB_INITIALIZED: AtomicBool = AtomicBool::new(false);

// ─────────────────── 嵌入式 freelist ───────────────────

/// 空闲 slot 内嵌指针（Linux SLUB 同款：freelist 存在对象自身首 8 字节）
#[repr(C)]
struct FreeSlot {
    next: *mut FreeSlot,
}

// ─────────────────── Slab 页头 ───────────────────

/// 嵌入在每个 slab 页起始处的元数据
///
/// 对标 Linux `struct slab`。含 frozen 标记支持 per-CPU frozen slab。
#[repr(C)]
struct SlabPageHeader {
    /// 魔数 SLAB_MAGIC，dealloc 路由用
    magic: u32,
    /// 所属 size class 索引 (0..NUM_SLAB_CLASSES)
    cache_idx: u16,
    /// 已分配给用户代码的 slot 数（对标 Linux inuse）
    nr_allocated: u16,
    /// 总 slot 数（页内可用 slot 数）
    nr_total: u16,
    /// 此 slab 占用的页数的 order（单页 slab = 0）
    page_order: u8,
    /// 是否被某个 CPU 冻结（对标 Linux frozen 标记）
    /// frozen slab 不在全局 partial 链表上，由 CPU 独占
    frozen: u8,
    /// 页内首个 slot 的偏移（缓存着色后）。dealloc 用其校验
    /// 指针确实指向本页某个合法 slot，而不是复用页里残留 magic
    /// 的用户缓冲（否则会把用户数据当成 freelist next 推入链）。
    first_slot: u16,
    /// 空闲 slot 链表头（兓当两个角色）
    /// - 非 frozen 时：所有空闲 slot 的链表
    /// - frozen 时：跨 CPU 归还的空闲 slot 链表（CPU 的 freelist 另存）
    free_list: *mut FreeSlot,
    /// partial 双向链表 —— next
    next: *mut SlabPageHeader,
    /// partial 双向链表 —— prev
    prev: *mut SlabPageHeader,
}

// SAFETY: SlabPageHeader 只在持 SLUB 锁或 per-CPU 锁时访问
unsafe impl Send for SlabPageHeader {}

impl SlabPageHeader {
    /// 判断此 slab 是否完全空闲（可整页回收）
    #[inline]
    fn is_empty(&self) -> bool {
        self.nr_allocated == 0
    }

    /// 判断此 slab 是否已满（无空闲 slot）
    #[inline]
    fn is_full(&self) -> bool {
        self.free_list.is_null()
    }

    /// 从 slab 分配一个 slot，返回指针；slab 满返回 None
    fn alloc_slot(&mut self) -> Option<*mut u8> {
        if self.free_list.is_null() {
            return None;
        }
        let slot = self.free_list;
        unsafe {
            self.free_list = (*slot).next;
        }
        self.nr_allocated += 1;
        Some(slot as *mut u8)
    }

    /// 归还一个 slot 到 slab
    ///
    /// # Safety
    /// `ptr` 必须是本 slab 分配出去的 slot 地址
    unsafe fn dealloc_slot(&mut self, ptr: *mut u8) {
        dbg_assert_slab_owner(self as *const _ as usize, "dealloc_slot");
        let slot = ptr as *mut FreeSlot;
        (*slot).next = self.free_list;
        self.free_list = slot;
        self.nr_allocated -= 1;
    }
}

// ─────────────────── SlabCache：单个 size class ───────────────────

/// 一个 size class 的 slab 缓存，管理该大小的所有 slab 页
struct SlabCache {
    /// 对象大小（字节）
    object_size: usize,
    /// partial slab 双向链表头（有空闲 slot 的 slab）
    partial: *mut SlabPageHeader,
    /// partial 链表中的 slab 数
    nr_partial: usize,
    /// 总 slab 页数（含 full）
    nr_total_slabs: usize,
    /// 最少保留的 partial slab 数（对标 Linux SLUB min_partial，防 thrashing）
    min_partial: usize,
    /// 累计分配次数（统计用）
    alloc_count: u64,
    /// 累计释放次数（统计用）
    dealloc_count: u64,
    /// 累计回收页数
    reclaim_count: u64,
    /// 缓存着色计数器（对标 Linux SLUB cache coloring，减少 L1 cache set 冲突）
    color_next: usize,
    /// 最大缓存着色偏移（字节）
    color_range: usize,
}

// SAFETY: 只在持锁时访问
unsafe impl Send for SlabCache {}

impl SlabCache {
    const fn new(object_size: usize) -> Self {
        // 对标 Linux SLUB: 小对象保留更多空 slab 防 thrashing
        // Linux set_min_partial(): clamp(ilog2(size)/2, MIN_PARTIAL=5, MAX_PARTIAL=10).
        // 保 5 页空 slab 上限成本 <20KiB/class，换取网络/RR 类 alloc-free 抖动
        // 负载下不再反复走 buddy 归还/重取整页（页元数据清理 + memset 均省掉）。
        let min_partial = if object_size <= 256 { 5 } else { 4 };
        // 缓存着色：用 header 后的对齐空派做偏移变化
        // 每个新 slab 页的 slot 起始地址偏移一个 cache line，减少多 slab 页间的 cache set 冲突
        // color_range = 最多能偏移几个 cache line（用 1 个 slot 的空间换偏移多样性）
        let color_range = if object_size >= CACHE_LINE_SIZE {
            // 大对象本身已跨多 cache line，着色意义不大
            0
        } else {
            // 最多用 1 个 slot 的空间做着色，换取 (object_size / CACHE_LINE_SIZE) 个偏移位置
            // 比如 16B 对象: 可以有 0,16,32,48 四个偏移
            (CACHE_LINE_SIZE / object_size).saturating_sub(1) * object_size
        };
        Self {
            object_size,
            partial: core::ptr::null_mut(),
            nr_partial: 0,
            nr_total_slabs: 0,
            min_partial,
            alloc_count: 0,
            dealloc_count: 0,
            reclaim_count: 0,
            color_next: 0,
            color_range,
        }
    }

    /// 初始化一个新 slab 页并加入 partial 链表
    ///
    /// `page_addr` 是 slab 页的虚拟地址（从帧分配器获取）
    /// 缓存着色：每个新 slab 页的 slot 起始地址偏移不同，减少 L1 cache set 冲突
    fn init_slab_page(&mut self, page_addr: usize) {
        let header_size = core::mem::size_of::<SlabPageHeader>();
        let base_first_slot = align_up(header_size, self.object_size);

        // 缓存着色：在 base 基础上加 color_next 偏移
        let colored_first_slot = base_first_slot + self.color_next;
        let nr_slots = (PAGE_SIZE - colored_first_slot) / self.object_size;

        // 推进着色计数器（循环）
        if self.color_range > 0 {
            self.color_next += self.object_size;
            if self.color_next > self.color_range {
                self.color_next = 0;
            }
        }

        // 在页首写入 SlabPageHeader
        let header = page_addr as *mut SlabPageHeader;
        let cache_idx = size_to_slab_class(self.object_size)
            .unwrap_or(0) as u16;

        unsafe {
            (*header).magic = SLAB_MAGIC;
            (*header).cache_idx = cache_idx;
            (*header).nr_allocated = 0;
            (*header).nr_total = nr_slots as u16;
            (*header).page_order = 0; // 单页 slab
            (*header).frozen = 0;
            (*header).first_slot = colored_first_slot as u16;
            (*header).free_list = core::ptr::null_mut();
            (*header).next = core::ptr::null_mut();
            (*header).prev = core::ptr::null_mut();

            // 构建 freelist：从最后一个 slot 往前链，使分配时从低地址开始
            let mut prev: *mut FreeSlot = core::ptr::null_mut();
            for i in (0..nr_slots).rev() {
                let slot_addr = page_addr + colored_first_slot + i * self.object_size;
                let slot = slot_addr as *mut FreeSlot;
                (*slot).next = prev;
                prev = slot;
            }
            (*header).free_list = prev;
        }

        // 加入 partial 链表
        self.add_to_partial(header);
        self.nr_total_slabs += 1;
    }

    /// 释放对象回 slab
    ///
    /// 返回 Some(ppn) 如果有空 slab 页需要回收（调用方须在释放 SLUB 锁后归还帧）
    ///
    /// # Safety
    /// `ptr` 必须是此 cache 分配出的对象
    unsafe fn dealloc(&mut self, ptr: *mut u8) -> Option<super::PhysPageNum> {
        let page_addr = (ptr as usize) & !(PAGE_SIZE - 1);
        let header = page_addr as *mut SlabPageHeader;

        let was_full = (*header).is_full();

        (*header).dealloc_slot(ptr);
        self.dealloc_count += 1;

        if was_full {
            // slab 从 full 变为 partial，加回 partial 链表
            self.add_to_partial(header);
        }

        // 整页空闲 → 根据 min_partial 决定是否回收
        // 对标 Linux SLUB: 保留 min_partial 个空 slab 防止反复 alloc/free 页帧（thrashing）
        if (*header).is_empty() {
            if self.nr_partial > self.min_partial {
                if !verify_reclaim_dbg(header, 1) {
                    // 探针：检出活对象，暂不回收（保页防污染），定位用
                    return None;
                }
                self.remove_from_partial(header);
                self.nr_total_slabs -= 1;
                self.reclaim_count += 1;
                slab_trace(
                    TR_RECLAIM,
                    (*header).cache_idx as usize,
                    page_addr,
                    0,
                    (*header).nr_total as usize,
                );

                // 清除魔数，防止悬挂引用
                (*header).magic = 0;

                let paddr = kernel_vaddr_to_paddr(page_addr);
                return Some(super::PhysPageNum(paddr >> 12));
            }
            // else: 保留在 partial 链表中供后续分配复用
        }
        None
    }

    // ── partial 链表操作（双向链表，O(1) 插入/删除） ──

    fn add_to_partial(&mut self, slab: *mut SlabPageHeader) {
        unsafe {
            (*slab).next = self.partial;
            (*slab).prev = core::ptr::null_mut();
            if !self.partial.is_null() {
                (*self.partial).prev = slab;
            }
            self.partial = slab;
        }
        self.nr_partial += 1;
    }

    fn remove_from_partial(&mut self, slab: *mut SlabPageHeader) {
        unsafe {
            let prev = (*slab).prev;
            let next = (*slab).next;
            if !prev.is_null() {
                (*prev).next = next;
            } else {
                self.partial = next;
            }
            if !next.is_null() {
                (*next).prev = prev;
            }
            (*slab).prev = core::ptr::null_mut();
            (*slab).next = core::ptr::null_mut();
        }
        self.nr_partial -= 1;
    }

    /// 从 partial 链表取出第一个 slab 页（用于 freeze 给 CPU）
    fn take_partial(&mut self) -> Option<*mut SlabPageHeader> {
        let slab = self.partial;
        if slab.is_null() {
            return None;
        }
        self.remove_from_partial(slab);
        Some(slab)
    }

    /// 跨 CPU 释放对象到 frozen slab 的 page.free_list
    ///
    /// frozen slab 不修改 nr_allocated（避免与 owning CPU 的数据竞争）。
    /// nr_allocated 在 unfreeze 时通过计数 freelist 重建。
    ///
    /// # Safety
    /// `ptr` 必须属于 `page` 所管理的 slab，调用方必须持有 SLUB 锁
    unsafe fn cross_cpu_free(&mut self, page: *mut SlabPageHeader, ptr: *mut u8) {
        dbg_assert_slab_owner(page as usize, "cross_cpu_free");
        let slot = ptr as *mut FreeSlot;
        (*slot).next = (*page).free_list;
        (*page).free_list = slot;
        // 不修改 nr_allocated：frozen slab 的 nr_allocated 由 unfreeze 重建
        self.dealloc_count += 1;
    }
}

// ─────────────────── SlubAllocator：全局 SLUB ───────────────────

/// 全局 SLUB 分配器
pub struct SlubAllocator {
    caches: [SlabCache; NUM_SLAB_CLASSES],
}

// SAFETY: 受 SpinLock 保护
unsafe impl Send for SlubAllocator {}

impl SlubAllocator {
    const fn new() -> Self {
        Self {
            caches: [
                SlabCache::new(SLAB_SIZE_CLASSES[0]),
                SlabCache::new(SLAB_SIZE_CLASSES[1]),
                SlabCache::new(SLAB_SIZE_CLASSES[2]),
                SlabCache::new(SLAB_SIZE_CLASSES[3]),
                SlabCache::new(SLAB_SIZE_CLASSES[4]),
                SlabCache::new(SLAB_SIZE_CLASSES[5]),
                SlabCache::new(SLAB_SIZE_CLASSES[6]),
                SlabCache::new(SLAB_SIZE_CLASSES[7]),
            ],
        }
    }

    /// 为指定 size class 添加新 slab 页
    fn add_slab_page(&mut self, class: usize, page_addr: usize) {
        self.caches[class].init_slab_page(page_addr);
    }

    /// 释放对象
    ///
    /// 返回 Some(ppn) 如果有空 slab 页需要回收
    ///
    /// # Safety
    /// `ptr` 必须是 SLUB 分配出的对象
    unsafe fn dealloc(&mut self, ptr: *mut u8) -> Option<super::PhysPageNum> {
        let page_addr = (ptr as usize) & !(PAGE_SIZE - 1);
        let header = page_addr as *const SlabPageHeader;
        let cache_idx = (*header).cache_idx as usize;
        if cache_idx < NUM_SLAB_CLASSES {
            return self.caches[cache_idx].dealloc(ptr);
        }
        None
    }

    /// 获取统计信息
    pub fn stats(&self) -> SlubStats {
        let mut total_slabs = 0usize;
        let mut total_partial = 0usize;
        let mut total_allocs = 0u64;
        let mut total_deallocs = 0u64;
        let mut total_reclaims = 0u64;

        for cache in &self.caches {
            total_slabs += cache.nr_total_slabs;
            total_partial += cache.nr_partial;
            total_allocs += cache.alloc_count;
            total_deallocs += cache.dealloc_count;
            total_reclaims += cache.reclaim_count;
        }

        SlubStats {
            total_slabs,
            total_partial,
            total_allocs,
            total_deallocs,
            total_reclaims,
        }
    }
}

/// SLUB 统计信息
pub struct SlubStats {
    pub total_slabs: usize,
    pub total_partial: usize,
    pub total_allocs: u64,
    pub total_deallocs: u64,
    pub total_reclaims: u64,
}

/// 全局 SLUB 实例（保护 SlabCache 的 partial 链表和 full slab 列表）。
/// FIFO ticket 锁：per-CPU frozen-slab 快路径通常不碰它，慢路径/跨核 free
/// 的争用由 ticket 排队，避免 TTAS 重试风暴。
static SLUB: TicketSpinLock<SlubAllocator> = TicketSpinLock::new(SlubAllocator::new());

// ─────────────────── Per-CPU Frozen Slab（对标 Linux kmem_cache_cpu）───────────────────

use super::percpu_cache::MAX_CPUS;

/// 单个 size class 在一个 CPU 上的 frozen slab 状态
///
/// 对标 Linux `struct kmem_cache_cpu`：
/// - `freelist`：从 frozen slab 偷来的 freelist，per-CPU 独占，无需全局锁
/// - `page`：当前 frozen slab 页指针
struct CpuSlabEntry {
    freelist: *mut FreeSlot,
    page: *mut SlabPageHeader,
}

unsafe impl Send for CpuSlabEntry {}

impl CpuSlabEntry {
    const fn empty() -> Self {
        Self {
            freelist: core::ptr::null_mut(),
            page: core::ptr::null_mut(),
        }
    }

    #[inline]
    fn has_freelist(&self) -> bool {
        !self.freelist.is_null()
    }
}

/// 单个 CPU 的所有 size class 的 frozen slab 状态
struct PerCpuSlabs {
    classes: [CpuSlabEntry; NUM_SLAB_CLASSES],
}

impl PerCpuSlabs {
    const fn new() -> Self {
        Self {
            classes: [
                CpuSlabEntry::empty(), CpuSlabEntry::empty(),
                CpuSlabEntry::empty(), CpuSlabEntry::empty(),
                CpuSlabEntry::empty(), CpuSlabEntry::empty(),
                CpuSlabEntry::empty(), CpuSlabEntry::empty(),
            ],
        }
    }
}

/// 全局 per-CPU frozen slab 状态
/// 每个 CPU 有自己的 SpinLock（对标 Linux 的 per-CPU 变量 + preempt_disable）
/// 同一 CPU 内不存在跨 hart 竞争，但 timer 等中断路径会重入分配器，
/// 必须用 IrqTicketSpinLock 关闭本地中断：普通 SpinLock 会被 timer 打断后
/// 同一 hart 重入 CPU_SLABS[cpu] 自锁（RV BuildStorm 7 核卡 POSIX_TIMERS、
/// 持锁者卡自身 slab 锁的 ABBA 根因）。
/// per-CPU frozen-slab 状态（Linux `kmem_cache_cpu` 的零原子等价实现）。
///
/// 这里必须用 `LocalIrqCell` 而不是任何自旋锁：CPU_SLABS[cpu] **只被
/// cpu 自己访问**（跨 CPU free 在全局 SLUB 锁内挂 page.free_list，不碰
/// 目标 CPU 的 CPU_SLABS），因此 per-CPU 变量 + 关本地中断就是完整语义，
/// 原子 ticket/TTAS 只会白白付 RMW 与串行化代价。
static CPU_SLABS: [LocalIrqCell<PerCpuSlabs>; MAX_CPUS] =
    [const { LocalIrqCell::new(PerCpuSlabs::new()) }; MAX_CPUS];

// ─────────────────── 公共接口 ───────────────────

/// 初始化 SLUB 分配器
///
/// 必须在帧分配器初始化之后调用。初始化后，GlobalAlloc 会自动路由
/// ≤2048B 的分配到 SLUB。
pub fn init() {
    // 预分配一些 slab 页给高频 size class，减少运行时 slow path
    let prealloc_classes: [(usize, usize); 4] = [
        (0, 4), // 16B: 4 pages
        (1, 2), // 32B: 2 pages
        (2, 2), // 64B: 2 pages
        (3, 1), // 128B: 1 page
    ];

    for &(class, pages) in &prealloc_classes {
        for _ in 0..pages {
            if let Some(page_addr) = alloc_slab_page() {
                SLUB.lock().add_slab_page(class, page_addr);
            }
        }
    }

    SLUB_INITIALIZED.store(true, Ordering::Release);
    crate::println!(
        "[slub] SLUB allocator initialized: {} size classes (16B..2048B), per-CPU frozen slab enabled",
        NUM_SLAB_CLASSES
    );
}

/// [DEBUG 插桩] 在向 slab 页写 freelist/对象前，校验该物理页 MetaSlot owner 仍为 Slab。
/// 若 owner 已变（User/Free/...），说明此页已被 reclaim 回 buddy 并改派他用，
/// 而 slab 仍持悬挂指针往里写 = stale-write 污染用户页的铁证。
#[inline]
fn dbg_assert_slab_owner(page_addr: usize, site: &str) {
    if !crate::mm::uaf_dbg_enabled() {
        return;
    }
    let paddr = kernel_vaddr_to_paddr(page_addr);
    let ppn = super::PhysPageNum(paddr >> 12);
    if let Some(slot) = super::page_meta::try_meta_of(ppn) {
        let owner = slot.get_owner();
        if owner != super::page_meta::FrameOwner::Slab {
            crate::println!(
                "[SLAB-STALE-WRITE] site={} ppn={:#x} page_addr={:#x} owner={:?} rc={} map_count={} cpu={} \
                 — slab writing into page no longer owned by slab! ROOT CAUSE",
                site, ppn.0, page_addr, owner, slot.ref_count(), slot.map_count(),
                super::percpu_cache::current_cpu_id(),
            );
        }
    }
}

/// 诊断：检查 slab 返回的指针是否落在 MetaSlot owner=Slab 的页上
#[inline]
fn slab_validate_ptr(ptr: *mut u8, class: usize) {
    if !crate::mm::uaf_dbg_enabled() {
        return;
    }
    let paddr = kernel_vaddr_to_paddr(ptr as usize);
    let ppn = super::PhysPageNum(paddr >> 12);
    let owner = super::page_meta::meta_of(ppn).get_owner();
    if owner != super::page_meta::FrameOwner::Slab {
        let rc = super::page_meta::meta_of(ppn).ref_count();
        panic!(
            "[SLAB-CROSS] slub_alloc class={} returned ptr={:#x} ppn={:#x} but MetaSlot owner={:?} rc={} — free-list corrupted!",
            class, ptr as usize, ppn.0, owner, rc
        );
    }
}

// ─────────────────── [DEBUG] 对象级双重分配/双重释放追踪 ───────────────────
//
// 现有 [SLAB-CROSS]/[SLAB-STALE-WRITE] 只校验"页 owner 是否仍为 Slab"，抓不到
// **同一 slab 对象在同页内被二次发放**（页 owner 不变）。iperf 取证显示扇区缓存
// 的 512B 堆缓冲（class 5）被 pid18 内核栈指针覆盖 = 同一对象既归缓存又归他人。
// 这里用静态开放寻址集合跟踪**全部 class** 的当前存活对象地址，alloc 时若已存在
// = 双重分配铁证；free 时若不存在 = 双重释放/野指针释放。无堆分配、独立自旋锁。

const DBG_SET_CAP: usize = 2 << 20; // 2M 槽：全 class 对象级追踪（class1/2/5 均观测到坏链）
const DBG_SET_MASK: usize = DBG_SET_CAP - 1;

struct DbgAllocSet {
    /// 0 = empty, aligned pointer = live, pointer | 1 = freed.
    slots: [usize; DBG_SET_CAP],
    last_free_ra: [usize; DBG_SET_CAP],
    last_alloc_ra: [usize; DBG_SET_CAP],
    /// 每个槽所属 slab class（同一地址跨 class 复用也能被点名）。
    class: [u8; DBG_SET_CAP],
}

static DBG_ALLOC_SET: SpinLock<DbgAllocSet> = SpinLock::new(DbgAllocSet {
    slots: [0usize; DBG_SET_CAP],
    last_free_ra: [0usize; DBG_SET_CAP],
    last_alloc_ra: [0usize; DBG_SET_CAP],
    class: [0u8; DBG_SET_CAP],
});
static DBG_TABLE_FULL: AtomicBool = AtomicBool::new(false);

/// 对象级双重分配/释放追踪开关。与 FRAMEDBG 一样用 bootarg 门控：
/// 正式 BuildStorm/judge 轮不带 `SLABDBG=1`，每次检查只付一次 Relaxed 读。
static SLAB_DBG_ON: AtomicBool = AtomicBool::new(false);

// ── SLABDBG 事件环：全 class 追踪抓到 DBL-ALLOC 时，打印触发前每个 CPU 的
// alloc/free/drain/freeze/unfreeze 序列，直接还原重复链是怎么形成的。 ──
const SLAB_TRACE_CAP: usize = 4096;
const SLAB_TRACE_MASK: usize = SLAB_TRACE_CAP - 1;

// op 编码：alloc=1/2/3/4，free=5/6/7，merge/reclaim=8/9
const TR_FAST_ALLOC: u64 = 1;
const TR_DRAIN_ALLOC: u64 = 2;
const TR_FREEZE_ALLOC: u64 = 3;
const TR_NEWPAGE_ALLOC: u64 = 4;
const TR_FAST_FREE: u64 = 5;
const TR_CROSS_FREE: u64 = 6;
const TR_SLOW_FREE: u64 = 7;
const TR_UNFREEZE: u64 = 8;
const TR_RECLAIM: u64 = 9;

#[derive(Clone, Copy)]
struct SlabTraceEvent {
    t: u64,
    op: u64,
    class: u64,
    cpu: u64,
    pid: u64,
    page: u64,
    ptr: u64,
    arg: u64,
}

impl SlabTraceEvent {
    const fn zero() -> Self {
        Self { t: 0, op: 0, class: 0, cpu: 0, pid: 0, page: 0, ptr: 0, arg: 0 }
    }
}

static mut SLAB_TRACE_RINGS: [[SlabTraceEvent; SLAB_TRACE_CAP]; MAX_CPUS] =
    [[SlabTraceEvent::zero(); SLAB_TRACE_CAP]; MAX_CPUS];
static SLAB_TRACE_IDX: [AtomicU32; MAX_CPUS] =
    [const { AtomicU32::new(0) }; MAX_CPUS];

#[inline(always)]
fn slab_trace(op: u64, class: usize, page: usize, ptr: usize, arg: usize) {
    if !slab_dbg() {
        return;
    }
    let cpu = super::percpu_cache::current_cpu_id();
    if cpu >= MAX_CPUS {
        return;
    }
    let was_enabled = crate::arch::is_interrupt_enabled();
    if was_enabled {
        crate::arch::disable_interrupts();
    }
    let seq = SLAB_TRACE_IDX[cpu].fetch_add(1, Ordering::Relaxed);
    let e = unsafe { &mut SLAB_TRACE_RINGS[cpu][(seq as usize) & SLAB_TRACE_MASK] };
    e.t = crate::arch::asm_helpers::rdtime();
    e.op = op;
    e.class = class as u64;
    e.cpu = cpu as u64;
    e.pid = crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed) as u64;
    e.page = page as u64;
    e.ptr = ptr as u64;
    e.arg = arg as u64;
    core::sync::atomic::fence(Ordering::Release);
    if was_enabled {
        crate::arch::enable_interrupts();
    }
}

fn slab_trace_dump() {
    for cpu in 0..MAX_CPUS {
        let seq = SLAB_TRACE_IDX[cpu].load(Ordering::Acquire) as usize;
        if seq == 0 {
            continue;
        }
        let begin = seq.saturating_sub(80).min(seq);
        let end = seq;
        crate::console::raw_emergency_print(format_args!(
            "[SLABTRACE] cpu={} events [{}..{})\n",
            cpu, begin, end,
        ));
        for n in begin..end {
            let e = unsafe { &SLAB_TRACE_RINGS[cpu][n & SLAB_TRACE_MASK] };
            if e.t == 0 && e.op == 0 {
                continue;
            }
            crate::console::raw_emergency_print(format_args!(
                "[SLABTRACE] cpu={} n={} t={} op={} class={} pid={} page={:#x} ptr={:#x} arg={:#x}\n",
                cpu, n, e.t, e.op, e.class, e.pid, e.page, e.ptr, e.arg,
            ));
        }
    }
}


#[inline(always)]
fn slab_dbg() -> bool {
    SLAB_DBG_ON.load(Ordering::Relaxed)
}

/// 由 cmdline 解析结果设置对象级 SLUB 追踪开关。
pub fn set_slab_dbg(on: bool) {
    SLAB_DBG_ON.store(on, Ordering::Relaxed);
}

#[inline(always)]
fn dbg_caller_ra() -> usize {
    let ra: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("mv {}, ra", out(reg) ra, options(nomem, nostack)); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("move {}, $ra", out(reg) ra, options(nomem, nostack)); }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    { ra = 0; }
    ra
}

fn dbg_dump_stack_candidates(tag: &str, ptr: usize) {
    let sp: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("move {}, $sp", out(reg) sp, options(nomem, nostack)); }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    { sp = 0; }

    unsafe extern "C" {
        static stext: u8;
        static etext: u8;
    }
    let text_start = unsafe { &stext as *const u8 as usize };
    let text_end = unsafe { &etext as *const u8 as usize };
    let page_end = (sp + PAGE_SIZE) & !(PAGE_SIZE - 1);
    crate::println!(
        "[SLABDBG] tag={} ptr={:#x} sp={:#x} text=[{:#x},{:#x})",
        tag, ptr, sp, text_start, text_end,
    );
    let mut p = sp;
    while p + core::mem::size_of::<usize>() <= page_end {
        let candidate = unsafe { core::ptr::read_unaligned(p as *const usize) };
        if candidate >= text_start && candidate < text_end {
            crate::println!(
                "[SLABDBG-STACK] off={:#x} addr={:#x}",
                p - sp, candidate,
            );
        }
        p += core::mem::size_of::<usize>();
    }
}

fn dbg_track_alloc(ptr: usize, class: usize, caller_ra: usize) {
    if ptr == 0 || DBG_TABLE_FULL.load(Ordering::Relaxed) { return; }
    let mut s = DBG_ALLOC_SET.lock();
    let mut i = (ptr >> 9) & DBG_SET_MASK;
    let mut probes = 0;
    loop {
        let v = s.slots[i];
        if v == 0 {
            s.slots[i] = ptr;
            s.last_alloc_ra[i] = caller_ra;
            s.class[i] = class as u8;
            return;
        }
        if (v & !1usize) == ptr {
            if v & 1 != 0 {
                s.slots[i] = ptr;
                s.last_alloc_ra[i] = caller_ra;
                s.class[i] = class as u8;
                return;
            }
            let first_ra = s.last_alloc_ra[i];
            let first_class = s.class[i] as usize;
            drop(s);
            let cpu = super::percpu_cache::current_cpu_id();
            let pid = crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed);
            crate::println!(
                "[SLAB-DBL-ALLOC] ptr={:#x} first_class={} class={} cpu={} pid={} first_ra={:#x} second_ra={:#x}",
                ptr, first_class, class, cpu, pid, first_ra, caller_ra,
            );
            dbg_dump_stack_candidates("double-alloc", ptr);
            slab_trace_dump();
            panic!("SLUB double allocation (all classes tracked)");
        }
        i = (i + 1) & DBG_SET_MASK;
        probes += 1;
        if probes >= DBG_SET_CAP {
            drop(s);
            if !DBG_TABLE_FULL.swap(true, Ordering::Relaxed) {
                crate::console::raw_emergency_print(format_args!(
                    "[SLABDBG] tracking table full; tracking disabled (no panic)\n",
                ));
            }
            return;
        }
    }
}

fn dbg_track_free(ptr: usize, class: usize, caller_ra: usize) {
    if ptr == 0 || DBG_TABLE_FULL.load(Ordering::Relaxed) { return; }
    let mut s = DBG_ALLOC_SET.lock();
    let mut i = (ptr >> 9) & DBG_SET_MASK;
    let mut probes = 0;
    loop {
        let v = s.slots[i];
        if v != 0 && (v & !1usize) == ptr {
            if v & 1 == 0 {
                if s.class[i] as usize != class {
                    let first_ra = s.last_alloc_ra[i];
                    let first_class = s.class[i] as usize;
                    drop(s);
                    let cpu = super::percpu_cache::current_cpu_id();
                    let pid = crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed);
                    crate::println!(
                        "[SLAB-CLASS-MISMATCH] ptr={:#x} alloc_class={} free_class={} cpu={} pid={} alloc_ra={:#x} free_ra={:#x}",
                        ptr, first_class, class, cpu, pid, first_ra, caller_ra,
                    );
                    dbg_dump_stack_candidates("class-mismatch-free", ptr);
                    slab_trace_dump();
                    panic!("SLUB class mismatch free (page reused while object live?)");
                }
                s.slots[i] = ptr | 1;
                s.last_free_ra[i] = caller_ra;
                return;
            }
            let previous_ra = s.last_free_ra[i];
            let alloc_ra = s.last_alloc_ra[i];
            let tracked_class = s.class[i] as usize;
            drop(s);
            let cpu = super::percpu_cache::current_cpu_id();
            let pid = crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed);
            crate::println!(
                "[SLAB-DBL-FREE] ptr={:#x} class={} cpu={} pid={} alloc_ra={:#x} previous_ra={:#x} caller_ra={:#x}",
                ptr, tracked_class, cpu, pid, alloc_ra, previous_ra, caller_ra,
            );
            dbg_dump_stack_candidates("double-free", ptr);
            slab_trace_dump();
            panic!("SLUB double free (all classes tracked)");
        }
        if v == 0 {
            drop(s);
            let cpu = super::percpu_cache::current_cpu_id();
            let pid = crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed);
            crate::println!(
                "[SLAB-UNTRACKED-FREE] ptr={:#x} class={} cpu={} pid={} caller_ra={:#x}",
                ptr, class, cpu, pid, caller_ra,
            );
            dbg_dump_stack_candidates("untracked-free", ptr);
            slab_trace_dump();
            panic!("SLUB untracked free (all classes tracked)");
        }
        i = (i + 1) & DBG_SET_MASK;
        probes += 1;
        if probes >= DBG_SET_CAP {
            drop(s);
            if !DBG_TABLE_FULL.swap(true, Ordering::Relaxed) {
                crate::console::raw_emergency_print(format_args!(
                    "[SLABDBG] tracking table full; tracking disabled (no panic)\n",
                ));
            }
            return;
        }
    }
}

/// SLUB 分配入口
///
/// 对标 Linux SLUB 的 `___slab_alloc` 三级路径：
/// 1. per-CPU frozen slab freelist（O(1)，仅 per-CPU 锁，无全局锁）
/// 2. frozen slab 的跨 CPU 归还 freelist（drain cross-CPU frees）
/// 3. 从全局 partial 取新 slab 或分配新页
pub fn slub_alloc(size: usize, _align: usize) -> Option<*mut u8> {
    let class = size_to_slab_class(size)?;
    let cpu_id = super::percpu_cache::current_cpu_id();

    // ── FAST PATH：从 CPU 的 frozen slab freelist 弹出（仅 per-CPU 锁，无全局锁）──
    // 不修改 page.nr_allocated：避免与 SLUB 锁下的 cross_cpu_free 竞争
    // nr_allocated 在 unfreeze 时通过计数 freelist 精确重建
    let mut fast_ptr: Option<*mut u8> = None;
    {
        let mut cpu = CPU_SLABS[cpu_id].lock();
        let entry = &mut cpu.classes[class];
        if entry.has_freelist() {
            let slot = entry.freelist;
            if slab_dbg() && (slot as usize) < 0x8000_0000 {
                crate::console::raw_emergency_print(format_args!(
                    "[SLABDBG-CORRUPT] fastpath class={} cpu={} slot={:#x} page={:#x}
",
                    class, cpu_id, slot as usize, entry.page as usize,
                ));
                slab_trace_dump();
                panic!("slub freelist corrupted before deref");
            }
            unsafe {
                let next = (*slot).next;
                entry.freelist = next;
            }
            slab_trace(TR_FAST_ALLOC, class, entry.page as usize, slot as usize, entry.freelist as usize);
            fast_ptr = Some(slot as *mut u8);
        }
    }
    // 对象跟踪必须在 CPU_SLABS 锁外做：alloc 若在持 CPU 锁时取 DBG 跟踪锁，
    // 与 free 的 DBG→CPU 顺序构成 ABBA（SLABDBG jobs8 复现实测 8 核冻死）。
    if let Some(ptr) = fast_ptr {
        slab_validate_ptr(ptr, class);
        if slab_dbg() {
            dbg_track_alloc(ptr as usize, class, dbg_caller_ra());
        }
        return Some(ptr);
    }
    // per-CPU 锁已释放

    // ── SLOW PATH ──
    let result = slub_alloc_slowpath(class, cpu_id);
    if let Some(ptr) = result {
        slab_validate_ptr(ptr, class);
        if slab_dbg() {
            dbg_track_alloc(ptr as usize, class, dbg_caller_ra());
        }
    }
    result
}

/// SLABDBG：扫描一条 freelist 是否已包含 slot（防二次释放/循环链）。
/// 遇到坏指针立即留证 panic，绝不在坏指针上解引用。
#[inline]
fn freelist_contains(
    head: *mut FreeSlot, slot: *mut FreeSlot,
    class: usize, cpu: usize, page: usize, site: &str,
) -> bool {
    let mut p = head;
    let mut n = 0usize;
    while !p.is_null() {
        let ppn = kernel_vaddr_to_paddr(p as usize) >> 12;
        let owner_ok = crate::mm::page_meta::try_meta_of(super::PhysPageNum(ppn))
            .map_or(false, |m| m.get_owner() == crate::mm::page_meta::FrameOwner::Slab);
        if (p as usize) < 0x8000_0000
            || (p as usize) % core::mem::align_of::<FreeSlot>() != 0
            || !owner_ok
        {
            crate::console::raw_emergency_print(format_args!(
                "[SLABDBG-CORRUPT] scan site={} class={} cpu={} page={:#x} head={:#x} bad={:#x} slot={:#x} owner_ok={} caller={:#x}
",
                site, class, cpu, page, head as usize, p as usize, slot as usize, owner_ok, dbg_caller_ra(),
            ));
            slab_trace_dump();
            panic!("slub freelist corrupted during scan");
        }
        if p == slot {
            return true;
        }
        n += 1;
        if n > (1usize << 20) {
            return false; // 循环链：不在这里死循环
        }
        unsafe { p = (*p).next; }
    }
    false
}

/// SLABDBG：返回两条 freelist 的重叠节点（用于 unfreeze 前诊断）。
fn freelist_overlap(a: *mut FreeSlot, b: *mut FreeSlot) -> Option<*mut FreeSlot> {
    let mut p = a;
    let mut guard = 0usize;
    while !p.is_null() && guard < (1usize << 20) {
        let mut q = b;
        let mut guard2 = 0usize;
        while !q.is_null() && guard2 < (1usize << 20) {
            if p == q {
                return Some(p);
            }
            unsafe { q = (*q).next; }
            guard2 += 1;
        }
        unsafe { p = (*p).next; }
        guard += 1;
    }
    None
}

/// SLABDBG：计数一条 freelist（带坏指针/超限保护）。
fn freelist_count(head: *mut FreeSlot) -> Option<u32> {
    let mut p = head;
    let mut n = 0u32;
    while !p.is_null() {
        let ppn = kernel_vaddr_to_paddr(p as usize) >> 12;
        let owner_ok = crate::mm::page_meta::try_meta_of(super::PhysPageNum(ppn))
            .map_or(false, |m| m.get_owner() == crate::mm::page_meta::FrameOwner::Slab);
        if (p as usize) < 0x8000_0000 || !owner_ok {
            return None;
        }
        n += 1;
        if n > 200_000 {
            return None;
        }
        unsafe { p = (*p).next; }
    }
    Some(n)
}

/// SLUB 分配慢路径
///
/// 1. 尝试 drain frozen slab 的跨 CPU 归还 freelist
/// 2. 如无，unfreeze 当前 slab，从全局 partial 取新 slab 并 freeze
/// 3. 如全局也无，分配新页
fn slub_alloc_slowpath(class: usize, cpu_id: usize) -> Option<*mut u8> {
    // 1. 检查 frozen slab 的 page.free_list（跨 CPU 归还的对象）
    // 需同时持 per-CPU 锁 + SLUB 锁，避免与 cross_cpu_free 对 page.free_list 的数据竞争
    // 锁序：per-CPU → SLUB（始终一致，无死锁风险）
    {
        let mut cpu = CPU_SLABS[cpu_id].lock();
        let entry = &mut cpu.classes[class];
        if !entry.page.is_null() {
            let _slub = SLUB.lock(); // 保护 page.free_list 访问
            unsafe {
                dbg_assert_slab_owner(entry.page as usize, "slowpath_drain");
                let cross_fl = (*entry.page).free_list;
                if !cross_fl.is_null() {
                    (*entry.page).free_list = core::ptr::null_mut();
                    entry.freelist = cross_fl;
                    let slot = entry.freelist;
                    if slab_dbg() && (slot as usize) < 0x8000_0000 {
                        crate::console::raw_emergency_print(format_args!(
                            "[SLABDBG-CORRUPT] slowdrain class={} cpu={} slot={:#x} page={:#x}
",
                            class, cpu_id, slot as usize, entry.page as usize,
                        ));
                        slab_trace_dump();
                        panic!("slub cross freelist corrupted before deref");
                    }
                    entry.freelist = (*slot).next;
                    slab_trace(
                        TR_DRAIN_ALLOC,
                        class,
                        entry.page as usize,
                        slot as usize,
                        entry.freelist as usize,
                    );
                    return Some(slot as *mut u8);
                }
            }
        }
    }

    // 2. 当前 frozen slab 已耗尽，需要换一个新的
    //    先 unfreeze 当前 slab（归还全局 partial）
    unfreeze_cpu_slab(class, cpu_id);

    // 3. 从全局 partial 取一个 slab 来 freeze
    //    在 SLUB 锁内设 frozen=1，并在**同一临界区**内把 page.free_list 读出后
    //    清空（handoff）。若把 read+null 推迟到 step5 的 CPU 锁里，cross_cpu_free
    //    会在 SLUB 锁内并发向旧 page.free_list 压入对象，随后被 step5 的 null
    //    覆盖而丢失（SLABTRACE 实锤 lost free），也会与 entry.freelist 形成
    //    重复链。这里 SLUB 锁内的 read+null 与 cross_cpu_free 互斥。
    let mut from_partial = true;
    let frozen_head = {
        let mut slub = SLUB.lock();
        let page = slub.caches[class].take_partial();
        if let Some(p) = page {
            unsafe {
                (*p).frozen = 1;
                let fl = (*p).free_list;
                (*p).free_list = core::ptr::null_mut();
                Some((p, fl))
            }
        } else {
            from_partial = false;
            None
        }
    };

    let (new_page, frozen_fl) = match frozen_head {
        Some((p, fl)) => (p, fl),
        None => {
            // 4. 全局也没有 partial → 分配新页
            let page_addr = alloc_slab_page()?;
            let mut slub = SLUB.lock();
            slub.add_slab_page(class, page_addr);
            let page = slub.caches[class].take_partial()?;
            unsafe {
                (*page).frozen = 1;
                let fl = (*page).free_list;
                (*page).free_list = core::ptr::null_mut();
                (page, fl)
            }
        }
    };

    // 5. Freeze 新 slab 给当前 CPU
    //    page.free_list 已在 SLUB 临界区内完成 handoff；这里只把已持有的链
    //    挂到 per-CPU entry，不再触碰 page.free_list，cross_cpu_free 无竞态窗口。
    {
        let mut cpu = CPU_SLABS[cpu_id].lock();
        let entry = &mut cpu.classes[class];
        unsafe {
            entry.freelist = frozen_fl;
            entry.page = new_page;

            // 分配一个
            if entry.has_freelist() {
                let slot = entry.freelist;
                entry.freelist = (*slot).next;
                slab_trace(
                    if from_partial { TR_FREEZE_ALLOC } else { TR_NEWPAGE_ALLOC },
                    class,
                    new_page as usize,
                    slot as usize,
                    entry.freelist as usize,
                );
                return Some(slot as *mut u8);
            }
        }
    }

    None
}

/// Unfreeze 当前 CPU 的 frozen slab，归还全局
fn unfreeze_cpu_slab(class: usize, cpu_id: usize) {
    let (page, remaining_freelist) = {
        let mut cpu = CPU_SLABS[cpu_id].lock();
        let entry = &mut cpu.classes[class];
        if entry.page.is_null() {
            return;
        }
        let page = entry.page;
        let fl = entry.freelist;
        entry.page = core::ptr::null_mut();
        entry.freelist = core::ptr::null_mut();
        (page, fl)
    };
    // per-CPU 锁已释放

    // 可能需要回收的页
    let mut reclaim_ppn: Option<super::PhysPageNum> = None;

    {
        let mut slub = SLUB.lock();
        unsafe {
            dbg_assert_slab_owner(page as usize, "unfreeze_merge");
            slab_trace(
                TR_UNFREEZE,
                class,
                page as usize,
                remaining_freelist as usize,
                (*page).free_list as usize,
            );
            if slab_dbg() {
                if let Some(overlap) = freelist_overlap(remaining_freelist, (*page).free_list) {
                    crate::console::raw_emergency_print(format_args!(
                        "[SLABDBG-OVERLAP] unfreeze class={} cpu={} page={:#x} overlap={:#x} cpu_head={:#x} page_head={:#x}
",
                        class, cpu_id, page as usize, overlap as usize,
                        remaining_freelist as usize, (*page).free_list as usize,
                    ));
                    slab_trace_dump();
                    panic!("slub CPU and page freelists overlap before unfreeze");
                }
                let n = freelist_count(remaining_freelist).unwrap_or(0)
                      + freelist_count((*page).free_list).unwrap_or(0);
                if n > (*page).nr_total as u32 {
                    crate::console::raw_emergency_print(format_args!(
                        "[SLABDBG-OVERCOUNT] unfreeze class={} cpu={} page={:#x} free={} nr_total={}
",
                        class, cpu_id, page as usize, n, (*page).nr_total,
                    ));
                    slab_trace_dump();
                    panic!("slub free count exceeds nr_total before unfreeze");
                }
            }
            // 合并 CPU freelist 和 page.free_list（跨 CPU 归还的）
            if !remaining_freelist.is_null() {
                let mut tail = remaining_freelist;
                while !(*tail).next.is_null() {
                    tail = (*tail).next;
                }
                (*tail).next = (*page).free_list;
                (*page).free_list = remaining_freelist;
            }

            // 计数合并后的 freelist，重建 nr_allocated
            // （frozen 期间 nr_allocated 未维护，此处精确重建）
            let mut total_free: u16 = 0;
            let mut node = (*page).free_list;
            while !node.is_null() {
                total_free += 1;
                node = (*node).next;
            }
            (*page).nr_allocated = (*page).nr_total.saturating_sub(total_free);
            (*page).frozen = 0;

            // 判断 slab 状态
            if (*page).is_empty()
                && slub.caches[class].nr_partial > slub.caches[class].min_partial
                && verify_reclaim_dbg(page, 2)
            {
                slab_trace(
                    TR_RECLAIM,
                    class,
                    page as usize,
                    total_free as usize,
                    (*page).nr_total as usize,
                );
                slub.caches[class].nr_total_slabs -= 1;
                slub.caches[class].reclaim_count += 1;
                (*page).magic = 0;
                let paddr = kernel_vaddr_to_paddr(page as usize);
                reclaim_ppn = Some(super::PhysPageNum(paddr >> 12));
            } else if (*page).nr_allocated < (*page).nr_total {
                slub.caches[class].add_to_partial(page);
            }
            // else: 全满 → 不加入任何链表
        }
    }
    // SLUB 锁已释放

    if let Some(ppn) = reclaim_ppn {
        // MetaSlot: 清除 Slab 标记（dealloc_frame 内部会清 owner 并还给 buddy）
        super::frame::dealloc_frame(ppn);
    }
}

/// SLUB 释放入口
///
/// 对标 Linux SLUB 的 `__slab_free`：
/// - Fast path：对象属于当前 CPU 的 frozen slab → 直接 push 到 CPU freelist（无全局锁）
/// - Slow path：对象属于其他 slab → 通过全局锁归还
///
/// # Safety
/// `ptr` 必须是 SLUB 分配出的对象
pub unsafe fn slub_dealloc(ptr: *mut u8, _size: usize) {
    let page_addr = (ptr as usize) & !(PAGE_SIZE - 1);
    let page = page_addr as *mut SlabPageHeader;

    // 防御 1：页 owner 必须仍是 Slab。复用页即使页首残留 SLAB_MAGIC
    // 也绝不把用户缓冲推入 slab freelist（BuildStorm 坏指针
    // 0x3539…"xz-2059" 即用户数据被误当 next 的化石）。
    let paddr = kernel_vaddr_to_paddr(page_addr);
    let ppn = super::PhysPageNum(paddr >> 12);
    let owner_ok = crate::mm::page_meta::try_meta_of(ppn)
        .map_or(false, |m| m.get_owner() == crate::mm::page_meta::FrameOwner::Slab);
    if !owner_ok || (*page).magic != SLAB_MAGIC {
        crate::println!(
            "[SLAB-UAF] slub_dealloc ptr={:#x} page={:#x} magic={:#x} owner_ok={} caller={:#x} — skipping",
            ptr as usize, page_addr, (*page).magic, owner_ok, dbg_caller_ra()
        );
        return;
    }

    let cache_idx = (*page).cache_idx as usize;
    if cache_idx >= NUM_SLAB_CLASSES {
        return;
    }

    // 防御 2：ptr 必须是本页合法 slot。first_slot 记录缓存着色后的首个
    // slot 偏移；任何指向对象中部/复用页残留地址的 free 都会被拒绝。
    let obj_size = SLAB_SIZE_CLASSES[cache_idx];
    let off = (ptr as usize) - page_addr;
    let first = (*page).first_slot as usize;
    if off < first || off >= PAGE_SIZE || (off - first) % obj_size != 0 {
        crate::println!(
            "[SLAB-UAF] slub_dealloc bad slot ptr={:#x} page={:#x} class={} off={:#x} first={:#x} obj_size={} caller={:#x} — skipping",
            ptr as usize, page_addr, cache_idx, off, first, obj_size, dbg_caller_ra()
        );
        return;
    }

    if slab_dbg() {
        dbg_track_free(ptr as usize, cache_idx, dbg_caller_ra());
    }

    let cpu_id = super::percpu_cache::current_cpu_id();

    // ── FAST PATH：对象属于本 CPU 的 frozen slab ──
    {
        let mut cpu = CPU_SLABS[cpu_id].lock();
        let entry = &mut cpu.classes[cache_idx];
        if entry.page == page {
            // 直接 push 到 CPU 本地 freelist（O(1)，无全局锁）
            // 不修改 page.nr_allocated：避免与 SLUB 锁下的 cross_cpu_free 竞争
            dbg_assert_slab_owner(page_addr, "fastpath_dealloc");
            let slot = ptr as *mut FreeSlot;
            if slab_dbg() && freelist_contains(entry.freelist, slot, cache_idx, cpu_id, page_addr, "fastpath") {
                crate::console::raw_emergency_print(format_args!(
                    "[SLABDBG-DOUBLEFREE] fastpath class={} cpu={} ptr={:#x} page={:#x} caller={:#x}
",
                    cache_idx, cpu_id, ptr as usize, page_addr, dbg_caller_ra(),
                ));
                slab_trace_dump();
                panic!("slub double free detected on CPU freelist");
            }
            let old_head = entry.freelist;
            (*slot).next = entry.freelist;
            entry.freelist = slot;
            slab_trace(
                TR_FAST_FREE,
                cache_idx,
                page_addr,
                ptr as usize,
                old_head as usize,
            );
            return;
        }
    }
    // per-CPU 锁已释放

    // ── SLOW PATH：对象属于其他 slab（或其他 CPU 的 frozen slab）──
    let reclaim_ppn = {
        let mut slub = SLUB.lock();
        if slab_dbg() {
            let slot = ptr as *mut FreeSlot;
            let head = if (*page).frozen != 0 {
                (*page).free_list
            } else {
                (*page).free_list
            };
            if freelist_contains(head, slot, cache_idx, cpu_id, page_addr, "slowpath") {
                crate::console::raw_emergency_print(format_args!(
                    "[SLABDBG-DOUBLEFREE] slowpath class={} cpu={} ptr={:#x} page={:#x} frozen={} caller={:#x}
",
                    cache_idx, cpu_id, ptr as usize, page_addr, (*page).frozen, dbg_caller_ra(),
                ));
                slab_trace_dump();
                panic!("slub double free detected on page freelist");
            }
        }
        if (*page).frozen != 0 {
            // 跨 CPU 归还到 frozen slab 的 page.free_list
            // frozen slab 不能回收（CPU 还持有它），nr_allocated 由 unfreeze 重建
            let old_head = (*page).free_list;
            slub.caches[cache_idx].cross_cpu_free(page, ptr);
            slab_trace(
                TR_CROSS_FREE,
                cache_idx,
                page_addr,
                ptr as usize,
                old_head as usize,
            );
            None
        } else {
            // 普通 slab（在 partial 或 full 链表上）
            let old_head = (*page).free_list;
            let reclaim = slub.caches[cache_idx].dealloc(ptr);
            slab_trace(
                TR_SLOW_FREE,
                cache_idx,
                page_addr,
                ptr as usize,
                old_head as usize,
            );
            reclaim
        }
    };
    // SLUB 锁已释放

    if let Some(ppn) = reclaim_ppn {
        // MetaSlot: dealloc_frame 内部会清 owner 并还给 buddy
        super::frame::dealloc_frame(ppn);
    }
}

/// 判断一个地址是否属于 SLUB 分配的 slab 页
///
/// 通过检查页首的 SLAB_MAGIC 魔数识别。
pub fn is_slab_page(ptr: *const u8) -> bool {
    if !SLUB_INITIALIZED.load(Ordering::Acquire) {
        return false;
    }
    let page_addr = (ptr as usize) & !(PAGE_SIZE - 1);
    if page_addr == 0 {
        return false;
    }
    unsafe {
        let header = page_addr as *const SlabPageHeader;
        if (*header).magic != SLAB_MAGIC {
            return false;
        }
    }
    let paddr = kernel_vaddr_to_paddr(page_addr);
    let ppn = super::PhysPageNum(paddr >> 12);
    crate::mm::page_meta::try_meta_of(ppn)
        .map_or(false, |m| m.get_owner() == crate::mm::page_meta::FrameOwner::Slab)
}

/// 获取 SLUB 统计信息
pub fn stats() -> SlubStats {
    SLUB.lock().stats()
}

/// 获取指定大小对应的 slab slot 大小（用于 GlobalAlloc 对齐检查）
#[inline]
pub fn slab_class_size(size: usize) -> Option<usize> {
    size_to_slab_class(size).map(|c| SLAB_SIZE_CLASSES[c])
}

// ─────────────────── 内部辅助 ───────────────────

/// 将请求大小映射到 slab size class 索引
fn size_to_slab_class(size: usize) -> Option<usize> {
    for (i, &class_size) in SLAB_SIZE_CLASSES.iter().enumerate() {
        if size <= class_size {
            return Some(i);
        }
    }
    None
}

/// 对齐到指定边界（向上取整）
fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}

/// 回收前核对：走一遍页内 free_list，统计真实空闲 slot 数。若它 != nr_total，
/// 说明此页仍有 LIVE 对象，却因 nr_allocated 记账错误被判为空、正要归还 buddy ——
/// 这正是「slab 页携带活内核对象被复用为用户页」的根因。返回 true=安全可回收。
///
/// # Safety
/// `header` 必须指向一个有效的 slab 页头，调用方须持相应锁。
unsafe fn verify_reclaim_dbg(header: *mut SlabPageHeader, site: u32) -> bool {
    if !crate::mm::uaf_dbg_enabled() && !slab_dbg() {
        return true;
    }
    let nr_total = (*header).nr_total;
    let cnt = match freelist_count((*header).free_list) {
        Some(n) => n,
        None => {
            crate::println!(
                "[SLABLEAK] reclaim site={} page={:#x} cache_idx={} nr_total={} — free_list is CORRUPTED, refusing reclaim",
                site, header as usize, (*header).cache_idx, nr_total,
            );
            return false;
        }
    };
    if cnt != nr_total as u32 {
        crate::println!(
            "[SLABLEAK] reclaim site={} page={:#x} cache_idx={} nr_total={} free_list_len={} nr_allocated={} -> {} LIVE object(s) on page about to be freed to buddy",
            site, header as usize, (*header).cache_idx, nr_total, cnt, (*header).nr_allocated,
            (nr_total as u32).saturating_sub(cnt)
        );
        return false;
    }
    true
}

/// 从帧分配器获取一整页用于 slab
///
/// 返回内核线性映射虚拟地址。不清零（slab 会用 freelist 覆盖）。
fn alloc_slab_page() -> Option<usize> {
    // 走 frame::alloc_frame() 取 order-0 页，它会设置引用计数=1 并清零
    let ppn = super::frame::alloc_frame()?;
    // MetaSlot 标记为 Slab 用途（alloc_frame 默认设 User，此处精确化），
    // 并写入 allocator cookie 供 GlobalAlloc::dealloc O(1) 路由。
    let slot = crate::mm::page_meta::meta_of(ppn);
    slot.set_owner(crate::mm::page_meta::FrameOwner::Slab);
    slot.set_allocator(crate::mm::page_meta::AllocCookie::Slab);
    Some(ppn.kernel_addr())
}

/// 将内核虚拟地址转换回物理地址
///
/// RISC-V64: 恒等映射，vaddr == paddr
/// LoongArch64: 去掉 DMW0 前缀
#[inline]
fn kernel_vaddr_to_paddr(vaddr: usize) -> usize {
    #[cfg(target_arch = "riscv64")]
    { vaddr }
    #[cfg(target_arch = "loongarch64")]
    { vaddr & 0x0000_FFFF_FFFF_FFFF }
}
