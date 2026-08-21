//! CPU 上下文管理

#![allow(dead_code)]

/// CPU 上下文结构
/// 用于保存和恢复线程状态
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct Context {
    /// 返回地址
    pub ra: usize,
    /// 栈指针
    pub sp: usize,
    /// 被调用者保存寄存器 s0-s11
    pub s: [usize; 12],
    /// 程序计数器
    pub sepc: usize,
    /// 状态寄存器
    pub sstatus: usize,
}

impl Context {
    /// 创建空上下文
    pub const fn zero() -> Self {
        Self {
            ra: 0,
            sp: 0,
            s: [0; 12],
            sepc: 0,
            sstatus: 0,
        }
    }
    
    /// 创建默认上下文
    pub const fn new() -> Self {
        Self::zero()
    }
    
    /// 创建新线程的初始上下文
    pub fn init(entry: usize, sp: usize, sstatus: usize) -> Self {
        Self {
            ra: entry,
            sp,
            s: [0; 12],
            sepc: entry,
            sstatus,
        }
    }
    
    /// 设置返回值（a0 寄存器）
    pub fn set_return_value(&mut self, _value: usize) {
        // 在完整实现中，需要保存 a0
        // 这里简化处理
    }
}

/// 上下文切换
/// 
/// # Safety
/// 调用者必须确保 current 和 next 指向有效的 Context
#[naked]
pub unsafe extern "C" fn switch_context(
    current: *mut Context,
    next: *const Context,
) {
    core::arch::naked_asm!(
        // 保存当前上下文
        "sd ra, 0(a0)",
        "sd sp, 8(a0)",
        "sd s0, 16(a0)",
        "sd s1, 24(a0)",
        "sd s2, 32(a0)",
        "sd s3, 40(a0)",
        "sd s4, 48(a0)",
        "sd s5, 56(a0)",
        "sd s6, 64(a0)",
        "sd s7, 72(a0)",
        "sd s8, 80(a0)",
        "sd s9, 88(a0)",
        "sd s10, 96(a0)",
        "sd s11, 104(a0)",
        
        // 恢复下一个上下文
        "ld ra, 0(a1)",
        "ld sp, 8(a1)",
        "ld s0, 16(a1)",
        "ld s1, 24(a1)",
        "ld s2, 32(a1)",
        "ld s3, 40(a1)",
        "ld s4, 48(a1)",
        "ld s5, 56(a1)",
        "ld s6, 64(a1)",
        "ld s7, 72(a1)",
        "ld s8, 80(a1)",
        "ld s9, 88(a1)",
        "ld s10, 96(a1)",
        "ld s11, 104(a1)",
        
        "ret",
    );
}
