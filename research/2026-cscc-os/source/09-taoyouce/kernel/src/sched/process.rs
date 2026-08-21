//! 进程生命周期管理
//!
//! 实现 fork, exec, exit, wait 等进程操作
//!
//! # 地址空间管理
//! 每个进程拥有独立的地址空间（AddressSpace），包含：
//! - 独立的页表（Sv39 三级页表）
//! - 虚拟内存区域（VMA）列表
//! - 用户栈（位于高地址）
//! - 堆区域（可通过 brk 扩展）

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use alloc::vec;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};
use crate::sync::{IrqMcsSpinLock, SpinLock};
use crate::mm::{PhysAddr, VirtAddr, AddressSpace, Vma, VmaType, VmaPermission, PAGE_SIZE, VirtPageNum, PhysPageNum};
use super::process_acct::{acct_snapshot, write_process_accounting, ProcessAcctSnapshot};
pub use super::process_stack::{KernelStack, KSTK_CANARY};
#[cfg(target_arch = "loongarch64")]
use super::process_stack::track_live_kernel_stack;
use super::process_wait::{
    build_waitid_siginfo, wait_status_from_continued, wait_status_from_exit_reason,
    wait_status_from_stopped_signal, write_user_bytes, WaitidEvent,
};
pub use super::process_state::{signal_is_core_dumping, ExitReason, ProcessState};
pub use super::process_fork::{
    fork, fork_with_ctx,
    create_init_process, create_process_from_elf, create_process_from_path,
};
pub use super::process_exit::{
    exit, exit_with_reason, exit_group_with_reason, de_thread_for_exec, kill,
    wait, wait_nonblock, wait_for_waitid, waitid_reap, waitid_reap_filtered,
    reap_detached_zombies,
    build_waitid_siginfo_pub, build_rusage_buf_from_ticks,
    WaitTarget, ThreadMode, WaitidErr,
};
pub(crate) use super::process_exit::reparent_children_locked;
pub use super::process_sched::{
    stop_current_for_signal,
    preempt_current_for_trap, switch_current_to_next,
    complete_thread_startup_syscall, switch_thread_startup_to_leader,
    handoff_child_for_trap,
    suspend_current_and_run_next, suspend_current_for_io,
    suspend_current_for_lock_wait, suspend_current_until_deadline_return,
    suspend_current_for_nanosleep, sleep_current_for_signal,
    check_sleep_deadlines,
    nearest_deadline, note_sleep_deadline, note_rt_sleeper,
    set_need_resched, set_need_resched_on, take_need_resched, need_resched_pending,
    account_enter_kernel, account_exit_kernel,
    account_syscall_combined_exit,
    HART_KERNEL_ENTER_TICK, HART_PENDING_SIGNALS, HART_CURRENT_PID,
    set_current_syscall_enter, set_current_syscall_exit,
    account_io_read, account_io_write,
    enter_wait_channel, leave_wait_channel, note_waitqueue_wake,
    wait_channel_for, account_reap_child,
    init, try_process_stats_snapshot,
};
use super::cpu_rq::{
    any_queued, lock_rq, publish_rq, push_wakelist, rq_nr_running, take_wakelist,
    unlink_wakelist, WF_SYNC, WF_TTWU,
};
use super::policy::{
    busiest_cpus, now_ns, place_entity, select_task_rq, wakeup_gran_ns, SCHED_BASE_SLICE_NS,
};
use super::process_trace::{
    cldbg_enabled, cldbg_timer_skip_permitted, wtrace_process, wtrace_save_enabled, wtrace_trap,
};
use super::smp::MAX_CPUS;

/// 全表 pending-signal 扫描提示：任何进程被置入新 pending 信号时置位；
/// `select_next` 只在置位时才做「优先调度带未决未屏蔽信号进程」的全表扫描，
/// 扫描确认全表无未决未屏蔽信号后清除。稳态无信号流量（如 hackbench 400 进程
/// 高频切换）下将该 O(n) 扫描完全短路。
pub static SIG_SCAN_HINT: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(true);

#[inline]
pub fn note_signal_hint() {
    SIG_SCAN_HINT.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// 进程 ID 类型
pub type Pid = usize;

/// 无效 PID
pub const INVALID_PID: Pid = 0;

/// 就绪队列容量上限。注意：进程表本身是无界 BTreeMap、PID 单调增长，此常量
/// 仅限定**同时可入就绪队列**的任务数。原值 256 在 hackbench（默认 10 组 = 400
/// 任务）等重负载下会溢出：enqueue_ready 队满即静默丢弃，导致后来创建的任务
/// （如 cyclictest）永远进不了就绪队列、永不被调度。1024 在 libc-bench
/// b_pthread_create（2500 线程一次性入队）积压期又会满队触发逐次 O(cap)
/// compact 风暴，提升到 4096 容纳峰值并发。
pub const MAX_PROCESSES: usize = 4096;

/// 「可能存在 runnable 的 RT/DEADLINE 任务」sticky 标志（`select_next` 快路径守门）。
/// RT/DL 任务经 `enqueue_ready` 入队时置位；`select_next` 完整优先级扫描确认无
/// RT ready 时清除（自校正：直接置 `on_ready_queue=false` 的旁路不递减任何计数，
/// 标志只会保守偏 true，绝不会漏掉 RT）。偏 true 的代价仅是多走一次完整扫描。
pub static RT_DL_PRESENT: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 是否曾经出现过 SCHED_DEADLINE 任务。`deadline::dl_timer_tick` 据此在
/// 纯 CFS/SCHED_FIFO/RR 工作负载下完全跳过周期表锁与全表扫描；一旦置位
/// 永久保持（保守），语义等价于始终 tick。
pub static DL_EVER_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[inline]
pub fn note_deadline_active() {
    DL_EVER_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
}

// [DIAG] RT 唤醒→上CPU 延迟测量（临时诊断）
pub static RT_WAKE_TS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static RT_WAKE_PID: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
pub static RT_RUN_TS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static RT_RUN_PID: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// [DIAG] 无串口扰动的分段延迟统计：每指标 (count, sum_us, max_us)。
// 指标: 0=switch→usertrapret 1=switch→sc115入口 2=switch→sc113入口 3=switch→gettime体
//       4=switch→restore跳转前 5=switch→trap入口(handler)
//       6=窗口内TLB重填回退trap(计数) 7=窗口内其他非syscall trap(us=ecode)
#[allow(clippy::declare_interior_mutable_const)]
const RTSTAT_ZERO: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static RTSTAT: [core::sync::atomic::AtomicU64; 78] = [RTSTAT_ZERO; 78];

#[inline]
pub fn rtstat_record(metric: usize, us: u64) {
    use core::sync::atomic::Ordering::Relaxed;
    RTSTAT[metric * 3].fetch_add(1, Relaxed);
    RTSTAT[metric * 3 + 1].fetch_add(us, Relaxed);
    RTSTAT[metric * 3 + 2].fetch_max(us, Relaxed);
}

pub fn rtstat_dump() {
    use core::sync::atomic::Ordering::Relaxed;
    for m in 0..26 {
        let n = RTSTAT[m * 3].load(Relaxed);
        let s = RTSTAT[m * 3 + 1].load(Relaxed);
        let mx = RTSTAT[m * 3 + 2].load(Relaxed);
        if n > 0 {
            crate::println!("[rtstat] m={} n={} avg_us={} max_us={}", m, n, s / n, mx);
        }
    }
}
pub static RT_WAKE_HOPS: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
pub static SWITCH_SEQ: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ============ CFS nice → weight table (Linux kernel/sched/core.c) ============

/// Weight for nice 0 (default). Linux NICE_0_LOAD = 1024.
pub const NICE_0_WEIGHT: u32 = 1024;

/// SCHED_RR default time slice in timer ticks.
/// Linux DEF_TIMESLICE = 100ms. At HZ=100 (10ms/tick) → 10 ticks.
pub const RR_TIMESLICE_TICKS: u32 = 10;

/// Linux `sched_prio_to_weight[40]`: maps nice values -20..+19 to scheduling
/// weights. Index = nice + 20. Higher weight → more CPU time.
/// Source: kernel/sched/core.c (Linux 6.x).
const SCHED_PRIO_TO_WEIGHT: [u32; 40] = [
    /* -20 */ 88761, 71755, 56483, 46273, 36291,
    /* -15 */ 29154, 23254, 18705, 14949, 11916,
    /* -10 */  9548,  7620,  6100,  4904,  3906,
    /*  -5 */  3121,  2501,  1991,  1586,  1277,
    /*   0 */  1024,   820,   655,   526,   423,
    /*   5 */   335,   272,   215,   172,   137,
    /*  10 */   110,    87,    70,    56,    45,
    /*  15 */    36,    29,    23,    18,    15,
];

/// Convert a nice value [-20, 19] to its CFS scheduling weight.
pub fn nice_to_weight(nice: i32) -> u32 {
    let idx = (nice + 20).clamp(0, 39) as usize;
    SCHED_PRIO_TO_WEIGHT[idx]
}

/// 默认用户栈大小（8 页 = 32KB）
pub const DEFAULT_USER_STACK_PAGES: usize = 8;

/// 运行竞赛/基准程序时预留更大的用户栈 VMA。
/// BuildStorm/rustc needs multi-MiB stacks (RUST_MIN_STACK=16MiB); 256 KiB left
/// every deep frame to auto-grow. Prefault stays small; reservation is the VMA
/// ceiling before grow — keep 2 MiB mapped so early rustc frames do not depend
/// solely on grow-from-fault.
pub const USER_STACK_RESERVED_PAGES: usize = 512; // 2 MiB

/// 预先映射栈顶附近的页，兼顾启动开销与后续按需增长。
/// 8 页（32 KiB）足够容纳 argv/envp/auxv 与程序启动早期栈；其余靠
/// 缺页按需增长。每 exec 少预清零/映射/回收 24 页（exec 画像 memset
/// 占 27%，其中大头就是栈预映射帧的清零）。
pub const USER_STACK_PREFAULT_PAGES: usize = 8; // 32 KiB

/// 默认用户栈顶地址（用户空间高地址）
/// LoongArch64: 用户程序链接在 0x120000000 附近，栈需要在更高地址
/// RISC-V64: 用户程序通常在低地址，2GB 足够
#[cfg(target_arch = "loongarch64")]
pub const DEFAULT_USER_STACK_TOP: usize = 0x7F_FFFE_0000; // ~512GB 范围内的高地址

#[cfg(not(target_arch = "loongarch64"))]
pub const DEFAULT_USER_STACK_TOP: usize = 0x8000_0000;

/// 默认用户栈底地址
pub const DEFAULT_USER_STACK_BOTTOM: usize = DEFAULT_USER_STACK_TOP - DEFAULT_USER_STACK_PAGES * PAGE_SIZE;

#[inline]
pub(super) fn is_valid_user_ptr(ptr: usize) -> bool {
    ptr != 0 && ptr < DEFAULT_USER_STACK_TOP
}

/// 默认内核栈大小（16 页 = 64KB）
/// 历史：曾为 8 页/32KB。但在 LA libcbench thread_stress 时 pthread 嵌套
/// + handle_page_fault heap-grow 递归会让内核栈用量超过 32KB，
/// 溢出下沿（LA 上通过 DMW0 直接映射物理地址，无 guard page）
/// 覆盖相邻进程（尤其是 init pid=1）的 kernel_stack 的 TrapContext，
/// 导致 init sepc 被篡改后 TLB refill fail 然后 SIGSEGV init 关机。
/// 64KB 是折衷：抵御绝大多数内核深度，又不过度浪费内存。
pub const DEFAULT_KERNEL_STACK_PAGES: usize = 16;

/// 默认内核栈大小（字节）
pub const DEFAULT_KERNEL_STACK_SIZE: usize = DEFAULT_KERNEL_STACK_PAGES * PAGE_SIZE;

/// 等待任意信号的睡眠哨兵
pub const SIGNAL_WAIT_SENTINEL: Pid = usize::MAX - 1;

/// 等待 I/O 事件的睡眠哨兵
pub const IO_WAIT_SENTINEL: Pid = usize::MAX;

/// nanosleep 定时睡眠哨兵（wakeup_channel 存放 rdtime deadline）
pub const NANOSLEEP_SENTINEL: Pid = usize::MAX - 2;

pub const DEADLINE_RETURN_SENTINEL: Pid = usize::MAX - 3;

pub const FUTEX_WOKEN_SENTINEL: Pid = usize::MAX - 4;
pub const POLL_WAIT_SENTINEL: Pid = usize::MAX - 5;
/// POSIX mq `mq_timedsend / timedreceive` 的"正在睡（待对端事件 / 超时）"哨兵。
/// `check_sleep_deadlines` 把它列入 keep_wait 集合 → 唤醒时**不**清
/// `waiting_for` / `wakeup_channel`，让 mq 自己的 re-entry 检查能识别超时。
pub const MQ_SLEEP_SENTINEL: Pid = usize::MAX - 6;
/// 被对端显式 `wake_one` 唤醒的 mq waiter 哨兵（区别于 deadline 唤醒）。
pub const MQ_WOKEN_SENTINEL: Pid = usize::MAX - 7;
pub const LOCK_WAIT_SENTINEL: Pid = usize::MAX - 8;
pub const SOCKET_WAIT_SENTINEL: Pid = usize::MAX - 9;
/// CLONE_VFORK 父等待哨兵：父睡在 clone() 返回点，仅允许"子 execve/exit"
/// 两个事件唤醒。必须占用独立 waiting_for 值——若沿用父上一次睡眠残留的
/// IO/POLL 哨兵，任何 `wake_io_waiters` 广播（如兄弟子进程退出关闭 pipe）
/// 都会把 vfork 父提前唤醒；父随即 munmap posix_spawn 子栈，仍在其上执行
/// exec 前置段的子进程立刻 SEGV_MAPERR（BuildStorm cargo 间歇崩溃根因）。
pub const VFORK_WAIT_SENTINEL: Pid = usize::MAX - 10;
pub const THREAD_STARTUP_CREDITS: u8 = 4;

static HEAP_SNAPSHOT_LEVEL: AtomicUsize = AtomicUsize::new(0);

pub(super) fn maybe_print_heap_snapshot(table: &ProcessTable, tag: &str) {
    let (used, total, count, free_total, largest_free) = crate::mm::heap_stats();
    let level = used / (8 * 1024 * 1024);
    if level < 8 || HEAP_SNAPSHOT_LEVEL.load(Ordering::Relaxed) >= level {
        return;
    }
    HEAP_SNAPSHOT_LEVEL.store(level, Ordering::Relaxed);

    let mut zombies = 0usize;
    let mut threads = 0usize;
    let mut child_refs = 0usize;
    let mut fd_slots = 0usize;
    let mut open_fds = 0usize;
    let mut vmas = 0usize;
    let mut mapped_frames = 0usize;
    let mut max_fd_slots = 0usize;
    let mut max_fd_pid = 0usize;

    for (&pid, process) in table.processes.iter() {
        if process.state == ProcessState::Zombie {
            zombies += 1;
        }
        if process.is_thread {
            threads += 1;
        }
        child_refs += process.children.len();
        fd_slots += process.fd_table.len();
        open_fds += process.fd_table.iter().filter(|fd| fd.is_some()).count();
        if process.fd_table.len() > max_fd_slots {
            max_fd_slots = process.fd_table.len();
            max_fd_pid = pid;
        }
        if let Some(ref mm) = process.mm {
            let addr_space = mm.lock();
            vmas += addr_space.vma_count();
            mapped_frames += addr_space.mapped_frame_count();
        }
    }

    crate::println!(
        "[heap-snap:{}] used={} total={} alloc_count={} free_total={} largest_free={} processes={} zombies={} threads={} child_refs={} fd_slots={} open_fds={} max_fd_pid={} max_fd_slots={} vmas={} mapped_frames={} current={}",
        tag,
        used,
        total,
        count,
        free_total,
        largest_free,
        table.processes.len(),
        zombies,
        threads,
        child_refs,
        fd_slots,
        open_fds,
        max_fd_pid,
        max_fd_slots,
        vmas,
        mapped_frames,
        table.current.unwrap_or(0),
    );
}

/// 等待信息
#[derive(Debug, Clone)]
struct WaitInfo {
    // ...
}

/// IO_WAIT 状态进程数（粗略计数，仅用于 wake_io_waiters 的 fast-path）
/// 在 PROCESS_TABLE 锁保护下增/减；外部可在不持锁时 Relaxed 读取以判断是否需要进锁。
/// 在 wake_io_waiters 内部通过实际扫描 self-heal 任何泄漏（进程被 kill 时的少计数）。
pub static IO_WAITER_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

// ============================================================================
// PROCESS_TABLE 无锁统计镜像（calc_load / procfs loadavg 真值）
// ============================================================================
//
// Linux `calc_global_load` 的 `nr_active` 不需要持 tasklist 锁逐任务枚举：
// 它由 `nr_running + nr_uninterruptible` 两个无锁计数合成。本内核此前每 5s
// 采一次 `PROCESS_TABLE` 锁再全表数状态，属于把低频采样做成全局停顿点。
// 这里以 `ProcessState` 的所有生产写入点为单点漏斗（见 `set_process_state`）
// 维护原子镜像；PCB 插入/物理移除点维护 `NR_PROCESSES`。镜像可能短暂落后
// 一次状态写入，但 Linux 的 load average 同样是定时采样的近似量，不允许为
// 精确同步而重拿中央锁。
pub static NR_PROCESSES: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static NR_STATE_RUNNING: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static NR_STATE_RUNNABLE: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// 复位全部统计镜像。仅在调度子系统初始化（单次、无并发）时调用。
pub(crate) fn reset_process_counters() {
    NR_PROCESSES.store(0, Ordering::Relaxed);
    NR_STATE_RUNNING.store(0, Ordering::Relaxed);
    NR_STATE_RUNNABLE.store(0, Ordering::Relaxed);
    IO_WAITER_COUNT.store(0, Ordering::Relaxed);
}

/// `ProcessState` 状态写入的单点漏斗：更新 lock-free 负载统计镜像。
///
/// 调用前提：PCB 已插入进程表（新 PCB 插入由 `insert`/`insert_runnable`
/// 单独计 `NR_PROCESSES` 并按其初态计状态计数）；物理移除由 `reap_or_defer`
/// 单独扣减。所有生产路径的状态赋值必须经过本函数，禁止绕过。
#[inline]
pub(crate) fn set_process_state(process: &mut ProcessControlBlock, new: ProcessState) {
    let old = process.state;
    if old == new {
        return;
    }
    match old {
        ProcessState::Running => {
            NR_STATE_RUNNING.fetch_sub(1, Ordering::Relaxed);
        }
        ProcessState::Runnable => {
            NR_STATE_RUNNABLE.fetch_sub(1, Ordering::Relaxed);
        }
        ProcessState::Sleeping if process.waiting_for == Some(IO_WAIT_SENTINEL) => {
            IO_WAITER_COUNT.fetch_sub(1, Ordering::Relaxed);
        }
        _ => {}
    }
    process.state = new;
    match new {
        ProcessState::Running => {
            NR_STATE_RUNNING.fetch_add(1, Ordering::Relaxed);
        }
        ProcessState::Runnable => {
            NR_STATE_RUNNABLE.fetch_add(1, Ordering::Relaxed);
        }
        ProcessState::Sleeping if process.waiting_for == Some(IO_WAIT_SENTINEL) => {
            IO_WAITER_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
}

#[inline]
fn account_inserted_pcb(pcb: &ProcessControlBlock) {
    NR_PROCESSES.fetch_add(1, Ordering::Relaxed);
    match pcb.state {
        ProcessState::Running => {
            NR_STATE_RUNNING.fetch_add(1, Ordering::Relaxed);
        }
        ProcessState::Runnable => {
            NR_STATE_RUNNABLE.fetch_add(1, Ordering::Relaxed);
        }
        ProcessState::Sleeping if pcb.waiting_for == Some(IO_WAIT_SENTINEL) => {
            IO_WAITER_COUNT.fetch_add(1, Ordering::Relaxed);
        }
        _ => {}
    }
}

#[inline]
fn account_removed_pcb(pcb: &ProcessControlBlock) {
    NR_PROCESSES.fetch_sub(1, Ordering::Relaxed);
    match pcb.state {
        ProcessState::Running => {
            NR_STATE_RUNNING.fetch_sub(1, Ordering::Relaxed);
        }
        ProcessState::Runnable => {
            NR_STATE_RUNNABLE.fetch_sub(1, Ordering::Relaxed);
        }
        ProcessState::Sleeping if pcb.waiting_for == Some(IO_WAIT_SENTINEL) => {
            IO_WAITER_COUNT.fetch_sub(1, Ordering::Relaxed);
        }
        _ => {}
    }
}

/// 无锁合成 Linux `nr_active = nr_running + nr_uninterruptible`。
/// `calc_load` 与 `/proc/loadavg` 的采样源，禁止再为读数抢 `PROCESS_TABLE`。
pub fn lock_free_active_tasks() -> usize {
    NR_STATE_RUNNING.load(Ordering::Relaxed)
        .saturating_add(NR_STATE_RUNNABLE.load(Ordering::Relaxed))
        .saturating_add(IO_WAITER_COUNT.load(Ordering::Relaxed))
}

/// 无锁合成 `/proc/loadavg` 第 1/4 字段的 runnable 数（Running + Runnable）。
pub fn lock_free_runnable_tasks() -> usize {
    NR_STATE_RUNNING
        .load(Ordering::Relaxed)
        .saturating_add(NR_STATE_RUNNABLE.load(Ordering::Relaxed))
}

/// `detached_zombies` 队列是否非空的 lock-free 提示。
/// 每次 syscall/trap 从用户态进入都会调 `reap_detached_zombies`；若该队列空（绝
/// 大多数情况），用本标志在加锁前直接短路，省掉一次 PROCESS_TABLE 全局锁
/// （§1.13①）。在锁保护下入队时置 true；reap 排空后按实际重置。保守：可能短暂为
/// true 而队列已空（无害，下次仍只是多走一次加锁判定）；绝不会队列非空而标志为
/// false（每个入队点都先置位）。
pub static DETACHED_ZOMBIES_PENDING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub(super) fn clear_sigchld_if_no_waitable_status(_table: &mut ProcessTable, _parent_pid: Pid) {
    // Linux: SIGCHLD pending bit is cleared by signal delivery
    // (check_and_deliver_signals), not by wait(). Clearing here before
    // the handler fires causes the handler to never run when the parent
    // is Runnable at child-exit time (e.g. clone301 test 5).
}

/// 内联 TrapContext 快照槽。相比 `Option<TrapContext>`，save/restore 都
/// 直接在 kstack 原位与本槽之间做单次 memcpy，消除 Option 值语义带来的
/// 栈中转拷贝（每次 IO 挂起+唤醒 4 次 304B 拷贝 → 2 次）。
#[derive(Clone)]
pub struct TrapSlot {
    valid: bool,
    cx: crate::arch::TrapContext,
}

impl TrapSlot {
    pub const fn empty() -> Self {
        #[cfg(target_arch = "riscv64")]
        let cx = crate::arch::TrapContext::zero();
        #[cfg(target_arch = "loongarch64")]
        let cx = crate::arch::TrapContext::zero_init();
        Self { valid: false, cx }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.valid = false;
    }

    #[inline]
    pub fn is_some(&self) -> bool {
        self.valid
    }

    #[inline]
    pub fn as_ref(&self) -> Option<&crate::arch::TrapContext> {
        if self.valid { Some(&self.cx) } else { None }
    }

    /// 从 kstack 原位单次拷入本槽。
    ///
    /// # Safety
    /// `src` 必须指向有效的 TrapContext。
    #[inline]
    pub unsafe fn save_from(&mut self, src: *const crate::arch::TrapContext) {
        core::ptr::copy_nonoverlapping(src, &mut self.cx, 1);
        self.valid = true;
    }

    /// 消费快照：单次拷回 kstack 原位。返回是否发生了恢复。
    ///
    /// # Safety
    /// `dst` 必须指向有效的 TrapContext。
    #[inline]
    pub unsafe fn take_into(&mut self, dst: *mut crate::arch::TrapContext) -> bool {
        if !self.valid {
            return false;
        }
        core::ptr::copy_nonoverlapping(&self.cx, dst, 1);
        self.valid = false;
        true
    }
}

/// IO 阻塞重放标记：TrapContext 原地留在本进程 kstack 顶（每进程独立
/// kstack，睡眠期间无人改写），只需一个 armed 标志表示"sepc 已回退到
/// ecall、唤醒后按原 kstack 上下文重放 syscall"。相比 TrapSlot 快照省去
/// 挂起/恢复各一次 ~304B memcpy（pipe/ctx 切换热路径每往返两次）。
pub struct IoWaitArm {
    armed: bool,
}

impl IoWaitArm {
    pub const fn empty() -> Self {
        Self { armed: false }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.armed = false;
    }

    #[inline]
    pub fn is_some(&self) -> bool {
        self.armed
    }

    #[inline]
    pub fn arm(&mut self) {
        self.armed = true;
    }

    /// 消费标记：返回是否处于 armed（ctx 本就在 kstack 原位，无需拷贝）。
    #[inline]
    pub fn take(&mut self) -> bool {
        let was = self.armed;
        self.armed = false;
        was
    }
}

pub(super) fn restore_wait_trap_context(process: &mut ProcessControlBlock) {
    process.io_wait_trap_context.clear();
    if let Some(saved) = process.wait_trap_context.take() {
        unsafe {
            if let Some(cx) = process.trap_context_mut() {
                *cx = saved;
            }
        }
    }
}

pub(super) fn sigchld_wakes_signal_wait(parent: &ProcessControlBlock) -> bool {
    sigchld_wakes_signal_wait_sig(parent, 17)
}

pub(super) fn sigchld_wakes_signal_wait_sig(parent: &ProcessControlBlock, sig: i32) -> bool {
    if sig < 1 || sig > 64 { return false; }
    let action = parent.signal_handlers[(sig - 1) as usize];
    (parent.signal_mask & (1u64 << (sig as u64 - 1))) == 0 && action.handler != 0 && action.handler != 1
}


/// 对标 Linux `struct fs_struct`（fs/fs_struct.c）：进程的文件系统上下文。
/// clone(CLONE_FS) 共享同一实例（pthread 默认），普通 fork 深拷贝，
/// unshare(CLONE_FS) 换私有副本。umask 住在这里与 Linux 一致（sys_umask
/// 改的是 current->fs->umask，对 CLONE_FS 共享者全组可见）。
#[derive(Clone)]
pub struct FsStruct {
    /// chroot 根。默认 "/"。
    pub root: String,
    /// 当前工作目录（始终规范形绝对路径）。
    pub cwd: String,
    /// POSIX `umask`：创建文件时清除 mode 中这些位。execve 不重置。
    pub umask: u32,
}

/// 进程控制块（扩展版）
pub struct ProcessControlBlock {
    /// 进程 ID
    pub pid: Pid,
    /// 父进程 ID
    pub parent_pid: Option<Pid>,
    /// 子进程列表
    pub children: Vec<Pid>,
    /// 进程状态
    pub state: ProcessState,
    /// Linux `task_struct::on_cpu` 等价状态；只在 `PROCESS_TABLE` 锁内修改。
    /// `Some(hart)` 表示该任务已被该 hart 独占，禁止进入就绪队列或被其他 hart 认领。
    pub on_cpu: Option<usize>,
    pub on_ready_queue: bool,
    /// 任务当前挂在哪个 hart 的 `CpuRq` 上（`None` = 不在任何就绪队列）。
    pub rq_cpu: Option<usize>,
    /// Linux `cpus_allowed`：可运行的 hart 位图。默认全 1，由
    /// `sched_setaffinity` 收窄；`enqueue_ready` / steal 必须尊重。
    pub cpus_allowed: u64,
    /// Linux `p->wake_cpu`：上次运行 / 唤醒选核提示。
    pub wake_cpu: Option<usize>,
    /// 退出状态
    pub exit_status: Option<ExitReason>,
    /// 线程组正在退出时由首个致命线程记录的组级原因。非 leader 只负责唤醒并
    /// SIGKILL leader；leader 最终退出时消费该值，避免跨栈冒充 leader。
    pub group_exit_reason: Option<ExitReason>,
    /// 进程地址空间（Arc<MmStruct> 引用计数包装，对标 Linux task->mm）。
    ///
    /// - 普通 fork：各自 Arc<MmStruct>（深拷贝 CoW AddressSpace）
    /// - CLONE_VM：Arc::clone 共享同一 MmStruct + mmget()
    /// - 内核线程 / 刚创建的 embryo：None
    pub mm: Option<Arc<crate::mm::mm_struct::MmStruct>>,
    /// 页表根物理地址（从 address_space 获取，用于快速访问）
    pub page_table_root: PhysAddr,
    /// 程序入口点
    pub entry_point: VirtAddr,
    /// 用户栈顶
    pub user_stack_top: VirtAddr,
    /// 用户栈底
    pub user_stack_bottom: VirtAddr,
    /// 堆起始地址
    pub heap_start: VirtAddr,
    /// 堆当前结束地址（brk）
    pub heap_end: VirtAddr,
    /// 程序名称
    pub name: String,
    pub comm: String,
    /// 当前进程真实可执行文件路径（供 /proc/self/exe 等语义使用）
    pub exec_path: String,
    /// Absolute path of the currently executing binary that THIS process is
    /// responsible for releasing from the exec write-deny map (ETXTBSY
    /// tracking, Linux i_writecount equivalent). Set by execve, cleared on
    /// exit / re-exec. Empty means this process holds no deny (e.g. embedded
    /// init, or a fork child that has not execve'd on its own yet — the child
    /// inherits `exec_path` for /proc/self/exe but NOT the deny ownership, so
    /// the deny count is released exactly once by the process that execve'd).
    pub exec_deny_path: String,
    /// Slice 2.2：真实化 `/proc/[pid]/cmdline` —— 保存最近一次 execve 传入
    /// 的 argv 列表（按 POSIX `cmdline` 语义，每条参数以 NUL 分隔，整体
    /// 末尾再加一 NUL）。clone/fork 时由 child 继承父的 saved_argv，
    /// execve 时由 `replace_current_process_address_space` 覆写为新 argv。
    /// init 进程默认 `vec![name]`，保证 `cat /proc/1/cmdline` 总能出
    /// 至少一个非空字符串，符合 LTP `proc01` / coreutils `ps` 的行为期待。
    /// Arc 包裹：fork 继承只加引用计数，不逐条深拷；execve 整体替换。
    pub saved_argv: Arc<Vec<String>>,
    pub uid: u32,
    pub euid: u32,
    /// Saved set-user-ID（POSIX `saved_uid`）。`setresuid(2)` 第三参写入；
    /// 默认与 `euid` 相同；`execve(2)` 对 setuid 程序设为 binary 的 owner。
    pub suid: u32,
    pub gid: u32,
    pub egid: u32,
    /// Saved set-group-ID。语义与 `suid` 对称。
    pub sgid: u32,
    /// Supplementary group IDs (`task->cred->group_info`)。
    ///
    /// Linux 中 `getgroups(2)`/`setgroups(2)` 操作的是真实 cred 组列表：
    /// fork/clone 继承，execve 保留，`setgroups(0, NULL)` 清空。init/root
    /// 默认至少属于 gid 0，这样 `getgroups()` 返回集合包含 `getgid()`。
    pub supplementary_groups: Vec<u32>,
    pub dumpable: i32,
    pub no_new_privs: bool,
    pub keepcaps: bool,
    pub thp_disable: bool,
    pub child_subreaper: bool,
    /// membarrier(2) per-mm 注册状态。Linux：注册可跨 fork 继承、execve 重置。
    /// PRIVATE_EXPEDITED / PRIVATE_EXPEDITED_SYNC_CORE 在未注册时发起屏障 → EPERM。
    pub membarrier_private_expedited: bool,
    pub membarrier_private_sync_core: bool,
    pub membarrier_global_expedited: bool,
    pub timerslack_ns: usize,
    pub default_timerslack_ns: usize,
    pub pdeathsig: i32,
    pub cap_bset: u64,
    /// Slice 2.x 真化〔去 capget/capset silent 0〕：POSIX capabilities 三集
    /// （Linux `task->cred->cap_{effective,permitted,inheritable}`）。
    /// `capset(2)` 真写、`capget(2)` 真读；fork 继承父；execve 按 file caps
    /// 规则重置（root 维持全集，非 root 清零——Linux `cap_bprm_creds_from_file`
    /// 简化路径）。
    ///
    /// Linux 是 `kernel_cap_t`（u64 mask，bit i = CAP_i）。新创建任务（init）
    /// 是 root → 全 1；fork 后 child 继承父；setuid 非 root 后按
    /// `cap_emulate_setxuid` 清零。
    pub cap_effective: u64,
    pub cap_permitted: u64,
    pub cap_inheritable: u64,
    pub cap_ambient: u64,
    /// Slice 2.5 真实化：POSIX capabilities `securebits` 位掩码（Linux
    /// `task->cred->securebits`）。`SECBIT_KEEP_CAPS / SECBIT_NO_SETUID_FIXUP /
    /// SECBIT_NOROOT` 及其 LOCKED 变体共 6 位。fork/clone 继承；execve 不重置；
    /// `prctl(PR_SET_SECUREBITS, x)` 写入，`PR_GET_SECUREBITS` 读取。LOCKED
    /// 位一旦设置无法清除（POSIX 安全约束）。
    pub securebits: u32,
    /// Slice 2.5 真实化：seccomp 模式（`task->seccomp.mode`）。
    /// 0 = SECCOMP_MODE_DISABLED（默认）
    /// 1 = SECCOMP_MODE_STRICT（仅 read/write/exit/sigreturn）
    /// 2 = SECCOMP_MODE_FILTER（带 BPF filter）
    /// `prctl(PR_SET_SECCOMP)` 或 `seccomp(2)` syscall 写入；不可降级
    /// （NO_NEW_PRIVS 锁定 + 单向升级）。fork/clone 继承；execve 不重置。
    pub seccomp_mode: u32,
    /// 真化〔去 BPF silent placeholder〕：cBPF (Classic BPF) seccomp filter 链。
    /// 每个 `Arc<SeccompFilter>` 是一段已经 verifier 通过、可立即解释执行的
    /// `struct sock_filter[]`。Linux 一个 task 可以累加（每次 SET_MODE_FILTER
    /// 或 PR_SET_SECCOMP 都把新 filter prepend 到链头，运行时按 task → parent
    /// → … 旧→新顺序跑，取最严格 action）。我们采用 push_back + 顺序遍历
    /// + `action_severity` 严格度比较，与 Linux 等价。fork/clone 共享 Arc，
    /// execve 保留（POSIX 强制：FILTER 状态跨 execve 不丢，除非 cap_setpcap
    /// 触发清除，本项目不实装该路径）。
    pub seccomp_filters: alloc::vec::Vec<alloc::sync::Arc<crate::seccomp::SeccompFilter>>,
    /// Slice 2.5 真实化：audit subsystem session id (`/proc/[pid]/sessionid`)。
    /// 默认 u32::MAX 表示 AUDIT_SID_UNSET（未通过 PAM 登录）。`auditd` 会
    /// 在 login 时通过 `audit_set_loginuid()` 关联写入此值；fork/clone 继承；
    /// execve/setuid 不重置（与 loginuid 解耦）。
    pub audit_sessionid: u32,
    /// Slice 2.5 真实化：OOM-killer 调节值 (`/proc/[pid]/oom_score_adj`)。
    /// 范围 `[-1000, 1000]`，默认 0；`-1000` 表示 OOM 不杀（OOM_SCORE_ADJ_MIN）。
    /// OOM-killer 计分时把此值加到 badness 上。fork/clone 继承；execve 不重置。
    pub oom_score_adj: i32,
    /// Slice 2.5 真实化：ELF aux 向量 (`/proc/[pid]/auxv`)。
    /// execve 时由 loader 写入 (AT_PAGESZ, AT_PHDR, AT_PHENT, AT_PHNUM, AT_ENTRY,
    /// AT_BASE, AT_FLAGS, AT_RANDOM, AT_UID, AT_EUID, AT_GID, AT_EGID, AT_SECURE,
    /// AT_PLATFORM, AT_EXECFN 等)。每项 (a_type, a_val) 16 字节小端，末尾
    /// (AT_NULL=0, 0) 终止。`getauxval(3)` / `ldd` / glibc startup 都会读。
    pub saved_auxv: Arc<Vec<(u64, u64)>>,
    /// Slice 2.5 真实化：execve 时传入的 envp 列表 (`/proc/[pid]/environ`)。
    /// 与 saved_argv 平行，但每条以 NUL 分隔（不在末尾再加 NUL，Linux 是
    /// "每条 NUL 结尾"，最后一条后**没有**额外 NUL）。execve 覆盖；fork
    /// 继承；exit 时清空。`bash -c 'cat /proc/$$/environ | tr "\\0" "\\n"'`
    /// 是常用调试手段。
    pub saved_envp: Arc<Vec<String>>,
    /// Slice 2.5 真实化：当前正在执行的 syscall (`/proc/[pid]/syscall`)。
    /// Linux 格式：`<nr> <a0> <a1> <a2> <a3> <a4> <a5> <sp> <pc>`，运行中时
    /// 显示当前 syscall + 6 个参；非 syscall 状态显示 `running`；已 zombie
    /// 显示空。我们用 PCB 字段记 (nr, [a0..a5])，sp/pc 用最后一次 trap context。
    /// dispatcher 入口 set，dispatcher 出口 clear（nr=u64::MAX 表示 running）。
    pub last_syscall: (u64, [u64; 6]),
    /// Slice 2.5 真实化：PR_SET_MM 的可调字段（Linux `prctl(PR_SET_MM, …)`
    /// 允许 root 调整这些 mm_struct 字段，影响 `/proc/[pid]/stat` 与 maps）。
    /// 字段对应 prctl 选项：
    ///   PR_SET_MM_START_CODE/END_CODE/START_DATA/END_DATA/
    ///   PR_SET_MM_START_BRK/BRK/START_STACK/
    ///   PR_SET_MM_ARG_START/ARG_END/ENV_START/ENV_END
    /// 默认 0；execve 由 ELF loader 填初值。
    pub mm_start_code: usize,
    pub mm_end_code: usize,
    pub mm_start_data: usize,
    pub mm_end_data: usize,
    pub mm_start_brk: usize,
    pub mm_brk: usize,
    pub mm_start_stack: usize,
    pub mm_arg_start: usize,
    pub mm_arg_end: usize,
    pub mm_env_start: usize,
    pub mm_env_end: usize,
    /// 所属 PID namespace 根 pid。`getpid()` 在该 ns 内的可见值
    /// 等于 `self.tgid - pidns_root + 1`（root = 1）。默认 0 表示
    /// 全局 ns（无偏移）。
    pub pidns_root: Pid,
    /// 命名空间集合掩码（CLONE_NEW* 标记位）— 占位字段，便于将来
    /// 区分 net/uts/ipc/mount ns。
    pub clone_ns_flags: u64,
    /// 真实命名空间代理 — 对标 Linux `task->nsproxy`。
    /// 每个进程持有自己的 NsProxy（通过 Arc 共享，unshare 时 clone-on-write）。
    pub nsproxy: Arc<SpinLock<crate::kernel::namespace::NsProxy>>,
    /// Cached mount namespace ID for fast context switch (avoids nsproxy lock).
    pub cached_mnt_ns_id: usize,
    /// cgroup v2 path this process belongs to (e.g. "/", "/user.slice/session-1").
    /// Fork inherits parent's cgroup_path; write to cgroup.procs moves process.
    pub cgroup_path: String,
    // ── ptrace state — per Linux task_struct ptrace fields ──
    /// PID of the tracer process (0 = not being traced)
    pub ptrace_tracer: usize,
    /// ptrace options set by PTRACE_SETOPTIONS (bit flags)
    pub ptrace_options: u32,
    /// Whether the tracee is stopped for ptrace (TASK_TRACED equivalent)
    pub ptrace_stopped: bool,
    /// ptrace request that caused the stop (SINGLESTEP/SYSCALL/signal)
    pub ptrace_stop_reason: u8,
    /// Pending signal for tracee resume (0 = no signal)
    pub ptrace_pending_sig: u32,
    /// Syscall-entry stop: true = at entry, false = at exit (for PTRACE_SYSCALL)
    pub ptrace_syscall_entry: bool,
    /// Exit signal sent to parent on child termination (Linux `task->exit_signal`).
    /// Default SIGCHLD (17) for fork; clone3 can set any signal.
    pub exit_signal: i32,
    /// 进程文件系统根（chroot 根）。
    ///
    /// Linux `chroot(2)` 修改的是进程的 fs_struct->root，而不是全局 VFS。
    /// 绝对路径名解析需要以此为根；fork/clone/exec 继承该值。默认 "/"。
    ///
    /// 对标 Linux `task_struct->fs`（fs_struct{root,pwd,umask}）：clone(CLONE_FS)
    /// 的任务共享同一实例，任一线程 chdir/chroot/umask 全组可见。
    pub fs: Arc<SpinLock<FsStruct>>,
    /// 文件描述符表 (0=stdin, 1=stdout, 2=stderr 为 None，由 UART 直接处理)
    pub fd_table: Vec<Option<Arc<crate::fs::FileDescriptor>>>,
    /// Owner PID of the descriptor table visible through this task.
    ///
    /// Normal fork creates a private table (`fd_table_owner == pid`).  Linux
    /// `clone(CLONE_FILES)` shares one descriptor table between otherwise
    /// separate tasks; in this compact kernel model sharers redirect fd
    /// operations to the owner PCB.  `close_range(CLOSE_RANGE_UNSHARE)` later
    /// materializes a private copy and resets this field to `pid`.
    pub fd_table_owner: Pid,
    /// kcmp(2) resource-share identities. A normal fork gives the child its own
    /// pid; clone(CLONE_FS / CLONE_IO / CLONE_SYSVSEM) makes the child inherit
    /// the parent's id so kcmp() reports the resource as shared even across
    /// distinct tgids (kcmp03).
    pub fs_share_id: Pid,
    pub io_share_id: Pid,
    pub sysvsem_share_id: Pid,
    /// 下一次自动分配 fd 的搜索起点
    pub fd_next_hint: usize,
    /// 等待的子进程（用于 wait）
    pub waiting_for: Option<Pid>,
    pub wait_exit_code_ptr: usize,
    pub wait_rusage_ptr: usize,
    /// `waitid` 阻塞模式下的 siginfo 用户指针。非 0 表示当前 sleep 是
    /// `sys_waitid` 触发的；exit() 唤醒 parent 时按 waitid 协议写 128 字节
    /// siginfo + a0=0（而不是按 waitpid 写 wstatus + a0=child_pid）。
    pub wait_siginfo_ptr: usize,
    /// `waitid` 的 rusage 指针（位于 a4，不是 waitpid 的 a3）。
    pub wait_rusage_ptr_waitid: usize,
    /// `waitid` P_PGID 模式下记录要等的进程组 id。`None` 表示非 pgid
    /// 等待；`Some(pgid)` 表示 exit() 唤醒此 parent 时还要校验
    /// `child.pgid == pgid` 才算可 reap，不匹配则保持 parent 继续 sleep。
    pub wait_pgid: Option<Pid>,
    /// 当前 waitpid/waitid 的线程过滤模式（`__WALL` / `__WCLONE`）。`None`
    /// 等价于 `Some(NonThreadOnly)`（默认）。exit() 唤醒 parent 时按此
    /// 字段过滤：不匹配的 child exit 不应叫醒此 waiter。
    pub wait_thread_mode: Option<ThreadMode>,
    pub wait_is_waitid: bool,
    pub wait_wuntraced: bool,
    pub wait_wcontinued: bool,
    pub wait_wnowait: bool,
    pub wait_trap_context: Option<crate::arch::TrapContext>,
    pub stopped_signal: Option<i32>,
    pub stopped_waitable: bool,
    pub continued_waitable: bool,
    /// 唤醒通道（用于睡眠/唤醒）
    pub wakeup_channel: usize,
    /// rt_sigtimedwait 的等待信号集（do_sigtimedwait 睡眠期间有效）。
    /// child-exit 路径据此判断 SIGCHLD 是否落在等待集内而唤醒等待者——
    /// sigtimedwait 语义下信号被同步 dequeue，不要求安装 handler。
    pub signal_wait_set: u64,
    pub nanosleep_rem_ptr: usize,
    pub nanosleep_trap_context: Option<crate::arch::TrapContext>,
    /// 内核栈（用于 trap 处理）
    pub kernel_stack: Option<KernelStack>,
    /// TrapContext 在内核栈上的位置
    pub trap_context_ptr: usize,
    pub signal_wait_trap_context: Option<crate::arch::TrapContext>,
    /// 信号处理函数表 (64 个信号，下标=signum-1)
    /// handler=0 → SIG_DFL, handler=1 → SIG_IGN, 其他 → 用户态函数地址
    pub signal_handlers: [SignalAction; 64],
    /// 待处理信号位图
    pub pending_signals: u64,
    pub pending_signal_fds: Vec<(i32, i32)>,
    /// 信号附带 `siginfo_t` 队列（Slice 2.0）：`rt_sigqueueinfo` /
    /// `rt_tgsigqueueinfo` 投递时拷入用户态 128 字节 siginfo_t；SA_SIGINFO
    /// handler 投递时按 sig 编号 FIFO 取出，覆盖默认 4×u64 填充，把
    /// `si_value / si_pid / si_uid` 等真实字段还原给用户态。
    ///
    /// 限长 32 — 与 Linux RLIMIT_SIGPENDING 默认下限相近；超出按 FIFO 丢弃
    /// 最早项（POSIX 容许在 RLIMIT_SIGPENDING 满时返回 EAGAIN，但我们
    /// 暂以"丢弃 + 仍投信号"为权宜，保证不漏 deliver）。
    pub pending_siginfo: alloc::collections::VecDeque<(i32, [u8; 128])>,
    /// 信号屏蔽字
    pub signal_mask: u64,
    /// 信号处理前保存的 TrapContext（用于 sigreturn 恢复）
    pub saved_trap_context: Option<[usize; 32]>,
    /// 信号处理前保存的信号掩码
    pub saved_signal_mask: Option<u64>,
    /// 投递信号 handler 时的用户栈指针（handler 入口 sp）。用于检测 handler
    /// 经由 siglongjmp 退出（不走 sigreturn）时 saved_trap_context 已失效：
    /// 若后续投递信号时用户 sp 已回退到此值之上，则判定 handler 已被放弃。
    pub signal_handler_sp: usize,
    /// sigsuspend 期间临时替换 signal_mask 之前的原 mask；
    /// signal handler 投递与 sigreturn 必须用此值恢复，避免被 nested handler 覆盖。
    pub sigsuspend_restore_mask: Option<u64>,
    /// 当前正在处理的信号的 action flags（用于 SA_RESTART 判断）
    pub current_signal_flags: usize,
    /// 备用信号栈基址（sigaltstack ss_sp）
    pub sigaltstack_sp: usize,
    /// 备用信号栈大小（sigaltstack ss_size）
    pub sigaltstack_size: usize,
    /// 备用信号栈标志（SS_DISABLE=2, SS_ONSTACK=1）
    pub sigaltstack_flags: u32,
    /// 线程组 leader PID (tgid)。对主线程 tgid == pid，对子线程 tgid == leader 的 pid。
    pub tgid: Pid,
    /// clear_child_tid 地址（CLONE_CHILD_CLEARTID 线程退出时写 0 并 futex wake）
    pub clear_child_tid: usize,
    /// 是否为线程（通过 CLONE_THREAD 创建）
    pub is_thread: bool,
    /// clone 后 child-runs-first: 需要在下次 trap 返回时让出给此子进程
    pub yield_to_child: Option<Pid>,
    /// CLONE_VFORK: parent blocked until child exits or execs
    pub vfork_waiting_for: Option<Pid>,
    // shared_vm_parent 已删除——被 Arc<MmStruct> 天然共享替代，
    // 不再需要 O(N) 遍历推导地址空间所有者。
    pub startup_credits: u8,
    /// RLIMIT_NOFILE 动态值 (soft limit)
    pub rlimit_nofile: usize,
    /// RLIMIT_NOFILE hard limit
    pub rlimit_nofile_max: usize,
    /// robust futex list head (set_robust_list 保存)
    pub robust_list_head: usize,
    /// robust futex list len
    pub robust_list_len: usize,
    /// ITIMER_REAL alarm deadline (rdtime ticks, 0=disabled)
    pub alarm_deadline: usize,
    /// ITIMER_REAL interval (rdtime ticks, 0=one-shot)
    pub itimer_real_interval: u64,
    /// Slice 2.x C13 真化：ITIMER_VIRTUAL —— 基于 `cpu_user_ticks` 累积。
    /// `itimer_virtual_deadline` = 触发时刻的 cpu_user_ticks 目标值
    /// （0 = disabled）；`itimer_virtual_interval` = 到期后重置量
    /// （0 = one-shot，与 Linux `setitimer` 一致）。到期投递 SIGVTALRM(26)。
    pub itimer_virtual_deadline: u64,
    pub itimer_virtual_interval: u64,
    /// Slice 2.x C13 真化：ITIMER_PROF —— 基于
    /// `cpu_user_ticks + cpu_kernel_ticks` 累积；到期投递 SIGPROF(27)。
    pub itimer_prof_deadline: u64,
    pub itimer_prof_interval: u64,
    /// SysV 共享内存 attachments: (shmid, base_va)
    pub shm_attachments: Vec<(i32, usize)>,
    /// FD_CLOEXEC 位图快路径（fd 0-63），每 bit 对应一个 fd 的 close-on-exec 标志
    pub cloexec_bits: u64,
    /// FD_CLOEXEC 高 fd 扩展集合（fd >= 64）。
    ///
    /// Linux 的 close-on-exec 是 per descriptor-table-entry 状态，不能放在
    /// `FileDescriptor`/open-file-description 上；否则 dup/fork 后会错误共享。
    pub cloexec_fds: BTreeSet<usize>,
    pub did_exec: bool,
    pub mempolicy_mode: i32,
    pub mempolicy_nodemask: u64,
    pub mbind_policies: Vec<(usize, usize, i32, u64)>,
    /// 进程组 ID（默认 = pid，fork 时继承父进程的 pgid）
    pub pgid: Pid,
    /// 会话 ID（默认 = pid，fork 时继承父进程的 sid）
    pub sid: Pid,
    pub sched_policy: i32,
    pub sched_priority: i32,
    /// SCHED_RESET_ON_FORK：fork 出的子进程强制回 SCHED_NORMAL/非负 nice，
    /// 且子进程清除此标志（对标 Linux sched(7)）。
    pub sched_reset_on_fork: bool,
    /// SCHED_DEADLINE parameters (对标 Linux sched_dl_entity)
    pub dl_runtime: u64,
    pub dl_deadline: u64,
    pub dl_period: u64,
    /// POSIX nice 值（`PRIO_PROCESS`），范围 `[-20, 19]`，越小越优先。
    /// 0 = 默认；root 才能调到负值（与 Linux 一致）。fork/clone 时继承。
    /// 由 `setpriority(2)` 写入，被 `getpriority(2)`（返回 `20 - nice`）、
    /// `getrusage` 等读取。
    pub nice: i32,
    /// CFS scheduling weight derived from nice value via `nice_to_weight()`.
    /// Linux kernel/sched/core.c `sched_prio_to_weight[40]`.
    /// nice 0 = 1024 (NICE_0_WEIGHT). Higher weight → more CPU time.
    /// Updated whenever `nice` changes. fork/clone inherits. execve preserves.
    pub sched_weight: u32,
    /// CFS virtual runtime in nanoseconds. Processes with lower vruntime get
    /// scheduled first. Incremented by `delta_exec * NICE_0_WEIGHT / weight`
    /// on each timer tick. fork inherits parent's vruntime.
    /// Per Linux kernel/sched/fair.c `curr->vruntime += calc_delta_fair(delta)`.
    pub vruntime: u64,
    /// EEVDF 虚拟截止时间（`se->deadline`）。
    pub deadline: u64,
    /// EEVDF 请求时间片（`se->slice`，ns）。
    pub slice_ns: u64,
    /// EEVDF lag（`avg_vruntime - vruntime`，`place_entity` 写入）。
    pub vlag: i64,
    /// Linux per-task `personality(2)` 状态。0 = PER_LINUX，其余按
    /// `<sys/personality.h>` 定义（低 8 位是 persona 类型，高 24 位是
    /// `ADDR_*` / `READ_IMPLIES_EXEC` 等 flag 位）。fork/clone 继承；
    /// `execve(2)` 不重置（与 `task_struct.personality` 一致）。
    pub personality: u32,
    /// 16 项 RLIMIT 数组 `[ (rlim_cur, rlim_max); 16 ]`，索引与 Linux
    /// `<sys/resource.h>` 的 `RLIMIT_*` 完全一致：
    ///  0=CPU 1=FSIZE 2=DATA 3=STACK 4=CORE 5=RSS 6=NPROC 7=NOFILE
    ///  8=MEMLOCK 9=AS 10=LOCKS 11=SIGPENDING 12=MSGQUEUE 13=NICE
    ///  14=RTPRIO 15=RTTIME
    ///
    /// `RLIMIT_NOFILE`（idx 7）与 `rlimit_nofile / rlimit_nofile_max` 字段镜像，
    /// 写 NOFILE 时必须同时更新双方，以兼容 `fd_table.rs` 与 `fcntl12` 已有代码。
    pub rlimits: [(u64, u64); 16],
    /// 累积用户态 CPU 时间（rdtime ticks）。
    pub cpu_user_ticks: u64,
    /// 累积内核态 CPU 时间（rdtime ticks）。
    pub cpu_kernel_ticks: u64,
    /// 上次因 RLIMIT_CPU 软限投递 SIGXCPU 时的整秒值（0=从未）。
    /// 用于「软限到达后每秒投递一次 SIGXCPU」的限速，对标 Linux
    /// `check_cpu_itimer`/`run_posix_cpu_timers` 的软限逐秒重发。
    pub rlimit_cpu_xcpu_sec: u64,
    /// 已 reap 子进程累积的用户态时间（用于 sys_times tms_cutime）。
    pub cpu_children_user_ticks: u64,
    /// 已 reap 子进程累积的内核态时间。
    pub cpu_children_kernel_ticks: u64,
    /// 本进程驻留集峰值 (KiB)，退出时由 `compute_rss_kb` 结算。
    /// 对应 getrusage(RUSAGE_SELF).ru_maxrss 的高水位语义。
    pub maxrss_kb: u64,
    /// 已 reap 子进程(及其后代)的 ru_maxrss 取最大值 (KiB)，对应
    /// getrusage(RUSAGE_CHILDREN).ru_maxrss。fork 不继承(由 `new` 置 0)。
    pub children_maxrss_kb: u64,
    /// 进程上次被 `switch_to` 切入 CPU 时的 rdtime，用来在切出时
    /// 累加 (now - this) 到对应的 user/kernel bucket。
    pub cpu_last_switched_in_tick: u64,
    /// 本次被 `switch_to` 切入 CPU 时的调度时钟（`policy::now_ns()` 纳秒）。
    /// 切出时用它与 `now_ns()` 的差给 CFS vruntime 结账；与上面的 rdtime
    /// cputime 游标分离，保证 EEVDF 单位在两架构上都是纳秒。
    pub sched_last_in_ns: u64,
    /// 精确 cputime 分账开关：见 `process_sched::current_cputime_precise`。
    /// fork 继承（times03 等先 times() 再 fork 的用例依赖子进程精确拆分）。
    pub cputime_precise: bool,
    /// 调度时间片起点：本次被 `switch_to` 真正切入 CPU 时的 rdtime。
    /// 与 `cpu_last_switched_in_tick` 分离 —— 后者是 cputime 记账游标，
    /// 会被每次 trap 的 account_enter/exit_kernel 重置，不能用来衡量时间片。
    /// 对应 Linux 中 cputime 记账与 `task_tick` 时间片判定相互独立的语义。
    pub sched_slice_start_tick: u64,
    /// 上次从用户态进入内核（ecall / 中断）时的 rdtime；
    /// 进入内核时由 trap 入口设置，离开内核时累计差值到 user/kernel bucket。
    pub cpu_kernel_entry_tick: u64,
    /// 当前是否在内核态执行（true：cpu_kernel_entry_tick 之后所累计的 ticks 应归 kernel）。
    pub cpu_in_kernel: bool,
    /// Slice 2.2：进程启动时刻（rdtime ticks 绝对值，与 `cpu_*_ticks` 同基准）。
    /// `/proc/[pid]/stat` 第 22 个字段 `starttime`（POSIX `proc(5)`：自 boot 起的
    /// 时钟滴答数，单位 USER_HZ=100）。由 PCB::new 在创建瞬间快照 rdtime，
    /// fork/clone/exec 全部沿用 child 自身 PCB::new 的快照（与 Linux 一致：
    /// execve 不重置 starttime；fork 后 child 用 child 创建时刻）。
    pub start_time_ticks: u64,
    /// 进程"实例"唯一标识（全局单调递增，PCB::new 瞬刻分配）。与数值 PID 不同，
    /// PID 在进程退出回收后可被复用（见 `alloc_pid` + ns_last_pid），而 `instance_id`
    /// 永不重复，用于把 pidfd 绑定到具体进程实例：原实例退出后 pidfd 即便数值 PID
    /// 被新进程复用也应判定为陈旧（ESRCH）。对标 Linux `struct pid` 引用语义。
    pub instance_id: u64,
    /// Slice 2.2：协作让出（sched_yield / 主动 sleep / 等 IO 进 wait queue）
    /// 触发的上下文切换累计。`/proc/[pid]/status: voluntary_ctxt_switches`。
    pub voluntary_ctxt_switches: u64,
    /// Slice 2.2：抢占式切换累计（时间片到期被换下 / 高优先级抢占）。
    /// `/proc/[pid]/status: nonvoluntary_ctxt_switches`。
    pub involuntary_ctxt_switches: u64,
    /// Slice 2.2 IO 计费（`/proc/[pid]/io`，Linux `task_io_accounting`）：
    /// `rchar` —— 用户层 `read(2)` / `pread(2)` 等成功读取的字节累计（包括
    /// 从 pipe/socket/fifo/tty 读出的字节，不仅 disk）。
    pub io_rchar: u64,
    /// `wchar` —— 用户层 `write(2)` 等成功写出的字节累计。
    pub io_wchar: u64,
    /// `syscr` —— 进程发起的 read 类 syscall 次数（read/readv/pread/preadv 等）。
    pub io_syscr: u64,
    /// `syscw` —— 进程发起的 write 类 syscall 次数。
    pub io_syscw: u64,
    /// `read_bytes` —— 真正从底层存储读出的字节（Linux 排除 cache hit；
    /// 我们没有 page cache 层，与 `rchar` 暂同步累计 disk-backed 读取）。
    pub io_read_bytes: u64,
    /// `write_bytes` —— 真正写到底层存储的字节（同上语义说明）。
    pub io_write_bytes: u64,
    /// I/O scheduling priority (ioprio_set/ioprio_get).
    /// Format: (class << 13) | level. None = never set (return default BE 4).
    pub ioprio: Option<u16>,
    /// Slice 2.2：进程当前等待原因（`/proc/[pid]/wchan` 显示给 ps/top 的
    /// 简短符号名）。Linux 是内核 RIP→符号查找；我们用调度路径主动写入
    /// 形如 `"sys_read"`/`"sys_poll"`/`"futex_wait"` 的语义标签。
    /// `None` 表示 RUNNABLE/RUNNING（`/proc/[pid]/wchan` 输出 `0`）。
    pub wait_channel: Option<&'static str>,
    pub io_wait_trap_context: IoWaitArm,
    /// Linux waitqueue wakeups are edge-sensitive but must not be lost if the
    /// waker runs after a task enqueues its WaitEntry and before that task has
    /// completed the transition to Sleeping.  In that pre-sleep window the task
    /// is still Running/Runnable, so `wake_up()` must be recorded without
    /// corrupting scheduler state.  `suspend_current_for_io_inner()` consumes
    /// this flag and immediately restarts the blocked syscall instead of
    /// sleeping.
    pub waitqueue_woken: bool,
    /// Slice 2.2：审计登录 uid（`/proc/[pid]/loginuid`），Linux 由 PAM
    /// 在 login(1) 后写入；fork/setuid 都不重置（与 euid/suid 解耦）。
    /// `u32::MAX` 对应 Linux `INVALID_UID` 语义（尚未登录）。
    pub loginuid: u32,
    /// Landlock filesystem access-control domain.
    /// Per Linux: inherited on fork, layered on restrict_self, not reset on execve.
    pub landlock_domain: crate::security::landlock::LandlockDomain,
    /// Priority Inheritance boost: when a high-priority waiter blocks on a
    /// PI futex owned by this task, we temporarily boost sched_priority to
    /// prevent priority inversion. 0 = no boost (normal). Per Linux
    /// kernel/locking/rtmutex.c rt_mutex_adjust_prio().
    /// On FUTEX_UNLOCK_PI or owner death: reset to 0 (effective priority
    /// falls back to the base sched_priority).
    pub pi_boost_priority: i32,
}

/// 信号处理动作（对应 Linux struct sigaction）
#[derive(Clone, Copy)]
pub struct SignalAction {
    /// 处理函数地址（0=SIG_DFL, 1=SIG_IGN, 其他=用户态handler）
    pub handler: usize,
    /// 信号标志（SA_RESTORER=0x04000000, SA_SIGINFO=4 等）
    pub flags: usize,
    /// 信号恢复函数（sa_restorer，musl 传入自己的 sigreturn 跳板）
    pub restorer: usize,
    /// 信号处理期间的额外屏蔽字
    pub mask: u64,
}

impl ProcessControlBlock {
    /// 统计当前驻留集 (present 物理页 × 页大小)，单位 KiB。遍历所有 VMA 的
    /// 已映射页，与 procfs smaps 的 RSS 口径一致。无 mm 时返回 0。
    pub fn compute_rss_kb(&self) -> u64 {
        let Some(ref mm) = self.mm else { return 0 };
        let page_sz = crate::mm::PAGE_SIZE;
        let space = mm.lock();
        let present: usize = space.vmas_iter().map(|vma| vma.resident_pages()).sum();
        (present as u64) * (page_sz as u64) / 1024
    }

    /// 创建新进程控制块
    pub fn new(pid: Pid) -> Self {
        Self::new_inner(pid, true)
    }

    /// 同 `new` 但不安装默认 stdio fd 表：fork/clone 会立即用父表覆盖，
    /// 省掉 6 次 Arc 分配 + 计数退还（这些构造项从未对用户态可见）。
    /// 直接在 Box 内构造（inline(always) 让 LLVM 把构造直写进堆块，
    /// 避免 ~5KB 的 PCB 先在栈上构造再 memcpy 进 Box）。
    /// 堆块优先从 PCB 回收池取（对标 Linux task_struct 的 SLAB 缓存），
    /// 命中时免每次 fork/clone 的 ~5KB 堆分配/释放。
    pub fn new_boxed_for_fork(pid: Pid) -> alloc::boxed::Box<Self> {
        if let Some(ptr) = pcb_pool_pop() {
            unsafe {
                core::ptr::write(ptr, Self::new_inner(pid, false));
                alloc::boxed::Box::from_raw(ptr)
            }
        } else {
            alloc::boxed::Box::new(Self::new_inner(pid, false))
        }
    }

    pub fn new_for_fork(pid: Pid) -> Self {
        Self::new_inner(pid, false)
    }

    /// fork 路径的 nsproxy 占位符：fork_with_ctx 随即用父 nsproxy 覆盖，
    /// 构造期不必真建一份 NsProxy（init_ns() 每次分配主机名 String、
    /// 各 ns 的 map、并消耗新 NsId）。全局常驻单例只加引用计数。
    /// fork/clone 路径的 fs_struct 占位符：随即被父 fs（共享或深拷贝）覆盖，
    /// 构造期不必真分配。全局常驻单例只加引用计数。
    fn placeholder_fs() -> Arc<SpinLock<FsStruct>> {
        static SLOT: SpinLock<Option<Arc<SpinLock<FsStruct>>>> = SpinLock::new(None);
        let mut g = SLOT.lock();
        if let Some(p) = g.as_ref() {
            return p.clone();
        }
        let p = Arc::new(SpinLock::new(FsStruct {
            root: String::new(),
            cwd: String::new(),
            umask: 0o022,
        }));
        *g = Some(p.clone());
        p
    }

    fn placeholder_nsproxy() -> Arc<SpinLock<crate::kernel::namespace::NsProxy>> {
        static SLOT: SpinLock<Option<Arc<SpinLock<crate::kernel::namespace::NsProxy>>>> =
            SpinLock::new(None);
        let mut g = SLOT.lock();
        if let Some(p) = g.as_ref() {
            return p.clone();
        }
        let p = Arc::new(SpinLock::new(crate::kernel::namespace::NsProxy::init_ns()));
        *g = Some(p.clone());
        p
    }

    #[inline(always)]
    fn new_inner(pid: Pid, with_stdio: bool) -> Self {
        Self {
            pid,
            parent_pid: None,
            children: Vec::new(),
            state: ProcessState::Embryo,
            on_cpu: None,
            on_ready_queue: false,
            rq_cpu: None,
            cpus_allowed: u64::MAX,
            wake_cpu: None,
            exit_status: None,
            group_exit_reason: None,
            mm: None,
            page_table_root: PhysAddr(0),
            entry_point: VirtAddr(0),
            user_stack_top: VirtAddr(DEFAULT_USER_STACK_TOP),
            user_stack_bottom: VirtAddr(DEFAULT_USER_STACK_BOTTOM),
            heap_start: VirtAddr(0),
            heap_end: VirtAddr(0),
            name: String::new(),
            comm: String::new(),
            exec_path: String::new(),
            exec_deny_path: String::new(),
            saved_argv: Arc::new(Vec::new()),
            uid: 0,
            euid: 0,
            suid: 0,
            gid: 0,
            egid: 0,
            sgid: 0,
            // fork 路径：随即被父值覆盖，免分配
            supplementary_groups: if with_stdio { vec![0] } else { Vec::new() },
            dumpable: 1,
            no_new_privs: false,
            keepcaps: false,
            thp_disable: false,
            child_subreaper: false,
            membarrier_private_expedited: false,
            membarrier_private_sync_core: false,
            membarrier_global_expedited: false,
            timerslack_ns: 50_000,
            default_timerslack_ns: 50_000,
            pdeathsig: 0,
            cap_bset: u64::MAX,
            // 真化 cap 三集默认：init 是 root → 全 1。普通子进程 fork 继承父。
            cap_effective: u64::MAX,
            cap_permitted: u64::MAX,
            cap_inheritable: 0,
            cap_ambient: 0,
            // Slice 2.5 真实化默认值（与 Linux task_struct 新创建任务一致）
            securebits: 0,
            seccomp_mode: 0,
            seccomp_filters: alloc::vec::Vec::new(),
            audit_sessionid: u32::MAX, // AUDIT_SID_UNSET
            oom_score_adj: 0,
            saved_auxv: Arc::new(Vec::new()),
            saved_envp: Arc::new(Vec::new()),
            // u64::MAX 表示"非 syscall 状态"（procfs 输出 "running"）
            last_syscall: (u64::MAX, [0; 6]),
            mm_start_code: 0,
            mm_end_code: 0,
            mm_start_data: 0,
            mm_end_data: 0,
            mm_start_brk: 0,
            mm_brk: 0,
            mm_start_stack: 0,
            mm_arg_start: 0,
            mm_arg_end: 0,
            mm_env_start: 0,
            mm_env_end: 0,
            pidns_root: 0,
            clone_ns_flags: 0,
            nsproxy: if with_stdio {
                Arc::new(SpinLock::new(crate::kernel::namespace::NsProxy::init_ns()))
            } else {
                // fork 路径：随即被父 nsproxy 覆盖，免真构造
                Self::placeholder_nsproxy()
            },
            // 必须与上面的 nsproxy 一致：`switch_to` 用这个缓存值设置
            // `ACTIVE_MNT_NS`，留 0 会让 `map_for(0)=None`，于是该进程在 CPU 上
            // 时 /proc、/dev、/tmp 等挂载点全部不可见（init 读 /proc/cmdline 拿
            // 到 0 字节、拿不到 SINGLE_CMD 即此故障）。fork/clone 路径随后会用
            // 父进程的真实 id 覆盖。
            cached_mnt_ns_id: if with_stdio {
                crate::fs::mount::INIT_MNT_NS
            } else {
                0
            },
            cgroup_path: if with_stdio { String::from("/") } else { String::new() },
            ptrace_tracer: 0,
            ptrace_options: 0,
            ptrace_stopped: false,
            ptrace_stop_reason: 0,
            ptrace_pending_sig: 0,
            ptrace_syscall_entry: false,
            exit_signal: 17, // SIGCHLD default
            fs: if with_stdio {
                Arc::new(SpinLock::new(FsStruct {
                    root: String::from("/"),
                    cwd: String::from("/"),
                    umask: 0o022,
                }))
            } else {
                Self::placeholder_fs()
            },
            fd_table: if with_stdio {
                use alloc::sync::Arc;
                use crate::fs::{FileDescriptor, stdio::{Stdin, Stdout}};
                let table = vec![
                    Some(Arc::new(FileDescriptor::new(Arc::new(Stdin), 0o0))),   // 0: stdin  O_RDONLY
                    Some(Arc::new(FileDescriptor::new(Arc::new(Stdout), 0o1))),  // 1: stdout O_WRONLY
                    Some(Arc::new(FileDescriptor::new(Arc::new(Stdout), 0o1))),  // 2: stderr O_WRONLY
                ];
                for fd_obj in table.iter().flatten() {
                    fd_obj.inc_fd_ref();
                }
                table
            } else {
                Vec::new()
            },
            fd_table_owner: pid,
            fs_share_id: pid,
            io_share_id: pid,
            sysvsem_share_id: pid,
            fd_next_hint: 3,
            waiting_for: None,
            wait_exit_code_ptr: 0,
            wait_rusage_ptr: 0,
            wait_siginfo_ptr: 0,
            wait_rusage_ptr_waitid: 0,
            wait_pgid: None,
            wait_thread_mode: None,
            wait_is_waitid: false,
            wait_wuntraced: false,
            wait_wcontinued: false,
            wait_wnowait: false,
            wait_trap_context: None,
            stopped_signal: None,
            stopped_waitable: false,
            continued_waitable: false,
            wakeup_channel: 0,
            signal_wait_set: 0,
            nanosleep_rem_ptr: 0,
            nanosleep_trap_context: None,
            kernel_stack: None,
            trap_context_ptr: 0,
            signal_wait_trap_context: None,
            io_wait_trap_context: IoWaitArm::empty(),
            signal_handlers: [SignalAction { handler: 0, flags: 0, restorer: 0, mask: 0 }; 64],
            pending_signals: 0,
            pending_signal_fds: Vec::new(),
            pending_siginfo: alloc::collections::VecDeque::new(),
            signal_mask: 0,
            saved_trap_context: None,
            saved_signal_mask: None,
            signal_handler_sp: 0,
            sigsuspend_restore_mask: None,
            current_signal_flags: 0,
            sigaltstack_sp: 0,
            sigaltstack_size: 0,
            sigaltstack_flags: 2, // SS_DISABLE — 默认无备用栈
            tgid: pid,
            clear_child_tid: 0,
            is_thread: false,
            yield_to_child: None,
            vfork_waiting_for: None,
            // shared_vm_parent removed — Arc<MmStruct> handles sharing
            startup_credits: 0,
            rlimit_nofile: 1024,
            rlimit_nofile_max: 4096,
            robust_list_head: 0,
            robust_list_len: 0,
            alarm_deadline: 0,
            itimer_real_interval: 0,
            itimer_virtual_deadline: 0,
            itimer_virtual_interval: 0,
            itimer_prof_deadline: 0,
            itimer_prof_interval: 0,
            shm_attachments: Vec::new(),
            cloexec_bits: 0,
            cloexec_fds: BTreeSet::new(),
            did_exec: false,
            mempolicy_mode: 0,
            mempolicy_nodemask: 0,
            mbind_policies: Vec::new(),
            pgid: pid,  // 默认进程组 ID = 自身 pid
            sid: pid,   // 默认会话 ID = 自身 pid
            sched_policy: 0,
            sched_reset_on_fork: false,
            sched_priority: 0,
            dl_runtime: 0,
            dl_deadline: 0,
            dl_period: 0,
            nice: 0,
            sched_weight: NICE_0_WEIGHT,
            vruntime: 0,
            deadline: 0,
            slice_ns: SCHED_BASE_SLICE_NS,
            vlag: 0,
            personality: 0,
            // RLIMIT 默认值参考 musl / glibc / Linux init defaults：
            //   CPU/FSIZE/DATA/CORE/RSS/MEMLOCK/AS/LOCKS/MSGQUEUE/NICE/RTPRIO/RTTIME → infinity
            //   STACK   = 8 MiB
            //   NPROC   = 65535
            //   NOFILE  = (1024, 4096)，与 Linux 内核 init 默认 INR_OPEN_CUR/
            //             INR_OPEN_MAX 一致；与 rlimit_nofile/_max 同步。LTP pipe15
            //             需要 rlim_max >= pipe_count*2+3（默认 2051）。
            //   SIGPENDING = 65535
            rlimits: {
                let inf = u64::MAX;
                let mut r = [(inf, inf); 16];
                // Soft 32 MiB: matches STACK_AUTO_GROW_WINDOW and rustc's
                // RUST_MIN_STACK=16MiB guidance (BuildStorm LA SIGSEGV).
                r[3]  = (32 * 1024 * 1024, inf);           // STACK
                r[4]  = (0, inf);                          // CORE: 软限 0（同 Linux 默认，不产 core）
                r[6]  = (65535, 65535);                    // NPROC
                r[7]  = (1024, 4096);                      // NOFILE
                r[11] = (65535, 65535);                    // SIGPENDING
                r
            },
            cpu_user_ticks: 0,
            cpu_kernel_ticks: 0,
            rlimit_cpu_xcpu_sec: 0,
            cpu_children_user_ticks: 0,
            cpu_children_kernel_ticks: 0,
            maxrss_kb: 0,
            children_maxrss_kb: 0,
            cpu_last_switched_in_tick: 0,
            sched_last_in_ns: 0,
            cputime_precise: false,
            sched_slice_start_tick: 0,
            cpu_kernel_entry_tick: 0,
            cpu_in_kernel: false,
            // Slice 2.2: 真启动时间 = PCB::new 瞬刻的 rdtime 快照。Linux
            // `/proc/[pid]/stat[22]` 用 boot-relative ticks，但相对值 OK，
            // 因为同基准（busybox `uptime` / `ps` 只算差值）。
            start_time_ticks: crate::arch::asm_helpers::rdtime(),
            instance_id: {
                static NEXT_INSTANCE_ID: core::sync::atomic::AtomicU64 =
                    core::sync::atomic::AtomicU64::new(1);
                NEXT_INSTANCE_ID.fetch_add(1, core::sync::atomic::Ordering::SeqCst)
            },
            voluntary_ctxt_switches: 0,
            involuntary_ctxt_switches: 0,
            io_rchar: 0,
            io_wchar: 0,
            io_syscr: 0,
            io_syscw: 0,
            io_read_bytes: 0,
            io_write_bytes: 0,
            ioprio: None,
            wait_channel: None,
            waitqueue_woken: false,
            // INVALID_UID == (uid_t)-1 == u32::MAX, 与 Linux 一致表示未设置
            loginuid: u32::MAX,
            landlock_domain: crate::security::landlock::LandlockDomain::new(),
            pi_boost_priority: 0,
        }
    }

    /// 当前 cwd 快照（克隆）。热路径请直接 `self.fs.lock()` 在锁内使用。
    pub fn cwd(&self) -> String {
        self.fs.lock().cwd.clone()
    }

    /// 当前 chroot 根快照（克隆）。
    pub fn fs_root(&self) -> String {
        self.fs.lock().root.clone()
    }

    /// chroot 根是否为普通 "/"（路径解析快路径判定）。
    pub fn fs_root_is_plain(&self) -> bool {
        self.fs.lock().root == "/"
    }

    pub fn umask(&self) -> u32 {
        self.fs.lock().umask
    }

    pub fn set_comm(&mut self, value: &str) {
        self.comm.clear();
        let mut used = 0usize;
        for ch in value.chars() {
            let len = ch.len_utf8();
            if used + len > 15 {
                break;
            }
            self.comm.push(ch);
            used += len;
        }
    }

    pub fn set_comm_from_name(&mut self, name: &str) {
        let base = match name.rsplit('/').next() {
            Some(s) if !s.is_empty() => s,
            _ => name,
        };
        self.set_comm(base);
    }

    pub fn clear_wait_state(&mut self) {
        // 仍在 Sleeping 时直接清 IO_WAIT 哨兵，必须同步无锁负载镜像；
        // 已经经 set_process_state 转出 Sleeping 的路径不在这里扣（漏斗已扣）。
        if self.state == ProcessState::Sleeping
            && self.waiting_for == Some(IO_WAIT_SENTINEL)
        {
            IO_WAITER_COUNT.fetch_sub(1, Ordering::Relaxed);
        }
        self.waiting_for = None;
        self.wakeup_channel = 0;
        self.signal_wait_set = 0;
        self.nanosleep_rem_ptr = 0;
        self.nanosleep_trap_context = None;
        self.wait_channel = None;
        self.waitqueue_woken = false;
        self.wait_wuntraced = false;
        self.wait_wcontinued = false;
    }

    /// 创建进程地址空间
    ///
    /// 为进程创建独立的页表和用户栈。
    ///
    /// # 返回
    /// - `Ok(())`: 创建成功
    /// - `Err(&'static str)`: 创建失败
    pub fn create_address_space(&mut self) -> Result<(), &'static str> {
        // 创建新的地址空间（包含独立页表）
        let mut address_space = AddressSpace::new();

        // 分配用户栈（使用默认大小）
        self.allocate_user_stack_with_size(&mut address_space, USER_STACK_RESERVED_PAGES)?;

        // 更新页表根地址
        self.page_table_root = PhysAddr(address_space.page_table().root_ppn().0 << 12);

        // 包装为 Arc<MmStruct>（mm_users=1, mm_count=1）
        self.mm = Some(Arc::new(crate::mm::mm_struct::MmStruct::new(address_space)));

        Ok(())
    }

    /// 分配用户栈（内部使用，默认大小）
    ///
    /// 在用户空间高地址分配栈空间。
    ///
    /// # 参数
    /// - `address_space`: 进程地址空间
    ///
    /// # 返回
    /// - `Ok(())`: 分配成功
    /// - `Err(&'static str)`: 分配失败
    #[allow(dead_code)]
    fn allocate_user_stack(&mut self, address_space: &mut AddressSpace) -> Result<(), &'static str> {
        self.allocate_user_stack_with_size(address_space, DEFAULT_USER_STACK_PAGES)?;
        Ok(())
    }

    /// 分配指定大小的用户栈
    ///
    /// # 参数
    /// - `address_space`: 进程地址空间
    /// - `stack_pages`: 栈页数
    ///
    /// # 返回
    /// - `Ok(VirtAddr)`: 栈顶地址
    /// - `Err(&'static str)`: 分配失败
    pub fn allocate_user_stack_with_size(
        &mut self,
        address_space: &mut AddressSpace,
        stack_pages: usize,
    ) -> Result<VirtAddr, &'static str> {
        let (stack_bottom, stack_top) = install_user_stack(
            address_space,
            DEFAULT_USER_STACK_TOP,
            stack_pages,
            USER_STACK_PREFAULT_PAGES.min(stack_pages),
        )?;
        self.user_stack_top = stack_top;
        self.user_stack_bottom = stack_bottom;
        Ok(stack_top)
    }

    /// 分配内核栈
    ///
    /// 为进程分配内核栈，用于 trap 处理时保存上下文。
    /// 内核栈在内核地址空间中分配，使用物理页帧。
    ///
    /// # 返回
    /// - `Ok(())`: 分配成功
    /// - `Err(&'static str)`: 内存不足
    ///
    /// # 内存布局
    /// ```text
    /// +------------------+ <- kernel_stack.top() (高地址)
    /// |   TrapContext    | <- trap_context_ptr
    /// +------------------+
    /// |                  |
    /// |   栈空间         |
    /// |   (向下增长)     |
    /// |                  |
    /// +------------------+ <- kernel_stack.bottom (低地址)
    /// ```
    pub fn allocate_kernel_stack(&mut self) -> Result<(), &'static str> {
        // 分配内核栈（默认 16 页 = 64KB）
        let kernel_stack = KernelStack::new(DEFAULT_KERNEL_STACK_PAGES)
            .ok_or("Failed to allocate kernel stack: out of memory")?;

        // 计算 TrapContext 的位置（在栈顶）
        self.trap_context_ptr = kernel_stack.trap_context_position();


        // 诊断：用全局集合记录 live ppn，alloc 时检测碰撞
        #[cfg(target_arch = "loongarch64")]
        {
            track_live_kernel_stack(self.pid, kernel_stack.ppn());

            // 写 canary 到栈底（最低 8 字节），用于检测栈溢出
            unsafe {
                let canary_ptr = kernel_stack.bottom as *mut u64;
                core::ptr::write_volatile(canary_ptr, KSTK_CANARY);
            }
        }

        // 保存内核栈
        self.kernel_stack = Some(kernel_stack);

        Ok(())
    }

    /// 检查 kernel_stack 底部 canary 是否被覆盖
    #[cfg(target_arch = "loongarch64")]
    pub fn check_kernel_stack_canary(&self) -> bool {
        if let Some(ref ks) = self.kernel_stack {
            unsafe {
                let canary_ptr = ks.bottom as *const u64;
                let actual = core::ptr::read_volatile(canary_ptr);
                actual == KSTK_CANARY
            }
        } else {
            true
        }
    }

    /// 获取内核栈顶地址
    ///
    /// 用于设置 TrapContext 中的 kernel_sp
    pub fn kernel_stack_top(&self) -> usize {
        self.kernel_stack
            .as_ref()
            .map(|ks| ks.top())
            .unwrap_or(0)
    }

    /// [DEBUG] 给定地址是否落在本进程内核栈范围内。
    pub fn kernel_stack_contains(&self, addr: usize) -> bool {
        match self.kernel_stack {
            Some(ref ks) => addr >= ks.bottom && addr < ks.bottom + ks.size,
            None => false,
        }
    }

    /// 获取 TrapContext 的可变引用
    ///
    /// # Safety
    /// 调用者必须确保 trap_context_ptr 指向有效的 TrapContext
    pub unsafe fn trap_context_mut(&mut self) -> Option<&mut crate::arch::TrapContext> {
        if self.trap_context_ptr != 0 {
            Some(&mut *(self.trap_context_ptr as *mut crate::arch::TrapContext))
        } else {
            None
        }
    }

    /// 获取 TrapContext 的不可变引用
    ///
    /// # Safety
    /// 调用者必须确保 trap_context_ptr 指向有效的 TrapContext
    pub unsafe fn trap_context(&self) -> Option<&crate::arch::TrapContext> {
        if self.trap_context_ptr != 0 {
            Some(&*(self.trap_context_ptr as *const crate::arch::TrapContext))
        } else {
            None
        }
    }

    /// 初始化 TrapContext 用于首次进入用户态
    ///
    /// 设置 TrapContext 的各个字段，使得执行 sret 后能正确跳转到用户程序入口点。
    ///
    /// # 设置内容
    /// - `sepc` = entry_point（用户程序入口点）
    /// - `sstatus.SPP = 0`（返回用户态）
    /// - `sstatus.SPIE = 1`（sret 后启用中断）
    /// - `sp` = user_stack_top（用户栈顶）
    /// - `kernel_satp` = 内核页表的 satp 值
    /// - `kernel_sp` = 内核栈顶
    /// - `trap_handler` = trap 处理函数地址
    ///
    /// # 前置条件
    /// - 必须先调用 `allocate_kernel_stack()` 分配内核栈
    /// - 必须设置 `entry_point` 和 `user_stack_top`
    ///
    /// # 返回
    /// - `Ok(())`: 初始化成功
    /// - `Err(&'static str)`: 内核栈未分配或其他错误
    ///
    /// # Requirements
    /// - 5.1: sepc = entry_point
    /// - 5.2: sstatus.SPP = 0 (User mode)
    /// - 5.3: sstatus.SPIE = 1 (Enable interrupts after sret)
    /// - 5.4: SATP = user's page table root
    pub fn init_trap_context(&mut self) -> Result<(), &'static str> {
        use crate::arch::TrapContext;
        use crate::arch::trap_handler_rust;

        // 检查内核栈是否已分配
        if self.kernel_stack.is_none() {
            return Err("Kernel stack not allocated");
        }

        // 检查 trap_context_ptr 是否有效
        if self.trap_context_ptr == 0 {
            return Err("TrapContext pointer not set");
        }

        // 获取内核 satp（当前运行在内核态，读取当前 satp）
        // 在 RustMicroOS 中，内核使用恒等映射，satp 在启动时设置
        let kernel_satp: usize;
        kernel_satp = crate::arch::asm_helpers::read_satp();

        // 获取内核栈顶
        let kernel_sp = self.kernel_stack_top();

        // 创建初始 TrapContext
        // app_init_context 会设置：
        // - sepc = entry（用户程序入口点）
        // - sstatus.SPP = 0（返回用户态）
        // - sstatus.SPIE = 1（sret 后启用中断）
        // - sp = user_stack_top（用户栈顶）
        // - kernel_satp, kernel_sp, trap_handler
        let trap_cx = TrapContext::app_init_context(
            self.entry_point.0,           // sepc = entry point
            self.user_stack_top.0,        // sp = user stack top
            kernel_satp,                  // kernel page table
            kernel_sp,                    // kernel stack top
            trap_handler_rust as usize,   // trap handler function
        );

        // 将 TrapContext 写入内核栈顶
        // 使用 write_volatile 逐字段写入，避免 compiler_builtins memcpy
        // 在 LoongArch64 large code model 下 memcpy 可能有寻址 bug
        unsafe {
            let dst = self.trap_context_ptr as *mut u8;
            let src = &trap_cx as *const TrapContext as *const u8;
            let size = core::mem::size_of::<TrapContext>();
            // 逐 8 字节写入（TrapContext 是 8 字节对齐的）
            let dst_u64 = dst as *mut u64;
            let src_u64 = src as *const u64;
            let count = size / 8;
            for i in 0..count {
                core::ptr::write_volatile(dst_u64.add(i), core::ptr::read(src_u64.add(i)));
            }
        }

        Ok(())
    }

    /// 设置 clone/vfork/信号让位的「下次 trap 返回让出给子进程」标记，并维护
    /// 全局 `YIELD_HANDOFF_PENDING` 计数（仅 None→Some 转变时 +1），使
    /// `handoff_child_for_trap` 能在无 handoff 的绝大多数 syscall 上 lock-free 短路。
    pub fn set_yield_to_child(&mut self, child: Pid) {
        if self.yield_to_child.is_none() {
            crate::sched::process_sched::YIELD_HANDOFF_PENDING
                .fetch_add(1, Ordering::Relaxed);
        }
        self.yield_to_child = Some(child);
    }

    /// 取走 `yield_to_child`，并在确为 Some 时递减 `YIELD_HANDOFF_PENDING`。
    pub fn take_yield_to_child(&mut self) -> Option<Pid> {
        let v = self.yield_to_child.take();
        if v.is_some() {
            crate::sched::process_sched::YIELD_HANDOFF_PENDING
                .fetch_sub(1, Ordering::Relaxed);
        }
        v
    }

    /// 获取地址空间的 SATP 值
    ///
    /// 用于切换页表。
    /// - RISC-V64: SATP 寄存器值（Sv39 模式，`(8<<60) | root_ppn`）
    /// - LoongArch64: PGDL 物理字节地址
    pub fn satp(&self) -> usize {
        if let Some(ref mm) = self.mm {
            mm.lock().satp()
        } else if self.page_table_root.0 != 0 {
            // 线程：从 leader 继承的 page_table_root 构造 satp
            #[cfg(target_arch = "riscv64")]
            { (8usize << 60) | (self.page_table_root.0 / 4096) }
            // LA: page_table_root 已经是字节物理地址，直接用
            #[cfg(target_arch = "loongarch64")]
            { self.page_table_root.0 }
        } else {
            0
        }
    }

    /// 获取地址空间的互斥访问权（通过 MmStruct 内部 SpinLock）。
    ///
    /// 返回 SpinLockGuard，通过 Deref/DerefMut 访问 AddressSpace。
    /// 无 mm 时返回 None。
    pub fn address_space_lock(&self) -> Option<crate::sync::SpinLockGuard<'_, AddressSpace>> {
        self.mm.as_ref().map(|m| m.lock())
    }

    /// 检查是否有地址空间。
    #[inline]
    pub fn has_mm(&self) -> bool {
        self.mm.is_some()
    }

    /// 释放进程资源
    ///
    /// 释放地址空间和所有相关资源。
    ///
    /// # 释放的资源
    /// - 地址空间（包括所有物理页，由 FrameTracker 自动回收）
    /// - 内核栈（由 KernelStack 的 Drop 自动回收）
    /// - ASID（地址空间标识符）
    /// - 文件描述符表
    /// - 子进程列表（注意：子进程本身不会被释放，只是清空列表）
    ///
    /// # 注意
    /// 调用此方法后，进程将无法再执行任何操作。
    /// 通常在进程退出时调用。
    pub fn release_resources(&mut self) -> Vec<alloc::sync::Arc<crate::fs::FileDescriptor>> {
        // 注：原此处对 software `AsidAllocator` 做 `find(self.pid)` 反查再 free，
        // 但 `AsidAllocator::alloc/get_or_alloc` 在生产路径从未被调用（仅单测使用），
        // 故 `asid_map` 恒为全 0，`find` 必然线性扫满 MAX_ASID+1=65536 个槽后返回
        // None——纯死代码且每次进程/线程退出白扫 65536 次（icount profiler 实证：
        // 占线程 create+join 退出指令的 ~98.7%）。RV 不用 ASID，LA 走硬件 CSR ASID
        // （arch/loongarch64/csr.rs），均不依赖此 software allocator。直接移除该
        // 无效扫描，零行为改变（本就没释放任何东西）。

        // 注意：不在这里释放 address_space 和 kernel_stack！
        // exit() 在 child 的内核栈上运行，且 satp 仍指向 child 的页表，
        // 提前释放会导致 use-after-free。它们会在 processes.remove() 时
        // 随 PCB Drop 自动释放。

        // 3. 关闭所有文件描述符（线程共享 leader 的 fd_table，不清理）。
        //
        // 关键：file_lock OFD 锁释放、inotify IN_CLOSE_* 投递、pipe/socket
        // peer wakeup 都必须在 PROCESS_TABLE 锁释放之后执行（避免嵌套死锁）。
        // 我们把 fd Arc 抽出来返回给调用方，由调用方在锁外调用
        // `crate::fs::fd::finalize_closed_fd_for_pid(arc, self.pid)`。
        let closed_fds: Vec<alloc::sync::Arc<crate::fs::FileDescriptor>> =
            if self.fd_table_owner == self.pid {
            self.fd_table
                .iter_mut()
                .filter_map(|slot| slot.take())
                .collect()
        } else {
            Vec::new()
        };
        self.cloexec_bits = 0;
        self.cloexec_fds.clear();
        self.fd_next_hint = 0;

        // Release ETXTBSY write-deny on the executable path.
        if !self.exec_deny_path.is_empty() {
            crate::fs::allow_write_access(&self.exec_deny_path);
            self.exec_deny_path.clear();
        }

        // 4. 清空子进程列表
        self.children.clear();

        // 5. 重置其他状态
        self.clear_wait_state();
        self.wait_exit_code_ptr = 0;
        self.wait_rusage_ptr = 0;
        self.wait_siginfo_ptr = 0;
        self.wait_rusage_ptr_waitid = 0;
        self.wait_pgid = None;
        self.wait_thread_mode = None;
        self.wait_is_waitid = false;
        self.wait_wuntraced = false;
        self.wait_wcontinued = false;
        self.wait_wnowait = false;
        self.wait_trap_context = None;
        self.io_wait_trap_context.clear();
        self.stopped_signal = None;
        self.stopped_waitable = false;
        self.continued_waitable = false;

        closed_fds
    }

    /// Drop this PCB's private fd-table entries without POSIX per-process close
    /// side effects.
    ///
    /// Used only while constructing/changing fd-table ownership: PCB::new()
    /// creates a default stdio table, but fork/clone immediately replaces or
    /// redirects it.  Those constructor entries were never visible to userspace
    /// for this task, so they must decrement the installed-fd count but must not
    /// release POSIX locks for the task.
    pub fn discard_fd_table_refs_without_posix(&mut self) {
        for fd_obj in self.fd_table.iter_mut().filter_map(|slot| slot.take()) {
            fd_obj.dec_fd_ref_and_notify_if_last();
        }
        self.cloexec_bits = 0;
        self.cloexec_fds.clear();
        self.fd_next_hint = 0;
    }

    /// 关闭指定的文件描述符
    ///
    /// # 参数
    /// - `fd`: 文件描述符编号
    ///
    /// # 返回
    /// - `Ok(())`: 关闭成功
    /// - `Err(&'static str)`: 文件描述符无效
    pub fn close_fd(&mut self, fd: usize) -> Result<(), &'static str> {
        if fd >= self.fd_table.len() {
            return Err("Invalid file descriptor");
        }

        if self.fd_table[fd].is_none() {
            return Err("File descriptor not open");
        }

        if let Some(fd_obj) = self.fd_table[fd].take() {
            fd_obj.dec_fd_ref_and_notify_if_last();
        }
        if fd < self.fd_next_hint {
            self.fd_next_hint = fd;
        }
        Ok(())
    }

    /// 获取打开的文件描述符数量
    pub fn open_fd_count(&self) -> usize {
        self.fd_table.iter().filter(|fd| fd.is_some()).count()
    }

    /// 检查进程是否已释放资源（Zombie 状态下文件等已关闭）
    ///
    /// `release_resources` 不再 truncate `fd_table`（保留 vec 容量以便 panic 路径
    /// 的诊断输出）；它只把每个 `Some(Arc<FileDescriptor>)` 抽出到返回值。这里
    /// 因此检查所有 slot 都已成为 `None`，而非 vec 整体长度。
    pub fn is_resources_released(&self) -> bool {
        self.fd_table.iter().all(|slot| slot.is_none())
            && self.children.is_empty()
    }
}

/// 在地址空间中安装用户栈 VMA，并预先映射栈顶附近若干页。
pub fn install_user_stack(
    address_space: &mut AddressSpace,
    stack_top: usize,
    reserved_pages: usize,
    prefault_pages: usize,
) -> Result<(VirtAddr, VirtAddr), &'static str> {
    let reserved_pages = reserved_pages.max(1);
    let prefault_pages = prefault_pages.max(1).min(reserved_pages);
    let stack_top_va = VirtAddr(stack_top);
    let stack_bottom_va = VirtAddr(stack_top - reserved_pages * PAGE_SIZE);

    let permission = VmaPermission {
        read: true,
        write: true,
        execute: false,
        user: true,
    };

    let mut vma = Vma::new(stack_bottom_va, stack_top_va, VmaType::Stack, permission);
    let prefault_bottom = stack_top - prefault_pages * PAGE_SIZE;
    for vpn_num in VirtAddr(prefault_bottom).floor().0..stack_top_va.ceil().0 {
        vma.map_page(VirtPageNum(vpn_num), address_space.page_table_mut());
    }
    address_space.add_vma(vma);

    Ok((stack_bottom_va, stack_top_va))
}

/// Linux `update_curr` 的按需投影：返回任务当前**有效** CFS 状态
/// `(vruntime, deadline)`。
///
/// 周期 tick 不再为 CFS 抢全局表锁；运行中任务的 vruntime 由
/// `sched_last_in_ns` 与当前纳秒调度钟按 delta_exec 现算。该函数只在
/// 已持 `PROCESS_TABLE` 的消费点使用（wakeup preemption / fork 继承），
/// 因此 PCB 字段仍受表锁保护，没有数据竞争。
#[inline]
pub fn effective_cfs_state(p: &ProcessControlBlock, now_ns: u64) -> (u64, u64) {
    if p.sched_policy != 0 || p.on_cpu.is_none() || p.sched_last_in_ns == 0 {
        return (p.vruntime, p.deadline);
    }
    let delta = now_ns.saturating_sub(p.sched_last_in_ns);
    if delta == 0 {
        return (p.vruntime, p.deadline);
    }
    let vruntime = p
        .vruntime
        .saturating_add(crate::sched::policy::calc_delta_fair(delta, p.sched_weight));
    let deadline =
        crate::sched::policy::entity_deadline(vruntime, p.slice_ns, p.sched_weight);
    (vruntime, deadline)
}

/// 全局进程表锁：`IrqMcsSpinLock<Option<ProcessTable>>` 的包装，额外承担
/// **per-CPU `current` 投影**。
///
/// 对标 Linux：`current` 是 per-CPU 量，每个 CPU 在内核态看到的都是自己正在
/// 运行的任务。本内核所有 `current` 的读写都在这把大锁的临界区内，且内核态
/// 不可抢占（trap 入口清 SIE / LA 的 CRMD.IE，syscall 期间不重开中断），因此
/// 取锁时从 lock-free `HART_CURRENT_PID`（架构切栈后发布的唯一真相）投影到
/// `ProcessTable::current`，释放锁前同步诊断镜像 `current_by_hart`。这样临界区
/// 与无锁热路径读取同一个 committed current，不再存在两套可分叉真相。
///
/// 没有这层投影时，`current` 是全系统单槽：AP 一旦真正执行用户任务，两个 hart
/// 会互相覆盖对方的 current，syscall 会用别的进程的地址空间翻译用户指针。
///
/// 锁实现为 MCS 队列 + 关本地中断（Linux qspinlock 的等待结构）：每个 hart
/// 在自己的 per-CPU cacheline 上自旋，释放者只写下一节点的 locked，是所有
/// 排队自旋锁中 cacheline 争用最小的形态；中断关闭保证同 hart 不重入。
pub struct ProcTableLock {
    inner: IrqMcsSpinLock<Option<ProcessTable>>,
}

/// 持锁时长诊断（仅 `HUNGDBG=1` 时记录，正常运行只读一个原子布尔）。
///
/// 多核退化的时间去向已由争用计数收敛到"有人长期持 `PROCESS_TABLE`"：
/// `lock_contended=4906` 而 `lock_spins=50828894`，平均每次争用自旋上万轮。
/// 这里记录**最长一次持锁**的时长与持锁者返回地址，`symbolize.sh` 一翻即知
/// 是哪段临界区，避免继续猜。
static PT_HOLD_START: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static PT_HOLD_RA: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static PT_MAX_HOLD: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static PT_MAX_HOLD_RA: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// 读取**最近一次**持锁者的返回地址（0 = 未记录）。
///
/// 停摆时 `try_lock` 失败即说明有人卡在临界区里，这个地址一经 addr2line 解析
/// 就直接点名是哪段代码，不必再从现象反推。
pub fn proc_table_last_holder_ra() -> usize {
    PT_HOLD_RA.load(core::sync::atomic::Ordering::Relaxed)
}

/// 是否记录持锁诊断：`HUNGDBG` 与 `HANGDUMP` 任一开启即记录。
#[inline(always)]
fn pt_hold_diag_enabled() -> bool {
    crate::cmdline::hungdbg_enabled() || crate::cmdline::hangdump_enabled()
}

/// 读取 (最长持锁 tick 数, 该次持锁者返回地址)。
pub fn proc_table_max_hold() -> (u64, usize) {
    (
        PT_MAX_HOLD.load(core::sync::atomic::Ordering::Relaxed),
        PT_MAX_HOLD_RA.load(core::sync::atomic::Ordering::Relaxed),
    )
}

pub struct ProcTableGuard<'a> {
    guard: Option<crate::sync::IrqMcsSpinLockGuard<'a, Option<ProcessTable>>>,
    hart: usize,
}

unsafe impl Sync for ProcTableLock {}
unsafe impl Send for ProcTableLock {}

impl ProcTableLock {
    pub const fn new(value: Option<ProcessTable>) -> Self {
        Self { inner: IrqMcsSpinLock::new(value) }
    }

    /// 取进程表锁，并在持有期间关本地中断。
    ///
    /// 这把锁会被 trap 路径本身获取（`preempt_current_for_trap`、
    /// `check_and_deliver_signals`、`finish_staged_user_return`）。若持锁期间
    /// 吃到时钟中断，handler 会去抢这把**非递归**锁 → 该 hart 永久自锁，其余
    /// hart 随后全部堆在同一把锁上，整机停摆。GDB 全核取证实测：8 核中 6 个
    /// 停在 `spinlock.rs:81`，调用点分别是上述三条 trap 路径，另一核正在
    /// `sys_write` 中被 trap 打断。间歇性只是因为中断要恰好落在持锁窗口内。
    #[inline]
    pub fn lock(&self) -> ProcTableGuard<'_> {
        ProcTableGuard::attach(self.inner.lock())
    }

    #[inline]
    pub fn try_lock(&self) -> Option<ProcTableGuard<'_>> {
        self.inner.try_lock().map(ProcTableGuard::attach)
    }

    #[inline]
    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }
}

impl<'a> ProcTableGuard<'a> {
    #[inline]
    fn attach(mut guard: crate::sync::IrqMcsSpinLockGuard<'a, Option<ProcessTable>>) -> Self {
        let hart = current_hart_slot();
        if let Some(table) = guard.as_mut() {
            let published =
                crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Acquire);
            table.current = if published == 0 { None } else { Some(published) };
        }
        if pt_hold_diag_enabled() {
            let ra: usize;
            #[cfg(target_arch = "riscv64")]
            unsafe {
                core::arch::asm!("mv {}, ra", out(reg) ra)
            };
            #[cfg(target_arch = "loongarch64")]
            unsafe {
                core::arch::asm!("move {}, $ra", out(reg) ra)
            };
            PT_HOLD_RA.store(ra, core::sync::atomic::Ordering::Relaxed);
            PT_HOLD_START.store(
                crate::arch::asm_helpers::rdtime(),
                core::sync::atomic::Ordering::Relaxed,
            );
        }
        Self { guard: Some(guard), hart }
    }
}

impl<'a> core::ops::Deref for ProcTableGuard<'a> {
    type Target = Option<ProcessTable>;
    #[inline]
    fn deref(&self) -> &Self::Target {
        self.guard.as_ref().expect("process table guard used after release")
    }
}

impl<'a> core::ops::DerefMut for ProcTableGuard<'a> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.guard.as_mut().expect("process table guard used after release")
    }
}

impl<'a> Drop for ProcTableGuard<'a> {
    #[inline]
    fn drop(&mut self) {
        let hart = self.hart;
        if let Some(table) = self.guard.as_mut().and_then(|g| g.as_mut()) {
            table.current_by_hart[hart] = table.current;
        }
        if pt_hold_diag_enabled() {
            let start = PT_HOLD_START.load(core::sync::atomic::Ordering::Relaxed);
            if start != 0 {
                let held = crate::arch::asm_helpers::rdtime().saturating_sub(start);
                if held > PT_MAX_HOLD.load(core::sync::atomic::Ordering::Relaxed) {
                    PT_MAX_HOLD.store(held, core::sync::atomic::Ordering::Relaxed);
                    PT_MAX_HOLD_RA.store(
                        PT_HOLD_RA.load(core::sync::atomic::Ordering::Relaxed),
                        core::sync::atomic::Ordering::Relaxed,
                    );
                }
            }
        }
        // IrqMcsSpinLockGuard 的 Drop 保证先解锁、后恢复中断。
        drop(self.guard.take());
    }
}

/// 本 hart 在 per-hart 数组中的下标；与 `process_sched::hart_slot()` 同源
/// （`read_cpu_id()`），越界回落 0。
#[inline(always)]
pub(crate) fn current_hart_slot() -> usize {
    let id = crate::arch::asm_helpers::read_cpu_id();
    if id < crate::sched::smp::MAX_CPUS {
        id
    } else {
        0
    }
}

/// 全局进程表
pub static PROCESS_TABLE: ProcTableLock = ProcTableLock::new(None);

/// 返回内核栈包含 `addr` 的任务 pid（BSCRASH 栈归属取证）。trap 上下文调用，
/// 用 try_lock 避免与持锁路径互等；拿不到锁时返回 None（宁可漏报不死锁）。
pub fn kstack_owner_of(addr: usize) -> Option<Pid> {
    let guard = PROCESS_TABLE.try_lock()?;
    guard.as_ref()?.kstack_owner_of(addr)
}

/// 目录改名后让所有进程的 cwd / fs_root 跟随（见 ProcessTable::
/// rewrite_paths_after_dir_rename）。供 sys_renameat2 在目录 rename 成功后调用。
pub fn rewrite_cwd_after_dir_rename(old_path: &str, new_path: &str) {
    let mut table = PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        t.rewrite_paths_after_dir_rename(old_path, new_path);
    }
}

/// 下一个可用 PID
pub(super) static NEXT_PID: AtomicUsize = AtomicUsize::new(1);

/// Slice 2.5 真实化：返回当前 `NEXT_PID` 之前已分配的最大 PID（Linux
/// `/proc/loadavg` 第 5 字段 `last_pid`）。多 CPU 下不保证严格"最新"，
/// 但 Linux 内核也只用单 atomic 读，行为对齐。
pub fn current_last_pid() -> usize {
    NEXT_PID.load(core::sync::atomic::Ordering::Relaxed).saturating_sub(1)
}

/// 写 /proc/sys/kernel/ns_last_pid：把"最近分配 PID"置为 `val`，使下一次
/// `alloc_pid` 从 `val + 1` 起搜索（Linux `pid_ns->last_pid = val`）。配合
/// `alloc_pid` 的占用跳过逻辑，可令下一次 fork 复用已回收的 PID。
pub fn set_last_pid(val: usize) {
    NEXT_PID.store(val.saturating_add(1), core::sync::atomic::Ordering::SeqCst);
}

/// PCB 盞收池（对标 Linux task_struct SLAB 缓存）：`reap_or_defer` 物理移除
/// PCB 时先 `drop_in_place` 释放内部资源（内核栈/fd 表/mm 引用照常走 Drop），
/// 但 ~5KB 堆块本身不退还分配器而进池；`new_boxed_for_fork` 命中时直接
/// 在旧块上重构造，免每次 fork/clone 的大块堆分配/释放往返。
/// 池满（上限 32 ≈ 160KB）时照旧完整释放。
const PCB_POOL_CAP: usize = 32;
static PCB_POOL: crate::sync::SpinLock<PcbPool> =
    crate::sync::SpinLock::new(PcbPool { slots: [core::ptr::null_mut(); PCB_POOL_CAP], len: 0 });

struct PcbPool {
    slots: [*mut ProcessControlBlock; PCB_POOL_CAP],
    len: usize,
}

// 裸指针仅作为已释放内容的堆块句柄保存，访问全程持 PCB_POOL 锁。
unsafe impl Send for PcbPool {}

fn pcb_pool_pop() -> Option<*mut ProcessControlBlock> {
    let mut pool = PCB_POOL.lock();
    if pool.len > 0 {
        pool.len -= 1;
        let len = pool.len;
        Some(pool.slots[len])
    } else {
        None
    }
}

fn pcb_pool_recycle(pcb: alloc::boxed::Box<ProcessControlBlock>) {
    let mut pool = PCB_POOL.lock();
    if pool.len < PCB_POOL_CAP {
        let ptr = alloc::boxed::Box::into_raw(pcb);
        unsafe { core::ptr::drop_in_place(ptr) };
        let len = pool.len;
        pool.slots[len] = ptr;
        pool.len += 1;
    }
    // 池满：pcb 离开作用域正常 Drop + 堆块退还。
}

/// PCB 直接映射查找缓存槽数（必须是 2 的幂）。
const LOOKUP_CACHE_SLOTS: usize = 64;

/// HUNGDBG 快照中的一行任务状态。
pub struct HangTaskRow {
    pub pid: Pid,
    pub tgid: Pid,
    pub comm: alloc::string::String,
    pub state: ProcessState,
    pub on_cpu: Option<usize>,
    pub wait_channel: Option<&'static str>,
    pub waiting_for: Option<usize>,
    pub woken: bool,
    pub wakeup_channel: usize,
    pub on_ready_queue: bool,
    pub pending_signals: u64,
    pub syscall_nr: i64,
    pub args: [u64; 3],
}

/// HUNGDBG 快照中的一行 per-hart 所有权。
pub struct HangHartRow {
    pub hart: usize,
    pub current_by_hart: Option<Pid>,
    pub shadow_pid: usize,
    pub kstack_owner: usize,
    pub staged_previous: usize,
    pub staged: usize,
}

/// 表锁下取得的一致性快照，放锁后再输出。
pub struct HangSnapshot {
    pub current: Option<Pid>,
    pub tasks: alloc::vec::Vec<HangTaskRow>,
    pub harts: alloc::vec::Vec<HangHartRow>,
}

impl HangSnapshot {
    /// 必须在放开 PROCESS_TABLE 之后调用：串口输出比锁临界区长几个数量级。
    pub fn print(&self) {
        crate::println!("[HUNGDBG] ==== process table (current={:?}) ====", self.current);
        for t in self.tasks.iter() {
            crate::println!(
                "[HUNGDBG] pid={} tgid={} comm={} state={:?} on_cpu={:?} wchan={:?} waiting_for={:?} woken={} wakeup_ch={} on_rq={} sig_pending={:#x} syscall_nr={} a0={:#x} a1={:#x} a2={:#x}",
                t.pid, t.tgid, t.comm, t.state, t.on_cpu, t.wait_channel, t.waiting_for,
                t.woken, t.wakeup_channel, t.on_ready_queue, t.pending_signals,
                t.syscall_nr, t.args[0], t.args[1], t.args[2]
            );
        }
        for h in self.harts.iter() {
            crate::println!(
                "[HUNGDBG] hart={} current_by_hart={:?} shadow_pid={} kstack_owner={} staged_prev={} staged_next={}",
                h.hart, h.current_by_hart, h.shadow_pid, h.kstack_owner,
                h.staged_previous, h.staged,
            );
        }
    }
}

/// 进程表
pub struct ProcessTable {
    /// 进程映射
    pub(super) processes: BTreeMap<Pid, Box<ProcessControlBlock>>,
    /// 当前锁持有 hart 的 committed current 投影。
    ///
    /// Linux 的 `current` 是 per-CPU 的（RISC-V 内核把 `task_struct` 放在 `tp`）。
    /// 本内核所有对 `current` 的读写都发生在持 `PROCESS_TABLE` 锁的临界区内，
    /// 且内核态执行期间本 hart 中断关闭（trap 入口清 SIE，syscall 全程不开），
    /// 加锁时从 `HART_CURRENT_PID` 投影进该字段；临界区内 `current` 恒等于
    /// 执行本临界区的 hart 已完成物理栈切换并正式提交的当前进程。
    /// 投影由 `ProcTableLock`/`ProcTableGuard` 负责，见其文档。
    pub current: Option<Pid>,
    /// 每 hart current 的锁内诊断镜像；权威值为 `HART_CURRENT_PID`。
    pub(super) current_by_hart: [Option<Pid>; crate::sched::smp::MAX_CPUS],
    /// 已成为僵尸、且不会被任何 `wait4` 回收的 PID（线程，或父进程
    /// 忽略 SIGCHLD 而自动 detach 的子进程）。对标 Linux 对 auto-reap
    /// 任务的 `release_task` 延迟释放：在 `mark_zombie` 时入队，由
    /// `reap_detached_zombies` 在陷入路径上 O(k) 排空，避免每次 trap
    /// 全表扫描。
    pub(super) detached_zombies: Vec<Pid>,
    /// 直接映射 PCB 查找缓存（槽位 = pid % LOOKUP_CACHE_SLOTS，每槽
    /// `(pid, Box 内 PCB 稳定地址)`）。`get`/`get_mut` 高频命中少数几个
    /// PID（当前进程 + 切换对端），Box 的堆地址在 insert/remove 之间
    /// 稳定，命中即免去 BTreeMap 搜索；insert/remove 时全部失效。整个
    /// 结构受 PROCESS_TABLE 锁保护，Cell 仅用于让只读 `get` 也能填缓存。
    lookup_cache: [core::cell::Cell<(Pid, *mut ProcessControlBlock)>; LOOKUP_CACHE_SLOTS],
    /// 历史字段：旁路离队计数。per-CPU 队列改为物理 unlink，此值仅作诊断。
    pub(crate) ready_stale: usize,
    /// 持有 startup_credits 的新生线程候选队列（clone 时入队）。
    /// next_startup_after 从这里惰性取候选，避免每次调度全表 O(n)
    /// 扫描（libc-bench 2500 线程时 next_startup_after 占 387M 指令）。
    startup_queue: alloc::collections::VecDeque<Pid>,
    /// 定时睡眠到期树（对标 Linux hrtimer 红黑树）：键=(deadline, pid)，
    /// 值=是否 alarm/itimer 项。mark_sleeping / futex 直写点 / setitimer
    /// 登记；check_sleep_deadlines 只弹到期前缀（O(log n + 到期数)），
    /// 取代每 tick O(#processes) 全表扫。条目惰性失效：弹出时按进程
    /// 实时状态校验，陈旧项（已被其他事件唤醒/重入睡）直接丢弃。
    pub(super) sleep_timers: BTreeMap<(u64, Pid), bool>,
}

// SAFETY: ProcessTable 只在 PROCESS_TABLE 自旋锁内被访问，Cell/裸指针
// 不会跨线程无同步共享。
unsafe impl Send for ProcessTable {}

impl ProcessTable {
    pub fn new() -> Self {
        Self {
            processes: BTreeMap::new(),
            current: None,
            current_by_hart: [None; MAX_CPUS],
            detached_zombies: Vec::new(),
            lookup_cache: core::array::from_fn(|_| {
                core::cell::Cell::new((INVALID_PID, core::ptr::null_mut()))
            }),
            ready_stale: 0,
            startup_queue: alloc::collections::VecDeque::new(),
            sleep_timers: BTreeMap::new(),
        }
    }

    /// 获取进程数量
    pub fn process_count(&self) -> usize {
        self.processes.len()
    }

    /// Slice 2.2: 只读迭代所有进程 PCB —— procfs `/proc/[pid]/status` 在统计
    /// `Threads:` 时需要数同 tgid 的 PCB；其它 procfs 聚合视图（loadavg、
    /// /proc/stat ctxt 求和等）也需要此接口。返回值借用 PROCESS_TABLE 锁，
    /// 调用方需在锁定生命周期内使用。
    pub fn iter(&self) -> impl Iterator<Item = &ProcessControlBlock> {
        self.processes.values().map(|b| b.as_ref())
    }

    /// 分配新 PID。遵循 Linux `alloc_pidmap` 语义：从 `NEXT_PID` 起跳过仍登记
    /// 在进程表中的 PID（运行中或未回收的僵尸），把命中的空闲 PID 作为最后分配值
    /// 写回。常规情况下 `NEXT_PID` 单调递增、候选恒为空闲，行为与旧实现一致；当
    /// 用户写入 /proc/sys/kernel/ns_last_pid（见 `set_last_pid`）把 `NEXT_PID`
    /// 回拨到此前已回收的 PID 时，下一次 fork 即可复用该 PID —— LTP
    /// pidfd_send_signal03 依赖此行为构造 PID 复用场景。
    pub fn alloc_pid(&self) -> Pid {
        const PID_MAX: usize = 4_194_304; // Linux PID_MAX_DEFAULT
        const PID_WRAP_BASE: usize = 300; // Linux RESERVED_PIDS
        loop {
            let candidate = NEXT_PID.fetch_add(1, Ordering::SeqCst);
            if candidate >= PID_MAX {
                NEXT_PID.store(PID_WRAP_BASE, Ordering::SeqCst);
                continue;
            }
            if candidate == 0 {
                continue;
            }
            if !self.processes.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    /// 获取进程
    pub fn get(&self, pid: Pid) -> Option<&ProcessControlBlock> {
        let slot = &self.lookup_cache[pid & (LOOKUP_CACHE_SLOTS - 1)];
        let (cpid, ptr) = slot.get();
        if cpid == pid && !ptr.is_null() {
            // SAFETY: 缓存仅在 insert/remove 之间有效（彼时会失效），Box 的
            // 堆地址稳定；&self 借用保证表未被并发修改。
            return Some(unsafe { &*ptr });
        }
        self.processes.get(&pid).map(|p| {
            let r = p.as_ref();
            slot.set((pid, r as *const ProcessControlBlock as *mut _));
            r
        })
    }

    fn invalidate_lookup_cache(&self) {
        for slot in self.lookup_cache.iter() {
            slot.set((INVALID_PID, core::ptr::null_mut()));
        }
    }

    /// 获取可变进程
    pub fn get_mut(&mut self, pid: Pid) -> Option<&mut ProcessControlBlock> {
        let slot = &self.lookup_cache[pid & (LOOKUP_CACHE_SLOTS - 1)];
        let (cpid, ptr) = slot.get();
        if cpid == pid && !ptr.is_null() {
            // SAFETY: 同 `get`；&mut self 保证独占。
            return Some(unsafe { &mut *ptr });
        }
        self.processes.get_mut(&pid).map(|p| {
            let r = p.as_mut();
            slot.set((pid, r as *mut _));
            r
        })
    }

    /// 解析某进程的有效地址空间所有者 PID。
    ///
    /// 新架构：每个有地址空间的进程/线程都持有自己的 `mm: Some(Arc<MmStruct>)`
    /// （CLONE_VM 共享同一 Arc），无需 `shared_vm_parent` 链追溯。
    /// 自身有 mm → 自身；否则回退到 tgid leader。
    fn address_space_owner_of(pid: Pid, p: &ProcessControlBlock) -> Pid {
        if p.mm.is_some() {
            pid
        } else {
            p.tgid
        }
    }

    /// 诊断占位（旧 UAF 探针已由 MetaSlot 替代）。
    pub fn effective_address_space_dbg(&self, _pid: Pid) -> Option<crate::sync::SpinLockGuard<'_, AddressSpace>> {
        None
    }

    /// 诊断占位（旧 UAF 探针已由 MetaSlot 替代）。
    pub fn ownership_dbg(&self, pid: Pid) -> Option<(Pid, Option<Pid>, Pid)> {
        let p = self.processes.get(&pid)?;
        Some((p.tgid, None, Self::address_space_owner_of(pid, p)))
    }

    /// [DEBUG 取证] 扫描所有进程的内核栈区间，判断物理/内核虚拟地址 `pa`
    /// 是否落在某条仍存活的内核栈内。若是 → 该帧同时被内核栈与（调用方传入的）
    /// 堆缓存/用户页占用 = 物理双重分配铁证。
    pub fn kstk_owner_scan_dbg(&self, pa: usize, _crash_pid: Pid) {
        for (pid, p) in self.processes.iter() {
            if let Some(ref ks) = p.kernel_stack {
                if pa >= ks.bottom && pa < ks.bottom + ks.size {
                    crate::println!(
                        "[KSTK-OWN] pa={:#x} falls INSIDE live kstack of pid={} tgid={} \
                         state={:?} is_thread={} ks=[{:#x},{:#x}) — PHYSICAL DOUBLE-USE",
                        pa, pid, p.tgid, p.state, p.is_thread,
                        ks.bottom, ks.bottom + ks.size,
                    );
                }
            }
        }
    }

    /// 诊断占位（旧 UAF 探针已由 MetaSlot 替代）。
    pub fn ksp_on_user_frame_dbg(&self, _sp: usize, _sepc: usize) {}

    /// 诊断占位（旧 UAF 探针已由 MetaSlot 替代）。
    pub fn reap_dangle_check_dbg(&self, _pid: Pid) {}

    /// 物理移除一个 zombie 的 PCB（连带 `Drop` 释放其内核栈），但若当前
    /// hart 正运行在该 zombie 的内核栈上，则**推迟**移除：此刻释放就是
    /// use-after-free——后续 `usertrapret`/`__restore` 仍在这张栈上执行，
    /// `-smp 2` 下另一 hart 抢走该页清零即损坏。
    ///
    /// 对标 Linux：一个任务的内核栈永远由 `finish_task_switch` 中的后继任务
    /// 释放，绝不在自身 inline 释放。这里把"自释放"情形交给延迟回收队列
    /// `detached_zombies`，它在我们 `sret` 切走后、于父任务自己的栈上排空
    /// （`reap_detached_zombies_locked` 会跳过 `pid == current`）。
    pub fn reap_or_defer(&mut self, pid: Pid) {
        #[cfg(target_arch = "loongarch64")]
        crate::arch::loongarch64::fpu::on_exit(pid);
        let sp: usize;
        #[cfg(target_arch = "riscv64")]
        unsafe { core::arch::asm!("mv {}, sp", out(reg) sp, options(nomem, nostack)); }
        #[cfg(target_arch = "loongarch64")]
        unsafe { core::arch::asm!("move {}, $sp", out(reg) sp, options(nomem, nostack)); }
        #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
        { sp = 0; }
        let on_own_stack = self
            .processes
            .get(&pid)
            .map_or(false, |p| p.kernel_stack_contains(sp) || p.on_cpu.is_some())
            // SMP：只看**本 hart** 的 sp 不够——另一个 hart 可能正站在这张栈上
            // （典型：该进程刚在 hart B 上 exit，hart B 尚未切到自己的 idle 栈）。
            // 漏掉这一项就会 freed-while-mapped，见 `HART_KSTACK_OWNER` 的取证。
            || crate::sched::process_sched::kstack_in_use_by_any_hart(pid);
        if on_own_stack {
            if !self.detached_zombies.contains(&pid) {
                self.detached_zombies.push(pid);
                DETACHED_ZOMBIES_PENDING.store(true, core::sync::atomic::Ordering::Release);
            }
        } else {
            self.invalidate_lookup_cache();
            // 递减未消费的 yield_to_child 计数：进程带着未被 handoff 消费的
            // 标记退出（如 clone 后子进程先退光、父进程直接 exit）时，若不在
            // 此处结清，YIELD_HANDOFF_PENDING 将永久 >0，此后每次 syscall 的
            // handoff_child_for_trap 短路失效，都要空付一把全局锁 + 查表。
            if let Some(p) = self.processes.get_mut(&pid) {
                p.take_yield_to_child();
                // 延迟到此刻的 mmput（见 mark_zombie）：此刻
                // reap_or_defer 已确认没有 hart 再使用该内核栈，可以安全
                // 释放用户帧和页表结构。
                if p.mm.is_some() {
                    self.mmput_for_exit(pid);
                }
            }
            if let Some(p) = self.processes.get_mut(&pid) {
                account_removed_pcb(p);
            }
            if let Some(pcb) = self.processes.remove(&pid) {
                pcb_pool_recycle(pcb);
            }
        }
    }

    /// 返回内核栈区间包含 `addr` 的任务 pid（BSCRASH 栈归属取证用）。
    pub fn kstack_owner_of(&self, addr: usize) -> Option<Pid> {
        for (pid, p) in self.processes.iter() {
            if p.kernel_stack_contains(addr) {
                return Some(*pid);
            }
        }
        None
    }

    /// [DEBUG] 物理帧 `ppn` 是否（按页）与某个仍持有内核栈的进程重叠。
    /// 返回 (pid, state, is_thread, tgid)。用于取证：把仍是活进程内核栈的帧
    /// 交给用户态 = freed-while-live-kstack 双重分配铁证。
    pub fn dbg_frame_in_live_kstack(&self, ppn: usize) -> Option<(Pid, ProcessState, bool, Pid)> {
        #[cfg(target_arch = "riscv64")]
        let page_va = ppn << 12;
        #[cfg(not(target_arch = "riscv64"))]
        let page_va = crate::mm::phys_to_kernel_vaddr(ppn << 12);
        let page_end = page_va + crate::arch::consts::PAGE_SIZE;
        for (pid, p) in self.processes.iter() {
            if let Some(ref ks) = p.kernel_stack {
                if page_va < ks.bottom + ks.size && ks.bottom < page_end {
                    return Some((*pid, p.state, p.is_thread, p.tgid));
                }
            }
        }
        None
    }

    /// 对标 Linux `mmput()`：退出时递减 mm_users，归零则释放用户页。
    ///
    /// 替代旧的 `release_address_space_if_last_user` O(N) 遍历：
    /// 现在 mm_users 是原子引用计数，mmput() = O(1) 原子递减。
    fn mmput_for_exit(&mut self, exiting_pid: Pid) {
        // `take()` 而非 `clone()`：递减与「本 PCB 不再是使用者」必须是同一个动作。
        // 只递减不摘除时，任何再次走到退出收尾的路径都会把同一份 mm 再减一次。
        // 退出后续代码不再需要地址空间（用户页由 mmput 归零时统一释放）。
        let mm = match self.processes.get_mut(&exiting_pid) {
            Some(p) => match p.mm.take() {
                Some(mm) => mm,
                None => return,
            },
            None => return,
        };
        // mmput: mm_users-- → 到 0 时 release_user_frames + mmdrop
        mm.mmput();
    }

    /// 获取拥有 fd_table 的进程（线程重定向到 tgid leader，实现 CLONE_FILES 共享语义）
    pub fn get_fd_process(&self, pid: Pid) -> Option<&ProcessControlBlock> {
        let p = self.get(pid)?;
        // 普通进程(非 CLONE_FILES) fd_table_owner == 自身，省去第二次 BTreeMap
        // 搜索——此函数在每次 read/write/fd 操作热路径上被调用。
        if p.fd_table_owner == pid {
            return Some(p);
        }
        self.get(p.fd_table_owner)
    }

    /// 获取拥有 fd_table 的可变进程
    pub fn get_fd_process_mut(&mut self, pid: Pid) -> Option<&mut ProcessControlBlock> {
        let fd_pid = self.get(pid)?.fd_table_owner;
        self.get_mut(fd_pid)
    }

    /// Materialize a private descriptor table for `pid` when it currently
    /// redirects fd operations to another task's table (Linux
    /// `close_range(CLOSE_RANGE_UNSHARE)` / `unshare(CLONE_FILES)` semantics).
    pub fn unshare_fd_table_for_pid(&mut self, pid: Pid) -> bool {
        let owner = match self.get(pid) {
            Some(p) => p.fd_table_owner,
            None => return false,
        };
        if owner == pid {
            return true;
        }
        let (fd_table, fd_next_hint, cloexec_bits, cloexec_fds) = match self.get(owner) {
            Some(owner_proc) => (
                owner_proc.fd_table.clone(),
                owner_proc.fd_next_hint,
                owner_proc.cloexec_bits,
                owner_proc.cloexec_fds.clone(),
            ),
            None => return false,
        };
        for fd_obj in fd_table.iter().flatten() {
            fd_obj.inc_fd_ref();
        }
        let proc = match self.get_mut(pid) {
            Some(proc) => proc,
            None => {
                for fd_obj in fd_table.iter().flatten() {
                    fd_obj.dec_fd_ref_and_notify_if_last();
                }
                return false;
            }
        };
        proc.discard_fd_table_refs_without_posix();
        proc.fd_table = fd_table;
        proc.fd_next_hint = fd_next_hint.min(proc.fd_table.len());
        proc.cloexec_bits = cloexec_bits;
        proc.cloexec_fds = cloexec_fds;
        proc.fd_table_owner = pid;
        true
    }

    /// If `exiting_pid` owns a descriptor table that is still shared by live
    /// tasks, move the table object to one of those sharers before releasing
    /// resources.
    ///
    /// Linux `CLONE_FILES` shares a file table object whose lifetime is
    /// independent of any particular task.  Our compact model stores the actual
    /// table in one owner PCB and redirects sharers via `fd_table_owner`; when
    /// that owner exits first, ownership must be transferred instead of closing
    /// all descriptors prematurely.
    pub fn transfer_fd_table_owner_on_exit(&mut self, exiting_pid: Pid) {
        let owns_table = self
            .get(exiting_pid)
            .map_or(false, |p| p.fd_table_owner == exiting_pid);
        if !owns_table {
            return;
        }
        let new_owner = self.processes.iter().find_map(|(&pid, proc)| {
            let proc = proc.as_ref();
            if pid != exiting_pid
                && proc.fd_table_owner == exiting_pid
                && !matches!(proc.state, ProcessState::Zombie | ProcessState::Unused)
            {
                Some(pid)
            } else {
                None
            }
        });
        let Some(new_owner) = new_owner else {
            return;
        };

        let (fd_table, fd_next_hint, cloexec_bits, cloexec_fds) = {
            let old = match self.get_mut(exiting_pid) {
                Some(old) => old,
                None => return,
            };
            let fd_table = core::mem::take(&mut old.fd_table);
            let fd_next_hint = old.fd_next_hint;
            let cloexec_bits = old.cloexec_bits;
            let cloexec_fds = core::mem::take(&mut old.cloexec_fds);
            old.cloexec_bits = 0;
            old.fd_next_hint = 0;
            old.fd_table_owner = new_owner;
            (fd_table, fd_next_hint, cloexec_bits, cloexec_fds)
        };

        if let Some(owner_proc) = self.get_mut(new_owner) {
            // A CLONE_FILES/thread sharer should not own private entries here,
            // but drop any constructor leftovers defensively without POSIX
            // close side effects.
            owner_proc.discard_fd_table_refs_without_posix();
            owner_proc.fd_table = fd_table;
            owner_proc.fd_next_hint = fd_next_hint.min(owner_proc.fd_table.len());
            owner_proc.cloexec_bits = cloexec_bits;
            owner_proc.cloexec_fds = cloexec_fds;
            owner_proc.fd_table_owner = new_owner;
        } else {
            return;
        }

        for (&pid, proc) in self.processes.iter_mut() {
            if pid != exiting_pid && proc.fd_table_owner == exiting_pid {
                proc.fd_table_owner = new_owner;
            }
        }
    }

    /// 插入进程到进程表
    pub fn insert(&mut self, pid: Pid, mut pcb: Box<ProcessControlBlock>) {
        let enqueue = pcb.state == ProcessState::Runnable;
        pcb.on_cpu = None;
        pcb.on_ready_queue = false;
        account_inserted_pcb(&pcb);
        self.invalidate_lookup_cache();
        self.processes.insert(pid, pcb);
        if enqueue {
            self.enqueue_ready(pid);
        }
    }

    /// 返回所有 PID 列表
    pub fn pids(&self) -> alloc::vec::Vec<Pid> {
        self.processes.keys().copied().collect()
    }

    /// 目录被重命名后，修正所有进程的 cwd / fs_root 字符串，使其跟随被改名
    /// 的目录。Linux 把 cwd 维护为 dentry 引用，重命名一个恰是某进程 cwd
    /// （或其祖先）的目录会透明地改变该进程的绝对路径；本内核把 cwd 存为
    /// 字符串，故在此按前缀改写以等价模拟（inotify02：rename(cwd) 后用相对
    /// 路径 unlink 仍须命中原目录）。
    pub fn rewrite_paths_after_dir_rename(&mut self, old_path: &str, new_path: &str) {
        fn rewrite_prefix(s: &mut String, old: &str, new: &str) {
            if s.as_str() == old {
                *s = String::from(new);
            } else if let Some(rest) = s.strip_prefix(old) {
                if rest.starts_with('/') {
                    let mut combined = String::from(new);
                    combined.push_str(rest);
                    *s = combined;
                }
            }
        }
        // 共享 fs_struct（CLONE_FS 线程组）只能改写一次，否则 new 也含 old
        // 前缀时会二次替换；按 Arc 指针去重。
        let mut seen: Vec<*const SpinLock<FsStruct>> = Vec::new();
        for (_pid, proc) in self.processes.iter_mut() {
            let ptr = Arc::as_ptr(&proc.fs);
            if seen.contains(&ptr) {
                continue;
            }
            seen.push(ptr);
            let mut fs = proc.fs.lock();
            rewrite_prefix(&mut fs.cwd, old_path, new_path);
            rewrite_prefix(&mut fs.root, old_path, new_path);
        }
    }

    /// 获取当前进程
    pub fn current(&self) -> Option<&ProcessControlBlock> {
        self.current.and_then(|pid| self.get(pid))
    }

    /// 获取当前可变进程
    pub fn current_mut(&mut self) -> Option<&mut ProcessControlBlock> {
        self.current.and_then(move |pid| self.processes.get_mut(&pid).map(|p| p.as_mut()))
    }

    /// 设置本 hart 的当前进程。任务的 `on_cpu`、per-hart current 与无锁影子
    /// 状态在同一 `PROCESS_TABLE` 临界区内一次性更新。
    pub fn set_current(&mut self, pid: Option<Pid>) {
        let hart = current_hart_slot();
        let previous = self.current;
        let mut requeue_previous = false;

        if previous != pid {
            if let Some(previous_pid) = previous {
                if let Some(previous_task) = self.get_mut(previous_pid) {
                    if previous_task.on_cpu == Some(hart) {
                        previous_task.on_cpu = None;
                    }
                    requeue_previous = previous_task.state == ProcessState::Runnable
                        && !previous_task.on_ready_queue;
                }
            }
        }

        let next_policy = if let Some(next_pid) = pid {
            let next = self
                .get_mut(next_pid)
                .expect("set_current target does not exist");
            assert!(
                next.on_cpu.is_none() || next.on_cpu == Some(hart),
                "set_current attempted to steal pid {} from hart {:?}",
                next_pid,
                next.on_cpu,
            );
            next.on_cpu = Some(hart);
            next.sched_policy.clamp(0, 255) as u8
        } else {
            0
        };

        self.current = pid;
        self.current_by_hart[hart] = pid;
        let pcb_ptr = pid
            .and_then(|p| self.processes.get_mut(&p))
            .map_or(core::ptr::null_mut(), |b| b.as_mut() as *mut ProcessControlBlock);
        crate::sched::process_sched::HART_CURRENT_PCB
            .store(pcb_ptr, core::sync::atomic::Ordering::Relaxed);
        crate::sched::process_sched::HART_CURRENT_PID
            .store(pid.unwrap_or(0), core::sync::atomic::Ordering::Release);
        crate::sched::process_sched::HART_CURRENT_POLICY[hart]
            .store(next_policy, core::sync::atomic::Ordering::Release);
        if pid.is_none() {
            self.release_staged_switch();
        }

        if requeue_previous {
            if let Some(previous_pid) = previous {
                self.enqueue_ready(previous_pid);
            }
        }
    }

    /// 归还本 hart 尚未提交的调度预约。
    ///
    /// 本 hart 已确定不会再抵达 return-to-user 的提交点（进入 idle、当前任务
    /// 死亡等）。`switch_to` 的预约已把 next 置 `Running + on_cpu=本 hart` 并摘
    /// 出就绪环；若只丢弃槽位，它就成了「没有任何 hart 在执行、也不在就绪环」
    /// 的孤儿，任何唤醒都救不回来，最终全机空转。
    fn release_staged_switch(&mut self) {
        let hart = current_hart_slot();
        let next_pid = crate::sched::process_sched::take_staged_user_return();
        if next_pid == INVALID_PID || next_pid == 0 {
            return;
        }
        if let Some(next) = self.get_mut(next_pid) {
            if next.on_cpu == Some(hart) {
                next.on_cpu = None;
            }
            if next.state == ProcessState::Running {
                set_process_state(next, ProcessState::Runnable);
            }
        }
        self.enqueue_ready(next_pid);
    }

    /// 在架构汇编已切到 next 内核栈后完成逻辑调度提交。
    ///
    /// `switch_to` 只预约 next，并让 previous 保持 on_cpu；本函数才解除 previous
    /// 的执行保留、按需重新入队，并把 per-hart 表镜像提交为 next。
    pub(crate) fn finish_staged_switch(&mut self, previous_pid: Pid, next_pid: Pid) {
        let hart = current_hart_slot();
        let mut requeue_previous = false;

        if previous_pid != INVALID_PID && previous_pid != next_pid {
            if let Some(previous) = self.get_mut(previous_pid) {
                if previous.on_cpu == Some(hart) {
                    previous.on_cpu = None;
                }
                requeue_previous = previous.state == ProcessState::Runnable
                    && !previous.on_ready_queue;
            }
        }

        let (mnt_ns_id, next_policy) = {
            let next = self
                .get_mut(next_pid)
                .expect("staged scheduler target disappeared");
            assert_eq!(
                next.on_cpu,
                Some(hart),
                "staged scheduler target lost hart reservation",
            );
            set_process_state(next, ProcessState::Running);
            if next.cached_mnt_ns_id == 0 {
                next.cached_mnt_ns_id = crate::fs::mount::INIT_MNT_NS;
            }
            (next.cached_mnt_ns_id, next.sched_policy.clamp(0, 255) as u8)
        };

        self.current = Some(next_pid);
        self.current_by_hart[hart] = Some(next_pid);
        crate::sched::process_sched::HART_CURRENT_POLICY[hart]
            .store(next_policy, core::sync::atomic::Ordering::Release);
        if requeue_previous {
            self.enqueue_ready(previous_pid);
        }
        crate::fs::mount::set_active_mnt_ns(mnt_ns_id);
    }

    /// 从所有 per-CPU 队列物理摘除 `pid`（对标 Linux `dequeue_task`）。
    pub(crate) fn unlink_ready(&mut self, pid: Pid) {
        if pid == INVALID_PID {
            return;
        }
        unlink_wakelist(pid);
        let cpu = self.get(pid).and_then(|p| p.rq_cpu);
        if let Some(cpu) = cpu {
            if cpu < MAX_CPUS {
                let mut rq = lock_rq(cpu);
                rq.remove_pid(pid);
                publish_rq(cpu, &rq);
            }
        } else {
            for h in 0..MAX_CPUS {
                let mut rq = lock_rq(h);
                rq.remove_pid(pid);
                publish_rq(h, &rq);
            }
        }
        let was_queued = self.get(pid).map(|p| p.on_ready_queue).unwrap_or(false);
        if let Some(process) = self.get_mut(pid) {
            process.on_ready_queue = false;
            process.rq_cpu = None;
        }
        if was_queued {
            self.ready_stale = self.ready_stale.saturating_add(1);
        }
    }

    fn allowed_mask(&self, pid: Pid) -> u64 {
        let online = crate::sched::smp::online_mask_fast();
        let want = self.get(pid).map(|p| p.cpus_allowed).unwrap_or(u64::MAX);
        let mask = want & online;
        if mask == 0 {
            online
        } else {
            mask
        }
    }

    fn select_rq_cpu(&self, pid: Pid, flags: u32) -> usize {
        let mask = self.allowed_mask(pid);
        let this = current_hart_slot();
        let prev = self.get(pid).and_then(|p| p.wake_cpu.or(p.rq_cpu));
        select_task_rq(mask, this, prev, flags)
    }

    fn drain_wakelist(&mut self, cpu: usize) {
        if cpu >= MAX_CPUS {
            return;
        }
        let pids = take_wakelist(cpu);
        for pid in pids {
            let still = self.get(pid).map_or(false, |p| {
                p.state == ProcessState::Runnable && p.on_cpu.is_none()
            });
            if !still {
                continue;
            }
            self.activate_on_cpu(pid, cpu);
        }
        let rq = lock_rq(cpu);
        publish_rq(cpu, &rq);
    }

    /// 已经选定目标 CPU：place_entity 后挂进该 CPU 的 CFS/RT 树。
    fn activate_on_cpu(&mut self, pid: Pid, cpu: usize) {
        if cpu >= MAX_CPUS || pid == INVALID_PID {
            return;
        }
        // class: 0=SCHED_OTHER/CFS, 1=SCHED_FIFO/RR, 2=SCHED_DEADLINE。
        let (class, vrt, weight, rt_prio, dl_deadline) = match self.get(pid) {
            Some(p) if p.state == ProcessState::Runnable && p.on_cpu.is_none() => {
                let eff = if p.pi_boost_priority > p.sched_priority {
                    p.pi_boost_priority
                } else {
                    p.sched_priority
                };
                (
                    match p.sched_policy {
                        6 => 2,
                        1 | 2 => 1,
                        _ => 0,
                    },
                    p.vruntime,
                    p.sched_weight,
                    eff.clamp(1, 99) as u8,
                    p.dl_deadline,
                )
            }
            _ => return,
        };
        let mut rq = lock_rq(cpu);
        rq.update_pelt(now_ns());
        let avg = rq.cfs.avg_vruntime();
        let load = rq.cfs.sum_weight;
        let (placed, deadline, vlag) = place_entity(avg, vrt, weight, load);
        match class {
            2 => rq.enqueue_dl(pid, dl_deadline),
            1 => rq.enqueue_rt(pid, rt_prio),
            _ => rq.enqueue_cfs(pid, placed, deadline, weight),
        }
        publish_rq(cpu, &rq);
        drop(rq);
        if let Some(p) = self.get_mut(pid) {
            p.on_ready_queue = true;
            p.rq_cpu = Some(cpu);
            if class == 0 {
                p.vruntime = placed;
                p.deadline = deadline;
                p.vlag = vlag;
                p.slice_ns = crate::sched::policy::sched_slice_ns(weight, load.max(1));
            }
        }
    }

    fn task_runnable_on(&self, pid: Pid, cpu: usize) -> bool {
        if pid == 0 || pid == INVALID_PID || cpu >= MAX_CPUS {
            return false;
        }
        let Some(p) = self.get(pid) else {
            return false;
        };
        if p.state != ProcessState::Runnable || p.on_cpu.is_some() {
            return false;
        }
        if (p.cpus_allowed & (1u64 << cpu)) == 0 {
            return false;
        }
        if crate::sched::process_sched::kstack_in_use_by_any_hart(pid) {
            return false;
        }
        crate::kernel::cgroup::cgroup_cpu_can_run(&p.cgroup_path)
            && !crate::kernel::cgroup::cgroup_is_frozen(&p.cgroup_path)
    }

    fn task_runnable_relaxed(&self, pid: Pid, cpu: usize) -> bool {
        if pid == 0 || pid == INVALID_PID || cpu >= MAX_CPUS {
            return false;
        }
        let Some(p) = self.get(pid) else {
            return false;
        };
        p.state == ProcessState::Runnable
            && p.on_cpu.is_none()
            && (p.cpus_allowed & (1u64 << cpu)) != 0
            && !crate::sched::process_sched::kstack_in_use_by_any_hart(pid)
    }

    /// `switch_to` 失败后把仍 Runnable 且无人认领的任务救回队列。
    pub(crate) fn requeue_orphaned(&mut self, pid: Pid) {
        if pid == 0 || pid == INVALID_PID {
            return;
        }
        let ok = self.get(pid).map_or(false, |p| {
            p.state == ProcessState::Runnable && p.on_cpu.is_none() && !p.on_ready_queue
        });
        if ok {
            self.enqueue_ready(pid);
        }
    }

    /// Re-enqueue a runnable process at the tail of the ready queue.
    /// Used by SCHED_RR time slice expiration and RT bandwidth throttling.
    pub(super) fn requeue_at_tail(&mut self, pid: Pid) {
        if pid == INVALID_PID {
            return;
        }
        self.unlink_ready(pid);
        if let Some(p) = self.get(pid) {
            if p.on_cpu.is_some() || p.state != ProcessState::Runnable {
                return;
            }
        } else {
            return;
        }
        self.enqueue_ready(pid);
    }

    pub(crate) fn enqueue_ready(&mut self, pid: Pid) {
        self.enqueue_ready_wf(pid, WF_TTWU);
    }

    pub(crate) fn enqueue_ready_wf(&mut self, pid: Pid, flags: u32) {
        if pid == INVALID_PID {
            return;
        }
        let should_enqueue = {
            let process = match self.get_mut(pid) {
                Some(process) => process,
                None => return,
            };
            if process.state != ProcessState::Runnable
                || process.on_cpu.is_some()
                || process.on_ready_queue
            {
                false
            } else {
                process.on_ready_queue = true;
                true
            }
        };
        if !should_enqueue {
            return;
        }
        let sched_class = self.get(pid).map(|p| p.sched_policy);
        let is_rt = sched_class.map_or(false, |p| matches!(p, 1 | 2 | 6));
        if sched_class == Some(6) {
            note_deadline_active();
        }
        if is_rt {
            RT_DL_PRESENT.store(true, core::sync::atomic::Ordering::Relaxed);
        }
        let cpu = self.select_rq_cpu(pid, flags);
        if let Some(process) = self.get_mut(pid) {
            process.rq_cpu = Some(cpu);
        }
        let this = current_hart_slot();
        // 跨核：ttwu_queue_wakelist，目标 CPU 在 select_next 里 drain 入树。
        // WF_SYNC（pipe/jobserver）必须本地入树，否则唤醒者马上睡会把对端冻在远端。
        if cpu != this && (flags & WF_SYNC) == 0 {
            push_wakelist(cpu, pid);
            crate::sched::process_sched::kick_cpu(cpu);
            crate::sched::process_sched::kick_idle_hart_if_any();
            return;
        }
        self.activate_on_cpu(pid, cpu);
        crate::sched::process_sched::kick_cpu(cpu);
        crate::sched::process_sched::kick_idle_hart_if_any();
    }

    /// 把已在就绪队列的任务登记进 RT/DL 候选集（sched_setscheduler 将就绪任务
    /// 提升为 RT/DEADLINE 时调用，否则 select_next 快选会漏掉它）。
    pub fn note_rt_ready(&mut self, pid: Pid) {
        let (cpu, class, prio, dl) = match self.get(pid) {
            Some(p) => (
                p.rq_cpu.unwrap_or_else(current_hart_slot),
                match p.sched_policy {
                    6 => 2,
                    1 | 2 => 1,
                    _ => 0,
                },
                if p.pi_boost_priority > p.sched_priority {
                    p.pi_boost_priority
                } else {
                    p.sched_priority
                },
                p.dl_deadline,
            ),
            None => return,
        };
        if class == 2 {
            note_deadline_active();
        }
        if cpu < MAX_CPUS {
            let mut rq = lock_rq(cpu);
            match class {
                2 => rq.enqueue_dl(pid, dl),
                1 => rq.enqueue_rt(pid, prio.clamp(1, 99) as u8),
                _ => {}
            }
            publish_rq(cpu, &rq);
        }
        RT_DL_PRESENT.store(true, core::sync::atomic::Ordering::Relaxed);
    }

    fn remove_ready(&mut self, pid: Pid) {
        self.unlink_ready(pid);
    }

    fn dequeue_ready_matching<F>(
        &mut self,
        cpu: usize,
        dest: usize,
        current_pid: Pid,
        mut accepts: F,
    ) -> Option<Pid>
    where
        F: FnMut(&ProcessControlBlock) -> bool,
    {
        if cpu >= MAX_CPUS {
            return None;
        }
        // 先 SCHED_DEADLINE（EDF 序），再 SCHED_FIFO/RR（优先级 bitmap 序）。
        let rt_pids: Vec<Pid> = {
            let rq = lock_rq(cpu);
            let mut v = rq.dl.pids_edf();
            v.extend(rq.rt.pids_by_priority());
            v
        };
        for pid in rt_pids {
            if pid == current_pid || !self.task_runnable_on(pid, dest) {
                continue;
            }
            if self.get(pid).map_or(false, |p| accepts(p)) {
                self.unlink_ready(pid);
                if let Some(p) = self.get_mut(pid) {
                    p.rq_cpu = Some(dest);
                }
                return Some(pid);
            }
        }
        let cfs_pids: Vec<Pid> = {
            let rq = lock_rq(cpu);
            rq.cfs.pids()
        };
        for pid in cfs_pids {
            if pid == current_pid || !self.task_runnable_on(pid, dest) {
                continue;
            }
            if self.get(pid).map_or(false, |p| accepts(p)) {
                self.unlink_ready(pid);
                if let Some(p) = self.get_mut(pid) {
                    p.rq_cpu = Some(dest);
                }
                return Some(pid);
            }
        }
        None
    }

    pub fn insert_runnable(&mut self, pid: Pid, mut pcb: Box<ProcessControlBlock>) {
        set_process_state(&mut pcb, ProcessState::Runnable);
        pcb.on_cpu = None;
        pcb.on_ready_queue = false;
        account_inserted_pcb(&pcb);
        self.invalidate_lookup_cache();
        self.processes.insert(pid, pcb);
        self.enqueue_ready(pid);
        // 踢醒覆盖：fork 入队同样立即唤空闲 hart（审查 P2：旧实现只
        // mark_runnable 踢，新任务在空闲核上白等一个 tick）。
        crate::sched::process_sched::kick_idle_hart_if_any();
    }

    pub fn mark_runnable(&mut self, pid: Pid, clear_wait: bool) -> bool {
        self.mark_runnable_wf(pid, clear_wait, WF_TTWU)
    }

    /// Linux `try_to_wake_up(..., wake_flags)`。pipe 读写必须带 `WF_SYNC`
    ///（Linus `1b6b26ae7053`：jobserver 丢同步唤醒会把 `-j` 并行度打没）。
    pub fn mark_runnable_wf(&mut self, pid: Pid, clear_wait: bool, flags: u32) -> bool {
        let should_enqueue = {
            let process = match self.get_mut(pid) {
                Some(process) => process,
                None => return false,
            };
            if matches!(
                process.state,
                ProcessState::Zombie | ProcessState::Unused | ProcessState::Stopped
            ) {
                return false;
            }
            set_process_state(process, ProcessState::Runnable);
            if clear_wait {
                // Slice 2.5 wchan 真 hook：runnable → `/proc/[pid]/wchan` 应输出
                // "0"。Linux 内核 `get_wchan()` 在 task 不在内核栈上时返回 0。
                process.clear_wait_state();
            }
            true
        };
        if should_enqueue {
            self.enqueue_ready_wf(pid, flags);
            // 踢醒统一收口（select_idle_sibling 对标）。
            crate::sched::process_sched::kick_idle_hart_if_any();
            // 唤醒抢占（对标 Linux check_preempt_wakeup 的 vruntime 比较版）：
            // 原实现只对 RT/DL 唤醒置 NEED_RESCHED，SCHED_OTHER 被唤醒线程在全
            // hart 忙时最坏等满一个时间片（100-400ms）才被 tick 抢占（审查
            // P1-2）。被唤醒者 vruntime 领先当前运行任务时才置位，交由最近的
            // 返回用户态抢占点重新评估，避免无条件置位引发切换风暴。
            // Linux `check_preempt_wakeup` / `wakeup_preempt_entity`：
            // curr.vruntime - woken.vruntime > wakeup_gran 才抢占，并踢 curr 所在核。
            let woken = self.get(pid).map(|p| (p.vruntime, p.deadline, p.sched_weight));
            let cur_pid = self.current.unwrap_or(INVALID_PID);
            let now_ns = crate::sched::policy::now_ns();
            let cur = self.get(cur_pid).and_then(|p| {
                if p.sched_policy == 0 {
                    let (vrt, dl) = effective_cfs_state(p, now_ns);
                    Some((vrt, p.on_cpu, dl))
                } else {
                    None
                }
            });
            if let (Some((woken_vrt, woken_dl, weight)), Some((cur_vrt, on_cpu, cur_dl))) =
                (woken, cur)
            {
                let gran = wakeup_gran_ns(weight);
                let vdiff = cur_vrt.saturating_sub(woken_vrt);
                let earlier_deadline = woken_dl != 0 && (cur_dl == 0 || woken_dl < cur_dl);
                if vdiff > gran || earlier_deadline {
                    if let Some(hart) = on_cpu {
                        crate::sched::process_sched::set_need_resched_on(hart);
                        crate::sched::process_sched::kick_cpu(hart);
                    } else {
                        crate::sched::process_sched::set_need_resched();
                    }
                }
            }
        }
        should_enqueue
    }

    /// 同步唤醒直切：把唤醒者置 Runnable 但不入就绪环，只占 wake_handoff
    /// 槽（单槽，旧占位者 spill 回就绪环）。仅适用于「唤醒后调用者自己
    /// 马上阻塞」的同步交接点（pipe 直递）；非 SCHED_OTHER 或有未决信号
    /// 时回退常规 mark_runnable（保持 RT 抢占/信号优先语义）。
    pub fn mark_runnable_handoff(&mut self, pid: Pid) -> bool {
        let eligible = {
            let process = match self.get_mut(pid) {
                Some(process) => process,
                None => return false,
            };
            if matches!(
                process.state,
                ProcessState::Zombie | ProcessState::Unused | ProcessState::Stopped
            ) {
                return false;
            }
            if process.sched_policy != 0
                || process.on_cpu.is_some()
                || (process.pending_signals & !process.signal_mask) != 0
                || process.on_ready_queue
            {
                None
            } else {
                set_process_state(process, ProcessState::Runnable);
                process.clear_wait_state();
                Some(())
            }
        };
        if eligible.is_none() {
            return self.mark_runnable(pid, true);
        }
        let cpu = current_hart_slot();
        let old = {
            let mut rq = lock_rq(cpu);
            let old = rq.take_handoff();
            rq.wake_handoff = pid;
            publish_rq(cpu, &rq);
            old
        };
        if old != INVALID_PID && old != pid {
            self.enqueue_ready_wf(old, WF_SYNC);
        }
        true
    }

    pub fn mark_sleeping(&mut self, pid: Pid, waiting_for: Option<Pid>, wakeup_channel: usize) -> bool {
        let marked = {
            let process = match self.get_mut(pid) {
                Some(process) => process,
                None => return false,
            };
            if matches!(process.state, ProcessState::Zombie | ProcessState::Unused) {
                return false;
            }
            // 同一任务已处于 Sleeping 但等待哨兵变化时（重入睡/改等待点），
            // set_process_state 会因 old==new 短路，必须在这里单独维护
            // IO_WAIT 无锁计数，否则 loadavg 的 nr_uninterruptible 会漂移。
            let old_waiting = process.waiting_for;
            if process.state == ProcessState::Sleeping && old_waiting != waiting_for {
                if old_waiting == Some(IO_WAIT_SENTINEL) {
                    IO_WAITER_COUNT.fetch_sub(1, Ordering::Relaxed);
                }
                if waiting_for == Some(IO_WAIT_SENTINEL) {
                    IO_WAITER_COUNT.fetch_add(1, Ordering::Relaxed);
                }
            }
            process.waiting_for = waiting_for;
            process.wakeup_channel = wakeup_channel;
            set_process_state(process, ProcessState::Sleeping);
            // 定时睡眠统一登记最近到期点：使 check_sleep_deadlines 可以用
            // `now < NEAREST_DEADLINE` 无锁短路（所有阻塞路径都经此单点入口）。
            if wakeup_channel > 1 {
                note_sleep_deadline(wakeup_channel as u64);
                // RT/DEADLINE 定时睡眠者登记快查槽，到期走定点唤醒快路径
                // （见 check_sleep_deadlines），绕过全表扫描。
                if matches!(process.sched_policy, 1 | 2 | 6) {
                    note_rt_sleeper(pid, wakeup_channel as u64);
                }
            }
            // Slice 2.5 wchan 真 hook：mark_sleeping 是所有阻塞路径的单点入
            // 口，按 sentinel/PID 推导一个 `&'static str` 标签写入 PCB，让
            // `/proc/[pid]/wchan` 能输出 Linux 风格的内核等待点名称（busybox
            // top/ps 用）。Linux 中 wchan 是 RIP→符号查找；我们没有符号表，
            // 用语义化短名替代。Caller 若已经显式 set 过更精确的标签
            // （如 poll/futex/pipe_wait 细分），就**不**覆盖。
            if process.wait_channel.is_none() {
                process.wait_channel = Some(wait_channel_for(waiting_for));
            }
            true
        };
        if marked {
            if wakeup_channel > 1 {
                self.register_sleep_timer(pid, wakeup_channel as u64, false);
            }
            self.remove_ready(pid);
        }
        marked
    }

    /// 向到期树登记一个定时睡眠/alarm 到期点（惰性失效，重复登记无害）。
    pub fn register_sleep_timer(&mut self, pid: Pid, deadline: u64, is_alarm: bool) {
        self.sleep_timers.insert((deadline, pid), is_alarm);
    }

    pub fn mark_zombie(&mut self, pid: Pid, reason: ExitReason) -> bool {
        let cgroup_path_for_uncharge;
        let is_detached_thread;
        let marked = {
            let process = match self.get_mut(pid) {
                Some(process) => process,
                None => return false,
            };
            // 幂等：已经收尾过的任务不能再走一遍。下面的收尾动作全是**计数递减**
            // （mmput 减 mm_users、cgroup pids uncharge、memcg on_exit），重复执行
            // 会让 mm_users 提前归零 → `release_user_frames` 把仍在使用的用户页
            // 还回分配器，之后任意进程都可能读到别人的数据。debug 构建会在
            // `mmput` 的 `debug_assert!(prev > 0)` 当场炸出，release 构建则静默
            // 继续，表现为随机进程的随机数据损坏。
            if process.state == ProcessState::Zombie {
                if process.exit_status.is_none() {
                    process.exit_status = Some(reason);
                }
                return false;
            }
            cgroup_path_for_uncharge = process.cgroup_path.clone();
            is_detached_thread = process.is_thread;
            // 结清未消费的 yield_to_child：zombie 不会再走 trap 返回路径消费
            // 它；若留着，YIELD_HANDOFF_PENDING 恒 >0，全系统每次 syscall 的
            // handoff 短路失效（空付全局锁 + 查表，null syscall +~60 指令）。
            process.take_yield_to_child();
            set_process_state(process, ProcessState::Zombie);
            if process.exit_status.is_none() {
                process.exit_status = Some(reason);
            }
            true
        };
        if marked {
            // cgroup pids.current--: uncharge on process exit
            crate::kernel::cgroup::cgroup_pids_uncharge(&cgroup_path_for_uncharge);
            // cgroup v2: unlink the exited task from its cgroup membership set
            // (cgroup_exit) so it leaves cgroup.procs and no longer blocks rmdir.
            crate::kernel::cgroup::cgroup_remove_pid(pid as usize);
            // cgroup v1 memory：退出进程从所属 memcg 成员集合移除。
            crate::mm::memcg::on_exit(pid);
            self.remove_ready(pid);
            // A thread is never linked into any parent's `children` list, so no
            // `wait4` will ever reap it; like Linux's auto-reaped tasks it must
            // be released lazily off the trap path.  Enqueue it for the
            // deferred reaper instead of leaving an O(N) table scan to
            // rediscover it on every trap.
            if is_detached_thread {
                self.detached_zombies.push(pid);
                DETACHED_ZOMBIES_PENDING.store(true, core::sync::atomic::Ordering::Release);
            }
            // 对标 Linux `do_exit` → `mmput()`：原子递减 mm_users，
            // 归零时释放用户页 + mmdrop。O(1)，替代旧 O(N) 遍历。
            // 但 mmput 必须在**没有 hart 仍运行在该任务的地址空间/内核栈上**
            // 之后才能执行：RV BuildStorm SMP=8 实测，exit 在第二次
            // `select_next` 前就 mmput 释放页表根，当前 hart 还在旧 satp 上
            // 取指/响应 trap，连 __trap_entry 自身都 InstructionPageFault
            // （sepc=stvec=__trap_entry, scause=12），持有 PROCESS_TABLE 的
            // hart 永远回不到 exit，其余 hart 全部堆死在该锁上。
            let still_on_cpu = self.current == Some(pid)
                || crate::sched::process_sched::kstack_in_use_by_any_hart(pid);
            if !still_on_cpu {
                self.mmput_for_exit(pid);
            }
        }
        if self.current == Some(pid) {
            self.set_current(None);
        }
        marked
    }

    pub(super) fn complete_waitpid_status_locked(
        &mut self,
        parent_pid: Pid,
        child_pid: Pid,
        wstatus: i32,
        cpu_user_ticks: u64,
        cpu_kernel_ticks: u64,
    ) {
        #[cfg(target_arch = "riscv64")]
        const REG_A0: usize = 10;
        #[cfg(target_arch = "loongarch64")]
        const REG_A0: usize = 4;
        if let Some(parent) = self.get_mut(parent_pid) {
            restore_wait_trap_context(parent);
            let status_ptr = parent.wait_exit_code_ptr;
            let rusage_ptr = parent.wait_rusage_ptr;
            parent.wait_exit_code_ptr = 0;
            parent.wait_rusage_ptr = 0;
            parent.wait_siginfo_ptr = 0;
            parent.wait_rusage_ptr_waitid = 0;
            parent.wait_pgid = None;
            parent.wait_thread_mode = None;
            parent.wait_is_waitid = false;
            parent.wait_wuntraced = false;
            parent.wait_wcontinued = false;
            parent.wait_wnowait = false;
            unsafe {
                if let Some(cx) = parent.trap_context_mut() {
                    cx.x[REG_A0] = child_pid;
                }
            }
            if is_valid_user_ptr(status_ptr) {
                let wstatus_bytes = wstatus.to_le_bytes();
                if let Some(ref mm) = parent.mm {
                    let mut addr_space = mm.lock();
                    let _ = write_user_bytes(&mut *addr_space, status_ptr, &wstatus_bytes);
                }
            }
            if is_valid_user_ptr(rusage_ptr) {
                let rusage_buf =
                    build_rusage_buf_from_ticks(cpu_user_ticks, cpu_kernel_ticks);
                if let Some(ref mm) = parent.mm {
                    let mut addr_space = mm.lock();
                    let _ = write_user_bytes(&mut *addr_space, rusage_ptr, &rusage_buf);
                }
            }
        }
    }

    pub(super) fn complete_waitid_status_locked(
        &mut self,
        parent_pid: Pid,
        child_pid: Pid,
        event: WaitidEvent,
        uid: u32,
        cpu_user_ticks: u64,
        cpu_kernel_ticks: u64,
    ) -> bool {
        #[cfg(target_arch = "riscv64")]
        const REG_A0: usize = 10;
        #[cfg(target_arch = "loongarch64")]
        const REG_A0: usize = 4;
        let Some(parent) = self.get_mut(parent_pid) else { return false };
        restore_wait_trap_context(parent);
        let infop = parent.wait_siginfo_ptr;
        let ru_ptr = parent.wait_rusage_ptr_waitid;
        let nowait = parent.wait_wnowait;
        parent.wait_siginfo_ptr = 0;
        parent.wait_rusage_ptr_waitid = 0;
        parent.wait_exit_code_ptr = 0;
        parent.wait_rusage_ptr = 0;
        parent.wait_pgid = None;
        parent.wait_thread_mode = None;
        parent.wait_is_waitid = false;
        parent.wait_wuntraced = false;
        parent.wait_wcontinued = false;
        parent.wait_wnowait = false;
        unsafe {
            if let Some(cx) = parent.trap_context_mut() {
                cx.x[REG_A0] = 0;
            }
        }
        if infop != 0 {
            let mut siginfo = [0u8; 128];
            build_waitid_siginfo(
                &mut siginfo,
                child_pid,
                event,
                uid,
                cpu_user_ticks,
                cpu_kernel_ticks,
            );
            if let Some(ref mm) = parent.mm {
                let mut addr_space = mm.lock();
                let _ = write_user_bytes(&mut *addr_space, infop, &siginfo);
            }
        }
        if ru_ptr != 0 {
            let rusage_buf = build_rusage_buf_from_ticks(cpu_user_ticks, cpu_kernel_ticks);
            if let Some(ref mm) = parent.mm {
                let mut addr_space = mm.lock();
                let _ = write_user_bytes(&mut *addr_space, ru_ptr, &rusage_buf);
            }
        }
        !nowait
    }

    fn wake_parent_for_child_status(
        &mut self,
        child_pid: Pid,
        wstatus: i32,
        stopped_event: bool,
        continued_event: bool,
    ) {
        let (parent_pid, is_thread, child_pgid) = match self.get(child_pid) {
            Some(child) => match child.parent_pid {
                Some(parent_pid) => (parent_pid, child.is_thread, child.pgid),
                None => return,
            },
            None => return,
        };
        // A ptrace signal-delivery-stop is always reported to the tracer via
        // wait(), independent of WUNTRACED (Linux do_wait → ptrace_attach).
        let ptrace_stop_for_parent = self
            .get(child_pid)
            .map_or(false, |child| child.ptrace_stopped && child.ptrace_tracer == parent_pid);
        let wake_for_waitpid = if let Some(parent) = self.get_mut(parent_pid) {
            if !is_thread {
                parent.pending_signals |= 1u64 << (17 - 1); crate::sched::process::note_signal_hint();
            }
            let thread_mode_accepts = match parent.wait_thread_mode {
                None | Some(ThreadMode::NonThreadOnly) => !is_thread,
                Some(ThreadMode::CloneOnly) => is_thread,
                Some(ThreadMode::All) => true,
            };
            let target_match = if parent.wait_pgid.is_some() {
                parent.wait_pgid == Some(child_pgid)
            } else {
                parent.waiting_for == Some(child_pid) || parent.waiting_for.is_none()
            };
            let event_match =
                (stopped_event && (parent.wait_wuntraced || ptrace_stop_for_parent))
                    || (continued_event && parent.wait_wcontinued);
            let any_wait_wake = thread_mode_accepts && target_match && event_match;
            let wake_for_signal = !is_thread
                && parent.waiting_for == Some(SIGNAL_WAIT_SENTINEL)
                && sigchld_wakes_signal_wait(parent);
            if parent.state == ProcessState::Sleeping && any_wait_wake {
                Some(true)
            } else if parent.state == ProcessState::Sleeping && wake_for_signal {
                Some(false)
            } else {
                None
            }
        } else {
            None
        };
        if let Some(complete_waitpid) = wake_for_waitpid {
            if complete_waitpid {
                let waitid_mode = self
                    .get(parent_pid)
                    .map_or(false, |parent| parent.wait_is_waitid);
                if waitid_mode {
                    let (event, uid, cpu_u, cpu_k) = match self.get(child_pid) {
                        Some(child) => {
                            let event = if stopped_event {
                                WaitidEvent::Stopped(child.stopped_signal.unwrap_or(19))
                            } else {
                                WaitidEvent::Continued
                            };
                            (event, child.uid, child.cpu_user_ticks, child.cpu_kernel_ticks)
                        }
                        None => (WaitidEvent::Stopped(19), 0, 0, 0),
                    };
                    let consume = self.complete_waitid_status_locked(
                        parent_pid,
                        child_pid,
                        event,
                        uid,
                        cpu_u,
                        cpu_k,
                    );
                    if consume {
                        if let Some(child) = self.get_mut(child_pid) {
                            match event {
                                WaitidEvent::Stopped(_) => child.stopped_waitable = false,
                                WaitidEvent::Continued => child.continued_waitable = false,
                                WaitidEvent::Exited(_) => {}
                            }
                        }
                        clear_sigchld_if_no_waitable_status(self, parent_pid);
                    }
                } else {
                    if let Some(child) = self.get_mut(child_pid) {
                        if stopped_event {
                            child.stopped_waitable = false;
                        }
                        if continued_event {
                            child.continued_waitable = false;
                        }
                    }
                    self.complete_waitpid_status_locked(parent_pid, child_pid, wstatus, 0, 0);
                    clear_sigchld_if_no_waitable_status(self, parent_pid);
                }
            }
            self.mark_runnable(parent_pid, complete_waitpid);
        }
    }

    pub fn stop_process_for_signal(&mut self, pid: Pid, sig: i32) -> bool {
        let stopped = {
            let process = match self.get_mut(pid) {
                Some(process) => process,
                None => return false,
            };
            if matches!(process.state, ProcessState::Zombie | ProcessState::Unused) {
                return false;
            }
            set_process_state(process, ProcessState::Stopped);
            process.stopped_signal = Some(sig);
            process.stopped_waitable = true;
            process.continued_waitable = false;
            true
        };
        if stopped {
            self.unlink_ready(pid);
            self.wake_parent_for_child_status(
                pid,
                wait_status_from_stopped_signal(sig),
                true,
                false,
            );
        }
        stopped
    }

    pub fn continue_process_for_signal(&mut self, pid: Pid) -> bool {
        let was_stopped = self
            .get(pid)
            .map_or(false, |process| process.state == ProcessState::Stopped);
        if !was_stopped {
            return self.get(pid).is_some();
        }
        if let Some(process) = self.get_mut(pid) {
            process.stopped_signal = None;
            process.stopped_waitable = false;
            process.continued_waitable = true;
            set_process_state(process, ProcessState::Runnable);
        }
        self.enqueue_ready(pid);
        crate::sched::process_sched::kick_idle_hart_if_any();
        if self.get(pid).is_some() {
            self.wake_parent_for_child_status(
                pid,
                wait_status_from_continued(),
                false,
                true,
            );
        }
        true
    }

    /// Resume a ptrace-stopped tracee (PTRACE_CONT / PTRACE_KILL). Clears the
    /// signal-delivery-stop, optionally re-injects `inject_sig` (data argument
    /// of PTRACE_CONT, or SIGKILL for PTRACE_KILL) as a fresh pending signal,
    /// transitions the tracee back to Runnable and re-enqueues it so the
    /// scheduler picks it up. Mirrors Linux `ptrace_resume()`.
    pub fn ptrace_resume_tracee(&mut self, pid: Pid, inject_sig: u32) -> bool {
        let resumed = {
            let process = match self.get_mut(pid) {
                Some(process) => process,
                None => return false,
            };
            process.ptrace_stopped = false;
            process.ptrace_stop_reason = 0;
            process.ptrace_pending_sig = 0;
            process.stopped_signal = None;
            process.stopped_waitable = false;
            process.continued_waitable = false;
            if inject_sig != 0 && inject_sig < 64 {
                process.pending_signals |= 1u64 << (inject_sig - 1); crate::sched::process::note_signal_hint();
            }
            if process.state == ProcessState::Stopped {
                set_process_state(process, ProcessState::Runnable);
            }
            !matches!(process.state, ProcessState::Zombie | ProcessState::Unused)
        };
        if resumed {
            self.enqueue_ready(pid);
            crate::sched::process_sched::kick_idle_hart_if_any();
        }
        resumed
    }

    /// 给正在**别的 hart** 上运行的线程挂 SIGKILL，并向该 hart 发一次
    /// reschedule IPI 让它尽快进入内核处理致命信号（对标 Linux
    /// `zap_other_threads` + `signal_wake_up` → `kick_process`）。
    ///
    /// 该线程随后在自己的 hart 上走完正常退出路径，自行释放内核栈与地址空间。
    pub fn request_group_kill(&mut self, pid: Pid, hart: usize) {
        const SIGKILL: u32 = 9;
        if let Some(p) = self.get_mut(pid) {
            p.pending_signals |= 1u64 << (SIGKILL - 1);
        }
        crate::sched::smp::kick_hart(hart);
    }

    /// 该 pid 是否正在**某个 hart** 上运行；返回那个 hart 的编号。
    ///
    /// SMP 必需的判据：一个线程正在别的 hart 上执行时，它的内核栈上有活动帧、
    /// 地址空间正在被使用，任何"替它释放资源"的操作都是 use-after-free。
    /// 对标 Linux：`zap_other_threads`/`de_thread` 只给兄弟线程挂 SIGKILL，
    /// 由它们各自在自己的 CPU 上走完 `do_exit`。
    pub fn running_on_hart(&self, pid: Pid) -> Option<usize> {
        let me = crate::sched::process::current_hart_slot();
        self.get(pid)
            .and_then(|task| task.on_cpu)
            .filter(|&hart| hart != me)
    }

    #[must_use]
    pub fn switch_to(&mut self, next_pid: Pid, reason: u64) -> bool {
        if crate::sched::process_sched::user_return_handoff_pending() {
            crate::sched::process_sched::HART_SWITCH_FAIL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            self.requeue_orphaned(next_pid);
            return false;
        }
        if self.get(next_pid).map_or(true, |p| {
            p.state != ProcessState::Runnable || p.on_cpu.is_some()
        }) {
            crate::sched::process_sched::HART_SWITCH_FAIL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            self.requeue_orphaned(next_pid);
            return false;
        }
        // Linux `finish_task_switch` 语义：任务阻塞后，其原 hart 在跳上 idle
        // 栈之前仍**物理运行在该任务的内核栈**上（suspend 收尾、锁释放、
        // wtrace 打印都在其栈帧里）。此窗口内若另一 hart 把刚被唤醒的它选走
        // 并在同一条栈顶恢复 TrapContext，两个执行流互踩栈帧——实测受害面
        // 从 fmt 参数、FrameTracker drop 到 memcpy 随机分布（BuildStorm
        // rustc/cargo 间歇 SIGSEGV/SIGILL/内核栈错配的统一根因）。
        // on_cpu 在 set_current(None) 时已清，无法表达"栈仍被占"；以
        // HART_KSTACK_OWNER 为物理真值：仍被任何 hart 占用就拒绝本次切换，
        // 调度器选别的任务，原 hart 落到 idle 栈后自然放行。
        if crate::sched::process_sched::kstack_in_use_by_any_hart(next_pid) {
            crate::sched::process_sched::HART_SWITCH_FAIL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            self.requeue_orphaned(next_pid);
            return false;
        }
        // 1) 给即将切出的进程结账：从其上次切入到现在的差值入 user/kernel bucket。
        let now = crate::arch::asm_helpers::rdtime();
        let now_ns = crate::sched::policy::now_ns();
        // 全局切换计数在唯一真实切换点递增。此前只有 yield_now 等遗留路径累加，
        // 计数恒为 0，使 hangdump 的「切换冻结」判据从开机起恒真、诊断全程误触发
        //（整轮串口被周期倒出淹没，还在表锁临界区内放大争用）。
        crate::sched::inc_context_switch_count();
        let previous = self.current.unwrap_or(INVALID_PID);
        let mut prev_cgroup_charge: Option<(alloc::string::String, u64)> = None;
        if previous != INVALID_PID {
            if let Some(prev) = self.get_mut(previous) {
                let last_in = prev.cpu_last_switched_in_tick;
                if last_in != 0 && now > last_in {
                    let delta = now - last_in;
                    // cgroup cpu.max 按真实 delta_exec 在切换点结账，不再每个
                    // 周期 tick 抢表锁快照路径。usec 由 rdtime 换算。
                    let delta_usec = delta
                        .saturating_mul(1_000_000)
                        / (crate::arch::consts::CLOCK_FREQ as u64).max(1);
                    if delta_usec != 0
                        && !crate::kernel::cgroup::cgroup_cpu_quota_active()
                        && !prev.cgroup_path.is_empty()
                    {
                        prev_cgroup_charge = Some((prev.cgroup_path.clone(), delta_usec));
                    }
                    let hart = current_hart_slot();
                    if prev.cpu_in_kernel {
                        prev.cpu_kernel_ticks = prev.cpu_kernel_ticks.saturating_add(delta);
                        crate::sched::process_sched::HART_SYS_TICKS[hart]
                            .fetch_add(delta, core::sync::atomic::Ordering::Relaxed);
                    } else {
                        prev.cpu_user_ticks = prev.cpu_user_ticks.saturating_add(delta);
                        crate::sched::process_sched::HART_USER_TICKS[hart]
                            .fetch_add(delta, core::sync::atomic::Ordering::Relaxed);
                    }
                    // Linux `update_curr`：每次真正切走都按 delta_exec 记账，
                    // 不能只靠 10ms tick（pipe/jobserver 切换远快于 tick）。
                    // delta_exec 使用纳秒调度钟（见 `sched_last_in_ns`）。
                    if prev.sched_policy == 0 && prev.sched_last_in_ns != 0 {
                        let delta_ns = now_ns.saturating_sub(prev.sched_last_in_ns);
                        let dv = crate::sched::policy::calc_delta_fair(
                            delta_ns,
                            prev.sched_weight,
                        );
                        prev.vruntime = prev.vruntime.saturating_add(dv);
                        prev.deadline = crate::sched::policy::entity_deadline(
                            prev.vruntime,
                            prev.slice_ns,
                            prev.sched_weight,
                        );
                    }
                }
                prev.cpu_last_switched_in_tick = 0;
                prev.sched_last_in_ns = 0;
                prev.sched_slice_start_tick = 0;
                // child-runs-first 让位提示在进程「阻塞」切出时结清：否则
                // YIELD_HANDOFF_PENDING 在其阻塞期间恒 >0，全系统每次 syscall 的
                // handoff 短路失效（空付全局锁+查表，null syscall +67 指令实测）。
                // 被抢占但仍 Running/Runnable 的进程保留提示——它很快回到 CPU，
                // 下一次 trap 返回仍会消费（清掉会改变线程风暴下的让位时序，
                // b_malloc_thread_stress icount 实测 −2.5%）。
                if !matches!(
                    prev.state,
                    ProcessState::Running | ProcessState::Runnable
                ) {
                    prev.take_yield_to_child();
                }
                // Slice 2.2: voluntary vs involuntary ctxt_switches。Linux 语义：
                // - timer 抢占（SWITCH_TIMER）= involuntary：进程被 tick 强行赶下台
                // - 任何其它路径（YIELD/NANOSLEEP/futex/wait/sleep/syscall block/exit
                //   reason=0 默认）= voluntary：进程主动让出或在 syscall 中阻塞
                // 与 `/proc/[pid]/status` 的 voluntary_ctxt_switches /
                // nonvoluntary_ctxt_switches 行直接对应。
                if reason == crate::sched::trace::SWITCH_TIMER {
                    prev.involuntary_ctxt_switches =
                        prev.involuntary_ctxt_switches.saturating_add(1);
                } else {
                    prev.voluntary_ctxt_switches =
                        prev.voluntary_ctxt_switches.saturating_add(1);
                }
                // Scheduler invariant: there can be only one Running task, the
                // current CPU owner.  Most blocking/preemption paths mark the
                // old task Sleeping/Runnable before calling switch_to(), but
                // direct switch paths must not leave a non-current task stuck in
                // Running; otherwise wait/pipe code will not see it as runnable
                // and can spin forever on an empty pipe.
                if previous != next_pid && prev.state == ProcessState::Running {
                    set_process_state(prev, ProcessState::Runnable);
                }
            }
        }
        if let Some((path, usec)) = prev_cgroup_charge.take() {
            crate::kernel::cgroup::charge_cpu(&path, usec);
        }
        // 2) 预约新进程到本 hart，但 current/previous 的 on_cpu 保持不变，直到
        // 架构汇编已经切入 next 栈后由 finish_staged_switch 原子提交。
        let hart = current_hart_slot();
        if let Some(next) = self.get_mut(next_pid) {
            set_process_state(next, ProcessState::Running);
            next.on_cpu = Some(hart);
            next.on_ready_queue = false;
            next.rq_cpu = Some(hart);
            next.wake_cpu = Some(hart);
            next.cpu_last_switched_in_tick = now;
            next.sched_last_in_ns = now_ns;
            next.sched_slice_start_tick = now;
            // 精确分账进程在阻塞 syscall 中途切走再切回时，其 syscall 出口
            // 读到的 enter tick 若停留在自己入口时刻，会把整段阻塞期错记成
            // kernel 时间；切换点刷新全局 enter tick（本函数已读 rdtime，零
            // 额外时钟成本），把结算窗口截到本次切入。
            crate::sched::process_sched::HART_KERNEL_ENTER_TICK
                .store(now, core::sync::atomic::Ordering::Relaxed);
        }
        self.unlink_ready(next_pid);
        if let Some(next) = self.get_mut(next_pid) {
            next.rq_cpu = Some(hart);
        }
        crate::sched::process_sched::HART_SWITCH_OK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        // 此刻 Rust 仍站在 previous 栈上；只发布 pending target，不改 committed
        // current/current_by_hart，也不解除 previous.on_cpu。
        let next_pcb_ptr = self
            .processes
            .get_mut(&next_pid)
            .map_or(core::ptr::null_mut(), |p| p.as_mut() as *mut ProcessControlBlock);
        crate::sched::process_sched::stage_user_return(
            next_pid,
            next_pcb_ptr,
            previous,
        );
        // [DIAG] RT 唤醒→上CPU 延迟
        {
            let wake_pid = RT_WAKE_PID.load(core::sync::atomic::Ordering::Relaxed);
            if wake_pid == next_pid {
                RT_WAKE_PID.store(0, core::sync::atomic::Ordering::Relaxed);
                RT_RUN_TS.store(now, core::sync::atomic::Ordering::Relaxed);
                RT_RUN_PID.store(next_pid, core::sync::atomic::Ordering::Relaxed);
                let wake = RT_WAKE_TS.load(core::sync::atomic::Ordering::Relaxed);
                let freq = crate::arch::consts::CLOCK_FREQ as u64;
                let d_us = now.saturating_sub(wake) * 1_000_000 / freq;
                let hops = RT_WAKE_HOPS.swap(0, core::sync::atomic::Ordering::Relaxed);
                if crate::RTDIAG {
                    rtstat_record(13, d_us);
                }
                if d_us > 150 {
                    let prev_nr = self
                        .processes
                        .get(&previous)
                        .map(|p| p.last_syscall.0 as i64)
                        .unwrap_or(-1);
                    crate::rtdiag_println!(
                        "[rtdiag] run-late pid={} sched_us={} hops={} prev={} prev_nr={}",
                        next_pid, d_us, hops, previous, prev_nr
                    );
                }
            } else if wake_pid != 0 {
                // RT 已唤醒但本次切到了别的任务：计一跳（定性 select_next 绕过 vs 无调度点）
                RT_WAKE_HOPS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            }
        }
        SWITCH_SEQ.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        crate::sched::trace::record_switch(self, previous, next_pid, reason);
        true
    }

    pub fn preempt_to(&mut self, next_pid: Pid, reason: u64) -> bool {
        let current_pid = match self.current {
            Some(pid) => pid,
            None => return self.switch_to(next_pid, reason),
        };
        if current_pid == next_pid {
            return false;
        }
        if self.get(next_pid).map_or(true, |p| {
            p.state != ProcessState::Runnable || p.on_cpu.is_some()
        }) {
            return false;
        }
        let requeue_current = if let Some(current) = self.get_mut(current_pid) {
            if current.state == ProcessState::Running {
                set_process_state(current, ProcessState::Runnable);
                true
            } else {
                false
            }
        } else {
            false
        };
        if requeue_current {
            self.enqueue_ready(current_pid);
        }
        self.switch_to(next_pid, reason)
    }

    fn take_ready_pid(&mut self, pid: Pid) -> Option<Pid> {
        if pid == 0 || pid == INVALID_PID {
            return None;
        }
        if crate::sched::process_sched::kstack_in_use_by_any_hart(pid) {
            return None;
        }
        if self.processes.get(&pid).map_or(false, |p| {
            p.state == ProcessState::Runnable && p.on_cpu.is_none()
        }) {
            self.remove_ready(pid);
            Some(pid)
        } else {
            None
        }
    }

    fn take_child_handoff(&mut self, current_pid: Pid, preferred_tgid: Option<Pid>) -> Option<Pid> {
        let child_pid = self.get_mut(current_pid).and_then(|p| p.take_yield_to_child())?;
        let child_ok = self.get(child_pid).map_or(false, |child| {
            child.state == ProcessState::Runnable
                && preferred_tgid.map_or(true, |tgid| child.tgid == tgid)
        });
        if child_ok {
            self.take_ready_pid(child_pid)
        } else {
            None
        }
    }

    /// HUNGDBG: dump every task's scheduling/wait state. Used by the hung-task
    /// watchdog (Linux khungtaskd 风格) to identify a lost-wakeup deadlock.
    ///
    /// 调用方持表锁，故只做内存快照；串口输出必须由 [`HangSnapshot::print`]
    /// 在放锁之后完成。实测持锁直打时单次 dump 占住 PROCESS_TABLE 约 9.5s，
    /// 其余 hart 全部自旋，诊断本身就能把系统压成「无可运行任务」，制造出它
    /// 本该观测的那个 hang。
    pub fn snapshot_for_hang(&self) -> HangSnapshot {
        let mut tasks = alloc::vec::Vec::with_capacity(self.processes.len());
        for (&pid, p) in self.processes.iter() {
            let (nr, a) = p.last_syscall;
            tasks.push(HangTaskRow {
                pid,
                tgid: p.tgid,
                comm: p.comm.clone(),
                state: p.state,
                on_cpu: p.on_cpu,
                wait_channel: p.wait_channel,
                waiting_for: p.waiting_for,
                woken: p.waitqueue_woken,
                wakeup_channel: p.wakeup_channel,
                on_ready_queue: p.on_ready_queue,
                pending_signals: p.pending_signals,
                syscall_nr: nr as i64,
                args: [a[0], a[1], a[2]],
            });
        }
        // 每 hart 的所有权矩阵：current_by_hart 与无锁影子（CURRENT/KSTACK/
        // STAGED）联合可直接指认「谁持有一个 Running-却无人执行 的任务」。
        let mut harts = alloc::vec::Vec::new();
        for hart in 0..crate::sched::smp::MAX_CPUS {
            use core::sync::atomic::Ordering;
            let shadow_pid =
                crate::sched::process_sched::HART_CURRENT_PID.load_at(hart, Ordering::Relaxed);
            let kstack_owner =
                crate::sched::process_sched::HART_KSTACK_OWNER.load_at(hart, Ordering::Relaxed);
            let staged_previous =
                crate::sched::process_sched::staged_previous_pid_at(hart);
            let staged =
                crate::sched::process_sched::staged_pid_at(hart);
            if self.current_by_hart[hart].is_none()
                && shadow_pid == 0
                && kstack_owner == 0
                && staged == 0
            {
                continue;
            }
            harts.push(HangHartRow {
                hart,
                current_by_hart: self.current_by_hart[hart],
                shadow_pid,
                kstack_owner,
                staged_previous,
                staged,
            });
        }
        HangSnapshot { current: self.current, tasks, harts }
    }

    pub fn select_next(&mut self, current_pid: Pid, preferred_tgid: Option<Pid>) -> Option<Pid> {
        let dest = current_hart_slot();
        self.drain_wakelist(dest);
        if let Some(pid) = self.take_child_handoff(current_pid, preferred_tgid) {
            return Some(pid);
        }
        if let Some(tgid) = preferred_tgid {
            if let Some(pid) = self.next_startup_after(current_pid, tgid) {
                if let Some(taken) = self.take_ready_pid(pid) {
                    return Some(taken);
                }
            }
            if let Some(pid) = self.next_startup_leader(current_pid, tgid) {
                if let Some(taken) = self.take_ready_pid(pid) {
                    return Some(taken);
                }
            }
        }
        if SIG_SCAN_HINT.load(core::sync::atomic::Ordering::Relaxed) {
            let (found, any_pending) =
                self.next_runnable_with_pending_signal(current_pid, preferred_tgid);
            if let Some(pid) = found {
                if let Some(taken) = self.take_ready_pid(pid) {
                    return Some(taken);
                }
            }
            if !any_pending {
                SIG_SCAN_HINT.store(false, core::sync::atomic::Ordering::Relaxed);
            }
        }
        if let Some(pid) = self.pick_from_cpu(dest, dest, current_pid, preferred_tgid) {
            return Some(pid);
        }
        if let Some(pid) = self.steal_for_cpu(dest, current_pid, preferred_tgid) {
            return Some(pid);
        }
        let fallback = self.next_runnable_after(current_pid, preferred_tgid)?;
        self.take_ready_pid(fallback)
    }

    fn pick_from_cpu(
        &mut self,
        src: usize,
        dest: usize,
        current_pid: Pid,
        preferred_tgid: Option<Pid>,
    ) -> Option<Pid> {
        if src >= MAX_CPUS {
            return None;
        }
        if src != dest {
            self.drain_wakelist(src);
        }
        let hpid = {
            let mut rq = lock_rq(src);
            rq.take_handoff()
        };
        if hpid != INVALID_PID {
            let local_empty = rq_nr_running(src) == 0;
            if hpid != current_pid
                && preferred_tgid.is_none()
                && local_empty
                && self.task_runnable_relaxed(hpid, dest)
                && self.get(hpid).map_or(false, |p| {
                    p.sched_policy == 0 && (p.pending_signals & !p.signal_mask) == 0
                })
            {
                if let Some(p) = self.get_mut(hpid) {
                    p.rq_cpu = Some(dest);
                }
                return Some(hpid);
            }
            self.enqueue_ready_wf(hpid, WF_SYNC);
        }
        if preferred_tgid.is_none() {
            let only = {
                let rq = lock_rq(src);
                if rq.rt.is_empty() && rq.dl.is_empty() {
                    rq.cfs.only_pid()
                } else {
                    None
                }
            };
            if let Some(pid) = only {
                if pid != current_pid && self.task_runnable_on(pid, dest) {
                    self.unlink_ready(pid);
                    if let Some(p) = self.get_mut(pid) {
                        p.rq_cpu = Some(dest);
                    }
                    return Some(pid);
                }
            }
        }
        if RT_DL_PRESENT.load(core::sync::atomic::Ordering::Relaxed) {
            if let Some(pid) = self.pick_rt_from_cpu(src, dest, current_pid) {
                return Some(pid);
            }
        }
        if SIG_SCAN_HINT.load(core::sync::atomic::Ordering::Relaxed) {
            if let Some(pid) = self.dequeue_ready_matching(src, dest, current_pid, |p| {
                (p.pending_signals & !p.signal_mask) != 0
            }) {
                return Some(pid);
            }
        }
        if let Some(tgid) = preferred_tgid {
            if let Some(pid) = self.dequeue_ready_matching(src, dest, current_pid, |p| p.tgid == tgid)
            {
                return Some(pid);
            }
        }
        if let Some(pid) = self.pick_cfs_from_cpu(src, dest, current_pid) {
            return Some(pid);
        }
        if let Some(pid) = self.dequeue_ready_matching(src, dest, current_pid, |p| {
            crate::kernel::cgroup::cgroup_cpu_can_run(&p.cgroup_path)
                && !crate::kernel::cgroup::cgroup_is_frozen(&p.cgroup_path)
        }) {
            return Some(pid);
        }
        self.dequeue_ready_matching(src, dest, current_pid, |_| true)
    }

    /// Linux RT/DL 选任务：DL 队列按 EDF，RT 队列按 bitmap 优先级 +
    /// 同级 FIFO。快照只是迁移检查用；选择顺序由队列结构保证，不再
    /// 全队列扫描比较 deadline/priority。
    fn pick_rt_from_cpu(&mut self, src: usize, dest: usize, current_pid: Pid) -> Option<Pid> {
        let dl_snap: Vec<Pid> = {
            let rq = lock_rq(src);
            rq.dl.pids_edf()
        };
        for pid in dl_snap {
            let valid = pid != current_pid
                && self.get(pid).map_or(false, |p| {
                    p.state == ProcessState::Runnable && p.sched_policy == 6
                });
            if !valid {
                let mut rq = lock_rq(src);
                rq.dl.remove_by_pid(pid);
                publish_rq(src, &rq);
                continue;
            }
            if !self.task_runnable_on(pid, dest) {
                continue;
            }
            self.unlink_ready(pid);
            if let Some(p) = self.get_mut(pid) {
                p.rq_cpu = Some(dest);
            }
            return Some(pid);
        }

        let rt_snap: Vec<Pid> = {
            let rq = lock_rq(src);
            rq.rt.pids_by_priority()
        };
        for pid in rt_snap {
            let valid = pid != current_pid
                && self.get(pid).map_or(false, |p| {
                    p.state == ProcessState::Runnable
                        && matches!(p.sched_policy, 1 | 2)
                });
            if !valid {
                let mut rq = lock_rq(src);
                rq.rt.remove(pid);
                publish_rq(src, &rq);
                continue;
            }
            if !self.task_runnable_on(pid, dest) {
                continue;
            }
            self.unlink_ready(pid);
            if let Some(p) = self.get_mut(pid) {
                p.rq_cpu = Some(dest);
            }
            return Some(pid);
        }
        None
    }

    fn pick_cfs_from_cpu(&mut self, src: usize, dest: usize, current_pid: Pid) -> Option<Pid> {
        let pid = {
            let rq = lock_rq(src);
            rq.cfs.pick_eevdf(|pid| {
                pid != current_pid
                    && pid != INVALID_PID
                    && self.task_runnable_on(pid, dest)
                    && self.get(pid).map_or(false, |p| p.sched_policy == 0)
            })
        }?;
        self.unlink_ready(pid);
        if let Some(p) = self.get_mut(pid) {
            p.rq_cpu = Some(dest);
        }
        Some(pid)
    }

    fn steal_for_cpu(
        &mut self,
        dest: usize,
        current_pid: Pid,
        preferred_tgid: Option<Pid>,
    ) -> Option<Pid> {
        let mask = crate::sched::smp::online_mask_fast();
        let mut busiest = [(0usize, 0u64); MAX_CPUS];
        let busy_n = busiest_cpus(dest, mask, &mut busiest);
        for &(src, _) in &busiest[..busy_n] {
            // 源核只剩 1 个可运行且自己空闲时仍可偷（newidle）；源核正在跑则至少留 1。
            let src_busy = self.current_by_hart[src].is_some();
            if src_busy && rq_nr_running(src) < 1 {
                continue;
            }
            if let Some(pid) = self.pick_from_cpu(src, dest, current_pid, preferred_tgid) {
                return Some(pid);
            }
        }
        None
    }

    pub fn has_other_runnable(&self, _current_pid: Pid) -> bool {
        any_queued()
    }

    /// 是否存在调度优先级高于 `current_pid` 的就绪任务。
    pub fn has_higher_priority_runnable(&self, current_pid: Pid) -> bool {
        // 纯 CFS 系统从未出现过 RT/DL 任务：直接短路。RT_DL_PRESENT 一旦
        // 置位永久保持，语义等价于始终扫描。
        if !RT_DL_PRESENT.load(core::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        let rank = |policy: i32, base: i32, boost: i32| -> (u8, i32) {
            match policy {
                6 => (3, 0),
                1 | 2 => (2, if boost > base { boost } else { base }),
                _ => (1, 0),
            }
        };
        let (cur_rank, cur_prio) = match self.get(current_pid) {
            Some(p) => rank(p.sched_policy, p.sched_priority, p.pi_boost_priority),
            None => return true,
        };
        for h in 0..MAX_CPUS {
            let mut found = false;
            {
                let rq = lock_rq(h);
                let mut check = |pid: Pid| -> bool {
                    if pid == current_pid || pid == 0 {
                        return false;
                    }
                    let Some(p) = self.processes.get(&pid) else {
                        return false;
                    };
                    if p.state != ProcessState::Runnable || !p.on_ready_queue {
                        return false;
                    }
                    let (r, pr) = rank(p.sched_policy, p.sched_priority, p.pi_boost_priority);
                    r > cur_rank || (r == cur_rank && r == 2 && pr > cur_prio)
                };
                found = rq.dl.any_edf(&mut check) || rq.rt.any_priority(&mut check);
            }
            if found {
                return true;
            }
        }
        false
    }

    /// 返回 (优先调度候选, 全表是否存在任何未决未屏蔽信号)。
    /// 后者用于清除 `SIG_SCAN_HINT`：仅当全表确认无任何未决未屏蔽信号（含非
    /// Runnable 者）时才可安全短路后续扫描。
    fn next_runnable_with_pending_signal(
        &self,
        current_pid: Pid,
        preferred_tgid: Option<Pid>,
    ) -> (Option<Pid>, bool) {
        let mut any_pending = false;
        let mut found: Option<Pid> = None;
        let mut tgid_found: Option<Pid> = None;
        for (&pid, process) in self.processes.iter() {
            if (process.pending_signals & !process.signal_mask) == 0 {
                continue;
            }
            any_pending = true;
            if pid == current_pid || process.state != ProcessState::Runnable {
                continue;
            }
            if found.is_none() {
                found = Some(pid);
            }
            if preferred_tgid == Some(process.tgid) && tgid_found.is_none() {
                tgid_found = Some(pid);
            }
            if tgid_found.is_some() || (preferred_tgid.is_none() && found.is_some()) {
                break;
            }
        }
        (tgid_found.or(found), any_pending)
    }

    /// 把持有 startup_credits 的新生线程登记进候选队列（clone / 信用转移时调用）。
    pub(crate) fn note_startup_candidate(&mut self, pid: Pid) {
        self.startup_queue.push_back(pid);
    }

    /// 从 startup_queue 惰性取下一个可运行的新生线程（代替旧的全表 O(n) 双扫描）。
    /// 失效项（已退出/信用耗尽/非线程）直接剔除；暂不匹配（别的 tgid / 未就绪 /
    /// 恰为 current）轮转到队尾保留。最多轮转一圈防死循环。
    fn next_startup_after(&mut self, current_pid: Pid, tgid: Pid) -> Option<Pid> {
        let mut remaining = self.startup_queue.len();
        while remaining > 0 {
            remaining -= 1;
            let pid = match self.startup_queue.pop_front() {
                Some(pid) => pid,
                None => return None,
            };
            let (valid, matches) = match self.processes.get(&pid) {
                Some(p) if p.startup_credits > 0 && p.is_thread => (
                    true,
                    pid != current_pid && p.tgid == tgid && p.state == ProcessState::Runnable,
                ),
                _ => (false, false),
            };
            if !valid {
                continue; // 剔除失效项
            }
            if !matches {
                self.startup_queue.push_back(pid); // 保留待后续
                continue;
            }
            let still_has_credits = if let Some(process) = self.get_mut(pid) {
                process.startup_credits = process.startup_credits.saturating_sub(1);
                process.startup_credits > 0
            } else {
                false
            };
            if still_has_credits {
                self.startup_queue.push_back(pid);
            }
            return Some(pid);
        }
        None
    }

    fn next_startup_leader(&mut self, current_pid: Pid, tgid: Pid) -> Option<Pid> {
        if current_pid == tgid {
            return None;
        }
        let ready = self.processes.get(&tgid).map_or(false, |leader| {
            leader.state == ProcessState::Runnable && leader.startup_credits > 0
        });
        if ready {
            if let Some(leader) = self.get_mut(tgid) {
                leader.startup_credits = leader.startup_credits.saturating_sub(1);
            }
            Some(tgid)
        } else {
            None
        }
    }

    pub fn take_startup_handoff_target(&mut self, current_pid: Pid) -> Option<Pid> {
        let target_pid = {
            let current = self.get_mut(current_pid)?;
            if current.startup_credits == 0 {
                return None;
            }
            current.startup_credits = 0;
            if current.is_thread {
                Some(current.tgid)
            } else {
                None
            }
        }?;
        if self.get(target_pid).map_or(false, |target| target.state == ProcessState::Runnable) {
            if let Some(target) = self.get_mut(target_pid) {
                target.startup_credits = THREAD_STARTUP_CREDITS;
            }
            if self.get(target_pid).map_or(false, |t| t.is_thread) {
                self.note_startup_candidate(target_pid);
            }
            Some(target_pid)
        } else {
            None
        }
    }

    pub fn next_runnable_after(&mut self, current_pid: Pid, preferred_tgid: Option<Pid>) -> Option<Pid> {
        if let Some(tgid) = preferred_tgid {
            if let Some(pid) = self.next_startup_after(current_pid, tgid) {
                return Some(pid);
            }
            if let Some(pid) = self.next_startup_leader(current_pid, tgid) {
                return Some(pid);
            }
        }

        // 单遍 O(N) 收集全部调度类候选，替代原先 ~8 趟独立全表扫描。BTreeMap
        // 升序遍历 ⇒「首个命中即最小 pid」，故 *_gt/*_any/*_lt 用 is_none 判定即
        // 等价于原 `find` 的「首个匹配」语义。hackbench 数千任务时每次 schedule
        // 的 PROCESS_TABLE 锁持有时间线性下降 → 定时器中断/RT 唤醒不再被长临界区
        // 拖迟（cyclictest STRESS 尾延迟根因之一）。优先级次序与原实现严格一致：
        // deadline → RT(PI-aware) → 挂起信号 → preferred_tgid → CFS → 任意。
        let mut dl_best_pid: Option<Pid> = None;
        let mut dl_best_deadline = u64::MAX;
        let mut rt_max_pri: i32 = -1;
        let mut rt_cand_gt: Option<Pid> = None;
        let mut rt_cand_any: Option<Pid> = None;
        let mut sig_gt: Option<Pid> = None;
        let mut sig_any: Option<Pid> = None;
        let mut tgid_gt: Option<Pid> = None;
        let mut tgid_lt: Option<Pid> = None;
        let mut cfs_best_pid: Option<Pid> = None;
        let mut cfs_best_vrt: u64 = u64::MAX;
        let mut any_gt: Option<Pid> = None;
        let mut any_lt: Option<Pid> = None;
        for (&pid, process) in self.processes.iter() {
            if process.state != ProcessState::Runnable {
                continue;
            }
            let is_other = pid != current_pid;
            let gt = pid > current_pid;
            // SCHED_DEADLINE (policy 6): EDF — earliest deadline first, above RT
            if is_other
                && process.sched_policy == 6
                && process.dl_deadline > 0
                && process.dl_deadline < dl_best_deadline
            {
                dl_best_deadline = process.dl_deadline;
                dl_best_pid = Some(pid);
            }
            // RT (SCHED_FIFO=1 / SCHED_RR=2): highest effective priority (PI-aware)
            if is_other && (process.sched_policy == 1 || process.sched_policy == 2) {
                let eff = if process.pi_boost_priority > process.sched_priority {
                    process.pi_boost_priority
                } else {
                    process.sched_priority
                };
                if eff > rt_max_pri {
                    rt_max_pri = eff;
                    rt_cand_gt = if gt { Some(pid) } else { None };
                    rt_cand_any = Some(pid);
                } else if eff == rt_max_pri {
                    if gt && rt_cand_gt.is_none() {
                        rt_cand_gt = Some(pid);
                    }
                }
            }
            // 挂起且未屏蔽信号
            if (process.pending_signals & !process.signal_mask) != 0 {
                if gt && sig_gt.is_none() {
                    sig_gt = Some(pid);
                }
                if is_other && sig_any.is_none() {
                    sig_any = Some(pid);
                }
            }
            // preferred_tgid
            if let Some(tgid) = preferred_tgid {
                if process.tgid == tgid {
                    if gt {
                        if tgid_gt.is_none() {
                            tgid_gt = Some(pid);
                        }
                    } else if pid < current_pid && tgid_lt.is_none() {
                        tgid_lt = Some(pid);
                    }
                }
            }
            // CFS vruntime
            if is_other && process.sched_policy == 0 && process.vruntime < cfs_best_vrt {
                cfs_best_vrt = process.vruntime;
                cfs_best_pid = Some(pid);
            }
            // 任意可运行兜底
            if gt {
                if any_gt.is_none() {
                    any_gt = Some(pid);
                }
            } else if pid < current_pid && any_lt.is_none() {
                any_lt = Some(pid);
            }
        }
        if dl_best_pid.is_some() {
            return dl_best_pid;
        }
        if rt_max_pri >= 0 {
            return rt_cand_gt.or(rt_cand_any);
        }
        if let Some(pid) = sig_gt.or(sig_any) {
            return Some(pid);
        }
        if preferred_tgid.is_some() {
            if let Some(pid) = tgid_gt.or(tgid_lt) {
                return Some(pid);
            }
        }
        if cfs_best_pid.is_some() {
            return cfs_best_pid;
        }
        any_gt.or(any_lt)
    }

    /// hangdump 误报门禁：是否存在「用户态推进中」或「待调度」的任务。
    ///
    /// `state==Running 且 cpu_in_kernel==false` = 正在用户态计算（rustc codegen
    /// 的经典形态：切换计数可冻结数分钟但系统健康）；`Runnable` / `on_ready_queue`
    /// = 调度器有待投任务。任一成立即判定系统活着，禁止全表倒出——观测手段
    /// 不得伪造出它要观测的停摆。
    pub fn has_user_running_or_runnable(&self) -> bool {
        self.processes.values().any(|p| {
            (p.state == ProcessState::Running && !p.cpu_in_kernel)
                || p.state == ProcessState::Runnable
                || p.on_ready_queue
        })
    }

    /// HANGDUMP=1 法证：**持锁只格式化**全部进程的调度状态与等待点，输出行
    /// 由调用方在放开 PROCESS_TABLE 之后打印。
    ///
    /// 持锁直打时，串口输出时长（远大于格式化）全部计入表锁临界区：其余 hart
    /// 的 timer/trap 路径关中断堆在锁上，TCG 下这些空转 vCPU 反过来饿死正在
    /// 打印的 hart，实测单次倒出把表锁占住 128 秒、整机形同冻结。快照-后打是
    /// 观测无害化的硬规则（HUNGDBG 周期采样已因同一教训改造过）。
    pub fn hangdump_lines(&self, switches: usize) -> alloc::vec::Vec<alloc::string::String> {
        let mut lines = alloc::vec::Vec::with_capacity(self.processes.len() + 8);
        lines.push(alloc::format!(
            "[hangdump] ==== ctx-switches frozen at {} | {} procs | current={:?} ====",
            switches,
            self.processes.len(),
            self.current
        ));
        for (pid, p) in self.processes.iter() {
            let sepc = if p.trap_context_ptr != 0 {
                unsafe { (*(p.trap_context_ptr as *const crate::arch::TrapContext)).sepc }
            } else {
                0
            };
            lines.push(alloc::format!(
                "[hangdump] pid={} tgid={} ppid={:?} state={:?} wchan={} wakeup={:#x} sepc={:#x} onq={}",
                pid,
                p.tgid,
                p.parent_pid,
                p.state,
                p.wait_channel.unwrap_or("-"),
                p.wakeup_channel,
                sepc,
                p.on_ready_queue
            ));
        }
        // 光有任务表分不清「等待者还没被唤醒」和「唤醒发生过但丢了」：两者
        // 都显示 Sleeping+futex_wait_queue_me。补上每个等待字的当前真实值，
        // 值已被改写却仍在睡 = 丢唤醒，且地址直接点名。
        crate::kernel::futex::futex_waiter_lines_for_hang(self, &mut lines);
        lines.push(alloc::string::String::from("[hangdump] ==== end ===="));
        lines
    }

    /// 唤醒所有 IO_WAIT sleeping 进程
    pub fn wake_io_waiters(&mut self) {
        // Fast-path：无任何 IO_WAIT 停泊进程 ⇒ 直接返回。`IO_WAITER_COUNT` 仅在
        // 唯一 IO_WAIT 入口自增、并在下方实际扫描中 self-heal，故读到 0 即权威。
        // 本方法有多个调用点（线程退出 / wait / signal / timerfd / *notify）**直接**
        // 调用、绕过 syscall::mod 包装的 fast-path，必须在此自带短路。
        if IO_WAITER_COUNT.load(core::sync::atomic::Ordering::Relaxed) == 0 {
            return;
        }
        // 只遍历**真实存在的进程**（O(#processes)）。旧实现 `for pid in 1..NEXT_PID`
        // 是 O(历史最大 PID)：`NEXT_PID` 随每次 clone 单调递增，libc-bench
        // pthread create/join 把它推到数千，使每次线程退出退化为 O(N) 全 PID 空扫
        // （对每个 PID 值做一次 BTreeMap get）→ 整体 O(N²)（icount profiler 实证：
        // ProcessTable::get + BTreeMap search 占 createjoin 随 N 增长项的 ~37%）。
        let to_wake: alloc::vec::Vec<Pid> = self
            .processes
            .iter()
            .filter(|(_, p)| {
                p.state == ProcessState::Sleeping
                    && p.waiting_for == Some(IO_WAIT_SENTINEL)
            })
            .map(|(&pid, _)| pid)
            .collect();
        for pid in to_wake {
            self.mark_runnable(pid, true);
        }
        // self-heal 计数器：扫描实际剩余 IO_WAIT 进程数（kill 路径会泄漏增量）
        let remaining = self.processes.values()
            .filter(|p| p.state == ProcessState::Sleeping && p.waiting_for == Some(IO_WAIT_SENTINEL))
            .count();
        IO_WAITER_COUNT.store(remaining, core::sync::atomic::Ordering::Relaxed);
    }
}



/// 获取当前进程 PID
pub fn getpid() -> Option<Pid> {
    PROCESS_TABLE.lock()
        .as_ref()
        .and_then(|t| t.current)
}

/// 返回指定 tid 任务已累积的 (user_ticks, kernel_ticks)（rdtime 单位）。
/// 供 `clock_gettime` 解码 musl `pthread_getcpuclockid` 动态 per-thread
/// CPU 时钟（CLOCK_THREAD_CPUTIME_ID 类）后取真实线程 CPU 时间。
pub fn task_cpu_ticks(tid: Pid) -> Option<(u64, u64)> {
    PROCESS_TABLE.lock()
        .as_ref()
        .and_then(|t| t.get(tid))
        .map(|p| (p.cpu_user_ticks, p.cpu_kernel_ticks))
}

/// 返回某 tgid 下所有线程的 (user_ticks, kernel_ticks) 累加值。
/// 供 CLOCK_PROCESS_CPUTIME_ID 与 per-process 动态 CPU 时钟使用。
pub fn process_cpu_ticks(tgid: Pid) -> Option<(u64, u64)> {
    let table = PROCESS_TABLE.lock();
    let table = table.as_ref()?;
    let mut user = 0u64;
    let mut kernel = 0u64;
    let mut found = false;
    for p in table.iter() {
        if p.tgid == tgid {
            user = user.saturating_add(p.cpu_user_ticks);
            kernel = kernel.saturating_add(p.cpu_kernel_ticks);
            found = true;
        }
    }
    if found { Some((user, kernel)) } else { None }
}

/// 当前线程的 tid 与其所属 tgid（用于 CLOCK_THREAD/PROCESS_CPUTIME_ID）。
pub fn current_tid_tgid() -> Option<(Pid, Pid)> {
    let table = PROCESS_TABLE.lock();
    let table = table.as_ref()?;
    let cur = table.current?;
    let tgid = table.get(cur).map(|p| p.tgid).unwrap_or(cur);
    Some((cur, tgid))
}

/// 获取父进程 PID
pub fn getppid() -> Option<Pid> {
    PROCESS_TABLE.lock()
        .as_ref()
        .and_then(|t| t.current())
        .and_then(|p| p.parent_pid)
}

/// 设置堆边界（brk 系统调用）
/// 设置堆边界（brk 系统调用）
/// 当堆扩展时，添加或扩展 Heap VMA 使缺页处理能延迟分配物理页
/// 当堆收缩时，释放多余页面并收缩 Heap VMA
pub fn brk(new_brk: VirtAddr) -> Result<VirtAddr, &'static str> {
    let mut table = PROCESS_TABLE.lock();
    let table = table.as_mut().ok_or("Process table not initialized")?;

    let current_pid = table.current.ok_or("No current process")?;
    // Thread fallback: use tgid leader's address space and heap
    let as_pid = {
        let p = table.get(current_pid).ok_or("Current process not found")?;
        if p.mm.is_some() { current_pid } else { p.tgid }
    };
    let process = table.get_mut(as_pid).ok_or("Leader process not found")?;

    if new_brk.0 < process.heap_start.0 {
        return Err("Invalid brk: below heap start");
    }

    let old_end = process.heap_end;
    // VMA end 比 brk 多 2 页 padding：musl mallocng 在 brk 边界附近还会写 chunk 元数据
    // 到下一页边界，所以仅 +1 页不够（曾导致 LA libc-bench BADV=brk_aligned 0x12002c000
    // 落在 Mmap PROT_NONE 守护页里，PME 不可恢复）。
    let aligned_end = VirtAddr(((new_brk.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)) + 2 * PAGE_SIZE);

    if new_brk.0 > old_end.0 {
        // Linux `do_brk_flags` refuses to grow the break over an existing
        // mapping (find_vma_intersection): the newly requested page range
        // [PAGE_ALIGN(old_brk), PAGE_ALIGN(new_brk)) must be free. A SysV shm
        // segment (or any mmap) attached just past the break therefore caps
        // brk growth — growing into it returns ENOMEM (shmt09 case 4). The
        // heap VMA itself (incl. its brk padding) is excluded from the check.
        {
            let check_start = (old_end.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            let check_end = (new_brk.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
            if check_end > check_start {
                if let Some(ref mm) = process.mm {
                    let addr_space = mm.lock();
                    // Linux `do_brk_flags` 只把「当前 brk 所在的那条堆 VMA」
                    // 视为自身可扩展区域，其它任何映射（包括另一条 Heap VMA）
                    // 都是硬边界——否则堆会与它重叠，破坏 VMA 不重叠不变式。
                    let self_heap = addr_space
                        .find_vma(old_end)
                        .filter(|v| v.vma_type == crate::mm::VmaType::Heap)
                        .map(|v| (v.start.0, v.end.0));
                    let collides = addr_space.vmas_iter().any(|vma| {
                        let is_self_heap = self_heap
                            .is_some_and(|(s, e)| vma.start.0 == s && vma.end.0 == e);
                        !is_self_heap && check_start < vma.end.0 && check_end > vma.start.0
                    });
                    if collides {
                        return Err("ENOMEM: brk would collide with existing mapping");
                    }
                }
            }
        }
        // cgroup memory.max enforcement: check before expanding heap
        let expand_bytes = (new_brk.0 - old_end.0) as u64;
        if !crate::kernel::cgroup::cgroup_memory_can_charge(&process.cgroup_path, expand_bytes) {
            return Err("ENOMEM: cgroup memory.max exceeded");
        }
        crate::kernel::cgroup::cgroup_memory_charge(&process.cgroup_path, expand_bytes);

        // 堆扩展：确保有 VMA 覆盖新区域并预映射物理页
        if let Some(ref mm) = process.mm {
            let mut addr_space = mm.lock();
            // 以当前 brk（old_end）所在的 Heap VMA 为锚点扩展，而不是盲用
            // heap_start：MAP_FIXED 可以合法地把 heap_start 所在页替换成其它
            // 映射（Linux 语义），此时按 heap_start 查找会命中那个外来 VMA
            // 并把它当堆扩展（brk01 LA：PROT_NONE 守护页被拉长后反过来
            // 堵死 brk 碰撞检查）。
            let heap_anchor = addr_space.find_vma(old_end)
                .filter(|v| v.vma_type == crate::mm::VmaType::Heap)
                .map(|v| v.start);
            if let Some(anchor) = heap_anchor {
                addr_space.expand_heap_vma(anchor, aligned_end);
            } else {
                use crate::mm::{VmaType, VmaPermission};
                let perm = VmaPermission::new(true, true, false, true);
                let seg_start = VirtAddr(old_end.0 & !(PAGE_SIZE - 1));
                addr_space.map_region(seg_start, aligned_end, VmaType::Heap, perm);
            }
        }
    } else if new_brk.0 < old_end.0 {
        // 堆收缩：释放 [new_aligned_end, old_aligned_end) 范围的页面并收缩 VMA
        let old_aligned_end = VirtAddr(((old_end.0 + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)) + PAGE_SIZE);

        if let Some(ref mm) = process.mm {
            let mut addr_space = mm.lock();
            if aligned_end < old_aligned_end {
                addr_space.unmap_region(aligned_end, old_aligned_end);
            }
            if let Some(vma) = addr_space
                .find_vma_mut(new_brk)
                .filter(|v| v.vma_type == crate::mm::VmaType::Heap)
            {
                vma.end = aligned_end;
            }
        }
    }
    // new_brk == old_end 时无需任何操作

    process.heap_end = new_brk;
    Ok(process.heap_end)
}

/// 创建 init 进程（PID 1）
///
/// init 进程是系统的第一个用户进程，负责启动其他用户程序。
///

/// 获取进程的 SATP 值
///
/// 用于切换到指定进程的地址空间。
pub fn get_process_satp(pid: Pid) -> Option<usize> {
    PROCESS_TABLE.lock()
        .as_ref()
        .and_then(|t| t.get(pid))
        .and_then(|p| Some(p.satp()))
}

/// 切换到指定进程的地址空间
///
/// # 安全性
/// 此函数会修改 SATP 寄存器，必须在适当的上下文中调用。
pub unsafe fn switch_address_space(pid: Pid) {
    if let Some(satp) = get_process_satp(pid) {
        if satp != 0 {
            core::arch::asm!(
                "csrw satp, {}",
                "sfence.vma",
                in(reg) satp
            );
        }
    }
}


/// 进程管理自测试
pub fn self_test() {
    crate::println!("[process] Running self-test...");

    // 测试常量
    assert_eq!(INVALID_PID, 0);
    assert_eq!(MAX_PROCESSES, 4096);
    assert_eq!(DEFAULT_USER_STACK_PAGES, 8);
    #[cfg(target_arch = "loongarch64")]
    assert_eq!(DEFAULT_USER_STACK_TOP, 0x7F_FFFE_0000);
    #[cfg(not(target_arch = "loongarch64"))]
    assert_eq!(DEFAULT_USER_STACK_TOP, 0x8000_0000);

    // 测试 ProcessState
    let _states = [
        ProcessState::Unused,
        ProcessState::Embryo,
        ProcessState::Runnable,
        ProcessState::Running,
        ProcessState::Sleeping,
        ProcessState::Zombie,
    ];

    // 测试 ExitReason。Slice 2.0 后 Killed 合并到 signaled(9)，
    // 保留三个变体以覆盖：Normal / Signaled(非 dump) / Signaled(dump)。
    let _reasons = [
        ExitReason::Normal(0),
        ExitReason::signaled(9),
        ExitReason::signaled_with_dump(11),
    ];

    // 测试 ProcessControlBlock
    let pcb = ProcessControlBlock::new(100);
    assert_eq!(pcb.pid, 100);
    assert_eq!(pcb.state, ProcessState::Embryo);
    assert!(pcb.mm.is_none());
    assert_eq!(pcb.user_stack_top.0, DEFAULT_USER_STACK_TOP);
    assert_eq!(pcb.user_stack_bottom.0, DEFAULT_USER_STACK_BOTTOM);
    assert!(pcb.exec_path.is_empty());

    // 测试地址空间创建
    let mut pcb_with_addr_space = ProcessControlBlock::new(101);
    let result = pcb_with_addr_space.create_address_space();
    assert!(result.is_ok(), "Failed to create address space");
    assert!(pcb_with_addr_space.mm.is_some());
    assert!(pcb_with_addr_space.page_table_root.0 != 0);

    // 测试 SATP 获取
    let satp = pcb_with_addr_space.satp();
    assert!(satp != 0, "SATP should not be zero");
    crate::println!("[process] Created address space with SATP: {:#x}", satp);

    // 测试内核栈分配
    let kernel_stack_result = pcb_with_addr_space.allocate_kernel_stack();
    assert!(kernel_stack_result.is_ok(), "Failed to allocate kernel stack");
    assert!(pcb_with_addr_space.kernel_stack.is_some());
    assert!(pcb_with_addr_space.trap_context_ptr != 0);

    // 验证内核栈属性
    let kernel_stack = pcb_with_addr_space.kernel_stack.as_ref().unwrap();
    assert_eq!(kernel_stack.size, DEFAULT_KERNEL_STACK_SIZE);
    assert!(kernel_stack.bottom != 0);
    assert_eq!(kernel_stack.top(), kernel_stack.bottom + kernel_stack.size);
    crate::println!("[process] Kernel stack: bottom={:#x}, top={:#x}, size={}",
        kernel_stack.bottom, kernel_stack.top(), kernel_stack.size);

    // 验证 TrapContext 位置
    let trap_ctx_pos = kernel_stack.trap_context_position();
    assert!(trap_ctx_pos >= kernel_stack.bottom);
    assert!(trap_ctx_pos < kernel_stack.top());
    crate::println!("[process] TrapContext position: {:#x}", trap_ctx_pos);

    // 保存内核栈信息用于后续验证
    let kernel_stack_top_val = kernel_stack.top();

    // 测试 kernel_stack_top 方法
    assert_eq!(pcb_with_addr_space.kernel_stack_top(), kernel_stack_top_val);

    // 测试 init_trap_context 方法
    // 设置入口点用于测试
    pcb_with_addr_space.entry_point = VirtAddr(0x10000);
    let init_result = pcb_with_addr_space.init_trap_context();
    assert!(init_result.is_ok(), "Failed to init trap context");

    // 验证 TrapContext 内容
    unsafe {
        if let Some(trap_cx) = pcb_with_addr_space.trap_context() {
            // 验证 sepc = entry_point (Requirement 5.1)
            assert_eq!(trap_cx.sepc, 0x10000, "sepc should equal entry point");

            // 验证 sp = user_stack_top
            assert_eq!(trap_cx.sp(), DEFAULT_USER_STACK_TOP, "sp should equal user stack top");

            // 验证 sstatus.SPP = 0 (User mode) (Requirement 5.2)
            // SPP 是 bit 8
            let spp = (trap_cx.sstatus >> 8) & 1;
            assert_eq!(spp, 0, "sstatus.SPP should be 0 (User mode)");

            // 验证 sstatus.SPIE = 1 (Enable interrupts after sret) (Requirement 5.3)
            // SPIE 是 bit 5
            let spie = (trap_cx.sstatus >> 5) & 1;
            assert_eq!(spie, 1, "sstatus.SPIE should be 1");

            // 验证 kernel_sp = kernel_stack_top
            assert_eq!(trap_cx.kernel_sp, kernel_stack_top_val, "kernel_sp should equal kernel stack top");

            // 验证 kernel_satp（在分页启用时不为 0）
            // 注意：在自测试期间，内核可能还没有启用分页，所以 kernel_satp 可能为 0
            // 这是正常的，因为内核使用恒等映射
            let kernel_satp_val: usize;
            kernel_satp_val = crate::arch::asm_helpers::read_satp();
            if kernel_satp_val != 0 {
                assert!(trap_cx.kernel_satp != 0, "kernel_satp should not be zero when paging is enabled");
            } else {
                crate::println!("[process] Note: kernel_satp is 0 (paging not enabled yet)");
            }

            // 验证 trap_handler 不为 0
            assert!(trap_cx.trap_handler != 0, "trap_handler should not be zero");

            crate::println!("[process] TrapContext initialized:");
            crate::println!("  sepc={:#x}, sp={:#x}", trap_cx.sepc, trap_cx.sp());
            crate::println!("  sstatus={:#x} (SPP={}, SPIE={})", trap_cx.sstatus, spp, spie);
            crate::println!("  kernel_satp={:#x}, kernel_sp={:#x}", trap_cx.kernel_satp, trap_cx.kernel_sp);
        }
    }

    // 测试 init_trap_context 在没有内核栈时应该失败
    let mut pcb_no_stack = ProcessControlBlock::new(103);
    let no_stack_result = pcb_no_stack.init_trap_context();
    assert!(no_stack_result.is_err(), "init_trap_context should fail without kernel stack");

    // 测试地址空间访问
    assert!(pcb_with_addr_space.address_space_lock().is_some());

    // 测试文件描述符操作
    let mut pcb_fd_test = ProcessControlBlock::new(102);
    assert_eq!(pcb_fd_test.open_fd_count(), 3); // stdin, stdout, stderr

    // 测试关闭文件描述符
    let close_result = pcb_fd_test.close_fd(0);
    assert!(close_result.is_ok());
    assert_eq!(pcb_fd_test.open_fd_count(), 2);

    // 测试关闭无效文件描述符
    let invalid_close = pcb_fd_test.close_fd(100);
    assert!(invalid_close.is_err());

    // 测试关闭已关闭的文件描述符
    let double_close = pcb_fd_test.close_fd(0);
    assert!(double_close.is_err());

    // 测试资源释放
    pcb_with_addr_space.release_resources();
    assert!(pcb_with_addr_space.mm.is_none());
    assert_eq!(pcb_with_addr_space.page_table_root.0, 0);
    assert!(pcb_with_addr_space.kernel_stack.is_none());
    assert_eq!(pcb_with_addr_space.trap_context_ptr, 0);
    assert!(pcb_with_addr_space.is_resources_released());

    // 测试 ProcessTable
    let mut table = ProcessTable::new();
    assert!(table.current.is_none());

    // 测试 current_mut
    let current = table.current_mut();
    assert!(current.is_none());

    // 测试 wait（现在是 diverging 函数，不能直接调用）
    // 我们只验证函数存在
    let _wait_fn: fn(WaitTarget, ThreadMode, bool, bool, usize, usize) -> ! = wait;
    let _wait_nonblock_fn:
        fn(WaitTarget, ThreadMode, bool, bool)
            -> Result<Option<(Pid, i32, u64, u64)>, &'static str> = wait_nonblock;

    // 测试 exit（注意：exit 是 diverging 函数，不能直接调用）
    // 我们只验证函数存在
    let _exit_fn: fn(i32) -> ! = exit;
    let _exit_with_reason_fn: fn(ExitReason) -> ! = exit_with_reason;

    // 测试辅助函数存在性
    let _get_satp_fn: fn(Pid) -> Option<usize> = get_process_satp;

    // 测试 create_process_from_path 函数存在性和错误处理
    // 注意：完整测试需要文件系统已挂载且有有效 ELF 文件
    // 这里只测试函数签名和基本错误处理
    let _create_from_path_fn: fn(&str) -> Result<Pid, &'static str> = create_process_from_path;

    // 测试加载不存在的文件应该返回错误
    // 注意：这个测试依赖于文件系统状态
    // 如果 VFS 未初始化，会返回 "Process table not initialized" 或 "Failed to load ELF from filesystem"
    let result = create_process_from_path("/nonexistent_file");
    if result.is_err() {
        crate::println!("[process] create_process_from_path correctly returns error for nonexistent file");
    }

    crate::println!("[process] Self-test passed!");
}
