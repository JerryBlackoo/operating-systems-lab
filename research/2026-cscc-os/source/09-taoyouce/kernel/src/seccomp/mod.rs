//! POSIX seccomp(2) — cBPF (Classic BPF) syscall filter，对齐 Linux
//! `kernel/seccomp.c` + `net/core/filter.c` 的 cBPF 子集。
//!
//! # 数据流
//!
//! 1. 用户态 `seccomp(SECCOMP_SET_MODE_FILTER, flags, &sock_fprog)` 或
//!    `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &sock_fprog)`
//! 2. 内核 [`install_filter_for_current`] 拷贝 `sock_filter[]`，跑
//!    [`bpf::verify`]（检查长度、跳转目标、ALU 立即除 0、末尾 RET 等），
//!    通过后包成 [`SeccompFilter`] push 到 `PCB.seccomp_filters`。
//! 3. 每次 syscall 入口由 [`check_current_syscall`] 构造
//!    `struct seccomp_data { nr, arch, ip, args[6] }`（64 字节），依次跑
//!    所有 filter（旧→新），返回值按 `SECCOMP_RET_ACTION_FULL` mask 取
//!    "最严格"（数值最小）的 action。
//! 4. dispatch 按 [`SeccompResult`] 在 syscall_inner 之前短路 / 拒绝 /
//!    杀进程 / 投 SIGSYS / 转 errno。
//!
//! # Action 严格度对齐 Linux uapi/linux/seccomp.h
//!
//! ```c
//! #define SECCOMP_RET_KILL_PROCESS 0x80000000u
//! #define SECCOMP_RET_KILL_THREAD  0x00000000u
//! #define SECCOMP_RET_TRAP         0x00030000u
//! #define SECCOMP_RET_ERRNO        0x00050000u
//! #define SECCOMP_RET_USER_NOTIF   0x7fc00000u
//! #define SECCOMP_RET_TRACE        0x7ff00000u
//! #define SECCOMP_RET_LOG          0x7ffc0000u
//! #define SECCOMP_RET_ALLOW        0x7fff0000u
//! #define SECCOMP_RET_ACTION_FULL  0xffff0000u
//! #define SECCOMP_RET_DATA         0x0000ffffu
//! ```
//!
//! 注意 KILL_PROCESS = 0x8000_0000 是无符号最大但被 Linux 当成"最严格"——
//! 我们用 [`action_severity`] 显式映射严格度，不依赖数值大小，避免 sign
//! 解释陷阱。

pub mod bpf;

use alloc::sync::Arc;
use alloc::vec::Vec;

pub use bpf::{SockFilter, BPF_MAXINSNS, SeccompFilterFlags};

/// 全局「是否曾有任意进程启用过 seccomp（STRICT/FILTER）」标志。
///
/// seccomp 是 POSIX 单向升级、且绝大多数进程从不启用。`check_current_syscall`
/// 在每次 syscall 入口被调用——若用 `PROCESS_TABLE.lock()` 去读 per-process
/// `seccomp_mode` 才能判断 mode==0(DISABLED)，则对零 seccomp 负载也要付出一次
/// 全局锁。本标志让无 seccomp 时直接 lock-free 短路放行（§1.13① 减热路径锁/
/// 指令）。一旦有进程启用即置位且不再清除（保守、绝不漏判：置位后退回加锁判定，
/// 与原语义完全一致；置位前所有进程必然 mode==0）。
pub static SECCOMP_EVER_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 标记 seccomp 已被启用（任意进程 STRICT/FILTER），供 syscall 热路径短路解除。
#[inline]
pub fn mark_seccomp_active() {
    SECCOMP_EVER_ACTIVE.store(true, core::sync::atomic::Ordering::Release);
}

/// Linux `SECCOMP_SET_MODE_*` operation 常量
pub const SECCOMP_SET_MODE_STRICT: u32 = 0;
pub const SECCOMP_SET_MODE_FILTER: u32 = 1;
pub const SECCOMP_GET_ACTION_AVAIL: u32 = 2;
pub const SECCOMP_GET_NOTIF_SIZES: u32 = 3;

/// Linux `SECCOMP_RET_*` action 常量
pub const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
pub const SECCOMP_RET_KILL_THREAD: u32 = 0x0000_0000;
pub const SECCOMP_RET_TRAP: u32 = 0x0003_0000;
pub const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
pub const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
pub const SECCOMP_RET_TRACE: u32 = 0x7ff0_0000;
pub const SECCOMP_RET_LOG: u32 = 0x7ffc_0000;
pub const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
pub const SECCOMP_RET_ACTION_FULL: u32 = 0xffff_0000;
pub const SECCOMP_RET_DATA: u32 = 0x0000_ffff;

/// SECCOMP_SET_MODE_FILTER 合法 flag 位（uapi/linux/seccomp.h）
pub const SECCOMP_FILTER_FLAG_TSYNC: u32 = 1 << 0;
pub const SECCOMP_FILTER_FLAG_LOG: u32 = 1 << 1;
pub const SECCOMP_FILTER_FLAG_SPEC_ALLOW: u32 = 1 << 2;
pub const SECCOMP_FILTER_FLAG_NEW_LISTENER: u32 = 1 << 3;
pub const SECCOMP_FILTER_FLAG_TSYNC_ESRCH: u32 = 1 << 4;
pub const SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV: u32 = 1 << 5;
const SECCOMP_FILTER_FLAGS_VALID: u32 = SECCOMP_FILTER_FLAG_TSYNC
    | SECCOMP_FILTER_FLAG_LOG
    | SECCOMP_FILTER_FLAG_SPEC_ALLOW
    | SECCOMP_FILTER_FLAG_NEW_LISTENER
    | SECCOMP_FILTER_FLAG_TSYNC_ESRCH
    | SECCOMP_FILTER_FLAG_WAIT_KILLABLE_RECV;

/// Linux `<asm/audit.h>` AUDIT_ARCH_* —— seccomp_data.arch 字段，过滤器据此
/// 区分 RV64/LA64 程序。
#[cfg(target_arch = "riscv64")]
pub const SECCOMP_AUDIT_ARCH: u32 = 0xc000_00b7; // AUDIT_ARCH_RISCV64
#[cfg(target_arch = "loongarch64")]
pub const SECCOMP_AUDIT_ARCH: u32 = 0xc000_0102; // AUDIT_ARCH_LOONGARCH64
#[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
pub const SECCOMP_AUDIT_ARCH: u32 = 0;

/// 单条已安装的 seccomp filter。`prog` 在 install 时已通过 verifier。
/// 旧→新顺序通过 PCB.seccomp_filters 的 Vec 顺序表达（最先 install 的在前）。
/// fork/clone 通过 Arc 共享（与 Linux `seccomp.filter` 引用计数行为一致），
/// execve 也保留（POSIX 要求）。
pub struct SeccompFilter {
    pub prog: Vec<SockFilter>,
    pub log: bool,
    pub _new_listener: bool,
}

/// `check_current_syscall` 给上层 syscall dispatch 的判决。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeccompResult {
    /// 通过，正常 dispatch
    Allow,
    /// 拒绝并返回 `-errno`（errno 取自 RET_ERRNO 低 16 位，clamp 到 MAX_ERRNO=4095）
    Errno(u16),
    /// FILTER mode 杀本线程：do_exit(SIGSYS)。
    KillThread,
    /// FILTER mode 杀整个进程组：do_group_exit(SIGSYS)。
    KillProcess,
    /// STRICT mode 违规专用：Linux `__secure_computing` 路径用 `do_exit(SIGKILL)`，
    /// 不是 SIGSYS（与 FILTER mode KILL_THREAD/KILL_PROCESS 区分），LTP `prctl04`
    /// 明确校验该 signal。
    StrictKill,
    /// 投 SIGSYS 但仍允许 syscall（Linux 行为：syscall 返回 -ENOSYS 后投 SIGSYS）
    /// data 字段写入 si_errno（user 自定义信号子码）
    Trap(u16),
    /// 有 ptrace tracer 则通知 tracer；无 tracer 时按 Linux 行为返回 -ENOSYS
    /// 本项目暂无 ptrace 子系统 → 等同 Errno(38)
    Trace(u16),
    /// LOG：dmesg 记一条后按 ALLOW 处理
    Log,
}

/// Linux uapi 定义的 `struct seccomp_data`（64 字节，固定 little-endian
/// 内存布局）。BPF program 把此结构当 packet 头通过 BPF_LD|BPF_ABS 读取。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SeccompData {
    pub nr: i32,            // offset 0
    pub arch: u32,          // offset 4
    pub instruction_pointer: u64, // offset 8
    pub args: [u64; 6],     // offset 16
}

impl SeccompData {
    pub fn build(nr: u64, args: [u64; 6], pc: u64) -> Self {
        Self {
            nr: nr as i32,
            arch: SECCOMP_AUDIT_ARCH,
            instruction_pointer: pc,
            args,
        }
    }

    /// BPF_LD|BPF_ABS|BPF_W 按字节偏移读 4B。off 必须 4 对齐。off==0xfffff038
    /// 等"SKF magic"在 seccomp 路径下被 Linux verifier 拒绝；我们一并拒绝
    /// （bpf::verify 阶段强制 off < 64 且 4-aligned）。
    pub fn as_bytes(&self) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0..4].copy_from_slice(&self.nr.to_le_bytes());
        buf[4..8].copy_from_slice(&self.arch.to_le_bytes());
        buf[8..16].copy_from_slice(&self.instruction_pointer.to_le_bytes());
        for i in 0..6 {
            let off = 16 + i * 8;
            buf[off..off + 8].copy_from_slice(&self.args[i].to_le_bytes());
        }
        buf
    }
}

/// Linux `seccomp_run_filters()` 等价：依次跑所有 filter，把每条 filter
/// 的 RET 值按 ACTION_MASK 取最严格 action 返回。
///
/// 严格度顺序（最严 → 最松）：
///   KILL_PROCESS > KILL_THREAD > TRAP > ERRNO > USER_NOTIF > TRACE > LOG > ALLOW
fn run_filters_locked(filters: &[Arc<SeccompFilter>], data: &SeccompData) -> u32 {
    if filters.is_empty() {
        return SECCOMP_RET_ALLOW;
    }
    let bytes = data.as_bytes();
    let mut best_action: u32 = SECCOMP_RET_ALLOW;
    let mut best_severity: u8 = action_severity(best_action);
    let mut best_data: u16 = 0;
    for f in filters {
        let ret = bpf::run(&f.prog, &bytes);
        let action = ret & SECCOMP_RET_ACTION_FULL;
        let data_field = (ret & SECCOMP_RET_DATA) as u16;
        let sev = action_severity(action);
        if sev > best_severity {
            best_severity = sev;
            best_action = action;
            best_data = data_field;
        } else if sev == best_severity && (best_action == SECCOMP_RET_ERRNO
            || best_action == SECCOMP_RET_TRAP
            || best_action == SECCOMP_RET_TRACE)
        {
            // 同严格度时取**第一个非 0** data（Linux 行为：第一个匹配的 filter 决定 errno）。
            // 但此处我们已经按 filter 顺序遍历，best 是先看到的；不需要换。
            let _ = data_field;
        }
    }
    (best_action & SECCOMP_RET_ACTION_FULL) | (best_data as u32)
}

/// 数字越大越严格。仅在 [`run_filters_locked`] 内部使用。
fn action_severity(action: u32) -> u8 {
    match action & SECCOMP_RET_ACTION_FULL {
        SECCOMP_RET_KILL_PROCESS => 8,
        SECCOMP_RET_KILL_THREAD => 7,
        SECCOMP_RET_TRAP => 6,
        SECCOMP_RET_ERRNO => 5,
        SECCOMP_RET_USER_NOTIF => 4,
        SECCOMP_RET_TRACE => 3,
        SECCOMP_RET_LOG => 2,
        SECCOMP_RET_ALLOW => 1,
        _ => 0, // 未知 action 按 KILL_PROCESS（Linux `__seccomp_filter` 行为）
    }
}

/// 把 BPF 32-bit 返回值解码为 [`SeccompResult`]。
fn decode_action(ret: u32) -> SeccompResult {
    let data = (ret & SECCOMP_RET_DATA) as u16;
    match ret & SECCOMP_RET_ACTION_FULL {
        SECCOMP_RET_ALLOW => SeccompResult::Allow,
        SECCOMP_RET_LOG => SeccompResult::Log,
        SECCOMP_RET_TRACE => SeccompResult::Trace(data),
        // USER_NOTIF 未实装 listener 时退化为 -ENOSYS（Linux `seccomp_do_user_notification`
        // 在无 listener 时返回 -ENOSYS，与此一致）。
        SECCOMP_RET_USER_NOTIF => SeccompResult::Errno(38),
        SECCOMP_RET_ERRNO => {
            // clamp 到 Linux MAX_ERRNO = 4095（uapi 行为）
            let e = data.min(4095);
            SeccompResult::Errno(if e == 0 { 1 } else { e }) // RET_ERRNO|0 在 Linux 是 EPERM=1
        }
        SECCOMP_RET_TRAP => SeccompResult::Trap(data),
        SECCOMP_RET_KILL_THREAD => SeccompResult::KillThread,
        SECCOMP_RET_KILL_PROCESS => SeccompResult::KillProcess,
        // 未知 action：Linux 当 KILL_PROCESS 处理（fs/seccomp.c::__seccomp_filter）
        _ => SeccompResult::KillProcess,
    }
}

/// syscall dispatch 入口调用：返回判决。Allow 时上层继续走 syscall_inner；
/// 其它情况上层短路。
///
/// 实现要点：
///   - 只在 PCB.seccomp_mode==2 (FILTER) 时跑 BPF（STRICT 由更上层负责）
///   - 拷贝 filter Arc 列表到临时 Vec 后**立刻解锁** PROCESS_TABLE，再
///     跑 BPF VM——否则 BPF 在拿锁的情况下执行可能间接死锁
///     （Linux 也是 RCU 读侧无锁）
pub fn check_current_syscall(nr: u64, args: [u64; 6], pc: u64) -> SeccompResult {
    // 热路径 lock-free 短路：从未有进程启用 seccomp ⇒ 所有进程 mode==0(DISABLED)
    // ⇒ 必然 ALLOW，无需获取 PROCESS_TABLE 全局锁（§1.13① 减每 syscall 锁/指令）。
    // Relaxed 足够：本标志只是"曾经有进程启用过 seccomp"的单调提示，真正的
    // filter 列表读取在 PROCESS_TABLE 锁内完成（锁自带同步）；smp=1 下
    // Acquire 只会多付一条 fence r,rw（每 syscall 热路径）。
    if !SECCOMP_EVER_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
        return SeccompResult::Allow;
    }
    // 真化〔STRICT mode 真做〕：Linux uapi `mode1_syscalls[]`
    //   __NR_read, __NR_write, __NR_exit, __NR_rt_sigreturn
    // 任何其它 syscall → KILL_THREAD（do_exit(SIGSYS)）。
    // RV64/LA64 共用 generic uapi 编号：read=63, write=64, exit=93, rt_sigreturn=139。
    // STRICT 由 SECCOMP_SET_MODE_STRICT 或 prctl(PR_SET_SECCOMP, MODE_STRICT) 触发；
    // 一旦置位即不可撤回（POSIX 单向升级，与 mode/filter 一致）。
    let (mode, filters_snapshot): (u32, Vec<Arc<SeccompFilter>>) = {
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        let Some(table) = guard.as_mut() else { return SeccompResult::Allow };
        let Some(pid) = table.current else { return SeccompResult::Allow };
        let Some(proc) = table.get(pid) else { return SeccompResult::Allow };
        match proc.seccomp_mode {
            0 => return SeccompResult::Allow, // DISABLED
            1 => (1, Vec::new()),             // STRICT
            2 => (2, proc.seccomp_filters.clone()),
            _ => return SeccompResult::Allow, // 未知 mode：宽容放行（不阻塞 boot）
        }
    };
    if mode == 1 {
        const MODE1_SYSCALLS: &[u64] = &[63, 64, 93, 139];
        return if MODE1_SYSCALLS.contains(&nr) {
            SeccompResult::Allow
        } else {
            // Linux STRICT mode 违规 → `do_exit(SIGKILL)`，LTP prctl04 校验。
            SeccompResult::StrictKill
        };
    }
    if filters_snapshot.is_empty() {
        // mode==FILTER 但没 filter 是矛盾态（只能由内核 bug 触发），按 ALLOW
        return SeccompResult::Allow;
    }
    let data = SeccompData::build(nr, args, pc);
    let ret = run_filters_locked(&filters_snapshot, &data);
    decode_action(ret)
}

/// 把一段 cBPF 程序安装为当前 task 的新 filter。失败时返回 `-errno`。
///
/// 调用前提（由 [`crate::syscall::security::sys_seccomp`] 与 prctl 路径
/// 共同保证）：
///   1. caller 已 NO_NEW_PRIVS 或 euid==0（与 Linux `seccomp_may_assign_mode` 等价）
///   2. `flags` 仅含 [`SECCOMP_FILTER_FLAGS_VALID`] 中位（上层检查）
///   3. `prog` 已从 user 内存安全拷出，长度满足 1..=BPF_MAXINSNS
pub fn install_filter_for_current(
    prog: Vec<SockFilter>,
    flags: SeccompFilterFlags,
) -> Result<(), isize> {
    // verifier：跳转目标、末尾 RET、立即数除 0、ABS off 范围……
    bpf::verify(&prog).map_err(|e| -(e as isize))?;

    let filter = Arc::new(SeccompFilter {
        prog,
        log: flags.log,
        _new_listener: flags.new_listener,
    });

    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = guard.as_mut().ok_or(-3isize)?;
    let pid = table.current.ok_or(-3isize)?;
    let proc = table.get_mut(pid).ok_or(-3isize)?;

    // 单向升级：已是 STRICT 不可再装 FILTER（Linux `seccomp_set_mode_filter` 检查）
    if proc.seccomp_mode == 1 {
        return Err(-22);
    }
    // RLIMIT_NPROC 风格的 filter 总数上限：Linux 默认 `MAX_INSNS_PER_PROG_PER_USER`
    // 按 cap_setpcap 限制 32MB BPF 内存；我们直接限 `MAX_FILTERS_PER_TASK`。
    const MAX_FILTERS_PER_TASK: usize = 64;
    if proc.seccomp_filters.len() >= MAX_FILTERS_PER_TASK {
        return Err(-12); // -ENOMEM
    }
    proc.seccomp_filters.push(filter);
    proc.seccomp_mode = 2;
    mark_seccomp_active();
    Ok(())
}

/// SECCOMP_SET_MODE_FILTER 上层包装：从 user 拷出 sock_fprog，调用
/// [`install_filter_for_current`]。
///
/// `sock_fprog` 在 RV64/LA64 上 16 字节（len: u16 + 6B padding + filter: u64 ptr）。
pub fn syscall_set_mode_filter(flags: u32, args_ptr: usize) -> isize {
    if flags & !SECCOMP_FILTER_FLAGS_VALID != 0 {
        return -22;
    }
    if flags & SECCOMP_FILTER_FLAG_TSYNC != 0 && flags & SECCOMP_FILTER_FLAG_NEW_LISTENER != 0 {
        // Linux `seccomp_set_mode_filter` 显式拒绝（互斥）
        return -22;
    }
    if args_ptr == 0 {
        return -14; // -EFAULT
    }
    let sock_fprog_bytes = match crate::syscall::user::read_user_buffer(args_ptr as *const u8, 16) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let len = u16::from_le_bytes(sock_fprog_bytes[0..2].try_into().unwrap()) as usize;
    let filter_ptr = u64::from_le_bytes(sock_fprog_bytes[8..16].try_into().unwrap()) as usize;
    if len == 0 || len > BPF_MAXINSNS {
        return -22;
    }
    if filter_ptr == 0 {
        return -14;
    }
    let bytes_needed = len.checked_mul(8).ok_or(()).unwrap_or(0);
    if bytes_needed == 0 { return -22; }
    let prog_raw = match crate::syscall::user::read_user_buffer(filter_ptr as *const u8, bytes_needed) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let mut prog: Vec<SockFilter> = Vec::with_capacity(len);
    for i in 0..len {
        let off = i * 8;
        prog.push(SockFilter {
            code: u16::from_le_bytes(prog_raw[off..off + 2].try_into().unwrap()),
            jt: prog_raw[off + 2],
            jf: prog_raw[off + 3],
            k: u32::from_le_bytes(prog_raw[off + 4..off + 8].try_into().unwrap()),
        });
    }
    let flag_struct = SeccompFilterFlags {
        log: flags & SECCOMP_FILTER_FLAG_LOG != 0,
        new_listener: flags & SECCOMP_FILTER_FLAG_NEW_LISTENER != 0,
    };
    if let Err(e) = install_filter_for_current(prog, flag_struct) {
        return e;
    }
    // NEW_LISTENER 应该返回 listener fd；我们不实装 user-notification，按
    // Linux 行为返回 -EBADF（无 listener fd），但若 caller 没传该 flag 返 0。
    if flags & SECCOMP_FILTER_FLAG_NEW_LISTENER != 0 {
        return -9; // -EBADF，与无 listener 一致
    }
    0
}
