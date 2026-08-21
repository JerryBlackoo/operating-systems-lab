//! RustMicroOS 用户空间库
//!
//! 提供系统调用封装和用户态运行时支持

#![no_std]
#![feature(linkage)]
#![feature(panic_info_message)]

pub mod syscall;
pub mod console;
pub mod fs;
pub mod capability;
pub mod io_uring;

use core::panic::PanicInfo;

// 重新导出常用类型和函数
pub use fs::*;
pub use capability::*;
pub use io_uring::*;
pub use syscall::{sys_exit, sys_write, sys_read, sys_fork, sys_exec, sys_waitpid, sys_getpid, sys_yield, sys_sbrk};

/// 用户程序入口
/// 
/// 内核 execve 在用户栈上布置 Linux 标准 ABI：
/// sp -> [argc, argv[0], argv[1], ..., NULL, envp[0], ..., NULL]
#[no_mangle]
#[link_section = ".text.entry"]
pub extern "C" fn _start() -> ! {
    extern "Rust" {
        fn main(argc: usize, argv: &[&str]) -> i32;
    }
    
    // 读取栈指针（内核设置为指向 argc）
    let sp: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("mv {}, sp", out(reg) sp); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("move {}, $sp", out(reg) sp); }
    
    let argc = unsafe { *(sp as *const usize) };
    let argv_base = (sp + core::mem::size_of::<usize>()) as *const *const u8;
    
    // 解析 argv C 字符串为 &str（最多 32 个参数）
    const MAX_ARGS: usize = 32;
    let mut argv_strs: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let argc = argc.min(MAX_ARGS);
    
    for i in 0..argc {
        let ptr = unsafe { *argv_base.add(i) };
        if ptr.is_null() { break; }
        let mut len = 0;
        while unsafe { *ptr.add(len) } != 0 && len < 4096 {
            len += 1;
        }
        let slice = unsafe { core::slice::from_raw_parts(ptr, len) };
        argv_strs[i] = unsafe { core::str::from_utf8_unchecked(slice) };
    }
    
    let exit_code = unsafe { main(argc, &argv_strs[..argc]) };
    syscall::sys_exit(exit_code);
}

/// Panic 处理
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if let Some(location) = info.location() {
        println!(
            "Panic at {}:{}: {}",
            location.file(),
            location.line(),
            info.message()
        );
    } else {
        println!("Panic: {}", info.message());
    }
    syscall::sys_exit(-1);
}

/// 弱链接的 main 函数
#[linkage = "weak"]
#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    panic!("Cannot find main!");
}
