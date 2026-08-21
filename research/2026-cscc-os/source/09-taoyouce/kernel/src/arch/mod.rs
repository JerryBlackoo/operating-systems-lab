//! 架构相关代码
//!
//! 支持 RISC-V 64-bit 和 LoongArch64 双架构。
//! 上层代码通过本模块的统一导出使用架构无关接口。

// ============================================================================
// 架构无关的内联汇编辅助函数
// ============================================================================
pub mod asm_helpers;

// ============================================================================
// HAL trait 定义（架构无关）
// ============================================================================
pub mod hal_trait;

// ============================================================================
// 架构子模块（条件编译）
// ============================================================================

#[cfg(target_arch = "riscv64")]
pub mod riscv64;

#[cfg(target_arch = "loongarch64")]
pub mod loongarch64;

// ============================================================================
// 统一常量导出
// ============================================================================

/// 架构无关的平台常量
///
/// 根据目标架构条件编译，重新导出对应架构的常量模块。
pub mod consts {
    #[cfg(target_arch = "riscv64")]
    pub use super::riscv64::consts::*;

    #[cfg(target_arch = "loongarch64")]
    pub use super::loongarch64::consts::*;
}

// ============================================================================
// 统一类型导出
// ============================================================================

/// 统一导出 TrapContext 类型
#[cfg(target_arch = "riscv64")]
pub use riscv64::trap_context::TrapContext;

#[cfg(target_arch = "loongarch64")]
pub use loongarch64::La64TrapContext as TrapContext;

/// 统一导出 Context（内核上下文切换）类型
#[cfg(target_arch = "riscv64")]
pub use riscv64::context::Context;

#[cfg(target_arch = "loongarch64")]
pub use loongarch64::La64Context as Context;

// ============================================================================
// 统一导出 trap 相关函数
// ============================================================================

/// 用户态返回函数
#[cfg(target_arch = "riscv64")]
pub use riscv64::trap::usertrapret;

#[cfg(target_arch = "loongarch64")]
pub use loongarch64::trap::usertrapret;

/// 按当前最近定时睡眠到期点立即重新 arm 硬件定时器（hrtimer 精确到期）。
#[cfg(target_arch = "riscv64")]
pub use riscv64::trap::rearm_timer;

#[cfg(target_arch = "loongarch64")]
pub use loongarch64::trap::rearm_timer;

/// trap 处理入口（Rust 部分）
#[cfg(target_arch = "riscv64")]
pub use riscv64::trap::trap_handler_rust;

#[cfg(target_arch = "loongarch64")]
pub use loongarch64::trap::trap_handler_rust;

// ============================================================================
// 统一导出 HAL 实现类型
// ============================================================================

#[cfg(target_arch = "riscv64")]
#[allow(unused_imports)]
pub use riscv64::*;

#[cfg(target_arch = "loongarch64")]
#[allow(unused_imports)]
pub use loongarch64::*;

/// 架构 HAL 实现类型别名
///
/// RISC-V64 暂未实现 ArchHal trait，HalImpl 仅在 LoongArch64 下可用。
/// 上层代码使用 HalImpl 时需要 cfg 保护或确保 riscv64 下不会被编译到。
#[cfg(target_arch = "loongarch64")]
pub type HalImpl = loongarch64::La64Hal;

// ============================================================================
// 架构无关的便利函数（条件编译实现）
// ============================================================================

/// 每 hart 的 idle 栈大小。
/// 从 64KB 提到 128KB：RV SMP=8 启动期已实测 boot 栈被 ap_online 深帧 +
/// 嵌套 timer trap 压穿 64KB 边界，idle 路径同阶帧深，留同样余量。
const IDLE_STACK_SIZE: usize = 128 * 1024;

/// per-CPU idle 栈存储区。`.bss`，16 字节对齐满足 RISC-V/LoongArch ABI。
#[repr(align(16))]
struct IdleStacks([[u8; IDLE_STACK_SIZE]; crate::sched::smp::MAX_CPUS]);

static mut IDLE_STACKS: IdleStacks =
    IdleStacks([[0u8; IDLE_STACK_SIZE]; crate::sched::smp::MAX_CPUS]);

/// 取本 hart 的 idle 栈栈顶（栈向下增长）。
#[inline]
fn idle_stack_top(hart: usize) -> usize {
    let base = unsafe { core::ptr::addr_of!(IDLE_STACKS.0[hart]) } as usize;
    base + IDLE_STACK_SIZE
}

/// 切到**本 hart 专属的 idle 栈**后运行 `f`（永不返回）。
///
/// 对标 Linux：每个 CPU 有自己的 idle task（`init_task` 的 per-CPU 副本），
/// 调度器循环跑在该 CPU 私有的内核栈上。本内核此前让 idle+调度循环留在
/// "恰好当前所在"的栈上：一个 hart 从某进程的内核路径进入 idle 后，会继续
/// 使用**那个进程的内核栈**；同一时刻另一个 hart 又选中该进程并在 trap 时
/// 使用同一份栈 → 两个 hart 互踩同一块栈内存。
///
/// 取证（`rv-smpgdb-20260811T030257Z.gdb.txt` + `symbolize.sh`）：故障指令是
/// `kernel_idle_until_runnable` 里 `ld a0,-192(s0)` 取出的指针被 `lb` 解引用，
/// `stval=0x517cc1b727220601`（垃圾），而当时 `sp=0x88a2e680` 正落在 init 的
/// 内核栈 `ksp=0x88a30000` 区间内。
///
/// idle 栈是**独立的 per-CPU 静态区**，不复用 `boot_stack`。
///
/// 血泪：最初直接把 sp 设成 `boot_stack_top - hart*64KB`，但那正是该 hart
/// **当前正在使用**的启动栈——把 sp 拉回槽顶后，新压入的帧会覆盖仍处于活动状态的
/// 外层帧（含中断返回所需信息），随后内核就取指到地址 0：
/// `[trap] Unhandled trap: InstructionAccessFault, sepc=0x0` + `KERNEL PANIC`。
/// 独立区域从结构上排除了这种自我覆盖。对标 Linux：每个 CPU 的 idle task 有自己
/// 独立的内核栈，不与启动栈共用。
///
/// # Safety
/// 调用后原栈帧不再使用（`f` 永不返回），调用点自身也必须是 `-> !`。
#[inline(never)]
pub unsafe fn run_on_hart_idle_stack(f: extern "C" fn() -> !) -> ! {
    let hart = crate::arch::asm_helpers::read_cpu_id().min(crate::sched::smp::MAX_CPUS - 1);
    let sp = idle_stack_top(hart);
    #[cfg(target_arch = "riscv64")]
    core::arch::asm!(
        "mv sp, {sp}",
        "jalr {f}",
        sp = in(reg) sp,
        f = in(reg) f,
        options(noreturn)
    );
    // LA：用 `jirl $r0`（丢弃返回地址，`f` 永不返回）。写 `$ra` 会抬高 $r1 的
    // 寄存器压力，导致同 CGU 内 `csrxchg` 的 mask 操作数被分配到 `$ra`，而
    // LoongArch 规定该位置不得是 `$r0/$r1`（编译期报 `must not be $r0 or $r1`）。
    #[cfg(target_arch = "loongarch64")]
    core::arch::asm!(
        "move $sp, {sp}",
        "jirl $r0, {f}, 0",
        sp = in(reg) sp,
        f = in(reg) f,
        options(noreturn)
    );
}

/// 等待中断（低功耗等待）
#[inline]
pub fn wfi() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("wfi");
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        core::arch::asm!("idle 0");
    }
}

/// 查询中断是否开启
#[inline]
pub fn is_interrupt_enabled() -> bool {
    #[cfg(target_arch = "riscv64")]
    {
        let sstatus: usize;
        unsafe { core::arch::asm!("csrr {}, sstatus", out(reg) sstatus); }
        (sstatus & (1usize << 1)) != 0
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let crmd = asm_helpers::read_sstatus();
        (crmd & (1usize << 2)) != 0 // CRMD_IE = bit 2
    }
}

/// 关闭中断
#[inline]
pub fn disable_interrupts() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("csrc sstatus, {}", in(reg) 1usize << 1);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        loongarch64::disable_interrupts();
    }
}

/// 开启中断
#[inline]
pub fn enable_interrupts() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("csrs sstatus, {}", in(reg) 1usize << 1);
    }
    #[cfg(target_arch = "loongarch64")]
    {
        loongarch64::enable_interrupts();
    }
}

/// 获取当前 hart/CPU ID
#[inline]
pub fn hartid() -> usize {
    #[cfg(target_arch = "riscv64")]
    {
        let id: usize;
        unsafe {
            core::arch::asm!("mv {}, tp", out(reg) id);
        }
        id
    }
    #[cfg(target_arch = "loongarch64")]
    {
        loongarch64::hartid()
    }
}

/// 刷新 TLB
#[inline]
pub fn sfence_vma() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("sfence.vma");
    }
    #[cfg(target_arch = "loongarch64")]
    {
        loongarch64::sfence_vma();
    }
}

/// 刷新指令缓存
#[inline]
pub fn fence_i() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("fence.i");
    }
    #[cfg(target_arch = "loongarch64")]
    {
        loongarch64::fence_i();
    }
}

/// 架构相关自测试
pub fn self_test() {
    crate::println!("[arch] Running self-test...");

    // 测试 hart ID 获取
    let hart = hartid();
    crate::println!("[arch] Current hart ID: {}", hart);

    // 测试中断控制
    disable_interrupts();
    enable_interrupts();

    // 测试 TLB 刷新
    sfence_vma();

    // 测试指令缓存刷新
    fence_i();

    // 测试架构常量
    #[cfg(target_arch = "riscv64")]
    {
        use riscv64::consts::{KERNEL_BASE, CLOCK_FREQ, PAGE_SIZE};
        crate::println!("[arch] KERNEL_BASE: {:#x}", KERNEL_BASE);
        crate::println!("[arch] CLOCK_FREQ: {} Hz", CLOCK_FREQ);
        crate::println!("[arch] PAGE_SIZE: {} bytes", PAGE_SIZE);

        // 测试上下文结构
        let ctx = riscv64::context::Context::init(0x8000_0000, 0x8001_0000, 0);
        crate::println!("[arch] Context initialized: entry={:#x}, sp={:#x}", ctx.ra, ctx.sp);

        // 测试零上下文
        let mut zero_ctx = riscv64::context::Context::zero();
        zero_ctx.set_return_value(42);
    }

    #[cfg(target_arch = "loongarch64")]
    {
        use loongarch64::consts::{KERNEL_BASE, CLOCK_FREQ, PAGE_SIZE};
        crate::println!("[arch] KERNEL_BASE: {:#x}", KERNEL_BASE);
        crate::println!("[arch] CLOCK_FREQ: {} Hz", CLOCK_FREQ);
        crate::println!("[arch] PAGE_SIZE: {} bytes", PAGE_SIZE);
    }

    crate::println!("[arch] Self-test passed!");
}
