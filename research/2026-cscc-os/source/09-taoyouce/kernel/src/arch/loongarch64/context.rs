//! # LoongArch64 上下文数据结构
//!
//! LoongArch64 context data structures for trap handling and kernel thread switching.
//!
//! ## 来源说明 / Source Attribution
//!
//! 数据结构设计参考自 PolyHAL 项目（https://github.com/Byte-OS/polyhal）
//! 的 `TrapFrame` 和 `Context` 定义，适配 RustMicroOS 微内核架构。
//! 遵循 MIT/Apache-2.0 双协议。
//!
//! Data structure design adapted from PolyHAL project's `TrapFrame` and `Context`,
//! customized for RustMicroOS microkernel architecture.
//! Licensed under MIT/Apache-2.0 dual license.
//!
//! ## LoongArch64 寄存器约定 / Register Convention
//!
//! | 寄存器 | ABI 名称 | 用途 |
//! |--------|---------|------|
//! | $r0 | $zero | 常量零 / Constant zero |
//! | $r1 | $ra | 返回地址 / Return address |
//! | $r2 | $tp | 线程指针 / Thread pointer |
//! | $r3 | $sp | 栈指针 / Stack pointer |
//! | $r4-$r11 | $a0-$a7 | 参数/返回值 / Arguments/Return values |
//! | $r12-$r20 | $t0-$t8 | 临时寄存器 / Temporaries |
//! | $r21 | $reserved | 保留 / Reserved |
//! | $r22 | $fp/$s9 | 帧指针 / Frame pointer |
//! | $r23-$r31 | $s0-$s8 | 被调用者保存 / Callee-saved |

use core::fmt;
use crate::arch::hal_trait::{TrapContextOps, ContextOps, IpcMessage};

// ============================================================================
// La64TrapContext — 陷阱上下文 / Trap Context
// ============================================================================

/// LoongArch64 陷阱上下文 — 保存用户态完整寄存器状态
///
/// LoongArch64 trap context — saves complete user-mode register state.
///
/// 🟡 基于 PolyHAL 的 TrapFrame 适配微内核（MIT/Apache-2.0）
///
/// 当用户态程序触发异常（系统调用、页错误、中断等）时，
/// 硬件和软件协作将所有通用寄存器及关键 CSR 保存到此结构体中。
///
/// ## 与 RISC-V64 TrapContext 的对应关系
///
/// | LoongArch64 | RISC-V64 | 说明 |
/// |-------------|----------|------|
/// | `regs[11]` ($a7) | `x[17]` (a7) | 系统调用号 |
/// | `regs[4..10]` ($a0-$a5) | `x[10..16]` (a0-a5) | 系统调用参数 |
/// | `regs[4]` ($a0) | `x[10]` (a0) | 返回值 |
/// | `era` | `sepc` | 异常返回地址 |
/// | `prmd` | `sstatus` | 先前模式信息 |
/// | `badv` | `stval` | 出错虚拟地址 |
#[repr(C)]
#[derive(Clone, Copy)]
pub struct La64TrapContext {
    /// 通用寄存器 $r0-$r31（兼容 RISC-V64 的 `x` 字段名）
    pub x: [usize; 32],

    /// ERA/sepc: 异常返回地址（兼容 RISC-V64 的 `sepc` 字段名）
    pub sepc: usize,

    /// PRMD/sstatus: 先前模式信息（兼容 RISC-V64 的 `sstatus` 字段名）
    pub sstatus: usize,

    /// BADV: 出错虚拟地址
    pub badv: usize,

    /// 内核 SATP（LoongArch64 不使用，保持兼容）
    pub kernel_satp: usize,

    /// 内核栈指针
    pub kernel_sp: usize,

    /// trap handler 地址（LoongArch64 通过 EENTRY CSR 设置，保持兼容）
    pub trap_handler: usize,
}

impl fmt::Debug for La64TrapContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("La64TrapContext")
            .field("era", &format_args!("{:#x}", self.sepc))
            .field("prmd", &format_args!("{:#x}", self.sstatus))
            .field("badv", &format_args!("{:#x}", self.badv))
            .field("sp($r3)", &format_args!("{:#x}", self.x[3]))
            .field("ra($r1)", &format_args!("{:#x}", self.x[1]))
            .field("a0($r4)", &format_args!("{:#x}", self.x[4]))
            .field("a7($r11)", &format_args!("{:#x}", self.x[11]))
            .finish()
    }
}

impl Default for La64TrapContext {
    fn default() -> Self {
        Self::zero_init()
    }
}

impl La64TrapContext {
    /// 创建零初始化的陷阱上下文
    pub const fn zero_init() -> Self {
        Self {
            x: [0; 32],
            sepc: 0,
            sstatus: 0,
            badv: 0,
            kernel_satp: 0,
            kernel_sp: 0,
            trap_handler: 0,
        }
    }

    /// 设置返回值 (a0 = x[4])
    #[inline]
    pub fn set_return_value(&mut self, v: usize) { self.x[4] = v; }

    /// 设置栈指针 (sp = x[3])
    #[inline]
    pub fn set_sp(&mut self, sp: usize) -> &mut Self { self.x[3] = sp; self }

    /// 创建用户态初始上下文（兼容 RISC-V64 的 app_init_context）
    pub fn app_init_context(entry: usize, user_sp: usize, _kernel_satp: usize, kernel_sp: usize, _trap_handler: usize) -> Self {
        let sstatus: usize = 3 | (1 << 2); // PPLV=3, PIE=1
        let mut ctx = Self::zero_init();
        ctx.sepc = entry;
        ctx.sstatus = sstatus;
        ctx.x[3] = user_sp;
        ctx.kernel_sp = kernel_sp;
        ctx
    }

    /// 获取系统调用号（$a7 = $r11）
    #[inline]
    pub fn syscall_id(&self) -> usize {
        self.x[11]
    }

    /// 获取系统调用参数（$a0-$a5 = $r4-$r9）
    ///
    /// Get syscall arguments from $a0-$a5 ($r4-$r9).
    #[inline]
    pub fn syscall_args_la(&self) -> [usize; 6] {
        [
            self.x[4],  // $a0
            self.x[5],  // $a1
            self.x[6],  // $a2
            self.x[7],  // $a3
            self.x[8],  // $a4
            self.x[9],  // $a5
        ]
    }

    /// 获取栈指针（$sp = $r3）
    ///
    /// Get stack pointer from $sp ($r3).
    #[inline]
    pub fn sp(&self) -> usize {
        self.x[3]
    }
}

// ============================================================================
// TrapContextOps trait 实现 / TrapContextOps Trait Implementation
// ============================================================================

impl TrapContextOps for La64TrapContext {
    fn zero() -> Self {
        Self::zero_init()
    }

    /// 系统调用号：$a7 = $r11
    fn syscall_num(&self) -> usize {
        self.x[11]
    }

    /// 系统调用参数：$a0-$a5 = $r4-$r9
    fn syscall_args(&self) -> [usize; 6] {
        [
            self.x[4],  // $a0
            self.x[5],  // $a1
            self.x[6],  // $a2
            self.x[7],  // $a3
            self.x[8],  // $a4
            self.x[9],  // $a5
        ]
    }

    /// 返回值：$a0 = $r4
    fn set_return_value(&mut self, v: usize) {
        self.x[4] = v;
    }

    /// 程序计数器：ERA
    fn set_pc(&mut self, pc: usize) {
        self.sepc = pc;
    }

    fn get_pc(&self) -> usize {
        self.sepc
    }

    /// 栈指针：$sp = $r3
    fn set_sp(&mut self, sp: usize) {
        self.x[3] = sp;
    }

    fn get_sp(&self) -> usize {
        self.x[3]
    }

    /// 参数寄存器：$a0-$a5 = $r4-$r9
    fn set_arg(&mut self, idx: usize, val: usize) {
        assert!(idx < 6, "Argument index out of range: {}", idx);
        self.x[4 + idx] = val;
    }

    fn get_arg(&self, idx: usize) -> usize {
        assert!(idx < 6, "Argument index out of range: {}", idx);
        self.x[4 + idx]
    }

    /// 创建用户态初始上下文
    ///
    /// Create initial trap context for launching a user-mode program.
    ///
    /// LoongArch64 特有设置：
    /// - PRMD.PPLV = 3（返回用户态 PLV3）
    /// - PRMD.PIE = 1（返回后使能中断）
    fn app_init_context(
        entry: usize,
        user_sp: usize,
        _kernel_satp: usize,
        kernel_sp: usize,
        _trap_handler: usize,
    ) -> Self {
        // 设置 PRMD：PPLV=3（用户态）, PIE=1（使能中断）
        // PRMD: PPLV=3 (user mode PLV3), PIE=1 (enable interrupts on return)
        let prmd: usize = 3 | (1 << 2); // PPLV=3, PIE=1

        let mut ctx = Self::zero_init();
        ctx.sepc = entry;
        ctx.sstatus = prmd;
        ctx.x[3] = user_sp;  // $sp
        ctx.kernel_sp = kernel_sp;
        ctx
    }
}

// ============================================================================
// La64Context — 内核上下文 / Kernel Context
// ============================================================================

/// LoongArch64 内核上下文 — 线程切换时保存 callee-saved 寄存器
///
/// LoongArch64 kernel context — saves callee-saved registers during thread switch.
///
/// 🟡 基于 PolyHAL 的 Context 适配（MIT/Apache-2.0）
///
/// 仅保存 callee-saved 寄存器（被调用者保存寄存器），
/// caller-saved 寄存器由调用约定自动处理。
///
/// ## LoongArch64 Callee-Saved 寄存器
///
/// | 字段 | 寄存器 | ABI 名称 | 说明 |
/// |------|--------|---------|------|
/// | ra | $r1 | $ra | 返回地址 |
/// | sp | $r3 | $sp | 栈指针 |
/// | s0 | $r23 | $s0 | 被调用者保存 |
/// | s1 | $r24 | $s1 | 被调用者保存 |
/// | s2 | $r25 | $s2 | 被调用者保存 |
/// | s3 | $r26 | $s3 | 被调用者保存 |
/// | s4 | $r27 | $s4 | 被调用者保存 |
/// | s5 | $r28 | $s5 | 被调用者保存 |
/// | s6 | $r29 | $s6 | 被调用者保存 |
/// | s7 | $r30 | $s7 | 被调用者保存 |
/// | s8 | $r31 | $s8 | 被调用者保存 |
/// | fp | $r22 | $fp/$s9 | 帧指针 |
/// | tp | $r2 | $tp | 线程指针 |
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct La64Context {
    /// $r1: 返回地址 / Return address
    pub ra: usize,
    /// $r3: 栈指针 / Stack pointer
    pub sp: usize,
    /// $r23: 被调用者保存寄存器 s0 / Callee-saved s0
    pub s0: usize,
    /// $r24: 被调用者保存寄存器 s1 / Callee-saved s1
    pub s1: usize,
    /// $r25: 被调用者保存寄存器 s2 / Callee-saved s2
    pub s2: usize,
    /// $r26: 被调用者保存寄存器 s3 / Callee-saved s3
    pub s3: usize,
    /// $r27: 被调用者保存寄存器 s4 / Callee-saved s4
    pub s4: usize,
    /// $r28: 被调用者保存寄存器 s5 / Callee-saved s5
    pub s5: usize,
    /// $r29: 被调用者保存寄存器 s6 / Callee-saved s6
    pub s6: usize,
    /// $r30: 被调用者保存寄存器 s7 / Callee-saved s7
    pub s7: usize,
    /// $r31: 被调用者保存寄存器 s8 / Callee-saved s8
    pub s8: usize,
    /// $r22: 帧指针 / Frame pointer (also s9)
    pub fp: usize,
    /// $r2: 线程指针 / Thread pointer
    pub tp: usize,
}

// ============================================================================
// 汇编上下文切换函数声明 / Assembly Context Switch Function Declaration
// ============================================================================

extern "C" {
    /// LoongArch64 上下文切换汇编函数
    ///
    /// LoongArch64 context switch assembly function.
    ///
    /// 🟢 直接复用 PolyHAL 的 callee-saved 寄存器保存/恢复汇编框架（MIT/Apache-2.0）
    /// 🟡 适配 RustMicroOS 微内核的 La64Context 内存布局
    ///
    /// 保存 `current` 的 callee-saved 寄存器（ra, sp, s0-s8, fp, tp），
    /// 恢复 `next` 的 callee-saved 寄存器，然后通过 `jr $ra` 跳转到
    /// 目标上下文的返回地址继续执行。
    ///
    /// # Safety
    ///
    /// 调用者必须确保 `current` 和 `next` 指向有效的 `La64Context` 结构体。
    /// Caller must ensure both `current` and `next` point to valid `La64Context` structs.
    ///
    /// # 参数 / Parameters
    /// - `current` ($a0): 当前上下文指针（保存寄存器到此处）
    /// - `next` ($a1): 目标上下文指针（从此处恢复寄存器）
    fn __la64_switch(current: *mut La64Context, next: *const La64Context);
}

// ============================================================================
// La64Context 便利方法 / La64Context Convenience Methods
// ============================================================================

impl La64Context {
    /// 创建默认上下文（兼容 RISC-V64 的 Context::new()）
    pub const fn new() -> Self {
        Self {
            ra: 0, sp: 0, s0: 0, s1: 0, s2: 0, s3: 0, s4: 0, s5: 0,
            s6: 0, s7: 0, s8: 0, fp: 0, tp: 0,
        }
    }

    /// 设置返回值（兼容 RISC-V64）
    pub fn set_return_value(&mut self, _val: usize) {
        // La64Context 是内核上下文，不直接设置返回值
    }
}

// ============================================================================
// ContextOps trait 实现 / ContextOps Trait Implementation
// ============================================================================

impl ContextOps for La64Context {
    fn zero() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s0: 0,
            s1: 0,
            s2: 0,
            s3: 0,
            s4: 0,
            s5: 0,
            s6: 0,
            s7: 0,
            s8: 0,
            fp: 0,
            tp: 0,
        }
    }

    /// 创建新线程/进程的初始内核上下文
    ///
    /// Create initial kernel context for a new thread/process.
    /// - `entry` → ra（jr $ra 时跳转到此处开始执行）
    /// - `kstack_top` → sp（内核栈顶地址）
    /// - `satp` → 暂存（LoongArch64 使用 PGDL CSR，此处保留接口兼容）
    fn init(entry: usize, kstack_top: usize, _satp: usize) -> Self {
        Self {
            ra: entry,
            sp: kstack_top,
            s0: 0,
            s1: 0,
            s2: 0,
            s3: 0,
            s4: 0,
            s5: 0,
            s6: 0,
            s7: 0,
            s8: 0,
            fp: 0,
            tp: 0,
        }
    }

    /// 标准线程/进程切换
    ///
    /// Standard context switch between two kernel threads.
    /// 保存 `current` 的 callee-saved 寄存器，恢复 `next` 的寄存器。
    ///
    /// 🟡 基于 PolyHAL 适配（MIT/Apache-2.0）
    ///
    /// 调用汇编函数 `__la64_switch`（定义在 switch.S 中），
    /// 该函数保存当前线程的 ra, sp, s0-s8, fp, tp 到 `current`，
    /// 从 `next` 恢复这些寄存器，然后通过 `jr $ra` 跳转到
    /// 目标线程上次被切换出去时的返回地址继续执行。
    fn switch(current: &mut Self, next: &Self) {
        unsafe {
            __la64_switch(
                current as *mut La64Context,
                next as *const La64Context,
            );
        }
    }

    // ipc_switch 使用默认实现（退化为普通 switch）
    // ipc_switch uses default implementation (degrades to normal switch)
}
