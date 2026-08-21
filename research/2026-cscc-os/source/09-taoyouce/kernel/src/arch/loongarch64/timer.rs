//! # LoongArch64 定时器操作
//!
//! LoongArch64 timer operations.
//!
//! ## 来源说明 / Source Attribution
//!
//! 定时器操作参考自 PolyHAL 项目（https://github.com/Byte-OS/polyhal）
//! 和《龙芯架构参考手册》卷一第 7.5 节。遵循 MIT/Apache-2.0 双协议。
//!
//! Timer operations adapted from PolyHAL project and
//! "LoongArch Reference Manual Volume 1" Section 7.5.
//! Licensed under MIT/Apache-2.0.
//!
//! ## LoongArch64 定时器机制 / Timer Mechanism
//!
//! LoongArch64 使用 TCFG/TVAL/TICLR 三个 CSR 管理定时器：
//! - **TCFG**：配置定时器（使能 + 周期/单次 + 初始值）
//! - **TVAL**：当前倒计时值（只读）
//! - **TICLR**：清除定时器中断（写 1 清除）
//!
//! 另外，LoongArch64 有一个 64 位稳定计数器（Stable Counter），
//! 通过 `rdtime.d` 指令读取，类似 RISC-V 的 `rdtime`。
//!
//! ## 与 RISC-V64 的对应关系
//!
//! | LoongArch64 | RISC-V64 | 说明 |
//! |-------------|----------|------|
//! | TCFG CSR | SBI set_timer | 定时器配置 |
//! | TVAL CSR | time CSR | 当前时间 |
//! | TICLR CSR | — | 中断清除 |
//! | rdtime 指令 | rdtime 指令 | 读取稳定计数器 |
//! | 100MHz (QEMU) | 10MHz (QEMU) | 默认频率 |

#![allow(dead_code)]

use super::csr::*;

// ============================================================================
// 定时器常量 / Timer Constants
// ============================================================================

/// LoongArch64 QEMU virt 平台的定时器频率（100MHz）
///
/// Timer frequency for LoongArch64 QEMU virt platform (100MHz).
/// RISC-V64 QEMU virt 为 10MHz，LoongArch64 为 100MHz。
pub const LA64_TIMER_FREQ: u64 = 100_000_000; // 100MHz

/// 定时器中断间隔（10ms = 频率 / 100）
///
/// Timer interrupt interval (10ms = frequency / 100).
pub const TICK_INTERVAL: u64 = LA64_TIMER_FREQ / 100;

// ============================================================================
// 定时器操作 / Timer Operations
// ============================================================================

/// 初始化定时器
///
/// Initialize the LoongArch64 timer.
///
/// 配置 TCFG 寄存器：
/// - En=1（使能定时器）
/// - Periodic=1（周期模式）
/// - InitVal=TICK_INTERVAL（10ms 间隔）
pub fn init() {
    // 设置定时器为周期模式，间隔 10ms
    // Set timer to periodic mode with 10ms interval
    //
    // TCFG 格式：[N:2]=InitVal, [1]=Periodic, [0]=En
    // TCFG format: [N:2]=InitVal, [1]=Periodic, [0]=En
    let tcfg_val = (TICK_INTERVAL << TCFG_INITVAL_SHIFT as u64)
        | TCFG_PERIODIC as u64
        | TCFG_EN as u64;

    csrwr!(tcfg_val as usize, "0x41");
    
    // 验证 timer 配置
    let tcfg_read = csrrd!("0x41");
    let ecfg_read = csrrd!("0x4");
    let crmd_read = csrrd!("0x0");
    crate::println!("[timer] TCFG={:#x} ECFG={:#x} CRMD={:#x} TICK={}", 
        tcfg_read, ecfg_read, crmd_read, TICK_INTERVAL);
}

/// 设置下一次定时器中断
///
/// Set the next timer interrupt.
///
/// # 参数 / Parameters
/// - `ticks`: 距离下一次中断的 tick 数
pub fn set_next(ticks: u64) {
    // 重新配置 TCFG 为单次模式
    // Reconfigure TCFG for one-shot mode
    let tcfg_val = (ticks << TCFG_INITVAL_SHIFT as u64) | TCFG_EN as u64;
    csrwr!(tcfg_val as usize, "0x41");
}

/// 清除定时器中断
///
/// Clear the timer interrupt.
/// 在定时器中断处理函数中调用，写 TICLR.CLR=1 清除中断。
///
/// Called in timer interrupt handler, writes TICLR.CLR=1 to clear interrupt.
pub fn clear_timer_interrupt() {
    // 使用直接内联汇编，避免 csrwr! 宏在 large code model 下的 bug
    unsafe {
        core::arch::asm!(
            "li.w $t0, 1",
            "csrwr $t0, 0x44",
            out("$t0") _,
            options(nostack)
        );
    }
}

/// 读取稳定计数器（当前时间 tick 数）
///
/// Read the stable counter (current time in ticks).
///
/// LoongArch64 使用 `rdtime.d` 指令读取 64 位稳定计数器，
/// 类似 RISC-V 的 `rdtime` 指令。
pub fn get_time() -> u64 {
    // LoongArch64 rdtime.d 指令：读取稳定计数器到 rd，计数器 ID 到 rj
    // rdtime.d instruction: reads stable counter to rd, counter ID to rj
    let time: u64;
    let _id: u64;
    unsafe {
        core::arch::asm!(
            "rdtime.d {}, {}",
            out(reg) time,
            out(reg) _id,
        );
    }
    time
}

/// 获取定时器频率（Hz）
///
/// Get timer frequency in Hz.
#[inline]
pub const fn freq() -> u64 {
    LA64_TIMER_FREQ
}

/// 获取当前定时器倒计时值
///
/// Get current timer countdown value from TVAL CSR.
pub fn get_tval() -> usize {
    csrrd!("0x42")
}
