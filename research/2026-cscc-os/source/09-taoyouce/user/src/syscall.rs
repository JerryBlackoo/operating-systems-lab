//! 系统调用封装

use core::arch::asm;

/// 系统调用号
pub const SYS_EXIT: usize = 93;
pub const SYS_WRITE: usize = 64;
pub const SYS_READ: usize = 63;
pub const SYS_FORK: usize = 220;
pub const SYS_EXEC: usize = 221;
pub const SYS_WAITPID: usize = 260;
pub const SYS_GETPID: usize = 172;
pub const SYS_YIELD: usize = 124;
pub const SYS_MMAP: usize = 222;
pub const SYS_MUNMAP: usize = 215;

/// 系统调用（0 参数）
fn syscall0(id: usize) -> isize {
    let ret: isize;
    unsafe {
        #[cfg(target_arch = "riscv64")]
        asm!("ecall", inlateout("a0") 0usize => ret, in("a7") id);
        #[cfg(target_arch = "loongarch64")]
        asm!("syscall 0", inlateout("$a0") 0usize => ret, in("$a7") id);
    }
    ret
}

/// 系统调用（1 参数）
fn syscall1(id: usize, arg0: usize) -> isize {
    let ret: isize;
    unsafe {
        #[cfg(target_arch = "riscv64")]
        asm!("ecall", inlateout("a0") arg0 => ret, in("a7") id);
        #[cfg(target_arch = "loongarch64")]
        asm!("syscall 0", inlateout("$a0") arg0 => ret, in("$a7") id);
    }
    ret
}

/// 系统调用（3 参数）
fn syscall3(id: usize, arg0: usize, arg1: usize, arg2: usize) -> isize {
    let ret: isize;
    unsafe {
        #[cfg(target_arch = "riscv64")]
        asm!("ecall", inlateout("a0") arg0 => ret, in("a1") arg1, in("a2") arg2, in("a7") id);
        #[cfg(target_arch = "loongarch64")]
        asm!("syscall 0", inlateout("$a0") arg0 => ret, in("$a1") arg1, in("$a2") arg2, in("$a7") id);
    }
    ret
}

/// 退出进程
pub fn sys_exit(exit_code: i32) -> ! {
    syscall1(SYS_EXIT, exit_code as usize);
    unreachable!()
}

/// 写入文件描述符
pub fn sys_write(fd: usize, buffer: &[u8]) -> isize {
    syscall3(SYS_WRITE, fd, buffer.as_ptr() as usize, buffer.len())
}

/// 读取文件描述符
pub fn sys_read(fd: usize, buffer: &mut [u8]) -> isize {
    syscall3(SYS_READ, fd, buffer.as_mut_ptr() as usize, buffer.len())
}

/// 创建子进程（clone with SIGCHLD, child_stack=0 → classic fork）
pub fn sys_fork() -> isize {
    const SIGCHLD: usize = 17;
    syscall3(SYS_FORK, SIGCHLD, 0, 0)
}

/// 执行程序（无参数版本，向后兼容）
pub fn sys_exec(path: &str) -> isize {
    sys_execve(path, &[], &[])
}

/// execve — 执行程序，传递 argv 和 envp
/// argv/envp: null-terminated C string 指针数组
pub fn sys_execve(path: &str, argv: &[*const u8], envp: &[*const u8]) -> isize {
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    
    // 构建 null 结尾的指针数组（栈上最多 32 个参数）
    let mut argv_ptrs = [0usize; 33];
    let argc = argv.len().min(32);
    for i in 0..argc {
        argv_ptrs[i] = argv[i] as usize;
    }
    argv_ptrs[argc] = 0; // NULL terminator
    
    let mut envp_ptrs = [0usize; 33];
    let envc = envp.len().min(32);
    for i in 0..envc {
        envp_ptrs[i] = envp[i] as usize;
    }
    envp_ptrs[envc] = 0;
    
    syscall3(SYS_EXEC, buf.as_ptr() as usize, argv_ptrs.as_ptr() as usize, envp_ptrs.as_ptr() as usize)
}

/// 等待子进程（阻塞）
///
/// 必须用 syscall4 显式传 rusage=0：
/// 内核 `wait()` 从 a3 读 `rusage_ptr` 并据此写 144 字节 zeroed `struct rusage`
/// 到用户空间。如果 a3 是 stale garbage（syscall3 不清 a3），写入会落到随机
/// 用户地址甚至触发 lazy/CoW fault-in，破坏 init 栈/堆/text，曾导致 init pid=1
/// 在 era=0x9d8 等异常地址段错误然后 `[kernel] Init process exited` 关机。
pub fn sys_waitpid(pid: isize, exit_code: *mut i32) -> isize {
    syscall4(SYS_WAITPID, pid as usize, exit_code as usize, 0, 0)
}

/// 等待子进程（非阻塞，WNOHANG）
/// 返回: >0 = 已退出的子进程 pid, 0 = 没有已退出子进程, <0 = 错误
pub fn sys_waitpid_nohang(pid: isize, exit_code: *mut i32) -> isize {
    const WNOHANG: usize = 1;
    syscall4(SYS_WAITPID, pid as usize, exit_code as usize, WNOHANG, 0)
}

/// 获取进程 ID
pub fn sys_getpid() -> isize {
    syscall0(SYS_GETPID)
}

/// 让出 CPU
pub fn sys_yield() -> isize {
    syscall0(SYS_YIELD)
}

/// sbrk 系统调用号
pub const SYS_SBRK: usize = 214;

/// 调整堆大小（sbrk 语义包装 brk 系统调用）
/// 
/// 参数:
/// - increment: 堆大小增量（正数增加，负数减少，0 返回当前堆顶）
/// 
/// 返回:
/// - 成功: 返回之前的堆顶地址
/// - 失败: 返回 -1
pub fn sys_sbrk(increment: isize) -> isize {
    // brk(0) 返回当前堆顶
    let old_brk = syscall1(SYS_SBRK, 0);
    if old_brk < 0 {
        return -1;
    }
    if increment == 0 {
        return old_brk;
    }
    // 计算新的绝对地址并调用 brk(new_addr)
    let new_addr = (old_brk as usize).wrapping_add(increment as usize);
    let result = syscall1(SYS_SBRK, new_addr);
    if result as usize != new_addr {
        return -1; // brk 失败
    }
    old_brk // 返回旧堆顶（sbrk 约定）
}

// ============ 文件系统系统调用 ============

pub const SYS_OPEN: usize = 56;
pub const SYS_CLOSE: usize = 57;
pub const SYS_MKDIRAT: usize = 34;
pub const SYS_UNLINKAT: usize = 35;
pub const SYS_NEWFSTATAT: usize = 79;
pub const SYS_GETCWD: usize = 17;
pub const SYS_CHDIR: usize = 49;
pub const SYS_GETDENTS: usize = 61;
pub const SYS_DUP: usize = 23;
pub const SYS_DUP3: usize = 24;
pub const SYS_PIPE: usize = 59;
pub const SYS_LSEEK: usize = 62;

/// 系统调用（2 参数）
fn syscall2(id: usize, arg0: usize, arg1: usize) -> isize {
    let ret: isize;
    unsafe {
        #[cfg(target_arch = "riscv64")]
        asm!("ecall", inlateout("a0") arg0 => ret, in("a1") arg1, in("a7") id);
        #[cfg(target_arch = "loongarch64")]
        asm!("syscall 0", inlateout("$a0") arg0 => ret, in("$a1") arg1, in("$a7") id);
    }
    ret
}

/// 系统调用（4 参数）
pub fn syscall4(id: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize) -> isize {
    let ret: isize;
    unsafe {
        #[cfg(target_arch = "riscv64")]
        asm!("ecall", inlateout("a0") arg0 => ret, in("a1") arg1, in("a2") arg2, in("a3") arg3, in("a7") id);
        #[cfg(target_arch = "loongarch64")]
        asm!("syscall 0", inlateout("$a0") arg0 => ret, in("$a1") arg1, in("$a2") arg2, in("$a3") arg3, in("$a7") id);
    }
    ret
}

/// 系统调用（6 参数）
fn syscall6(id: usize, arg0: usize, arg1: usize, arg2: usize, arg3: usize, arg4: usize, arg5: usize) -> isize {
    let ret: isize;
    unsafe {
        #[cfg(target_arch = "riscv64")]
        asm!("ecall", inlateout("a0") arg0 => ret, in("a1") arg1, in("a2") arg2, in("a3") arg3, in("a4") arg4, in("a5") arg5, in("a7") id);
        #[cfg(target_arch = "loongarch64")]
        asm!("syscall 0", inlateout("$a0") arg0 => ret, in("$a1") arg1, in("$a2") arg2, in("$a3") arg3, in("$a4") arg4, in("$a5") arg5, in("$a7") id);
    }
    ret
}

/// 打开文件
pub fn sys_open(path: &str, flags: u32) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    // 创建 null-terminated C 字符串（Linux openat 标准）
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    syscall4(SYS_OPEN, AT_FDCWD, buf.as_ptr() as usize, flags as usize, 0)
}

/// 打开文件（带 mode 参数，用于 O_CREAT）
pub fn sys_open_with_mode(path: &str, flags: u32, mode: u32) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    syscall4(SYS_OPEN, AT_FDCWD, buf.as_ptr() as usize, flags as usize, mode as usize)
}

/// 关闭文件
pub fn sys_close(fd: usize) -> isize {
    syscall1(SYS_CLOSE, fd)
}

/// 创建目录 — 通过 mkdirat(AT_FDCWD, path, 0o755) 实现
pub fn sys_mkdir(path: &str) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    syscall3(SYS_MKDIRAT, AT_FDCWD, buf.as_ptr() as usize, 0o755)
}

/// 删除文件 — unlinkat(AT_FDCWD, path, 0)。generic uapi 没有 legacy unlink。
pub fn sys_unlink(path: &str) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    syscall3(SYS_UNLINKAT, AT_FDCWD, buf.as_ptr() as usize, 0)
}

/// 创建符号链接: symlinkat(target, AT_FDCWD, linkpath)
pub fn sys_symlink(target: &str, linkpath: &str) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    let mut tbuf = [0u8; 256];
    let tlen = target.len().min(255);
    tbuf[..tlen].copy_from_slice(&target.as_bytes()[..tlen]);
    tbuf[tlen] = 0;
    let mut lbuf = [0u8; 256];
    let llen = linkpath.len().min(255);
    lbuf[..llen].copy_from_slice(&linkpath.as_bytes()[..llen]);
    lbuf[llen] = 0;
    // symlinkat(target, newdirfd, linkpath)
    syscall3(36, tbuf.as_ptr() as usize, AT_FDCWD, lbuf.as_ptr() as usize)
}

/// 删除目录 — unlinkat(AT_FDCWD, path, AT_REMOVEDIR)。generic uapi 没有 legacy rmdir。
pub fn sys_rmdir(path: &str) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    const AT_REMOVEDIR: usize = 0x200;
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    syscall3(SYS_UNLINKAT, AT_FDCWD, buf.as_ptr() as usize, AT_REMOVEDIR)
}

/// Linux generic `struct stat`（riscv64/loongarch64 均为 128 字节布局）
#[repr(C)]
#[derive(Default)]
struct LinuxStat {
    st_dev: u64,
    st_ino: u64,
    st_mode: u32,
    st_nlink: u32,
    st_uid: u32,
    st_gid: u32,
    st_rdev: u64,
    __pad: u64,
    st_size: i64,
    st_blksize: i32,
    __pad2: i32,
    st_blocks: i64,
    st_atime_sec: i64,
    st_atime_nsec: i64,
    st_mtime_sec: i64,
    st_mtime_nsec: i64,
    st_ctime_sec: i64,
    st_ctime_nsec: i64,
    __unused: [u32; 2],
}

/// 获取文件状态 — newfstatat(AT_FDCWD, path, statbuf, 0)。generic uapi 没有 legacy stat。
pub fn sys_stat(path: &str, stat: &mut crate::fs::FileStat) -> isize {
    const AT_FDCWD: usize = (-100isize) as usize;
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    let mut ks = LinuxStat::default();
    let ret = syscall4(
        SYS_NEWFSTATAT,
        AT_FDCWD,
        buf.as_ptr() as usize,
        &mut ks as *mut _ as usize,
        0,
    );
    if ret == 0 {
        stat.dev = ks.st_dev;
        stat.inode = ks.st_ino;
        stat.mode = ks.st_mode;
        stat.nlink = ks.st_nlink;
        stat.uid = ks.st_uid;
        stat.gid = ks.st_gid;
        stat.size = ks.st_size as u64;
        stat.atime = ks.st_atime_sec as u64;
        stat.mtime = ks.st_mtime_sec as u64;
        stat.ctime = ks.st_ctime_sec as u64;
    }
    ret
}

/// 获取当前工作目录
pub fn sys_getcwd(buf: &mut [u8]) -> isize {
    syscall2(SYS_GETCWD, buf.as_mut_ptr() as usize, buf.len())
}

/// 改变当前工作目录
pub fn sys_chdir(path: &str) -> isize {
    let mut buf = [0u8; 256];
    let len = path.len().min(255);
    buf[..len].copy_from_slice(&path.as_bytes()[..len]);
    buf[len] = 0;
    syscall2(SYS_CHDIR, buf.as_ptr() as usize, len)
}

/// 读取目录项
pub fn sys_getdents(fd: usize, entries: &mut [crate::fs::DirEntry]) -> isize {
    syscall3(SYS_GETDENTS, fd, entries.as_mut_ptr() as usize, entries.len())
}

/// 复制文件描述符
pub fn sys_dup(fd: usize) -> isize {
    syscall1(SYS_DUP, fd)
}

/// 文件偏移
pub fn sys_lseek(fd: usize, offset: isize, whence: usize) -> isize {
    syscall3(SYS_LSEEK, fd, offset as usize, whence)
}

/// 创建管道
/// 
/// 参数:
/// - pipe_fd: 长度为 2 的数组，pipe_fd[0] 为读端，pipe_fd[1] 为写端
/// 
/// 返回:
/// - 成功: 0
/// - 失败: -1
pub fn sys_pipe(pipe_fd: &mut [usize; 2]) -> isize {
    syscall1(SYS_PIPE, pipe_fd.as_mut_ptr() as usize)
}

// ============ 能力系统系统调用 ============

pub const SYS_CAP_CREATE: usize = 500;
pub const SYS_CAP_DERIVE: usize = 501;
pub const SYS_CAP_REVOKE: usize = 502;
pub const SYS_CAP_CHECK: usize = 503;
pub const SYS_CAP_INFO: usize = 504;
pub const SYS_CAP_READ: usize = 505;
pub const SYS_CAP_WRITE: usize = 506;

/// 创建能力
pub fn sys_cap_create(cap_type: u32, addr: usize, size: usize, rights: u32) -> isize {
    syscall4(SYS_CAP_CREATE, cap_type as usize, addr, size, rights as usize)
}

/// 派生能力
pub fn sys_cap_derive(parent: u64, new_rights: u32) -> isize {
    syscall2(SYS_CAP_DERIVE, parent as usize, new_rights as usize)
}

/// 撤销能力
pub fn sys_cap_revoke(handle: u64) -> isize {
    syscall1(SYS_CAP_REVOKE, handle as usize)
}

/// 检查能力权限
pub fn sys_cap_check(handle: u64, required: u32) -> isize {
    syscall2(SYS_CAP_CHECK, handle as usize, required as usize)
}

/// 获取能力信息
pub fn sys_cap_info(handle: u64, info: &mut crate::capability::CapInfo) -> isize {
    syscall2(SYS_CAP_INFO, handle as usize, info as *mut _ as usize)
}

/// 通过能力读取
pub fn sys_cap_read(handle: u64, offset: usize, buf: *mut u8, len: usize) -> isize {
    syscall4(SYS_CAP_READ, handle as usize, offset, buf as usize, len)
}

/// 通过能力写入
pub fn sys_cap_write(handle: u64, offset: usize, buf: *const u8, len: usize) -> isize {
    syscall4(SYS_CAP_WRITE, handle as usize, offset, buf as usize, len)
}

// ============ io_uring 系统调用 ============

pub const SYS_IO_URING_SETUP: usize = 425;
pub const SYS_IO_URING_ENTER: usize = 426;
pub const SYS_IO_URING_REGISTER: usize = 427;

/// 创建 io_uring 实例
pub fn sys_io_uring_setup(entries: u32, params: &mut crate::io_uring::IoUringParams) -> isize {
    syscall2(SYS_IO_URING_SETUP, entries as usize, params as *mut _ as usize)
}

/// 提交和等待 io_uring 请求
pub fn sys_io_uring_enter(
    fd: i32,
    sqes: *const crate::io_uring::IoUringSqe,
    to_submit: u32,
    cqes: *mut crate::io_uring::IoUringCqe,
    cqe_size: u32,
    min_complete: u32,
) -> isize {
    syscall6(
        SYS_IO_URING_ENTER,
        fd as usize,
        sqes as usize,
        to_submit as usize,
        cqes as usize,
        cqe_size as usize,
        min_complete as usize,
    )
}

// ============ 系统信息系统调用 ============

pub const SYS_SYSINFO: usize = 179;
pub const SYS_GETTIME: usize = 169;

/// 系统信息结构
#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct SysInfo {
    pub uptime: u64,
    pub total_ram: u64,
    pub free_ram: u64,
    pub procs: u32,
    pub cpu_count: u32,
}

/// 获取系统信息
pub fn sys_sysinfo(info: &mut SysInfo) -> isize {
    syscall1(SYS_SYSINFO, info as *mut _ as usize)
}

/// 获取时间（纳秒）
pub fn sys_gettime() -> isize {
    syscall0(SYS_GETTIME)
}

// ============ 信号系统调用 ============

pub const SYS_KILL: usize = 129;

/// 发送信号给进程
/// - pid > 0: 发送给指定进程
/// - pid == 0: 发送给同组所有进程
/// - pid == -1: 发送给所有进程
pub fn sys_kill(pid: isize, sig: i32) -> isize {
    syscall2(SYS_KILL, pid as usize, sig as usize)
}

// ============ 进程组 / 定时 ============

pub const SYS_NANOSLEEP: usize = 101;
pub const SYS_SETPGID: usize = 154;

/// 设置进程组 ID (setpgid)。pid/pgid 为 0 表示当前进程。
pub fn sys_setpgid(pid: isize, pgid: isize) -> isize {
    syscall2(SYS_SETPGID, pid as usize, pgid as usize)
}

/// 休眠指定秒数 (nanosleep)。被信号打断时返回 -EINTR。
pub fn sys_nanosleep_secs(secs: i64) -> isize {
    // struct timespec { tv_sec: i64, tv_nsec: i64 }
    let ts: [i64; 2] = [secs, 0];
    syscall2(SYS_NANOSLEEP, ts.as_ptr() as usize, 0)
}
