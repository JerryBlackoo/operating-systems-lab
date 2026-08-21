//! Rust 语言项
//! 
//! 提供 panic handler 和内存分配错误处理
//!
//! # 增强的 Panic Handler
//! 
//! 本模块提供了增强的 panic 处理功能，包括：
//! - 栈回溯（Stack Backtrace）
//! - 符号解析（如可用）
//! - GDB 通知（如果 GDB Stub 已连接）
//! - 详细的错误上下文信息

use core::panic::PanicInfo;
use crate::println;
use crate::error::{Backtrace, get_timestamp, timestamp_to_ms};

/// 内核符号表（简化版本）
/// 
/// 在实际实现中，这可以从 ELF 符号表加载。
/// 目前提供一些已知的内核函数符号用于演示。
mod symbol_table {
    /// 符号条目
    pub struct Symbol {
        pub name: &'static str,
        pub addr: usize,
        pub size: usize,
    }
    
    /// 已知的内核符号（静态定义）
    /// 
    /// 注意：这些地址是示例值，实际地址取决于链接器输出。
    /// 在生产环境中，应该从 ELF 符号表动态加载。
    static KNOWN_SYMBOLS: &[Symbol] = &[
        Symbol { name: "kernel_main", addr: 0x8020_0000, size: 0x1000 },
        Symbol { name: "trap_handler", addr: 0x8020_1000, size: 0x500 },
        Symbol { name: "panic", addr: 0x8020_2000, size: 0x200 },
        Symbol { name: "mm::init", addr: 0x8020_3000, size: 0x300 },
        Symbol { name: "sched::init", addr: 0x8020_4000, size: 0x400 },
        Symbol { name: "capability::init", addr: 0x8020_5000, size: 0x200 },
    ];
    
    /// 查找地址对应的符号
    /// 
    /// 返回符号名称和相对于符号起始的偏移量。
    pub fn lookup(addr: usize) -> Option<(&'static str, usize)> {
        for sym in KNOWN_SYMBOLS {
            if addr >= sym.addr && addr < sym.addr + sym.size {
                return Some((sym.name, addr - sym.addr));
            }
        }
        None
    }
    
    /// 检查符号表是否可用
    pub fn is_available() -> bool {
        !KNOWN_SYMBOLS.is_empty()
    }
}

/// 捕获带符号解析的栈回溯
fn capture_backtrace_with_symbols() -> Backtrace {
    let mut backtrace = Backtrace::capture();
    
    // 尝试为每个栈帧解析符号
    for frame in &mut backtrace.frames {
        if let Some((name, offset)) = symbol_table::lookup(frame.return_address) {
            frame.symbol_name = Some(name);
            frame.symbol_offset = offset;
        }
    }
    
    backtrace
}

/// 打印栈回溯
fn print_backtrace(backtrace: &Backtrace) {
    if backtrace.is_empty() {
        println!("  <no backtrace available>");
        println!("  (frame pointer may not be preserved)");
        return;
    }
    
    for (i, frame) in backtrace.frames.iter().enumerate() {
        if let Some(name) = frame.symbol_name {
            println!(
                "  #{:2}: {:#018x} <{}+{:#x}>",
                i, frame.return_address, name, frame.symbol_offset
            );
        } else {
            println!(
                "  #{:2}: {:#018x}",
                i, frame.return_address
            );
        }
    }
    
    if backtrace.truncated {
        println!("  ... (truncated, max {} frames)", Backtrace::MAX_FRAMES);
    }
}

/// 通知 GDB 发生了 panic
/// 
/// 如果 GDB Stub 已连接，通知它发生了致命错误。
/// 这允许开发者在 GDB 中检查 panic 时的程序状态。
fn notify_gdb_panic(info: &PanicInfo) {
    // 检查 GDB 是否启用
    if !crate::config::is_gdb_enabled() {
        return;
    }
    
    // 获取 GDB Stub 实例
    if let Some(stub) = crate::gdb::get_stub() {
        if stub.is_connected() {
            // 通知 GDB 发生了异常
            // 使用 SIGABRT (6) 表示 panic
            const SIGABRT: u64 = 6;
            stub.exception(SIGABRT);
            
            println!("[gdb] GDB notified of panic (SIGABRT)");
            
            // 如果有位置信息，打印给 GDB 用户
            if let Some(location) = info.location() {
                println!("[gdb] Panic location: {}:{}:{}", 
                    location.file(), location.line(), location.column());
            }
        }
    }
}

/// 获取当前 CPU/Hart ID
fn get_hart_id() -> usize {
    crate::arch::hartid()
}

/// 增强的 Panic 处理函数
/// 
/// 提供详细的 panic 信息，包括：
/// - 时间戳
/// - Hart ID
/// - 位置信息（文件、行号、列号）
/// - 错误消息
/// - 栈回溯（带符号解析）
/// - GDB 通知
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // 禁用中断，防止被打断
    crate::arch::disable_interrupts();

    // 对标 Linux `oops_enter()` → `bust_spinlocks(1)`：崩溃路径不再依赖控制台锁。
    // 多核下持 UART 锁的可能正是已经崩掉/halt 的那个 CPU，此时任何 `println!`
    // 都会永久自旋，panic 现场将完全不可见（实测：串口停在打印中途）。
    crate::console::bust_locks();

    
    // 获取时间戳
    let timestamp = get_timestamp();
    let timestamp_ms = timestamp_to_ms(timestamp);
    
    // 获取 Hart ID
    let hart_id = get_hart_id();
    
    // 打印 panic 头部（带颜色）
    println!();
    println!("\x1b[41;37m╔══════════════════════════════════════════════════════════════╗\x1b[0m");
    println!("\x1b[41;37m║                      KERNEL PANIC                            ║\x1b[0m");
    println!("\x1b[41;37m╚══════════════════════════════════════════════════════════════╝\x1b[0m");
    println!();
    
    // 打印基本信息
    println!("\x1b[33m[Panic Info]\x1b[0m");
    println!("  Timestamp:  {} ({} ms since boot)", timestamp, timestamp_ms);
    println!("  Hart ID:    {}", hart_id);
    
    // 打印位置信息
    if let Some(location) = info.location() {
        println!();
        println!("\x1b[33m[Location]\x1b[0m");
        println!("  File:       {}", location.file());
        println!("  Line:       {}", location.line());
        println!("  Column:     {}", location.column());
    }
    
    // 打印错误消息
    println!();
    println!("\x1b[33m[Message]\x1b[0m");
    println!("  {}", info.message());
    
    // 捕获并打印栈回溯
    println!();
    println!("\x1b[33m[Stack Backtrace]\x1b[0m");
    let backtrace = capture_backtrace_with_symbols();
    print_backtrace(&backtrace);
    
    // 打印符号表状态
    if symbol_table::is_available() {
        println!();
        println!("\x1b[36m[Symbol Resolution]\x1b[0m");
        println!("  Symbol table: available (limited)");
        println!("  Note: Full symbol resolution requires loading from ELF");
    }
    
    // 通知 GDB（如果已连接）
    println!();
    println!("\x1b[33m[GDB Notification]\x1b[0m");
    if crate::config::is_gdb_enabled() {
        if let Some(stub) = crate::gdb::get_stub() {
            if stub.is_connected() {
                println!("  GDB connected: yes");
                notify_gdb_panic(info);
            } else {
                println!("  GDB connected: no (stub initialized but not connected)");
            }
        } else {
            println!("  GDB stub: not initialized");
        }
    } else {
        println!("  GDB: disabled in config");
    }
    
    // 打印寄存器状态（如果可用）
    println!();
    println!("\x1b[33m[Register State]\x1b[0m");
    print_register_state();
    
    // 打印结束信息
    println!();
    println!("\x1b[31m════════════════════════════════════════════════════════════════\x1b[0m");
    println!("\x1b[31m  System halted. Please reset or power cycle to restart.\x1b[0m");
    println!("\x1b[31m════════════════════════════════════════════════════════════════\x1b[0m");
    
    // 停机循环
    loop {
        crate::arch::wfi();
    }
}

/// 打印当前寄存器状态
/// 
/// 打印一些关键的 CSR 寄存器值，帮助调试。
fn print_register_state() {
    // 读取关键 CSR 寄存器
    let mut sstatus: usize = 0;
    let mut sepc: usize = 0;
    let mut scause: usize = 0;
    let mut stval: usize = 0;
    let mut sp: usize = 0;
    let mut fp: usize = 0;
    let mut ra: usize = 0;
    
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("csrr {}, sstatus", out(reg) sstatus);
        core::arch::asm!("csrr {}, sepc", out(reg) sepc);
        core::arch::asm!("csrr {}, scause", out(reg) scause);
        core::arch::asm!("csrr {}, stval", out(reg) stval);
        core::arch::asm!("mv {}, sp", out(reg) sp);
        core::arch::asm!("mv {}, fp", out(reg) fp);
        core::arch::asm!("mv {}, ra", out(reg) ra);
    }
    
    println!("  sstatus:    {:#018x}", sstatus);
    println!("  sepc:       {:#018x}", sepc);
    println!("  scause:     {:#018x}", scause);
    println!("  stval:      {:#018x}", stval);
    println!("  sp:         {:#018x}", sp);
    println!("  fp:         {:#018x}", fp);
    println!("  ra:         {:#018x}", ra);
}

/// 内存分配错误处理
/// 
/// 当堆内存分配失败时调用此函数。
/// 提供详细的分配失败信息。
#[alloc_error_handler]
fn alloc_error(layout: core::alloc::Layout) -> ! {
    let (used, total, count, free_total, largest_free) = crate::mm::heap_stats();
    println!(
        "[alloc_error] heap_used={} heap_total={} alloc_count={} free_total={} largest_free={}",
        used,
        total,
        count,
        free_total,
        largest_free,
    );
    if let Some((processes, zombies, threads, child_refs, current_pid)) =
        crate::sched::process::try_process_stats_snapshot()
    {
        println!(
            "[alloc_error] processes={} zombies={} threads={} child_refs={} current_pid={}",
            processes,
            zombies,
            threads,
            child_refs,
            current_pid,
        );
    } else {
        println!("[alloc_error] process_stats_unavailable");
    }
    let bins = crate::mm::heap_size_bins();
    println!(
        "[alloc_error] bins16={} bins32={} bins64={} bins128={} bins256={} bins512={} bins1k={} bins2k={} bins4k={} bins8k={} bins16k={} bins32k={} bins64k={} bins128k={} bins256k={} bins_large={}",
        bins[0],
        bins[1],
        bins[2],
        bins[3],
        bins[4],
        bins[5],
        bins[6],
        bins[7],
        bins[8],
        bins[9],
        bins[10],
        bins[11],
        bins[12],
        bins[13],
        bins[14],
        bins[15],
    );
    if let Some((sector_cache, inode_cache)) = crate::fs::ext4::cache_stats_snapshot() {
        println!(
            "[alloc_error] ext4_sector_cache={} ext4_inode_cache={}",
            sector_cache,
            inode_cache,
        );
    } else {
        println!("[alloc_error] ext4_cache_stats_unavailable");
    }
    if let Some((overlay, file_cow, neg)) = crate::fs::ext4::overlay_stats_snapshot() {
        println!(
            "[alloc_error] ext4_overlay={} ext4_file_cow={} ext4_neg_cache={}",
            overlay,
            file_cow,
            neg,
        );
    }
    {
        let fs = crate::mm::page_meta::collect_stats();
        println!(
            "[alloc_error] frames total={} free={} user={} kstack={} slab={} pgtbl={} pgcache={} heap={} dma={} shmem={} hyp={}",
            fs.total_scanned, fs.free, fs.user, fs.kernel_stack, fs.slab,
            fs.page_table, fs.page_cache, fs.heap, fs.dma, fs.shared_mem, fs.hypervisor,
        );
    }
    if let Some((futex_keys, futex_waiters)) = crate::syscall::futex_waiter_stats_snapshot() {
        println!(
            "[alloc_error] futex_keys={} futex_waiters={}",
            futex_keys,
            futex_waiters,
        );
    } else {
        println!("[alloc_error] futex_stats_unavailable");
    }
    panic!(
        "Memory allocation failed: size={} bytes, align={} bytes",
        layout.size(),
        layout.align()
    );
}

/// Panic Handler 自测试
/// 
/// 注意：此函数不会触发真正的 panic，只测试辅助功能。
#[cfg(test)]
pub fn self_test() {
    use crate::println;
    
    println!("[lang_items] Running self-test...");
    
    // 测试符号查找
    let result = symbol_table::lookup(0x8020_0500);
    if let Some((name, offset)) = result {
        println!("[lang_items] Symbol lookup test: {} + {:#x}", name, offset);
    }
    
    // 测试栈回溯捕获
    let backtrace = capture_backtrace_with_symbols();
    println!("[lang_items] Backtrace captured: {} frames", backtrace.len());
    
    // 测试时间戳
    let ts = crate::error::get_timestamp();
    println!("[lang_items] Current timestamp: {}", ts);
    
    println!("[lang_items] Self-test passed!");
}
