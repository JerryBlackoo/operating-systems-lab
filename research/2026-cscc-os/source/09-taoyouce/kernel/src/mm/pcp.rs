//! Per-CPU 页帧缓存（对标 Linux `struct per_cpu_pages` / `pcp->lists`）
//!
//! 这是物理页分配器的快路径底座：order-0 页在 buddy 与调用者之间增加
//! 一层 per-CPU LIFO 栈。本模块只负责**无分配、固定容量**的栈存储与
//! 锁保护，不触碰 buddy 账本；页在 pcp 中的 free/allocated 记账由
//! `frame.rs` 在进入/离开本层时完成，保持单一所有权。
//!
//! 锁选择 `IrqTicketSpinLock`：
//! - 关本地中断：分配/释放页可能发生在定时器/trap 上下文中，避免同 hart
//!   重入同一 per-CPU 栈；
//! - FIFO ticket：内存压力路径会跨 CPU drain 他人的 pcp，公平性避免
//!   排水线程在高争用下被饿死。

use super::PhysPageNum;
use crate::percpu::MAX_CPUS;
use crate::sync::IrqTicketSpinLock;

/// Linux per_cpu_pages 水位：高于 high 时把一批页还给 buddy，低于 low
/// 时从 buddy 批量补充。high/low 均以 batch 为粒度，避免“缺一页就锁
/// 全局 buddy”的抖动。
pub const PCP_HIGH: usize = 128;
pub const PCP_LOW: usize = 48;
/// 单次从 buddy 批量补充 / 排空的页数（Linux `pcp->batch`）。
pub const PCP_BATCH: usize = 32;
/// 本地栈物理容量 = high + 1 个 batch：允许一次释放洪峰越过 high 再统一
/// 排水，而不是每页都锁全局 buddy。
pub const PCP_CAP: usize = PCP_HIGH + PCP_BATCH;

/// 单个 CPU 的固定容量页栈（LIFO，与 Linux pcp list 一致）。
pub(super) struct PerCpuPageStack {
    pages: [PhysPageNum; PCP_CAP],
    len: usize,
    /// 本地命中 / 未命中 / buddy 补充 / 排水统计。
    pub(super) hits: u64,
    pub(super) misses: u64,
    pub(super) refills: u64,
    pub(super) drains: u64,
}

impl PerCpuPageStack {
    pub(super) const fn new() -> Self {
        Self {
            pages: [PhysPageNum(0); PCP_CAP],
            len: 0,
            hits: 0,
            misses: 0,
            refills: 0,
            drains: 0,
        }
    }

    #[inline]
    pub(super) fn pop(&mut self) -> Option<PhysPageNum> {
        if self.len == 0 {
            None
        } else {
            self.len -= 1;
            Some(self.pages[self.len])
        }
    }

    #[inline]
    pub(super) fn push(&mut self, ppn: PhysPageNum) -> bool {
        if self.len >= PCP_CAP {
            false
        } else {
            self.pages[self.len] = ppn;
            self.len += 1;
            true
        }
    }

    #[inline]
    pub(super) fn needs_refill(&self) -> bool {
        self.len < PCP_LOW
    }

    #[inline]
    pub(super) fn should_flush(&self) -> bool {
        self.len > PCP_HIGH
    }

    /// 弹出最多 `out.len()` 页，返回实际数量。
    #[inline]
    pub(super) fn take_batch(&mut self, out: &mut [PhysPageNum]) -> usize {
        let n = self.len.min(out.len());
        for slot in out.iter_mut().take(n) {
            *slot = self.pop().expect("pcp batch pop in range");
        }
        n
    }

    /// 压入尽量多的页，返回实际压入数量。
    #[inline]
    pub(super) fn put_batch(&mut self, items: &[PhysPageNum]) -> usize {
        let mut n = 0;
        for &ppn in items {
            if !self.push(ppn) {
                break;
            }
            n += 1;
        }
        n
    }

    #[inline]
    pub(super) fn len(&self) -> usize {
        self.len
    }
}

/// 每 CPU 一个本地页栈。下标 = 硬件 hart id。
pub(super) static PERCPU_PAGES: [IrqTicketSpinLock<PerCpuPageStack>; MAX_CPUS] =
    [const { IrqTicketSpinLock::new(PerCpuPageStack::new()) }; MAX_CPUS];

/// 导出全局统计（/proc 诊断用，不做高精度）。
pub fn pcp_stats() -> (usize, u64, u64, u64, u64) {
    let mut pages = 0usize;
    let mut hits = 0u64;
    let mut misses = 0u64;
    let mut refills = 0u64;
    let mut drains = 0u64;
    for cpu in 0..MAX_CPUS {
        let stack = PERCPU_PAGES[cpu].lock();
        pages += stack.len();
        hits += stack.hits;
        misses += stack.misses;
        refills += stack.refills;
        drains += stack.drains;
    }
    (pages, hits, misses, refills, drains)
}
