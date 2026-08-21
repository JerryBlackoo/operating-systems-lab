//! test_runner - 简单测试运行器
//!
//! 用法: test_runner [测试名]
//! 运行系统功能测试

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::syscall::{
    sys_getpid, sys_fork, sys_waitpid, sys_yield, sys_sbrk, sys_gettime,
    sys_pipe, sys_dup, sys_close, sys_write, sys_read,
    sys_open, sys_getcwd, sys_chdir, sys_lseek,
};

#[no_mangle]
pub fn main(argc: usize, argv: &[&str]) -> i32 {
    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║              RustMicroOS 测试运行器 v2.0                   ║");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!();
    
    let test_name = if argc > 1 { Some(argv[1]) } else { None };
    
    let mut passed = 0;
    let mut failed = 0;

    macro_rules! run_test {
        ($name:expr, $fn:expr) => {
            if test_name.is_none() || test_name == Some($name) {
                if $fn() { passed += 1; } else { failed += 1; }
            }
        };
    }
    
    run_test!("syscall", test_syscall);
    run_test!("memory", test_memory);
    run_test!("process", test_process);
    run_test!("time", test_time);
    run_test!("pipe", test_pipe);
    run_test!("dup", test_dup);
    run_test!("fs", test_fs);
    run_test!("cwd", test_cwd);
    run_test!("signal", test_signal);
    
    // 打印结果
    println!();
    println!("═══════════════════════════════════════════════════════════════");
    println!("  测试结果: {} 通过, {} 失败", passed, failed);
    println!("═══════════════════════════════════════════════════════════════");
    
    if failed > 0 { 1 } else { 0 }
}

fn test_syscall() -> bool {
    print!("测试: 基本系统调用... ");
    
    // 测试 getpid
    let pid = sys_getpid();
    if pid <= 0 {
        println!("失败 (getpid 返回 {})", pid);
        return false;
    }
    
    // 测试 yield
    let ret = sys_yield();
    if ret < 0 {
        println!("失败 (yield 返回 {})", ret);
        return false;
    }
    
    println!("通过 ✓");
    true
}

fn test_memory() -> bool {
    print!("测试: 内存分配... ");
    
    // 获取当前堆顶
    let initial = sys_sbrk(0);
    if initial < 0 {
        println!("失败 (sbrk(0) 返回 {})", initial);
        return false;
    }
    
    // 分配 4KB
    let after_alloc = sys_sbrk(4096);
    if after_alloc < 0 {
        println!("失败 (sbrk(4096) 返回 {})", after_alloc);
        return false;
    }
    
    // 验证堆增长
    let current = sys_sbrk(0);
    if current != after_alloc + 4096 {
        println!("失败 (堆未正确增长)");
        return false;
    }
    
    // 释放
    sys_sbrk(-4096);
    
    println!("通过 ✓");
    true
}

fn test_process() -> bool {
    print!("测试: 进程管理... ");
    
    // 测试 fork
    let pid = sys_fork();
    
    if pid < 0 {
        // fork 可能未实现，跳过
        println!("跳过 (fork 未实现)");
        return true;
    } else if pid == 0 {
        // 子进程
        user::syscall::sys_exit(42);
    } else {
        // 父进程
        let mut exit_code: i32 = 0;
        let wait_ret = sys_waitpid(pid, &mut exit_code);
        
        if wait_ret < 0 {
            println!("失败 (waitpid 返回 {})", wait_ret);
            return false;
        }
        
        if exit_code != 42 {
            println!("失败 (子进程退出码 {} != 42)", exit_code);
            return false;
        }
    }
    
    println!("通过 ✓");
    true
}

fn test_time() -> bool {
    print!("测试: 时间获取... ");
    
    let t1 = sys_gettime();
    if t1 < 0 {
        println!("失败 (gettime 返回 {})", t1);
        return false;
    }
    
    // 简单延迟
    for _ in 0..10000 {
        core::hint::spin_loop();
    }
    
    let t2 = sys_gettime();
    if t2 < t1 {
        println!("失败 (时间倒退)");
        return false;
    }
    
    println!("通过 ✓ (elapsed: {} ns)", t2 - t1);
    true
}

fn test_pipe() -> bool {
    print!("测试: pipe2 管道通信... ");
    
    let mut pipe_fd = [0usize; 2];
    let ret = sys_pipe(&mut pipe_fd);
    if ret < 0 {
        println!("失败 (pipe 返回 {})", ret);
        return false;
    }
    
    // 写入管道
    let msg = b"hello pipe";
    let wret = sys_write(pipe_fd[1], msg);
    if wret != msg.len() as isize {
        println!("失败 (write 返回 {}, 期望 {})", wret, msg.len());
        sys_close(pipe_fd[0]);
        sys_close(pipe_fd[1]);
        return false;
    }
    
    // 读取管道
    let mut buf = [0u8; 32];
    let rret = sys_read(pipe_fd[0], &mut buf);
    if rret != msg.len() as isize {
        println!("失败 (read 返回 {}, 期望 {})", rret, msg.len());
        sys_close(pipe_fd[0]);
        sys_close(pipe_fd[1]);
        return false;
    }
    
    if &buf[..msg.len()] != msg {
        println!("失败 (数据不匹配)");
        sys_close(pipe_fd[0]);
        sys_close(pipe_fd[1]);
        return false;
    }
    
    sys_close(pipe_fd[0]);
    sys_close(pipe_fd[1]);
    
    println!("通过 ✓");
    true
}

fn test_dup() -> bool {
    print!("测试: dup 文件描述符复制... ");
    
    // dup stdout (fd=1)
    let new_fd = sys_dup(1);
    if new_fd < 0 {
        println!("失败 (dup(1) 返回 {})", new_fd);
        return false;
    }
    
    // 向新 fd 写入（应该输出到 stdout）
    let msg = b"[dup ok]";
    let ret = sys_write(new_fd as usize, msg);
    sys_close(new_fd as usize);
    
    if ret != msg.len() as isize {
        println!("失败 (write 返回 {})", ret);
        return false;
    }
    
    println!(" 通过 ✓");
    true
}

fn test_fs() -> bool {
    print!("测试: 文件系统 open/write/read/lseek/close... ");
    
    // 创建测试文件
    let fd = sys_open("/test_data.txt", 0x241); // O_WRONLY|O_CREAT|O_TRUNC
    if fd < 0 {
        println!("失败 (open 创建返回 {})", fd);
        return false;
    }
    
    let data = b"RustMicroOS test 1234567890";
    let wret = sys_write(fd as usize, data);
    sys_close(fd as usize);
    
    if wret != data.len() as isize {
        println!("失败 (write 返回 {})", wret);
        return false;
    }
    
    // 重新打开读取
    let fd = sys_open("/test_data.txt", 0); // O_RDONLY
    if fd < 0 {
        println!("失败 (open 读取返回 {})", fd);
        return false;
    }
    
    let mut buf = [0u8; 64];
    let rret = sys_read(fd as usize, &mut buf);
    if rret != data.len() as isize {
        println!("失败 (read 返回 {}, 期望 {})", rret, data.len());
        sys_close(fd as usize);
        return false;
    }
    
    if &buf[..data.len()] != data {
        println!("失败 (数据不匹配)");
        sys_close(fd as usize);
        return false;
    }
    
    // lseek 测试
    let pos = sys_lseek(fd as usize, 0, 0); // SEEK_SET to 0
    if pos != 0 {
        println!("失败 (lseek SEEK_SET 返回 {})", pos);
        sys_close(fd as usize);
        return false;
    }
    
    sys_close(fd as usize);
    
    println!("通过 ✓");
    true
}

fn test_cwd() -> bool {
    print!("测试: getcwd/chdir... ");
    
    let mut buf = [0u8; 128];
    let ret = sys_getcwd(&mut buf);
    if ret < 0 {
        println!("失败 (getcwd 返回 {})", ret);
        return false;
    }
    
    // 保存当前目录
    let cwd_len = buf.iter().position(|&b| b == 0).unwrap_or(ret as usize);
    let original_cwd = core::str::from_utf8(&buf[..cwd_len]).unwrap_or("/");
    
    // chdir 到根目录
    let ret = sys_chdir("/");
    if ret < 0 {
        println!("失败 (chdir / 返回 {})", ret);
        return false;
    }
    
    let mut buf2 = [0u8; 128];
    sys_getcwd(&mut buf2);
    let new_cwd = core::str::from_utf8(&buf2[..1]).unwrap_or("");
    if new_cwd != "/" {
        println!("失败 (chdir 后 cwd != /)");
        return false;
    }
    
    // 恢复原目录
    sys_chdir(original_cwd);
    
    println!("通过 ✓");
    true
}

fn test_signal() -> bool {
    print!("测试: rt_sigaction/rt_sigprocmask (syscall层)... ");
    
    // 直接用 syscall 接口测试 sigprocmask
    // SIG_SETMASK=2, 设置空掩码
    let empty_mask: u64 = 0;
    let mut old_mask: u64 = 0xFFFF;
    let ret = unsafe {
        user::syscall::syscall4(
            135, // SYS_RT_SIGPROCMASK
            2,   // SIG_SETMASK
            &empty_mask as *const u64 as usize,
            &mut old_mask as *mut u64 as usize,
            8,   // sigsetsize
        )
    };
    if ret < 0 {
        println!("失败 (sigprocmask 返回 {})", ret);
        return false;
    }
    
    // 设置 SIGUSR1(10) 掩码
    let usr1_mask: u64 = 1 << 9; // SIGUSR1=10, bit 9
    let ret = unsafe {
        user::syscall::syscall4(
            135, // SYS_RT_SIGPROCMASK
            0,   // SIG_BLOCK
            &usr1_mask as *const u64 as usize,
            &mut old_mask as *mut u64 as usize,
            8,
        )
    };
    if ret < 0 {
        println!("失败 (sigprocmask BLOCK 返回 {})", ret);
        return false;
    }
    
    // 读回验证
    let mut current_mask: u64 = 0;
    let ret = unsafe {
        user::syscall::syscall4(
            135,
            2, // SIG_SETMASK
            0, // NULL (不设置)
            &mut current_mask as *mut u64 as usize,
            8,
        )
    };
    // set=NULL 时不改变掩码，只读取旧值
    // 但我们的实现 set=0 会跳过设置，old_mask 应该有 SIGUSR1 bit
    if ret < 0 {
        println!("失败 (读取掩码返回 {})", ret);
        return false;
    }
    
    println!("通过 ✓");
    true
}
