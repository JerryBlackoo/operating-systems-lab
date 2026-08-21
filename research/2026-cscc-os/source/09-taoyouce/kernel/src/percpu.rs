//! Per-CPU 基础设施（对标 Linux `percpu` / `DEFINE_PER_CPU`）
//!
//! 这是所有 per-CPU 数据结构共用的唯一底座：
//! - `MAX_CPUS` 是编译期上限（决赛 LA SMP=12，取 16 容纳扩展）；
//! - `current_cpu_id()` / `set_cpu_id()` 屏蔽 RISC-V `tp` 与 LoongArch
//!   CPUID CSR 差异；
//! - 内存页帧 per-CPU pageset、SLUB frozen-slab、调度 per-CPU rq 都只依赖
//!   本模块，不再各自实现一套 CPU-id 读取逻辑。
//!
//! 注意：per-CPU 数据必须由「本 hart 独占的锁」保护（本项目为
//! `IrqTicketSpinLock`，关本地中断 + FIFO ticket），而不是普通自旋锁：
//! 普通锁会被本地定时器/设备中断在同一 hart 上重入，形成自死锁。

/// 编译期支持的最大 hart 数。
pub const MAX_CPUS: usize = 16;

/// 读取当前 hart 的硬件 CPU ID。
///
/// 越界时回退 0：在极早期 boot（tp/CPUID 尚未建立）不允许把数组索引
/// 推成非法值。
#[inline]
pub fn current_cpu_id() -> usize {
    let id = crate::arch::asm_helpers::read_cpu_id();
    if id < MAX_CPUS {
        id
    } else {
        0
    }
}

/// 设置当前 hart 的 CPU ID（RISC-V 写入 tp；LoongArch 只读，忽略）。
#[inline]
pub fn set_cpu_id(cpu_id: usize) {
    crate::arch::asm_helpers::write_cpu_id(cpu_id);
}
