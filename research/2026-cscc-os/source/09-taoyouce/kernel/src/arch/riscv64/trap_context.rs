//! Trap 上下文
//!
//! 保存用户态进入内核态时的完整 CPU 状态，
//! 以便在系统调用或中断处理完成后恢复用户态执行。

/// Trap 上下文结构
/// 
/// 保存所有通用寄存器和必要的 CSR 寄存器。
/// 布局必须与 trap.S 中的汇编代码保持一致。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrapContext {
    /// 通用寄存器 x0-x31
    /// x0 (zero) 始终为 0，但为了对齐方便也保存
    pub x: [usize; 32],
    /// sstatus 寄存器 - 保存特权级等状态
    pub sstatus: usize,
    /// sepc 寄存器 - 保存触发 trap 的指令地址
    pub sepc: usize,
    /// 内核页表的 satp 值（用于切换地址空间）
    pub kernel_satp: usize,
    /// 内核栈指针
    pub kernel_sp: usize,
    /// trap 处理函数入口地址
    pub trap_handler: usize,
    /// 内核 tp（= 当前 hart 的 cpu id）。用户程序会把 tp 用作 TLS
    /// 指针，__alltraps 从用户态进入后 tp 已被用户值覆盖；由
    /// __restore 在 sret 前（此时 tp 仍为内核 cpu id）写入本字段，
    /// __alltraps 下次进入时从此 reload，恢复正确 cpu id。
    pub kernel_tp: usize,
}

impl TrapContext {
    /// 创建空的 TrapContext
    pub const fn zero() -> Self {
        Self {
            x: [0; 32],
            sstatus: 0,
            sepc: 0,
            kernel_satp: 0,
            kernel_sp: 0,
            trap_handler: 0,
            kernel_tp: 0,
        }
    }

    /// 设置栈指针 (x2/sp)
    pub fn set_sp(&mut self, sp: usize) {
        self.x[2] = sp;
    }

    /// 获取栈指针
    pub fn sp(&self) -> usize {
        self.x[2]
    }

    /// 设置返回值 (x10/a0)
    pub fn set_return_value(&mut self, value: usize) {
        self.x[10] = value;
    }

    /// 获取系统调用号 (x17/a7)
    pub fn syscall_id(&self) -> usize {
        self.x[17]
    }

    /// 获取系统调用参数
    pub fn syscall_args(&self) -> [usize; 6] {
        [
            self.x[10], // a0
            self.x[11], // a1
            self.x[12], // a2
            self.x[13], // a3
            self.x[14], // a4
            self.x[15], // a5
        ]
    }

    /// 创建用于启动用户程序的初始 TrapContext
    /// 
    /// # 参数
    /// - `entry`: 用户程序入口地址
    /// - `sp`: 用户栈顶地址
    /// - `kernel_satp`: 内核页表的 satp 值
    /// - `kernel_sp`: 内核栈顶地址
    /// - `trap_handler`: trap 处理函数地址
    pub fn app_init_context(
        entry: usize,
        sp: usize,
        kernel_satp: usize,
        kernel_sp: usize,
        trap_handler: usize,
    ) -> Self {
        // 设置 sstatus:
        // - SPP = 0 (返回用户态)
        // - SPIE = 1 (sret 后启用中断)
        let mut sstatus: usize;
        unsafe {
            core::arch::asm!("csrr {}, sstatus", out(reg) sstatus);
        }
        // 清除 SPP 位 (bit 8)，设置为用户态
        sstatus &= !(1 << 8);
        // 设置 SPIE 位 (bit 5)，sret 后启用中断
        sstatus |= 1 << 5;

        let mut ctx = Self::zero();
        ctx.sepc = entry;
        ctx.sstatus = sstatus;
        ctx.set_sp(sp);
        ctx.kernel_satp = kernel_satp;
        ctx.kernel_sp = kernel_sp;
        ctx.trap_handler = trap_handler;
        ctx
    }
}

impl Default for TrapContext {
    fn default() -> Self {
        Self::zero()
    }
}
