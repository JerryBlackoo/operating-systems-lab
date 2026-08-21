//! sysinfo - 显示系统信息
//!
//! 显示 RustMicroOS 的系统状态和配置信息

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::syscall::{sys_sysinfo, SysInfo};

#[no_mangle]
pub fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!();
    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║                    RustMicroOS 系统信息                    ║");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!();
    
    // 获取系统信息
    let mut info = SysInfo::default();
    let _ = sys_sysinfo(&mut info);
    
    // 内核信息
    print_section("内核信息");
    println!("  内核名称:     RustMicroOS");
    println!("  内核版本:     1.0.0");
    println!("  架构:         RISC-V 64-bit (rv64gc)");
    println!("  内核类型:     微内核 (Microkernel)");
    println!("  编程语言:     Rust");
    println!();
    
    // CPU 信息
    print_section("CPU 信息");
    println!("  CPU 核心数:   {}", info.cpu_count);
    println!("  ISA 扩展:     IMAFDC");
    println!();
    
    // 内存信息
    print_section("内存信息");
    let total_mb = info.total_ram / 1024 / 1024;
    let free_mb = info.free_ram / 1024 / 1024;
    let used_bytes = info.total_ram - info.free_ram;
    let used_kb = used_bytes / 1024;
    
    println!("  总内存:       {} MB", total_mb);
    if used_kb >= 1024 {
        println!("  已用内存:     {} MB", used_kb / 1024);
    } else {
        println!("  已用内存:     {} KB", used_kb);
    }
    println!("  空闲内存:     {} MB", free_mb);
    
    // 内存使用条（使用更精确的计算，避免整数除法截断）
    let usage_percent = if info.total_ram > 0 {
        // 先乘后除，避免小数被截断
        let used = info.total_ram - info.free_ram;
        ((used * 100) / info.total_ram) as usize
    } else {
        0
    };
    // 如果使用率很低但不为0，至少显示1%
    let display_percent = if usage_percent == 0 && info.free_ram < info.total_ram {
        // 计算实际使用的千分比
        let usage_permille = if info.total_ram > 0 {
            ((info.total_ram - info.free_ram) * 1000 / info.total_ram) as usize
        } else {
            0
        };
        if usage_permille > 0 { 1 } else { 0 }
    } else {
        usage_percent
    };
    print!("  使用率:       [");
    for i in 0..20 {
        if i < display_percent / 5 {
            print!("█");
        } else {
            print!("░");
        }
    }
    println!("] {}%", display_percent);
    println!();
    
    // 进程信息
    print_section("进程信息");
    println!("  进程数:       {}", info.procs);
    println!();
    
    // 运行时间
    print_section("运行时间");
    let uptime = info.uptime;
    let hours = uptime / 3600;
    let minutes = (uptime % 3600) / 60;
    let seconds = uptime % 60;
    println!("  系统运行:     {}h {}m {}s", hours, minutes, seconds);
    println!();
    
    // 特性列表
    print_section("内核特性");
    println!("  ✓ 能力安全系统 (Capability-based Security)");
    println!("  ✓ CHERI 能力模拟 (Memory Safety)");
    println!("  ✓ io_uring 异步 I/O");
    println!("  ✓ 多级反馈队列调度 (MLFQ)");
    println!("  ✓ 实时调度支持 (EDF)");
    println!("  ✓ GDB Stub 调试器");
    println!("  ✓ ProcFS 状态监控");
    println!("  ✓ 形式化验证规约");
    println!();
    
    // 子系统状态
    print_section("子系统状态");
    println!("  内存管理:     ● 运行中");
    println!("  调度器:       ● 运行中");
    println!("  IPC:          ● 运行中");
    println!("  能力系统:     ● 运行中");
    println!("  io_uring:     ● 运行中");
    println!("  文件系统:     ● 运行中");
    println!("  网络栈:       ○ 未启用");
    println!();
    
    println!("═══════════════════════════════════════════════════════════════");
    
    0
}

fn print_section(title: &str) {
    println!("┌─ {} ─────────────────────────────────────────", title);
}
