//! 物理页帧分配器
//!
//! 使用 Buddy System 算法管理物理内存。
//!
//! # 架构重构（Asterinas/OSTD 模型）
//!
//! 旧设计中 `PageRefCounter`（非原子 u16 数组 + 全局 SpinLock）、散落的诊断数组
//! （FT_LIVE / HEAP_OWNED / SLAB_OWNED / PROV / TargetLog）已全部删除，统一替换为
//! `page_meta::MetaSlot` —— 每帧 8 字节原子元数据，O(1) 按 PPN 索引，无锁读写。
//!
//! 关键不变量：
//! - **Inv.1**：buddy 弹出的帧其 MetaSlot.ref_count 必须 == UNUSED(0)
//! - **Inv.2**：帧释放走 RAII Drop → ref_count → 0 → 自动还给 buddy
//! - **Inv.3**：`FrameTracker`（兼容包装）经 MetaSlot 管理引用计数
//!
//! CoW 引用计数由 `MetaSlot.ref_count` 原子管理，不再需要全局锁。

#![allow(dead_code)]

use super::{PhysAddr, PhysPageNum, PAGE_SIZE};
use super::page_meta::{self, FrameOwner};
use super::pcp::{PCP_BATCH, PCP_CAP, PERCPU_PAGES};
use crate::percpu::{current_cpu_id, MAX_CPUS};
use crate::sync::TicketSpinLock;
use alloc::boxed::Box;
use alloc::vec::Vec;

/// 最大阶数（2^18 页 = 1GB）
const MAX_ORDER: usize = 19;

/// 物理内存起始地址（内核结束后）
#[cfg(target_arch = "riscv64")]
const PHYS_MEM_START: usize = 0x8020_0000;
#[cfg(target_arch = "loongarch64")]
const PHYS_MEM_START: usize = 0x0020_0000;
/// 物理内存结束地址
const PHYS_MEM_END: usize = crate::arch::consts::MEMORY_END;

/// Buddy 分配器
pub struct BuddyAllocator {
    /// 空闲链表（每个 order 一个）
    free_lists: [Vec<PhysPageNum>; MAX_ORDER],
    /// 起始页号
    start: PhysPageNum,
    /// 结束页号
    end: PhysPageNum,
    /// 已分配页数
    allocated: usize,
    /// total usable pages across all regions (handles discontiguous banks)
    total_pages: usize,
    /// 每页「在 buddy 空闲链表中」状态位（对标 Linux `PageBuddy`）。
    /// 一页在某个空闲块覆盖范围内时为 true。`dealloc` 借此拒绝二次释放——
    /// 否则同一帧会被推进空闲链表两次，被发给两个 owner（内核栈 + 堆/DMA 缓冲）
    /// 形成物理帧双重占用。
    free_map: Option<Box<[bool]>>,
    /// 空闲块头页标记：0 = 非块头，否则 = order+1（对标 Linux `PageBuddy` +
    /// `page_private(order)`）。配合 `pos_map` 使 buddy 合并的查找/摘除 O(1)，
    /// 取代旧的 `free_lists[o].iter().position()` 全链表线性扫描。
    head_map: Option<Box<[u8]>>,
    /// 块头页在 `free_lists[order]` 中的下标（仅块头有效）。
    pos_map: Option<Box<[u32]>>,
}

// order-0 的 per-CPU pageset 已外置到 `super::pcp`（Linux
// `struct per_cpu_pages` 对标）。buddy 本身只保留全局 2^order 空闲链表。

/// 最大页数
const MAX_PAGES: usize = (PHYS_MEM_END - PHYS_MEM_START) / PAGE_SIZE;

impl BuddyAllocator {
    /// 创建新的分配器
    pub const fn new() -> Self {
        Self {
            free_lists: [
                Vec::new(), Vec::new(), Vec::new(), Vec::new(),
                Vec::new(), Vec::new(), Vec::new(), Vec::new(),
                Vec::new(), Vec::new(), Vec::new(), Vec::new(),
                Vec::new(), Vec::new(), Vec::new(), Vec::new(),
                Vec::new(), Vec::new(), Vec::new(),
            ],
            start: PhysPageNum(0),
            end: PhysPageNum(0),
            allocated: 0,
            total_pages: 0,
            free_map: None,
            head_map: None,
            pos_map: None,
        }
    }

    /// 按需分配/增长 `free_map`（boot 期 `init`/`add_region` 调用，单核、堆已就绪）。
    ///
    /// 位图按**实测 RAM 末尾**定容而非编译期上限 `MAX_PAGES`（8 GiB 规格）：
    /// `-m 1G` 时三张图共 ~1.5MB，`-m 8G` 时 ~12MB；LA 的 `add_region`
    /// 在 `init` 之后追加更大的高 bank，此处支持保内容增长。
    fn ensure_free_map(&mut self, upto_pa: usize) {
        let capped = upto_pa.min(PHYS_MEM_END).max(PHYS_MEM_START);
        let need = (capped - PHYS_MEM_START) / PAGE_SIZE;

        fn grow<T: Copy + Default>(slot: &mut Option<Box<[T]>>, need: usize) {
            let cur = slot.as_ref().map_or(0, |m| m.len());
            if cur >= need {
                return;
            }
            let mut new = alloc::vec![T::default(); need].into_boxed_slice();
            if let Some(old) = slot.take() {
                new[..old.len()].copy_from_slice(&old);
            }
            *slot = Some(new);
        }

        grow(&mut self.free_map, need);
        grow(&mut self.head_map, need);
        grow(&mut self.pos_map, need);
    }

    /// 把空闲块 push 进 `free_lists[order]` 并登记头页标记/下标。
    #[inline]
    fn push_free_block(&mut self, head: PhysPageNum, order: usize) {
        if let (Some(hm), Some(pm)) = (self.head_map.as_mut(), self.pos_map.as_mut()) {
            if let Some(idx) = Self::free_map_idx(head) {
                if let (Some(h), Some(p)) = (hm.get_mut(idx), pm.get_mut(idx)) {
                    *h = order as u8 + 1;
                    *p = self.free_lists[order].len() as u32;
                }
            }
        }
        self.free_lists[order].push(head);
    }

    /// 从 `free_lists[order]` 尾部弹出一个空闲块并清头页标记。
    #[inline]
    fn pop_free_block(&mut self, order: usize) -> Option<PhysPageNum> {
        let head = self.free_lists[order].pop()?;
        if let Some(hm) = self.head_map.as_mut() {
            if let Some(idx) = Self::free_map_idx(head) {
                if let Some(h) = hm.get_mut(idx) {
                    *h = 0;
                }
            }
        }
        Some(head)
    }

    /// 若 `head` 是 `order` 阶空闲块头，O(1) 从链表摘除并返回 true。
    #[inline]
    fn take_free_block(&mut self, head: PhysPageNum, order: usize) -> bool {
        let idx = match Self::free_map_idx(head) {
            Some(idx) => idx,
            None => return false,
        };
        let (hm, pm) = match (self.head_map.as_mut(), self.pos_map.as_mut()) {
            (Some(hm), Some(pm)) => (hm, pm),
            _ => return false,
        };
        if idx >= hm.len() || hm[idx] != order as u8 + 1 {
            return false;
        }
        let pos = pm[idx] as usize;
        let list = &mut self.free_lists[order];
        debug_assert!(pos < list.len() && list[pos] == head);
        list.swap_remove(pos);
        hm[idx] = 0;
        if pos < list.len() {
            let moved = list[pos];
            if let Some(midx) = Self::free_map_idx(moved) {
                if let Some(p) = pm.get_mut(midx) {
                    *p = pos as u32;
                }
            }
        }
        true
    }

    /// 物理页号 → free_map 索引（与 PHYS_MEM_START 对齐，区间外返回 None）。
    #[inline]
    fn free_map_idx(ppn: PhysPageNum) -> Option<usize> {
        let pa = ppn.0 << 12;
        if pa >= PHYS_MEM_START && pa < PHYS_MEM_END {
            Some((pa - PHYS_MEM_START) / PAGE_SIZE)
        } else {
            None
        }
    }

    /// 把 [ppn, ppn+count) 标记为 free / allocated。
    #[inline]
    fn mark_pages(&mut self, ppn: PhysPageNum, count: usize, free: bool) {
        let map = match self.free_map.as_mut() {
            Some(m) => m,
            None => return,
        };
        for i in 0..count {
            if let Some(idx) = Self::free_map_idx(PhysPageNum(ppn.0 + i)) {
                if let Some(slot) = map.get_mut(idx) {
                    *slot = free;
                }
            }
        }
    }

    /// 该页当前是否在某空闲块覆盖范围内。
    #[inline]
    fn is_page_free(&self, ppn: PhysPageNum) -> bool {
        let map = match self.free_map.as_ref() {
            Some(m) => m,
            None => return false,
        };
        Self::free_map_idx(ppn)
            .and_then(|idx| map.get(idx).copied())
            .unwrap_or(false)
    }

    /// 初始化分配器
    pub fn init(&mut self, start: usize, end: usize) {
        let start_ppn = PhysAddr(start).ceil();
        let end_ppn = PhysAddr(end).floor();

        self.start = start_ppn;
        self.end = end_ppn;
        self.total_pages = end_ppn.0 - start_ppn.0;

        self.reserve_free_list_capacity();
        self.ensure_free_map(end_ppn.0 << 12);

        let mut current = start_ppn.0;
        while current < end_ppn.0 {
            let mut order = 0;
            while order < MAX_ORDER - 1 {
                let size = 1 << (order + 1);
                if current % size != 0 || current + size > end_ppn.0 {
                    break;
                }
                order += 1;
            }

            self.push_free_block(PhysPageNum(current), order);
            self.mark_pages(PhysPageNum(current), 1 << order, true);
            current += 1 << order;
        }

        crate::println!(
            "[frame] Buddy allocator: {} - {} ({} pages)",
            start_ppn.0, end_ppn.0, end_ppn.0 - start_ppn.0
        );
    }

    /// 将每个 order 的空闲链表容量预留到「当前总页数下该 order 的理论最大块数」，
    /// 使 `alloc`/`dealloc` 中的 `Vec::push` 永不再触发堆再分配。
    fn reserve_free_list_capacity(&mut self) {
        for order in 0..MAX_ORDER {
            let target = (self.total_pages >> order) + 4;
            let len = self.free_lists[order].len();
            if target > len {
                self.free_lists[order].reserve(target - len);
            }
        }
    }

    /// Add an additional, possibly discontiguous, physical RAM region.
    pub fn add_region(&mut self, start: usize, end: usize) {
        let start_ppn = PhysAddr(start).ceil();
        let end_ppn = PhysAddr(end).floor();
        if start_ppn.0 >= end_ppn.0 {
            return;
        }

        self.total_pages += end_ppn.0 - start_ppn.0;
        self.reserve_free_list_capacity();
        self.ensure_free_map(end_ppn.0 << 12);

        let mut current = start_ppn.0;
        while current < end_ppn.0 {
            let mut order = 0;
            while order < MAX_ORDER - 1 {
                let size = 1 << (order + 1);
                if current % size != 0 || current + size > end_ppn.0 {
                    break;
                }
                order += 1;
            }

            self.push_free_block(PhysPageNum(current), order);
            self.mark_pages(PhysPageNum(current), 1 << order, true);
            current += 1 << order;
        }

        if end_ppn.0 > self.end.0 {
            self.end = end_ppn;
        }

        crate::println!(
            "[frame] Buddy add region: {} - {} ({} pages)",
            start_ppn.0, end_ppn.0, end_ppn.0 - start_ppn.0
        );
    }

    /// 分配 2^order 个连续页帧
    pub fn alloc(&mut self, order: usize) -> Option<PhysPageNum> {
        if order >= MAX_ORDER {
            return None;
        }

        for o in order..MAX_ORDER {
            if let Some(frame) = self.pop_free_block(o) {
                for split_order in (order..o).rev() {
                    let buddy = PhysPageNum(frame.0 + (1 << split_order));
                    self.push_free_block(buddy, split_order);
                }
                self.mark_pages(frame, 1 << order, false);

                self.allocated += 1 << order;
                return Some(frame);
            }
        }

        None
    }

    /// 释放页帧
    pub fn dealloc(&mut self, frame: PhysPageNum, order: usize) {
        if order >= MAX_ORDER {
            return;
        }

        // 对标 Linux `PageBuddy`：拒绝二次释放。
        if self.is_page_free(frame) {
            crate::println!(
                "[BUDDY-DOUBLEFREE] ppn={:#x} order={} already free — dropping to prevent double-ownership",
                frame.0, order
            );
            return;
        }
        self.mark_pages(frame, 1 << order, true);

        let mut current = frame;
        let mut current_order = order;

        // 尝试合并伙伴（head_map/pos_map 使查找+摘除 O(1)）
        while current_order < MAX_ORDER - 1 {
            let buddy_ppn = current.0 ^ (1 << current_order);
            let buddy = PhysPageNum(buddy_ppn);

            if self.take_free_block(buddy, current_order) {
                current = PhysPageNum(current.0.min(buddy_ppn));
                current_order += 1;
            } else {
                break;
            }
        }

        self.push_free_block(current, current_order);
        self.allocated -= 1 << order;
    }

    /// 把一页从 per-CPU pageset 正式交给调用方：free_map 置 false，
    /// `allocated` 加一。返回 false = 该页不在 free 状态（所有权损坏）。
    #[inline]
    fn activate_pcp_page(&mut self, frame: PhysPageNum) -> bool {
        if !self.is_page_free(frame) {
            crate::println!(
                "[BUDDY-PCP] activate ppn={:#x} but not free — dropping to prevent double-ownership",
                frame.0
            );
            return false;
        }
        self.mark_pages(frame, 1, false);
        self.allocated += 1;
        true
    }

    /// 把一页从调用方转入 per-CPU pageset 账本：free_map 置 true，
    /// `allocated` 减一。返回 false = 疑似二次释放。
    #[inline]
    fn stage_pcp_page(&mut self, frame: PhysPageNum) -> bool {
        if self.is_page_free(frame) {
            crate::println!(
                "[BUDDY-DOUBLEFREE] ppn={:#x} order=0 already free — dropping to prevent double-ownership",
                frame.0
            );
            return false;
        }
        self.mark_pages(frame, 1, true);
        self.allocated -= 1;
        true
    }

    /// 把处于 per-CPU pageset 账本的页归还 buddy 全局链表。
    /// 进入时该页必须 free（pageset 语义）；先激活计数再走普通 dealloc
    /// 触发伙伴合并，净效果 allocated 不变。
    #[inline]
    fn move_pcp_page_to_buddy(&mut self, frame: PhysPageNum) {
        if !self.is_page_free(frame) {
            crate::println!(
                "[BUDDY-PCP] drain ppn={:#x} but not free — dropping",
                frame.0
            );
            return;
        }
        self.mark_pages(frame, 1, false);
        self.allocated += 1;
        self.dealloc(frame, 0);
    }

    /// 获取已分配页数
    pub fn allocated(&self) -> usize {
        self.allocated
    }

    /// 获取总页数
    pub fn total(&self) -> usize {
        self.total_pages
    }
}

/// 全局页帧分配器。
///
/// 使用 FIFO `TicketSpinLock`：order-0 快路径已由 per-CPU pageset 吸收，
/// 全局锁只服务 order>0、批量 refill/drain 与压力路径；ticket 公平性防止
/// 单核连续大分配饿死其他 hart 的小分配。
pub static FRAME_ALLOCATOR: TicketSpinLock<BuddyAllocator> = TicketSpinLock::new(BuddyAllocator::new());

// ── typed_frame.rs / Frame<M>::Drop 回调入口 ────────────────────────────
//
// 这两个函数供 typed_frame.rs 的 RAII Drop 调用，将帧归还 buddy。
// 它们只操作 buddy 分配器（MetaSlot 已在 typed_frame.rs 的 Drop 中处理完毕）。

/// 归还单帧到 buddy（typed_frame.rs Frame<M>::Drop / UniqueFrame<M>::Drop 调用）。
pub(super) fn return_frame_to_buddy(ppn: PhysPageNum, order: usize) {
    if order == 0 {
        pcp_release_page(ppn);
    } else {
        FRAME_ALLOCATOR.lock().dealloc(ppn, order);
    }
}

/// 归还连续帧块到 buddy（typed_frame.rs Segment<M>::Drop 调用）。
pub(super) fn return_frames_to_buddy(ppn: PhysPageNum, order: usize) {
    FRAME_ALLOCATOR.lock().dealloc(ppn, order);
}

// ── 初始化 ──────────────────────────────────────────────────────────────

/// 初始化页帧分配器
pub fn init(start: usize, end: usize) {
    FRAME_ALLOCATOR.lock().init(start, end);
}

/// Add an additional (possibly discontiguous) physical RAM region.
pub fn add_region(start: usize, end: usize) {
    FRAME_ALLOCATOR.lock().add_region(start, end);
}

// ── 内核堆借页 ─────────────────────────────────────────────────────────

/// 从 buddy 借出 2^order 个连续物理页帧，永久转作内核堆增长区。
///
/// 这些帧此后不再归还 buddy，故不参与 CoW 引用计数、无需 RAII tracker；
/// MetaSlot 标记为 `FrameOwner::Heap`（ref_count = UNIQUE），供统计和防御性
/// 检查使用。
///
/// 锁约定：仅由内核堆扩堆路径在持堆锁时调用，内部只取 `FRAME_ALLOCATOR` 锁；
/// buddy 空闲链表已在 init 预留满容量、`alloc` 全程不回调堆，锁序恒为
/// HEAP→FRAME，无环路、无自死锁。
pub fn alloc_kernel_heap_region(order: usize) -> Option<(usize, usize)> {
    let ppn = FRAME_ALLOCATOR.lock().alloc(order)?;
    let page_count = 1usize << order;

    // MetaSlot 标记为堆占用（Inv.1：检查每帧 UNUSED → 设为 UNIQUE + Heap）
    page_meta::batch_init_heap(ppn, page_count);
    page_meta::meta_of(ppn).set_allocator(page_meta::AllocCookie::Unknown);

    let vaddr = ppn.kernel_addr();
    let len = page_count * PAGE_SIZE;
    Some((vaddr, len))
}

// ── GDB 锚点函数 — 硬件观察点取证（AGENTS.md 312-316）───────────────
/// GDB 在此函数设断点后 disable 自己并武装 watchpoint。
/// 仅当目标帧（已知崩溃帧 PPN）被分配时调用，不影响正常路径。
#[inline(never)]
#[no_mangle]
pub extern "C" fn uaf_target_alloc(ppn: usize) {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("nop", in("a0") ppn, options(nostack, nomem)); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("nop", in("$a0") ppn, options(nostack, nomem)); }
}

/// 目标帧被释放时的锚点
#[inline(never)]
#[no_mangle]
pub extern "C" fn uaf_target_free(ppn: usize) {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("nop\nnop", in("a0") ppn, options(nostack, nomem)); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("nop\nnop", in("$a0") ppn, options(nostack, nomem)); }
}

/// 调试目标 PPN（已知崩溃帧），0 表示禁用。
/// 硬编码 ppn=0x86f1d 用于 GDB 取证（run18 崩溃帧）。
static UAF_TARGET_PPN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0x86f1d);

pub fn set_uaf_target(ppn: usize) {
    UAF_TARGET_PPN.store(ppn, core::sync::atomic::Ordering::Relaxed);
}

/// 帧调试插桩总开关：map_count 追踪、freed-while-mapped/活内核栈检测等
/// 取证链。这些检查在帧分配/映射热路径上每页付出原子操作乃至 try_lock，
/// 因此默认关闭；由 bootarg `FRAMEDBG=1` 运行期开启，无需改源码重建就能拿
/// 双重分配 / UAF 铁证（关闭时每次检查只是一次 Relaxed 原子读）。
static FRAME_DBG_ON: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 运行期读取帧调试开关。
#[inline(always)]
pub(super) fn frame_dbg() -> bool {
    FRAME_DBG_ON.load(core::sync::atomic::Ordering::Relaxed)
}

/// 由 cmdline 解析结果设置帧调试开关。
pub fn set_frame_dbg(on: bool) {
    FRAME_DBG_ON.store(on, core::sync::atomic::Ordering::Relaxed);
}

// ── 单帧分配/释放 ──────────────────────────────────────────────────────

/// [DEBUG 插桩] 帧从 buddy 弹出时校验 map_count==0。非 0 = freed-while-mapped 铁证。
#[inline]
fn dbg_check_map_on_alloc(ppn: PhysPageNum, slot: &page_meta::MetaSlot, ctx: &str) {
    if !frame_dbg() {
        return;
    }
    let mc = slot.map_count();
    if mc != 0 {
        crate::println!(
            "[STALE-MAP] {} ppn={:#x} map_count={} owner={:?} — \
             freed-while-mapped DOUBLE-ALLOC root cause!",
            ctx, ppn.0, mc, slot.get_owner()
        );
        // 扫描哪个进程/vpn 仍映射它（冷路径，best-effort）
        page_meta::assert_unmapped(ppn, ctx);
        // 复位以免级联误报
        for _ in 0..mc { slot.dec_map(); }
    }
}

/// [DEBUG 插桩] 把帧交给用户态前：若它（按页）落在某个仍持有内核栈的进程
/// 范围内，说明我们正把一个活进程的内核栈帧改派成用户页 = freed-while-live
/// 双重分配铁证。用 try_lock 避免与 FRAME_ALLOCATOR 锁序死锁（best-effort）。
#[inline]
fn dbg_check_user_over_kstack(ppn: PhysPageNum) {
    if !frame_dbg() {
        return;
    }
    if let Some(guard) = crate::sched::process::PROCESS_TABLE.try_lock() {
        if let Some(table) = guard.as_ref() {
            if let Some((pid, state, is_thread, tgid)) = table.dbg_frame_in_live_kstack(ppn.0) {
                crate::println!(
                    "[USER-OVER-KSTACK] ppn={:#x} overlaps kstack of pid={} tgid={} state={:?} is_thread={} \
                     — handing a LIVE kernel stack frame to USER! ROOT CAUSE",
                    ppn.0, pid, tgid, state, is_thread,
                );
            }
        }
    }
}

/// Per-CPU pageset 快路径：本地弹一页。未命中时从 buddy 批量补 32 页，
/// 本 hart 只拿走一页，其余留在本地栈（Linux `rmqueue_bulk` 对标）。
/// 返回的页已经完成 free_map / allocated 记账，仍待调用方初始化 MetaSlot。
/// 从全局 buddy 批量补充 pageset（Linux `rmqueue_bulk`）。`take_first`
/// 为 true 时第一页直接返回给调用方；false 时整批留在本地栈。
fn pcp_refill(cpu: usize, take_first: bool) -> Option<PhysPageNum> {
    let mut batch = [PhysPageNum(0); PCP_BATCH];
    let n = {
        let mut allocator = FRAME_ALLOCATOR.lock();
        let mut n = 0usize;
        while n < PCP_BATCH {
            match allocator.alloc(0) {
                Some(ppn) => {
                    batch[n] = ppn;
                    n += 1;
                }
                None => break,
            }
        }
        n
    };
    if n == 0 {
        return None;
    }

    let first = take_first.then_some(batch[0]);
    let keep_start = usize::from(take_first);
    if keep_start < n {
        {
            let mut allocator = FRAME_ALLOCATOR.lock();
            for ppn in &batch[keep_start..n] {
                allocator.stage_pcp_page(*ppn);
            }
        }
        let mut stack = PERCPU_PAGES[cpu].lock();
        let inserted = stack.put_batch(&batch[keep_start..n]);
        if inserted > 0 {
            stack.refills += 1;
        }
        drop(stack);
        // 本地栈被其他 hart drain/refill 打满：剩余页直接回 buddy。
        if keep_start + inserted < n {
            let mut allocator = FRAME_ALLOCATOR.lock();
            for ppn in &batch[keep_start + inserted..n] {
                allocator.move_pcp_page_to_buddy(*ppn);
            }
        }
    }
    first
}

fn pcp_alloc_page() -> Option<PhysPageNum> {
    let cpu = current_cpu_id().min(MAX_CPUS.saturating_sub(1));
    {
        let mut stack = PERCPU_PAGES[cpu].lock();
        if let Some(ppn) = stack.pop() {
            stack.hits += 1;
            // Linux pcp 在低于 low watermark 时**预取**下一批，不等到空。
            let need_refill = stack.needs_refill();
            drop(stack);
            // 激活必须在自己独立的 FRAME_ALLOCATOR 临界区内完成；随后
            // 若需要预取，必须**先释放全局锁**再进入 pcp_refill——
            // 否则同 hart 嵌套获取同一把 ticket 锁自死锁。
            let activated = {
                let mut allocator = FRAME_ALLOCATOR.lock();
                allocator.activate_pcp_page(ppn)
            };
            if !activated {
                // 所有权探针已报错；落入全局慢路径，不把坏页交给调用方。
                return pcp_refill(cpu, true);
            }
            if need_refill {
                pcp_refill(cpu, false);
            }
            return Some(ppn);
        }
        stack.misses += 1;
    }
    pcp_refill(cpu, true)
}

/// 单帧释放的 per-CPU 快路径；本地栈满时转回 buddy。
fn pcp_release_page(ppn: PhysPageNum) {
    let cpu = current_cpu_id().min(MAX_CPUS.saturating_sub(1));
    {
        let mut allocator = FRAME_ALLOCATOR.lock();
        if !allocator.stage_pcp_page(ppn) {
            return;
        }
    }
    // Linux pcp free：越过 high watermark 后一次归还一个 batch，而不是
    // 等到容量满再逐页回灌 buddy。
    let flush = {
        let mut stack = PERCPU_PAGES[cpu].lock();
        if !stack.push(ppn) {
            drop(stack);
            let mut allocator = FRAME_ALLOCATOR.lock();
            allocator.move_pcp_page_to_buddy(ppn);
            return;
        }
        if !stack.should_flush() {
            return;
        }
        let mut out = [PhysPageNum(0); PCP_BATCH];
        let n = stack.take_batch(&mut out);
        (out, n)
    };
    let (out, n) = flush;
    let mut allocator = FRAME_ALLOCATOR.lock();
    for ppn in &out[..n] {
        allocator.move_pcp_page_to_buddy(*ppn);
    }
}

/// 内存压力兜底：把所有 CPU 的 pageset 页排回 buddy 全局链表，返回移动页数。
fn pcp_drain_all() -> usize {
    // Linux `drain_all_pages`：每个 CPU 一次锁内整体摘除，放锁后统一回灌
    // buddy。单次 128 页的固定栈缓冲不触发堆分配，也不给并发生产者制造
    // “摘一批、又长一批”的无界循环。
    let mut moved = 0usize;
    for cpu in 0..MAX_CPUS {
        let mut batch = [PhysPageNum(0); PCP_CAP];
        let n = {
            let mut stack = PERCPU_PAGES[cpu].lock();
            let n = stack.take_batch(&mut batch);
            if n > 0 {
                stack.drains += 1;
            }
            n
        };
        if n == 0 {
            continue;
        }
        let mut allocator = FRAME_ALLOCATOR.lock();
        for ppn in &batch[..n] {
            allocator.move_pcp_page_to_buddy(*ppn);
        }
        moved += n;
    }
    moved
}

/// idle CPU 预清零帧池（Linux 无对应机制；对标 MangoCore/WaterOS 实测
/// page-fault 热路径 memset 占比）。idle loop 在无可运行任务时批量清零
/// 空闲帧入池；`alloc_frame` 优先弹池，命中即跳过热路径上的 4K memset。
/// 池内帧的 MetaSlot 保持 UNUSED，弹出时才 init_as_shared（不破坏 Inv.1）。
const ZERO_POOL_MAX: usize = 1024; // 4 MiB 上限

/// 无堆固定数组栈：池操作不得触发堆分配（堆扩容 → alloc_frame →
/// 再取池锁 = 同 hart 自死锁）。
struct ZeroPool {
    frames: [PhysPageNum; ZERO_POOL_MAX],
    len: usize,
}

static ZERO_POOL: crate::sync::SpinLock<ZeroPool> = crate::sync::SpinLock::new(ZeroPool {
    frames: [PhysPageNum(0); ZERO_POOL_MAX],
    len: 0,
});

/// idle 补池：每次最多清零 `batch` 帧；池满/无空闲帧时 O(1) 返回。
pub fn zero_pool_refill(batch: usize) {
    for _ in 0..batch {
        if ZERO_POOL.lock().len >= ZERO_POOL_MAX {
            return;
        }
        let raw = pcp_alloc_page().or_else(|| FRAME_ALLOCATOR.lock().alloc(0));
        let Some(ppn) = raw else { return };
        zero_frame(ppn, 1);
        let mut pool = ZERO_POOL.lock();
        if pool.len < ZERO_POOL_MAX {
            let idx = pool.len;
            pool.frames[idx] = ppn;
            pool.len += 1;
        } else {
            drop(pool);
            FRAME_ALLOCATOR.lock().dealloc(ppn, 0);
            return;
        }
    }
}

#[inline]
fn zero_pool_pop() -> Option<PhysPageNum> {
    let mut pool = ZERO_POOL.lock();
    if pool.len == 0 {
        return None;
    }
    pool.len -= 1;
    let idx = pool.len;
    Some(pool.frames[idx])
}

/// 内存压力回退：普通分配路径失败时把池内帧全部归还 buddy 再重试。
/// 返回归还帧数。
fn zero_pool_drain_to_buddy() -> usize {
    let mut n = 0;
    loop {
        let Some(ppn) = zero_pool_pop() else { break };
        FRAME_ALLOCATOR.lock().dealloc(ppn, 0);
        n += 1;
    }
    n
}

/// 分配单帧并初始化 MetaSlot（ref_count = 1, owner = User）。
///
/// 清零帧内容，设 MetaSlot 为共享引用计数 1。
pub fn alloc_frame() -> Option<PhysPageNum> {
    if let Some(ppn) = zero_pool_pop() {
        let slot = page_meta::meta_of(ppn);
        dbg_check_map_on_alloc(ppn, slot, "alloc_frame(zero_pool)");
        dbg_check_user_over_kstack(ppn);
        slot.init_as_shared(FrameOwner::User);
        let target = UAF_TARGET_PPN.load(core::sync::atomic::Ordering::Relaxed);
        if target != 0 && ppn.0 == target {
            uaf_target_alloc(ppn.0);
        }
        return Some(ppn);
    }
    let raw = pcp_alloc_page()
        .or_else(|| FRAME_ALLOCATOR.lock().alloc(0))
        .or_else(|| alloc_pressure_fallback(0));
    raw.map(|ppn| {
        // MetaSlot Inv.1：检查 UNUSED → 初始化为共享引用 1
        let slot = page_meta::meta_of(ppn);
        // [DEBUG 插桩] 铁证检测：从 buddy 弹出的空闲帧若仍被用户 PTE 映射
        // （map_count != 0）= freed-while-mapped 双重分配根因。
        dbg_check_map_on_alloc(ppn, slot, "alloc_frame");
        dbg_check_user_over_kstack(ppn);
        slot.init_as_shared(FrameOwner::User);

        // 清零页帧
        zero_frame(ppn, 1);

        // GDB 锚点：目标帧被分配时触发
        let target = UAF_TARGET_PPN.load(core::sync::atomic::Ordering::Relaxed);
        if target != 0 && ppn.0 == target {
            uaf_target_alloc(ppn.0);
        }

        ppn
    })
}

/// 同 `alloc_frame` 但**不清零**帧内容：仅供调用方随即用 `copy_from_slice`
/// 整页覆写的场景（CoW 拷贝、fork 深拷贝、页缓存私有化），省掉一次 4KB
/// memset。不得用于会把未初始化内容暴露给用户态的路径。
pub fn alloc_frame_uninit() -> Option<PhysPageNum> {
    let raw = pcp_alloc_page()
        .or_else(|| FRAME_ALLOCATOR.lock().alloc(0))
        .or_else(|| alloc_pressure_fallback(0));
    raw.map(|ppn| {
        let slot = page_meta::meta_of(ppn);
        dbg_check_map_on_alloc(ppn, slot, "alloc_frame_uninit");
        dbg_check_user_over_kstack(ppn);
        slot.init_as_shared(FrameOwner::User);
        let target = UAF_TARGET_PPN.load(core::sync::atomic::Ordering::Relaxed);
        if target != 0 && ppn.0 == target {
            uaf_target_alloc(ppn.0);
        }
        ppn
    })
}

/// 释放单帧（引用计数 -1，降到 0 时归还 buddy）。
///
/// 兼容旧代码直接调用路径（非 FrameTracker）。
/// [DEBUG 插桩] 读取当前 hart 的 sp 寄存器。
#[inline(always)]
fn dbg_current_sp() -> usize {
    let sp: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("move {}, $sp", out(reg) sp, options(nomem, nostack)); }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    { sp = 0; }
    sp
}

/// [DEBUG 插桩] 释放页帧块前：若任一页 owner==KernelStack 且当前 hart 的 sp
/// 仍落在该页内，说明正在“释放自己脚下的内核栈” = UAF 铁证。
#[inline]
fn dbg_check_kstack_free(ppn: PhysPageNum, page_count: usize, ctx: &str, caller_ra: usize) {
    if !frame_dbg() {
        return;
    }
    let sp = dbg_current_sp();
    for i in 0..page_count {
        let p = PhysPageNum(ppn.0 + i);
        let slot = match page_meta::try_meta_of(p) { Some(s) => s, None => continue };
        if slot.get_owner() != FrameOwner::KernelStack { continue; }
        // RISC-V identity map: 页内核虚拟地址 == 物理地址；LA 需转换
        #[cfg(target_arch = "riscv64")]
        let page_va = p.0 << 12;
        #[cfg(not(target_arch = "riscv64"))]
        let page_va = crate::mm::phys_to_kernel_vaddr(p.0 << 12);
        if sp >= page_va && sp < page_va + PAGE_SIZE {
            crate::println!(
                "[KSTK-FREE-SELF] {} block_base={:#x} pc={}pages matchpage={:#x} sp={:#x} caller_ra={:#x} — freeing kernel stack we are RUNNING ON! ROOT CAUSE",
                ctx, ppn.0, page_count, p.0, sp, caller_ra,
            );
        }
    }
}

/// 读取调用者返回地址（dealloc_frames 自己的 ra）。必须在函数体最前调用。
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

pub fn dealloc_frame(ppn: PhysPageNum) {
    let caller_ra = dbg_caller_ra();
    // GDB 锚点：目标帧被释放时触发
    let target = UAF_TARGET_PPN.load(core::sync::atomic::Ordering::Relaxed);
    if target != 0 && ppn.0 == target {
        uaf_target_free(ppn.0);
    }
    dbg_check_kstack_free(ppn, 1, "dealloc_frame", caller_ra);

    let slot = page_meta::meta_of(ppn);
    page_meta::record_free(ppn, slot.get_owner());
    let rc = slot.ref_count();

    if rc == 0 {
        crate::println!(
            "[META] dealloc_frame: ppn={:#x} ref_count already 0 — double-free prevented",
            ppn.0,
        );
        return;
    }

    let will_free = if rc == page_meta::REF_COUNT_UNIQUE { true } else { rc == 1 };
    if will_free {
        page_meta::assert_unmapped(ppn, "dealloc_frame");
    }

    if rc == page_meta::REF_COUNT_UNIQUE {
        // 独占帧直接释放
        if slot.release_unique() {
            pcp_release_page(ppn);
        }
    } else {
        // 共享帧递减引用
        if slot.dec_ref() {
            slot.finalize_release();
            pcp_release_page(ppn);
        }
    }
}

/// 强制释放单帧（不检查引用计数，直接归还 buddy）。
///
/// **仅供紧急/特殊路径使用**（如 early boot cleanup）。正常代码应使用
/// `dealloc_frame` 或 RAII Drop。
pub fn dealloc_frame_force(ppn: PhysPageNum) {
    if let Some(slot) = page_meta::try_meta_of(ppn) {
        // 无论当前状态，强制归零 ref_count + 元数据
        slot.force_release();
    }
    pcp_release_page(ppn);
}

/// 增加帧引用计数（用于 CoW fork）。
pub fn inc_frame_ref(ppn: PhysPageNum) {
    let slot = page_meta::meta_of(ppn);
    slot.inc_ref();
}

/// 减少帧引用计数（inc_frame_ref 的对称操作；futex 页钉扎等短窗口使用）。
pub fn dec_frame_ref(ppn: PhysPageNum) {
    let slot = page_meta::meta_of(ppn);
    slot.dec_ref();
}

/// 获取帧引用计数。
pub fn get_frame_ref(ppn: PhysPageNum) -> u16 {
    let slot = page_meta::meta_of(ppn);
    let rc = slot.ref_count();
    if rc == page_meta::REF_COUNT_UNIQUE {
        1 // 独占帧对外等价引用计数 1
    } else if rc > u16::MAX as u32 {
        u16::MAX
    } else {
        rc as u16
    }
}

// ── HugeTLB 2MiB 块缓存 ────────────────────────────────────────────────
//
// 长跑批次里 4K 分配/释放的碎片化会让 order-9（2MiB）连续块在中后期无法
// 从 buddy 拿到（hugetlb shmget/mmap 缺页 ENOMEM）。对策：曾经成形的 2MiB
// 块释放时不还 buddy（避免被 4K churn 切碎），进 per-order 缓存供下次
// hugetlb 分配直接复用；任何常规分配失败（真正内存压力）时整体 drain 回
// buddy 再重试——缓存是软预留，绝不挤占常规内存。
const HUGE_BLOCK_ORDER: usize = 9;
static HUGE_BLOCK_CACHE: crate::sync::SpinLock<alloc::vec::Vec<PhysPageNum>> =
    crate::sync::SpinLock::new(alloc::vec::Vec::new());

fn huge_cache_pop() -> Option<PhysPageNum> {
    HUGE_BLOCK_CACHE.lock().pop()
}

fn huge_cache_push(ppn: PhysPageNum) {
    HUGE_BLOCK_CACHE.lock().push(ppn);
}

/// 分配失败兜底（对标 Linux direct reclaim）：先把 hugetlb 块缓存退回
/// buddy，再驱逐页缓存，然后重试一次。页缓存会长到吃满空闲内存，
/// 不回收则长跑批次中后期的大块连续分配（hugetlb order-9）必然 ENOMEM。
/// order>0 时需要连续块，碎片化下少量驱逐难以凑出连续区间，故全量驱逐。
fn alloc_pressure_fallback(order: usize) -> Option<PhysPageNum> {
    // 0. 预清零池先回 buddy（帧本就干净可直接复用）。
    let pool_drained = zero_pool_drain_to_buddy();
    // 1. 先把所有 CPU pageset 排回 buddy（Linux rmqueue fallback → drain pcp）。
    let pcp_moved = pcp_drain_all();
    // 2. 软预留的 2MiB hugetlb 块回池。
    let drained = huge_cache_drain();
    // 3. 仍不够才驱逐页缓存（order>0 需要连续块，全量驱逐）。
    let target = if order == 0 { 1024 } else { usize::MAX };
    let reclaimed = crate::mm::page_cache::emergency_reclaim(target);
    if pool_drained == 0 && pcp_moved == 0 && drained == 0 && reclaimed == 0 {
        return None;
    }
    FRAME_ALLOCATOR.lock().alloc(order)
}

/// 把缓存的 2MiB 块全部归还 buddy（内存压力兜底）。返回归还块数。
pub fn huge_cache_drain() -> usize {
    let blocks: alloc::vec::Vec<PhysPageNum> = {
        let mut c = HUGE_BLOCK_CACHE.lock();
        core::mem::take(&mut *c)
    };
    let n = blocks.len();
    let mut alloc_guard = FRAME_ALLOCATOR.lock();
    for ppn in blocks {
        alloc_guard.dealloc(ppn, HUGE_BLOCK_ORDER);
    }
    n
}

// ── 连续帧分配/释放 ────────────────────────────────────────────────────

/// 分配 2^order 个连续页帧。
///
/// MetaSlot 每帧设为 ref_count = 1, owner = User（兼容旧语义）。
/// 清零全部帧内容。
pub fn alloc_frames(order: usize) -> Option<PhysPageNum> {
    let raw = if order == HUGE_BLOCK_ORDER {
        huge_cache_pop().or_else(|| FRAME_ALLOCATOR.lock().alloc(order))
    } else if order == 0 {
        pcp_alloc_page().or_else(|| FRAME_ALLOCATOR.lock().alloc(0))
    } else {
        FRAME_ALLOCATOR.lock().alloc(order)
    };
    let raw = raw.or_else(|| alloc_pressure_fallback(order));
    raw.map(|ppn| {
        let page_count = 1usize << order;

        // [DEBUG 插桩] 多页块每帧校验 map_count==0
        for i in 0..page_count {
            let p = PhysPageNum(ppn.0 + i);
            dbg_check_map_on_alloc(p, page_meta::meta_of(p), "alloc_frames");
        }

        // MetaSlot Inv.1：检查每帧 UNUSED → 初始化
        page_meta::batch_init_shared(ppn, page_count, FrameOwner::User, order as u8);
        page_meta::meta_of(ppn).set_allocator(page_meta::AllocCookie::Buddy);

        // 清零
        zero_frame(ppn, page_count);

        ppn
    })
}

/// 同 `alloc_frames` 但**不清零**：仅供内容先写后读的内核栈等场景，
/// 不得用于会把未初始化内容暴露给用户态的路径。
pub fn alloc_frames_uninit(order: usize) -> Option<PhysPageNum> {
    let raw = if order == HUGE_BLOCK_ORDER {
        huge_cache_pop().or_else(|| FRAME_ALLOCATOR.lock().alloc(order))
    } else if order == 0 {
        pcp_alloc_page().or_else(|| FRAME_ALLOCATOR.lock().alloc(0))
    } else {
        FRAME_ALLOCATOR.lock().alloc(order)
    };
    let raw = raw.or_else(|| alloc_pressure_fallback(order));
    raw.map(|ppn| {
        let page_count = 1usize << order;
        for i in 0..page_count {
            let p = PhysPageNum(ppn.0 + i);
            dbg_check_map_on_alloc(p, page_meta::meta_of(p), "alloc_frames_uninit");
        }
        page_meta::batch_init_shared(ppn, page_count, FrameOwner::User, order as u8);
        page_meta::meta_of(ppn).set_allocator(page_meta::AllocCookie::Buddy);
        ppn
    })
}

/// 释放 2^order 个连续页帧。
///
/// 每帧通过 MetaSlot 正确释放（处理 UNIQUE 和共享两种状态），
/// 最后将整个连续块归还 buddy。
pub fn dealloc_frames(ppn: PhysPageNum, order: usize) {
    let caller_ra = dbg_caller_ra();
    let page_count = 1usize << order;
    dbg_check_kstack_free(ppn, page_count, "dealloc_frames", caller_ra);
    for i in 0..page_count {
        let slot = page_meta::meta_of(PhysPageNum(ppn.0 + i));
        page_meta::record_free(PhysPageNum(ppn.0 + i), slot.get_owner());
        let rc = slot.ref_count();
        if rc == page_meta::REF_COUNT_UNIQUE {
            slot.release_unique();
        } else if rc >= 1 {
            if slot.dec_ref() {
                slot.finalize_release();
            }
        }
        // rc == 0: already free, no-op (shouldn't happen but defensive)
    }
    if order == HUGE_BLOCK_ORDER {
        huge_cache_push(ppn);
    } else if order == 0 {
        pcp_release_page(ppn);
    } else {
        FRAME_ALLOCATOR.lock().dealloc(ppn, order);
    }
}

// ── FrameTracker（兼容包装）─────────────────────────────────────────────
//
// FrameTracker 保持现有外部接口不变，但内部通过 MetaSlot 管理引用计数。
// 新代码应优先使用 typed_frame.rs 的 Frame<M> / UniqueFrame<M> / Segment<M>，
// FrameTracker 作为过渡兼容层。

/// 页帧追踪器（RAII）
///
/// Drop 时通过 MetaSlot ref_count 释放：
/// - order == 0（单帧）：ref_count -1，到 0 时归还 buddy
/// - order > 0（多帧）：强制清零并归还 buddy
#[derive(Debug)]
pub struct FrameTracker {
    pub ppn: PhysPageNum,
    pub order: usize,
}

impl FrameTracker {
    pub fn new(ppn: PhysPageNum) -> Self {
        Self { ppn, order: 0 }
    }

    pub fn new_order(ppn: PhysPageNum, order: usize) -> Self {
        Self { ppn, order }
    }
}

/// 释放一个 FrameTracker/SharedFrame 持有的 MetaSlot 引用。
///
/// 多页块把首帧作为提交点：获取引用时首帧最先 +1，释放时首帧最后 -1。
/// 因此首帧从 1 原子减到 0 的线程一定能观察到其余帧已经完成减引用，且只
/// 有该线程可以清理元数据并把整块归还 buddy。这个顺序避免并发 drop 时不同
/// CPU 分别成为不同页面的最后释放者，进而造成整块泄漏或重复归还。
fn release_tracked_frames(ppn: PhysPageNum, order: usize, context: &str) {
    #[inline]
    fn release_slot(slot: &page_meta::MetaSlot) -> bool {
        let rc = slot.ref_count();
        assert!(
            rc != page_meta::REF_COUNT_UNUSED,
            "[META] release_tracked_frames: ref_count already zero"
        );
        if rc == page_meta::REF_COUNT_UNIQUE {
            slot.release_unique()
        } else {
            slot.dec_ref()
        }
    }

    let page_count = 1usize << order;
    for i in 1..page_count {
        release_slot(page_meta::meta_of(PhysPageNum(ppn.0 + i)));
    }

    let head = page_meta::meta_of(ppn);
    if !release_slot(head) {
        return;
    }

    for i in 0..page_count {
        let frame_ppn = PhysPageNum(ppn.0 + i);
        let slot = page_meta::meta_of(frame_ppn);
        assert_eq!(
            slot.ref_count(),
            page_meta::REF_COUNT_UNUSED,
            "[META] release_tracked_frames: block tail still referenced"
        );
        page_meta::assert_unmapped(frame_ppn, context);
        slot.finalize_release();
    }

    if order == HUGE_BLOCK_ORDER {
        huge_cache_push(ppn);
    } else {
        FRAME_ALLOCATOR.lock().dealloc(ppn, order);
    }
}

impl Drop for FrameTracker {
    fn drop(&mut self) {
        release_tracked_frames(self.ppn, self.order, "FrameTracker::drop");
    }
}

// ── SharedFrame：平衡共享帧句柄 ─────────────────────────────────────────
//
// 不变量：MetaSlot.ref_count == 全部存活持有者数（VMA frames 条目、页缓存
// 条目、全局零页引用等）。clone = MetaSlot +1（每帧），drop = MetaSlot -1，
// 归零时归还 buddy。旧代码直接 clone `Arc<FrameTracker>` 只增 Arc 强计数、
// 不增 MetaSlot，导致 fork/页缓存/零页共享帧在任一方释放时被提前归还 buddy
// （freed-while-mapped）。2026-08-14 FRAMEPROBE 探针实锤：
//   [FPDBG] clone-slow-hit … meta=1 strong=2
//   [FPDBG] release … meta=1 strong=2
//   [FPDBG] cow old_ppn=0x9cf3 old_meta=1 old_strong=2/3/8
// 本类型把「共享即计数」做成类型不变量，任何 .clone() 自动配对 inc。
#[derive(Debug)]
pub struct SharedFrame(alloc::sync::Arc<core::mem::ManuallyDrop<FrameTracker>>);

impl SharedFrame {
    /// 从分配器新帧构造（MetaSlot ref 已由分配器初始化为 1，本句柄持有该份）。
    #[inline]
    pub fn from_alloc(tracker: FrameTracker) -> Self {
        Self(alloc::sync::Arc::new(core::mem::ManuallyDrop::new(tracker)))
    }

    /// 从裸 ppn 建立新持有（MetaSlot +1，多帧块每帧 +1）。
    /// 用于 frames/PTE 修复分支等「已映射帧再登记一个所有者」的路径。
    #[inline]
    pub fn from_ppn_inc(ppn: PhysPageNum, order: usize) -> Self {
        let count = if order == 0 { 1 } else { 1usize << order };
        for i in 0..count {
            inc_frame_ref(PhysPageNum(ppn.0 + i));
        }
        Self(alloc::sync::Arc::new(core::mem::ManuallyDrop::new(
            FrameTracker::new_order(ppn, order),
        )))
    }

    /// Arc 强计数（同一 Arc 实例的克隆数；仅诊断/启发式，权威共享判据是 MetaSlot）。
    #[inline]
    pub fn strong(&self) -> usize {
        alloc::sync::Arc::strong_count(&self.0)
    }

    /// MetaSlot 引用计数（权威共享判据：== 全部存活持有者数）。
    #[inline]
    pub fn meta_ref(&self) -> u16 {
        get_frame_ref(self.0.ppn)
    }

    /// 底层 Arc 的指针（ptr_eq_prefix_ptrs 的帧身份比较用）。
    #[inline]
    pub fn arc_ptr(&self) -> *const FrameTracker {
        alloc::sync::Arc::as_ptr(&self.0).cast::<FrameTracker>()
    }

    /// 与另一句柄是否同一 Arc 实例（同帧同来源）。
    #[inline]
    pub fn is_same_arc(&self, other: &Self) -> bool {
        alloc::sync::Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Clone for SharedFrame {
    /// clone = 新持有者：MetaSlot +1（多帧块每帧 +1）+ Arc 克隆。
    #[inline]
    fn clone(&self) -> Self {
        let order = self.0.order;
        let count = if order == 0 { 1 } else { 1usize << order };
        for i in 0..count {
            inc_frame_ref(PhysPageNum(self.0.ppn.0 + i));
        }
        Self(self.0.clone())
    }
}

impl Drop for SharedFrame {
    /// 每个 SharedFrame 句柄都对应一份 MetaSlot 引用。
    ///
    /// Arc 内层使用 ManuallyDrop，禁止最后一个 Arc 再隐式执行 FrameTracker::drop；
    /// 否则最后一个句柄会被减两次。每个句柄都在这里恰好释放一次，归零线程
    /// 由 MetaSlot 的原子减引用唯一确定。
    #[inline]
    fn drop(&mut self) {
        release_tracked_frames(self.0.ppn, self.0.order, "SharedFrame::drop");
    }
}

impl core::ops::Deref for SharedFrame {
    type Target = FrameTracker;
    #[inline]
    fn deref(&self) -> &FrameTracker {
        &self.0
    }
}

/// 分配页帧并返回追踪器
pub fn frame_alloc() -> Option<FrameTracker> {
    alloc_frame().map(FrameTracker::new)
}

/// 同 `frame_alloc` 但不清零（见 `alloc_frame_uninit` 的适用约束）。
pub fn frame_alloc_uninit() -> Option<FrameTracker> {
    alloc_frame_uninit().map(FrameTracker::new)
}

/// 分配 2^order 个连续页帧并返回 RAII 追踪器。
pub fn frame_alloc_order(order: usize) -> Option<FrameTracker> {
    alloc_frames(order).map(|ppn| FrameTracker::new_order(ppn, order))
}

/// 全局共享零页（Linux `ZERO_PAGE` 对标）。
///
/// 私有匿名映射的**读**缺页不分配真实帧，而是把所有进程的该类页都映射到
/// 这一张只读零页上（PTE 去 W/D），写入时经 CoW 断开（Arc 引用计数恒 >1，
/// `handle_cow` 必走复制分支）。本静态持有一份 Arc，保证零页永不归还 buddy。
static ZERO_PAGE: crate::sync::SpinLock<Option<SharedFrame>> =
    crate::sync::SpinLock::new(None);

/// 取全局零页的共享引用（懒初始化；OOM 时返回 None，调用方回退真实分配）。
/// clone = MetaSlot +1：每个映射零页的 VMA 各持一份引用，最后一个引用释放
/// 也不归还（本静态恒持一份），语义对标 Linux ZERO_PAGE 的 page ref 恒 >0。
pub fn zero_page_frame() -> Option<SharedFrame> {
    let mut guard = ZERO_PAGE.lock();
    if guard.is_none() {
        *guard = frame_alloc().map(SharedFrame::from_alloc);
    }
    guard.clone()
}

/// 释放页帧（通过 FrameTracker）
pub fn frame_dealloc(tracker: FrameTracker) {
    drop(tracker);
}

// ── 统计信息 ────────────────────────────────────────────────────────────

/// 获取总内存大小（字节）
pub fn get_total_memory() -> usize {
    FRAME_ALLOCATOR.lock().total() * PAGE_SIZE
}

/// 获取可用内存大小（字节）
pub fn get_available_memory() -> usize {
    let allocator = FRAME_ALLOCATOR.lock();
    (allocator.total() - allocator.allocated()) * PAGE_SIZE
}

/// 重导出
#[allow(unused_imports)]
pub use BuddyAllocator as FrameAllocator;

/// 页帧统计信息
#[derive(Default)]
pub struct FrameStats {
    pub total: usize,
    pub free: usize,
    pub used: usize,
    pub cached: usize,
    pub slab: usize,
    pub page_tables: usize,
}

/// 获取页帧统计信息（使用 MetaSlot 真实数据）
pub fn get_stats() -> FrameStats {
    let allocator = FRAME_ALLOCATOR.lock();
    let total = allocator.total();
    let used = allocator.allocated();
    let free = total.saturating_sub(used);
    drop(allocator);

    // 使用 MetaSlot 的真实统计数据
    let meta_stats = page_meta::collect_stats();

    FrameStats {
        total,
        free,
        used,
        cached: meta_stats.page_cache,
        slab: meta_stats.slab,
        page_tables: meta_stats.page_table,
    }
}

// ── 内部辅助 ────────────────────────────────────────────────────────────

/// RV Zicboz `cbo.zero` 可用标志（boot 期由 DTB 探测置位）。
#[cfg(target_arch = "riscv64")]
static ZICBOZ: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// 启用 `cbo.zero` 页清零快路径（仅当 DTB 声明 Zicboz 且块大小 64B）。
#[cfg(target_arch = "riscv64")]
pub fn enable_zicboz() {
    ZICBOZ.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// 清零物理帧内容。
#[inline]
fn zero_frame(ppn: PhysPageNum, page_count: usize) {
    let base = ppn.kernel_addr();
    let byte_len = page_count * PAGE_SIZE;
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        // LoongArch64 large code model workaround:
        // compiler_builtins 的 memset 在 large code model 下有 PC-relative 寻址 bug，
        // 使用内联汇编逐 8 字节清零绕过。
        // 8 路展开（页大小恒为 64B 的倍数）：TCG 下指针增量/比较/分支
        // 开销摊薄到 64B/迭代。
        let mut ptr = base as *mut u64;
        let end = (base + byte_len) as *mut u64;
        while ptr < end {
            core::ptr::write_volatile(ptr, 0u64);
            core::ptr::write_volatile(ptr.add(1), 0u64);
            core::ptr::write_volatile(ptr.add(2), 0u64);
            core::ptr::write_volatile(ptr.add(3), 0u64);
            core::ptr::write_volatile(ptr.add(4), 0u64);
            core::ptr::write_volatile(ptr.add(5), 0u64);
            core::ptr::write_volatile(ptr.add(6), 0u64);
            core::ptr::write_volatile(ptr.add(7), 0u64);
            ptr = ptr.add(8);
        }
    }
    #[cfg(target_arch = "riscv64")]
    unsafe {
        if ZICBOZ.load(core::sync::atomic::Ordering::Relaxed) {
            // Zicboz `cbo.zero`：每条指令清零 64B cache 块（页恒 64B 对齐/
            // 倍数），TCG 下每页 64 条指令完成清零，对标 Linux clear_page 的
            // Zicboz 快路径。编码：.insn i MISC-MEM(0x0F), funct3=2, rd=x0,
            // rs1=块地址, imm=4。
            let mut p = base;
            let end = base + byte_len;
            while p < end {
                core::arch::asm!(
                    ".insn i 0x0F, 2, x0, {0}, 4",
                    in(reg) p,
                    options(nostack),
                );
                p += 64;
            }
        } else {
            core::ptr::write_bytes(base as *mut u8, 0, byte_len);
        }
    }
    #[cfg(not(any(target_arch = "loongarch64", target_arch = "riscv64")))]
    unsafe {
        core::ptr::write_bytes(base as *mut u8, 0, byte_len);
    }
}

// ── 旧接口兼容层（供尚未迁移的模块使用）────────────────────────────────
//
// 这些函数在旧代码被逐步迁移到 typed_frame 后应删除。

/// UAF 探针总开关：正式轮恒 false；FRAMEDBG=1 时启用 slab 回收前的
/// 活对象计数校验（verify_reclaim_dbg / slab_validate_ptr），用于抓
/// “整页归还 buddy 但仍有对象被引用”的双重分配根因。
#[inline]
pub fn uaf_dbg_enabled() -> bool { frame_dbg() }

/// 已移除。MetaSlot 天然替代 buddy free list 扫描。
#[inline]
pub fn is_in_buddy_free_dbg(_ppn: PhysPageNum) -> bool { false }

/// 已移除。使用 `page_meta::meta_of(ppn).ref_count()` 替代。
#[inline]
pub fn ft_live_count_dbg(_ppn: PhysPageNum) -> u16 { 0 }

/// 已移除。使用 `page_meta::dump_frame_meta(ppn)` 替代。
#[inline]
pub fn target_log_dump_dbg() {}

/// 已移除。
#[inline]
pub fn kstk_record_freed_dbg(_bottom: usize, _size: usize) {}

/// 已移除。
#[inline]
pub fn kstk_running_on_freed_dbg(_sp: usize) -> Option<(usize, usize)> { None }

/// 已移除。
#[inline]
pub fn prov_dump_dbg(_ppn: usize) {}

/// [DEBUG 取证] dump 物理地址 `_pa` 所在帧的 MetaSlot 归属（owner/refcount/order）。
/// 用于判断"缓存扇区堆缓冲区帧"是否同时被别的子系统占用（owner 非 Slab/heap，
/// 或 refcount 异常）。
pub fn frame_ownership_dump_dbg(pa: usize, tag: &str) {
    let ppn = PhysPageNum(pa >> 12);
    if let Some(slot) = crate::mm::page_meta::try_meta_of(ppn) {
        crate::println!(
            "[FRAME-OWN] tag={} pa={:#x} ppn={:#x} owner={:?} ref={} flags={:#04x} order={}",
            tag, pa, ppn.0, slot.get_owner(), slot.ref_count(),
            slot.get_flags(), slot.get_order(),
        );
    } else {
        crate::println!("[FRAME-OWN] tag={} pa={:#x} ppn={:#x} — untracked range", tag, pa, ppn.0);
    }
}

/// 已移除。
#[inline]
pub fn slab_owned_mark_dbg(_ppn: PhysPageNum) {}

/// 已移除。
#[inline]
pub fn slab_owned_clear_dbg(_ppn: PhysPageNum) {}

/// 已移除。
#[inline]
pub fn slab_slot_user_check_dbg(_slot_pa: usize) {}
