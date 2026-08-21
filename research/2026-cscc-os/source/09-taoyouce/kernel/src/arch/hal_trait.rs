//! # 硬件抽象层 (HAL) Trait 定义
//!
//! Hardware Abstraction Layer trait definitions for RustMicroOS microkernel.
//!
//! 本模块定义了 RustMicroOS 微内核的架构无关硬件抽象接口，
//! 对标 seL4 的 `arch/` 目录组织和 Asterinas 的 OSTD 框架设计理念。
//!
//! ## 设计理念 / Design Philosophy
//!
//! - **微内核专用**：与宏内核 HAL（如 PolyHAL/axhal）不同，本 trait 体系
//!   包含微内核特有的能力授权映射（`map_with_cap`）、ASID 精确 TLB 刷新
//!   （`flush_tlb_asid`）、IPC 快速路径切换（`ipc_switch`）等接口。
//! - **零开销抽象**：所有 trait 方法设计为可内联的薄封装，编译器可完全消除
//!   虚函数调用开销（通过关联类型 + 单态化实现）。
//! - **双架构支持**：统一接口同时覆盖 RISC-V64（Sv39 + SBI）和
//!   LoongArch64（软件 TLB + EIOINTC），通过条件编译选择具体实现。
//!
//! ## 对标参考 / References
//!
//! - **seL4**: `arch/arm/`, `arch/riscv/`, `arch/x86/` 目录下的架构抽象
//! - **Asterinas OSTD**: `ostd::arch` 模块的 trait 接口设计
//! - **PolyHAL**: `polyhal::TrapFrame`, `polyhal::PageTable` 等底层抽象
//! - **starry-next/axhal**: `axhal::arch` 的 `ArchHal` trait 抽象方式
//!
//! ## 模块结构 / Module Structure
//!
//! ```text
//! hal_trait.rs
//! ├── ArchHal          — 架构主 trait（关联类型 + 平台操作）
//! ├── TrapContextOps   — 陷阱上下文操作 trait
//! ├── ContextOps       — 内核上下文切换 trait
//! ├── PageTableOps     — 页表操作 trait（含微内核能力授权）
//! ├── MmError          — 内存管理错误类型
//! └── IpcMessage       — IPC 消息（轻量级，用于快速路径切换）
//! ```

use core::fmt;

// ============================================================================
// 错误类型 / Error Types
// ============================================================================

/// 内存管理错误类型
///
/// Memory management error type for page table operations.
/// 对标 seL4 的 `seL4_Error` 中与内存相关的错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmError {
    /// 物理页帧已被映射 / Physical frame already mapped
    AlreadyMapped,
    /// 虚拟页未映射 / Virtual page not mapped
    NotMapped,
    /// 权限不足（能力检查失败）/ Insufficient capability rights
    CapabilityDenied,
    /// 物理内存不足 / Out of physical memory
    OutOfMemory,
    /// 无效的虚拟地址 / Invalid virtual address
    InvalidVaddr,
    /// 无效的物理地址 / Invalid physical address
    InvalidPaddr,
    /// 无效的页表标志 / Invalid page table flags
    InvalidFlags,
    /// 页表层级错误 / Page table level error
    LevelError,
}

impl fmt::Display for MmError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MmError::AlreadyMapped => write!(f, "physical frame already mapped"),
            MmError::NotMapped => write!(f, "virtual page not mapped"),
            MmError::CapabilityDenied => write!(f, "insufficient capability rights"),
            MmError::OutOfMemory => write!(f, "out of physical memory"),
            MmError::InvalidVaddr => write!(f, "invalid virtual address"),
            MmError::InvalidPaddr => write!(f, "invalid physical address"),
            MmError::InvalidFlags => write!(f, "invalid page table flags"),
            MmError::LevelError => write!(f, "page table level error"),
        }
    }
}

// ============================================================================
// IPC 快速路径消息 / IPC Fast-Path Message
// ============================================================================

/// IPC 快速路径消息
///
/// Lightweight IPC message for fast-path context switch.
/// 对标 seL4 的 `seL4_MessageInfo_t`，仅包含寄存器级别的消息传递数据，
/// 用于 `ContextOps::ipc_switch` 在上下文切换时直接传递少量数据。
///
/// ## 与 `crate::ipc::Message` 的区别
///
/// - `IpcMessage`：轻量级，仅用于 HAL 层的快速路径切换，4 个寄存器
/// - `crate::ipc::Message`：完整 IPC 消息，包含标签、徽章等元数据
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct IpcMessage {
    /// 消息标签（包含消息长度和操作类型）
    /// Message label (contains length and operation type)
    pub label: usize,
    /// 消息寄存器（最多 4 个机器字，对标 seL4 的 MR0-MR3）
    /// Message registers (up to 4 machine words, aligned with seL4 MR0-MR3)
    pub regs: [usize; 4],
}

impl IpcMessage {
    /// 创建空消息 / Create an empty IPC message
    pub const fn empty() -> Self {
        Self {
            label: 0,
            regs: [0; 4],
        }
    }

    /// 创建带标签的消息 / Create a labeled IPC message
    pub const fn new(label: usize, r0: usize, r1: usize, r2: usize, r3: usize) -> Self {
        Self {
            label,
            regs: [r0, r1, r2, r3],
        }
    }
}

// ============================================================================
// 陷阱上下文操作 Trait / Trap Context Operations Trait
// ============================================================================

/// 陷阱上下文操作 trait
///
/// Trap context operations for saving/restoring user-mode register state.
///
/// 定义了用户态陷入内核态时的寄存器上下文操作接口。
/// 每个架构需要实现自己的 `TrapContext` 结构体并实现此 trait。
///
/// ## 架构差异 / Architecture Differences
///
/// | 操作 | RISC-V64 | LoongArch64 |
/// |------|----------|-------------|
/// | 系统调用号 | `x[17]` (a7) | `regs[11]` ($a7=$r11) |
/// | 系统调用参数 | `x[10..16]` (a0-a5) | `regs[4..10]` ($a0-$a5=$r4-$r9) |
/// | 返回值 | `x[10]` (a0) | `regs[4]` ($a0=$r4) |
/// | 程序计数器 | `sepc` | `era` |
/// | 栈指针 | `x[2]` (sp) | `regs[3]` ($sp=$r3) |
///
/// ## 对标参考
///
/// - seL4: `arch/riscv/object/tcb.c` 中的 `setRegister`/`getRegister`
/// - Asterinas OSTD: `ostd::arch::TrapFrame`
/// - PolyHAL: `polyhal::TrapFrame`
pub trait TrapContextOps: Sized + Clone + Copy + fmt::Debug {
    /// 创建零初始化的陷阱上下文
    ///
    /// Create a zero-initialized trap context.
    /// 所有寄存器和 CSR 字段均为 0。
    fn zero() -> Self;

    /// 获取系统调用号
    ///
    /// Get the syscall number from the trap context.
    /// - RISC-V64: 从 `a7` (x17) 寄存器读取
    /// - LoongArch64: 从 `$a7` ($r11) 寄存器读取
    fn syscall_num(&self) -> usize;

    /// 获取系统调用参数（6 个）
    ///
    /// Get syscall arguments (6 registers).
    /// - RISC-V64: `a0-a5` (x10-x15)
    /// - LoongArch64: `$a0-$a5` ($r4-$r9)
    fn syscall_args(&self) -> [usize; 6];

    /// 设置系统调用返回值
    ///
    /// Set the syscall return value.
    /// - RISC-V64: 写入 `a0` (x10)
    /// - LoongArch64: 写入 `$a0` ($r4)
    fn set_return_value(&mut self, v: usize);

    /// 设置程序计数器
    ///
    /// Set the program counter (next instruction to execute on trap return).
    /// - RISC-V64: 写入 `sepc`
    /// - LoongArch64: 写入 `era`
    fn set_pc(&mut self, pc: usize);

    /// 获取程序计数器
    ///
    /// Get the program counter (instruction that caused the trap).
    fn get_pc(&self) -> usize;

    /// 设置栈指针
    ///
    /// Set the stack pointer.
    /// - RISC-V64: 写入 `sp` (x2)
    /// - LoongArch64: 写入 `$sp` ($r3)
    fn set_sp(&mut self, sp: usize);

    /// 获取栈指针
    ///
    /// Get the stack pointer.
    fn get_sp(&self) -> usize;

    /// 设置参数寄存器（用于 execve 等场景传递参数）
    ///
    /// Set argument register (used for passing arguments in execve, etc.).
    /// - `idx`: 参数索引 0-5，对应 a0-a5
    /// - `val`: 参数值
    fn set_arg(&mut self, idx: usize, val: usize);

    /// 获取参数寄存器
    ///
    /// Get argument register value.
    fn get_arg(&self, idx: usize) -> usize;

    /// 创建用户态初始上下文
    ///
    /// Create initial trap context for launching a user-mode program.
    /// 设置入口地址、用户栈、内核页表 token、内核栈等。
    ///
    /// # 参数 / Parameters
    /// - `entry`: 用户程序入口地址 / User program entry point
    /// - `user_sp`: 用户栈顶地址 / User stack top
    /// - `kernel_satp`: 内核页表 token / Kernel page table token (satp/PGDL)
    /// - `kernel_sp`: 内核栈顶地址 / Kernel stack top
    /// - `trap_handler`: 陷阱处理函数地址 / Trap handler entry
    fn app_init_context(
        entry: usize,
        user_sp: usize,
        kernel_satp: usize,
        kernel_sp: usize,
        trap_handler: usize,
    ) -> Self;
}


// ============================================================================
// 内核上下文切换 Trait / Kernel Context Switch Trait
// ============================================================================

/// 内核上下文切换操作 trait
///
/// Kernel context (callee-saved registers) operations for thread/process switching.
///
/// 定义了内核态线程切换时的寄存器保存/恢复接口。
/// 仅保存 callee-saved 寄存器（被调用者保存寄存器），
/// caller-saved 寄存器由调用约定自动处理。
///
/// ## 架构差异 / Architecture Differences
///
/// | 寄存器类别 | RISC-V64 | LoongArch64 |
/// |-----------|----------|-------------|
/// | 返回地址 | `ra` (x1) | `$ra` ($r1) |
/// | 栈指针 | `sp` (x2) | `$sp` ($r3) |
/// | 帧指针 | `s0/fp` (x8) | `$fp` ($r22) |
/// | 线程指针 | `tp` (x4) | `$tp` ($r2) |
/// | callee-saved | `s0-s11` (x8-x9, x18-x27) | `$s0-$s8` ($r23-$r31) |
///
/// ## 对标参考
///
/// - seL4: `arch/riscv/object/tcb.c` 中的 `switchToThread`
/// - Asterinas OSTD: `ostd::task::TaskContext`
/// - PolyHAL: `polyhal::Context`
pub trait ContextOps: Sized + Clone + Copy + fmt::Debug {
    /// 创建零初始化的内核上下文
    ///
    /// Create a zero-initialized kernel context.
    fn zero() -> Self;

    /// 创建新线程/进程的初始内核上下文
    ///
    /// Create initial kernel context for a new thread/process.
    ///
    /// # 参数 / Parameters
    /// - `entry`: 线程入口函数地址（将被设置为 `ra`，`ret` 时跳转到此处）
    /// - `kstack_top`: 内核栈顶地址
    /// - `satp`: 页表 token（RISC-V64 的 satp 或 LoongArch64 的 PGDL 值）
    fn init(entry: usize, kstack_top: usize, satp: usize) -> Self;

    /// 标准线程/进程切换
    ///
    /// Standard context switch between two kernel threads.
    /// 保存 `current` 的 callee-saved 寄存器，恢复 `next` 的寄存器。
    ///
    /// # Safety
    ///
    /// 调用者必须确保 `current` 和 `next` 指向有效的上下文结构。
    /// Caller must ensure both `current` and `next` point to valid context structs.
    fn switch(current: &mut Self, next: &Self);

    /// 微内核 IPC 快速路径切换
    ///
    /// Microkernel IPC fast-path context switch.
    /// 在切换上下文的同时传递 IPC 消息，跳过调度器，
    /// 直接从发送方切换到接收方。
    ///
    /// 对标 seL4 的 fastpath_call / fastpath_reply_recv，
    /// 这是微内核 IPC 性能的关键优化路径。
    ///
    /// # 参数 / Parameters
    /// - `current`: 当前（发送方）上下文
    /// - `next`: 目标（接收方）上下文
    /// - `msg`: 要传递的 IPC 消息
    ///
    /// # 默认实现 / Default Implementation
    ///
    /// 默认实现退化为普通 switch（忽略消息），
    /// 架构特定实现可以优化为在寄存器中直接传递消息。
    fn ipc_switch(current: &mut Self, next: &Self, _msg: &IpcMessage) {
        Self::switch(current, next);
    }
}

// ============================================================================
// 页表操作 Trait / Page Table Operations Trait
// ============================================================================

/// 页表操作 trait
///
/// Page table operations for virtual memory management.
///
/// 定义了微内核的页表管理接口，与宏内核的关键区别在于：
/// 1. `map_with_cap`：映射时需要验证能力授权（seL4 风格）
/// 2. `flush_asid`：按 ASID 精确刷新 TLB（避免全局 flush 的性能损失）
///
/// ## 架构差异 / Architecture Differences
///
/// | 特性 | RISC-V64 (Sv39) | LoongArch64 |
/// |------|----------------|-------------|
/// | 页表级数 | 3 级 (Sv39) | 3-4 级 |
/// | 页大小 | 4KB | 4KB-16KB (可配置) |
/// | TLB 管理 | 硬件 page walk | 软件 TLB 重填 |
/// | ASID 位数 | 16 位 (satp) | 10 位 (ASID CSR) |
/// | 页表 token | satp 寄存器 | PGDL/PGDH CSR |
///
/// ## 对标参考
///
/// - seL4: `arch/riscv/object/objecttype.c` 中的 `performPageInvocation`
/// - Asterinas OSTD: `ostd::mm::PageTable`
/// - PolyHAL: `polyhal::PageTable`
pub trait PageTableOps: Sized {
    /// 创建新的空页表
    ///
    /// Create a new empty page table with a freshly allocated root page.
    fn new() -> Self;

    /// 从页表 token 恢复页表结构
    ///
    /// Reconstruct a page table from its token (satp value or PGDL value).
    /// 用于从进程控制块中恢复页表引用。
    fn from_token(token: usize) -> Self;

    /// 获取页表 token
    ///
    /// Get the page table token for loading into hardware register.
    /// - RISC-V64: 返回 satp 寄存器值（MODE | ASID | PPN）
    /// - LoongArch64: 返回 PGDL 寄存器值
    fn token(&self) -> usize;

    /// 带能力授权的页面映射（微内核特有）
    ///
    /// Map a virtual page to a physical frame with capability authorization.
    /// This is the microkernel-specific mapping interface, inspired by seL4's
    /// `seL4_RISCV_Page_Map` which requires an Untyped capability to authorize
    /// physical memory access.
    ///
    /// # 参数 / Parameters
    /// - `vpn`: 虚拟页号 / Virtual Page Number
    /// - `ppn`: 物理页号 / Physical Page Number
    /// - `flags`: 页表标志位（R/W/X/U 等）/ Page table flags
    /// - `cap_rights`: 能力授权的权限位 / Capability-authorized permission bits
    ///
    /// # 错误 / Errors
    /// - `MmError::AlreadyMapped`: 虚拟页已被映射
    /// - `MmError::CapabilityDenied`: 能力权限不足
    /// - `MmError::OutOfMemory`: 无法分配中间页表页
    /// - `MmError::InvalidVaddr`: 无效的虚拟页号
    fn map_with_cap(
        &mut self,
        vpn: usize,
        ppn: usize,
        flags: usize,
        cap_rights: usize,
    ) -> Result<(), MmError>;

    /// 简单映射（不检查能力，用于内核自身映射）
    ///
    /// Simple mapping without capability check (for kernel's own mappings).
    /// 内核启动阶段和内核地址空间映射使用此接口。
    fn map(&mut self, vpn: usize, ppn: usize, flags: usize) -> Result<(), MmError> {
        // 默认实现：以最大权限调用 map_with_cap
        self.map_with_cap(vpn, ppn, flags, usize::MAX)
    }

    /// 取消页面映射
    ///
    /// Unmap a virtual page. The physical frame is NOT freed by this operation;
    /// frame deallocation is managed by the capability system.
    fn unmap(&mut self, vpn: usize);

    /// 虚拟页号到物理页号的翻译
    ///
    /// Translate a virtual page number to a physical page number.
    /// Returns `None` if the page is not mapped.
    fn translate(&self, vpn: usize) -> Option<usize>;

    /// 激活页表（加载到硬件寄存器）
    ///
    /// Activate this page table by loading its token into the hardware register.
    /// - RISC-V64: 写入 `satp` CSR
    /// - LoongArch64: 写入 `PGDL` CSR
    fn activate(&self);

    /// 按 ASID 刷新 TLB（微内核特有）
    ///
    /// Flush TLB entries for a specific ASID.
    /// This is critical for microkernel performance: when switching between
    /// address spaces, only the relevant ASID's TLB entries need to be flushed,
    /// avoiding the expensive global TLB flush.
    ///
    /// 对标 seL4 的 `invalidateTLBByASID`。
    ///
    /// - RISC-V64: `sfence.vma zero, asid`
    /// - LoongArch64: `invtlb 0x4, asid, zero`（按 ASID 无效化）
    fn flush_asid(&self, asid: u16);
}


// ============================================================================
// 架构主 Trait / Architecture Main Trait
// ============================================================================

/// 架构硬件抽象层主 trait
///
/// Main architecture HAL trait for RustMicroOS microkernel.
///
/// 这是 RustMicroOS 的核心架构抽象，通过关联类型将陷阱上下文、
/// 内核上下文、页表三大组件统一在一个 trait 下，实现架构无关的
/// 内核上层代码。
///
/// ## 设计对标 / Design References
///
/// - **seL4 `arch/` 目录**：每个架构目录包含 `object/`（内核对象操作）、
///   `machine/`（硬件操作）、`kernel/`（架构相关内核逻辑）三个子目录。
///   本 trait 将 `machine/` 的功能统一为静态方法。
///
/// - **Asterinas OSTD 框架**：`ostd::arch` 模块提供 `irq_enable()`、
///   `irq_disable()`、`timer_init()` 等函数。本 trait 采用类似的方法签名，
///   但通过 trait 关联类型实现编译期多态。
///
/// - **PolyHAL**：`polyhal::Hal` trait 提供 `init()`、`shutdown()` 等方法。
///   本 trait 扩展了微内核特有的 `flush_tlb_asid` 和 IPC 相关接口。
///
/// ## 使用方式 / Usage
///
/// ```rust,ignore
/// // 在 arch/mod.rs 中通过条件编译选择具体实现
/// #[cfg(target_arch = "riscv64")]
/// pub type HalImpl = riscv64::Rv64Hal;
///
/// #[cfg(target_arch = "loongarch64")]
/// pub type HalImpl = loongarch64::La64Hal;
///
/// // 内核上层代码通过类型别名使用
/// type TrapCtx = <HalImpl as ArchHal>::TrapContext;
/// type KernelCtx = <HalImpl as ArchHal>::Context;
/// type PageTbl = <HalImpl as ArchHal>::PageTable;
/// ```
pub trait ArchHal {
    /// 陷阱上下文类型（用户态 ↔ 内核态切换时保存的完整寄存器状态）
    ///
    /// Trap context type for user-kernel mode transitions.
    /// - RISC-V64: 32 个通用寄存器 + sstatus + sepc + kernel_satp + kernel_sp
    /// - LoongArch64: 32 个通用寄存器 + ERA + PRMD + BADV + kernel_sp
    type TrapContext: TrapContextOps;

    /// 内核上下文类型（线程切换时保存的 callee-saved 寄存器）
    ///
    /// Kernel context type for thread/process switching.
    /// - RISC-V64: ra + sp + s0-s11
    /// - LoongArch64: ra + sp + s0-s8 + fp + tp
    type Context: ContextOps;

    /// 页表类型
    ///
    /// Page table type for virtual memory management.
    /// - RISC-V64: Sv39 三级页表
    /// - LoongArch64: 多级页表 + 软件 TLB 重填
    type PageTable: PageTableOps;

    // ========================================================================
    // 中断控制 / Interrupt Control
    // ========================================================================

    /// 开启全局中断
    ///
    /// Enable global interrupts.
    /// - RISC-V64: `csrs sstatus, SIE`
    /// - LoongArch64: `csrwr CRMD` 设置 IE 位
    fn enable_interrupts();

    /// 关闭全局中断
    ///
    /// Disable global interrupts.
    /// - RISC-V64: `csrc sstatus, SIE`
    /// - LoongArch64: `csrwr CRMD` 清除 IE 位
    fn disable_interrupts();

    /// 查询全局中断是否开启
    ///
    /// Check if global interrupts are enabled.
    /// - RISC-V64: 读取 `sstatus.SIE`
    /// - LoongArch64: 读取 `CRMD.IE`
    fn is_interrupt_enabled() -> bool;

    /// 保存中断状态并关闭中断（临界区入口）
    ///
    /// Save current interrupt state and disable interrupts.
    /// Returns the saved state for later restoration.
    /// 用于实现自旋锁等临界区保护。
    ///
    /// # 默认实现
    ///
    /// 读取当前中断状态，关闭中断，返回之前的状态。
    fn interrupt_save_and_disable() -> bool {
        let was_enabled = Self::is_interrupt_enabled();
        Self::disable_interrupts();
        was_enabled
    }

    /// 恢复中断状态（临界区出口）
    ///
    /// Restore interrupt state from a previously saved value.
    ///
    /// # 默认实现
    ///
    /// 如果之前中断是开启的，则重新开启。
    fn interrupt_restore(saved: bool) {
        if saved {
            Self::enable_interrupts();
        }
    }

    // ========================================================================
    // 计时器 / Timer
    // ========================================================================

    /// 初始化计时器
    ///
    /// Initialize the architecture timer.
    /// - RISC-V64: 通过 SBI 设置 timer 中断
    /// - LoongArch64: 配置 TCFG CSR（定时器配置寄存器）
    fn timer_init();

    /// 设置下一次定时器中断
    ///
    /// Set the next timer interrupt to fire after `ticks` timer ticks.
    /// - RISC-V64: `sbi_set_timer(time + ticks)`
    /// - LoongArch64: 写入 TCFG CSR 设置倒计时
    fn timer_set_next(ticks: u64);

    /// 获取当前时间（计时器 tick 数）
    ///
    /// Get current timer value in ticks.
    /// - RISC-V64: 读取 `time` CSR（通过 `rdtime` 指令）
    /// - LoongArch64: 读取 TVAL CSR 或 stable counter
    fn timer_get_time() -> u64;

    /// 获取计时器频率（Hz）
    ///
    /// Get timer frequency in Hz.
    /// - RISC-V64: 通常 10MHz (QEMU virt)
    /// - LoongArch64: 通常 100MHz (QEMU virt)
    fn timer_freq() -> u64;

    /// 获取当前时间（纳秒）
    ///
    /// Get current time in nanoseconds.
    /// 默认实现通过 tick 数和频率计算。
    fn timer_get_time_ns() -> u64 {
        let ticks = Self::timer_get_time();
        let freq = Self::timer_freq();
        if freq == 0 {
            return 0;
        }
        // 避免溢出：先除后乘
        // ticks / freq * 1_000_000_000 + (ticks % freq) * 1_000_000_000 / freq
        let secs = ticks / freq;
        let remainder = ticks % freq;
        secs * 1_000_000_000 + remainder * 1_000_000_000 / freq
    }

    // ========================================================================
    // TLB / 缓存管理 / TLB & Cache Management
    // ========================================================================

    /// 刷新 TLB
    ///
    /// Flush TLB entries.
    /// - `vaddr == None`: 刷新所有 TLB 条目（全局 flush）
    /// - `vaddr == Some(addr)`: 仅刷新指定虚拟地址的 TLB 条目
    ///
    /// - RISC-V64: `sfence.vma` / `sfence.vma addr`
    /// - LoongArch64: `invtlb 0x0` (全部) / `invtlb 0x5, 0, addr` (指定地址)
    fn flush_tlb(vaddr: Option<usize>);

    /// 按 ASID 刷新 TLB（微内核特有）
    ///
    /// Flush all TLB entries associated with a specific ASID.
    /// This is a microkernel-critical operation: when a process's address space
    /// is modified or destroyed, only its ASID's TLB entries need invalidation.
    ///
    /// 对标 seL4 的 `invalidateTLBByASID` 和 `invalidateASIDEntry`。
    ///
    /// - RISC-V64: `sfence.vma zero, asid`
    /// - LoongArch64: `invtlb 0x4, asid, zero`
    fn flush_tlb_asid(asid: u16);

    /// 指令缓存同步屏障
    ///
    /// Instruction cache fence / synchronization barrier.
    /// 在修改代码页后必须调用，确保指令缓存与数据缓存一致。
    ///
    /// - RISC-V64: `fence.i`
    /// - LoongArch64: `ibar 0`（指令屏障）
    fn fence_i();

    // ========================================================================
    // 电源管理 / Power Management
    // ========================================================================

    /// 等待中断（低功耗等待）
    ///
    /// Wait for interrupt (low-power idle).
    /// CPU 进入低功耗状态，直到下一个中断到来。
    ///
    /// - RISC-V64: `wfi` 指令
    /// - LoongArch64: `idle 0` 指令
    fn wfi();

    /// 系统关机
    ///
    /// Shutdown the system. This function never returns.
    ///
    /// # 参数 / Parameters
    /// - `code`: 退出码（0 = 正常关机，非 0 = 错误关机）
    ///
    /// # 实现策略 / Implementation Strategy
    /// - RISC-V64: SBI `shutdown` 调用 → QEMU FINISHER 寄存器 → 死循环
    /// - LoongArch64: ACPI 关机 → 平台特定关机 → 死循环
    ///
    /// 对标 seL4 的 `handleUserLevelFault` 中的 halt 路径。
    fn shutdown(code: u32) -> !;

    // ========================================================================
    // 平台信息 / Platform Information
    // ========================================================================

    /// 获取当前 Hart/Core ID
    ///
    /// Get the current hardware thread (hart) or core ID.
    /// - RISC-V64: 读取 `tp` 寄存器（启动时由 SBI 设置）
    /// - LoongArch64: 读取 CSR.CPUID
    fn hart_id() -> usize;

    /// 获取页大小（字节）
    ///
    /// Get the page size in bytes.
    /// - RISC-V64: 4096 (Sv39)
    /// - LoongArch64: 4096-16384 (可配置，QEMU 默认 16KB，我们使用 4KB)
    fn page_size() -> usize;

    /// 获取最大物理地址位数
    ///
    /// Get the maximum number of physical address bits.
    /// - RISC-V64: 56 位 (Sv39)
    /// - LoongArch64: 48 位
    fn max_physical_address_bits() -> usize {
        // 默认 48 位，架构实现可覆盖
        48
    }

    /// 获取最大虚拟地址位数
    ///
    /// Get the maximum number of virtual address bits.
    /// - RISC-V64: 39 位 (Sv39)
    /// - LoongArch64: 48 位
    fn max_virtual_address_bits() -> usize {
        // 默认 39 位（Sv39），架构实现可覆盖
        39
    }

    /// 获取架构名称（用于日志和调试）
    ///
    /// Get the architecture name string (for logging and debugging).
    fn arch_name() -> &'static str;
}

// ============================================================================
// 页表标志位常量 / Page Table Flag Constants
// ============================================================================

/// 架构无关的页表标志位
///
/// Architecture-independent page table flags.
/// 各架构实现负责将这些标志位转换为硬件特定的 PTE 位。
///
/// 对标 seL4 的 `seL4_CapRights_t` 和 Asterinas OSTD 的 `PageFlags`。
pub mod pte_flags {
    /// 页面有效 / Page is valid (present)
    pub const VALID: usize = 1 << 0;
    /// 可读 / Readable
    pub const READ: usize = 1 << 1;
    /// 可写 / Writable
    pub const WRITE: usize = 1 << 2;
    /// 可执行 / Executable
    pub const EXEC: usize = 1 << 3;
    /// 用户态可访问 / User-mode accessible
    pub const USER: usize = 1 << 4;
    /// 全局页（不随 ASID 刷新）/ Global page (not flushed with ASID)
    pub const GLOBAL: usize = 1 << 5;
    /// 已访问 / Accessed
    pub const ACCESSED: usize = 1 << 6;
    /// 已修改（脏页）/ Dirty
    pub const DIRTY: usize = 1 << 7;

    // 组合标志 / Combined flags

    /// 用户态只读 / User read-only
    pub const USER_RO: usize = VALID | READ | USER | ACCESSED;
    /// 用户态读写 / User read-write
    pub const USER_RW: usize = VALID | READ | WRITE | USER | ACCESSED | DIRTY;
    /// 用户态读+执行 / User read-execute
    pub const USER_RX: usize = VALID | READ | EXEC | USER | ACCESSED;
    /// 用户态读写+执行 / User read-write-execute
    pub const USER_RWX: usize = VALID | READ | WRITE | EXEC | USER | ACCESSED | DIRTY;
    /// 内核态读写 / Kernel read-write
    pub const KERNEL_RW: usize = VALID | READ | WRITE | ACCESSED | DIRTY | GLOBAL;
    /// 内核态读+执行 / Kernel read-execute
    pub const KERNEL_RX: usize = VALID | READ | EXEC | ACCESSED | GLOBAL;
    /// 内核态读写+执行 / Kernel read-write-execute
    pub const KERNEL_RWX: usize = VALID | READ | WRITE | EXEC | ACCESSED | DIRTY | GLOBAL;
}

// ============================================================================
// 辅助 trait 和类型 / Helper Traits and Types
// ============================================================================

/// 中断守卫（RAII 风格的中断保护）
///
/// Interrupt guard for RAII-style interrupt protection.
/// 创建时保存中断状态并关闭中断，销毁时恢复中断状态。
///
/// ## 使用示例 / Usage Example
///
/// ```rust,ignore
/// {
///     let _guard = InterruptGuard::<HalImpl>::new();
///     // 临界区代码，中断已关闭
///     // Critical section, interrupts are disabled
/// } // guard 销毁时自动恢复中断状态
/// ```
pub struct InterruptGuard<H: ArchHal> {
    saved: bool,
    _marker: core::marker::PhantomData<H>,
}

impl<H: ArchHal> InterruptGuard<H> {
    /// 创建中断守卫（关闭中断并保存状态）
    ///
    /// Create an interrupt guard (disable interrupts and save state).
    pub fn new() -> Self {
        let saved = H::interrupt_save_and_disable();
        Self {
            saved,
            _marker: core::marker::PhantomData,
        }
    }
}

impl<H: ArchHal> Drop for InterruptGuard<H> {
    fn drop(&mut self) {
        H::interrupt_restore(self.saved);
    }
}

// ============================================================================
// 编译期断言 / Compile-Time Assertions
// ============================================================================

/// 验证 IpcMessage 的大小符合预期（5 个 usize = 40 字节 on 64-bit）
///
/// Verify IpcMessage size is as expected (5 * usize = 40 bytes on 64-bit).
const _: () = {
    assert!(core::mem::size_of::<IpcMessage>() == 5 * core::mem::size_of::<usize>());
};
