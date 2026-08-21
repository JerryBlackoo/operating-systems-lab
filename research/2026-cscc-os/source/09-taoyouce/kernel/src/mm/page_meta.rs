//! Per-frame metadata subsystem (对标 Asterinas/OSTD MetaSlot + Linux struct page)
//!
//! 每个物理帧在全局 `META_ARRAY` 中拥有一个 `MetaSlot`，通过 PPN 直接算偏移
//! O(1) 访问。MetaSlot 承载：
//!
//! - `ref_count`：原子引用计数，替代旧 `PageRefCounter`（非原子、全局锁）
//!   - `REF_COUNT_UNUSED`  = 0：帧空闲，在 buddy free list 中
//!   - `REF_COUNT_UNIQUE`  = u32::MAX：独占帧（UniqueFrame），不可 clone
//!   - `1..u32::MAX-1`    ：共享引用数（Frame<M> clone 时 +1，drop 时 -1）
//!
//! - `owner`：帧用途标记（编译期泛型 + 运行时枚举双重保证）
//!
//! - `flags`：帧状态位（dirty / uptodate / locked 等）
//!
//! # 关键不变量
//!
//! - **Inv.1**：`alloc_frame` 只能从 `ref_count == UNUSED` 的帧中取
//! - **Inv.2**：帧释放走 RAII Drop → ref_count → 0 → 自动还给 buddy
//! - **Inv.3**：`owner` 在 alloc 时设置、在最终 free 时清零
//! - **Inv.4**：帧用途切换必须经过 `set_owner`，前提是 ref_count == UNIQUE

use core::sync::atomic::{AtomicU32, AtomicU16, AtomicU8, Ordering};

use super::{PhysPageNum, PAGE_SIZE};

// ── 物理内存边界常量（与 frame.rs 相同） ──────────────────────────────────

#[cfg(target_arch = "riscv64")]
const PHYS_MEM_START: usize = 0x8020_0000;
#[cfg(target_arch = "loongarch64")]
const PHYS_MEM_START: usize = 0x0020_0000;

const PHYS_MEM_END: usize = crate::arch::consts::MEMORY_END;

/// 物理帧总数上界（覆盖从 PHYS_MEM_START 到 PHYS_MEM_END 的全部帧）。
/// LoongArch 有不连续的低/高 RAM bank，中间的空洞帧浪费少量数组空间但保证
/// PPN → 索引映射的 O(1) 简单性，与旧 `PageRefCounter` 完全相同的权衡。
const MAX_FRAMES: usize = (PHYS_MEM_END - PHYS_MEM_START) / PAGE_SIZE;

// ── 引用计数特殊值 ──────────────────────────────────────────────────────

/// 帧空闲（在 buddy free list 中）。alloc 前必须处于此状态。
pub const REF_COUNT_UNUSED: u32 = 0;

/// 独占帧（UniqueFrame 持有）。不可 clone，可 &mut 访问元数据。
/// 使用 u32::MAX 作为哨兵值，与正常引用计数区间 [1, u32::MAX-1) 不重叠。
pub const REF_COUNT_UNIQUE: u32 = u32::MAX;

// ── 帧用途标记 ──────────────────────────────────────────────────────────

/// 帧用途枚举（运行时标记）。
///
/// 与编译期 `Frame<M>` 泛型参数互补：泛型在编译期禁止不同类型帧混用，
/// `FrameOwner` 在运行时提供可查询的用途信息（用于 debug 断言、/proc 统计等）。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameOwner {
    /// 空闲（在 buddy free list 中或未初始化）
    Free = 0,
    /// 用户匿名页（FrameTracker / CoW 页）
    User = 1,
    /// 内核栈（alloc_frames order-N 连续分配）
    KernelStack = 2,
    /// Slab 分配器页
    Slab = 3,
    /// 页表结构页
    PageTable = 4,
    /// 页缓存（文件 backed page）
    PageCache = 5,
    /// 内核堆借页（永久转作堆空间，不归还 buddy）
    Heap = 6,
    /// DMA 缓冲区（VirtIO 等）
    Dma = 7,
    /// 共享内存（sysv_shm / MAP_SHARED 匿名）
    SharedMem = 8,
    /// Hypervisor 用途
    Hypervisor = 9,
}

impl FrameOwner {
    /// 从 u8 原子值恢复枚举。超出范围视为 Free。
    #[inline]
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::User,
            2 => Self::KernelStack,
            3 => Self::Slab,
            4 => Self::PageTable,
            5 => Self::PageCache,
            6 => Self::Heap,
            7 => Self::Dma,
            8 => Self::SharedMem,
            9 => Self::Hypervisor,
            _ => Self::Free,
        }
    }
}

// ── 帧状态位 ────────────────────────────────────────────────────────────

/// 帧状态标志位（可组合）。
///
/// 对标 Linux page flags 中与 RustMicroOS 当前功能相关的子集。
/// 位域编码，一个 `AtomicU8` 可同时容纳多个标志。
pub mod frame_flags {
    /// 帧数据已被修改（需要回写）
    pub const PG_DIRTY: u8 = 1 << 0;
    /// 帧数据是最新的（从磁盘读入后）
    pub const PG_UPTODATE: u8 = 1 << 1;
    /// 帧被锁定不可回收（mlock / pin）
    pub const PG_LOCKED: u8 = 1 << 2;
    /// 帧正在进行 I/O（用于异步读写）
    pub const PG_WRITEBACK: u8 = 1 << 3;
    /// 帧已被保留（不参与正常分配）
    pub const PG_RESERVED: u8 = 1 << 4;
}

// ── Allocator cookie ─────────────────────────────────────────────────────

/// 帧当前属于哪个内核分配器路径。
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AllocCookie {
    /// 静态堆 / 未分类 / buddy 空闲
    Unknown = 0,
    /// `kmalloc_large`：由 GlobalAlloc 直通 buddy 的大对象
    Buddy = 1,
    /// SLUB slab 页
    Slab = 2,
}

impl AllocCookie {
    #[inline]
    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Buddy,
            2 => Self::Slab,
            _ => Self::Unknown,
        }
    }
}

// ── MetaSlot 结构体 ─────────────────────────────────────────────────────

/// 单个物理帧的元数据槽。
///
/// 所有字段都是原子类型，无需持全局锁即可安全读写。每个 MetaSlot 占 8 字节。
/// 上限提到 8 GiB（决赛 BuildStorm `-m 8G`）后：RISC-V (MAX_FRAMES ≈ 2.1M)
/// 占 ~17MB BSS、LoongArch (≈ 2.6M) 占 ~21MB BSS——静态数组按编译期上限分配，
/// `-m 1G` 时多付的 ~15MB 是「同一二进制既服务初赛 1G 也服务决赛 8G」的代价。
#[repr(C)]
pub struct MetaSlot {
    /// 引用计数。语义：
    /// - `REF_COUNT_UNUSED` (0)：帧空闲
    /// - `REF_COUNT_UNIQUE` (u32::MAX)：独占持有
    /// - `1..u32::MAX-1`：共享引用数
    ///
    /// `pub(super)` 允许 `typed_frame.rs` 的 `try_into_unique` 执行 CAS。
    pub(super) ref_count: AtomicU32,

    /// 帧用途标记（FrameOwner 枚举的 u8 表示）
    owner: AtomicU8,

    /// 帧状态标志位（PG_DIRTY | PG_UPTODATE | ...）
    flags: AtomicU8,

    /// 所属 compound 块的 order（内核栈 order-4 = 16 页等）。
    /// 仅在 compound head 帧有意义，tail 帧的 order 为 0。
    order: AtomicU8,

    /// Allocator cookie（Linux `page->slab_cache` 的轻量替代）：
    /// 0=Unknown，1=Buddy 大对象直通，2=SLUB slab 页。GlobalAlloc 的
    /// dealloc 用它做 O(1) 回收路由，不再依赖地址区间/魔数猜路径。
    allocator: AtomicU8,

    /// [DEBUG 插桩] 当前指向本帧的「用户叶子 PTE」数量。
    /// 在 page_table map/unmap/remap 维护，alloc 时校验必须为 0
    /// （帧从 buddy 弹出却仍被用户 PTE 映射 = freed-while-mapped 铁证）。
    map_count: AtomicU16,
}

// SAFETY: MetaSlot 所有字段都是原子类型或无内部可变性的 padding，
// 可安全地跨线程/中断共享。
unsafe impl Sync for MetaSlot {}
unsafe impl Send for MetaSlot {}

impl MetaSlot {
    /// 常量构造器（用于静态全局数组初始化）。
    const fn new() -> Self {
        Self {
            ref_count: AtomicU32::new(REF_COUNT_UNUSED),
            owner: AtomicU8::new(FrameOwner::Free as u8),
            flags: AtomicU8::new(0),
            order: AtomicU8::new(0),
            allocator: AtomicU8::new(AllocCookie::Unknown as u8),
            map_count: AtomicU16::new(0),
        }
    }

    /// [DEBUG] 用户叶子 PTE 映射本帧时 +1。
    #[inline]
    pub fn inc_map(&self) {
        self.map_count.fetch_add(1, Ordering::AcqRel);
    }

    /// [DEBUG] 用户叶子 PTE 取消映射本帧时 -1（饱和，不下溢）。
    #[inline]
    pub fn dec_map(&self) {
        let _ = self.map_count.fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| {
            Some(v.saturating_sub(1))
        });
    }

    /// [DEBUG] 当前用户叶子 PTE 映射计数。
    #[inline]
    pub fn map_count(&self) -> u16 {
        self.map_count.load(Ordering::Acquire)
    }

    // ── 引用计数操作 ────────────────────────────────────────────────

    /// 获取当前引用计数（Relaxed 读，用于调试/统计）。
    #[inline]
    pub fn ref_count(&self) -> u32 {
        self.ref_count.load(Ordering::Relaxed)
    }

    /// 从 UNUSED 状态初始化为 UNIQUE（独占）。
    ///
    /// **Inv.1 强制**：仅当 ref_count == UNUSED 时成功。
    /// 若帧不在 UNUSED 状态，说明 buddy 把一个仍被占用的帧交了出来——
    /// 这是 double-ownership 的铁证，立即 panic。
    ///
    /// 返回后调用方获得独占所有权（UniqueFrame 语义）。
    #[inline]
    pub fn init_as_unique(&self, new_owner: FrameOwner) {
        let prev = self.ref_count.compare_exchange(
            REF_COUNT_UNUSED,
            REF_COUNT_UNIQUE,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
        match prev {
            Ok(_) => {
                self.owner.store(new_owner as u8, Ordering::Release);
            }
            Err(actual) => {
                panic!(
                    "[META] init_as_unique: frame ref_count={} (expected UNUSED=0), owner={:?} — \
                     buddy handed out an occupied frame! This is a double-ownership bug.",
                    actual,
                    self.get_owner(),
                );
            }
        }
    }

    /// 从 UNUSED 状态初始化为共享引用计数 1。
    ///
    /// 与 `init_as_unique` 相同的 Inv.1 强制，但初始为共享计数 1（Frame 语义）。
    #[inline]
    pub fn init_as_shared(&self, new_owner: FrameOwner) {
        let prev = self.ref_count.compare_exchange(
            REF_COUNT_UNUSED,
            1,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
        match prev {
            Ok(_) => {
                self.owner.store(new_owner as u8, Ordering::Release);
            }
            Err(actual) => {
                panic!(
                    "[META] init_as_shared: frame ref_count={} (expected UNUSED=0), owner={:?} — \
                     buddy handed out an occupied frame!",
                    actual,
                    self.get_owner(),
                );
            }
        }
    }

    /// 共享引用计数 +1（clone Frame<M> 时调用）。
    ///
    /// 前提：当前 ref_count 在 [1, u32::MAX-2] 范围（已是共享状态）。
    /// 若 ref_count == UNUSED 或 UNIQUE，说明使用方式不对，panic。
    #[inline]
    pub fn inc_ref(&self) {
        let prev = self.ref_count.fetch_add(1, Ordering::AcqRel);
        debug_assert!(
            prev >= 1 && prev < REF_COUNT_UNIQUE - 1,
            "[META] inc_ref: unexpected ref_count={} (must be in shared range [1, MAX-2])",
            prev,
        );
    }

    /// 共享引用计数 -1（drop Frame<M> 时调用）。
    ///
    /// 返回 `true` 表示这是最后一个引用（ref_count 从 1 降到 0），调用方应
    /// 执行最终释放（清元数据、还给 buddy）。
    ///
    /// 返回 `false` 表示仍有其他引用。
    #[inline]
    pub fn dec_ref(&self) -> bool {
        let prev = self.ref_count.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(
            prev >= 1 && prev != REF_COUNT_UNIQUE,
            "[META] dec_ref: unexpected ref_count={} (must be >= 1 and not UNIQUE)",
            prev,
        );
        prev == 1
    }

    /// 独占帧降级为共享引用计数 1（UniqueFrame → Frame 转换时调用）。
    ///
    /// 前提：当前 ref_count == UNIQUE。
    #[inline]
    pub fn unique_to_shared(&self) {
        let prev = self.ref_count.compare_exchange(
            REF_COUNT_UNIQUE,
            1,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
        debug_assert!(
            prev.is_ok(),
            "[META] unique_to_shared: ref_count={} (expected UNIQUE)",
            prev.unwrap_err(),
        );
    }

    /// 独占帧释放（UniqueFrame drop 时调用）：从 UNIQUE → UNUSED。
    ///
    /// 返回 `true` 表示成功清零，调用方应还给 buddy。
    #[inline]
    pub fn release_unique(&self) -> bool {
        let prev = self.ref_count.compare_exchange(
            REF_COUNT_UNIQUE,
            REF_COUNT_UNUSED,
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
        if prev.is_ok() {
            self.owner.store(FrameOwner::Free as u8, Ordering::Release);
            self.flags.store(0, Ordering::Release);
            self.order.store(0, Ordering::Relaxed);
            self.allocator.store(AllocCookie::Unknown as u8, Ordering::Relaxed);
            true
        } else {
            debug_assert!(false, "[META] release_unique: ref_count={} (expected UNIQUE)", prev.unwrap_err());
            false
        }
    }

    /// 最后一个共享引用释放后清理元数据（dec_ref 返回 true 后调用）。
    ///
    /// 前提：ref_count 已经在 dec_ref 中被原子地减到 0 = UNUSED。
    /// 此方法只清理 owner/flags/order，不修改 ref_count。
    #[inline]
    pub fn finalize_release(&self) {
        self.owner.store(FrameOwner::Free as u8, Ordering::Release);
        self.flags.store(0, Ordering::Release);
        self.order.store(0, Ordering::Relaxed);
        self.allocator.store(AllocCookie::Unknown as u8, Ordering::Relaxed);
    }

    /// 强制释放：无论当前状态，将 ref_count 置零并清空所有元数据。
    ///
    /// **仅供 `dealloc_frame_force` 等紧急路径使用。**
    #[inline]
    pub fn force_release(&self) {
        self.ref_count.store(REF_COUNT_UNUSED, Ordering::Release);
        self.owner.store(FrameOwner::Free as u8, Ordering::Release);
        self.flags.store(0, Ordering::Release);
        self.order.store(0, Ordering::Relaxed);
        self.allocator.store(AllocCookie::Unknown as u8, Ordering::Relaxed);
    }

    // ── Owner 操作 ──────────────────────────────────────────────────

    /// 获取帧用途。
    #[inline]
    pub fn get_owner(&self) -> FrameOwner {
        FrameOwner::from_u8(self.owner.load(Ordering::Acquire))
    }

    /// 设置帧用途（仅在独占或首次初始化时调用）。
    #[inline]
    pub fn set_owner(&self, owner: FrameOwner) {
        self.owner.store(owner as u8, Ordering::Release);
    }

    /// 设置 allocator cookie（仅在 owner 初始化/切换时调用）。
    #[inline]
    pub fn set_allocator(&self, cookie: AllocCookie) {
        self.allocator.store(cookie as u8, Ordering::Release);
    }

    /// 读取 allocator cookie。
    #[inline]
    pub fn allocator(&self) -> AllocCookie {
        AllocCookie::from_u8(self.allocator.load(Ordering::Acquire))
    }

    /// 检查帧是否空闲。
    #[inline]
    pub fn is_free(&self) -> bool {
        self.ref_count.load(Ordering::Acquire) == REF_COUNT_UNUSED
    }

    /// 检查帧是否处于独占状态。
    #[inline]
    pub fn is_unique(&self) -> bool {
        self.ref_count.load(Ordering::Acquire) == REF_COUNT_UNIQUE
    }

    // ── Flags 操作 ──────────────────────────────────────────────────

    /// 获取帧标志位。
    #[inline]
    pub fn get_flags(&self) -> u8 {
        self.flags.load(Ordering::Acquire)
    }

    /// 设置指定标志位（OR）。
    #[inline]
    pub fn set_flag(&self, flag: u8) {
        self.flags.fetch_or(flag, Ordering::AcqRel);
    }

    /// 清除指定标志位（AND NOT）。
    #[inline]
    pub fn clear_flag(&self, flag: u8) {
        self.flags.fetch_and(!flag, Ordering::AcqRel);
    }

    /// 检查是否包含指定标志位。
    #[inline]
    pub fn has_flag(&self, flag: u8) -> bool {
        self.flags.load(Ordering::Acquire) & flag != 0
    }

    // ── Order 操作 ──────────────────────────────────────────────────

    /// 获取 compound 块的 order。
    #[inline]
    pub fn get_order(&self) -> u8 {
        self.order.load(Ordering::Relaxed)
    }

    /// 设置 compound 块的 order。
    #[inline]
    pub fn set_order(&self, order: u8) {
        self.order.store(order, Ordering::Relaxed);
    }
}

// ── 全局 MetaSlot 数组 ──────────────────────────────────────────────────

/// 每个元素用 `const { MetaSlot::new() }` 初始化（Rust 2021 inline const）。
/// 数组大小与旧 `PageRefCounter::ref_counts` 完全相同（MAX_FRAMES），
/// 按 `(ppn << 12 - PHYS_MEM_START) / PAGE_SIZE` 索引。
static META_ARRAY: [MetaSlot; MAX_FRAMES] = {
    // const context: repeat MetaSlot::new() for MAX_FRAMES elements
    // 使用 unsafe transmute 从全零初始化，因为 MetaSlot 的全零状态恰好对应
    // ref_count=UNUSED(0), owner=Free(0), flags=0, order=0, _pad=0
    // 这是安全的：AtomicU32(0) / AtomicU8(0) 的内存表示与全零字节相同。
    //
    // 如果 Rust 未来支持 const fn 中的 array repeat，可改为
    // `[MetaSlot::new(); MAX_FRAMES]`。
    unsafe { core::mem::MaybeUninit::<[MetaSlot; MAX_FRAMES]>::zeroed().assume_init() }
};

// ── 帧诊断追踪（冷路径全扫描 — 不影响热路径时序）──────────────────────

/// 帧释放前扫描所有活跃进程的页表，检查是否还有 PTE 指向该 PPN。
/// 仅在帧即将归还 buddy 时调用（冷路径），不改变 map/unmap 热路径。
pub fn assert_unmapped(ppn: PhysPageNum, ctx: &str) {
    use crate::sched::process::PROCESS_TABLE;
    if !crate::mm::frame::uaf_dbg_enabled() {
        return;
    }
    // 尝试获取锁；如果锁已被持有（可能从 mark_zombie → mmput 路径
    // 进入，此时 PROCESS_TABLE 锁已持有），跳过扫描避免死锁
    let guard = match PROCESS_TABLE.try_lock() {
        Some(g) => g,
        None => return,
    };
    let table = match guard.as_ref() {
        Some(t) => t,
        None => return,
    };
    let target = ppn.0;
    for proc in table.iter() {
        let pid = proc.pid;
        let mm = match proc.mm.as_ref() {
            Some(mm) => mm,
            None => continue,
        };
        // try_lock：避免嵌套死锁（当前进程可能已锁住自己的 mm）
        let addr_space = match mm.try_lock() {
            Some(a) => a,
            None => continue,
        };
        let pt = addr_space.page_table();
        for vma in addr_space.vmas_iter() {
            for vpn_num in vma.vpn_range() {
                let vpn = super::VirtPageNum(vpn_num);
                if let Some(mapped_ppn) = pt.translate(vpn) {
                    if mapped_ppn.0 == target {
                        crate::println!(
                            "\n[PREMATURE-FREE] {} ppn={:#x} still mapped in pid={} vpn={:#x} owner={:?} rc={}",
                            ctx, target, pid, vpn_num, meta_of(ppn).get_owner(), meta_of(ppn).ref_count(),
                        );
                        panic!("[PREMATURE-FREE] frame {:#x} freed while still mapped in pid={} vpn={:#x}", target, pid, vpn_num);
                    }
                }
            }
        }
    }
}

// ── [DEBUG] 全局释放环形缓冲：取证「损坏的用户页此前是哪种内核页」───────
const FREE_RING_SIZE: usize = 2048;
static FREE_RING_IDX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
/// 每槽：(ppn, prev_owner_code, seq) — 仅调试用，竞争下轻微错乱可接受。
static mut FREE_RING: [(u32, u8, u32); FREE_RING_SIZE] = [(0, 0, 0); FREE_RING_SIZE];

/// 记录一次帧释放（在 dealloc 真正归还 buddy 前调用）。
pub fn record_free(ppn: PhysPageNum, prev_owner: FrameOwner) {
    let seq = FREE_RING_IDX.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let i = seq % FREE_RING_SIZE;
    unsafe {
        let p = core::ptr::addr_of_mut!(FREE_RING[i]);
        core::ptr::write_volatile(p, (ppn.0 as u32, prev_owner as u8, seq as u32));
    }
}

/// 在损坏检测点调用：扫描释放环，报告该 ppn 最近一次释放时的前任 owner。
pub fn dump_free_ring(ppn: PhysPageNum) {
    let target = ppn.0 as u32;
    let mut found = false;
    for i in 0..FREE_RING_SIZE {
        let (rppn, owner_code, seq) = unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(FREE_RING[i]))
        };
        if rppn == target && seq != 0 {
            crate::println!(
                "[FREE-RING] ppn={:#x} was freed prev_owner_code={} (1=User 2=KStack 3=Slab 4=PageTable 6=Heap) seq={}",
                ppn.0, owner_code, seq
            );
            found = true;
        }
    }
    if !found {
        crate::println!("[FREE-RING] ppn={:#x} NOT found in recent free history", ppn.0);
    }
}

/// 输出帧追踪信息（供 trap handler 调用）
pub fn dump_trace(ppn: PhysPageNum) {
    let slot = meta_of(ppn);
    crate::println!(
        "[TRACE] ppn={:#x} owner={:?} rc={}",
        ppn.0, slot.get_owner(), slot.ref_count(),
    );
}

// ── PPN ↔ MetaSlot 映射 ────────────────────────────────────────────────

/// PPN → MetaSlot 数组索引。
///
/// 返回 `None` 表示 PPN 超出物理内存范围（设备 MMIO 地址等）。
#[inline]
fn ppn_to_index(ppn: PhysPageNum) -> Option<usize> {
    let pa = ppn.0 << 12;
    if pa >= PHYS_MEM_START && pa < PHYS_MEM_END {
        Some((pa - PHYS_MEM_START) / PAGE_SIZE)
    } else {
        None
    }
}

/// 获取物理帧的 MetaSlot 引用。
///
/// # Panics
///
/// PPN 超出物理内存范围时 panic（内核 bug：不应对 MMIO 地址查询帧元数据）。
#[inline]
pub fn meta_of(ppn: PhysPageNum) -> &'static MetaSlot {
    let idx = ppn_to_index(ppn).unwrap_or_else(|| {
        panic!(
            "[META] meta_of: ppn={:#x} (pa={:#x}) out of physical memory range [{:#x}, {:#x})",
            ppn.0,
            ppn.0 << 12,
            PHYS_MEM_START,
            PHYS_MEM_END,
        );
    });
    &META_ARRAY[idx]
}

/// 安全版本：PPN 超出范围返回 None 而非 panic。
#[inline]
pub fn try_meta_of(ppn: PhysPageNum) -> Option<&'static MetaSlot> {
    ppn_to_index(ppn).map(|idx| &META_ARRAY[idx])
}

// ── 批量操作（用于 alloc_frames / dealloc_frames 等多页场景） ───────────

/// 批量初始化连续帧为 UNIQUE 状态。
///
/// 对 `[ppn, ppn + count)` 范围内的每个帧调用 `init_as_unique`。
/// 任何一帧不在 UNUSED 状态都会 panic（Inv.1）。
/// head 帧设 order，tail 帧 order = 0。
pub fn batch_init_unique(ppn: PhysPageNum, count: usize, owner: FrameOwner, order: u8) {
    for i in 0..count {
        let slot = meta_of(PhysPageNum(ppn.0 + i));
        slot.init_as_unique(owner);
        if i == 0 {
            slot.set_order(order);
        }
    }
}

/// 批量初始化连续帧为共享引用计数 1。
pub fn batch_init_shared(ppn: PhysPageNum, count: usize, owner: FrameOwner, order: u8) {
    for i in 0..count {
        let slot = meta_of(PhysPageNum(ppn.0 + i));
        slot.init_as_shared(owner);
        if i == 0 {
            slot.set_order(order);
        }
    }
}

/// 批量释放独占帧：每个帧从 UNIQUE → UNUSED。
/// 返回实际成功释放的帧数（应等于 count，否则有 bug）。
pub fn batch_release_unique(ppn: PhysPageNum, count: usize) -> usize {
    let mut released = 0;
    for i in 0..count {
        let slot = meta_of(PhysPageNum(ppn.0 + i));
        if slot.release_unique() {
            released += 1;
        }
    }
    released
}

/// 批量标记帧为永久占用（内核堆借页：不归还 buddy，无 RAII tracker）。
/// 将 ref_count 从 UNUSED → UNIQUE，owner → Heap。
pub fn batch_init_heap(ppn: PhysPageNum, count: usize) {
    for i in 0..count {
        let slot = meta_of(PhysPageNum(ppn.0 + i));
        slot.init_as_unique(FrameOwner::Heap);
    }
}

// ── 统计接口（供 /proc/meminfo、get_memory_stats 等使用） ───────────────

/// 帧用途统计快照。
#[derive(Default)]
pub struct FrameMetaStats {
    pub total_scanned: usize,
    pub free: usize,
    pub user: usize,
    pub kernel_stack: usize,
    pub slab: usize,
    pub page_table: usize,
    pub page_cache: usize,
    pub heap: usize,
    pub dma: usize,
    pub shared_mem: usize,
    pub hypervisor: usize,
}

/// 全量扫描 META_ARRAY 统计各用途帧数（O(N)，仅用于 /proc/meminfo 等低频场景）。
pub fn collect_stats() -> FrameMetaStats {
    let mut stats = FrameMetaStats::default();
    for slot in META_ARRAY.iter() {
        stats.total_scanned += 1;
        match slot.get_owner() {
            FrameOwner::Free => stats.free += 1,
            FrameOwner::User => stats.user += 1,
            FrameOwner::KernelStack => stats.kernel_stack += 1,
            FrameOwner::Slab => stats.slab += 1,
            FrameOwner::PageTable => stats.page_table += 1,
            FrameOwner::PageCache => stats.page_cache += 1,
            FrameOwner::Heap => stats.heap += 1,
            FrameOwner::Dma => stats.dma += 1,
            FrameOwner::SharedMem => stats.shared_mem += 1,
            FrameOwner::Hypervisor => stats.hypervisor += 1,
        }
    }
    stats
}

/// 调试用：打印某帧的完整元数据状态（替代旧 `frame_ownership_dump_dbg`）。
pub fn dump_frame_meta(ppn: PhysPageNum) {
    if let Some(slot) = try_meta_of(ppn) {
        crate::println!(
            "[META] ppn={:#x} ref_count={} owner={:?} flags={:#04x} order={}",
            ppn.0,
            slot.ref_count(),
            slot.get_owner(),
            slot.get_flags(),
            slot.get_order(),
        );
    } else {
        crate::println!("[META] ppn={:#x} — outside tracked physical memory range", ppn.0);
    }
}
