//! RustMicroOS 内核属性测试框架
//!
//! 本模块实现内核各子系统的属性测试（Property-based Testing），
//! 用于验证内核组件的正确性和不变量。
//!
//! ## 测试模块
//! - `buddy_tests`: Buddy 分配器属性测试
//! - `page_table_tests`: 页表属性测试
//! - `capability_tests`: 能力系统属性测试
//! - `ipc_tests`: IPC 子系统属性测试
//! - `scheduler_tests`: 调度器属性测试
//! - `realtime_tests`: 实时调度属性测试
//! - `lockfree_tests`: 无锁数据结构属性测试
//! - `syscall_tests`: 系统调用集成测试
//! - `driver_tests`: 驱动隔离测试
//! - `hypervisor_tests`: 虚拟化属性测试
//! - `ebpf_tests`: eBPF 验证器属性测试
//! - `livepatch_tests`: 热补丁属性测试

#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

mod buddy_tests;
mod page_table_tests;
mod capability_tests;
mod ipc_tests;
mod scheduler_tests;
mod realtime_tests;
mod lockfree_tests;
mod syscall_tests;
mod driver_tests;
mod hypervisor_tests;
mod ebpf_tests;
mod livepatch_tests;
mod shell_tests;

use core::panic::PanicInfo;

/// 测试运行器
pub fn test_runner(tests: &[&dyn Testable]) {
    println!("========================================");
    println!("RustMicroOS 内核属性测试");
    println!("========================================");
    println!("运行 {} 个测试...\n", tests.len());
    
    let mut passed = 0;
    let mut failed = 0;
    
    for test in tests {
        if test.run() {
            passed += 1;
        } else {
            failed += 1;
        }
    }
    
    println!("\n========================================");
    println!("测试结果: {} 通过, {} 失败", passed, failed);
    println!("========================================");
    
    if failed > 0 {
        exit_qemu(QemuExitCode::Failed);
    } else {
        exit_qemu(QemuExitCode::Success);
    }
}

/// 可测试特征
pub trait Testable {
    fn run(&self) -> bool;
}

impl<T: Fn() -> bool> Testable for T {
    fn run(&self) -> bool {
        let name = core::any::type_name::<T>();
        print!("测试 {}... ", name);
        let result = self();
        if result {
            println!("[通过]");
        } else {
            println!("[失败]");
        }
        result
    }
}

/// QEMU 退出码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum QemuExitCode {
    Success = 0x10,
    Failed = 0x11,
}

/// 退出 QEMU
pub fn exit_qemu(exit_code: QemuExitCode) -> ! {
    use core::arch::asm;
    unsafe {
        // QEMU virt 平台的退出地址
        let exit_addr = 0x100000 as *mut u32;
        core::ptr::write_volatile(exit_addr, exit_code as u32);
    }
    loop {}
}

/// Panic 处理
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    println!("\n[PANIC] {}", info);
    exit_qemu(QemuExitCode::Failed);
}

/// 简单打印宏
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {{
        use core::fmt::Write;
        let _ = write!(crate::TestConsole, $($arg)*);
    }};
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

/// 测试控制台
pub struct TestConsole;

impl core::fmt::Write for TestConsole {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for byte in s.bytes() {
            unsafe {
                // UART 地址（QEMU virt）
                let uart = 0x10000000 as *mut u8;
                core::ptr::write_volatile(uart, byte);
            }
        }
        Ok(())
    }
}

/// 测试入口
#[no_mangle]
pub extern "C" fn _start() -> ! {
    test_main();
    loop {}
}

/// 简单随机数生成器（用于属性测试）
pub struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    pub fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    
    pub fn next_u64(&mut self) -> u64 {
        // xorshift64
        self.state ^= self.state << 13;
        self.state ^= self.state >> 7;
        self.state ^= self.state << 17;
        self.state
    }
    
    pub fn next_usize(&mut self) -> usize {
        self.next_u64() as usize
    }
    
    pub fn next_range(&mut self, min: usize, max: usize) -> usize {
        if min >= max {
            return min;
        }
        min + (self.next_usize() % (max - min))
    }
}
