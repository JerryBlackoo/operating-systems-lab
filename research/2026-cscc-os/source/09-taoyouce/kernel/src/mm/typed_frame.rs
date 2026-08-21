//! Type-safe frame handles (对标 Asterinas/OSTD Frame<M> 设计)
//!
//! 通过 Rust 泛型类型参数在 **编译期** 区分帧用途，配合 `page_meta::MetaSlot`
//! 的运行时 `ref_count` + `owner` 形成双重防御：
//!
//! - 编译期：`Frame<KernelStackMeta>` 与 `Frame<PageCacheMeta>` 是不同类型，
//!   不可能在同一集合中混用，不可能把一个当另一个传给函数。
//! - 运行时：`MetaSlot.ref_count` 保证 alloc 时帧必须是 UNUSED 状态，
//!   free 时 ref_count 归零才还给 buddy，double-free 立即检测。
//!
//! # 句柄类型
//!
//! - [`UniqueFrame<M>`]：独占所有权（不可 clone）。可 `&mut` 访问元数据。
//!   用于内核栈、slab 页、页表页等内核独占资源。
//! - [`Frame<M>`]：引用计数共享（类似 Arc）。clone = ref_count++，drop = ref_count--。
//!   用于用户页（CoW fork 共享）、页缓存（多进程映射同一文件页）。
//! - [`Segment<M>`]：连续多帧句柄。用于 `alloc_frames(order)` 分配的连续物理块
//!   （内核栈、DMA 缓冲区等）。
//!
//! # 类型转换
//!
//! - `UniqueFrame<M>` → `Frame<M>`：单向降级（`into_shared`），独占 → 共享。
//! - `Frame<M>` → `UniqueFrame<M>`：不支持（需要运行时验证唯一引用，用 `try_into_unique`）。

use core::marker::PhantomData;

use super::PhysPageNum;
use super::page_meta::{self, FrameOwner, MetaSlot};

// ── 帧元数据 Trait ──────────────────────────────────────────────────────

/// 所有帧元数据类型必须实现的 trait。
///
/// 每种帧用途（内核栈、slab、页表、页缓存、DMA 等）定义一个 struct 并实现
/// 此 trait，提供：
/// - `OWNER`：运行时用途标记（写入 MetaSlot.owner）
/// - `on_drop`：帧最终释放时的清理回调（可选）
pub trait FrameMeta: Send + Sync + 'static {
    /// 帧用途标记（运行时写入 MetaSlot.owner）。
    const OWNER: FrameOwner;

    /// 帧最终释放时的清理回调。
    ///
    /// 默认为空操作。实现方可重写以执行特定清理（如 slab 回收统计等）。
    /// 此回调在帧归还 buddy 之前调用。
    fn on_drop(_ppn: PhysPageNum) {}
}

// ── 具体帧元数据类型 ────────────────────────────────────────────────────

/// 用户匿名页元数据（MAP_ANONYMOUS / CoW 页 / 堆栈页）。
pub struct UserPageMeta;
impl FrameMeta for UserPageMeta {
    const OWNER: FrameOwner = FrameOwner::User;
}

/// 内核栈元数据。
pub struct KernelStackMeta;
impl FrameMeta for KernelStackMeta {
    const OWNER: FrameOwner = FrameOwner::KernelStack;
}

/// Slab 分配器页元数据。
pub struct SlabMeta;
impl FrameMeta for SlabMeta {
    const OWNER: FrameOwner = FrameOwner::Slab;
}

/// 页表结构页元数据。
pub struct PageTableMeta;
impl FrameMeta for PageTableMeta {
    const OWNER: FrameOwner = FrameOwner::PageTable;
}

/// 页缓存元数据（文件 backed page）。
pub struct PageCacheMeta;
impl FrameMeta for PageCacheMeta {
    const OWNER: FrameOwner = FrameOwner::PageCache;
}

/// 内核堆借页元数据（永久转作堆空间）。
pub struct HeapMeta;
impl FrameMeta for HeapMeta {
    const OWNER: FrameOwner = FrameOwner::Heap;
}

/// DMA 缓冲区元数据。
pub struct DmaMeta;
impl FrameMeta for DmaMeta {
    const OWNER: FrameOwner = FrameOwner::Dma;
}

/// 共享内存元数据（sysv_shm / MAP_SHARED 匿名）。
pub struct SharedMemMeta;
impl FrameMeta for SharedMemMeta {
    const OWNER: FrameOwner = FrameOwner::SharedMem;
}

/// Hypervisor 元数据。
pub struct HypervisorMeta;
impl FrameMeta for HypervisorMeta {
    const OWNER: FrameOwner = FrameOwner::Hypervisor;
}

// ── UniqueFrame<M>：独占帧句柄 ─────────────────────────────────────────

/// 独占所有权的物理帧句柄。
///
/// - 不可 clone（编译器强制）
/// - Drop 时自动释放（MetaSlot UNIQUE → UNUSED → buddy dealloc）
/// - 可通过 `into_shared()` 转为共享 `Frame<M>`
///
/// `M` 类型参数在编译期保证帧用途不混淆。
pub struct UniqueFrame<M: FrameMeta> {
    ppn: PhysPageNum,
    _meta: PhantomData<M>,
}

impl<M: FrameMeta> UniqueFrame<M> {
    /// 从一个已由 buddy 分配但尚未初始化 MetaSlot 的 PPN 创建独占帧。
    ///
    /// **仅供 `frame.rs` 的分配函数调用**。调用前 MetaSlot 必须处于 UNUSED 状态。
    ///
    /// # Safety contract (调用方保证)
    /// - `ppn` 刚从 `BuddyAllocator::alloc()` 取得
    /// - 此 PPN 的 MetaSlot 当前 `ref_count == UNUSED`
    pub(super) fn from_raw_ppn(ppn: PhysPageNum) -> Self {
        let slot = page_meta::meta_of(ppn);
        slot.init_as_unique(M::OWNER);
        Self {
            ppn,
            _meta: PhantomData,
        }
    }

    /// 获取物理页号。
    #[inline]
    pub fn ppn(&self) -> PhysPageNum {
        self.ppn
    }

    /// 获取帧的 MetaSlot 引用（用于读取/修改 flags 等）。
    #[inline]
    pub fn meta(&self) -> &'static MetaSlot {
        page_meta::meta_of(self.ppn)
    }

    /// 独占帧 → 共享帧（单向降级）。
    ///
    /// 消耗 self，返回 ref_count = 1 的 `Frame<M>`。此后可 clone。
    pub fn into_shared(self) -> Frame<M> {
        let slot = page_meta::meta_of(self.ppn);
        slot.unique_to_shared();
        let ppn = self.ppn;
        // 阻止 UniqueFrame::drop 触发 release_unique
        core::mem::forget(self);
        Frame {
            ppn,
            _meta: PhantomData,
        }
    }

    /// 放弃独占所有权，返回裸 PPN（**不** 释放帧、**不** 清 MetaSlot）。
    ///
    /// 调用方承诺稍后手动管理此帧的生命周期。
    /// 仅用于与旧代码的互操作层（如 sysv_shm 的 `mem::forget` 路径）。
    pub fn into_raw(self) -> PhysPageNum {
        let ppn = self.ppn;
        core::mem::forget(self);
        ppn
    }
}

impl<M: FrameMeta> Drop for UniqueFrame<M> {
    fn drop(&mut self) {
        M::on_drop(self.ppn);
        let slot = page_meta::meta_of(self.ppn);
        if slot.release_unique() {
            // 归还 buddy
            super::frame::return_frame_to_buddy(self.ppn, 0);
        }
    }
}

// UniqueFrame 不可 Clone（编译器自动保证，这里显式标注意图）
// impl<M: FrameMeta> !Clone for UniqueFrame<M> {}

// ── Frame<M>：共享帧句柄 ───────────────────────────────────────────────

/// 引用计数共享的物理帧句柄（类似 Arc<PhysFrame>，但零堆分配）。
///
/// - clone = MetaSlot ref_count++（原子操作，无锁）
/// - drop = MetaSlot ref_count--，归零时自动释放
/// - 不可 &mut 元数据（共享所有权，只能读）
pub struct Frame<M: FrameMeta> {
    ppn: PhysPageNum,
    _meta: PhantomData<M>,
}

impl<M: FrameMeta> Frame<M> {
    /// 从一个已由 buddy 分配但尚未初始化 MetaSlot 的 PPN 创建共享帧（ref_count = 1）。
    ///
    /// **仅供 `frame.rs` 的分配函数调用**。
    pub(super) fn from_raw_ppn(ppn: PhysPageNum) -> Self {
        let slot = page_meta::meta_of(ppn);
        slot.init_as_shared(M::OWNER);
        Self {
            ppn,
            _meta: PhantomData,
        }
    }

    /// 从已有的、ref_count 已经 ≥ 1 的 PPN 创建额外引用（ref_count++）。
    ///
    /// 用于从旧代码的裸 PPN 恢复 RAII 句柄（如 CoW fork 路径中父帧已有 ref=1，
    /// 子进程需要额外 +1）。
    ///
    /// # Safety contract (调用方保证)
    /// - `ppn` 的 MetaSlot ref_count 当前 ≥ 1
    /// - 调用方负责确保该帧确实是 `M` 类型
    pub(super) fn from_ppn_inc_ref(ppn: PhysPageNum) -> Self {
        let slot = page_meta::meta_of(ppn);
        slot.inc_ref();
        Self {
            ppn,
            _meta: PhantomData,
        }
    }

    /// 获取物理页号。
    #[inline]
    pub fn ppn(&self) -> PhysPageNum {
        self.ppn
    }

    /// 获取帧的 MetaSlot 引用。
    #[inline]
    pub fn meta(&self) -> &'static MetaSlot {
        page_meta::meta_of(self.ppn)
    }

    /// 放弃所有权，返回裸 PPN（**不** 释放帧、**不** 减 ref_count）。
    ///
    /// 调用方承诺稍后手动管理此帧的生命周期或重新包装为 Frame。
    pub fn into_raw(self) -> PhysPageNum {
        let ppn = self.ppn;
        core::mem::forget(self);
        ppn
    }

    /// 尝试将共享帧转为独占帧（仅当 ref_count == 1 时成功）。
    ///
    /// 成功：消耗 self，返回 `Ok(UniqueFrame<M>)`。
    /// 失败：返回 `Err(self)`（仍有其他引用）。
    pub fn try_into_unique(self) -> Result<UniqueFrame<M>, Self> {
        let slot = page_meta::meta_of(self.ppn);
        // 尝试将 ref_count 从 1 → UNIQUE
        let prev = slot.ref_count.compare_exchange(
            1,
            page_meta::REF_COUNT_UNIQUE,
            core::sync::atomic::Ordering::AcqRel,
            core::sync::atomic::Ordering::Relaxed,
        );
        match prev {
            Ok(_) => {
                let ppn = self.ppn;
                core::mem::forget(self);
                Ok(UniqueFrame {
                    ppn,
                    _meta: PhantomData,
                })
            }
            Err(_) => Err(self),
        }
    }
}

impl<M: FrameMeta> Clone for Frame<M> {
    fn clone(&self) -> Self {
        let slot = page_meta::meta_of(self.ppn);
        slot.inc_ref();
        Self {
            ppn: self.ppn,
            _meta: PhantomData,
        }
    }
}

impl<M: FrameMeta> Drop for Frame<M> {
    fn drop(&mut self) {
        let slot = page_meta::meta_of(self.ppn);
        if slot.dec_ref() {
            // 最后一个引用，执行清理并归还 buddy
            M::on_drop(self.ppn);
            slot.finalize_release();
            super::frame::return_frame_to_buddy(self.ppn, 0);
        }
    }
}

// ── Segment<M>：连续多帧句柄 ────────────────────────────────────────────

/// 连续物理帧块的 RAII 句柄（对标 Asterinas `Segment<M>`）。
///
/// 用于 `alloc_frames(order)` 分配的连续物理块：
/// - 内核栈：`Segment<KernelStackMeta>`（order-4 = 16 页）
/// - DMA 缓冲区：`Segment<DmaMeta>`（order 取决于设备需求）
///
/// Drop 时自动将整个连续块归还 buddy。
pub struct Segment<M: FrameMeta> {
    /// 连续块起始 PPN
    base_ppn: PhysPageNum,
    /// buddy 分配 order（块大小 = 2^order 页）
    order: usize,
    _meta: PhantomData<M>,
}

impl<M: FrameMeta> Segment<M> {
    /// 从 buddy 分配结果创建 Segment。
    ///
    /// **仅供 `frame.rs` 的 `alloc_frames` 调用**。
    /// 对 [base_ppn, base_ppn + 2^order) 的每个帧初始化 MetaSlot。
    pub(super) fn from_raw(base_ppn: PhysPageNum, order: usize) -> Self {
        let count = 1usize << order;
        page_meta::batch_init_unique(base_ppn, count, M::OWNER, order as u8);
        Self {
            base_ppn,
            order,
            _meta: PhantomData,
        }
    }

    /// 获取起始 PPN。
    #[inline]
    pub fn base_ppn(&self) -> PhysPageNum {
        self.base_ppn
    }

    /// 获取块的 order。
    #[inline]
    pub fn order(&self) -> usize {
        self.order
    }

    /// 获取块包含的页数（2^order）。
    #[inline]
    pub fn page_count(&self) -> usize {
        1usize << self.order
    }

    /// 获取起始内核虚拟地址（用于内核栈 bottom 计算等）。
    #[inline]
    pub fn kernel_vaddr(&self) -> usize {
        self.base_ppn.kernel_addr()
    }

    /// 获取块的字节大小。
    #[inline]
    pub fn byte_size(&self) -> usize {
        self.page_count() * super::PAGE_SIZE
    }

    /// 放弃所有权，返回 (base_ppn, order)。不释放帧、不清 MetaSlot。
    pub fn into_raw(self) -> (PhysPageNum, usize) {
        let result = (self.base_ppn, self.order);
        core::mem::forget(self);
        result
    }
}

impl<M: FrameMeta> Drop for Segment<M> {
    fn drop(&mut self) {
        let count = 1usize << self.order;
        // 调用元数据类型的清理回调（对每个帧）
        for i in 0..count {
            M::on_drop(PhysPageNum(self.base_ppn.0 + i));
        }
        // 批量释放 MetaSlot：UNIQUE → UNUSED
        page_meta::batch_release_unique(self.base_ppn, count);
        // 归还 buddy
        super::frame::return_frames_to_buddy(self.base_ppn, self.order);
    }
}

// ── Debug 实现 ──────────────────────────────────────────────────────────

impl<M: FrameMeta> core::fmt::Debug for UniqueFrame<M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UniqueFrame")
            .field("ppn", &format_args!("{:#x}", self.ppn.0))
            .field("owner", &page_meta::meta_of(self.ppn).get_owner())
            .finish()
    }
}

impl<M: FrameMeta> core::fmt::Debug for Frame<M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let slot = page_meta::meta_of(self.ppn);
        f.debug_struct("Frame")
            .field("ppn", &format_args!("{:#x}", self.ppn.0))
            .field("ref_count", &slot.ref_count())
            .field("owner", &slot.get_owner())
            .finish()
    }
}

impl<M: FrameMeta> core::fmt::Debug for Segment<M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Segment")
            .field("base_ppn", &format_args!("{:#x}", self.base_ppn.0))
            .field("order", &self.order)
            .field("pages", &self.page_count())
            .field("owner", &page_meta::meta_of(self.base_ppn).get_owner())
            .finish()
    }
}
