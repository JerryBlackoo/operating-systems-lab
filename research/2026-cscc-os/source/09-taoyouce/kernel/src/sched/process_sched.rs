//! Process scheduling, context switch, suspend, and accounting.
//!
//! Extracted from `process.rs` to separate scheduling mechanics from PCB data.

use core::sync::atomic::Ordering;
use crate::mm::VirtAddr;
use super::process::{
    ProcessControlBlock, ProcessTable, PROCESS_TABLE,
    Pid, ProcessState,
    IO_WAITER_COUNT, SIGNAL_WAIT_SENTINEL, IO_WAIT_SENTINEL,
    NANOSLEEP_SENTINEL, DEADLINE_RETURN_SENTINEL, FUTEX_WOKEN_SENTINEL,
    POLL_WAIT_SENTINEL, MQ_SLEEP_SENTINEL, MQ_WOKEN_SENTINEL,
    LOCK_WAIT_SENTINEL, SOCKET_WAIT_SENTINEL,
    set_process_state,
};
use super::process_exit::kernel_idle_until_runnable;
use super::process_trace::{
    cldbg_enabled, cldbg_timer_skip_permitted,
    wtrace_process, wtrace_save_enabled, wtrace_trap,
};

/// Per-hart kernel entry timestamp for combined syscall accounting.
/// Stored at trap entry (no lock), read by `account_syscall_combined_exit`
/// to do both user→kernel and kernel→user accounting in a single lock.
pub static HART_KERNEL_ENTER_TICK: PerHartU64 = PerHartU64::new(0);

/// Per-hart shadow of current process's deliverable signals
/// `(pending_signals & !signal_mask)`. Updated under PROCESS_TABLE lock
/// in `account_syscall_combined_exit`; read lock-free in
/// `check_and_deliver_signals` to skip the lock when 0.
pub static HART_PENDING_SIGNALS: PerHartU64 = PerHartU64::new(0);

/// Per-hart committed current PID. Architecture handoff assembly publishes it
/// only after SP has moved to the incoming kernel stack; process-table guards
/// project this same value for lock-based callers.
pub static HART_CURRENT_PID: PerHartUsize = PerHartUsize::new(0);

/// Per-hart 当前任务的调度类别（0=NONE/CFS, 1=SCHED_FIFO, 2=SCHED_RR,
/// 6=SCHED_DEADLINE）。由 `set_current` / `finish_staged_switch` 在持
/// PROCESS_TABLE 时随 PID 一起发布；RT bandwidth 的周期 tick 只读它，
/// 不再为判断“当前是否 RT”抢全局表锁。
pub static HART_CURRENT_POLICY: [core::sync::atomic::AtomicU8; crate::sched::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU8::new(0) }; crate::sched::smp::MAX_CPUS];

#[inline]
pub fn current_policy_hint() -> u8 {
    let id = crate::sched::process::current_hart_slot();
    HART_CURRENT_POLICY[id].load(core::sync::atomic::Ordering::Acquire)
}

/// Per-hart shadow pointer to the current process's PCB (`Box` contents in
/// `ProcessTable.processes` are heap-stable). Published before the matching PID
/// with release ordering at the stack-first handoff boundary; null when idle.
///
/// SAFETY contract: the accessed slot is selected by the executing hart's id,
/// and a trap/syscall runs with interrupts disabled on that hart, so nothing
/// else can concurrently mutate this hart's slot or move its PCB. A current
/// process's PCB cannot be freed while it is current (`set_current(None)`
/// precedes removal), so the pointer never dangles at its use sites.
pub static HART_CURRENT_PCB: PerHartPtr<crate::sched::process::ProcessControlBlock> =
    PerHartPtr::new();

/// 退出路径的「活动页表根帧保活」队列。
///
/// `mark_zombie` 最后一个 mm_users 退出时，`release_user_frames` 会先清空用户
/// 页，但 `MmStruct`/`AddressSpace`/`PageTable` 一旦析构，当前 hart 的 satp 仍
/// 指向的根页帧会立刻归还 buddy；SMP 下其它 hart 复用该帧后，本 hart 下一次
/// 内核取指 TLB refill 就走损坏页表，trap 入口自身 InstructionPageFault 死循环
/// （RV BuildStorm bs-longrun-rv-r6-143714 实锤）。因此把最后一个用户的 mm
/// Arc 留活到 return-to-user 激活**新** satp/PGDL 之后，再由
/// `drain_deferred_mm_drops()` 安全析构。
static DEFERRED_MM_DROPS: crate::sync::SpinLock<alloc::vec::Vec<
    alloc::sync::Arc<crate::mm::mm_struct::MmStruct>,
>> = crate::sync::SpinLock::new(alloc::vec::Vec::new());

/// 把最后一个用户退出的 mm Arc 暂存（调用方必须在旧页表仍是活动页表期间调用）。
pub(super) fn defer_last_user_mm(mm: alloc::sync::Arc<crate::mm::mm_struct::MmStruct>) {
    DEFERRED_MM_DROPS.lock().push(mm);
}

/// 新地址空间已激活后析构全部暂存 mm。调用位置必须在
/// `activate_user_satp(new_root)` 之后：此后旧根不再可能被本 hart 当作活动页表。
pub(super) fn drain_deferred_mm_drops() {
    let pending = {
        let mut queue = DEFERRED_MM_DROPS.lock();
        core::mem::take(&mut *queue)
    };
    // 在队列锁外析构：AddressSpace::drop 会执行 TLB drain/shootdown 同步。
    drop(pending);
}

/// 已由调度器选中、将在下一次 return-to-user 最终提交的任务。`switch_to` 的
/// Rust 代码仍运行在 previous 的内核栈上，不能提前覆盖 HART_CURRENT_*；
/// 否则窗口内 nested trap 会以 next 身份在 previous 栈上执行/记账，甚至让
/// reaper 提前回收 previous 栈。usertrapret 最后一刻调用 commit 发布。
static HART_STAGED_PID: PerHartUsize = PerHartUsize::new(0);
static HART_STAGED_PREVIOUS_PID: PerHartUsize = PerHartUsize::new(0);
static HART_STAGED_PCB: PerHartPtr<crate::sched::process::ProcessControlBlock> =
    PerHartPtr::new();

/// `switch_to` 在仍站于 previous 栈时只保留 next 预约，不发布 current。
/// 调用方持 PROCESS_TABLE 锁，PCB 地址稳定；同一 hart 在提交前不能再预约第二次。
pub fn stage_user_return(
    next_pid: usize,
    next_pcb: *mut crate::sched::process::ProcessControlBlock,
    previous_pid: usize,
) {
    assert_eq!(
        HART_STAGED_PID.load(Ordering::Acquire),
        0,
        "nested scheduler handoff on one hart",
    );
    HART_STAGED_PREVIOUS_PID.store(previous_pid, Ordering::Relaxed);
    HART_STAGED_PCB.store(next_pcb, Ordering::Relaxed);
    HART_STAGED_PID.store(next_pid, Ordering::Release);
}

#[inline(always)]
pub fn user_return_handoff_pending() -> bool {
    HART_STAGED_PID.load(Ordering::Acquire) != 0
}

/// 返回即将恢复到用户态的任务，不改变任何 per-hart 发布状态。
///
/// 调度切换后 Rust 仍运行在 previous 的内核栈上；return-to-user 的准备工作
/// （TLB/FPU/KSP/记账）必须用 staged next，但在最后一条汇编跳转前都不能把
/// HART_CURRENT_* 提前发布为 next。否则准备阶段自身一旦 nested fault，就会以
/// next 身份在 previous 栈上处理异常，重现跨任务内核栈踩踏。
#[inline(always)]
pub fn user_return_target(
) -> (usize, *mut crate::sched::process::ProcessControlBlock, bool) {
    let staged_pid = HART_STAGED_PID.load(Ordering::Acquire);
    if staged_pid != 0 {
        return (
            staged_pid,
            HART_STAGED_PCB.load(Ordering::Acquire),
            true,
        );
    }
    (
        HART_CURRENT_PID.load(Ordering::Acquire),
        HART_CURRENT_PCB.load(Ordering::Acquire),
        false,
    )
}

/// 物理栈切换后的唯一调度提交点。
///
/// 架构汇编先把 SP 移到目标 TrapContext 下方的目标内核栈，再调用本函数。因而从
/// 发布 next 开始，lock-free current、进程表投影和真实栈三者始终一致；previous
/// 也只会在本函数中解除 on_cpu/重新入队，等价 Linux `finish_task_switch`。
#[no_mangle]
pub extern "C" fn finish_staged_user_return(
    return_cx: *const crate::arch::TrapContext,
) {
    if return_cx.is_null() {
        return;
    }
    let sstatus = unsafe { (*return_cx).sstatus };
    #[cfg(target_arch = "riscv64")]
    let returning_user = sstatus & (1 << 8) == 0;
    #[cfg(target_arch = "loongarch64")]
    let returning_user = sstatus & 0x3 == 0x3;
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    let returning_user = false;
    if !returning_user {
        return;
    }

    let next_pid = HART_STAGED_PID.load(Ordering::Acquire);
    if next_pid == 0 {
        let current_pid = HART_CURRENT_PID.load(Ordering::Acquire);
        HART_KSTACK_OWNER.store(current_pid, Ordering::Release);
        return;
    }
    let previous_pid = HART_STAGED_PREVIOUS_PID.load(Ordering::Relaxed);
    let next_pcb = HART_STAGED_PCB.load(Ordering::Relaxed);
    assert!(!next_pcb.is_null(), "staged scheduler handoff lost next PCB");

    // SP 已在 next 栈上：先发布 lock-free 真相。此后即便 finalizer 自身异常，
    // nested trap 也会观察到 next PID + next PCB + next 栈，而非混合身份。
    HART_CURRENT_PCB.store(next_pcb, Ordering::Release);
    HART_CURRENT_PID.store(next_pid, Ordering::Release);
    HART_KSTACK_OWNER.store(next_pid, Ordering::Release);

    {
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        let table = guard
            .as_mut()
            .expect("process table unavailable during scheduler handoff");
        table.finish_staged_switch(previous_pid, next_pid);
    }

    HART_STAGED_PCB.store(core::ptr::null_mut(), Ordering::Relaxed);
    HART_STAGED_PREVIOUS_PID.store(0, Ordering::Relaxed);
    HART_STAGED_PID.store(0, Ordering::Release);

    // 信号投递可能停止/退出任务乃至再次调度，必须在 next 已成为 current 且
    // 物理运行于 next 栈后执行。旧路径在 previous 栈上投递 next 的信号，
    // 任何默认动作都会把 previous 当 current 操作。
    unsafe {
        crate::kernel::signal::check_and_deliver_signals(
            &mut *(return_cx as *mut crate::arch::TrapContext),
        );
    }
}

/// 取走本 hart 尚未提交的预约（返回 next pid，0 = 无）。
///
/// 只有 `ProcessTable::release_staged_switch` 该用它：预约已把 next 置
/// `Running + on_cpu=本 hart` 并摘出就绪环，单纯丢弃槽位会让它永远不被任何
/// hart 选中，也不会被任何唤醒救回。调用方必须把 next 归还就绪队列。
pub fn take_staged_user_return() -> usize {
    let pid = HART_STAGED_PID.load(Ordering::Acquire);
    if pid == 0 {
        return 0;
    }
    HART_STAGED_PCB.store(core::ptr::null_mut(), Ordering::Relaxed);
    HART_STAGED_PREVIOUS_PID.store(0, Ordering::Relaxed);
    HART_STAGED_PID.store(0, Ordering::Release);
    pid
}

/// HUNGDBG 取证用：读指定 hart 的 staged pid（0 = 无暂存任务）。
pub fn staged_pid_at(hart: usize) -> usize {
    HART_STAGED_PID.load_at(hart, Ordering::Relaxed)
}

pub fn staged_previous_pid_at(hart: usize) -> usize {
    HART_STAGED_PREVIOUS_PID.load_at(hart, Ordering::Relaxed)
}

/// 每 hart 一槽的索引。Linux 用 per-CPU `current`（RISC-V 内核把
/// `task_struct` 指针放在 `tp`）。此前这些"HART_*"影子状态是**单个**全局
/// 变量：注释写的是 per-hart，实现上所有 hart 共用一槽。SMP>1 时 hart B
/// 切换进程会覆盖 hart A 的当前 PCB/PID，于是 hart A 的 syscall 会用**别的
/// 进程的地址空间**翻译自己的用户指针，读写落到错误的物理页。
///
/// 观测证据：LA 8 核下 rustc 线程池共享的 hashbrown 控制字段被写乱，
/// 扩容时按乱值分配 `memory allocation of 1408749273104 bytes failed`
/// → SIGABRT；同一镜像同一命令在 SMP=1 下稳定通过
/// （`la-minibuild-20260810T175446Z.serial`：`MB_BUILD_RC=0`）。
#[inline(always)]
fn hart_slot() -> usize {
    let id = crate::arch::asm_helpers::read_cpu_id();
    if id < crate::sched::smp::MAX_CPUS {
        id
    } else {
        0
    }
}

/// per-hart `AtomicPtr`，接口与 `AtomicPtr` 一致，读写自动选中本 hart 的槽。
pub struct PerHartPtr<T>([core::sync::atomic::AtomicPtr<T>; crate::sched::smp::MAX_CPUS]);

impl<T> PerHartPtr<T> {
    pub const fn new() -> Self {
        Self(
            [const { core::sync::atomic::AtomicPtr::new(core::ptr::null_mut()) };
                crate::sched::smp::MAX_CPUS],
        )
    }

    #[inline(always)]
    pub fn load(&self, order: core::sync::atomic::Ordering) -> *mut T {
        self.0[hart_slot()].load(order)
    }

    #[inline(always)]
    pub fn store(&self, value: *mut T, order: core::sync::atomic::Ordering) {
        self.0[hart_slot()].store(value, order);
    }
}

/// per-hart `AtomicUsize`，接口与 `AtomicUsize` 一致。
pub struct PerHartUsize([core::sync::atomic::AtomicUsize; crate::sched::smp::MAX_CPUS]);

impl PerHartUsize {
    /// 初值恒为 0（与原 `AtomicUsize::new(0)` 一致）；参数保留以匹配调用写法。
    pub const fn new(_value: usize) -> Self {
        Self([const { core::sync::atomic::AtomicUsize::new(0) }; crate::sched::smp::MAX_CPUS])
    }

    #[inline(always)]
    pub fn load(&self, order: core::sync::atomic::Ordering) -> usize {
        self.0[hart_slot()].load(order)
    }

    #[inline(always)]
    pub fn store(&self, value: usize, order: core::sync::atomic::Ordering) {
        self.0[hart_slot()].store(value, order);
    }

    /// 读**指定** hart 的槽（跨核检查用，例如"这个 pid 的内核栈是否仍被某个
    /// hart 使用"）。越界返回 0。
    #[inline]
    pub fn load_at(&self, hart: usize, order: core::sync::atomic::Ordering) -> usize {
        if hart < crate::sched::smp::MAX_CPUS {
            self.0[hart].load(order)
        } else {
            0
        }
    }
}

/// 找第一个空闲 hart（`HART_CURRENT_PID == 0` = 已切到自己的 idle 栈、可立刻
/// 取任务），排除 `exclude`。只读原子、无锁；供 mark_runnable 的唤醒踢醒使用
/// （对标 Linux `select_idle_sibling`：任务入队即 IPI 空闲核，不等 100Hz
/// 定时器最坏 10ms 才把它唤醒）。
///
/// 在线核数走 `num_online_fast`（无锁原子镜像）——本函数在「已持
/// PROCESS_TABLE」的路径被调用，取 SMP_MANAGER 会与「AP 在 ap_online 持
/// SMP_MANAGER 时被 tick 中断、ISR 抢 PT」构成 ABBA 死锁（2026-08-15 血案）。
pub fn first_idle_hart_except(exclude: usize) -> Option<usize> {
    let mask = crate::sched::smp::online_mask_fast();
    for h in 0..crate::sched::smp::MAX_CPUS {
        if h == exclude || (mask & (1u64 << h)) == 0 {
            continue;
        }
        if HART_CURRENT_PID.load_at(h, core::sync::atomic::Ordering::Relaxed) == 0 {
            return Some(h);
        }
    }
    None
}

/// 在 `mask` 里找第一个空闲 hart（`HART_CURRENT_PID == 0`）。
/// 按硬件 hart 位扫描，不假设 hart 号是 0..online-1（BSP 可能是 4）。
pub fn first_idle_hart_in_mask(mask: u64) -> Option<usize> {
    let online = crate::sched::smp::online_mask_fast() & mask;
    for h in 0..crate::sched::smp::MAX_CPUS {
        if (online & (1u64 << h)) == 0 {
            continue;
        }
        if HART_CURRENT_PID.load_at(h, core::sync::atomic::Ordering::Relaxed) == 0 {
            return Some(h);
        }
    }
    None
}

/// 入队后踢醒一个空闲 hart（mark_runnable / insert_runnable / requeue /
/// SIGCONT / ptrace 恢复等所有"任务变可运行"路径的统一收口，对标 Linux
/// wake 路径的 select_idle_sibling 踢醒）。**调用方必须已持 PROCESS_TABLE**
/// （kick_hart 无锁，只写 IPI 寄存器/SBI）。
pub fn kick_idle_hart_if_any() {
    if let Some(hart) = first_idle_hart_except(crate::sched::process::current_hart_slot()) {
        kick_cpu(hart);
    }
}

/// 置目标 hart 的 NEED_RESCHED 并在跨核时发 reschedule IPI。
pub fn kick_cpu(cpu: usize) {
    if cpu >= crate::sched::smp::MAX_CPUS {
        return;
    }
    set_need_resched_on(cpu);
    if cpu != crate::sched::process::current_hart_slot() {
        crate::sched::smp::kick_hart(cpu);
    }
}

/// 每 hart 当前**正在其内核栈上执行**的进程 pid（0 = 本 hart 已在自己的 idle 栈上）。
///
/// 释放一个进程的内核栈之前必须确认**没有任何 hart** 还站在那张栈上。
/// 对标 Linux：任务的内核栈永远由 `finish_task_switch` 中的后继任务释放，
/// 绝不在自身仍在其上执行时释放。
///
/// 只看本 hart 的 sp 是不够的：SMP 下 hart A 回收进程 X 时，hart B 的 sp 可能正
/// 落在 X 的内核栈里。实测（FRAMEDBG 不变量拦截）：
/// `[STALE-MAP] alloc_frames_uninit ppn=0x90d04 map_count=20368 owner=Free
///  → freed-while-mapped DOUBLE-ALLOC root cause!`
/// 随后 `page_meta` 的 `ref_count` 被写成内核地址（`2152316936 = 0x80460008`），
/// 触发 `buddy handed out an occupied frame`。
pub static HART_KSTACK_OWNER: PerHartUsize = PerHartUsize::new(0);

/// 该 pid 的内核栈是否仍被**任意** hart 使用。
pub fn kstack_in_use_by_any_hart(pid: usize) -> bool {
    if pid == 0 {
        return false;
    }
    (0..crate::sched::smp::MAX_CPUS)
        .any(|h| HART_KSTACK_OWNER.load_at(h, Ordering::Relaxed) == pid)
}

/// per-hart `AtomicU64`，接口与 `AtomicU64` 一致。
pub struct PerHartU64([core::sync::atomic::AtomicU64; crate::sched::smp::MAX_CPUS]);

impl PerHartU64 {
    pub const fn new(_value: u64) -> Self {
        Self([const { core::sync::atomic::AtomicU64::new(0) }; crate::sched::smp::MAX_CPUS])
    }

    #[inline(always)]
    pub fn load(&self, order: core::sync::atomic::Ordering) -> u64 {
        self.0[hart_slot()].load(order)
    }

    #[inline(always)]
    pub fn store(&self, value: u64, order: core::sync::atomic::Ordering) {
        self.0[hart_slot()].store(value, order);
    }

    #[inline(always)]
    pub fn fetch_add(&self, value: u64, order: core::sync::atomic::Ordering) -> u64 {
        self.0[hart_slot()].fetch_add(value, order)
    }
}

/// 最近一个定时睡眠者的到期点（rdtime ticks，u64::MAX = 无）。供架构层在
/// `set_next_timer` 中把下一次定时器中断 arm 到 `min(now+周期tick, 此到期点)`，
/// 实现"对标 Linux hrtimer 的高精度到期"——睡眠线程仍挂起让出 CPU，但能在
/// deadline 精确被唤醒，而不必等满一个周期 tick（cyclictest 用 clock_nanosleep
/// ABSTIME 做 ~1ms 周期唤醒，旧实现要等到下一个 10ms tick 才评估 → 唤醒延迟
/// 最高一个 tick）。由 `suspend_current_for_nanosleep` 入睡时 fetch_min 收窄，
/// 由 `check_sleep_deadlines` 每次定时器中断重算（剔除已醒/过期者）。陈旧的过去
/// 值至多导致一次多余的立即中断，随即被重算修正，安全。
pub static NEAREST_DEADLINE: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(u64::MAX);

/// 读取最近到期点；架构层 `set_next_timer` 用它收窄下一次定时器中断时刻。
#[inline]
pub fn nearest_deadline() -> u64 {
    NEAREST_DEADLINE.load(Ordering::Relaxed)
}

/// 记录一个新的定时睡眠到期点（取更早者）。
#[inline]
pub fn note_sleep_deadline(deadline: u64) {
    NEAREST_DEADLINE.fetch_min(deadline, Ordering::Relaxed);
}

/// RT 定时睡眠者专用快查槽（单核单 RT sleeper 场景，cyclictest -t1）。
/// mark_sleeping 在 RT/DEADLINE 任务定时入睡时登记 (pid, deadline)；
/// check_sleep_deadlines 到期时走 O(log n) 定点唤醒快路径，避免 O(#processes)
/// 全表扫描出现在 RT 唤醒关键路径上（hackbench 400 任务时全扫 ~百µs 直接计入
/// cyclictest 延迟）。多个 RT sleeper 时仅最早者占槽，其余由全扫兜底服务。
pub static RT_SLEEP_PID: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
pub static RT_SLEEP_DL: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(u64::MAX);

#[inline]
pub fn note_rt_sleeper(pid: Pid, deadline: u64) {
    // 同 pid 重新入睡无条件覆盖（旧条目必已 stale）；不同 pid 取更早者。
    let cur_pid = RT_SLEEP_PID.load(Ordering::Relaxed);
    if cur_pid == pid || cur_pid == 0 || deadline < RT_SLEEP_DL.load(Ordering::Relaxed) {
        RT_SLEEP_PID.store(pid, Ordering::Relaxed);
        RT_SLEEP_DL.store(deadline, Ordering::Relaxed);
    }
}

/// 返回用户态时的"需重新调度"标志（对标 Linux `TIF_NEED_RESCHED`）。
///
/// 当一个 **RT/DEADLINE 任务在另一个任务处于内核态期间**被定时器唤醒时，那一刻
/// `interrupted_user == false`，trap 入口的定时器抢占被跳过；若不补救，被唤醒的高
/// 优先级任务要等到下一个周期 tick（~10ms）才被调度 → cyclictest 偶发 ~10ms Max
/// 尖峰的真因。置位本标志后，当前任务**返回用户态**这一抢占点会据此立即重新评估
/// 抢占（Linux 在 kernel→user 边界检查 need_resched 同理）。仅 RT/DEADLINE 唤醒置
/// 位，对非实时基准零扰动。
/// AtomicU32（非 AtomicBool）：RISC-V 无子字原子指令，AtomicBool 的 swap 会被
/// 编译成 ~10 条掩码 + amoand/amoor 序列；u32 宽度 swap 是单条 amoswap.w。
/// trap 返回用户态每次都要消费本标志，属最热路径。
pub static NEED_RESCHED: [core::sync::atomic::AtomicU32; crate::sched::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU32::new(0) }; crate::sched::smp::MAX_CPUS];

/// 每 hart 用户态 / 内核态 tick，供 `/proc/stat` 的 `cpuN` 行。
pub static HART_USER_TICKS: [core::sync::atomic::AtomicU64; crate::sched::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::sched::smp::MAX_CPUS];
pub static HART_SYS_TICKS: [core::sync::atomic::AtomicU64; crate::sched::smp::MAX_CPUS] =
    [const { core::sync::atomic::AtomicU64::new(0) }; crate::sched::smp::MAX_CPUS];
pub static HART_SWITCH_OK: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);
pub static HART_SWITCH_FAIL: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

#[inline]
fn need_resched_slot() -> usize {
    hart_slot()
}

#[inline]
pub fn set_need_resched() {
    NEED_RESCHED[need_resched_slot()].store(1, Ordering::Relaxed);
}

#[inline]
pub fn set_need_resched_on(hart: usize) {
    if hart < crate::sched::smp::MAX_CPUS {
        NEED_RESCHED[hart].store(1, Ordering::Relaxed);
    }
}

/// 读取并清除本 hart 的 need-resched 标志（返回用户态抢占点调用）。
/// 先普通读短路：未置位（绝大多数 trap）时免原子 RMW。
#[inline]
pub fn take_need_resched() -> bool {
    let slot = need_resched_slot();
    if NEED_RESCHED[slot].load(Ordering::Relaxed) == 0 {
        return false;
    }
    NEED_RESCHED[slot].swap(0, Ordering::Relaxed) != 0
}

/// 只读本 hart「需重新调度」标志（不消费）。供 `set_next_timer` 判定是否要把定时器收窄到
/// 极短延迟，让待决抢占点尽快到来。
#[inline]
pub fn need_resched_pending() -> bool {
    NEED_RESCHED[need_resched_slot()].load(Ordering::Relaxed) != 0
}

/// 是否曾有进程读取过 `/proc/[pid]/syscall`（或枚举 proc 任务目录）。
/// `last_syscall` 仅供 `/proc/[pid]/syscall` 与 HUNGDBG 观测使用；绝大多数
/// 进程（含全部 lmbench 微基准）从不读它。为此每次 syscall 入口/出口各加一把
/// PROCESS_TABLE 全局锁纯属浪费。仅当真有读者出现后才开启逐 syscall 记录，
/// 与 SECCOMP_EVER_ACTIVE / SCDBG 缓存同构（置位后不再清除，行为完全等价于
/// 始终记录——读者出现前的历史值无人观测）。
pub static SYSCALL_TRACK_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 标记 `/proc/[pid]/syscall` 观测已激活。由 procfs 在创建/读取该文件时调用。
/// 在 fd 创建（lookup/open）时调用即可保证随后那次 read syscall 的入口已被记录。
#[inline]
pub fn mark_syscall_track_active() {
    SYSCALL_TRACK_ACTIVE.store(true, Ordering::Relaxed);
}

/// 是否曾有进程读取过 `/proc/[pid]/io`。`io_rchar`/`io_wchar`/`io_syscr`/
/// `io_syscw`/`io_read_bytes`/`io_write_bytes` 仅供 `/proc/[pid]/io` 观测；
/// 绝大多数进程（含全部 lmbench / libcbench / iozone 微基准）从不读它，但旧
/// 实现每次 read/write/readv/pread/splice/sendfile 成功路径都为此抓一把
/// PROCESS_TABLE 全局锁 + 一次 BTreeMap 查找做记账，纯属浪费（§1.13①）。
/// 仅当真有读者出现后才开启逐 syscall 记账，与 SECCOMP_EVER_ACTIVE /
/// SYSCALL_TRACK_ACTIVE 同构：置位后不再清除，行为完全等价于始终记账——
/// 读者出现前的历史值无人观测；读者出现（读 /proc/[pid]/io）即激活，其后
/// 该进程所有 IO 均被记账，故基于「读 io → 干活 → 再读 io」求差的用例完全等价。
pub static IO_ACCOUNT_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// 标记 `/proc/[pid]/io` 观测已激活。由 procfs 在读取该文件时调用。
#[inline]
pub fn mark_io_account_active() {
    IO_ACCOUNT_ACTIVE.store(true, Ordering::Relaxed);
}

/// 当前有多少进程设置了 `yield_to_child`（clone child-runs-first / vfork /
/// 信号让位）。`handoff_child_for_trap` 在**每次** syscall 返回路径上被调用，
/// 但绝大多数 syscall 无 handoff 需求；旧实现每次都抓一把 PROCESS_TABLE 全局锁
/// + 一次 BTreeMap 查找仅为判定 `yield_to_child.is_none()`，纯属浪费（§1.13①）。
/// 用此计数 lock-free 短路：恒为 0（标记仅在 fork/vfork/信号让位的瞬间置上、
/// 下一次 trap 即被消费）。仅经 `set_yield_to_child`/`take_yield_to_child` 维护，
/// 计数偏大只会退回加锁判定（仍正确），偏小不可能（所有置位都 +1）。
pub static YIELD_HANDOFF_PENDING: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

pub fn stop_current_for_signal(sig: i32) -> ! {
    use crate::arch::usertrapret;

    #[cfg(target_arch = "riscv64")]
    const REG_A0: usize = 10;
    #[cfg(target_arch = "loongarch64")]
    const REG_A0: usize = 4;

    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("Process table not initialized");
    let current_pid = table.current.expect("No current process");
    if let Some(current) = table.get_mut(current_pid) {
        unsafe {
            if let Some(cx) = current.trap_context_mut() {
                cx.x[REG_A0] = 0;
            }
        }
    }
    table.stop_process_for_signal(current_pid, sig);
    let next_pid = table.select_next(current_pid, None);
    if let Some(next_pid) = next_pid {
        if !table.switch_to(next_pid, 0) {
            drop(guard);
            kernel_idle_until_runnable();
        }
        let next = table.get_mut(next_pid).unwrap();
        let user_satp = next.satp();
        let trap_cx_ptr =
            unsafe { next.trap_context_mut().unwrap() as *mut crate::arch::TrapContext };
        drop(guard);
        unsafe { usertrapret(&mut *trap_cx_ptr, user_satp); }
    } else {
        drop(guard);
        kernel_idle_until_runnable();
    }
    unreachable!();
}

fn switched_trap_context(table: &mut ProcessTable, next_pid: Pid) -> Option<(*mut crate::arch::TrapContext, usize, bool)> {
    let user_satp = table.get_mut(next_pid)?.satp();
    let (trap_cx_ptr, deliver_signal) = prepare_trap_context_for_user_return(table, next_pid)?;
    Some((trap_cx_ptr, user_satp, deliver_signal))
}

/// Linux 中部分 syscall 即使 handler 带 SA_RESTART 也永不自动重启
/// （内核返回 EINTR / ERESTART_RESTARTBLOCK 而非 ERESTARTSYS）：
/// 带超时语义的等待类调用重启会丢失已消耗的超时时间。
fn sa_restart_allowed(syscall_nr: usize) -> bool {
    !matches!(
        syscall_nr,
        22  // epoll_pwait
        | 72  // pselect6
        | 73  // ppoll
        | 98  // futex
        | 101 // nanosleep
        | 115 // clock_nanosleep
        | 137 // rt_sigtimedwait
        | 182 // mq_timedsend
        | 183 // mq_timedreceive
        | 192 // semtimedop
        | 243 // recvmmsg
        | 441 // epoll_pwait2
    )
}

pub(super) fn prepare_trap_context_for_user_return(
    table: &mut ProcessTable,
    pid: Pid,
) -> Option<(*mut crate::arch::TrapContext, bool)> {
    const SA_RESTART: usize = 0x10000000;
    // Linux signal_pending() 语义：被忽略（SIG_IGN / 默认忽略的 SIG_DFL）的
    // pending 信号不会打断阻塞中的系统调用。handler 表取自 tgid leader。
    let deliverable = crate::kernel::signal::next_deliverable_signal_action(table, pid, 0);
    let process = table.get_mut(pid)?;
    let suppress_stale_io_restore = process.wait_channel == Some("do_wait")
        || process.wait_trap_context.is_some()
        || process.wait_exit_code_ptr != 0
        || process.wait_siginfo_ptr != 0
        || process.wait_is_waitid;
    if suppress_stale_io_restore {
        process.io_wait_trap_context.clear();
    }
    if process.io_wait_trap_context.is_some() {
        if process.trap_context_ptr == 0 {
            return None;
        }
        let trap_cx_raw = process.trap_context_ptr as *mut crate::arch::TrapContext;
        // ctx 从未离开 kstack 原位，消费重放标记即可（零拷贝）。
        process.io_wait_trap_context.take();
        let trap_cx = unsafe { &mut *trap_cx_raw };
        #[cfg(target_arch = "riscv64")]
        let syscall_nr = trap_cx.x[17];
        #[cfg(target_arch = "loongarch64")]
        let syscall_nr = trap_cx.x[11];
        if let Some((_, action)) = deliverable {
            // Linux：用户 handler 带 SA_RESTART 时，可重启的阻塞 syscall
            // （如 read）在 handler 返回后自动重启 —— sepc 停在 ecall 上、
            // 寄存器保持入参原值；否则按 -EINTR 返回。
            // poll/select/epoll/nanosleep 等带超时语义的 syscall 在 Linux
            // 中即使 SA_RESTART 也永不重启，只能返回 EINTR。
            let restart = action.handler > 1
                && action.flags & SA_RESTART != 0
                && sa_restart_allowed(syscall_nr);
            if !restart {
                trap_cx.sepc = trap_cx.sepc.wrapping_add(4);
                #[cfg(target_arch = "riscv64")]
                { trap_cx.x[10] = (-4isize) as usize; }
                #[cfg(target_arch = "loongarch64")]
                { trap_cx.x[4] = (-4isize) as usize; }
            }
            return Some((trap_cx as *mut crate::arch::TrapContext, true));
        }
        wtrace_trap("prepare.restore_io_wait.after", pid, trap_cx);
        return Some((trap_cx as *mut crate::arch::TrapContext, false));
    }
    // do_wait（wait4/waitpid/wait3/waitid）阻塞被信号中断：与 io_wait 同语义 ——
    // handler 带 SA_RESTART 且该 syscall 允许重启时重新执行 ecall，否则返回 -EINTR。
    // 若有匹配子进程退出，exit() 收口路径会先 `restore_wait_trap_context`（消费
    // wait_trap_context，写 a0=child pid）再调本函数，此时 wait_trap_context 已为
    // None，故纯信号中断（无子进程退出）才进此分支。不修复时会把阻塞前残留的 a0
    // （即 wait4 的 pid 入参，如 -1）当返回值带回用户态，被 musl 解读为 errno=EPERM。
    // 判据不能只看 wait_channel：唤醒方若走 mark_runnable(pid, true)（如 kill/
    // exit 的普通唤醒），clear_wait_state 已把 wait_channel 清成 None，但
    // wait_trap_context 仍在 —— 该情形同样是"阻塞 wait 被信号打断"，必须走本
    // 分支恢复保存的上下文并写 -EINTR/重启，否则投递 handler 时保存的是残留
    // trap context（a0 = -1 入参）→ sigreturn 后 waitpid 返回 -EPERM。
    if process.wait_channel == Some("do_wait") || process.wait_trap_context.is_some() {
        // 无可投递信号的"伪唤醒"（如 SIGCHLD 处于 SIG_DFL 忽略态、exit 普通唤醒
        // 未收口 waitpid）：不能消费保存的上下文后带脏 a0 回用户态，也不能白白
        // 丢弃它 —— 回退到 ecall 重放 wait，让 do_wait 重新检查子进程状态。
        if deliverable.is_none() {
            if let Some(saved) = process.wait_trap_context.take() {
                process.clear_wait_state();
                let trap_cx = unsafe { process.trap_context_mut()? };
                *trap_cx = saved;
                trap_cx.sepc = trap_cx.sepc.wrapping_sub(4);
                wtrace_trap("prepare.wait_replay", pid, trap_cx);
                return Some((trap_cx as *mut crate::arch::TrapContext, false));
            }
        }
        if let (Some((_, action)), Some(saved)) =
            (deliverable, process.wait_trap_context.take())
        {
            // wait_trap_context 的 sepc 指向 ecall 之后（dispatcher 已 +4）。
            #[cfg(target_arch = "riscv64")]
            let syscall_nr = saved.x[17];
            #[cfg(target_arch = "loongarch64")]
            let syscall_nr = saved.x[11];
            let restart = action.handler > 1
                && action.flags & SA_RESTART != 0
                && sa_restart_allowed(syscall_nr);
            process.clear_wait_state();
            let trap_cx = unsafe { process.trap_context_mut()? };
            *trap_cx = saved;
            if restart {
                // 回退到 ecall，handler 返回后重新发起 wait
                trap_cx.sepc = trap_cx.sepc.wrapping_sub(4);
            } else {
                #[cfg(target_arch = "riscv64")]
                { trap_cx.x[10] = (-4isize) as usize; }
                #[cfg(target_arch = "loongarch64")]
                { trap_cx.x[4] = (-4isize) as usize; }
            }
            wtrace_trap("prepare.return", pid, trap_cx);
            return Some((trap_cx as *mut crate::arch::TrapContext, true));
        }
    }
    // Option<TrapContext> 按值 take 会无条件搬运 304B 载荷（None 也搬）；
    // 先 is_some 守门，让拷贝只发生在真有保存上下文的罕见路径。
    let saved = if process.signal_wait_trap_context.is_some() {
        process.signal_wait_trap_context.take()
    } else {
        None
    };
    // A deadline wake in check_sleep_deadlines keeps wakeup_channel but clears
    // wait_channel, so re-entry must also be recognized by the SIGNAL_WAIT
    // sentinel + preserved deadline; otherwise clear_wait_state below would
    // wipe the deadline and the replayed rt_sigtimedwait would restart its
    // full timeout forever instead of returning EAGAIN.
    let reenter_signal_wait = process.wait_channel == Some("do_sigtimedwait")
        || (saved.is_some()
            && process.waiting_for == Some(SIGNAL_WAIT_SENTINEL)
            && process.wakeup_channel > 1);
    let reenter_nanosleep = process.waiting_for == Some(NANOSLEEP_SENTINEL);
    let nanosleep_saved = if reenter_nanosleep {
        process.nanosleep_trap_context.take()
    } else {
        None
    };
    let has_deliverable_signal = deliverable.is_some();
    // Linux: signals interrupt nanosleep → handler fires, nanosleep returns -EINTR
    let signal_interrupts_nanosleep = has_deliverable_signal && reenter_nanosleep;
    let deliver_signal =
        has_deliverable_signal && (signal_interrupts_nanosleep || (!reenter_nanosleep && (saved.is_none() || !reenter_signal_wait)));
    if saved.is_some() && !reenter_signal_wait {
        process.clear_wait_state();
    }
    if signal_interrupts_nanosleep {
        // Write remaining time to user rem pointer before clearing state
        let rem_ptr = process.nanosleep_rem_ptr;
        let deadline = process.wakeup_channel as u64;
        if rem_ptr != 0 {
            let now = crate::arch::asm_helpers::rdtime();
            let remaining_ticks = deadline.saturating_sub(now);
            let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
            let rem_sec = (remaining_ticks / clock_freq) as i64;
            let rem_nsec = ((remaining_ticks % clock_freq) * 1_000_000_000 / clock_freq) as i64;
            if let Some(ref mm) = process.mm {
                let mut addr_space = mm.lock();
                let ptr = rem_ptr;
                let buf_sec = rem_sec.to_le_bytes();
                let buf_nsec = rem_nsec.to_le_bytes();
                let vpn0 = crate::mm::VirtAddr(ptr).floor();
                let off0 = crate::mm::VirtAddr(ptr).page_offset();
                if let Some(f) = addr_space.page_table().get_pte_flags(vpn0) {
                    if !f.contains(crate::mm::PTEFlags::W) {
                        addr_space.handle_page_fault(crate::mm::VirtAddr(ptr), true);
                    }
                }
                if let Some(ppn) = addr_space.page_table().translate(vpn0) {
                    let kva = crate::mm::phys_to_kernel_vaddr(ppn.0 * 4096 + off0);
                    unsafe {
                        core::ptr::copy_nonoverlapping(buf_sec.as_ptr(), kva as *mut u8, 8);
                        core::ptr::copy_nonoverlapping(buf_nsec.as_ptr(), (kva + 8) as *mut u8, 8);
                    }
                }
            }
        }
        process.waiting_for = None;
    }
    let had_saved = saved.is_some();
    let trap_cx = unsafe { process.trap_context_mut()? };
    if let Some(saved) = saved {
        wtrace_trap("prepare.restore_saved.before", pid, &saved);
        *trap_cx = saved;
    } else if let Some(saved) = nanosleep_saved {
        wtrace_trap("prepare.restore_nanosleep.before", pid, &saved);
        *trap_cx = saved;
    }
    if (had_saved || signal_interrupts_nanosleep) && deliver_signal {
        trap_cx.sepc += 4;
        trap_cx.set_return_value((-4isize) as usize); // -EINTR
    }
    wtrace_trap("prepare.return", pid, trap_cx);
    Some((trap_cx as *mut crate::arch::TrapContext, deliver_signal))
}

/// [DEBUG 插桩] 在向 PCB 的 TrapContext 槽（位于内核栈顶）写入前，校验该物理页
/// MetaSlot owner 仍为 KernelStack。若已变（User/Free/Slab/...），说明此内核栈帧
/// 已被释放并改派他用，而我们仍把整组寄存器写进去 = 把任务上下文写进用户页的
/// 双重分配铁证（iperf3 text page 被 socket 调用链寄存器覆盖）。
#[inline]
fn dbg_check_trapctx_owner(ptr: usize, pid: Pid, site: &str) {
    #[cfg(target_arch = "riscv64")]
    let pa = ptr;
    #[cfg(target_arch = "loongarch64")]
    let pa = ptr & 0x0000_FFFF_FFFF_FFFF;
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    let pa = ptr;
    let ppn = crate::mm::PhysPageNum(pa >> 12);
    if let Some(slot) = crate::mm::page_meta::try_meta_of(ppn) {
        let owner = slot.get_owner();
        if owner != crate::mm::page_meta::FrameOwner::KernelStack {
            crate::println!(
                "[TRAPCTX-STALE] site={} pid={} ptr={:#x} ppn={:#x} owner={:?} rc={} \
                 — writing TrapContext into a non-KStack frame! ROOT CAUSE",
                site, pid, ptr, ppn.0, owner, slot.ref_count(),
            );
        }
    }
}

fn save_current_trap_context(
    table: &mut ProcessTable,
    current_pid: Pid,
    current_cx: &crate::arch::TrapContext,
) -> bool {
    let current = match table.get_mut(current_pid) {
        Some(process) => process,
        None => return false,
    };
    unsafe {
        if let Some(saved_cx) = current.trap_context_mut() {
            dbg_check_trapctx_owner(saved_cx as *mut _ as usize, current_pid, "save_current");
            if wtrace_save_enabled() {
                wtrace_trap("save_current.input", current_pid, current_cx);
            }
            // current_cx 常就是 kstack 顶原位 ctx，自拷贝纯属浪费。
            if saved_cx as *const _ != current_cx as *const crate::arch::TrapContext {
                *saved_cx = *current_cx;
            }
            if wtrace_save_enabled() {
                wtrace_trap("save_current.stored", current_pid, saved_cx);
            }
            return true;
        }
    }
    false
}

pub fn preempt_current_for_trap(
    current_cx: &crate::arch::TrapContext,
    prefer_current_tgid: bool,
    reason: u64,
) -> Option<(*mut crate::arch::TrapContext, usize, bool)> {
    // 定时器抢占的零锁前置：rq 无锁镜像显示没有其他可运行任务时，本轮
    // 没有任何可抢占对象，不必为 timer_preemption_due 抢 PROCESS_TABLE。
    // 空闲 hart 的周期 tick 由此完全无表锁。
    if reason == crate::sched::trace::SWITCH_TIMER
        && !crate::sched::cpu_rq::any_queued()
    {
        // Deadline throttle may mark current Sleeping before this timer return.
        let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
        if pcb_ptr.is_null() || unsafe { (*pcb_ptr).state == ProcessState::Running } {
            return None;
        }
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut()?;
    let current_pid = table.current?;
    if reason == crate::sched::trace::SWITCH_TIMER
        && !timer_preemption_due(table, current_pid)
    {
        if cldbg_timer_skip_permitted() {
            let state = table.get(current_pid).map(|p| p.state);
            let tgid = table.get(current_pid).map(|p| p.tgid).unwrap_or(0);
            let other = table.has_other_runnable(current_pid);
            let last_in = table.get(current_pid).map(|p| p.sched_slice_start_tick).unwrap_or(0);
            let now = crate::arch::asm_helpers::rdtime();
            crate::println!(
                "[cldbg] timer.skip current={} tgid={} state={:?} other={} last_in={} now={} delta={}",
                current_pid,
                tgid,
                state,
                other,
                last_in,
                now,
                now.saturating_sub(last_in)
            );
        }
        return None;
    }
    if !save_current_trap_context(table, current_pid, current_cx) {
        return None;
    }
    let preferred_tgid = if prefer_current_tgid {
        table.get(current_pid).map(|p| p.tgid)
    } else {
        None
    };
    let Some(next_pid) = table.select_next(current_pid, preferred_tgid) else {
        // Current is no longer Running and there is no successor: go idle instead
        // of returning to user mode with a Sleeping PCB.
        let current_must_leave = table
            .get(current_pid)
            .map_or(true, |p| p.state != ProcessState::Running);
        if current_must_leave {
            drop(guard);
            kernel_idle_until_runnable();
        }
        return None;
    };
    if cldbg_enabled() {
        crate::println!(
            "[cldbg] preempt current={} next={} reason={} preferred_tgid={:?}",
            current_pid,
            next_pid,
            reason,
            preferred_tgid
        );
    }
    if !table.preempt_to(next_pid, reason) {
        return None;
    }
    switched_trap_context(table, next_pid)
}

fn timer_preemption_due(table: &ProcessTable, current_pid: Pid) -> bool {
    let Some(current) = table.get(current_pid) else {
        return true;
    };
    // Deadline throttle: non-Running current must leave even with no other task.
    if current.state != ProcessState::Running {
        return true;
    }
    if !table.has_other_runnable(current_pid) {
        return false;
    }
    if current.waiting_for == Some(NANOSLEEP_SENTINEL)
        && crate::kernel::signal::pending_interrupting_signal_for_process(table, current_pid, 0)
    {
        return false;
    }
    // RT/DEADLINE 任务一旦就绪，应立即抢占低类别的当前任务，不等其 CFS 时间片
    // 耗尽（对标 Linux check_preempt_curr）。否则在 hackbench 等重负载 SCHED_OTHER
    // 风暴下，p99 实时任务（cyclictest）会被饿死、永不被调度。
    if table.has_higher_priority_runnable(current_pid) {
        return true;
    }
    let last_in = current.sched_slice_start_tick;
    if last_in == 0 {
        return true;
    }
    // SCHED_RR：默认 100ms 量子按墙钟判定。到期后由 preempt_to 把当前任务
    // 标 Runnable 并 enqueue，RT bitmap 同级 FIFO 自然把它放到队尾。
    if current.sched_policy == 2 {
        let interval = (crate::arch::consts::CLOCK_FREQ as u64 / 100)
            .saturating_mul(crate::sched::process::RR_TIMESLICE_TICKS as u64)
            .max(1);
        let now = crate::arch::asm_helpers::rdtime();
        return now.saturating_sub(last_in) >= interval;
    }
    // CFS-aware time slice: scale by nice weight.
    // Base = DEFAULT_TIME_SLICE (10 ticks = 100ms at HZ=100).
    // Adjusted = base * weight / NICE_0_WEIGHT, clamped to [1, 40] ticks.
    // nice -20 (weight 88761) → ~867 ticks (~8.67s) — clamped to 40 (400ms)
    // nice  0  (weight 1024)  → 10 ticks (100ms)
    // nice 19  (weight 15)    → ~0.15 → clamped to 1 tick (10ms)
    let base_slice = crate::sched::DEFAULT_TIME_SLICE as u64;
    let weight = current.sched_weight.max(1) as u64;
    let nice_0 = crate::sched::process::NICE_0_WEIGHT as u64;
    let adjusted = (base_slice * weight / nice_0).clamp(1, 40);
    let tick_interval = (crate::arch::consts::CLOCK_FREQ as u64 / 100).max(1);
    let slice_ticks = tick_interval.saturating_mul(adjusted).max(1);
    let now = crate::arch::asm_helpers::rdtime();
    now.saturating_sub(last_in) >= slice_ticks
}

pub fn switch_current_to_next(
    current_cx: &crate::arch::TrapContext,
    reason: u64,
) -> Option<(*mut crate::arch::TrapContext, usize, bool)> {
    // 软 tick 检查点（cond_resched 风格）：内核内切换链（阻塞 syscall → 哨兵 →
    // 此处选 next）可长时间不经过 trap 入口的软 tick；hackbench 数百任务链式
    // 切换期间定时睡眠者（cyclictest 的 nanosleep deadline）到期得不到服务，
    // 唤醒迟到数 ms。在拿表锁选 next 之前服务到期者，使刚被唤醒的 RT 任务
    // 直接被本次 select_next 选中（同时消 wake-late 和 run-late）。
    {
        let now = crate::arch::asm_helpers::rdtime();
        if now >= nearest_deadline() {
            check_sleep_deadlines();
        }
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut()?;
    let current_pid = table.current?;
    if !save_current_trap_context(table, current_pid, current_cx) {
        return None;
    }
    let next_pid = table.select_next(current_pid, None)?;
    if !table.preempt_to(next_pid, reason) {
        return None;
    }
    switched_trap_context(table, next_pid)
}

pub fn complete_thread_startup_syscall(syscall_id: usize) -> bool {
    matches!(
        syscall_id,
        crate::syscall::nr::SYS_SET_ROBUST_LIST
            | crate::syscall::nr::SYS_RT_SIGPROCMASK
            | crate::syscall::nr::SYS_SET_TID_ADDRESS
    )
}

#[inline(always)]
pub fn switch_thread_startup_to_leader(
    current_cx: &crate::arch::TrapContext,
    syscall_id: usize,
    reason: u64,
) -> Option<(*mut crate::arch::TrapContext, usize, bool)> {
    // 快路径：绝大多数 syscall 不是线程启动尾声三件套，仅一次立即数比较。
    if !complete_thread_startup_syscall(syscall_id) {
        return None;
    }
    switch_thread_startup_to_leader_slow(current_cx, reason)
}

#[cold]
#[inline(never)]
fn switch_thread_startup_to_leader_slow(
    current_cx: &crate::arch::TrapContext,
    reason: u64,
) -> Option<(*mut crate::arch::TrapContext, usize, bool)> {
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut()?;
    let current_pid = table.current?;
    let current_tgid = table.get(current_pid).map(|p| p.tgid).unwrap_or(current_pid);
    if current_pid == current_tgid {
        return None;
    }
    if !save_current_trap_context(table, current_pid, current_cx) {
        return None;
    }
    let leader_pid = current_tgid;
    let leader_ready = table.get(leader_pid).map_or(false, |leader| {
        leader.state == ProcessState::Runnable && leader.tgid == current_tgid
    });
    if !leader_ready {
        return None;
    }
    if !table.preempt_to(leader_pid, reason) {
        return None;
    }
    switched_trap_context(table, leader_pid)
}

#[inline(always)]
pub fn handoff_child_for_trap(
    current_cx: &crate::arch::TrapContext,
    reason: u64,
) -> Option<(*mut crate::arch::TrapContext, usize, bool)> {
    // Lock-free 短路：无任何进程挂起 yield_to_child（绝大多数 syscall）时，
    // 不抓全局锁、不查 BTreeMap 直接返回。inline(always) + #[cold] 慢路径
    // 使快路径在调用点仅剩一次原子读+分支（免函数序/尾言保存恢复 s 寄存器）。
    if YIELD_HANDOFF_PENDING.load(Ordering::Relaxed) == 0 {
        return None;
    }
    handoff_child_for_trap_slow(current_cx, reason)
}

#[cold]
#[inline(never)]
fn handoff_child_for_trap_slow(
    current_cx: &crate::arch::TrapContext,
    reason: u64,
) -> Option<(*mut crate::arch::TrapContext, usize, bool)> {
    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut()?;
    let current_pid = table.current?;
    // Fast path: only proceed if yield_to_child is set
    let has_yield = table.get(current_pid).map_or(false, |p| p.yield_to_child.is_some());
    if !has_yield {
        return None;
    }
    if !save_current_trap_context(table, current_pid, current_cx) {
        return None;
    }
    let current_tgid = table.get(current_pid).map(|p| p.tgid).unwrap_or(current_pid);
    let child_pid = table.get_mut(current_pid).and_then(|p| p.take_yield_to_child())?;
    let child_ready = table.get(child_pid).map_or(false, |child| {
        child.state == ProcessState::Runnable
            && (child.tgid == current_tgid || child.parent_pid == Some(current_pid))
    });
    let signal_wakeup_ready = table.get(child_pid).map_or(false, |target| {
        target.state == ProcessState::Runnable
            && (target.pending_signals & !target.signal_mask) != 0
    });
    if cldbg_enabled() {
        let child_state = table.get(child_pid).map(|child| child.state);
        crate::println!(
            "[cldbg] handoff current={} child={} child_state={:?} child_ready={} signal_ready={} current_tgid={}",
            current_pid,
            child_pid,
            child_state,
            child_ready,
            signal_wakeup_ready,
            current_tgid
        );
    }
    let preempted = (child_ready || signal_wakeup_ready) && table.preempt_to(child_pid, reason);
    if !preempted {
        return None;
    }
    switched_trap_context(table, child_pid)
}

/// 初始化进程管理
pub fn init() {
    crate::sched::process::reset_process_counters();
    let mut table = PROCESS_TABLE.lock();
    *table = Some(ProcessTable::new());
    crate::println!("[process] Process management initialized");
}



pub fn try_process_stats_snapshot() -> Option<(usize, usize, usize, usize, usize)> {
    let guard = PROCESS_TABLE.try_lock()?;
    let table = guard.as_ref()?;
    let mut zombies = 0usize;
    let mut threads = 0usize;
    let mut child_refs = 0usize;
    for process in table.processes.values() {
        if process.state == ProcessState::Zombie {
            zombies += 1;
        }
        if process.is_thread {
            threads += 1;
        }
        child_refs += process.children.len();
    }
    Some((
        table.processes.len(),
        zombies,
        threads,
        child_refs,
        table.current.unwrap_or(0),
    ))
}

// Slice 2.0：移除原 `pub fn exec(path, args)` 的 stub —— 它从未真正
// load ELF / 重置 mm / 设置 sp+entry，唯一调用点是 sched/mod.rs 的
// self-test。真实 exec 走 syscall 层 `sys_execve` →
// `crate::syscall::exec::do_execve_inner`，由该路径完成 ELF 解析、
// 地址空间替换、栈/argv/envp/auxv 装配、信号 disposition 重置。
// 留 stub 反而会让"是否进入真路径"出现两条不一致语义，违反 Phase 8
// "syscall = ABI glue / 真实业务下沉" 的架构线，故删除。
//
// 如果未来需要内核内部的 exec 调用（如 init 启动 shell），请新建一个
// 走 `do_execve_inner` 的薄包装，而不是恢复这里的 stub。

/// Exit - 进程退出
///
/// 终止当前进程，释放资源，通知父进程。
///
/// # 资源释放
/// - 释放进程地址空间（包括所有物理页）
/// - 关闭所有文件描述符
/// - 将子进程托管给 init 进程
///
/// # 实现细节
/// 挂起当前进程，切换到下一个可运行进程
///
/// 将当前进程标记为 Sleeping，sepc 回退到 ecall 指令，
/// 找到下一个 Runnable 进程并通过 usertrapret 切换。
/// 当前进程被唤醒后会重新执行 ecall → 重新进入 syscall。
///
/// 该函数永不返回。
pub fn suspend_current_and_run_next() -> ! {
    suspend_current_for_io_inner(0)
}

/// 真实"用户态 → 内核态"分账：把 `cpu_last_switched_in_tick..now` 之间的
/// 占用算作 user 时间，更新 `cpu_last_switched_in_tick = now` 并标 `cpu_in_kernel=true`。
/// `now` 由调用方一次 `rdtime` 提供，避免 helper 内重复读时钟与 caller 漂移。
///
/// 与 Linux `account_user_time` / `account_system_time` 的 per-task 累计语义
/// 一致：不依赖固定 tick 中断节拍，而是按真实 CPU 周期差累计。
pub fn account_enter_kernel(now: u64) {
    // Lock-free fast path via HART_CURRENT_PCB (see its safety contract):
    // trap handling runs with interrupts disabled on the only hart.
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        account_enter_kernel_inner(unsafe { &mut *pcb_ptr }, now);
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        account_enter_kernel_inner(p, now);
    }
}

#[inline(always)]
fn account_enter_kernel_inner(p: &mut ProcessControlBlock, now: u64) {
    {
        let last_in = p.cpu_last_switched_in_tick;
        if last_in != 0 && now > last_in {
            let delta = now - last_in;
            // 此前一段时间 p 在跑：若 cpu_in_kernel=true 应该早已被 exit_kernel
            // 重置过 last_switched_in_tick；这里走到说明 p 是从用户态进入内核。
            if p.cpu_in_kernel {
                p.cpu_kernel_ticks = p.cpu_kernel_ticks.saturating_add(delta);
            } else {
                p.cpu_user_ticks = p.cpu_user_ticks.saturating_add(delta);
            }
        }
        p.cpu_last_switched_in_tick = now;
        p.cpu_in_kernel = true;

        // RLIMIT_CPU 强制：account_enter_kernel 在每次陷入(含定时器中断)时都会
        // 跑, 是用户态忙等场景下唯一可靠的逐 tick 结算点(account_exit_kernel
        // 在纯忙等的定时器中断返回路径上不一定经过)。达软限投递 SIGXCPU 并逐秒
        // 重发, 达硬限投递 SIGKILL。对标 Linux run_posix_cpu_timers。
        enforce_rlimit_cpu(p, now);

        // Update deliverable-signal cache so check_and_deliver_signals
        // fast-path works for timer/fault traps too (not only syscalls).
        HART_PENDING_SIGNALS.store(
            p.pending_signals & !p.signal_mask,
            Ordering::Relaxed,
        );
    }
}

/// RLIMIT_CPU（idx 0, 单位秒）强制: 进程 CPU 时间(用户+内核)达软限投递
/// SIGXCPU(24) 且之后每秒重发一次, 达硬限投递 SIGKILL(9)。LTP setrlimit06
/// 据此校验 cur=1s→SIGXCPU、max=2s→SIGKILL 的次序。
///
/// `now` = 当前 rdtime。CPU 用量取「已结算的 user+kernel」加上「自上次切入起
/// 仍在运行的在途段 (now - cpu_last_switched_in_tick)」，这样即使在某架构(如
/// LA)上纯用户态忙等期间 account_enter/exit_kernel 没有被逐 tick 调用、ticks
/// 暂未结算, 也能基于在途段实时判定, 保证两架构一致。
fn enforce_rlimit_cpu(p: &mut crate::sched::process::ProcessControlBlock, now: u64) {
    let (soft_cpu, hard_cpu) = p.rlimits[0];
    if soft_cpu == u64::MAX && hard_cpu == u64::MAX {
        return;
    }
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    if freq == 0 {
        return;
    }
    let mut cpu_total = p.cpu_user_ticks.saturating_add(p.cpu_kernel_ticks);
    if p.cpu_last_switched_in_tick != 0 && now > p.cpu_last_switched_in_tick {
        cpu_total = cpu_total.saturating_add(now - p.cpu_last_switched_in_tick);
    }
    let cpu_sec = cpu_total / freq;
    if hard_cpu != u64::MAX && cpu_sec >= hard_cpu {
        p.pending_signals |= 1u64 << (9 - 1); crate::sched::process::note_signal_hint(); // SIGKILL
    } else if soft_cpu != u64::MAX && cpu_sec >= soft_cpu && cpu_sec > p.rlimit_cpu_xcpu_sec {
        p.pending_signals |= 1u64 << (24 - 1); crate::sched::process::note_signal_hint(); // SIGXCPU
        p.rlimit_cpu_xcpu_sec = cpu_sec;
    }
}

/// 由 `timer_tick` 每个时钟节拍调用: 对当前进程做 RLIMIT_CPU 实时强制。
/// 是否曾有进程设置过有限的 RLIMIT_CPU。默认所有进程都是 RLIM_INFINITY，
/// 未置位时周期 tick 完全不抢 PROCESS_TABLE；一旦 setrlimit 写入有限值则
/// 永久置位，语义等价于始终 tick。
pub static RLIMIT_CPU_EVER_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

pub fn rlimit_cpu_tick() {
    if !RLIMIT_CPU_EVER_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let now = crate::arch::asm_helpers::rdtime();
    // Current-task tick path: use the per-hart PCB shadow, no central table lock.
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        enforce_rlimit_cpu(unsafe { &mut *pcb_ptr }, now);
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        enforce_rlimit_cpu(p, now);
    }
}

/// Slice 2.5 真实化：写当前进程的 PCB.last_syscall。
///
/// `/proc/[pid]/syscall` Linux 在 `fs/proc/base.c::proc_pid_syscall` 里读
/// `task->syscall_dispatch` 状态：`>=0` 时表示当前正在执行的 syscall 号，
/// `running` 时表示当前在用户态。我们用 PCB 上 `(nr, [a0..a5])` 记录，
/// 入口 `set_current_syscall_enter(nr, args)`，出口 `set_current_syscall_exit()`
/// （nr = u64::MAX，args 全 0，表示 running）。
pub fn set_current_syscall_enter(nr: u64, args: [u64; 6]) {
    // 无 /proc/[pid]/syscall 读者时（lmbench 等全部场景）跳过，省一把全局锁。
    if !SYSCALL_TRACK_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        unsafe { (*pcb_ptr).last_syscall = (nr, args) };
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        p.last_syscall = (nr, args);
    }
}

/// 与 `set_current_syscall_enter` 配对：在 dispatcher 出口清空到 "running"。
pub fn set_current_syscall_exit() {
    if !SYSCALL_TRACK_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        unsafe { (*pcb_ptr).last_syscall = (u64::MAX, [0; 6]) };
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        p.last_syscall = (u64::MAX, [0; 6]);
    }
}

/// 真实"内核态 → 用户态"分账：把 `cpu_last_switched_in_tick..now` 之间的
/// 占用算作 kernel 时间，更新 `cpu_last_switched_in_tick = now` 并标
/// `cpu_in_kernel=false`。
///
/// Slice 2.x C13 真化：在 cpu_*_ticks 累计后立即检查 ITIMER_VIRTUAL / PROF
/// 是否到期；到期投递 SIGVTALRM(26) / SIGPROF(27)，若 interval 非 0 则
/// 重设下一段。与 Linux `account_user_time` / `account_system_time` 在
/// `task_tick_fair` 路径里调 `run_posix_cpu_timers` 等价（差异：Linux 是
/// posix-cpu-timers 子系统，我们直接 inline 检查 itimer 字段）。
pub fn account_exit_kernel(now: u64) {
    // Lock-free fast path via HART_CURRENT_PCB (see its safety contract).
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &mut *pcb_ptr };
        account_exit_kernel_inner(p, now);
        HART_PENDING_SIGNALS.store(
            p.pending_signals & !p.signal_mask,
            Ordering::Relaxed,
        );
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        account_exit_kernel_inner(p, now);
        HART_PENDING_SIGNALS.store(
            p.pending_signals & !p.signal_mask,
            Ordering::Relaxed,
        );
    }
}

/// 对尚未发布到 HART_CURRENT_PCB 的 staged next 做 kernel→user 记账。
///
/// 仅供 return-to-user 准备路径使用；PCB 由 PROCESS_TABLE 中的 Box 提供稳定地址。
#[inline(always)]
pub fn account_exit_kernel_for(
    pcb_ptr: *mut crate::sched::process::ProcessControlBlock,
    now: u64,
) {
    if pcb_ptr.is_null() {
        return;
    }
    let p = unsafe { &mut *pcb_ptr };
    account_exit_kernel_inner(p, now);
    HART_PENDING_SIGNALS.store(
        p.pending_signals & !p.signal_mask,
        Ordering::Relaxed,
    );
}

/// Shared inner logic for kernel→user accounting on a PCB already obtained
/// under the PROCESS_TABLE lock.
#[inline(always)]
fn account_exit_kernel_inner(p: &mut ProcessControlBlock, now: u64) {
    let last_in = p.cpu_last_switched_in_tick;
    if last_in != 0 && now > last_in {
        let delta = now - last_in;
        if p.cpu_in_kernel {
            p.cpu_kernel_ticks = p.cpu_kernel_ticks.saturating_add(delta);
        } else {
            p.cpu_user_ticks = p.cpu_user_ticks.saturating_add(delta);
        }
    }
    p.cpu_last_switched_in_tick = now;
    p.cpu_in_kernel = false;

    // ITIMER_VIRTUAL：基于用户态 CPU 时间到期。
    if p.itimer_virtual_deadline != 0 && p.cpu_user_ticks >= p.itimer_virtual_deadline {
        // SIGVTALRM = 26
        p.pending_signals |= 1u64 << (26 - 1); crate::sched::process::note_signal_hint();
        if p.itimer_virtual_interval > 0 {
            p.itimer_virtual_deadline =
                p.cpu_user_ticks.saturating_add(p.itimer_virtual_interval);
        } else {
            p.itimer_virtual_deadline = 0;
        }
    }
    // ITIMER_PROF：基于用户+内核 CPU 时间到期。
    let cpu_total = p.cpu_user_ticks.saturating_add(p.cpu_kernel_ticks);
    if p.itimer_prof_deadline != 0 && cpu_total >= p.itimer_prof_deadline {
        // SIGPROF = 27
        p.pending_signals |= 1u64 << (27 - 1); crate::sched::process::note_signal_hint();
        if p.itimer_prof_interval > 0 {
            p.itimer_prof_deadline = cpu_total.saturating_add(p.itimer_prof_interval);
        } else {
            p.itimer_prof_deadline = 0;
        }
    }

    // RLIMIT_CPU 强制（与 account_enter_kernel 同源, 覆盖含 syscall 的结算点）。
    enforce_rlimit_cpu(p, now);
}

/// 精确 cputime 分账是否对当前进程启用（lock-free，遵循 HART_CURRENT_PCB
/// 单核契约）。默认关闭：多数进程从不消费 utime/stime 拆分，syscall 热路径
/// 免去两次 rdtime（QEMU TCG 下是 io helper）+ 分账运算；任何 cputime 消费者
/// （times/getrusage/CPUTIME 时钟/ITIMER_VIRTUAL|PROF/RLIMIT_CPU）首次触达
/// 即置位，此后逐 syscall 精确结算（Linux VIRT_CPU_ACCOUNTING_GEN 式）。
/// 未置位期间 CPU 总量仍由 switch_to/timer trap 结算（全记入 utime，等价于
/// Linux tick 采样口径的粗粒度）。PCB 指针为空时保守返回 true。
#[inline]
pub fn current_cputime_precise() -> bool {
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if pcb_ptr.is_null() {
        return true;
    }
    unsafe { (*pcb_ptr).cputime_precise }
}

/// 非精确分账进程的 syscall 出口轻量收尾：只刷新 HART_PENDING_SIGNALS
/// 缓存（供 check_and_deliver_signals 的 lock-free 跳过路径），不读时钟、
/// 不做 cputime 结算。
#[inline]
pub fn syscall_exit_signal_refresh() {
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if pcb_ptr.is_null() {
        return;
    }
    let p = unsafe { &*pcb_ptr };
    HART_PENDING_SIGNALS.store(p.pending_signals & !p.signal_mask, Ordering::Relaxed);
}

/// 为当前进程启用精确 cputime 分账。须传入新鲜 rdtime：同时刷新
/// HART_KERNEL_ENTER_TICK，防止本次 syscall 出口用陈旧 enter tick 结算出
/// 巨额虚假 kernel 段。
pub fn enable_cputime_precise_current(now: u64) {
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        unsafe { (*pcb_ptr).cputime_precise = true };
    }
    HART_KERNEL_ENTER_TICK.store(now, Ordering::Relaxed);
}

/// Combined user→kernel + kernel→user accounting for syscall paths.
/// Reads `HART_KERNEL_ENTER_TICK` (stored at trap entry without lock) and
/// does both enter and exit accounting in a **single** PROCESS_TABLE lock,
/// saving one lock+BTreeMap lookup per syscall vs the separate
/// `account_enter_kernel` + `account_exit_kernel` pair.
///
/// Also caches `(pending_signals & !signal_mask)` into
/// `HART_PENDING_SIGNALS` so `check_and_deliver_signals` can skip the lock
/// when no signals are deliverable.
pub fn account_syscall_combined_exit(exit_now: u64) {
    if crate::DIAG_SKIP_ACCT {
        return;
    }
    let enter_now = HART_KERNEL_ENTER_TICK.load(Ordering::Relaxed);
    // Lock-free fast path via the per-hart current-PCB shadow pointer:
    // on the syscall exit path interrupts are disabled on the (only) hart,
    // so no other context can touch the table or move/free the current PCB
    // (see HART_CURRENT_PCB safety contract). This skips the PROCESS_TABLE
    // spinlock + BTreeMap lookup on every syscall.
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &mut *pcb_ptr };
        account_syscall_combined_exit_inner(p, enter_now, exit_now);
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        account_syscall_combined_exit_inner(p, enter_now, exit_now);
    }
}

#[inline(always)]
fn account_syscall_combined_exit_inner(
    p: &mut ProcessControlBlock,
    enter_now: u64,
    exit_now: u64,
) {
    {
        // 注：cpu tick 累加用普通加法（非 saturating）——u64 tick 在 10MHz~1GHz
        // 时基下数百年不可能回绕（Linux cputime 同样直加）；saturating 序列
        // 在 RV 上每处多 sltu+neg+or 三条，本函数每 syscall 执行。
        // Phase 1: user→kernel (replaces account_enter_kernel)
        let last_in = p.cpu_last_switched_in_tick;
        if last_in != 0 && enter_now > last_in {
            // Time from last checkpoint to kernel entry = user time
            p.cpu_user_ticks = p.cpu_user_ticks.wrapping_add(enter_now - last_in);
        }
        // Phase 2: kernel→user (replaces account_exit_kernel body)
        if enter_now != 0 && exit_now > enter_now {
            p.cpu_kernel_ticks = p.cpu_kernel_ticks.wrapping_add(exit_now - enter_now);
        }
        p.cpu_last_switched_in_tick = exit_now;
        p.cpu_in_kernel = false;

        // ITIMER_VIRTUAL
        if p.itimer_virtual_deadline != 0 && p.cpu_user_ticks >= p.itimer_virtual_deadline {
            p.pending_signals |= 1u64 << (26 - 1); crate::sched::process::note_signal_hint();
            if p.itimer_virtual_interval > 0 {
                p.itimer_virtual_deadline =
                    p.cpu_user_ticks.saturating_add(p.itimer_virtual_interval);
            } else {
                p.itimer_virtual_deadline = 0;
            }
        }
        // ITIMER_PROF
        let cpu_total = p.cpu_user_ticks.wrapping_add(p.cpu_kernel_ticks);
        if p.itimer_prof_deadline != 0 && cpu_total >= p.itimer_prof_deadline {
            p.pending_signals |= 1u64 << (27 - 1); crate::sched::process::note_signal_hint();
            if p.itimer_prof_interval > 0 {
                p.itimer_prof_deadline = cpu_total.saturating_add(p.itimer_prof_interval);
            } else {
                p.itimer_prof_deadline = 0;
            }
        }
        enforce_rlimit_cpu(p, exit_now);

        // Cache deliverable signals for lock-free fast path in
        // check_and_deliver_signals.
        HART_PENDING_SIGNALS.store(
            p.pending_signals & !p.signal_mask,
            Ordering::Relaxed,
        );
    }
}

/// Slice 2.2: I/O 计费 helper —— 给 `/proc/[pid]/io` 用。
///
/// Linux `Documentation/filesystems/proc.rst` `/proc/[pid]/io` 语义：
/// - `rchar`/`wchar`：syscall 层成功传输的字节数（不区分 storage / cache /
///   pipe / tty）。read()=0 (EOF) 也计 syscr 但不加 rchar。
/// - `syscr`/`syscw`：read/readv/pread/preadv/sendfile-read-side 等 syscall
///   每次进入即 +1，不论返回值（Linux 在 `task_io_account_read` 之前就 +1）。
///   我们这里在 syscall 成功路径才统一 +1，未来需要可以提前到入口。
/// - `read_bytes`/`write_bytes`：仅算"会触发后端 storage I/O"的字节，对
///   regular file / block device 计入；pipe/socket/tty 不计。`is_storage`
///   参数让 caller 指明。
///
/// 我们用 PROCESS_TABLE.current 作为目标进程；这与 syscall handler 一定
/// 在当前进程上下文里执行的约束一致。注意：sys_read/sys_write 等成功路径
/// 已经在持锁/释放之间，本 helper 自己短期持锁，不会引起递归。
pub fn account_io_read(bytes: usize, is_storage: bool) {
    // 无 /proc/[pid]/io 读者时（lmbench/libcbench/iozone 等全部场景）跳过，
    // 省每次 read/readv/pread/splice/sendfile 一把全局锁 + 一次 BTreeMap 查找。
    if !IO_ACCOUNT_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &mut *pcb_ptr };
        p.io_rchar = p.io_rchar.saturating_add(bytes as u64);
        p.io_syscr = p.io_syscr.saturating_add(1);
        if is_storage {
            p.io_read_bytes = p.io_read_bytes.saturating_add(bytes as u64);
        }
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        p.io_rchar = p.io_rchar.saturating_add(bytes as u64);
        p.io_syscr = p.io_syscr.saturating_add(1);
        if is_storage {
            p.io_read_bytes = p.io_read_bytes.saturating_add(bytes as u64);
        }
    }
}

pub fn account_io_write(bytes: usize, is_storage: bool) {
    if !IO_ACCOUNT_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &mut *pcb_ptr };
        p.io_wchar = p.io_wchar.saturating_add(bytes as u64);
        p.io_syscw = p.io_syscw.saturating_add(1);
        if is_storage {
            p.io_write_bytes = p.io_write_bytes.saturating_add(bytes as u64);
        }
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        p.io_wchar = p.io_wchar.saturating_add(bytes as u64);
        p.io_syscw = p.io_syscw.saturating_add(1);
        if is_storage {
            p.io_write_bytes = p.io_write_bytes.saturating_add(bytes as u64);
        }
    }
}

/// Slice 2.5 wchan 真 hook：把 `mark_sleeping` 收到的 `waiting_for` sentinel/
/// PID 映射成 `/proc/[pid]/wchan` 的语义标签（&'static str，无堆分配）。
///
/// Linux 中 wchan 是 kernel RIP → symbol；我们没符号表，用 syscall 名替代。
/// 覆盖现存所有 sentinel + wait4 两种情形（指定 child / 任意 child）。
pub fn wait_channel_for(waiting_for: Option<Pid>) -> &'static str {
    match waiting_for {
        None => "wait_any_child",
        Some(IO_WAIT_SENTINEL) => "io_wait",
        Some(SIGNAL_WAIT_SENTINEL) => "sigsuspend",
        Some(NANOSLEEP_SENTINEL) => "nanosleep",
        Some(DEADLINE_RETURN_SENTINEL) => "deadline_wait",
        Some(FUTEX_WOKEN_SENTINEL) => "futex_wait_queue_me",
        Some(POLL_WAIT_SENTINEL) => "do_sys_poll",
        Some(MQ_SLEEP_SENTINEL) => "mqueue_wait",
        Some(MQ_WOKEN_SENTINEL) => "mqueue_woken",
        Some(LOCK_WAIT_SENTINEL) => "fcntl_setlk_wait",
        Some(SOCKET_WAIT_SENTINEL) => "sock_wait",
        Some(_pid) => "wait_pid",
    }
}

/// Slice 2.5 wchan 真 hook：syscall 入口显式 set 更精确的等待点标签。
///
/// `mark_sleeping` 的自动推导只能根据 sentinel 区分（io_wait/sigsuspend/...），
/// 不够细 — Linux 在 `pipe_wait`/`unix_stream_read_actor`/`sock_recvmsg`/
/// `do_sys_poll`/`futex_wait_queue_me`/`hrtimer_nanosleep` 等都是不同符号。
/// 阻塞 syscall handler 可在切入 `suspend_current_for_io` / `mark_sleeping`
/// 之前显式 `enter_wait_channel("pipe_wait")` set 一个更具体的标签；
/// `mark_sleeping` 看到 `wait_channel.is_some()` 就不会覆盖。
///
/// 注意：调用者负责在 syscall 返回 user 前 clear（或让 `mark_runnable` 在
/// `clear_wait=true` 时统一 clear）。
pub fn enter_wait_channel(label: &'static str) {
    // 免锁快路径：单核 + trap 全程关中断（HART_CURRENT_PCB 安全契约），
    // 只有当前进程会写自己的 wait_channel。
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        unsafe { (*pcb_ptr).wait_channel = Some(label) };
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        p.wait_channel = Some(label);
    }
}

/// Record a durable waitqueue wakeup for a task that has already enqueued a
/// WaitEntry but is not yet in `Sleeping`.  This mirrors Linux's
/// prepare_to_wait()/schedule() contract: a wake in the pre-schedule window
/// must cause the waiter to re-check the condition instead of going to sleep.
pub fn note_waitqueue_wake(pid: Pid, clear_wait: bool) -> bool {
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return false };
    let Some(p) = table.get_mut(pid) else { return false };
    if matches!(p.state, ProcessState::Zombie | ProcessState::Unused) {
        return false;
    }
    p.waitqueue_woken = true;
    if clear_wait {
        p.waiting_for = None;
        p.wakeup_channel = 0;
        p.wait_channel = None;
    }
    true
}

/// Slice 2.5 wchan 真 hook：显式 clear（错误退出/EAGAIN 提前返 user 等场景）。
pub fn leave_wait_channel() {
    // Same per-hart current shadow contract as enter_wait_channel.
    let pcb_ptr = HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        unsafe { (*pcb_ptr).wait_channel = None };
        return;
    }
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let pid = match table.current { Some(p) => p, None => return };
    if let Some(p) = table.get_mut(pid) {
        p.wait_channel = None;
    }
}

/// 把子进程的累积 CPU 时间合并到父进程的 `cpu_children_*`，对应 POSIX
/// `tms.tms_cutime / tms.tms_cstime` 的 "已 reap 子进程贡献" 语义。
/// 在 sys_wait4 / sys_waitid 真正回收 zombie 时调用。
pub fn account_reap_child(parent_pid: Pid, child_pid: Pid) {
    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return };
    let (child_u, child_k, child_cu, child_ck, child_mr) = match table.get(child_pid) {
        Some(c) => (
            c.cpu_user_ticks,
            c.cpu_kernel_ticks,
            c.cpu_children_user_ticks,
            c.cpu_children_kernel_ticks,
            c.maxrss_kb.max(c.children_maxrss_kb),
        ),
        None => return,
    };
    if let Some(p) = table.get_mut(parent_pid) {
        p.cpu_children_user_ticks =
            p.cpu_children_user_ticks.saturating_add(child_u).saturating_add(child_cu);
        p.cpu_children_kernel_ticks =
            p.cpu_children_kernel_ticks.saturating_add(child_k).saturating_add(child_ck);
        p.children_maxrss_kb = p.children_maxrss_kb.max(child_mr);
    }
}

/// IO 阻塞睡眠，附带短 deadline 防止丢失唤醒
/// (wake_io_waiters 仍可立即唤醒；deadline 是兜底)
pub fn suspend_current_for_io(deadline: u64) -> ! {
    suspend_current_for_io_inner(deadline)
}

pub fn suspend_current_for_lock_wait() -> ! {
    let mut guard = PROCESS_TABLE.lock();
    if let Some(table) = guard.as_mut() {
        if let Some(pid) = table.current {
            if let Some(current) = table.get_mut(pid) {
                current.waiting_for = Some(LOCK_WAIT_SENTINEL);
                current.wakeup_channel = 0;
            }
        }
    }
    drop(guard);
    suspend_current_for_io_inner(0)
}

/// CLONE_VFORK 父的同步阻塞点（Linux 语义：clone 在内核内等待子 execve/exit）。
///
/// 与 IO 睡眠不同，这里**不回退 sepc**：返回值（child_pid）已写入父
/// TrapContext，唤醒后直接恢复用户态、从 clone 调用点之后继续。仅置
/// Sleeping 标志而不让出 CPU 是不够的——trap 返回不检查状态，SMP 下父会
/// 继续执行 glibc 并 munmap 仍被子进程使用的 posix_spawn 栈。
///
/// 前置：调用方已在 PROCESS_TABLE 锁内把父置为
/// `Sleeping + VFORK_WAIT_SENTINEL`（fork_with_ctx 的 CLONE_VFORK 分支）。
pub fn suspend_vfork_parent_return(child_pid: usize) -> ! {
    use crate::arch::usertrapret;
    use crate::arch::TrapContext;

    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("Process table not initialized");
    let current_pid = table.current.expect("No current process");

    if let Some(current) = table.get_mut(current_pid) {
        unsafe {
            if let Some(cx) = current.trap_context_mut() {
                cx.set_return_value(child_pid);
            }
        }
        // 子可能已在另一 hart 完成 execve/exit 并消费了 vfork_waiting_for：
        // 此时父已被改回 Runnable，直接带返回值恢复用户态，不再入睡。
        if current.vfork_waiting_for.is_none() {
            set_process_state(current, ProcessState::Running);
            current.waiting_for = None;
            current.wait_channel = None;
            let user_satp = current.satp();
            let trap_cx_ptr =
                unsafe { current.trap_context_mut().unwrap() as *mut TrapContext };
            drop(guard);
            unsafe { usertrapret(&mut *trap_cx_ptr, user_satp) }
        }
    }

    let next_pid = table.select_next(current_pid, None);
    if let Some(target_pid) = next_pid {
        if !table.switch_to(target_pid, 0) {
            drop(guard);
            kernel_idle_until_runnable();
        }
        let next = table.get_mut(target_pid).unwrap();
        let user_satp = next.satp();
        let trap_cx = unsafe { next.trap_context_mut().unwrap() };
        let trap_cx_ptr = trap_cx as *mut TrapContext;
        drop(guard);
        unsafe { usertrapret(&mut *trap_cx_ptr, user_satp) }
    }
    drop(guard);
    kernel_idle_until_runnable();
}

pub fn suspend_current_until_deadline_return(deadline: u64, retval: isize) -> ! {
    use crate::arch::usertrapret;
    use crate::arch::TrapContext;

    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("Process table not initialized");
    let current_pid = table.current.expect("No current process");

    if let Some(current) = table.get_mut(current_pid) {
        unsafe {
            if let Some(cx) = current.trap_context_mut() {
                cx.set_return_value(retval as usize);
            }
        }
    }
    table.mark_sleeping(current_pid, Some(DEADLINE_RETURN_SENTINEL), deadline as usize);

    let next_pid = table.select_next(current_pid, None);

    if let Some(target_pid) = next_pid {
        if !table.switch_to(target_pid, 0) {
            drop(guard);
            kernel_idle_until_runnable();
        }
        let next = table.get_mut(target_pid).unwrap();
        let user_satp = next.satp();
        let trap_cx = unsafe { next.trap_context_mut().unwrap() };
        let trap_cx_ptr = trap_cx as *mut TrapContext;
        drop(guard);
        unsafe { usertrapret(&mut *trap_cx_ptr, user_satp); }
    }

    // 没有 next 时绝不能继续在 Sleeping 任务自己的内核栈上 WFI，也不能
    // 无条件把自己改回 Running。切到 per-hart idle 栈后由统一调度循环等待
    // deadline 唤醒并重新认领该任务。
    drop(guard);
    kernel_idle_until_runnable();
}

fn suspend_current_for_io_inner(deadline: u64) -> ! {
    use crate::arch::usertrapret;

    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("Process table not initialized");

    let current_pid = table.current.expect("No current process");

    // 1. 挂起当前进程：Sleeping，sepc 回退 4 (重新执行 ecall)
    // If a waitqueue wake arrived after this task enqueued its WaitEntry but
    // before it actually reached Sleeping, consume that pending wake and restart
    // the syscall immediately.  This is the Linux prepare_to_wait()/schedule()
    // lost-wakeup contract and prevents pipe/futex/poll waiters from sleeping
    // forever when the producer/consumer races in this small window.
    if let Some(current) = table.get_mut(current_pid) {
        let was_pre_woken = current.waitqueue_woken;
        if was_pre_woken {
            current.waitqueue_woken = false;
            current.io_wait_trap_context.clear();
            unsafe {
                if let Some(cx) = current.trap_context_mut() {
                    cx.sepc -= 4; // 回退到 ecall
                }
            }
            current.clear_wait_state();
            let user_satp = current.satp();
            let trap_cx_ptr =
                unsafe { current.trap_context_mut().unwrap() as *mut crate::arch::TrapContext };
            drop(guard);
            unsafe {
                usertrapret(&mut *trap_cx_ptr, user_satp);
            }
        }
        if current.waiting_for.is_none() {
            current.waiting_for = Some(IO_WAIT_SENTINEL);
        }
        if deadline > 1 { current.wakeup_channel = deadline as usize; }
        if current.trap_context_ptr != 0 {
            let cx_ptr = current.trap_context_ptr as *mut crate::arch::TrapContext;
            unsafe {
                (*cx_ptr).sepc -= 4; // 回退到 ecall
            }
            // ctx 原地留在本进程 kstack 顶，只置重放标记（免 304B 快照拷贝）。
            current.io_wait_trap_context.arm();
        } else {
            current.io_wait_trap_context.clear();
        }
    }
    let waiting_for = table.get(current_pid).and_then(|current| current.waiting_for);
    // 调用方（如 futex 定时等待）可能已在 PCB 上设置了 wakeup_channel
    // deadline；deadline 参数为 0 时必须保留它，否则定时等待永不超时。
    let wakeup_channel = if deadline > 1 {
        deadline as usize
    } else {
        table.get(current_pid).map(|p| p.wakeup_channel).unwrap_or(0)
    };
    if crate::cmdline::ftxdbg_enabled() {
        let prev = table.get(current_pid).map(|p| p.wakeup_channel).unwrap_or(0);
        crate::println!(
            "[ftxdbg] suspend.mark_sleeping pid={} prev_wakeup={} new_wakeup={} waiting_for={:?}",
            current_pid, prev, wakeup_channel, waiting_for
        );
    }
    table.mark_sleeping(current_pid, waiting_for, wakeup_channel);

    // 2. 找到下一个 Runnable 进程
    let next_pid = table.select_next(current_pid, None);

    match next_pid {
        Some(target_pid) => {
            // 3. 切换到目标进程
            if !table.switch_to(target_pid, 0) {
                drop(guard);
                kernel_idle_until_runnable();
            }
            let user_satp = table.get_mut(target_pid).unwrap().satp();
            let (trap_cx_ptr, deliver_signal) =
                prepare_trap_context_for_user_return(table, target_pid).unwrap();
            drop(guard);
            unsafe {
                if deliver_signal {
                    crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
                }
                usertrapret(&mut *trap_cx_ptr, user_satp);
            }
        }
        None => {
            drop(guard);
            kernel_idle_until_runnable();
        }
    }
    unreachable!();
}

/// 当前进程等待信号并切换到其他可运行进程。
///
/// 与 `suspend_current_and_run_next` 类似，但使用专门的 signal wait 哨兵，
/// 使子进程退出或显式发信号时可以把它唤醒。
pub fn sleep_current_for_signal() -> ! {
    use crate::arch::usertrapret;

    let current_pid = {
        let mut guard = PROCESS_TABLE.lock();
        let table = guard.as_mut().expect("Process table not initialized");
        let current_pid = table.current.expect("No current process");
        if let Some(current) = table.get_mut(current_pid) {
            unsafe {
                if let Some(cx) = current.trap_context_mut() {
                    let mut saved = *cx;
                    saved.sepc -= 4;
                    *cx = saved;
                    current.signal_wait_trap_context = Some(saved);
                }
            }
        }
        let wakeup_channel = table.get(current_pid).map(|p| p.wakeup_channel).unwrap_or(0);
        table.mark_sleeping(current_pid, Some(SIGNAL_WAIT_SENTINEL), wakeup_channel);
        current_pid
    };

    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("Process table not initialized");
    let target_pid = if table.get(current_pid).map_or(false, |p| {
        p.state == ProcessState::Runnable && p.on_cpu.is_none()
    }) {
        Some(current_pid)
    } else {
        table.select_next(current_pid, None)
    };

    if let Some(target_pid) = target_pid {
        if !table.switch_to(target_pid, 0) {
            drop(guard);
            kernel_idle_until_runnable();
        }
        let user_satp = table.get_mut(target_pid).unwrap().satp();
        let (trap_cx_ptr, deliver_signal) =
            prepare_trap_context_for_user_return(table, target_pid).unwrap();
        drop(guard);
        unsafe {
            if deliver_signal {
                crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
            }
            usertrapret(&mut *trap_cx_ptr, user_satp);
        }
    }

    drop(guard);
    kernel_idle_until_runnable();
}

/// 将当前进程挂起用于 nanosleep，deadline 存入 wakeup_channel。
/// timer_tick 中 `check_sleep_deadlines()` 会在 rdtime >= deadline 时唤醒。
/// 该函数永不返回——唤醒后重新执行 ecall。
pub fn suspend_current_for_nanosleep(deadline: u64) -> ! {
    use crate::arch::usertrapret;

    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().expect("Process table not initialized");
    let current_pid = table.current.expect("No current process");

    let startup_handoff = table.take_startup_handoff_target(current_pid);
    if let Some(current) = table.get_mut(current_pid) {
        unsafe {
            if let Some(cx) = current.trap_context_mut() {
                cx.sepc -= 4; // 回退到 ecall
                current.nanosleep_trap_context = Some(*cx);
            }
        }
    }
    table.mark_sleeping(current_pid, Some(NANOSLEEP_SENTINEL), deadline as usize);
    // 登记到期点并立即把硬件定时器收窄到 min(now+周期tick, 此 deadline)，使本次睡眠
    // 能在 deadline 精确被唤醒（hrtimer 风格），而非等满下一个周期 tick。
    note_sleep_deadline(deadline);
    crate::arch::rearm_timer();

    let next_pid = startup_handoff.or_else(|| table.select_next(current_pid, None));

    let target_pid = match next_pid {
        Some(pid) => {
            pid
        },
        None => {
            drop(guard);
            kernel_idle_until_runnable();
            unreachable!();
        }
    };

    if !table.switch_to(target_pid, crate::sched::trace::SWITCH_NANOSLEEP) {
        drop(guard);
        kernel_idle_until_runnable();
    }

    let user_satp = table.get_mut(target_pid).unwrap().satp();
    let (trap_cx_ptr, deliver_signal) = prepare_trap_context_for_user_return(table, target_pid).unwrap();

    drop(guard);

    unsafe {
        if deliver_signal {
            crate::syscall::check_and_deliver_signals(&mut *trap_cx_ptr);
        }
        usertrapret(&mut *trap_cx_ptr, user_satp);
    }
    unreachable!();
}

/// 由 timer_tick 调用——检查所有 NANOSLEEP_SENTINEL / SIGNAL_WAIT_SENTINEL 睡眠者的 deadline，
/// 到期者设为 Runnable。
pub fn check_sleep_deadlines() {
    let now: u64;
    now = crate::arch::asm_helpers::rdtime();

    // 无锁短路：所有定时到期源（wakeup_channel 经 mark_sleeping 单点入口、
    // futex_waitv 直写点、alarm/itimer 设置点）都会 note_sleep_deadline 收窄
    // NEAREST_DEADLINE，故最近到期点尚未来到时无需拿表锁全扫。陈旧的过小值
    // 只多做一次扫描（随后被重算修正），不会漏唤醒。
    // RT 定点唤醒快路径：cyclictest 类单 RT 定时睡眠者到期时，绕过 O(#processes)
    // 全表扫描直接唤醒该 pid（一次 BTreeMap 查找）。全扫（含 alarm、其他 sleeper、
    // NEAREST_DEADLINE 重算）推迟到下一 tick 由周期兜底执行，不在 RT 唤醒关键
    // 路径上花时间。槽内容陈旧（已被别的事件唤醒/重入睡）时按状态校验丢弃。
    if now >= RT_SLEEP_DL.load(Ordering::Relaxed) {
        let pid = RT_SLEEP_PID.load(Ordering::Relaxed);
        let dl = RT_SLEEP_DL.load(Ordering::Relaxed);
        RT_SLEEP_DL.store(u64::MAX, Ordering::Relaxed);
        RT_SLEEP_PID.store(0, Ordering::Relaxed);
        let mut woken = false;
        if pid != 0 {
            let mut guard = PROCESS_TABLE.lock();
            if let Some(table) = guard.as_mut() {
                let valid = table.get(pid).map_or(false, |p| {
                    p.state == ProcessState::Sleeping && p.wakeup_channel as u64 == dl
                });
                if valid {
                    let (keep_wait, wake_policy) = table.get(pid).map_or((false, 0), |p| {
                        (
                            p.waiting_for == Some(NANOSLEEP_SENTINEL)
                                || p.waiting_for == Some(SIGNAL_WAIT_SENTINEL)
                                || p.waiting_for == Some(POLL_WAIT_SENTINEL)
                                || p.waiting_for == Some(SOCKET_WAIT_SENTINEL)
                                || p.waiting_for == Some(MQ_SLEEP_SENTINEL)
                                || p.wait_channel == Some("futex_wait_queue_me"),
                            p.sched_policy,
                        )
                    });
                    // 与下方全扫路径同一门控：只有 RT/DEADLINE 唤醒才请求立即抢占
                    //（对标 Linux check_preempt_curr 的 RT 分支）。此前无条件置位：
                    // cargo/rustc 线程池的普通 futex 超时（SCHED_OTHER）每次都把全局
                    // NEED_RESCHED 置 1，而多数 hart 空闲、无人在"返回用户态"处消费，
                    // 旗标长期为 1 → set_next_timer 把有任务 hart 的定时器收窄成
                    // ~10µs 周期 → 8 核 100kHz 中断风暴（pchist：la64_trap_handler
                    // 21G 指令 / idle_loop 33G 指令），BuildStorm hello-world 编译
                    // 从 ~1min 劣化到 >12min（rep-la-minibuild-1-181607.serial）。
                    if wake_policy == 1 || wake_policy == 2 || wake_policy == 6 {
                        set_need_resched();
                    }
                    {
                        let late_us = now.saturating_sub(dl) * 1_000_000
                            / crate::arch::consts::CLOCK_FREQ as u64;
                        if crate::RTDIAG {
                            crate::sched::process::rtstat_record(12, late_us);
                        }
                        if late_us > 150 {
                            let cur = table.current.unwrap_or(0);
                            let nr = table.get(cur).map(|p| p.last_syscall.0 as i64).unwrap_or(-1);
                            crate::rtdiag_println!(
                                "[rtdiag] wake-late-fast pid={} late_us={} cur={} cur_nr={}",
                                pid, late_us, cur, nr
                            );
                        }
                    }
                    crate::sched::process::RT_WAKE_TS.store(now, Ordering::Relaxed);
                    crate::sched::process::RT_WAKE_PID.store(pid, Ordering::Relaxed);
                    if table.get(pid).map_or(false, |p| p.waiting_for == Some(DEADLINE_RETURN_SENTINEL)) {
                        crate::sched::deadline::replenish_deadline_task(table, pid);
                    }
                    table.mark_runnable(pid, !keep_wait);
                    if keep_wait {
                        if let Some(p) = table.get_mut(pid) {
                            p.wait_channel = None;
                        }
                    }
                    woken = true;
                }
            }
        }
        // 唤醒成功才短路；槽 stale 时继续走 NEAREST/全扫兜底，避免真正到期者被推迟。
        if woken {
            return;
        }
    }

    if now < NEAREST_DEADLINE.load(Ordering::Relaxed) {
        return;
    }

    // [DIAG] check 空窗测量：两次实际全扫之间隔 >1ms 即无 tick/无检查窗口
    {
        static LAST_CHECK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        let last = LAST_CHECK.swap(now, Ordering::Relaxed);
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        let gap_us = now.saturating_sub(last) * 1_000_000 / freq;
        if false && last != 0 && gap_us > 1000 {
            crate::rtdiag_println!("[rtdiag] check-gap us={}", gap_us);
        }
    }

    let diag_l0 = if crate::RTDIAG { crate::arch::asm_helpers::rdtime() } else { 0 };
    let mut guard = PROCESS_TABLE.lock();
    if crate::RTDIAG {
        let us = crate::arch::asm_helpers::rdtime().saturating_sub(diag_l0) * 1_000_000
            / crate::arch::consts::CLOCK_FREQ as u64;
        crate::sched::process::rtstat_record(18, us);
    }
    if let Some(table) = guard.as_mut() {
        // 只遍历**真实存在的进程**（O(#processes)）。旧实现 `for pid in 1..NEXT_PID`
        // 是 O(历史最大 PID)：NEXT_PID 随每次 clone 单调递增，被 libc-bench 推到数千
        // 后，每个 timer tick（LA 高达 4000Hz）都退化为 O(N) 全 PID 空扫（对每个 PID
        // 值做一次 BTreeMap get）→ 拖慢其后所有测试组。
        // 注意：本函数还负责 poll/socket/futex/mq/epoll 的超时唤醒（见下方哨兵集）。
        // 这些路径最终都经 mark_sleeping 单点入口（或 futex_waitv 直写点）落表，
        // 那里统一 note_sleep_deadline 收窄 NEAREST_DEADLINE，配合上方无锁短路。
        // 单趟只读遍历收集到期者（大多数 tick 一个都没有），随后仅对到期的
        // 少数 pid 做 get/get_mut 修改。旧实现先 collect 全部 pid 再对每个
        // pid 做 2~3 次 BTreeMap 查找，2500 线程时每次扫描 ~7500 次树查找，
        // 是 libc-bench 期间 timer_tick 热点桶（188M 指令）的主体。
        let mut min_dl = u64::MAX;
        let mut expired_sleeps: alloc::vec::Vec<(Pid, u64, Pid, i32, i32, bool)> =
            alloc::vec::Vec::new();
        let mut expired_alarms: alloc::vec::Vec<Pid> = alloc::vec::Vec::new();
        let diag_s0 = if crate::RTDIAG { crate::arch::asm_helpers::rdtime() } else { 0 };
        // hrtimer 到期树主路径：只弹到期前缀（O(log n + 到期数)），取代每次
        // O(#processes) 全表扫（hackbench 400 进程时全扫 ~40µs×每秒千次，
        // 既吃 CPU 又直接计入 cyclictest 唤醒尾延）。条目惰性失效：弹出时按
        // 进程实时状态校验，陈旧项（已被其他事件唤醒/重入睡/alarm 改期）丢弃。
        // 周期全扫兜底（每 ~100ms 一次）：捕获任何未经登记入口的漏网到期者。
        // 所有定时睡眠都经 mark_sleeping / futex 直写点登记入树（vfork 等待与
        // DEADLINE 节流睡眠无定时到期，不依赖本函数），全扫仅是防御性安全网；
        // hackbench 数百进程时一次全扫可达数百 µs，直接计入 cyclictest 尾延，
        // 故拉长周期把与 RT 唤醒撞车的概率再降一个量级。
        static LAST_FULL_SCAN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
        let scan_period = crate::arch::consts::CLOCK_FREQ as u64 / 10;
        let last_scan = LAST_FULL_SCAN.load(Ordering::Relaxed);
        let full_scan = now.wrapping_sub(last_scan) >= scan_period;
        if full_scan {
            LAST_FULL_SCAN.store(now, Ordering::Relaxed);
            for (&pid, p) in table.processes.iter() {
                if p.state == ProcessState::Sleeping && p.wakeup_channel > 1 {
                    let d = p.wakeup_channel as u64;
                    if now >= d {
                        let keep_wait = p.waiting_for == Some(NANOSLEEP_SENTINEL)
                            || p.waiting_for == Some(SIGNAL_WAIT_SENTINEL)
                            || p.waiting_for == Some(POLL_WAIT_SENTINEL)
                            || p.waiting_for == Some(SOCKET_WAIT_SENTINEL)
                            || p.waiting_for == Some(MQ_SLEEP_SENTINEL)
                            // futex 定时等待：必须保留 wakeup_channel，
                            // 让 futex 重入路径据其返回 -ETIMEDOUT；
                            // 否则 deadline 被清零后重入会按新等待重算
                            // 相对超时，无限续期永不超时。
                            || p.wait_channel == Some("futex_wait_queue_me");
                        expired_sleeps.push((
                            pid,
                            d,
                            p.tgid,
                            p.sched_policy,
                            p.sched_priority,
                            keep_wait,
                        ));
                    } else if d < min_dl {
                        min_dl = d;
                    }
                }
                if p.alarm_deadline != 0 {
                    let a = p.alarm_deadline as u64;
                    if now >= a {
                        if p.state != ProcessState::Zombie {
                            expired_alarms.push(pid);
                        }
                    } else if a < min_dl {
                        min_dl = a;
                    }
                }
            }
            // 全扫已覆盖一切到期者：同步排空树的到期前缀，避免重复处理。
            while let Some((&(dl, pid), _)) = table.sleep_timers.iter().next() {
                if dl > now {
                    break;
                }
                table.sleep_timers.remove(&(dl, pid));
            }
        } else {
            let mut rearm_alarms: alloc::vec::Vec<(Pid, u64)> = alloc::vec::Vec::new();
            loop {
                let (dl, pid, is_alarm) = match table.sleep_timers.iter().next() {
                    Some((&(dl, pid), &is_alarm)) if dl <= now => (dl, pid, is_alarm),
                    _ => break,
                };
                table.sleep_timers.remove(&(dl, pid));
                if is_alarm {
                    if let Some(p) = table.get(pid) {
                        if p.alarm_deadline != 0 && p.state != ProcessState::Zombie {
                            let a = p.alarm_deadline as u64;
                            if now >= a {
                                expired_alarms.push(pid);
                            } else {
                                // alarm 被改期（setitimer 重设）：重新登记新到期点。
                                rearm_alarms.push((pid, a));
                            }
                        }
                    }
                } else if let Some(p) = table.get(pid) {
                    if p.state == ProcessState::Sleeping && p.wakeup_channel as u64 == dl {
                        let keep_wait = p.waiting_for == Some(NANOSLEEP_SENTINEL)
                            || p.waiting_for == Some(SIGNAL_WAIT_SENTINEL)
                            || p.waiting_for == Some(POLL_WAIT_SENTINEL)
                            || p.waiting_for == Some(SOCKET_WAIT_SENTINEL)
                            || p.waiting_for == Some(MQ_SLEEP_SENTINEL)
                            || p.wait_channel == Some("futex_wait_queue_me");
                        expired_sleeps.push((
                            pid,
                            dl,
                            p.tgid,
                            p.sched_policy,
                            p.sched_priority,
                            keep_wait,
                        ));
                    }
                }
            }
            for (pid, a) in rearm_alarms {
                table.register_sleep_timer(pid, a, true);
            }
        }
        if crate::RTDIAG {
            let us = crate::arch::asm_helpers::rdtime().saturating_sub(diag_s0) * 1_000_000
                / crate::arch::consts::CLOCK_FREQ as u64;
            crate::sched::process::rtstat_record(19, us);
        }
        let diag_w0 = if crate::RTDIAG { crate::arch::asm_helpers::rdtime() } else { 0 };
        for &(pid, deadline, tgid, policy, priority, keep_wait) in expired_sleeps.iter() {
            // RT/DEADLINE 任务被唤醒：置 need_resched，使其在最近的抢占点
            // （返回用户态 / 收窄定时器触发的 tick）尽快抢占低优先级当前任务。
            if policy == 1 || policy == 2 || policy == 6 {
                set_need_resched();
                // [DIAG] RT 唤醒迟到量（rdtime ticks）
                let late = now.saturating_sub(deadline);
                let freq = crate::arch::consts::CLOCK_FREQ as u64;
                let late_us = late * 1_000_000 / freq;
                if late_us > 150 {
                    let (cur, nr) = {
                        let cur = table.current.unwrap_or(0);
                        let nr = table.get(cur).map(|p| p.last_syscall.0 as i64).unwrap_or(-1);
                        (cur, nr)
                    };
                    crate::rtdiag_println!(
                        "[rtdiag] wake-late pid={} late_us={} cur={} cur_nr={}",
                        pid, late_us, cur, nr
                    );
                }
                crate::sched::process::RT_WAKE_TS.store(now, Ordering::Relaxed);
                crate::sched::process::RT_WAKE_PID.store(pid, Ordering::Relaxed);
            }
            if table.get(pid).map_or(false, |p| p.waiting_for == Some(DEADLINE_RETURN_SENTINEL)) {
                crate::sched::deadline::replenish_deadline_task(table, pid);
            }
            crate::sched::trace::record_event(
                crate::sched::trace::EV_SLEEP_WAKE,
                pid,
                tgid,
                policy,
                priority,
                deadline,
                now.saturating_sub(deadline),
            );
            table.mark_runnable(pid, !keep_wait);
            if keep_wait {
                if let Some(p) = table.get_mut(pid) {
                    p.wait_channel = None;
                }
            }
        }
        for &pid in expired_alarms.iter() {
            let alarm_wake = if let Some(p) = table.get_mut(pid) {
                if p.alarm_deadline != 0
                    && now >= p.alarm_deadline as u64
                    && p.state != ProcessState::Zombie
                {
                    p.pending_signals |= 1u64 << (14 - 1); crate::sched::process::note_signal_hint(); // SIGALRM
                    if p.itimer_real_interval > 0 {
                        p.alarm_deadline = now.saturating_add(p.itimer_real_interval) as usize;
                    } else {
                        p.alarm_deadline = 0;
                    }
                    // do_wait（wait4/waitpid）阻塞被 SIGALRM 中断：必须保留
                    // wait_trap_context / wait_channel，让 trap 返回路径据
                    // SA_RESTART 决定重启还是返回 -EINTR。clear_wait 会清掉
                    // 这些状态，导致阻塞前残留的 a0（pid 入参，如 -1）被当
                    // 返回值带回用户态，被 musl 误读为 errno=EPERM。
                    if p.state == ProcessState::Sleeping {
                        Some(p.wait_channel == Some("do_wait"))
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            };
            if let Some(keep_wait) = alarm_wake {
                table.mark_runnable(pid, !keep_wait);
            }
        }
        // 到期者被处理后其新的到期点（itimer 重装的 alarm、keep_wait 保留的
        // wakeup_channel）也要参与最近到期点并重新登记到期树。
        let mut rereg: alloc::vec::Vec<(Pid, u64, bool)> = alloc::vec::Vec::new();
        for &(pid, ..) in expired_sleeps.iter() {
            if let Some(p) = table.get(pid) {
                if p.state == ProcessState::Sleeping && p.wakeup_channel > 1 {
                    let d = p.wakeup_channel as u64;
                    if d > now {
                        rereg.push((pid, d, false));
                        if d < min_dl {
                            min_dl = d;
                        }
                    }
                }
            }
        }
        for &pid in expired_alarms.iter() {
            if let Some(p) = table.get(pid) {
                if p.alarm_deadline != 0 {
                    let a = p.alarm_deadline as u64;
                    if a > now {
                        rereg.push((pid, a, true));
                        if a < min_dl {
                            min_dl = a;
                        }
                    }
                }
                if p.state == ProcessState::Sleeping && p.wakeup_channel > 1 {
                    let d = p.wakeup_channel as u64;
                    if d > now {
                        rereg.push((pid, d, false));
                        if d < min_dl {
                            min_dl = d;
                        }
                    }
                }
            }
        }
        for (pid, d, is_alarm) in rereg {
            table.register_sleep_timer(pid, d, is_alarm);
        }
        // 最近到期点 = 树首键（涵盖所有登记源）与全扫所见的更早者。
        if let Some((&(first_dl, _), _)) = table.sleep_timers.iter().next() {
            if first_dl < min_dl {
                min_dl = first_dl;
            }
        }
        NEAREST_DEADLINE.store(min_dl, Ordering::Relaxed);
        if crate::RTDIAG {
            let us = crate::arch::asm_helpers::rdtime().saturating_sub(diag_w0) * 1_000_000
                / crate::arch::consts::CLOCK_FREQ as u64;
            crate::sched::process::rtstat_record(20, us);
        }
    }
}
