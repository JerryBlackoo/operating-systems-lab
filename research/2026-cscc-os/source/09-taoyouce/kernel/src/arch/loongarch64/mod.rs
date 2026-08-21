//! # LoongArch64 架构支持
//!
//! LoongArch64 architecture support for RustMicroOS microkernel.
//!
//! ## 来源说明 / Source Attribution
//!
//! 本模块的底层代码（CSR 操作宏、TLB 指令封装、中断向量入口汇编）
//! 参考自 PolyHAL 项目（https://github.com/Byte-OS/polyhal），
//! 遵循 MIT/Apache-2.0 双协议。
//!
//! Low-level code in this module (CSR operation macros, TLB instruction wrappers,
//! interrupt vector entry assembly) is adapted from the PolyHAL project
//! (https://github.com/Byte-OS/polyhal), licensed under MIT/Apache-2.0 dual license.
//!
//! ## 模块结构 / Module Structure
//!
//! ```text
//! loongarch64/
//! ├── mod.rs       — 模块入口，导出子模块，La64Hal 实现，rust_main 入口
//! ├── entry.S      — 启动入口汇编（🟡 基于 PolyHAL 适配）
//! ├── trap.S       — 异常/中断入口汇编（🟢 复用自 PolyHAL + 🟡 微内核适配）
//! ├── csr.rs       — CSR 操作宏和常量定义（🟢 复用自 PolyHAL）
//! ├── context.rs   — La64TrapContext 和 La64Context 数据结构
//! ├── trap.rs      — trap handler（Rust 部分）
//! ├── mm.rs        — 页表相关框架（La64PTE, La64PageTable）
//! └── timer.rs     — 定时器操作
//! ```
//!
//! ## 参考文档 / References
//!
//! - 《龙芯架构参考手册》卷一：基础架构
//! - PolyHAL: https://github.com/Byte-OS/polyhal
//! - NPUcore-IMPACT: LoongArch64 OS 实现参考
//! - LoongArch Linux 内核: `arch/loongarch/`

// ============================================================================
// 启动入口汇编 / Boot Entry Assembly
// ============================================================================
//
// 通过 global_asm! 引入 LoongArch64 启动入口汇编代码 (entry.S)
// 该汇编代码完成 CRMD 配置、栈设置、BSS 清零，然后跳转到 rust_main
//
// Include LoongArch64 boot entry assembly (entry.S) via global_asm!
// The assembly configures CRMD, sets up stack, clears BSS, then jumps to rust_main

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(include_str!("entry.S"));

// ============================================================================
// 异常/中断入口汇编 / Trap Entry Assembly
// ============================================================================
//
// 通过 global_asm! 引入 LoongArch64 异常/中断入口汇编代码 (trap.S)
// 该汇编代码完成 32 个通用寄存器 + ERA/PRMD/BADV 的保存/恢复，
// 调用 Rust la64_trap_handler 函数，然后通过 ertn 返回。
//
// 🟢 寄存器保存/恢复框架复用自 PolyHAL（MIT/Apache-2.0）
// 🟡 微内核栈切换和 Rust handler 调用为 RustMicroOS 适配
//
// Include LoongArch64 trap entry assembly (trap.S) via global_asm!

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(include_str!("trap.S"));

// ============================================================================
// 上下文切换汇编 / Context Switch Assembly
// ============================================================================
//
// 通过 global_asm! 引入 LoongArch64 内核上下文切换汇编代码 (switch.S)
// 该汇编代码实现 __la64_switch 函数，保存/恢复 callee-saved 寄存器
// （ra, sp, s0-s8, fp, tp），用于内核线程/进程切换。
//
// 🟢 callee-saved 寄存器保存/恢复框架复用自 PolyHAL（MIT/Apache-2.0）
// 🟡 La64Context 内存布局为 RustMicroOS 微内核适配
//
// Include LoongArch64 context switch assembly (switch.S) via global_asm!

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(include_str!("switch.S"));

// ============================================================================
// TLB 重填异常入口汇编 / TLB Refill Exception Entry Assembly
// ============================================================================
//
// 通过 global_asm! 引入 LoongArch64 TLB 重填异常入口汇编代码 (tlb_refill.S)
// 该汇编代码使用 LDDIR/LDPTE 硬件辅助指令快速遍历页表并填充 TLB。
//
// 🟢 LDDIR/LDPTE 指令框架参考自 PolyHAL（MIT/Apache-2.0）
// 🔴 软件回退路径和微内核适配为 RustMicroOS 自己编写
//
// Include LoongArch64 TLB refill exception entry assembly (tlb_refill.S) via global_asm!

#[cfg(target_arch = "loongarch64")]
core::arch::global_asm!(include_str!("tlb_refill.S"));

// ============================================================================
// 子模块声明 / Submodule Declarations
// ============================================================================

/// CSR 操作宏和常量定义
/// CSR operation macros and constant definitions
#[macro_use]
pub mod csr;

/// 陷阱上下文和内核上下文数据结构
/// Trap context and kernel context data structures
pub mod context;

/// 异常/中断处理
/// Exception/interrupt handling
pub mod trap;

/// 页表和内存管理框架
/// Page table and memory management framework
pub mod mm;

/// 定时器操作
/// Timer operations
pub mod timer;

/// 扩展 I/O 中断控制器（EIOINTC）驱动
/// Extended I/O Interrupt Controller (EIOINTC) driver
pub mod eiointc;
/// IOCSR access helpers shared by interrupt and SMP code.
pub mod iocsr;

/// 用户态浮点上下文管理 / Per-process FPU context management
pub mod fpu;

// ============================================================================
// 公开导出 / Public Re-exports
// ============================================================================

pub use context::{La64TrapContext, La64Context};
pub use context::La64TrapContext as TrapContext;
pub use mm::{La64PageTable, La64PTE};

// 导入 HAL trait 定义
// Import HAL trait definitions
use crate::arch::hal_trait::{ArchHal, TrapContextOps, ContextOps, PageTableOps, MmError};

// ============================================================================
// 平台相关常量 / Platform Constants
// ============================================================================

/// LoongArch64 平台相关常量
///
/// Platform-specific constants for LoongArch64.
pub mod consts {
    /// 内核起始地址（LoongArch64 QEMU virt 平台）
    ///
    /// QEMU 将内核加载到物理地址 0x0020_0000。
    /// 内核链接在 DMW0 窗口地址 0x9000_0000_0020_0000。
    /// entry.S 在 DA=1 模式下启动，配置 DMW 后切换到 PG=1，
    /// 跳转到高地址继续执行。
    pub const KERNEL_BASE: usize = 0x9000_0000_0020_0000;

    /// DMW0 窗口基址
    pub const DMW0_BASE: usize = 0x9000_0000_0000_0000;

    /// DMW1 窗口基址（强序非缓存，用于 MMIO）
    pub const DMW1_BASE: usize = 0x8000_0000_0000_0000;

    /// 帧元数据/引用计数数组的编译期物理地址上限。决赛 BuildStorm 判题为
    /// `-m 8G`（高 bank = [0x8000_0000, 0x2_7000_0000)），故上限放到 10 GiB PA；
    /// 运行时实际可用范围仍按 FDT /memory 实测 clamp（见 `mm::init`）。
    pub const MEMORY_END: usize = 0x2_8000_0000;
    /// QEMU LoongArch virt RAM is split into two discontiguous banks:
    ///   low  bank: [0, 0x1000_0000)           (256MB) holds kernel image + heap
    ///   high bank: [0x8000_0000, ...)         (-m 总量减 256MB，-m 1G 时到 0xb000_0000)
    pub const RAM_LOW_END: usize = 0x1000_0000;
    pub const RAM_HIGH_BASE: usize = 0x8000_0000;
    /// 高 bank 的编译期上限（= MEMORY_END）；FDT 探测失败时的保守回退是
    /// `RAM_HIGH_FALLBACK_END`（-m 1G 布局），绝不能凭上限捏造不存在的内存。
    pub const RAM_HIGH_END: usize = MEMORY_END;
    pub const RAM_HIGH_FALLBACK_END: usize = 0xb000_0000;

    /// 页大小
    /// Page size
    pub const PAGE_SIZE: usize = 4096;

    /// 页大小位数
    /// Page size bits
    pub const PAGE_SIZE_BITS: usize = 12;

    /// 时钟频率（100MHz，LoongArch64 QEMU virt）
    ///
    /// Clock frequency (100MHz for LoongArch64 QEMU virt).
    pub const CLOCK_FREQ: usize = 100_000_000;
}

// ============================================================================
// La64Hal — ArchHal trait 实现 / ArchHal Trait Implementation
// ============================================================================

/// LoongArch64 架构 HAL 实现
///
/// LoongArch64 architecture HAL implementation.
///
/// 实现 `ArchHal` trait，将 LoongArch64 的硬件操作统一到架构无关接口。
/// 所有方法均为零开销的内联汇编封装。
///
/// ## 与 RISC-V64 Rv64Hal 的对应关系
///
/// | 操作 | Rv64Hal | La64Hal |
/// |------|---------|---------|
/// | 使能中断 | csrs sstatus, SIE | csrxchg CRMD, IE |
/// | 关闭中断 | csrc sstatus, SIE | csrxchg CRMD, ~IE |
/// | 等待中断 | wfi | idle 0 |
/// | 刷新 TLB | sfence.vma | invtlb |
/// | 指令屏障 | fence.i | ibar 0 |
/// | 关机 | SBI shutdown | ACPI/平台特定 |
pub struct La64Hal;

/// 进入关机序列标志：GED/PM1A MMIO 写在 QEMU 10.0.2 下会间歇触发
/// AddressError（BSCRASH 多轮实测，ERA 都落在 shutdown 的 MMIO 写指令）。
/// trap 侧据此把“关机 MMIO 失败”降级为死循环 idle，而不是 KERNEL PANIC。
static SHUTDOWN_IN_PROGRESS: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

impl ArchHal for La64Hal {
    type TrapContext = La64TrapContext;
    type Context = La64Context;
    type PageTable = La64PageTable;

    // ========================================================================
    // 中断控制 / Interrupt Control
    // ========================================================================

    /// 开启全局中断
    ///
    /// Enable global interrupts by setting CRMD.IE bit.
    #[inline]
    fn enable_interrupts() {
        // 设置 CRMD.IE 位（bit 2）
        // Set CRMD.IE bit (bit 2)
        csrxchg!(csr::CRMD_IE, csr::CRMD_IE, "0x0");
    }

    /// 关闭全局中断
    ///
    /// Disable global interrupts by clearing CRMD.IE bit.
    #[inline]
    fn disable_interrupts() {
        // 清除 CRMD.IE 位（bit 2）
        // Clear CRMD.IE bit (bit 2)
        csrxchg!(0usize, csr::CRMD_IE, "0x0");
    }

    /// 查询全局中断是否开启
    ///
    /// Check if global interrupts are enabled by reading CRMD.IE.
    #[inline]
    fn is_interrupt_enabled() -> bool {
        // 读取 CRMD 寄存器，检查 IE 位
        // Read CRMD register, check IE bit
        let crmd = csrrd!("0x0");
        (crmd & csr::CRMD_IE) != 0
    }

    // ========================================================================
    // 计时器 / Timer
    // ========================================================================

    fn timer_init() {
        timer::init();
    }

    fn timer_set_next(ticks: u64) {
        timer::set_next(ticks);
    }

    #[inline]
    fn timer_get_time() -> u64 {
        timer::get_time()
    }

    #[inline]
    fn timer_freq() -> u64 {
        timer::freq()
    }

    // ========================================================================
    // TLB / 缓存管理 / TLB & Cache Management
    // ========================================================================

    /// 刷新 TLB
    ///
    /// Flush TLB entries.
    /// - `vaddr == None`: invtlb 0x0（全部刷新）
    /// - `vaddr == Some(addr)`: invtlb 0x6（按地址刷新全局条目）
    #[inline]
    fn flush_tlb(vaddr: Option<usize>) {
        match vaddr {
            // ASID-tagged TLB 下，按 VA 就地失效必须带当前地址空间的真实 ASID，
            // 否则 $zero=ASID0 会漏掉本 ASID 的条目。
            Some(va) => csr::invtlb_addr_asid(mm::get_asid() as usize, va),
            None => csr::invtlb_all(),
        }
    }

    /// 按 ASID 刷新 TLB（微内核关键操作）
    ///
    /// Flush TLB by ASID (critical for microkernel).
    #[inline]
    fn flush_tlb_asid(asid: u16) {
        csr::invtlb_asid(asid as usize);
    }

    /// 指令缓存同步屏障
    ///
    /// Instruction cache fence.
    /// LoongArch64 使用 `ibar 0` 指令（指令屏障）。
    #[inline]
    fn fence_i() {
        unsafe { core::arch::asm!("ibar 0"); }
    }

    // ========================================================================
    // 电源管理 / Power Management
    // ========================================================================

    /// 等待中断（低功耗等待）
    ///
    /// Wait for interrupt (low-power idle).
    /// LoongArch64 使用 `idle 0` 指令（对应 RISC-V 的 `wfi`）。
    #[inline]
    fn wfi() {
        unsafe { core::arch::asm!("idle 0"); }
    }

    /// 系统关机（LoongArch64）
    ///
    /// System shutdown for LoongArch64.
    ///
    /// 关机策略（按优先级尝试）/ Shutdown strategy (in priority order):
    /// 1. QEMU virt 平台 ACPI PM1a_CNT 寄存器（写入 SLP_TYP=5 | SLP_EN）
    /// 2. QEMU virt 平台 ACPI 通用关机（GED/GPIO 方式）
    /// 3. 死循环（最后的 fallback）
    ///
    /// 配合 QEMU `-no-reboot` 参数使用时，关机后 QEMU 进程会退出而非重启。
    /// When used with QEMU `-no-reboot` flag, QEMU process exits instead of rebooting.
    ///
    /// ## LoongArch64 QEMU virt 平台关机方式
    ///
    /// LoongArch64 QEMU virt 平台使用 ACPI 关机机制：
    /// - PM1a_CNT 寄存器地址: 通过 ACPI FADT 表获取，QEMU virt 默认为 0x10080004
    /// - 写入值: SLP_TYP=5 (S5 关机状态) << 10 | SLP_EN (bit 13)
    ///
    /// 备选方案：直接写入 QEMU 的 ACPI 关机端口。
    /// QEMU LoongArch virt 使用 LS7A 桥片，ACPI 基地址为 0x800000EFFF
    /// 但更简单的方式是使用 QEMU 的通用 ACPI PM 控制器。
    ///
    /// ## 参考 / References
    /// - NPUcore-IMPACT: LoongArch64 关机实现
    /// - QEMU 源码: `hw/loongarch/virt.c`
    /// - LoongArch Linux 内核: `arch/loongarch/kernel/reset.c`
    fn shutdown(_code: u32) -> ! {
        SHUTDOWN_IN_PROGRESS.store(true, core::sync::atomic::Ordering::Release);

        // 方案 1: GED sleep control（0x100e001c）优先——实测多数运行可正常
        // 关闭 QEMU；偶发 AddressError 由 trap 侧跳过当前 MMIO 写后继续
        // 执行下一方案，不再 panic。
        const QEMU_LA_GED_REG_BASE: usize = 0x8000_0000_100e_001c;
        const GED_SLEEP_CONTROL_OFFSET: usize = 0x00;
        const SLP_TYP_S5: u8 = 5 << 2;
        const SLP_EN: u8 = 1 << 5;
        let sleep_control_addr = QEMU_LA_GED_REG_BASE + GED_SLEEP_CONTROL_OFFSET;
        let sleep_value: u8 = SLP_TYP_S5 | SLP_EN;
        unsafe {
            core::arch::asm!(
                "st.b {val}, {addr}, 0",
                val = in(reg) sleep_value as usize,
                addr = in(reg) sleep_control_addr,
                options(nostack, preserves_flags)
            );
        }

        // 方案 2: ACPI PM1a_CNT（0x100d0014，DMW1 窗口）
        const QEMU_LA_PM1A_CNT: usize = 0x8000_0000_100d_0014;
        const PM1A_SLP_TYP_S5: u16 = 5 << 10;
        const PM1A_SLP_EN: u16 = 1 << 13;
        let pm1a_value: u16 = PM1A_SLP_TYP_S5 | PM1A_SLP_EN;
        unsafe {
            core::arch::asm!(
                "st.h {val}, {addr}, 0",
                val = in(reg) pm1a_value as usize,
                addr = in(reg) QEMU_LA_PM1A_CNT,
                options(nostack, preserves_flags)
            );
        }

        // 方案 3: 死循环（最后的 fallback）
        loop {
            unsafe { core::arch::asm!("idle 0"); }
        }
    }

    // ========================================================================
    // 平台信息 / Platform Information
    // ========================================================================

    /// 获取当前 CPU 编号
    ///
    /// Get current CPU ID from CSR.CPUID.
    #[inline]
    fn hart_id() -> usize {
        csrrd!("0x20")
    }

    #[inline]
    fn page_size() -> usize {
        consts::PAGE_SIZE
    }

    fn max_physical_address_bits() -> usize {
        48 // LoongArch64 支持 48 位物理地址
    }

    fn max_virtual_address_bits() -> usize {
        48 // LoongArch64 支持 48 位虚拟地址
    }

    fn arch_name() -> &'static str {
        "loongarch64"
    }
}

// ============================================================================
// LoongArch64 架构特定函数（条件编译导出）
// ============================================================================

/// 等待中断（低功耗等待）
///
/// LoongArch64: `idle 0` 指令
#[inline]
pub fn wfi() {
    unsafe { core::arch::asm!("idle 0"); }
}

/// 关闭中断
///
/// LoongArch64: 清除 CRMD.IE 位
#[inline]
pub fn disable_interrupts() {
    csrxchg!(0usize, csr::CRMD_IE, "0x0");
}

/// 开启中断
///
/// LoongArch64: 设置 CRMD.IE 位
#[inline]
pub fn enable_interrupts() {
    csrxchg!(csr::CRMD_IE, csr::CRMD_IE, "0x0");
}

/// 获取当前 CPU 编号
///
/// LoongArch64: 读取 CSR.CPUID
#[inline]
pub fn hartid() -> usize {
    csrrd!("0x20")
}

/// 刷新 TLB（全局）
///
/// LoongArch64: `invtlb 0x0, $zero, $zero`
#[inline]
pub fn sfence_vma() {
    csr::invtlb_all();
}

/// 指令缓存同步屏障
///
/// LoongArch64: `ibar 0`
#[inline]
pub fn fence_i() {
    unsafe { core::arch::asm!("ibar 0"); }
}

// ============================================================================
// LoongArch64 Rust 入口函数 / LoongArch64 Rust Entry Point
// ============================================================================
//
// 🟡 微内核适配：LoongArch64 的 Rust 入口函数
// 🟡 Microkernel adaptation: LoongArch64 Rust entry function
//
// 由 entry.S 中的 _start 汇编代码在完成 CRMD 配置、栈设置、BSS 清零后调用。
// LoongArch64 QEMU virt 平台不像 RISC-V 有 OpenSBI 传递 hartid/dtb 参数，
// 固件直接跳转到 _start，无参数传递。
//
// Called by _start assembly in entry.S after CRMD configuration, stack setup,
// and BSS zeroing. Unlike RISC-V where OpenSBI passes hartid/dtb, LoongArch64
// QEMU virt firmware jumps directly to _start with no parameters.

/// LoongArch64 内核 Rust 入口
///
/// LoongArch64 kernel Rust entry point.
///
/// # 调用链 / Call Chain
///
/// ```text
/// QEMU 固件 → _start (entry.S)
///   → 配置 CRMD (PLV=0, IE=0, DA=1, PG=0)
///   → 使能 FPU (EUEN.FPE=1)
///   → 设置栈指针 (boot_stack_top)
///   → 清零 BSS 段
///   → rust_main (本函数)
///     → 初始化控制台 (UART)
///     → 初始化 trap 处理
///     → 输出 "Hello from LoongArch64"
///     → 后续初始化...
/// ```
#[no_mangle]
pub extern "C" fn rust_main(_efi_boot: usize, cmdline: usize, system_table: usize) -> ! {
    // 直接写 UART 测试（绕过 console 模块）
    // entry.S 已经切换到 PG=1 模式，UART 需要通过 DMW1 窗口访问
    // DMW1: VSEG=0x8000, MAT=SUC, PLV0=1
    // UART 物理地址 0x1FE001E0 → DMW1 窗口地址 0x8000_0000_1FE0_01E0
    unsafe {
        let uart = 0x8000_0000_1FE0_01E0usize as *mut u8;
        let msg = b"LA64 rust_main entered\r\n";
        for &b in msg {
            // 等待 UART 就绪
            let lsr = (0x8000_0000_1FE0_01E0usize + 5) as *const u8;
            while lsr.read_volatile() & 0x20 == 0 {}
            uart.write_volatile(b);
        }
    }

    // 初始化 console
    crate::console::init();
    crate::println!("[LA64] Console initialized, calling kernel_main...");

    let dtb = find_boot_dtb(system_table).unwrap_or(0);
    crate::kernel_main_with_cmdline(0, dtb, Some(cmdline))
}

const DMW0_BASE: usize = 0x9000_0000_0000_0000;
const QEMU_FDT_BASE: usize = 0x100000;
const FDT_MAGIC: u32 = 0xd00d_feed;
const DEVICE_TREE_GUID: [u8; 16] = [
    0xd5, 0x21, 0xb6, 0xb1, 0x9c, 0xf1, 0xa5, 0x41,
    0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0,
];

#[repr(C)]
struct EfiGuid {
    bytes: [u8; 16],
}

#[repr(C)]
struct EfiSystemTable {
    _header: [u8; 24],
    _fw_vendor: u64,
    _fw_revision: u32,
    _con_in_handle: u64,
    _con_in: u64,
    _con_out_handle: u64,
    _con_out: u64,
    _stderr_handle: u64,
    _stderr_placeholder: u64,
    _runtime: u64,
    _boottime: u64,
    nr_tables: u64,
    tables: u64,
}

#[repr(C)]
struct EfiConfigTable {
    guid: EfiGuid,
    table: u64,
}

#[inline]
fn phys_to_virt(addr: usize) -> usize {
    if addr >= 0x8000_0000_0000_0000 {
        addr
    } else {
        DMW0_BASE | addr
    }
}

fn valid_fdt(addr: usize) -> Option<usize> {
    let addr = phys_to_virt(addr);
    let magic = unsafe { u32::from_be(core::ptr::read_unaligned(addr as *const u32)) };
    (magic == FDT_MAGIC).then_some(addr)
}

fn find_boot_dtb(system_table: usize) -> Option<usize> {
    if system_table != 0 {
        let table_addr = phys_to_virt(system_table);
        let table = unsafe { &*(table_addr as *const EfiSystemTable) };
        if table.tables != 0 && table.nr_tables <= 64 {
            let tables_addr = phys_to_virt(table.tables as usize);
            for index in 0..table.nr_tables as usize {
                let entry = unsafe {
                    &*((tables_addr + index * core::mem::size_of::<EfiConfigTable>())
                        as *const EfiConfigTable)
                };
                if entry.guid.bytes == DEVICE_TREE_GUID {
                    if let Some(dtb) = valid_fdt(entry.table as usize) {
                        crate::println!("[cmdline] EFI DTB found at {:#x}", dtb);
                        return Some(dtb);
                    }
                }
            }
        }
    }
    if let Some(dtb) = valid_fdt(QEMU_FDT_BASE) {
        crate::println!("[cmdline] QEMU DTB fallback at {:#x}", dtb);
        return Some(dtb);
    }
    crate::println!("[cmdline] No valid LoongArch DTB found; using single CPU");
    None
}
