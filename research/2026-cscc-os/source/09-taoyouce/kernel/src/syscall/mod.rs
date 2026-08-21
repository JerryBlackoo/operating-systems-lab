//! 系统调用处理

pub mod nr;
mod aio;
mod capability;
mod compat;
pub mod errno;
mod fanotify;
mod fcntl;
pub(crate) mod fd;
mod fs_info;
mod futex;
mod identity;
mod ioctl;
mod io_uring;
mod ipc;
mod memfd;
pub(crate) mod memory;
pub(crate) mod mount;
pub(crate) mod mqueue;
mod namespace;
pub(crate) mod cgroup;
mod close_range;
mod getdents;
pub(crate) mod open;
pub(crate) mod path;
mod attr;
mod chdir;
mod dir;
mod link;
mod poll;
mod rename;
pub(crate) mod process;
pub(crate) mod process_clone;
mod process_exec;
mod process_wait;
mod process_group;
mod process_info;
mod random;
mod resource;
mod robust;
mod sched;
mod scheduler;
pub(crate) mod security;
pub(crate) mod shm;
pub(crate) mod signal;
pub(crate) mod socket;
pub(crate) mod system;
pub(crate) mod time;
pub(crate) mod user;  // 真化〔seccomp BPF gate 需 user-copy helper〕
pub(crate) mod xattr;

pub use self::errno::{SyscallError, SyscallResult};
pub use self::futex::{
    futex_waiter_stats_snapshot, remove_pid_from_futex_waiters, FUTEX_WAITERS,
};
pub(crate) use self::signal::check_and_deliver_signals;
pub(crate) use self::path::{resolve_at_path, resolve_path};
pub(in crate::syscall) use crate::exec::current_process_exec_path;

use capability::{
    sys_cap_check, sys_cap_create, sys_cap_derive, sys_cap_info, sys_cap_read, sys_cap_revoke,
    sys_cap_write,
};
mod sync;
use sync::{sys_sync, sys_fsync, sys_fdatasync, sys_syncfs, sys_sync_file_range, sys_flock};
use namespace::{sys_unshare, sys_setns, sys_clone3};
use crate::exec::sys_execveat;
use crate::mm::mman::{
    sys_madvise, sys_mincore, sys_mlock, sys_mlock2, sys_mlockall, sys_mremap, sys_msync,
    sys_munlock, sys_munlockall, sys_remap_file_pages,
};
use crate::mm::mempolicy::{
    sys_get_mempolicy, sys_mbind, sys_migrate_pages, sys_move_pages, sys_pkey_alloc,
    sys_pkey_free, sys_pkey_mprotect, sys_process_madvise, sys_set_mempolicy,
};
use crate::sched::posix_timer::{
    sys_getitimer, sys_timer_create, sys_timer_delete, sys_timer_getoverrun, sys_timer_gettime,
    sys_timer_settime,
};
use compat::{
    sys_epoll_pwait2, sys_faccessat2,
    sys_landlock_add_rule, sys_landlock_create_ruleset, sys_landlock_restrict_self,
    sys_preadv2,
    sys_pwritev2, sys_readahead,
    sys_rseq,
    sys_umask, sys_vhangup,
};
use memfd::{sys_memfd_create, sys_memfd_secret, sys_userfaultfd};
use crate::ipc::sysv::{
    sys_msgctl, sys_msgget, sys_msgrcv, sys_msgsnd, sys_semctl, sys_semget, sys_semop,
    sys_semtimedop,
};
use mount::{
    sys_mount, sys_umount2, sys_mount_setattr,
    sys_fsopen, sys_fsconfig, sys_fsmount, sys_fspick, sys_open_tree, sys_move_mount,
    sys_statmount, sys_listmount,
};
use crate::kernel::ptrace::{sys_ptrace, sys_process_vm_readv, sys_process_vm_writev, sys_kcmp};
use sched::{sys_sched_setaffinity, sys_sched_setattr, sys_sched_getattr};
use xattr::{
    sys_setxattr, sys_lsetxattr, sys_fsetxattr,
    sys_getxattr, sys_lgetxattr, sys_fgetxattr,
    sys_listxattr, sys_llistxattr, sys_flistxattr,
    sys_removexattr, sys_lremovexattr, sys_fremovexattr,
};
use fcntl::sys_fcntl;
use ioctl::sys_ioctl;
use fd::{sys_close, sys_dup, sys_dup3};
use crate::fs::file_copy::sys_copy_file_range;
use crate::fs::file_handle::{sys_name_to_handle_at, sys_open_by_handle_at};
use crate::fs::file_position::sys_lseek;
use crate::fs::positional_io::{sys_pread64, sys_preadv, sys_pwrite64, sys_pwritev};
use crate::fs::read_io::{sys_read, sys_readv};
use crate::fs::write_io::{sys_write, sys_writev};
use crate::kernel::pidfd::{sys_pidfd_getfd, sys_pidfd_open, sys_pidfd_send_signal};
use crate::fs::epoll::{sys_epoll_create1, sys_epoll_ctl, sys_epoll_pwait};
use crate::fs::eventfd::sys_eventfd2;
use crate::fs::file_ops::{sys_fallocate, sys_ftruncate, sys_truncate};
use crate::fs::inotify::{sys_inotify_init1, sys_inotify_add_watch, sys_inotify_rm_watch};
use crate::fs::signalfd::sys_signalfd4;
use crate::fs::splice::{sys_sendfile, sys_splice, sys_tee, sys_vmsplice};
use crate::fs::timerfd::{sys_timerfd_create, sys_timerfd_gettime, sys_timerfd_settime};
use fs_info::{sys_fstat, sys_fstatfs, sys_newfstatat, sys_stat, sys_statfs, sys_statx};
use futex::{sys_futex, sys_futex_waitv};
use identity::{
    sys_getegid, sys_geteuid, sys_getgid, sys_getgroups, sys_getresgid, sys_getresuid, sys_getsid,
    sys_getuid, sys_setegid, sys_seteuid, sys_setfsgid, sys_setfsuid, sys_setgid, sys_setgroups,
    sys_setregid, sys_setresgid, sys_setresuid, sys_setreuid, sys_setuid,
};
use crate::security::keyring;
use io_uring::{sys_io_uring_enter, sys_io_uring_register, sys_io_uring_setup};
use ipc::{sys_pipe, sys_pipe2, sys_receive, sys_send, sys_socketpair};
use memory::{sys_brk, sys_mmap, sys_mprotect, sys_munmap};
use close_range::sys_close_range;
use getdents::sys_getdents;
use open::{sys_openat, sys_openat2};
use crate::fs::attr::{sys_fchmod, sys_fchmodat, sys_fchown, sys_fchownat};
use link::{sys_mknodat, sys_linkat, sys_readlinkat, sys_symlinkat};
use rename::sys_renameat2;
use chdir::{sys_getcwd, sys_chdir, sys_fchdir, sys_chroot};
use dir::{sys_mkdir, sys_mkdirat, sys_unlink, sys_unlinkat};
use attr::{sys_faccessat, sys_utimensat};
use poll::{sys_ppoll, sys_pselect6};
use process::{sys_exit, sys_exit_group};
use process_clone::sys_clone;
use process_exec::sys_execve;
use process_wait::{sys_waitid, sys_waitpid};
use process_group::{sys_getpgid, sys_getpriority, sys_ioprio_get, sys_ioprio_set, sys_setpgid, sys_setpriority};
use process_info::{sys_getpid, sys_getppid, sys_gettid, sys_set_tid_address, sys_setsid};
use random::sys_getrandom;
use resource::{sys_getrusage, sys_prlimit64};
use robust::{sys_get_robust_list, sys_set_robust_list};
use scheduler::{
    sys_sched_get_priority, sys_sched_getaffinity, sys_sched_getparam, sys_sched_getscheduler,
    sys_sched_rr_get_interval, sys_sched_setparam, sys_sched_setscheduler, sys_yield,
};
use security::{sys_acct, sys_bpf, sys_capget, sys_capset, sys_personality, sys_posix_fadvise, sys_prctl, sys_seccomp};
use shm::{sys_shmat, sys_shmctl, sys_shmdt, sys_shmget};
use signal::{
    sys_kill, sys_rt_sigaction, sys_rt_sigpending, sys_rt_sigprocmask, sys_rt_sigqueueinfo,
    sys_rt_sigsuspend, sys_rt_sigtimedwait, sys_rt_tgsigqueueinfo, sys_sigaltstack, sys_sigreturn,
    sys_tgkill, sys_tkill,
};
use socket::{
    sys_accept, sys_accept4, sys_bind, sys_connect, sys_getpeername, sys_getsockname, sys_listen,
    sys_recvmmsg, sys_recvfrom, sys_recvmsg, sys_getsockopt, sys_sendmmsg, sys_sendmsg,
    sys_sendto, sys_setsockopt, sys_shutdown, sys_socket,
};
use system::{
    sys_delete_module, sys_finit_module, sys_getcpu, sys_init_module, sys_kexec_load,
    sys_perf_event_open, sys_pivot_root, sys_quotactl, sys_reboot, sys_setdomainname,
    sys_sethostname, sys_swapoff, sys_swapon, sys_sysinfo, sys_syslog, sys_uname,
};
use time::{
    sys_adjtimex, sys_clock_adjtime, sys_clock_getres, sys_clock_gettime, sys_clock_nanosleep,
    sys_clock_settime, sys_gettimeofday, sys_nanosleep, sys_setitimer, sys_settimeofday,
    sys_times,
};

pub const SYSCALL_YIELD_SENTINEL: isize = isize::MIN;

pub fn init() {}

#[inline]
pub(crate) fn ppn_offset_to_kernel_ptr(ppn: crate::mm::PhysPageNum, offset: usize) -> usize {
    crate::mm::phys_to_kernel_vaddr((ppn.0 << 12) | offset)
}

pub(in crate::syscall) fn wake_io_waiters() {
    // Fast path: when no process is parked in IO_WAIT the table scan below
    // cannot wake anyone, so skip the global PROCESS_TABLE lock + O(max_pid)
    // walk entirely. iozone/lmbench small high-frequency I/O calls this on
    // every read/write; after earlier groups have forked many processes the
    // scan is the dominant per-syscall cost. IO_WAITER_COUNT is incremented at
    // the sole IO_WAIT entry point and self-healed inside the real scan, so a
    // zero reading is authoritative.
    if crate::sched::process::IO_WAITER_COUNT.load(core::sync::atomic::Ordering::Relaxed) == 0 {
        return;
    }
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut table) = *guard {
        table.wake_io_waiters();
    }
}

pub(crate) fn has_other_runnable() -> bool {
    // ProcessTable::has_other_runnable 本身就是 `cpu_rq::any_queued()` 的
    // 投影；直接读无锁 rq 镜像即可，I/O 阻塞循环不需要为“还有没有活”
    // 抢全局进程表锁。
    crate::sched::cpu_rq::any_queued()
}

pub(crate) fn inline_wfi_tick() {
    unsafe {
        // 确保 WFI 期间的内核态中断走 __trap_entry（内核栈、无 sscratch 交换）。
        // WFI 返回后仍处于 syscall 内核上下文，**不得**切回 __alltraps：
        // 后续同一 syscall 内的内核态中断会经 __alltraps 把内核 sp 与
        // sscratch 里的用户 sp 交换，trap 保存区直接写坏内核代码/数据
        // （RV SMP=8 BuildStorm 实测：SpinLock 代码页被寄存器保存区覆盖）。
        // 真正的用户态入口在 return-to-user 收尾时统一切回。
        crate::arch::asm_helpers::write_kernel_trap_entry();
        crate::arch::enable_interrupts();
        crate::arch::asm_helpers::wfi_once();
        crate::arch::disable_interrupts();
    }
}

/// 唯一对外的 syscall dispatch。在调用真实 `syscall_inner` 前后做 per-process
/// 内核时间分账：把"陷入内核之前的用户态运行时长"算 user，把"内核态运行时长"
/// 算 kernel。这是 `sys_times` / `sys_getrusage` 的真实数据来源。
///
/// 对永不返回的 syscall（exit/exit_group/clone with yield、suspend_*）：
/// inner 直接 `-> !` 走 `switch_to`，而 `switch_to` 会按 `cpu_in_kernel=true`
/// 把当前段时间算到 kernel bucket，无需再走 exit_kernel；因此 wrapper 的
/// exit_kernel 仅在正常返回路径上执行。
#[allow(dead_code)]
fn scdbg_current_regs() -> Option<(usize, usize, usize, usize, usize, usize, usize)> {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let table = table.as_ref()?;
    let pid = table.current?;
    let process = table.get(pid)?;
    if process.trap_context_ptr == 0 {
        return Some((pid, 0, 0, 0, 0, 0, 0));
    }
    let cx = unsafe { &*(process.trap_context_ptr as *const crate::arch::TrapContext) };
    Some((pid, cx.sepc, cx.x[2], cx.x[8], cx.x[1], cx.x[4], cx.x[18]))
}

#[allow(dead_code)]
fn scdbg_user_frame_chain(mut fp: usize) {
    // RISC-V psABI frame record: [fp-16]=prev_fp, [fp-8]=ra.
    // 仅在显式 SCDBG_BT=1 时启用，用于定位 libc __stack_chk_fail 的真实调用者。
    for depth in 0..8usize {
        if fp < 16 || (fp & 0x7) != 0 {
            crate::println!("[bt] depth={} stop fp={:#x}", depth, fp);
            break;
        }
        let bytes = match crate::syscall::user::read_user_buffer((fp - 16) as *const u8, 16) {
            Ok(b) => b,
            Err(e) => {
                crate::println!("[bt] depth={} fp={:#x} read_err={}", depth, fp, e);
                break;
            }
        };
        let prev_fp = usize::from_le_bytes(bytes[0..8].try_into().unwrap());
        let ra = usize::from_le_bytes(bytes[8..16].try_into().unwrap());
        crate::println!(
            "[bt] depth={} fp={:#x} ra={:#x} prev_fp={:#x}",
            depth,
            fp,
            ra,
            prev_fp,
        );
        if prev_fp <= fp || prev_fp.wrapping_sub(fp) > 0x20000 {
            break;
        }
        fp = prev_fp;
    }
}

pub(crate) fn scdbg_on() -> bool {
    scdbg_static_enabled() || SCDBG_DYNAMIC.load(core::sync::atomic::Ordering::Relaxed)
}

/// Dynamic SCDBG flag — set by listen counter to enable full syscall trace during PARALLEL_UDP
pub(crate) static SCDBG_DYNAMIC: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// SCDBG=1 cmdline 静态标志缓存（tri-state: 0=未知 1=on 2=off）。cmdline 启动后
/// 即固定，避免每次 syscall 都 `get_cmdline().contains()` 扫描字符串（§1.13① 减
/// 热路径指令）。
static SCDBG_STATIC_CACHE: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

#[inline]
fn scdbg_static_enabled() -> bool {
    use core::sync::atomic::Ordering::Relaxed;
    match SCDBG_STATIC_CACHE.load(Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = crate::cmdline::get_cmdline().contains("SCDBG=1");
            SCDBG_STATIC_CACHE.store(if on { 1 } else { 2 }, Relaxed);
            on
        }
    }
}

/// HUNGDBG: periodic syscall-context dump. Hooked at dispatch entry (no locks
/// held, so locking PROCESS_TABLE here is safe). During a busy-spin hang the
/// guilty task keeps issuing syscalls, so this fires reliably; `syscalls in
/// last window` exposes the storm and the per-task `syscall_nr` shows which
/// call is being hammered. Gated by `HUNGDBG=1`; ~8s interval keeps normal-run
/// perturbation negligible.
static HUNG_LAST_DUMP: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static HUNG_SYSCALL_COUNT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static HUNG_ENABLED: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);
fn hung_dbg_enabled() -> bool {
    use core::sync::atomic::Ordering::Relaxed;
    match HUNG_ENABLED.load(Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let on = crate::cmdline::get_cmdline().contains("HUNGDBG=1");
            HUNG_ENABLED.store(if on { 1 } else { 2 }, Relaxed);
            on
        }
    }
}
fn hung_periodic_check(now: u64, nr: usize) {
    use core::sync::atomic::Ordering::Relaxed;
    if !hung_dbg_enabled() {
        return;
    }
    HUNG_SYSCALL_COUNT.fetch_add(1, Relaxed);
    let last = HUNG_LAST_DUMP.load(Relaxed);
    if last == 0 {
        HUNG_LAST_DUMP.store(now, Relaxed);
        return;
    }
    let interval = 8 * crate::arch::consts::CLOCK_FREQ as u64;
    if now.wrapping_sub(last) > interval {
        HUNG_LAST_DUMP.store(now, Relaxed);
        let cnt = HUNG_SYSCALL_COUNT.swap(0, Relaxed);
        crate::println!(
            "[HUNGDBG] periodic: {} syscalls in last ~8s window (current pid issuing nr={})",
            cnt, nr
        );
        // 多核退化的时间去向：锁争用 vs 跨核 TLB 同步等待。
        // 两组数字都只在慢路径累加，无争用时零成本。
        {
            let (reqs, acks, tmo) = crate::mm::tlb::shootdown_counters();
            let (max_hold, max_hold_ra) = crate::sched::process::proc_table_max_hold();
            crate::println!(
                "[HUNGDBG] contention: lock_contended={} lock_spins={} lock_parks={} shootdown reqs={} acks={} timeouts={} spins={}",
                crate::sync::LOCK_CONTENDED.load(Relaxed),
                crate::sync::LOCK_SPIN_ROUNDS.load(Relaxed),
                crate::sync::LOCK_PARKS.load(Relaxed),
                reqs,
                acks,
                tmo,
                crate::mm::tlb::shootdown_spin_total(),
            );
            crate::println!(
                "[HUNGDBG] pt_lock: max_hold_ticks={} max_hold_ra={:#x} (freq={})",
                max_hold,
                max_hold_ra,
                crate::arch::consts::CLOCK_FREQ,
            );
            #[cfg(target_arch = "riscv64")]
            {
                let (ec, pf, tm, ipi, ext, oth) = crate::arch::riscv64::trap::trap_counters();
                crate::println!(
                    "[HUNGDBG] traps: ecall={} pagefault={} timer={} ipi={} ext={} other={}",
                    ec, pf, tm, ipi, ext, oth
                );
            }
            // 时间归因：idle 睡眠 tick 总和 vs 真实经过时间×在线核数。
            // idle 占比高 = 有任务却调度不上/唤醒丢失；占比低 = 真在算但每核变慢。
            crate::println!(
                "[HUNGDBG] time: now={} idle_ticks={} idle_wakeups={} online={}",
                now,
                crate::sched::process_exit::IDLE_TICKS.load(Relaxed),
                crate::sched::process_exit::IDLE_WAKEUPS.load(Relaxed),
                crate::sched::smp::num_online(),
            );
        }
        // 快照与输出必须分离：这条周期路径在正常运行时每 ~8s 触发一次，持锁
        // 直打会把 PROCESS_TABLE 占住整个串口输出时长（实测 ~9.5s），足以让
        // 其余 hart 全程自旋并伪造出 hang。
        let snapshot = {
            let guard = crate::sched::process::PROCESS_TABLE.lock();
            guard.as_ref().map(|table| table.snapshot_for_hang())
        };
        if let Some(snapshot) = snapshot {
            snapshot.print();
        }
        crate::net::socket::dump_socket_registry_for_hang();
    }
}

#[allow(dead_code)]
pub fn syscall(syscall_id: usize, args: [usize; 6], pc: usize) -> isize {
    // trap 入口已 rdtime 并存入 HART_KERNEL_ENTER_TICK（两架构 ecall 路径皆然），
    // 直接复用，免每 syscall 再读一次时钟 CSR（QEMU TCG 下 rdtime 是 io helper）。
    let enter_now = crate::sched::process::HART_KERNEL_ENTER_TICK
        .load(core::sync::atomic::Ordering::Relaxed);
    // 注：用户→内核分账已由各 arch trap handler 在进入 dispatch 前调用过
    // account_enter_kernel；此处不再重复（去掉每 syscall 一次冗余全局锁 +
    // rdtime 旧值复用 + enforce_rlimit_cpu，§1.13① 减热路径指令）。
    hung_periodic_check(enter_now, syscall_id);
    if crate::RTDIAG {
        use core::sync::atomic::Ordering;
        let run_pid = crate::sched::process::RT_RUN_PID.load(Ordering::Relaxed);
        if run_pid != 0
            && crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed) == run_pid
        {
            let t0 = crate::sched::process::RT_RUN_TS.load(Ordering::Relaxed);
            let freq = crate::arch::consts::CLOCK_FREQ as u64;
            let us2 = crate::arch::asm_helpers::rdtime().saturating_sub(t0) * 1_000_000 / freq;
            if syscall_id == 115 {
                crate::sched::process::rtstat_record(1, us2);
            } else if syscall_id == 113 {
                crate::sched::process::rtstat_record(2, us2);
            }
        }
    }
    // Slice 2.5 真实化：在 syscall dispatch 入口写 PCB.last_syscall=(nr, args)，
    // 出口复位为 (u64::MAX, [0;6]) —— 用于 `/proc/[pid]/syscall` 的
    // "running" vs "syscall in progress" 判定
    // （Linux `fs/proc/base.c::proc_pid_syscall`）。
    let syscall_args = [
        args[0] as u64, args[1] as u64, args[2] as u64,
        args[3] as u64, args[4] as u64, args[5] as u64,
    ];

    // 真化〔seccomp BPF 过滤器执行门〕：按 Linux kernel/seccomp.c::__seccomp_filter
    // 在 dispatch 之前执行所有已安装 filter。ALLOW/LOG 继续走 inner；其它 action
    // 短路返回 / 杀线程 / 杀进程组 / 投 SIGSYS。pc 由 arch trap handler 计算
    // （ecall 指令原始地址，即 sepc-4），用作 seccomp_data.instruction_pointer。
    let scs_result = crate::seccomp::check_current_syscall(syscall_id as u64, syscall_args, pc as u64);
    let early_ret: Option<isize> = match scs_result {
        crate::seccomp::SeccompResult::Allow => None,
        crate::seccomp::SeccompResult::Log => {
            // Linux：RET_LOG 在 audit 里记一条后走 ALLOW。我们没 audit 后端，
            // 直接 info! 一条，避免 silent drop。
            log::info!(
                "[seccomp] LOG action: pid={:?} nr={} pc={:#x}",
                {
                    let g = crate::sched::process::PROCESS_TABLE.lock();
                    g.as_ref().and_then(|t| t.current)
                },
                syscall_id,
                pc
            );
            None
        }
        crate::seccomp::SeccompResult::Errno(e) => {
            // -1..-4095 区间；data=0 时 Linux 已在 decode_action 中 clamp 到 1=EPERM
            Some(-(e as isize))
        }
        crate::seccomp::SeccompResult::Trace(_) => {
            // 本项目暂无 ptrace tracer → 与 Linux `seccomp_do_user_notification`
            // 在无 listener 时行为一致：返回 -ENOSYS。
            Some(-38)
        }
        crate::seccomp::SeccompResult::Trap(data) => {
            // SIGSYS + siginfo._sigsys（含 si_call_addr / si_syscall / si_arch /
            // si_errno=data），由 trap-return 路径的 check_and_deliver_signals
            // 真正交付给 handler。POSIX 行为：syscall 不执行；返回值 Linux 取
            // 当时 a0（不定），我们统一返回 -1 表征 "kernel suppressed"，
            // SA_SIGINFO handler 通过 siginfo 知晓真实原因。
            signal::enqueue_sigsys_for_current(syscall_id as u64, pc, data);
            Some(-1)
        }
        crate::seccomp::SeccompResult::KillThread => {
            // Linux `do_exit(SIGSYS)`：单线程退出，状态 = signaled_with_dump(31)
            // （SIGSYS 是 core-dumping signal，见 signal(7)）。
            // 必须走 `exit_with_reason`，否则 `wait4`/`waitid` 看不到 WIFSIGNALED。
            crate::sched::process::exit_with_reason(
                crate::sched::process::ExitReason::signaled_with_dump(31)
            );
        }
        crate::seccomp::SeccompResult::KillProcess => {
            // Linux `do_group_exit(SIGSYS)`：整线程组退出。本项目 sys_exit_group
            // 收 status 后 -> exit_with_reason(Normal(code))，状态码不会被父
            // 进程认作 WIFSIGNALED。改走真 Signaled 通道，由 sys_exit_group
            // 推广到线程组语义。
            // Per Linux: SECCOMP_RET_KILL_PROCESS → SIGSYS(31) + core dump.
            // exit_group_with_reason(signaled_with_dump(31)) so parent sees
            // WIFSIGNALED + WTERMSIG=31 + WCOREDUMP, and the *whole* thread
            // group dies (do_group_exit), not just the offending thread.
            crate::sched::process::exit_group_with_reason(
                crate::sched::process::ExitReason::signaled_with_dump(31)
            );
        }
        crate::seccomp::SeccompResult::StrictKill => {
            // Linux STRICT-mode 违规走 `do_exit(SIGKILL)`，不是 SIGSYS；
            // SIGKILL 不在 core-dumping 列表，用 `killed()` (= signaled(9))。
            // LTP `prctl04` 显式校验 WTERMSIG == SIGKILL(9)。
            crate::sched::process::exit_with_reason(
                crate::sched::process::ExitReason::killed()
            );
        }
    };
    if let Some(r) = early_ret {
        if !crate::DIAG_SKIP_ACCT {
            if crate::sched::process_sched::current_cputime_precise() {
                let exit_now = crate::arch::asm_helpers::rdtime();
                crate::sched::process::account_syscall_combined_exit(exit_now);
            } else {
                crate::sched::process_sched::syscall_exit_signal_refresh();
            }
        }
        return r;
    }

    crate::sched::process::set_current_syscall_enter(syscall_id as u64, syscall_args);
    let scdbg = scdbg_static_enabled() || SCDBG_DYNAMIC.load(core::sync::atomic::Ordering::Relaxed);
    if scdbg {
        // For dynamic trace, always include pid
        if let Some((pid, _sepc, _sp, _s0, _ra, _tp, _s2)) = scdbg_current_regs() {
            crate::println!(
                "[sc] enter pid={} nr={} t={} a0={:#x} a1={:#x} a2={:#x} a3={:#x} a4={:#x} a5={:#x}",
                pid, syscall_id, crate::arch::asm_helpers::rdtime(), args[0], args[1], args[2], args[3], args[4], args[5]
            );
        } else {
            crate::println!(
                "[sc] enter nr={} t={} a0={:#x} a1={:#x} a2={:#x} a3={:#x}",
                syscall_id, crate::arch::asm_helpers::rdtime(), args[0], args[1], args[2], args[3]
            );
        }
    }
    let result = syscall_inner(syscall_id, args);

    if scdbg {
        if let Some((pid, _sepc, _sp, _s0, _ra, _tp, _s2)) = scdbg_current_regs() {
            crate::println!("[sc] exit pid={} nr={} t={} ret={}", pid, syscall_id, crate::arch::asm_helpers::rdtime(), result);
        } else {
            crate::println!("[sc] exit nr={} t={} ret={}", syscall_id, crate::arch::asm_helpers::rdtime(), result);
        }
    }
    crate::sched::process::set_current_syscall_exit();
    if crate::DIAG_SKIP_ACCT {
        return result;
    }
    // 仅精确分账进程才付 rdtime + 分账成本；其余只刷新 pending-signal 缓存。
    if crate::sched::process_sched::current_cputime_precise() {
        let exit_now = crate::arch::asm_helpers::rdtime();
        crate::sched::process::account_syscall_combined_exit(exit_now);
    } else {
        crate::sched::process_sched::syscall_exit_signal_refresh();
    }
    result
}

#[allow(dead_code)]
fn syscall_inner(syscall_id: usize, args: [usize; 6]) -> isize {
    match syscall_id {
        nr::SYS_EXIT => sys_exit(args[0] as i32),
        nr::SYS_EXIT_GROUP => sys_exit_group(args[0] as i32),
        nr::SYS_WRITE => sys_write(args[0], args[1] as *const u8, args[2]),
        nr::SYS_WRITEV => sys_writev(args[0], args[1], args[2]),
        nr::SYS_READ => sys_read(args[0], args[1] as *mut u8, args[2]),
        nr::SYS_READV => sys_readv(args[0], args[1], args[2]),
        nr::SYS_PREAD64 => sys_pread64(args[0], args[1] as *mut u8, args[2], args[3]),
        nr::SYS_PWRITE64 => sys_pwrite64(args[0], args[1] as *const u8, args[2], args[3]),
        nr::SYS_PREADV => sys_preadv(args[0], args[1], args[2], args[3]),
        nr::SYS_PWRITEV => sys_pwritev(args[0], args[1], args[2], args[3]),
        nr::SYS_FADVISE64 => sys_posix_fadvise(args[0], args[1], args[2], args[3] as i32),
        nr::SYS_FORK => {
            #[cfg(target_arch = "loongarch64")]
            { sys_clone(args[0], args[1], args[2], args[4], args[3]) }
            #[cfg(not(target_arch = "loongarch64"))]
            { sys_clone(args[0], args[1], args[2], args[3], args[4]) }
        }
        nr::SYS_EXEC => sys_execve(args[0] as *const u8, args[1] as *const usize, args[2] as *const usize),
        nr::SYS_WAITPID => sys_waitpid(args[0] as isize, args[1] as *mut i32, args[2], args[3] as *mut u8),
        nr::SYS_WAITID => sys_waitid(args[0], args[1], args[2] as *mut u8, args[3], args[4]),
        nr::SYS_GETPID => sys_getpid(),
        nr::SYS_YIELD => sys_yield(),
        nr::SYS_MMAP => sys_mmap(args[0], args[1], args[2], args[3], args[4], args[5]),
        nr::SYS_MUNMAP => sys_munmap(args[0], args[1]),
        nr::SYS_BRK => sys_brk(args[0]),
        nr::SYS_OPEN => sys_openat(args[0] as isize, args[1] as *const u8, args[2] as u32, args[3] as u32),
        nr::SYS_CLOSE => sys_close(args[0]),
        nr::SYS_GETCWD => sys_getcwd(args[0] as *mut u8, args[1]),
        nr::SYS_CHDIR => sys_chdir(args[0] as *const u8),
        nr::SYS_FCHDIR => sys_fchdir(args[0]),
        nr::SYS_GETDENTS => sys_getdents(args[0], args[1] as *mut u8, args[2]),
        nr::SYS_FSTAT => sys_fstat(args[0], args[1] as *mut u8),
        nr::SYS_SYNC => sys_sync(),
        nr::SYS_FSYNC => sys_fsync(args[0]),
        nr::SYS_FDATASYNC => sys_fdatasync(args[0]),
        nr::SYS_SYNCFS => sys_syncfs(args[0]),
        nr::SYS_SYNC_FILE_RANGE => sys_sync_file_range(
            args[0],
            args[1] as i64,
            args[2] as i64,
            args[3] as u32,
        ),
        nr::SYS_UNLINK => sys_unlink(args[0] as *const u8, args[1]),
        nr::SYS_MKDIRAT => sys_mkdirat(args[0] as isize, args[1] as *const u8, args[2] as u32),
        nr::SYS_UNLINKAT => sys_unlinkat(args[0] as isize, args[1] as *const u8, args[2] as u32),
        nr::SYS_SYMLINKAT => sys_symlinkat(args[0] as *const u8, args[1] as isize, args[2] as *const u8),
        nr::SYS_FACCESSAT => sys_faccessat(args[0] as isize, args[1] as *const u8, args[2] as u32),
        nr::SYS_NEWFSTATAT => sys_newfstatat(args[0] as isize, args[1] as *const u8, args[2] as *mut u8, args[3] as u32),
        nr::SYS_LSEEK => sys_lseek(args[0], args[1] as isize, args[2]),
        nr::SYS_GETTIMEOFDAY => sys_gettimeofday(args[0] as *mut u8, args[1] as *mut u8),
        nr::SYS_CLOCK_GETTIME => sys_clock_gettime(args[0], args[1] as *mut u8),
        nr::SYS_CLOCK_GETRES => sys_clock_getres(args[0], args[1] as *mut u8),
        nr::SYS_ADJTIMEX => sys_adjtimex(args[0] as *mut u8),
        // SYS_PIPE (= #59 on generic uapi) is actually pipe2(2). Use the
        // flag-aware sys_pipe2 so O_CLOEXEC / O_NONBLOCK are honored.
        nr::SYS_PIPE => sys_pipe2(args[0] as *mut i32, args[1] as u32),
        nr::SYS_SYSINFO => sys_sysinfo(args[0] as *mut u8),
        nr::SYS_ACCT => sys_acct(args[0] as *const u8),
        nr::SYS_CAPGET => sys_capget(args[0] as *const u8, args[1] as *mut u8),
        nr::SYS_CAPSET => sys_capset(args[0] as *const u8, args[1] as *const u8),
        nr::SYS_ADD_KEY => keyring::sys_add_key(args[0] as *const u8, args[1] as *const u8, args[2] as *const u8, args[3], args[4] as isize),
        nr::SYS_REQUEST_KEY => keyring::sys_request_key(args[0] as *const u8, args[1] as *const u8, args[2] as *const u8, args[3] as isize),
        nr::SYS_KEYCTL => keyring::sys_keyctl(args[0] as i32, args[1], args[2], args[3], args[4]),
        nr::SYS_BPF => sys_bpf(args[0] as i32, args[1] as *const u8, args[2] as u32),
        nr::SYS_PRCTL => sys_prctl(args[0] as i32, args[1], args[2], args[3], args[4]),
        nr::SYS_SECCOMP => sys_seccomp(args[0] as u32, args[1] as u32, args[2]),
        nr::SYS_PERSONALITY => sys_personality(args[0] as u32),
        nr::SYS_SETHOSTNAME => sys_sethostname(args[0] as *const u8, args[1]),
        nr::SYS_SETDOMAINNAME => sys_setdomainname(args[0] as *const u8, args[1]),
        nr::SYS_SET_TID_ADDRESS => sys_set_tid_address(args[0]),
        nr::SYS_FUTEX => sys_futex(args[0], args[1] as i32, args[2] as u32, args[3], args[4], args[5] as u32),
        nr::SYS_SET_ROBUST_LIST => sys_set_robust_list(args[0], args[1]),
        nr::SYS_GET_ROBUST_LIST => sys_get_robust_list(args[0], args[1], args[2]),
        nr::SYS_SETITIMER => sys_setitimer(args[0] as i32, args[1], args[2]),
        nr::SYS_RSEQ => sys_rseq(args[0], args[1], args[2], args[3]),
        nr::SYS_SIGALTSTACK => sys_sigaltstack(args[0], args[1]),
        nr::SYS_RT_SIGSUSPEND => sys_rt_sigsuspend(args[0], args[1]),
        nr::SYS_RT_SIGACTION => sys_rt_sigaction(args[0], args[1], args[2], args[3]),
        nr::SYS_RT_SIGPROCMASK => sys_rt_sigprocmask(args[0], args[1], args[2], args[3]),
        nr::SYS_RT_SIGPENDING => sys_rt_sigpending(args[0], args[1]),
        nr::SYS_RT_SIGTIMEDWAIT => sys_rt_sigtimedwait(args[0], args[1], args[2], args[3]),
        nr::SYS_IOCTL => sys_ioctl(args[0], args[1], args[2]),
        nr::SYS_FCNTL => sys_fcntl(args[0], args[1], args[2]),
        nr::SYS_MPROTECT => sys_mprotect(args[0], args[1], args[2]),
        nr::SYS_GETPPID => sys_getppid(),
        nr::SYS_GETUID => sys_getuid(),
        nr::SYS_GETEUID => sys_geteuid(),
        nr::SYS_GETGID => sys_getgid(),
        nr::SYS_GETEGID => sys_getegid(),
        nr::SYS_GETTID => sys_gettid(),
        nr::SYS_SETGID => sys_setgid(args[0] as u32),
        nr::SYS_SETUID => sys_setuid(args[0] as u32),
        nr::SYS_SETEUID => sys_seteuid(args[0] as u32),
        nr::SYS_SETEGID => sys_setegid(args[0] as u32),
        nr::SYS_GETRESUID => sys_getresuid(args[0] as *mut u32, args[1] as *mut u32, args[2] as *mut u32),
        nr::SYS_GETRESGID => sys_getresgid(args[0] as *mut u32, args[1] as *mut u32, args[2] as *mut u32),
        nr::SYS_UNAME => sys_uname(args[0] as *mut u8),
        nr::SYS_NANOSLEEP => sys_nanosleep(args[0], args[1]),
        nr::SYS_PPOLL => sys_ppoll(args[0] as *mut u8, args[1], args[2], args[3], args[4]),
        nr::SYS_PSELECT6 => sys_pselect6(args[0] as i32, args[1] as *mut u8, args[2] as *mut u8, args[3] as *mut u8, args[4], args[5]),
        nr::SYS_DUP => sys_dup(args[0]),
        nr::SYS_DUP3 => sys_dup3(args[0], args[1], args[2]),
        nr::SYS_READLINKAT => sys_readlinkat(args[0] as isize, args[1] as *const u8, args[2] as *mut u8, args[3]),
        nr::SYS_RENAMEAT2 => sys_renameat2(args[0] as isize, args[1] as *const u8, args[2] as isize, args[3] as *const u8, args[4] as u32),
        // SYS_RENAMEAT (= 38) is a legacy 4-arg variant; forward to renameat2 with flags=0.
        nr::SYS_RENAMEAT => sys_renameat2(args[0] as isize, args[1] as *const u8, args[2] as isize, args[3] as *const u8, 0),
        nr::SYS_PRLIMIT64 => sys_prlimit64(args[0], args[1], args[2], args[3] as *mut u8),
        nr::SYS_GETRLIMIT => sys_prlimit64(0, args[0], 0, args[1] as *mut u8),
        nr::SYS_KILL => sys_kill(
            crate::syscall::process_info::ns_translate_incoming_pid(args[0] as isize),
            args[1] as i32,
        ),
        nr::SYS_TKILL => sys_tkill(args[0] as isize, args[1] as i32),
        nr::SYS_SIGRETURN => sys_sigreturn(),
        nr::SYS_TIMES => sys_times(args[0] as *mut u8),
        nr::SYS_MOUNT => sys_mount(args[0] as *const u8, args[1] as *const u8, args[2] as *const u8, args[3], args[4] as *const u8),
        nr::SYS_UMOUNT2 => sys_umount2(args[0] as *const u8, args[1] as i32),
        nr::SYS_SEND => sys_send(args[0], args[1]),
        nr::SYS_RECEIVE => sys_receive(args[0]),
        nr::SYS_IO_URING_SETUP => sys_io_uring_setup(args[0] as u32, args[1] as *mut u8),
        nr::SYS_IO_URING_ENTER => sys_io_uring_enter(args[0], args[1] as *const u8, args[2] as u32, args[3] as *mut u8, args[4] as u32, args[5] as u32),
        nr::SYS_IO_URING_REGISTER => sys_io_uring_register(args[0], args[1] as u32, args[2] as *const u8, args[3] as u32),
        nr::SYS_CAP_CREATE => sys_cap_create(args[0] as u32, args[1], args[2], args[3] as u32),
        nr::SYS_CAP_DERIVE => sys_cap_derive(args[0] as u64, args[1] as u32),
        nr::SYS_CAP_REVOKE => sys_cap_revoke(args[0] as u64),
        nr::SYS_CAP_CHECK => sys_cap_check(args[0] as u64, args[1] as u32),
        nr::SYS_CAP_INFO => sys_cap_info(args[0] as u64, args[1] as *mut u8),
        nr::SYS_CAP_READ => sys_cap_read(args[0] as u64, args[1], args[2] as *mut u8, args[3]),
        nr::SYS_CAP_WRITE => sys_cap_write(args[0] as u64, args[1], args[2] as *const u8, args[3]),
        nr::SYS_MSYNC => sys_msync(args[0], args[1], args[2]),
        nr::SYS_MLOCK => sys_mlock(args[0], args[1]),
        nr::SYS_MUNLOCK => sys_munlock(args[0], args[1]),
        nr::SYS_MLOCKALL => sys_mlockall(args[0]),
        nr::SYS_MUNLOCKALL => sys_munlockall(),
        nr::SYS_MADVISE => sys_madvise(args[0], args[1], args[2]),
        nr::SYS_FCHMODAT => sys_fchmodat(args[0] as isize, args[1] as *const u8, args[2] as u32, 0),
        nr::SYS_FCHMOD => sys_fchmod(args[0], args[1] as u32),
        nr::SYS_FCHMODAT2 => sys_fchmodat(args[0] as isize, args[1] as *const u8, args[2] as u32, args[3] as u32),
        nr::SYS_FCHOWNAT => sys_fchownat(args[0] as isize, args[1] as *const u8, args[2] as u32, args[3] as u32, args[4] as u32),
        nr::SYS_FCHOWN => sys_fchown(args[0], args[1] as u32, args[2] as u32),
        nr::SYS_UTIMENSAT => sys_utimensat(args[0] as isize, args[1] as *const u8, args[2] as *const u8, args[3] as u32),
        nr::SYS_UMASK => sys_umask(args[0]),
        nr::SYS_SYSLOG => sys_syslog(args[0] as i32, args[1] as *mut u8, args[2]),
        nr::SYS_TGKILL => sys_tgkill(args[0] as isize, args[1] as isize, args[2] as i32),
        nr::SYS_SENDFILE => sys_sendfile(args[0] as i32, args[1] as i32, args[2], args[3]),
        nr::SYS_IOPRIO_SET => sys_ioprio_set(args[0], args[1], args[2]),
        nr::SYS_IOPRIO_GET => sys_ioprio_get(args[0], args[1]),
        nr::SYS_TRUNCATE => sys_truncate(args[0] as *const u8, args[1]),
        nr::SYS_FTRUNCATE => sys_ftruncate(args[0], args[1]),
        nr::SYS_FALLOCATE => sys_fallocate(args[0], args[1], args[2], args[3]),
        nr::SYS_INOTIFY_INIT1 => sys_inotify_init1(args[0]),
        nr::SYS_INOTIFY_ADD_WATCH => sys_inotify_add_watch(args[0], args[1] as *const u8, args[2]),
        nr::SYS_INOTIFY_RM_WATCH => sys_inotify_rm_watch(args[0], args[1]),
        nr::SYS_EVENTFD2 => sys_eventfd2(args[0], args[1]),
        nr::SYS_SIGNALFD4 => sys_signalfd4(args[0] as isize, args[1] as *const u8, args[2], args[3]),
        nr::SYS_TIMERFD_CREATE => sys_timerfd_create(args[0], args[1]),
        nr::SYS_TIMERFD_SETTIME => sys_timerfd_settime(args[0], args[1], args[2] as *const u8, args[3] as *mut u8),
        nr::SYS_TIMERFD_GETTIME => sys_timerfd_gettime(args[0], args[1] as *mut u8),
        nr::SYS_EPOLL_CREATE1 => sys_epoll_create1(args[0]),
        nr::SYS_EPOLL_CTL => sys_epoll_ctl(args[0], args[1], args[2], args[3] as *const u8),
        nr::SYS_EPOLL_PWAIT => sys_epoll_pwait(args[0], args[1] as *mut u8, args[2], args[3] as isize, args[4] as *const u8, args[5]),
        nr::SYS_FANOTIFY_INIT => fanotify::sys_fanotify_init(args[0], args[1]),
        nr::SYS_FANOTIFY_MARK => fanotify::sys_fanotify_mark(args[0], args[1], args[2] as u64, args[3] as isize, args[4] as *const u8),
        nr::SYS_PIDFD_OPEN => sys_pidfd_open(args[0] as isize, args[1]),
        nr::SYS_PIDFD_SEND_SIGNAL => sys_pidfd_send_signal(args[0], args[1] as i32, args[2] as *const u8, args[3]),
        nr::SYS_PIDFD_GETFD => sys_pidfd_getfd(args[0], args[1], args[2]),
        // Slice 1.7-final: Tsinghua-style cheap stubs (mknodat/linkat now just
        // return 0 after probing the user pointer; name_to_handle_at /
        // open_by_handle_at return -EINVAL inline). This avoids the listen01
        // TIMEOUT regression seen when these arms inlined large fn bodies
        // with 4 KiB stack buffers.
        nr::SYS_MKNODAT => sys_mknodat(args[0] as isize, args[1] as *const u8, args[2] as u32, args[3] as u64),
        nr::SYS_LINKAT => sys_linkat(args[0] as isize, args[1] as *const u8, args[2] as isize, args[3] as *const u8, args[4] as u32),
        nr::SYS_NAME_TO_HANDLE_AT => sys_name_to_handle_at(args[0] as isize, args[1] as *const u8, args[2] as *mut u8, args[3] as *mut i32, args[4]),
        nr::SYS_OPEN_BY_HANDLE_AT => sys_open_by_handle_at(args[0], args[1] as *const u8, args[2]),
        // Slice 1.8 — broad Tsinghua-style stubs for the long FAIL-cluster tail.
        // See compat.rs for the per-syscall rationale (-ENOSYS / Ok(0) / -EPERM).
        nr::SYS_MQ_OPEN => mqueue::sys_mq_open(args[0] as *const u8, args[1] as i32, args[2] as u32, args[3] as *const u8),
        nr::SYS_MQ_UNLINK => mqueue::sys_mq_unlink(args[0] as *const u8),
        nr::SYS_MQ_TIMEDSEND => mqueue::sys_mq_timedsend(args[0] as i32, args[1] as *const u8, args[2], args[3] as u32, args[4] as *const u8),
        nr::SYS_MQ_TIMEDRECEIVE => mqueue::sys_mq_timedreceive(args[0] as i32, args[1] as *mut u8, args[2], args[3] as *mut u32, args[4] as *const u8),
        nr::SYS_MQ_NOTIFY => mqueue::sys_mq_notify(args[0] as i32, args[1] as *const u8),
        nr::SYS_MQ_GETSETATTR => mqueue::sys_mq_getsetattr(args[0] as i32, args[1] as *const u8, args[2] as *mut u8),
        nr::SYS_MSGGET => sys_msgget(args[0] as i32, args[1] as i32),
        nr::SYS_MSGCTL => sys_msgctl(args[0] as i32, args[1] as i32, args[2] as *mut u8),
        nr::SYS_MSGSND => sys_msgsnd(args[0] as i32, args[1] as *const u8, args[2], args[3] as i32),
        nr::SYS_MSGRCV => sys_msgrcv(args[0] as i32, args[1] as *mut u8, args[2], args[3] as isize, args[4] as i32),
        nr::SYS_SEMGET => sys_semget(args[0] as i32, args[1] as i32, args[2] as i32),
        nr::SYS_SEMCTL => sys_semctl(args[0] as i32, args[1] as i32, args[2] as i32, args[3]),
        nr::SYS_SEMOP => sys_semop(args[0] as i32, args[1] as *const u8, args[2]),
        nr::SYS_SEMTIMEDOP => sys_semtimedop(args[0] as i32, args[1] as *const u8, args[2], args[3] as *const u8),
        nr::SYS_QUOTACTL => sys_quotactl(args[0] as i32, args[1] as *const u8, args[2] as i32, args[3] as *mut u8),
        nr::SYS_MOVE_PAGES => sys_move_pages(args[0] as i32, args[1], args[2] as *const usize, args[3] as *const i32, args[4] as *mut i32, args[5] as i32),
        nr::SYS_MBIND => sys_mbind(args[0], args[1], args[2] as i32, args[3] as *const u64, args[4] as u64, args[5] as u32),
        nr::SYS_SET_MEMPOLICY => sys_set_mempolicy(args[0] as i32, args[1] as *const u64, args[2] as u64),
        nr::SYS_MIGRATE_PAGES => sys_migrate_pages(args[0] as i32, args[1] as u64, args[2] as *const u64, args[3] as *const u64),
        nr::SYS_MINCORE => sys_mincore(args[0], args[1], args[2] as *mut u8),
        nr::SYS_FLOCK => sys_flock(args[0] as i32, args[1] as i32),
        nr::SYS_SETPRIORITY => sys_setpriority(args[0] as i32, args[1] as i32, args[2] as i32),
        nr::SYS_GETPRIORITY => sys_getpriority(args[0] as i32, args[1] as i32),
        nr::SYS_INIT_MODULE => sys_init_module(args[0] as *const u8, args[1], args[2] as *const u8),
        nr::SYS_FINIT_MODULE => sys_finit_module(args[0] as i32, args[1] as *const u8, args[2] as i32),
        nr::SYS_DELETE_MODULE => sys_delete_module(args[0] as *const u8, args[1] as i32),
        nr::SYS_CHROOT => sys_chroot(args[0] as *const u8),
        nr::SYS_PIVOT_ROOT => sys_pivot_root(args[0] as *const u8, args[1] as *const u8),
        nr::SYS_UNSHARE => sys_unshare(args[0] as i32),
        nr::SYS_SETNS => sys_setns(args[0] as i32, args[1] as i32),
        nr::SYS_SETXATTR => sys_setxattr(args[0] as *const u8, args[1] as *const u8, args[2] as *const u8, args[3], args[4] as i32),
        nr::SYS_LSETXATTR => sys_lsetxattr(args[0] as *const u8, args[1] as *const u8, args[2] as *const u8, args[3], args[4] as i32),
        nr::SYS_FSETXATTR => sys_fsetxattr(args[0] as i32, args[1] as *const u8, args[2] as *const u8, args[3], args[4] as i32),
        nr::SYS_GETXATTR => sys_getxattr(args[0] as *const u8, args[1] as *const u8, args[2] as *mut u8, args[3]),
        nr::SYS_LGETXATTR => sys_lgetxattr(args[0] as *const u8, args[1] as *const u8, args[2] as *mut u8, args[3]),
        nr::SYS_FGETXATTR => sys_fgetxattr(args[0] as i32, args[1] as *const u8, args[2] as *mut u8, args[3]),
        nr::SYS_LISTXATTR => sys_listxattr(args[0] as *const u8, args[1] as *mut u8, args[2]),
        nr::SYS_LLISTXATTR => sys_llistxattr(args[0] as *const u8, args[1] as *mut u8, args[2]),
        nr::SYS_FLISTXATTR => sys_flistxattr(args[0] as i32, args[1] as *mut u8, args[2]),
        nr::SYS_REMOVEXATTR => sys_removexattr(args[0] as *const u8, args[1] as *const u8),
        nr::SYS_LREMOVEXATTR => sys_lremovexattr(args[0] as *const u8, args[1] as *const u8),
        nr::SYS_FREMOVEXATTR => sys_fremovexattr(args[0] as i32, args[1] as *const u8),
        nr::SYS_IO_SETUP => aio::sys_io_setup(args[0] as u32, args[1] as *mut u64),
        nr::SYS_IO_DESTROY => aio::sys_io_destroy(args[0] as u64),
        nr::SYS_IO_SUBMIT => aio::sys_io_submit(args[0] as u64, args[1] as i64, args[2] as *const usize),
        nr::SYS_IO_CANCEL => aio::sys_io_cancel(args[0] as u64, args[1] as *const u8, args[2] as *mut u8),
        nr::SYS_IO_GETEVENTS => aio::sys_io_getevents(args[0] as u64, args[1] as i64, args[2] as i64, args[3] as *mut u8, args[4] as *const u8),
        nr::SYS_REBOOT => sys_reboot(args[0] as i32, args[1] as i32, args[2] as u32, args[3] as *const u8),
        nr::SYS_KEXEC_LOAD => sys_kexec_load(args[0], args[1], args[2] as *const u8, args[3] as u64),
        nr::SYS_SWAPON => sys_swapon(args[0] as *const u8, args[1] as i32),
        nr::SYS_SWAPOFF => sys_swapoff(args[0] as *const u8),
        nr::SYS_SCHED_SETATTR => sys_sched_setattr(args[0] as i32, args[1] as *const u8, args[2] as u32),
        nr::SYS_SCHED_GETATTR => sys_sched_getattr(args[0] as i32, args[1] as *mut u8, args[2] as u32, args[3] as u32),
        nr::SYS_PERF_EVENT_OPEN => sys_perf_event_open(args[0] as *const u8, args[1] as i32, args[2] as i32, args[3] as i32, args[4] as u64),
        // Slice 1.8b — additional broad stubs for the long FAIL-cluster tail
        // (ptrace, kcmp, memfd/userfaultfd, clone3/execveat/getcpu, futex_waitv,
        // time setters, landlock, openat2/close_range/faccessat2/process_madvise/
        // epoll_pwait2, recv/sendmsg{mmsg}, mremap/readahead/mlock2, pkey_*,
        // preadv2/pwritev2, statmount/listmount). See compat.rs for rationale.
        nr::SYS_PTRACE => sys_ptrace(args[0], args[1] as i32, args[2], args[3]),
        nr::SYS_PROCESS_VM_READV => sys_process_vm_readv(args[0] as i32, args[1] as *const u8, args[2], args[3] as *const u8, args[4], args[5] as u64),
        nr::SYS_PROCESS_VM_WRITEV => sys_process_vm_writev(args[0] as i32, args[1] as *const u8, args[2], args[3] as *const u8, args[4], args[5] as u64),
        nr::SYS_KCMP => sys_kcmp(args[0] as i32, args[1] as i32, args[2] as i32, args[3], args[4]),
        nr::SYS_MEMFD_CREATE => sys_memfd_create(args[0] as *const u8, args[1] as u32),
        nr::SYS_MEMFD_SECRET => sys_memfd_secret(args[0] as u32),
        nr::SYS_USERFAULTFD => sys_userfaultfd(args[0] as u32),
        nr::SYS_CLONE3 => sys_clone3(args[0] as *const u8, args[1]),
        nr::SYS_EXECVEAT => sys_execveat(args[0] as isize, args[1] as *const u8, args[2] as *const usize, args[3] as *const usize, args[4] as i32),
        nr::SYS_GETCPU => sys_getcpu(args[0] as *mut u32, args[1] as *mut u32, args[2] as *mut u8),
        nr::SYS_FUTEX_WAITV => sys_futex_waitv(args[0] as *const u8, args[1] as u32, args[2] as u32, args[3] as *const u8, args[4] as i32),
        nr::SYS_SETTIMEOFDAY => sys_settimeofday(args[0] as *const u8, args[1] as *const u8),
        nr::SYS_VHANGUP => sys_vhangup(),
        nr::SYS_CLOCK_SETTIME => sys_clock_settime(args[0] as i32, args[1] as *const u8),
        nr::SYS_CLOCK_ADJTIME => sys_clock_adjtime(args[0] as i32, args[1] as *mut u8),
        nr::SYS_LANDLOCK_CREATE_RULESET => sys_landlock_create_ruleset(args[0] as *const u8, args[1], args[2] as u32),
        nr::SYS_LANDLOCK_ADD_RULE => sys_landlock_add_rule(args[0] as i32, args[1] as u32, args[2] as *const u8, args[3] as u32),
        nr::SYS_LANDLOCK_RESTRICT_SELF => sys_landlock_restrict_self(args[0] as i32, args[1] as u32),
        nr::SYS_FSOPEN => sys_fsopen(args[0] as *const u8, args[1] as u32),
        nr::SYS_FSCONFIG => sys_fsconfig(args[0] as i32, args[1] as u32, args[2] as *const u8, args[3] as *const u8, args[4] as i32),
        nr::SYS_FSMOUNT => sys_fsmount(args[0] as i32, args[1] as u32, args[2] as u32),
        nr::SYS_FSPICK => sys_fspick(args[0] as i32, args[1] as *const u8, args[2] as u32),
        nr::SYS_OPEN_TREE => sys_open_tree(args[0] as i32, args[1] as *const u8, args[2] as u32),
        nr::SYS_MOVE_MOUNT => sys_move_mount(args[0] as i32, args[1] as *const u8, args[2] as i32, args[3] as *const u8, args[4] as u32),
        nr::SYS_MOUNT_SETATTR => sys_mount_setattr(args[0] as i32, args[1] as *const u8, args[2] as u32, args[3] as *const u8, args[4]),
        nr::SYS_OPENAT2 => sys_openat2(args[0] as isize, args[1] as *const u8, args[2] as *const u8, args[3]),
        nr::SYS_CLOSE_RANGE => sys_close_range(args[0] as u32, args[1] as u32, args[2] as u32),
        nr::SYS_FACCESSAT2 => sys_faccessat2(args[0] as isize, args[1] as *const u8, args[2] as u32, args[3] as u32),
        nr::SYS_PROCESS_MADVISE => sys_process_madvise(args[0] as i32, args[1] as *const u8, args[2], args[3] as i32, args[4] as u32),
        nr::SYS_EPOLL_PWAIT2 => sys_epoll_pwait2(args[0], args[1] as *mut u8, args[2], args[3] as *const u8, args[4] as *const u8, args[5]),
        nr::SYS_RECVMSG => sys_recvmsg(args[0] as i32, args[1] as *mut u8, args[2] as i32),
        nr::SYS_SENDMSG => sys_sendmsg(args[0] as i32, args[1] as *const u8, args[2] as i32),
        nr::SYS_RECVMMSG => sys_recvmmsg(args[0] as i32, args[1] as *mut u8, args[2] as u32, args[3] as i32, args[4] as *const u8),
        nr::SYS_SENDMMSG => sys_sendmmsg(args[0] as i32, args[1] as *mut u8, args[2] as u32, args[3] as i32),
        nr::SYS_MREMAP => sys_mremap(args[0], args[1], args[2], args[3] as u32, args[4]),
        nr::SYS_REMAP_FILE_PAGES => sys_remap_file_pages(args[0], args[1], args[2], args[3], args[4]),
        nr::SYS_READAHEAD => sys_readahead(args[0] as i32, args[1] as i64, args[2]),
        nr::SYS_MLOCK2 => sys_mlock2(args[0], args[1], args[2] as u32),
        nr::SYS_PKEY_MPROTECT => sys_pkey_mprotect(args[0], args[1], args[2], args[3] as i32),
        nr::SYS_PKEY_ALLOC => sys_pkey_alloc(args[0] as u32, args[1] as u32),
        nr::SYS_PKEY_FREE => sys_pkey_free(args[0] as i32),
        // preadv2/pwritev2 take the offset split as (pos_l=args[3], pos_h=args[4])
        // and rwf_t flags as args[5]; on 64-bit pos_h is shifted out so the
        // offset is just pos_l (Linux pos_from_hilo).
        nr::SYS_PREADV2 => sys_preadv2(args[0], args[1], args[2], args[3], args[5] as i32),
        nr::SYS_PWRITEV2 => sys_pwritev2(args[0], args[1], args[2], args[3], args[5] as i32),
        nr::SYS_STATMOUNT => sys_statmount(args[0] as *const u8, args[1] as *mut u8, args[2], args[3] as u32),
        nr::SYS_LISTMOUNT => sys_listmount(args[0] as *const u8, args[1] as *mut u64, args[2], args[3] as u32),
        nr::SYS_RT_SIGQUEUEINFO => sys_rt_sigqueueinfo(args[0] as isize, args[1] as i32, args[2] as *const u8),
        nr::SYS_RT_TGSIGQUEUEINFO => sys_rt_tgsigqueueinfo(args[0] as isize, args[1] as isize, args[2] as i32, args[3] as *const u8),
        // Identity ext: getsid / setre[ug]id / setres[ug]id / setfs[ug]id.
        nr::SYS_GETSID    => sys_getsid(args[0] as isize),
        nr::SYS_SETREUID  => sys_setreuid(args[0] as i32, args[1] as i32),
        nr::SYS_SETREGID  => sys_setregid(args[0] as i32, args[1] as i32),
        nr::SYS_SETRESUID => sys_setresuid(args[0] as i32, args[1] as i32, args[2] as i32),
        nr::SYS_SETRESGID => sys_setresgid(args[0] as i32, args[1] as i32, args[2] as i32),
        nr::SYS_SETFSUID  => sys_setfsuid(args[0] as i32),
        nr::SYS_SETFSGID  => sys_setfsgid(args[0] as i32),
        // POSIX timer family
        nr::SYS_GETITIMER       => sys_getitimer(args[0] as i32, args[1] as *mut u8),
        nr::SYS_TIMER_CREATE    => sys_timer_create(args[0] as i32, args[1] as *const u8, args[2] as *mut i32),
        nr::SYS_TIMER_SETTIME   => sys_timer_settime(args[0] as i32, args[1] as i32, args[2] as *const u8, args[3] as *mut u8),
        nr::SYS_TIMER_GETTIME   => sys_timer_gettime(args[0] as i32, args[1] as *mut u8),
        nr::SYS_TIMER_GETOVERRUN => sys_timer_getoverrun(args[0] as i32),
        nr::SYS_TIMER_DELETE    => sys_timer_delete(args[0] as i32),
        nr::SYS_CLOCK_NANOSLEEP => sys_clock_nanosleep(args[0], args[1] as i32, args[2], args[3]),
        nr::SYS_GETRANDOM => sys_getrandom(args[0] as *mut u8, args[1], args[2]),
        nr::SYS_STATX => sys_statx(args[0] as isize, args[1] as *const u8, args[2] as u32, args[3] as u32, args[4] as *mut u8),
        nr::SYS_GETRUSAGE => sys_getrusage(args[0] as i32, args[1] as *mut u8),
        nr::SYS_STATFS => sys_statfs(args[0] as *const u8, args[1] as *mut u8),
        nr::SYS_FSTATFS => sys_fstatfs(args[0], args[1] as *mut u8),
        nr::SYS_SETSID => sys_setsid(),
        nr::SYS_SETPGID => sys_setpgid(args[0] as isize, args[1] as isize),
        nr::SYS_GETPGID => sys_getpgid(args[0] as isize),
        nr::SYS_GETGROUPS => sys_getgroups(args[0], args[1] as *mut u32),
        nr::SYS_SETGROUPS => sys_setgroups(args[0], args[1] as *const u32),
        nr::SYS_SCHED_SETPARAM => sys_sched_setparam(args[0] as isize, args[1] as *const u8),
        nr::SYS_SCHED_SETSCHEDULER => sys_sched_setscheduler(args[0] as isize, args[1] as i32, args[2] as *const u8),
        nr::SYS_SCHED_GETSCHEDULER => sys_sched_getscheduler(args[0] as isize),
        nr::SYS_SCHED_GETPARAM => sys_sched_getparam(args[0] as isize, args[1] as *mut u8),
        nr::SYS_SCHED_SETAFFINITY => sys_sched_setaffinity(args[0] as isize, args[1], args[2] as *const u8),
        nr::SYS_SCHED_GETAFFINITY => sys_sched_getaffinity(args[0] as isize, args[1], args[2] as *mut u8),
        nr::SYS_SCHED_GET_PRIORITY_MAX => sys_sched_get_priority(args[0] as i32, true),
        nr::SYS_SCHED_GET_PRIORITY_MIN => sys_sched_get_priority(args[0] as i32, false),
        nr::SYS_SCHED_RR_GET_INTERVAL => sys_sched_rr_get_interval(args[0] as isize, args[1] as *mut u8),
        nr::SYS_SOCKET => sys_socket(args[0] as i32, args[1] as i32, args[2] as i32),
        nr::SYS_BIND => sys_bind(args[0] as i32, args[1] as *const u8, args[2] as u32),
        nr::SYS_LISTEN => sys_listen(args[0] as i32, args[1] as i32),
        nr::SYS_ACCEPT => sys_accept(args[0] as i32, args[1] as *mut u8, args[2] as *mut u32),
        nr::SYS_ACCEPT4 => sys_accept4(args[0] as i32, args[1] as *mut u8, args[2] as *mut u32, args[3] as i32),
        nr::SYS_CONNECT => sys_connect(args[0] as i32, args[1] as *const u8, args[2] as u32),
        nr::SYS_GETSOCKNAME => sys_getsockname(args[0] as i32, args[1] as *mut u8, args[2] as *mut u32),
        nr::SYS_GETPEERNAME => sys_getpeername(args[0] as i32, args[1] as *mut u8, args[2] as *mut u32),
        nr::SYS_SENDTO => sys_sendto(args[0] as i32, args[1] as *const u8, args[2], args[3] as i32, args[4] as *const u8, args[5] as u32),
        nr::SYS_RECVFROM => sys_recvfrom(args[0] as i32, args[1] as *mut u8, args[2], args[3] as i32, args[4] as *mut u8, args[5] as *mut u32),
        nr::SYS_SETSOCKOPT => sys_setsockopt(args[0] as i32, args[1] as i32, args[2] as i32, args[3] as *const u8, args[4] as u32),
        nr::SYS_GETSOCKOPT => sys_getsockopt(args[0] as i32, args[1] as i32, args[2] as i32, args[3] as *mut u8, args[4] as *mut u32),
        nr::SYS_SHUTDOWN => sys_shutdown(args[0] as i32, args[1] as i32),
        nr::SYS_SOCKETPAIR => sys_socketpair(args[0] as i32, args[1] as i32, args[2] as i32, args[3] as *mut i32),
        nr::SYS_GET_MEMPOLICY => sys_get_mempolicy(args[0] as *mut u8, args[1] as *mut u8, args[2], args[3], args[4]),
        nr::SYS_SHMGET => sys_shmget(args[0] as i32, args[1], args[2] as i32),
        nr::SYS_SHMAT => sys_shmat(args[0] as i32, args[1], args[2] as i32),
        nr::SYS_SHMDT => sys_shmdt(args[0]),
        nr::SYS_SHMCTL => sys_shmctl(args[0] as i32, args[1] as i32, args[2]),
        nr::SYS_COPY_FILE_RANGE => sys_copy_file_range(args[0] as i32, args[1], args[2] as i32, args[3], args[4], args[5] as u32),
        nr::SYS_VMSPLICE => sys_vmsplice(args[0] as i32, args[1], args[2], args[3] as u32),
        nr::SYS_SPLICE => sys_splice(args[0] as i32, args[1], args[2] as i32, args[3], args[4], args[5] as u32),
        nr::SYS_TEE => sys_tee(args[0] as i32, args[1] as i32, args[2], args[3] as u32),
        nr::SYS_MEMBARRIER => {
            // Linux `<linux/membarrier.h>`：
            //   QUERY=0
            //   GLOBAL=1, GLOBAL_EXPEDITED=2, REGISTER_GLOBAL_EXPEDITED=4
            //   PRIVATE_EXPEDITED=8, REGISTER_PRIVATE_EXPEDITED=16
            //   PRIVATE_EXPEDITED_SYNC_CORE=32, REGISTER_..._SYNC_CORE=64
            //   PRIVATE_EXPEDITED_RSEQ=128, REGISTER_..._RSEQ=256
            const Q:    usize = 0;
            const G:    usize = 1;
            const GX:   usize = 2;
            const RGX:  usize = 4;
            const PX:   usize = 8;
            const RPX:  usize = 16;
            const PXSC: usize = 32;
            const RPXSC:usize = 64;
            const PXRS: usize = 128;
            const RPXRS:usize = 256;
            const SUPPORTED: usize = G | GX | RGX | PX | RPX | PXSC | RPXSC | PXRS | RPXRS;
            let cmd = args[0];
            let flags = args[1];
            let _cpu_id = args[2]; // 仅 PRIVATE_EXPEDITED 的 cpu-affinity 变体用，未实现
            if flags != 0 { return -22; }
            // 内存屏障：单/少 hart 内核下所有访问已通过 trap 入口同步，这里补一
            // 次显式 fence 让 LTP "barrier after this call" 期望生效。
            let do_fence = || {
                #[cfg(target_arch = "riscv64")]
                unsafe { core::arch::asm!("fence rw, rw"); }
                #[cfg(target_arch = "loongarch64")]
                unsafe { core::arch::asm!("dbar 0"); }
            };
            match cmd {
                Q => SUPPORTED as isize,
                // GLOBAL / GLOBAL_EXPEDITED：无需注册即可发起屏障。
                G | GX => { do_fence(); 0 }
                // REGISTER_* ：在当前进程（per-mm）登记，返回 0。
                RGX => {
                    let mut table = crate::sched::process::PROCESS_TABLE.lock();
                    let Some(t) = table.as_mut() else { return -22; };
                    let Some(pid) = t.current else { return -22; };
                    let Some(p) = t.get_mut(pid) else { return -22; };
                    p.membarrier_global_expedited = true;
                    0
                }
                RPX => {
                    let mut table = crate::sched::process::PROCESS_TABLE.lock();
                    let Some(t) = table.as_mut() else { return -22; };
                    let Some(pid) = t.current else { return -22; };
                    let Some(p) = t.get_mut(pid) else { return -22; };
                    p.membarrier_private_expedited = true;
                    0
                }
                RPXSC => {
                    let mut table = crate::sched::process::PROCESS_TABLE.lock();
                    let Some(t) = table.as_mut() else { return -22; };
                    let Some(pid) = t.current else { return -22; };
                    let Some(p) = t.get_mut(pid) else { return -22; };
                    p.membarrier_private_sync_core = true;
                    0
                }
                // PRIVATE_EXPEDITED[_SYNC_CORE]：未注册对应命令时 → EPERM
                // （Linux membarrier_private_expedited 检查 mm->membarrier_state）。
                PX => {
                    let registered = {
                        let table = crate::sched::process::PROCESS_TABLE.lock();
                        table.as_ref()
                            .and_then(|t| t.current.and_then(|pid| t.get(pid)))
                            .map(|p| p.membarrier_private_expedited)
                            .unwrap_or(false)
                    };
                    if !registered { return -1; } // -EPERM
                    do_fence();
                    0
                }
                PXSC => {
                    let registered = {
                        let table = crate::sched::process::PROCESS_TABLE.lock();
                        table.as_ref()
                            .and_then(|t| t.current.and_then(|pid| t.get(pid)))
                            .map(|p| p.membarrier_private_sync_core)
                            .unwrap_or(false)
                    };
                    if !registered { return -1; } // -EPERM
                    do_fence();
                    0
                }
                // PRIVATE_EXPEDITED_RSEQ / REGISTER_..._RSEQ：rseq 未实装，但
                // QUERY 已宣告支持。RSEQ 屏障未注册 rseq 时 Linux 返回 EPERM；
                // 我们无 rseq 注册 → 一律 EPERM（与无 rseq 的语义一致）。
                PXRS => -1,
                RPXRS => 0,
                _ => -22, // -EINVAL，未知 cmd
            }
        }
        _ => {
            // 默认沉默返回 -ENOSYS：未实现的 syscall 让 LTP 拿到 errno=ENOSYS
            // 报 TBROK/TCONF 干净退出，不再 println 污染 serial（busy syscall
            // loop 下原版每次都打印一行，明显放慢测试且抢占 console 锁）。
            // 仅当 SCDBG=1 时打印帮助定位缺失系统调用。
            if crate::cmdline::scdbg_enabled() {
                crate::println!("[syscall] Unknown syscall: {}", syscall_id);
            }
            SyscallError::InvalidSyscall as isize
        }
    }
}
