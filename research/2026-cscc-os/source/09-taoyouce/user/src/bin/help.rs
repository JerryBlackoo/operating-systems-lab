//! help - 显示帮助信息
//!
//! 用法: help [命令]

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

#[no_mangle]
pub fn main(argc: usize, argv: &[&str]) -> i32 {
    if argc > 1 {
        show_command_help(argv[1]);
    } else {
        show_general_help();
    }
    0
}

fn show_general_help() {
    println!("╔═══════════════════════════════════════════════════════════╗");
    println!("║              RustMicroOS 命令帮助                          ║");
    println!("╚═══════════════════════════════════════════════════════════╝");
    println!();
    println!("  基础命令:");
    println!("    ls [path]        列出目录内容");
    println!("    cat <file>       显示文件内容");
    println!("    echo <text>      输出文本");
    println!("    pwd              显示当前目录");
    println!("    cd <path>        切换目录");
    println!();
    println!("  文件操作:");
    println!("    touch <file>     创建空文件");
    println!("    mkdir <dir>      创建目录");
    println!("    rm [-r] <path>   删除文件或目录");
    println!("    cp <src> <dst>   复制文件");
    println!();
    println!("  系统信息:");
    println!("    sysinfo          显示系统信息");
    println!("    ps               显示进程列表");
    println!("    benchmark        运行性能测试");
    println!();
    println!("  特性演示:");
    println!("    cap_demo         能力系统演示");
    println!("    io_uring_demo    io_uring 演示");
    println!();
    println!("  其他:");
    println!("    help [cmd]       显示帮助信息");
    println!("    exit             退出 shell");
    println!();
    println!("  输入 'help <命令>' 获取详细帮助");
}

fn show_command_help(cmd: &str) {
    match cmd {
        "ls" => {
            println!("ls - 列出目录内容");
            println!();
            println!("用法: ls [path]");
            println!();
            println!("示例:");
            println!("  ls          列出当前目录");
            println!("  ls /bin     列出 /bin 目录");
        }
        "cat" => {
            println!("cat - 显示文件内容");
            println!();
            println!("用法: cat <file>");
            println!();
            println!("示例:");
            println!("  cat README.txt    显示 README.txt 内容");
        }
        "cap_demo" => {
            println!("cap_demo - 能力系统演示");
            println!();
            println!("演示 RustMicroOS 的能力安全系统:");
            println!("  1. 基本能力操作（创建、检查）");
            println!("  2. 能力派生（权限衰减）");
            println!("  3. 能力撤销（级联撤销）");
            println!("  4. 双层安全检查（能力 + CHERI）");
        }
        "io_uring_demo" => {
            println!("io_uring_demo - io_uring 异步 I/O 演示");
            println!();
            println!("演示 RustMicroOS 的 io_uring 子系统:");
            println!("  1. 基本 io_uring 操作");
            println!("  2. 批量 I/O 操作");
            println!("  3. 性能对比");
            println!("  4. SQPOLL 模式");
        }
        "benchmark" => {
            println!("benchmark - 系统性能基准测试");
            println!();
            println!("用法: benchmark [test]");
            println!();
            println!("测试项:");
            println!("  syscall    系统调用开销测试");
            println!("  memory     内存分配测试");
            println!("  io         I/O 性能测试");
            println!("  context    上下文切换测试");
            println!("  all        运行所有测试（默认）");
        }
        _ => {
            println!("未知命令: {}", cmd);
            println!("输入 'help' 查看所有可用命令");
        }
    }
}
