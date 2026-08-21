//! RustMicroOS Shell
//!
//! A simple interactive shell supporting:
//! - Built-in commands (help, exit, echo, version, pid, cd, pwd, ls, cat)
//! - External command execution via fork+exec
//! - Command line editing (backspace, line clear)

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::syscall::{
    sys_read, sys_fork, sys_exec, sys_execve, sys_waitpid, sys_getpid, 
    sys_exit, sys_write,
};

/// Shell version
const SHELL_VERSION: &str = "0.4.0";

/// Standard input file descriptor
const STDIN: usize = 0;

/// Command line buffer size
const LINE_BUFFER_SIZE: usize = 256;

/// Maximum number of arguments
const MAX_ARGS: usize = 16;

/// 系统调用号
const SYS_GETCWD: usize = 17;
const SYS_CHDIR: usize = 49;
const SYS_GETDENTS: usize = 61;
const SYS_OPEN: usize = 56;
const SYS_CLOSE: usize = 57;
const SYS_MKDIRAT: usize = 34;
const SYS_UNLINKAT: usize = 35;

/// 系统调用封装
fn syscall1(id: usize, arg0: usize) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") arg0 => ret,
            in("a7") id,
        );
    }
    ret
}

fn syscall2(id: usize, arg0: usize, arg1: usize) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") arg0 => ret,
            in("a1") arg1,
            in("a7") id,
        );
    }
    ret
}

fn syscall3(id: usize, arg0: usize, arg1: usize, arg2: usize) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") arg0 => ret,
            in("a1") arg1,
            in("a2") arg2,
            in("a7") id,
        );
    }
    ret
}

fn syscall4(id: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize) -> isize {
    let ret: isize;
    unsafe {
        core::arch::asm!(
            "ecall",
            inlateout("a0") arg0 => ret,
            in("a1") arg1,
            in("a2") arg2,
            in("a3") arg3,
            in("a7") id,
        );
    }
    ret
}

/// 获取当前工作目录
fn sys_getcwd(buf: *mut u8, size: usize) -> isize {
    syscall2(SYS_GETCWD, buf as usize, size)
}

/// 改变当前工作目录
fn sys_chdir(path: &str) -> isize {
    syscall2(SYS_CHDIR, path.as_ptr() as usize, path.len())
}

/// 读取目录项
fn sys_getdents(fd: usize, buf: *mut u8, size: usize) -> isize {
    syscall3(SYS_GETDENTS, fd, buf as usize, size)
}

/// 打开文件（Linux openat 标准格式）
fn sys_open(path: &str, flags: u32) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    syscall4(SYS_OPEN, AT_FDCWD, buf.as_ptr() as usize, flags as usize, 0)
}

/// 关闭文件
fn sys_close(fd: usize) -> isize {
    syscall1(SYS_CLOSE, fd)
}

/// 创建目录 — mkdirat(AT_FDCWD, path, 0o755)
fn sys_mkdir(path: &str) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    syscall3(SYS_MKDIRAT, AT_FDCWD, buf.as_ptr() as usize, 0o755)
}

/// 删除文件 — unlinkat(AT_FDCWD, path, 0)
fn sys_unlink(path: &str) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    syscall3(SYS_UNLINKAT, AT_FDCWD, buf.as_ptr() as usize, 0)
}

/// Directory entry structure (matches kernel)
#[repr(C)]
#[derive(Clone, Copy)]
struct DirEntry {
    ino: u64,
    file_type: u8,
    name_len: u8,
    name: [u8; 254],
}

/// Shell main function
#[no_mangle]
fn main(_argc: usize, _argv: &[&str]) -> i32 {
    print_welcome();
    
    let mut line_buf = [0u8; LINE_BUFFER_SIZE];
    
    loop {
        // Print prompt with current directory
        print_prompt();
        
        // Read a line of input
        let len = read_line(&mut line_buf);
        if len == 0 {
            continue;
        }
        
        // Parse command
        let cmd_str = match core::str::from_utf8(&line_buf[..len]) {
            Ok(s) => s.trim(),
            Err(_) => {
                println!("sh: invalid UTF-8 input");
                continue;
            }
        };
        
        if cmd_str.is_empty() {
            continue;
        }
        
        // Parse command and arguments
        let (cmd, args) = parse_command(cmd_str);
        
        // Execute command
        match execute_command(cmd, &args) {
            CommandResult::Continue => {}
            CommandResult::Exit(code) => {
                println!("Goodbye!");
                return code;
            }
            CommandResult::Error(msg) => {
                println!("sh: {}", msg);
            }
        }
    }
}

/// Print welcome message
fn print_welcome() {
    println!();
    println!("RustMicroOS Shell v{}", SHELL_VERSION);
    println!("Type 'help' for available commands");
    println!();
}

/// Print command prompt
fn print_prompt() {
    // 显示当前目录 + $ 提示符
    let mut buf = [0u8; 256];
    let ret = sys_getcwd(buf.as_mut_ptr(), buf.len());
    if ret > 0 {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(ret as usize);
        if let Ok(cwd) = core::str::from_utf8(&buf[..len]) {
            print!("[{}]$ ", cwd);
            return;
        }
    }
    // 如果获取目录失败，显示简单提示符
    print!("$ ");
}

/// Read a line of input
fn read_line(buf: &mut [u8]) -> usize {
    let mut pos = 0;
    
    loop {
        let mut c = [0u8; 1];
        let ret = sys_read(STDIN, &mut c);
        
        if ret <= 0 {
            return pos;
        }
        
        match c[0] {
            // Enter (CR or LF)
            b'\r' | b'\n' => {
                println!();
                return pos;
            }
            // Backspace or DEL
            0x08 | 0x7f => {
                if pos > 0 {
                    pos -= 1;
                    print!("\x08 \x08");
                }
            }
            // Ctrl+U - clear line
            0x15 => {
                while pos > 0 {
                    pos -= 1;
                    print!("\x08 \x08");
                }
            }
            // Ctrl+C - cancel input
            0x03 => {
                println!("^C");
                return 0;
            }
            // Ctrl+D - EOF
            0x04 => {
                if pos == 0 {
                    println!();
                    sys_exit(0);
                }
            }
            // Tab - ignore
            b'\t' => {}
            // Normal character
            _ => {
                if pos < buf.len() - 1 && c[0] >= 0x20 && c[0] < 0x7f {
                    buf[pos] = c[0];
                    pos += 1;
                    print!("{}", c[0] as char);
                }
            }
        }
    }
}

/// Parse command string into command name and argument list
fn parse_command(input: &str) -> (&str, [&str; MAX_ARGS]) {
    let mut args: [&str; MAX_ARGS] = [""; MAX_ARGS];
    let mut arg_count = 0;
    
    for part in input.split_whitespace() {
        if arg_count < MAX_ARGS {
            args[arg_count] = part;
            arg_count += 1;
        }
    }
    
    let cmd = if arg_count > 0 { args[0] } else { "" };
    (cmd, args)
}

/// Command execution result
enum CommandResult {
    Continue,
    Exit(i32),
    Error(&'static str),
}

/// Execute command
fn execute_command(cmd: &str, args: &[&str; MAX_ARGS]) -> CommandResult {
    match cmd {
        "help" | "?" => {
            cmd_help();
            CommandResult::Continue
        }
        "exit" | "quit" => {
            let code = if !args[1].is_empty() {
                args[1].parse().unwrap_or(0)
            } else {
                0
            };
            CommandResult::Exit(code)
        }
        "clear" | "cls" => {
            cmd_clear();
            CommandResult::Continue
        }
        "echo" => {
            cmd_echo(args);
            CommandResult::Continue
        }
        "version" => {
            cmd_version();
            CommandResult::Continue
        }
        "pid" => {
            println!("{}", sys_getpid());
            CommandResult::Continue
        }
        "pwd" => {
            cmd_pwd();
            CommandResult::Continue
        }
        "cd" => {
            cmd_cd(args);
            CommandResult::Continue
        }
        "ls" => {
            cmd_ls(args);
            CommandResult::Continue
        }
        "cat" => {
            cmd_cat(args);
            CommandResult::Continue
        }
        "mkdir" => {
            cmd_mkdir(args);
            CommandResult::Continue
        }
        "rm" => {
            cmd_rm(args);
            CommandResult::Continue
        }
        "touch" => {
            cmd_touch(args);
            CommandResult::Continue
        }
        // External commands
        _ => {
            execute_external(cmd, args)
        }
    }
}

/// help command
fn cmd_help() {
    println!("RustMicroOS Shell - Available Commands:");
    println!();
    println!("Built-in commands:");
    println!("  help, ?       Show this help message");
    println!("  exit [code]   Exit the shell");
    println!("  clear, cls    Clear the screen");
    println!("  echo <text>   Print text to stdout");
    println!("  version       Show shell version");
    println!("  pid           Show current process ID");
    println!("  pwd           Print working directory");
    println!("  cd <dir>      Change directory");
    println!("  ls [dir]      List directory contents");
    println!("  cat <file>    Display file contents");
    println!("  mkdir <dir>   Create directory");
    println!("  rm <file>     Remove file");
    println!("  touch <file>  Create empty file");
    println!();
    println!("External commands:");
    println!("  Any program in the filesystem can be executed");
    println!();
    println!("Tips:");
    println!("  - Use Backspace to delete characters");
    println!("  - Use Ctrl+U to clear the line");
    println!("  - Use Ctrl+C to cancel input");
    println!("  - Use Ctrl+D to exit");
}

/// clear command
fn cmd_clear() {
    print!("\x1b[2J\x1b[H");
}

/// echo command
fn cmd_echo(args: &[&str; MAX_ARGS]) {
    let mut first = true;
    for i in 1..MAX_ARGS {
        if args[i].is_empty() {
            break;
        }
        if !first {
            print!(" ");
        }
        print!("{}", args[i]);
        first = false;
    }
    println!();
}

/// version command
fn cmd_version() {
    println!("RustMicroOS Shell v{}", SHELL_VERSION);
    println!("Built for RISC-V 64-bit");
}

/// pwd command - print working directory
fn cmd_pwd() {
    let mut buf = [0u8; 256];
    let ret = sys_getcwd(buf.as_mut_ptr(), buf.len());
    if ret > 0 {
        let len = buf.iter().position(|&c| c == 0).unwrap_or(ret as usize);
        if let Ok(s) = core::str::from_utf8(&buf[..len]) {
            println!("{}", s);
        } else {
            println!("sh: pwd: invalid path");
        }
    } else {
        println!("sh: pwd: failed to get current directory");
    }
}

/// cd command - change directory
fn cmd_cd(args: &[&str; MAX_ARGS]) {
    let path = if args[1].is_empty() {
        "/"
    } else {
        args[1]
    };
    
    let ret = sys_chdir(path);
    if ret < 0 {
        println!("sh: cd: {}: No such directory", path);
    }
}

/// ls command - list directory
fn cmd_ls(args: &[&str; MAX_ARGS]) {
    let path = if args[1].is_empty() {
        "."
    } else {
        args[1]
    };
    
    // Use AT_FDCWD (-100) to indicate current directory
    let fd = if path == "." {
        (-100isize) as usize
    } else {
        // For other paths, we need to open the directory first
        // For now, just use AT_FDCWD and let kernel handle path resolution
        (-100isize) as usize
    };
    
    let mut entries = [DirEntry {
        ino: 0,
        file_type: 0,
        name_len: 0,
        name: [0u8; 254],
    }; 32];
    
    let ret = sys_getdents(fd, entries.as_mut_ptr() as *mut u8, 
                          entries.len() * core::mem::size_of::<DirEntry>());
    
    if ret < 0 {
        println!("sh: ls: cannot access '{}': No such file or directory", path);
        return;
    }
    
    let count = ret as usize;
    
    for i in 0..count {
        let entry = &entries[i];
        let name_len = entry.name_len as usize;
        if let Ok(name) = core::str::from_utf8(&entry.name[..name_len]) {
            let type_char = match entry.file_type {
                4 => 'd',  // Directory
                8 => '-',  // Regular file
                10 => 'l', // Symlink
                _ => '?',
            };
            println!("{} {}", type_char, name);
        }
    }
}

/// cat command - display file contents
fn cmd_cat(args: &[&str; MAX_ARGS]) {
    if args[1].is_empty() {
        println!("sh: cat: missing file operand");
        return;
    }
    
    let path = args[1];
    
    // Open file
    let fd = sys_open(path, 0); // O_RDONLY = 0
    if fd < 0 {
        println!("sh: cat: {}: No such file or directory", path);
        return;
    }
    
    // Read and print file contents
    let mut buf = [0u8; 512];
    loop {
        let n = sys_read(fd as usize, &mut buf);
        if n <= 0 {
            break;
        }
        // Write to stdout
        sys_write(1, &buf[..n as usize]);
    }
    
    sys_close(fd as usize);
}

/// mkdir command - create directory
fn cmd_mkdir(args: &[&str; MAX_ARGS]) {
    if args[1].is_empty() {
        println!("sh: mkdir: missing operand");
        return;
    }
    
    let path = args[1];
    let ret = sys_mkdir(path);
    if ret == -4 {
        // AlreadyExists
        println!("sh: mkdir: cannot create directory '{}': File exists", path);
    } else if ret == -3 {
        // NotFound (parent directory not found)
        println!("sh: mkdir: cannot create directory '{}': No such file or directory", path);
    } else if ret < 0 {
        println!("sh: mkdir: cannot create directory '{}': Error ({})", path, ret);
    }
}

/// rm command - remove file
fn cmd_rm(args: &[&str; MAX_ARGS]) {
    if args[1].is_empty() {
        println!("sh: rm: missing operand");
        return;
    }
    
    let path = args[1];
    let ret = sys_unlink(path);
    if ret < 0 {
        println!("sh: rm: cannot remove '{}': No such file or is a directory", path);
    }
}

/// touch command - create empty file
fn cmd_touch(args: &[&str; MAX_ARGS]) {
    if args[1].is_empty() {
        println!("sh: touch: missing operand");
        return;
    }
    
    let path = args[1];
    // 尝试打开文件，如果不存在则创建
    let fd = sys_open(path, 0x41); // O_CREAT | O_WRONLY = 0x41
    if fd >= 0 {
        sys_close(fd as usize);
    } else {
        println!("sh: touch: cannot touch '{}': Error", path);
    }
}

/// Execute external command
fn execute_external(cmd: &str, args: &[&str; MAX_ARGS]) -> CommandResult {
    let pid = sys_fork();
    
    if pid < 0 {
        return CommandResult::Error("fork failed");
    }
    
    if pid == 0 {
        // Child process: 构建 argv null-terminated C 字符串数组
        // argv[0] = cmd, argv[1..] = args (filtered non-empty)
        let mut arg_bufs: [[u8; 64]; 18] = [[0u8; 64]; 18]; // 最多 18 个参数
        let mut argv_ptrs: [*const u8; 19] = [core::ptr::null(); 19]; // 18 + NULL
        let mut argc = 0usize;
        
        // argv[0] = 程序名
        let cmd_bytes = cmd.as_bytes();
        let clen = cmd_bytes.len().min(63);
        arg_bufs[0][..clen].copy_from_slice(&cmd_bytes[..clen]);
        arg_bufs[0][clen] = 0;
        argv_ptrs[0] = arg_bufs[0].as_ptr();
        argc += 1;
        
        // argv[1..] = 非空参数
        for i in 0..MAX_ARGS {
            if args[i].is_empty() { break; }
            if argc >= 18 { break; }
            let a = args[i].as_bytes();
            let alen = a.len().min(63);
            arg_bufs[argc][..alen].copy_from_slice(&a[..alen]);
            arg_bufs[argc][alen] = 0;
            argv_ptrs[argc] = arg_bufs[argc].as_ptr();
            argc += 1;
        }
        argv_ptrs[argc] = core::ptr::null(); // NULL terminator
        
        // 先直接执行命令
        let ret = sys_execve(cmd, &argv_ptrs[..argc], &[]);
        
        if ret >= 0 {
            unreachable!()
        }
        
        // 如果失败，尝试在根目录查找
        let mut path_buf = [0u8; 64];
        path_buf[0] = b'/';
        let copy_len = cmd_bytes.len().min(62);
        path_buf[1..1+copy_len].copy_from_slice(&cmd_bytes[..copy_len]);
        
        if let Ok(full_path) = core::str::from_utf8(&path_buf[..1+copy_len]) {
            // 更新 argv[0] 为完整路径
            let flen = full_path.len().min(63);
            arg_bufs[0] = [0u8; 64];
            arg_bufs[0][..flen].copy_from_slice(&full_path.as_bytes()[..flen]);
            arg_bufs[0][flen] = 0;
            argv_ptrs[0] = arg_bufs[0].as_ptr();
            
            let ret = sys_execve(full_path, &argv_ptrs[..argc], &[]);
            if ret >= 0 {
                unreachable!()
            }
        }
        
        println!("sh: {}: command not found", cmd);
        sys_exit(127);
    }
    
    // Parent process: wait for child
    let mut exit_code: i32 = 0;
    let wait_ret = sys_waitpid(pid, &mut exit_code);
    
    if wait_ret < 0 {
        return CommandResult::Error("wait failed");
    }
    
    CommandResult::Continue
}
