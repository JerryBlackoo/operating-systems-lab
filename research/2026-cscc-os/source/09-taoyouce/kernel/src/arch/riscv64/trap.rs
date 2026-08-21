//! 中断和异常处理
//!
//! 实现 RISC-V 的 trap 处理，包括：
//! - 系统调用 (ecall)
//! - 时钟中断
//! - 页错误
//! - 其他异常

#![allow(dead_code)]
#![allow(clippy::enum_clike_unportable_variant)]

use core::arch::global_asm;
use core::sync::atomic::{AtomicUsize, Ordering};
use super::trap_context::TrapContext;

// 引入中断处理汇编代码
global_asm!(include_str!("trap.S"));

// 引入 GDB 模块用于异常处理
use crate::gdb;

/// 中断/异常原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum TrapCause {
    // 中断 (最高位为 1)
    SupervisorSoftwareInterrupt = 1 | (1 << 63),
    SupervisorTimerInterrupt = 5 | (1 << 63),
    SupervisorExternalInterrupt = 9 | (1 << 63),

    // 异常 (最高位为 0)
    InstructionMisaligned = 0,
    InstructionAccessFault = 1,
    IllegalInstruction = 2,
    Breakpoint = 3,
    LoadMisaligned = 4,
    LoadAccessFault = 5,
    StoreMisaligned = 6,
    StoreAccessFault = 7,
    UserEcall = 8,
    SupervisorEcall = 9,
    InstructionPageFault = 12,
    LoadPageFault = 13,
    StorePageFault = 15,
}

impl TrapCause {
    /// 从 scause 值解析
    pub fn from_scause(scause: usize) -> Self {
        let is_interrupt = (scause >> 63) & 1 == 1;
        let code = scause & 0x7fff_ffff_ffff_ffff;

        if is_interrupt {
            match code {
                1 => Self::SupervisorSoftwareInterrupt,
                5 => Self::SupervisorTimerInterrupt,
                9 => Self::SupervisorExternalInterrupt,
                _ => panic!("Unknown interrupt: {}", code),
            }
        } else {
            match code {
                0 => Self::InstructionMisaligned,
                1 => Self::InstructionAccessFault,
                2 => Self::IllegalInstruction,
                3 => Self::Breakpoint,
                4 => Self::LoadMisaligned,
                5 => Self::LoadAccessFault,
                6 => Self::StoreMisaligned,
                7 => Self::StoreAccessFault,
                8 => Self::UserEcall,
                9 => Self::SupervisorEcall,
                12 => Self::InstructionPageFault,
                13 => Self::LoadPageFault,
                15 => Self::StorePageFault,
                _ => panic!("Unknown exception: {}", code),
            }
        }
    }

    /// 是否是中断
    pub fn is_interrupt(&self) -> bool {
        (*self as usize) >> 63 == 1
    }
}

/// 初始化中断处理
///
/// 设置 stvec 寄存器指向 trap 入口点
pub fn init() {
    extern "C" {
        fn __trap_entry();
    }

    unsafe {
        // 设置启动阶段内核态中断向量
        core::arch::asm!(
            "csrw stvec, {}",
            in(reg) __trap_entry as usize
        );

        // 启用 S 模式中断：SSIE(1) + STIE(5) + SEIE(9)
        let sie: usize = (1 << 1) | (1 << 5) | (1 << 9);
        core::arch::asm!("csrs sie, {}", in(reg) sie);
    }

    // 启动周期性定时器中断
    set_next_timer();
}

/// 通过 Sstc stimecmp CSR 设置下一次定时器中断
///
/// 使用 RISC-V Sstc 扩展直接写 stimecmp 寄存器 (CSR 0x14D)，
/// 无需经过 SBI ecall 的 S→M→S 模式切换开销。
/// 写入 stimecmp > time 时自动原子清除 STIP pending bit。
/// QEMU 10.0 virt 机器默认启用 Sstc 扩展。
fn set_next_timer() {
    const TICK_INTERVAL: u64 = super::consts::CLOCK_FREQ as u64 / 100; // 10ms
    let time: u64;
    unsafe { core::arch::asm!("rdtime {}", out(reg) time); }
    // 最近一次定时器中断到期点的最小提前量（~10µs）。用于「到期点已过去但其
    // 睡眠者尚未被唤醒」时——绝不能 arm 满一个 10ms 周期，否则该过期睡眠者要空等
    // 到下一个周期 tick 才被 check_sleep_deadlines 唤醒（cyclictest ~10ms Max 尖峰
    // 真因）。改 arm 到 time+MIN_AHEAD 立即重触发去服务它。
    const MIN_AHEAD: u64 = TICK_INTERVAL / 1000; // ~10µs
    let mut next = time + TICK_INTERVAL;

    // `nearest_deadline()` / `need_resched_pending()` 是**全局**状态，表达的是
    // 「某个正在执行任务的 hart」的唤醒/抢占需求。空闲 hart（本 hart 没有当前
    // 任务）若也据此把周期收窄到 ~10µs，每个空闲 hart 都会变成 ~100k 次/秒的
    // 定时器中断源；QEMU TCG 下每个 vCPU 是一个宿主线程，这会稀释真正在干活的
    // hart。Linux 的对应做法是空闲 CPU 进入 NOHZ_IDLE、停掉周期 tick；这里取
    // 最小等价行为：空闲 hart 只保留基础周期 tick，不参与收窄。
    let hart_has_task = !crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed)
        .is_null();

    // hrtimer 风格高精度到期：若有更近的定时睡眠 deadline（含已到期的），把下一次
    // 定时器中断收窄到它（过期者钳到 time+MIN_AHEAD 立即触发），使睡眠者精确/即时被
    // 唤醒，而非等满一个 10ms 周期 tick。周期 tick 仍由 now+TICK_INTERVAL 上界保证。
    let dl = if hart_has_task {
        crate::sched::process::nearest_deadline()
    } else {
        u64::MAX
    };
    if dl != u64::MAX && dl < next {
        next = dl.max(time + MIN_AHEAD);
    }
    // need_resched 待决但本次未能立即抢占（在内核态/idle 被唤醒 → 当时跳过抢占）：
    // 把定时器收窄到 ~MIN_AHEAD，使抢占点在 µs 级到来，而非空等满一个 10ms 周期
    // tick。否则被唤醒的高优先级 RT 任务变 Runnable 后（已不在 NEAREST 睡眠集合内）
    // 会因「当前任务在用户态长时间无 trap」而被饿到下个周期 tick（cyclictest 调度
    // 延迟 ~10ms 尖峰根因）。标志在返回用户态的抢占点被 take 清除，故这串收窄定时器
    // 只持续到抢占真正发生，对非实时负载零扰动。
    if hart_has_task
        && crate::sched::process::need_resched_pending()
        && next > time + MIN_AHEAD
    {
        next = time + MIN_AHEAD;
    }
    // 直接写 stimecmp CSR (0x14D)，原子清除 STIP
    unsafe {
        core::arch::asm!("csrw 0x14D, {}", in(reg) next);
    }
}

/// 立即按当前最近到期点重新 arm 定时器。入睡登记新 deadline 后调用，使该 deadline
/// 不必等到下一个周期 tick 才生效。
pub fn rearm_timer() {
    set_next_timer();
}
static CLDBG_TIMER_USER_COUNT: AtomicUsize = AtomicUsize::new(0);
static TICKDBG_TIMER_COUNT: AtomicUsize = AtomicUsize::new(0);

fn cldbg_enabled() -> bool {
    crate::cmdline::cldbg_enabled()
}

fn tickdbg_enabled() -> bool {
    crate::cmdline::tickdbg_enabled()
}

fn pfdbg_enabled() -> bool {
    crate::cmdline::pfdbg_enabled()
}


/// 新的 Trap 处理函数（从汇编 __alltraps 调用）
///
/// # 参数
/// - `cx`: TrapContext 指针，指向保存在内核栈上的用户上下文
///
/// # 返回
/// 返回 TrapContext 指针，用于 __restore 恢复上下文
/// trap 分类计数（无门控、单条 Relaxed fetch_add，用于回答"CPU 忙在哪类 trap"）。
///
/// `HUNGDBG` 只统计 syscall；当 `docker stats` 显示 CPU 满载而 syscall 极少时，
/// 时间必然耗在**不经过 syscall 的内核路径**上。分类计数是区分"页错误风暴 /
/// 定时器风暴 / IPI 风暴 / 正常执行"的最短路径。
pub static TRAP_ECALL: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static TRAP_PAGEFAULT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static TRAP_TIMER: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static TRAP_IPI: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static TRAP_EXT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static TRAP_OTHER: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// 读取 trap 分类计数 (ecall, pagefault, timer, ipi, ext, other)。
pub fn trap_counters() -> (u64, u64, u64, u64, u64, u64) {
    use core::sync::atomic::Ordering::Relaxed;
    (
        TRAP_ECALL.load(Relaxed),
        TRAP_PAGEFAULT.load(Relaxed),
        TRAP_TIMER.load(Relaxed),
        TRAP_IPI.load(Relaxed),
        TRAP_EXT.load(Relaxed),
        TRAP_OTHER.load(Relaxed),
    )
}

#[inline(always)]
fn count_trap(scause: usize) {
    // bootarg 门控：计数器只被 HUNGDBG 周期倒出消费（syscall/mod.rs 的
    // riscv64 分支），正式轮零成本——诊断必须门控的纪律（审计 C14）。
    if !crate::cmdline::hungdbg_enabled() {
        return;
    }
    use core::sync::atomic::Ordering::Relaxed;
    let is_int = scause & (1usize << 63) != 0;
    let code = scause & 0x7fff_ffff_ffff_ffff;
    let c = if is_int {
        match code {
            1 => &TRAP_IPI,
            5 => &TRAP_TIMER,
            9 => &TRAP_EXT,
            _ => &TRAP_OTHER,
        }
    } else {
        match code {
            8 => &TRAP_ECALL,
            12 | 13 | 15 => &TRAP_PAGEFAULT,
            _ => &TRAP_OTHER,
        }
    };
    c.fetch_add(1, Relaxed);
}

#[no_mangle]
pub extern "C" fn trap_handler_rust(cx: &mut TrapContext) -> &mut TrapContext {
    // 汇编入口已经切到当前任务的真实内核栈；直到此刻才发布栈归属，避免
    // `switch_to` 尚在旧任务栈上运行时过早解除旧栈的回收保护。
    let current_pid = crate::sched::process_sched::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed);
    crate::sched::process_sched::HART_KSTACK_OWNER
        .store(current_pid, core::sync::atomic::Ordering::SeqCst);

    let scause: usize;
    let stval: usize;

    unsafe {
        core::arch::asm!("csrr {}, scause", out(reg) scause);
        core::arch::asm!("csrr {}, stval", out(reg) stval);
    }
    count_trap(scause);

    // 进入内核时立即兑现挂起的跨核 TLB 失效。
    //
    // `shootdown_others` 是**同步等 ack** 的（Linux `smp_call_function(wait=1)`
    // 语义），而本内核在内核态全程关中断，IPI 无法及时送到正在做 syscall 的核；
    // 发起方就得一直自旋。这里把等待上限压到"目标核下一次进入内核"，与用户态
    // 直接由 IPI 兑现的路径合起来覆盖全部情形。
    // 快路径只读一个原子标志，未挂起时零额外成本。
    {
        let cpu = crate::mm::percpu_cache::current_cpu_id();
        let mgr = crate::mm::tlb::get_manager();
        if mgr
            .cpu_state(cpu)
            .map_or(false, |s| s.has_lazy_flush())
        {
            mgr.handle_shootdown_ipi(cpu);
        }
    }

    // 软 tick（对标 Linux 的 TIF 轮询兜底）：QEMU TCG 下 stimecmp 到期后，
    // host-timer 回调注入 STIP 可迟到数 ms～百 ms（vCPU 忙翻译执行时尤甚），
    // 期间已到期的定时睡眠者（cyclictest RT 线程）迟迟不被唤醒 = STRESS Max
    // ~10ms 尖峰真因。任何 user trap（hackbench 的 ecall 间隔 ~百 µs）入口
    // 发现最近到期点已过，就主动跑一次 timer_tick 服务到期唤醒并重新 arm，
    // 不再干等硬件中断注入。
    // 同步异常路径只读一次时钟：soft-tick 检查与下方 ecall 入账共用
    //（QEMU TCG 下 rdtime 是 io helper，每 syscall 少一次）。
    let mut now_cached: u64 = 0;
    if scause & (1 << 63) == 0 {
        // 无任何 armed deadline（nearest_deadline == u64::MAX）时比较永假，
        // 先查哨兵再读时钟，免掉稳态每 syscall 一次 rdtime io helper。
        let dl = crate::sched::process::nearest_deadline();
        if dl != u64::MAX {
            now_cached = crate::arch::asm_helpers::rdtime();
            if now_cached >= dl {
                crate::sched::timer_tick();
                set_next_timer();
            }
        }
    }

    // [KSTKEXEC] 铁证探针：若当前内核 sp 落在一段「已被 Drop 释放且尚未复用」的
    // 内核栈区间内，说明 CPU 正在一段已释放的内核栈上运行（内核栈 UAF）。
    if crate::mm::uaf_dbg_enabled() {
        kstkexec_probe(cx, scause);
    }

    let interrupted_user = (cx.sstatus & (1 << 8)) == 0;
    let cause = TrapCause::from_scause(scause);
    if interrupted_user {
        if matches!(cause, TrapCause::UserEcall) {
            // Syscall: 仅精确分账进程才读时钟并存 enter tick（见
            // current_cputime_precise）；其余进程 syscall 热路径零 rdtime。
            if crate::sched::process_sched::current_cputime_precise() {
                let now = if now_cached != 0 {
                    now_cached
                } else {
                    crate::arch::asm_helpers::rdtime()
                };
                crate::sched::process::HART_KERNEL_ENTER_TICK
                    .store(now, core::sync::atomic::Ordering::Relaxed);
            }
        } else {
            let now = if now_cached != 0 {
                now_cached
            } else {
                crate::arch::asm_helpers::rdtime()
            };
            crate::sched::process::account_enter_kernel(now);
        }
        crate::sched::process::reap_detached_zombies();
    }

    match cause {
        TrapCause::UserEcall => {
            // sepc 指向 ecall 指令，需要 +4 跳过它
            // 真化〔seccomp pc 透传〕：原 ecall 指令地址 = sepc - 4，
            // 用于 seccomp_data.instruction_pointer（cBPF 程序可 BPF_LD|BPF_ABS
            // 偏移 8 读出 PC，过滤特定调用点）。
            let ecall_pc = cx.sepc;
            cx.sepc += 4;

            let syscall_id = cx.syscall_id();
            let args = cx.syscall_args();

            let result = crate::syscall::syscall(syscall_id, args, ecall_pc);

            if result == crate::syscall::SYSCALL_YIELD_SENTINEL {
                cx.x[10] = 0;
                if let Some((trap_cx_ptr, satp, deliver_signal)) =
                    crate::sched::process::switch_current_to_next(cx, crate::sched::trace::SWITCH_YIELD)
                {
                    let new_cx = unsafe { &mut *trap_cx_ptr };
                    if deliver_signal {
                        crate::syscall::check_and_deliver_signals(new_cx);
                    }
                    prepare_switch_context(new_cx, satp);
                    return new_cx;
                }
            } else {
                cx.x[10] = result as usize;
                if let Some((trap_cx_ptr, satp, deliver_signal)) =
                    crate::sched::process::switch_thread_startup_to_leader(
                        cx,
                        syscall_id,
                        crate::sched::trace::SWITCH_YIELD,
                    )
                {
                    let new_cx = unsafe { &mut *trap_cx_ptr };
                    if deliver_signal {
                        crate::syscall::check_and_deliver_signals(new_cx);
                    }
                    prepare_switch_context(new_cx, satp);
                    return new_cx;
                }
                // child-runs-first 让位由 trap 出口的公共 handoff_child_for_trap
                // 调用点统一处理（免每 syscall 双调用）。
            }
        }
        TrapCause::SupervisorTimerInterrupt => {
            handle_timer_trap(cx);
        }
        TrapCause::SupervisorSoftwareInterrupt => {
            // 软件中断（用于 IPI）
            // 清除软件中断
            unsafe {
                core::arch::asm!("csrc sip, {}", in(reg) 1 << 1);
            }
            // TLB shootdown：SBI IPI 不携带 payload，故以「本核是否有挂起的
            // shootdown」区分用途。`sfence.vma` 只作用于本 hart，发起方的本地
            // 失效对其他 hart 无效；不在这里真正失效，另一 hart 上运行同一 mm
            // 的线程会继续用陈旧条目访问已被 CoW/换页替换的旧物理页。
            // 对标 Linux `flush_tlb_func`。
            let cpu_id = crate::mm::percpu_cache::current_cpu_id();
            let manager = crate::mm::tlb::get_manager();
            let pending = manager
                .cpu_state(cpu_id)
                .map(|state| state.has_unacked_flush())
                .unwrap_or(false);
            if pending {
                manager.handle_shootdown_ipi(cpu_id);
            }
            crate::sched::process::set_need_resched();
        }
        TrapCause::SupervisorExternalInterrupt => {
            // 外部中断（如 VirtIO 设备）
            // 默认记录为 IRQ 9（S-mode external interrupt）
            crate::sched::record_interrupt(9);
            crate::net::net_stack::on_net_interrupt();
        }
        TrapCause::Breakpoint => {
            // 断点异常
            cx.sepc = handle_breakpoint(cx.sepc, stval);
        }
        TrapCause::IllegalInstruction => {
            // 非法指令
            cx.sepc = handle_illegal_instruction(cx.sepc, stval, interrupted_user);
        }
        TrapCause::LoadPageFault | TrapCause::StorePageFault | TrapCause::InstructionPageFault => {
            handle_page_fault_trap(cx, cause, stval, scause);
        }
        _ => {
            handle_unknown_trap(cx, cause, stval, scause);
        }
    }

    if interrupted_user {
        // 返回用户态前检查并投递信号。syscall 路径上 account_syscall_combined_exit
        // 刚在 PROCESS_TABLE 锁内刷新了 HART_PENDING_SIGNALS（= current 的
        // pending_signals & !signal_mask），且 S-mode trap 期间 SIE 已关、其间无新
        // 信号投递窗口；故缓存为 0 时可 lock-free 跳过 check_and_deliver_signals 的
        // 全局锁（§1.13①，省每 syscall 一把锁）。其它 trap（timer/fault 等）可能在
        // 不刷新缓存的情况下投了信号，故仍无条件检查。
        let skip_signal_check = matches!(cause, TrapCause::UserEcall)
            && crate::sched::process::HART_PENDING_SIGNALS
                .load(core::sync::atomic::Ordering::Relaxed)
                == 0;
        if !skip_signal_check {
            crate::syscall::check_and_deliver_signals(cx);
        }

        // 定时器抢占调度：轮转选择下一个 Runnable 进程（同组优先，其次跨 tgid）。
        // need_resched：若在本任务处于内核态期间有 RT/DEADLINE 任务被定时器唤醒，
        // 当时 interrupted_user==false 抢占被跳过；在此返回用户态的抢占点据标志补上，
        // 否则被唤醒的高优先级任务要空等到下一个周期 tick（cyclictest Max 尖峰根因）。
        // child-runs-first: clone 后立即让出给子线程。须在 need_resched 抢占之前
        // 判定——刚 clone 完的父进程若先被普通抢占选走他人，子线程首跑被推迟
        // （b_malloc_thread_stress icount 实测 −2.5%）。
        if let Some((trap_cx_ptr, satp, deliver_signal)) =
            crate::sched::process::handoff_child_for_trap(cx, crate::sched::trace::SWITCH_YIELD)
        {
            let new_cx = unsafe { &mut *trap_cx_ptr };
            if deliver_signal {
                crate::syscall::check_and_deliver_signals(new_cx);
            }
            prepare_switch_context(new_cx, satp);
            return new_cx;
        }

        let need_resched = crate::sched::process::take_need_resched();
        if matches!(cause, TrapCause::SupervisorTimerInterrupt) || need_resched {
            if cldbg_enabled() {
                cldbg_timer_user(cx);
            }
            if let Some((trap_cx_ptr, satp, deliver_signal)) =
                crate::sched::process::preempt_current_for_trap(cx, true, crate::sched::trace::SWITCH_TIMER)
            {
                let new_cx = unsafe { &mut *trap_cx_ptr };
                if deliver_signal {
                    crate::syscall::check_and_deliver_signals(new_cx);
                }
                prepare_switch_context(new_cx, satp);
                return new_cx;
            }
        }

    }

    cx
}
#[inline(never)]
fn kstkexec_probe(cx: &TrapContext, scause: usize) {
    let sp: usize;
    unsafe {
        core::arch::asm!("mv {}, sp", out(reg) sp);
    }
    if let Some((lo, hi)) = crate::mm::kstk_running_on_freed_dbg(sp) {
        crate::println!(
            "[KSTKEXEC] kernel running on FREED stack: sp={:#x} in freed kstack [{:#x},{:#x}) scause={:#x} sepc={:#x}",
            sp, lo, hi, scause, cx.sepc
        );
    }
}

#[inline(never)]
fn cldbg_timer_user(cx: &TrapContext) {
    if CLDBG_TIMER_USER_COUNT.fetch_add(1, Ordering::Relaxed) < 64 {
        crate::println!(
            "[cldbg] timer.user sepc={:#x} sstatus={:#x} sp={:#x} tp={:#x}",
            cx.sepc,
            cx.sstatus,
            cx.sp(),
            cx.x[4]
        );
    }
}

/// 定时器中断处理（syscall 热路径外提取，降低 trap_handler_rust 寄存器压力）。
#[inline(never)]
fn handle_timer_trap(cx: &mut TrapContext) {
    if tickdbg_enabled() {
        let n = TICKDBG_TIMER_COUNT.fetch_add(1, Ordering::Relaxed);
        if n % 10 == 0 {
            crate::println!("[tickdbg] n={} sepc={:#x} a7={:#x} sp={:#x}", n, cx.sepc, cx.x[17], cx.x[2]);
        }
    }
    crate::sched::record_interrupt(5); // RISC-V timer IRQ = 5
    // 先做调度 tick（其中 check_sleep_deadlines 会重算最近到期点），
    // 再据最新到期点 arm 下一次定时器中断（hrtimer 精确到期）。
    crate::sched::timer_tick();
    crate::net::net_stack::on_timer_tick();
    set_next_timer();
}

#[inline(never)]
fn handle_unknown_trap(cx: &TrapContext, cause: TrapCause, stval: usize, scause: usize) -> ! {
    crate::println!(
        "[trap] Unhandled trap: {:?}, sepc={:#x}, stval={:#x}",
        cause, cx.sepc, stval
    );
    handle_gdb_exception(scause as u64, cx.sepc);
    panic!("Unhandled trap");
}

/// 页错误 trap 处理（冷路径提取：让 trap_handler_rust 的 syscall 热路径少背
/// 这段大体量代码的寄存器压力/栈帧成本）。
#[inline(never)]
fn handle_page_fault_trap(cx: &mut TrapContext, cause: TrapCause, stval: usize, scause: usize) {
    // Two-phase page fault handling (matches Linux mmap_lock protocol):
    //   Phase 1: Under PROCESS_TABLE lock — resolve non-IO faults or
    //            extract metadata for file-backed demand paging.
    //   Phase 2: OUTSIDE lock — perform file I/O + readahead.
    //   Phase 3: Re-acquire lock — install the populated page(s).
    let is_write = cause == TrapCause::StorePageFault;
    let fault_addr = crate::mm::VirtAddr(stval);
    if pfdbg_enabled() {
        crate::println!("[pfdbg] enter sepc={:#x} stval={:#x} write={} inst={}", cx.sepc, stval, is_write, cause == TrapCause::InstructionPageFault);
    }

    // Phase 1: try to resolve under lock; extract file info if needed
    let (handled, fault_sig, needs_tlb_drain) = {
        use crate::sched::process::PROCESS_TABLE;
        use crate::mm::address_space::FaultAction;

        // --- Phase 1 ---
        let (action, as_pid_out) = {
            // PT 只解析 pid/mm 身份（对标 Linux fault 路径只持 mmap_lock、不持
            // tasklist 锁）：与 LA 同款收窄——缺页风暴下持 PT 等 mm 锁实测
            // 单次 100ms、全体 syscall 被征 6-30ms 税（ftrace-200501 证据）。
            let (mm_arc, as_pid) = {
                let mut table = PROCESS_TABLE.lock();
                let r = if let Some(table) = table.as_mut() {
                    if let Some(current_pid) = table.current {
                        let as_pid = table.get(current_pid)
                            .map(|p| if p.mm.is_some() { current_pid } else { p.tgid })
                            .unwrap_or(current_pid);
                        (table.get(as_pid).and_then(|p| p.mm.as_ref()).cloned(), as_pid)
                    } else {
                        (None, 0)
                    }
                } else {
                    (None, 0)
                };
                r
                // PROCESS_TABLE lock released here
            };
            match mm_arc {
                Some(mm) => {
                    let mut addr_space = mm.lock();
                    (addr_space.try_resolve_fault(fault_addr, is_write), as_pid)
                }
                None => (FaultAction::Failed(11), as_pid),
            }
        };

        let __res = match action {
            FaultAction::Handled => (true, 11, false),
            FaultAction::HandledNeedsTlbDrain => (true, 11, true),
            FaultAction::Failed(sig) => (false, sig, false),
            FaultAction::NeedFileRead(info) => {
                // --- Phase 2: File I/O outside lock ---
                use alloc::sync::Arc;
                use crate::mm::page_cache;
                use crate::mm::frame_alloc_uninit;

                let page_index = info.file_offset / crate::mm::PAGE_SIZE;

                // Check page cache first (equivalent to filemap_fault)
                let cached = page_cache::lookup(info.inode_id, page_index);
                let cache_hit_dbg = cached.is_some();
                let fault_frame = if let Some(cached_frame) = cached {
                    // MAP_SHARED 或 MAP_PRIVATE 只读/执行段（PTE 无 W）直接
                    // 共享页缓存帧：不可写页面永远不会被用户写，无需为每个
                    // rustc 进程复制一遍代码/只读数据段（Linux filemap_fault
                    // 对 MAP_PRIVATE 也是先映射 page cache 页、写时再 COW）。
                    if info.is_shared || !info.pte_flags.contains(crate::mm::PTEFlags::W) {
                        Some(cached_frame)
                    } else {
                        // MAP_PRIVATE writable: copy from cache to new private frame
                        if let Some(private) = frame_alloc_uninit() {
                            let src = cached_frame.ppn.as_bytes_mut();
                            let dst = private.ppn.as_bytes_mut();
                            dst.copy_from_slice(src);
                            Some(crate::mm::SharedFrame::from_alloc(private))
                        } else {
                            None
                        }
                    }
                } else {
                    // Page cache miss: read from file + readahead
                    if let Some(file) = info.inode.as_file() {
                        // Readahead: read up to 64 pages into page cache
                        let ra_count = {
                            let remaining_in_vma = (info.vma_end.0 - info.vma_start.0) / crate::mm::PAGE_SIZE;
                            let pages_from_fault = remaining_in_vma.saturating_sub(
                                (info.file_offset - info.vma_file_offset) / crate::mm::PAGE_SIZE
                            );
                            pages_from_fault.min(page_cache::DEFAULT_READAHEAD_PAGES)
                        };
                        page_cache::readahead(
                            info.inode_id,
                            page_index,
                            ra_count,
                            info.file_size,
                            |off, buf| {
                                match file.read(off, buf) {
                                    Ok(n) => n,
                                    Err(_) => 0,
                                }
                            },
                        );
                        // Now the faulting page should be in cache
                        let cached = page_cache::lookup(info.inode_id, page_index);
                        if let Some(cached_frame) = cached {
                            if info.is_shared || !info.pte_flags.contains(crate::mm::PTEFlags::W) {
                                // Shared mapping / read-only private mapping:
                                // use the cached frame directly (see above).
                                Some(cached_frame)
                            } else {
                                if let Some(private) = frame_alloc_uninit() {
                                    let src = cached_frame.ppn.as_bytes_mut();
                                    let dst = private.ppn.as_bytes_mut();
                                    dst.copy_from_slice(src);
                                    Some(crate::mm::SharedFrame::from_alloc(private))
                                } else {
                                    None
                                }
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };

                // [ZCODE] forensic probe (UAFDBG-gated): catch demand-paged executable
                // pages whose content is all-zero at install time — pinpoints the
                // "mapped but never filled / filled-then-zeroed" code-page corruption.
                if crate::mm::uaf_dbg_enabled() {
                    if let Some(ref f) = fault_frame {
                        let is_x = info.pte_flags.bits() & 0x8 != 0;
                        if is_x {
                            let bytes = f.ppn.as_bytes_mut();
                            let first = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                            if first == 0 {
                                crate::println!(
                                    "[ZCODE] as_pid={} inode={} pidx={} vpn={:#x} foff={:#x} fsize={:#x} cache_hit={} shared={} first={:#x}",
                                    as_pid_out, info.inode_id, page_index, info.vpn.0,
                                    info.file_offset, info.file_size, cache_hit_dbg, info.is_shared, first
                                );
                            }
                        }
                    }
                }

                // --- Phase 3: 重取 mm Arc（PT 只解析身份）后装页；不持 PT 等
                // mm 锁（与 Phase 1 同理由，LA 同款收窄）。
                if let Some(frame) = fault_frame {
                    let mm2 = {
                        let table = PROCESS_TABLE.lock();
                        table.as_ref()
                            .and_then(|t| t.get(as_pid_out))
                            .and_then(|p| p.mm.as_ref())
                            .cloned()
                    };
                    if let Some(mm) = mm2 {
                        let mut addr_space = mm.lock();
                        let ok = addr_space.install_demand_page(
                            info.vpn,
                            frame,
                            info.vma_idx,
                            info.pte_flags,
                            info.vma_start,
                        );
                        // Also install readahead pages
                        if ok {
                            let ra_start = page_index + 1;
                            let ra_count = {
                                let remaining = (info.vma_end.0 - info.vma_start.0) / crate::mm::PAGE_SIZE;
                                let from_fault = remaining.saturating_sub(
                                    (info.file_offset - info.vma_file_offset) / crate::mm::PAGE_SIZE + 1
                                );
                                from_fault.min(page_cache::DEFAULT_READAHEAD_PAGES.saturating_sub(1))
                            };
                            if ra_count > 0 {
                                addr_space.install_readahead_pages(
                                    info.inode_id,
                                    info.vma_idx,
                                    info.vma_start,
                                    info.vma_end,
                                    info.vma_file_offset,
                                    info.pte_flags,
                                    ra_start,
                                    ra_count,
                                    info.is_shared,
                                );
                            }
                        }
                        (ok, 11, false)
                    } else {
                        (false, 11, false)
                    }
                } else {
                    (false, 11, false) // frame_alloc failed
                }
            }
        };
        // cgroup v1 memory：缺页解析完成、PROCESS_TABLE 与地址空间锁均已
        // 释放后，做 memcg 计账 + 压力回收（域内 RSS 超 limit 则回收）。
        // 快路径（无有限上限域）仅一次原子读即返回。
        if __res.0 {
            crate::mm::memcg::account_and_reclaim(as_pid_out);
        }
        __res
    };

    if pfdbg_enabled() {
        crate::println!("[pfdbg] result handled={} sig={}", handled, fault_sig);
    }
    if handled && needs_tlb_drain {
        // CoW 强序点，且此处 PROCESS_TABLE / AddressSpace 锁均已释放。
        // 同时回收本进程 quarantine（CoW 摘除的旧帧 drain 后才归还 buddy）。
        crate::mm::address_space::AddressSpace::drain_and_reap_current_mm();
    }
    if !handled {
        // 无法处理的页错误
        // 检查是否是用户态页错误，如果是则终止进程而不是 panic
        let is_user_mode = (cx.sstatus & (1 << 8)) == 0; // SPP bit

        if is_user_mode || (stval < 0x8000_0000) {
            // A bad memory access raises SIGSEGV (or SIGBUS for a file
            // mapping page past EOF — fault_sig). If the task installed a
            // (catchable, unblocked) handler, queue it and let the
            // trap-return path build the handler frame so userspace can
            // recover (mprotect04 catches SIGSEGV and siglongjmps; mmap13
            // catches SIGBUS). Otherwise terminate *by signal*.
            // si_code matches Linux: a file-backed page past EOF is a
            // bus error (BUS_ADRERR); otherwise a bad access inside a
            // mapped region is a protection violation (SEGV_ACCERR) and
            // one to an unmapped address is SEGV_MAPERR.
            let si_code = if fault_sig == 7 {
                3 // BUS_ADRERR
            } else if crate::mm::current_user_vma_present(stval as usize) {
                2 // SEGV_ACCERR
            } else {
                1 // SEGV_MAPERR
            };
            if !crate::syscall::signal::try_queue_fault_signal(fault_sig, stval, si_code) {
                // BSCRASH=1：致命用户段错误的一次性取证（BuildStorm rustc
                // exit=139 定位）。argv0 直接指认崩溃的二进制；sepc/stval/
                // si_code 区分坏跳转、坏数据指针与真越界。
                if crate::cmdline::bscrash_enabled() {
                    use core::sync::atomic::Ordering;
                    let pcb = crate::sched::process_sched::HART_CURRENT_PCB
                        .load(Ordering::Relaxed);
                    let (pid, tgid, argv0) = if pcb.is_null() {
                        (0, 0, alloc::string::String::new())
                    } else {
                        let p = unsafe { &*pcb };
                        (
                            p.pid,
                            p.tgid,
                            p.saved_argv.first().cloned().unwrap_or_default(),
                        )
                    };
                    crate::println!(
                        "[BSCRASH-RV] fatal sig={} si_code={} pid={} tgid={} argv0={} sepc={:#x} stval={:#x} sp={:#x} ra={:#x} sstatus={:#x}",
                        fault_sig, si_code, pid, tgid, argv0,
                        cx.sepc, stval, cx.x[2], cx.x[1], cx.sstatus
                    );
                }
                crate::sched::process::exit_group_with_reason(
                    crate::sched::process::ExitReason::signaled_with_dump(fault_sig),
                );
            }
        } else {
            // 内核态页错误，打印调试信息并 panic。
            // SMP 取证：同时打印本 hart、本 hart 的 current、故障时的内核 sp 及
            // 该 sp 归属哪个进程的内核栈。多核下最常见的成因是"一个 hart 释放/
            // 复用了另一个 hart 上正在运行的线程的内核栈"，这三项信息足以当场
            // 分辨是栈归属错乱、栈已被回收，还是纯粹的空指针访问。
            let hart = crate::arch::asm_helpers::read_cpu_id();
            let cur = crate::sched::process_sched::HART_CURRENT_PID
                .load(core::sync::atomic::Ordering::Relaxed);
            let ksp: usize;
            unsafe { core::arch::asm!("mv {}, sp", out(reg) ksp) };
            crate::println!(
                "[trap] Kernel page fault at {:#x}, accessing {:#x}, cause={:?} hart={} current={} ksp={:#x}",
                cx.sepc, stval, cause, hart, cur, ksp
            );
            if let Some(table) = crate::sched::process::PROCESS_TABLE.try_lock() {
                if let Some(t) = table.as_ref() {
                    t.kstk_owner_scan_dbg(ksp, cur);
                }
            } else {
                crate::println!("[trap] (PROCESS_TABLE busy: kstack owner scan skipped)");
            }
            handle_gdb_exception(scause as u64, cx.sepc);
            panic!("Unhandled kernel page fault");
        }
    }
    // 页错误已处理，继续执行
}


/// 旧的中断处理入口（保持兼容性）
#[no_mangle]
pub extern "C" fn trap_handler(
    scause: usize,
    stval: usize,
    sepc: usize,
) -> usize {
    let cause = TrapCause::from_scause(scause);

    match cause {
        TrapCause::SupervisorTimerInterrupt => {
            if crate::mm::uaf_dbg_enabled() {
                let sp: usize;
                unsafe { core::arch::asm!("mv {}, sp", out(reg) sp); }
                if let Some(table) = crate::sched::process::PROCESS_TABLE.try_lock() {
                    if let Some(t) = table.as_ref() {
                        t.ksp_on_user_frame_dbg(sp, sepc);
                    }
                }
            }
            crate::sched::record_interrupt(5); // RISC-V timer IRQ = 5
            // 这里曾有一段单核时代的 [K-TIMER-DEADLOCK] 检测（`PROCESS_TABLE`
            // 已锁 → 打印告警）。多核下它既无意义（另一个 hart 正常持锁是常态）
            // 又有害：判断本身要抢锁/写串口，等于把粗粒度全局锁引入中断路径。
            // GDB 全 vCPU 取证（rv-smpgdb-20260811T021540Z.gdb.txt）显示 7 个 hart
            // 同时卡在 `trap_handler -> num_online -> SpinLock::lock`，而 BSP 正持
            // 同一把 `SMP_MANAGER` 锁停在 `boot_ap`。中断路径只做记账 + tick。
            crate::sched::timer_tick();
            crate::net::net_stack::on_timer_tick();
            // 重算最近到期点后再 arm（hrtimer 精确到期）。
            set_next_timer();

            sepc
        }
        TrapCause::SupervisorSoftwareInterrupt => {
            // IPI（reschedule / TLB shootdown）也会在**内核态**打到本 hart：
            // idle 循环的 wfi 窗口、以及组退出时被 `kick_hart` 踢的目标 hart 都
            // 处于内核态。缺这个分支会掉进下面的 `_ =>` panic
            // （观测：`[trap] Unhandled trap` 紧随 init 启动）。
            // 处理与用户态入口一致：清 SSIP，若本核有挂起的 shootdown 就执行。
            unsafe {
                core::arch::asm!("csrc sip, {}", in(reg) 1 << 1);
            }
            let cpu_id = crate::mm::percpu_cache::current_cpu_id();
            let manager = crate::mm::tlb::get_manager();
            let pending = manager
                .cpu_state(cpu_id)
                .map(|state| state.has_lazy_flush())
                .unwrap_or(false);
            if pending {
                manager.handle_shootdown_ipi(cpu_id);
            }
            crate::sched::process::set_need_resched();
            sepc
        }
        TrapCause::SupervisorExternalInterrupt => {
            // 外部中断（virtio blk/net）在内核态也会到达（AP 的 idle wfi 窗口）。
            // 与用户态入口一致：记账 + 推进网络栈轮询。
            crate::sched::record_interrupt(9);
            crate::net::net_stack::on_net_interrupt();
            sepc
        }
        TrapCause::UserEcall => {
            // 系统调用 - 跳过 ecall 指令
            sepc + 4
        }
        TrapCause::Breakpoint => {
            handle_breakpoint(sepc, stval)
        }
        TrapCause::IllegalInstruction => {
            handle_illegal_instruction(sepc, stval, (get_sstatus() & (1 << 8)) == 0)
        }
        TrapCause::LoadPageFault | TrapCause::StorePageFault => {
            // 这是**内核态** trap 入口（stvec = __trap_entry）。SMP 取证：一次打印
            // 全部判据——本 hart、SPP（真的是内核态吗）、本 hart 的 current、故障
            // 时的内核 sp，以及该 sp 属于哪个进程的内核栈。多核下最常见成因是
            // "另一个 hart 回收/复用了本 hart 正在用的内核栈"。
            let hart = crate::arch::asm_helpers::read_cpu_id();
            let spp = (get_sstatus() >> 8) & 1;
            let cur = crate::sched::process_sched::HART_CURRENT_PID
                .load(core::sync::atomic::Ordering::Relaxed);
            let ksp: usize;
            unsafe { core::arch::asm!("mv {}, sp", out(reg) ksp) };
            {
                use core::sync::atomic::Ordering;
                if KTRAP_CAUSE.load(Ordering::Relaxed) == 0 {
                    KTRAP_EPC.store(sepc, Ordering::Relaxed);
                    KTRAP_TVAL.store(stval, Ordering::Relaxed);
                    KTRAP_HART.store(hart, Ordering::Relaxed);
                    KTRAP_SP.store(ksp, Ordering::Relaxed);
                    KTRAP_PID.store(cur, Ordering::Relaxed);
                    KTRAP_CAUSE.store(scause, Ordering::Release);
                }
            }
            crate::println!(
                "[trap] KPF hart={} spp={} cur={} sepc={:#x} stval={:#x} scause={:#x} ksp={:#x}",
                hart, spp, cur, sepc, stval, scause, ksp
            );
            if let Some(table) = crate::sched::process::PROCESS_TABLE.try_lock() {
                if let Some(t) = table.as_ref() {
                    t.kstk_owner_scan_dbg(ksp, cur);
                }
            }
            handle_gdb_exception(scause as u64, sepc);
            panic!("Unhandled page fault");
        }
        _ => {
            // 免锁锚点先行：println 可能在 UART 锁内重入死锁（rel-rv-compile
            // 两轮均定格在 "[trap] Unhandled trap: " 半行），cause 全被吞。
            // 先把三个 CSR + hart 存进静态原子，冻结后 GDB 直读
            // KTRAP_{CAUSE,EPC,TVAL,HART} 即可拿到第一现场。
            let cur = crate::sched::process_sched::HART_CURRENT_PID
                .load(core::sync::atomic::Ordering::Relaxed);
            let ksp: usize;
            unsafe { core::arch::asm!("mv {}, sp", out(reg) ksp) };
            {
                use core::sync::atomic::Ordering;
                let hart = crate::arch::asm_helpers::read_cpu_id();
                if KTRAP_CAUSE.load(Ordering::Relaxed) == 0 {
                    KTRAP_EPC.store(sepc, Ordering::Relaxed);
                    KTRAP_TVAL.store(stval, Ordering::Relaxed);
                    KTRAP_HART.store(hart, Ordering::Relaxed);
                    KTRAP_SP.store(ksp, Ordering::Relaxed);
                    KTRAP_PID.store(cur, Ordering::Relaxed);
                    KTRAP_CAUSE.store(scause, Ordering::Release);
                }
            }
            crate::println!(
                "[trap] Unhandled trap: {:?}, sepc={:#x}, stval={:#x} hart={} cur={} ksp={:#x}",
                cause, sepc, stval, crate::arch::asm_helpers::read_cpu_id(), cur, ksp
            );
            handle_gdb_exception(scause as u64, sepc);
            panic!("Unhandled trap");
        }
    }
}

/// rel-rv-compile 冻结取证：首个 unknown kernel trap 的无锁快照（GDB 直读）。
#[no_mangle]
pub static KTRAP_CAUSE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[no_mangle]
pub static KTRAP_EPC: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[no_mangle]
pub static KTRAP_TVAL: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
#[no_mangle]
pub static KTRAP_HART: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
pub static KTRAP_SP: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);
pub static KTRAP_PID: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// 处理断点异常
fn handle_breakpoint(sepc: usize, _stval: usize) -> usize {
    if crate::config::is_gdb_enabled() {
        if let Some(stub) = gdb::get_stub() {
            if stub.is_connected() {
                stub.breakpoint_hit(sepc as u64);
                crate::println!("[gdb] Breakpoint hit at {:#x}", sepc);
                return sepc;
            }
        }
    }

    sepc + 4
}

/// 处理非法指令异常
///
/// 用户态垃圾/非法指令对标 Linux：发 SIGILL(ILL_ILLOPC)。若进程装了 handler
/// 则投递让其恢复（如 crashme 捕获 SIGILL 后 siglongjmp 继续下一 trip），否则
/// 按信号终止该进程——绝不因用户态非法指令 panic 整个内核。内核态非法指令才
/// 视为致命，dump 后 panic。
fn handle_illegal_instruction(sepc: usize, stval: usize, is_user: bool) -> usize {
    let insn = unsafe { core::ptr::read_volatile(sepc as *const u32) };
    // ebreak / c.ebreak：交给断点处理（gdb / __builtin_trap）
    if insn == 0x0010_0073 {
        return handle_breakpoint(sepc, stval);
    }
    if (insn as u16) == 0x9002 {
        if crate::config::is_gdb_enabled() {
            if let Some(stub) = gdb::get_stub() {
                if stub.is_connected() {
                    stub.breakpoint_hit(sepc as u64);
                    return sepc;
                }
            }
        }
        return sepc + 2;
    }
    // 用户态：投递 SIGILL（装了 handler 则恢复，否则按信号终止），绝不 panic 内核。
    if is_user {
        if !crate::syscall::signal::try_queue_fault_signal(4, sepc, 1) {
            crate::sched::process::exit_group_with_reason(
                crate::sched::process::ExitReason::signaled_with_dump(4),
            );
        }
        return sepc;
    }

    crate::println!(
        "[trap] Illegal instruction at {:#x}: {:#010x}",
        sepc, insn
    );

    handle_gdb_exception(2, sepc);
    panic!("Illegal instruction");
}

/// 通知 GDB 发生异常
fn handle_gdb_exception(exception_code: u64, _sepc: usize) {
    if crate::config::is_gdb_enabled() {
        if let Some(stub) = gdb::get_stub() {
            if stub.is_connected() {
                stub.exception(exception_code);
            }
        }
    }
}

/// 初始化从核的中断处理
pub fn init_secondary() {
    extern "C" {
        fn __trap_entry();
    }

    unsafe {
        core::arch::asm!(
            "csrw stvec, {}",
            in(reg) __trap_entry as usize,
        );

        // 启用 S 模式中断
        let sie: usize = (1 << 1) | (1 << 5) | (1 << 9);
        core::arch::asm!(
            "csrs sie, {}",
            in(reg) sie,
        );

        // 启用全局中断
        core::arch::asm!("csrsi sstatus, 2");
    }

    // arm 本 hart 的周期定时器。AP 现在真正参与执行用户任务：idle 时的 `wfi`
    // 必须能被本 hart 的 timer 唤醒去重新查全局就绪队列，执行任务时 tick 提供
    // 抢占点。stimecmp 是 per-hart CSR，BSP 的 arm 不覆盖 AP。
    set_next_timer();
}

/// 跳转到用户态执行
///
/// 设置好 TrapContext 后，调用此函数跳转到用户程序。
///
/// # Safety
/// 调用者必须确保 TrapContext 已正确初始化。
pub unsafe fn run_user(cx: &TrapContext) -> ! {
    extern "C" {
        fn __commit_and_restore(cx: *const TrapContext) -> !;
    }

    // 设置 sscratch 为用户栈指针
    core::arch::asm!(
        "csrw sscratch, {}",
        in(reg) cx.sp(),
    );

    __commit_and_restore(cx as *const TrapContext)
}

/// 获取当前 sstatus 寄存器值
pub fn get_sstatus() -> usize {
    let sstatus: usize;
    unsafe {
        core::arch::asm!("csrr {}, sstatus", out(reg) sstatus);
    }
    sstatus
}

/// 为从 trap_handler_rust 内部的非发散上下文切换准备 TrapContext。
///
/// 与 `usertrapret -> !` 不同，此函数不调用 `__restore`，而是：
/// 1. 结账内核时间
/// 2. 设置 stvec 为 __alltraps
/// 3. 设置 TrapContext.sstatus: SPP=0, SPIE=1
/// 4. 切换到用户页表
///
/// 调用方随后 `return new_cx`，通过正常的 __alltraps → __restore 路径返回用户态。
/// 这避免了 `usertrapret -> !` 的发散跳转，消除了与 Heisenbug 相关的
/// 控制流/栈帧异常。
fn prepare_switch_context(new_cx: &mut TrapContext, user_satp: usize) {
    extern "C" { fn __trap_entry(); }
    let (_, return_pcb, _) = crate::sched::process_sched::user_return_target();

    // switch_to 仍在 previous 栈上时只暂存 next；此处已是最终返回用户态的
    // 非发散出口，先结清旧 mm，再提交 next 的 current/active_mm。
    crate::mm::tlb::complete_current_pending_flush();
    let root_ppn = user_satp as u64 & 0xFFF_FFFF_FFFF;
    let reentering_mm =
        root_ppn != 0 && crate::mm::tlb::note_active_mm(root_ppn) != root_ppn;

    // 内核 → 用户分账
    let now = crate::arch::asm_helpers::rdtime();
    crate::sched::process_sched::account_exit_kernel_for(return_pcb, now);

    unsafe {
        // 1. 关闭中断（与 usertrapret 一致，防止 __restore 前被 timer 打断）
        core::arch::asm!("csrc sstatus, {}", in(reg) 1usize << 1);

        // 2. finalizer 之前保持内核态 trap 入口（__trap_entry）；真正的
        // 用户态入口在 __commit_and_restore 完成收尾后由汇编切回。
        core::arch::asm!("csrw stvec, {}", in(reg) __trap_entry as usize);
    }

    // 3. 设置 trapframe 中的内核信息（__alltraps 需要这些来正确重入内核）
    new_cx.kernel_satp = kernel_satp();
    new_cx.trap_handler = trap_handler_rust as usize;

    // SPP=0 (User mode), SPIE=1 (返回后启用中断), SIE=0
    let mut sstatus: usize;
    unsafe { core::arch::asm!("csrr {}, sstatus", out(reg) sstatus); }
    sstatus &= !(1usize << 8); // SPP=0
    sstatus &= !(1usize << 1); // SIE=0
    sstatus |= 1usize << 5;    // SPIE=1
    new_cx.sstatus = sstatus;

    // 切换到用户页表。仅当目标地址空间与当前 satp 不同（= 真正的跨进程
    // 上下文切换）时才写 satp + 全量 sfence.vma。返回同一地址空间（绝大多数
    // syscall/中断/信号返回）时 satp 不变、TLB 仍然有效——所有改动用户页表的
    // 路径（缺页/COW/换入用 flush_tlb_va，munmap/mprotect/exec/fork 用
    // flush_tlb_all）都已就地刷新自己的条目，故此处无需再全量刷 TLB。
    // 内核与用户共用页表（__alltraps 不切 satp），所以同进程返回时
    // 当前 satp == user_satp，原先无条件的 sfence.vma 是纯浪费。
    {
        let cur_satp: usize;
        unsafe { core::arch::asm!("csrr {}, satp", out(reg) cur_satp); }
        // ASID-tagged TLB：跨地址空间切换给目标分配/复用 ASID 写入 satp.ASID，
        // 不再每次全刷（仅 ASID 代数回绕时全量 sfence.vma 一次）；同进程返回
        // （PPN 相同）satp 不变、TLB 仍有效，无需任何写入/失效。
        if !crate::arch::asm_helpers::satp_same_address_space(cur_satp, user_satp)
            || reentering_mm
        {
            crate::arch::asm_helpers::activate_user_satp(user_satp);
        }
    }
    crate::mm::tlb::complete_current_pending_flush();
}

/// 设置 sstatus 的 SPP 位为用户态
pub fn set_user_mode() -> usize {
    let mut sstatus = get_sstatus();
    // 清除 SPP 位 (bit 8)
    sstatus &= !(1 << 8);
    // 设置 SPIE 位 (bit 5)
    sstatus |= 1 << 5;
    sstatus
}

/// 获取内核页表的 SATP 值
///
/// 在 RustMicroOS 中，内核使用恒等映射，读取当前 satp 即可
fn kernel_satp() -> usize {
    let satp: usize;
    unsafe {
        core::arch::asm!("csrr {}, satp", out(reg) satp);
    }
    satp
}

/// 返回用户态（usertrapret 的 Rust 实现）
///
/// 设置好所有寄存器后，通过 sret 跳转到用户态。
/// 此函数永不返回。
///
/// 参考: xv6-riscv/kernel/trap.c usertrapret()
///
/// # 关键步骤（与 xv6-riscv 一致）
/// 1. 关闭中断（防止在切换过程中被打断）
/// 2. 设置 stvec 指向 __alltraps（下次 trap 的入口）
/// 3. 设置 trapframe 中的内核信息（供下次 trap 使用）
/// 4. 设置 sstatus.SPP = 0（返回用户态）
/// 5. 设置 sstatus.SPIE = 1（返回后启用中断）
/// 6. 设置 sepc = 用户程序入口点
/// 7. 切换到用户页表并执行 sret
///
/// # 参数
/// - `trap_cx`: TrapContext 指针，包含用户态上下文
/// - `user_satp`: 用户进程的页表 SATP 值
///
/// # Safety
/// 调用者必须确保：
/// - trap_cx 指向有效的 TrapContext
/// - user_satp 是有效的用户页表 SATP 值
/// - TrapContext 已正确初始化（sepc, sstatus, sp 等）
///
/// # Requirements
/// - 5.1: sepc = entry_point
/// - 5.2: sstatus.SPP = 0 (User mode)
/// - 5.3: sstatus.SPIE = 1 (Enable interrupts after sret)
/// - 5.4: SATP = user's page table root
pub fn usertrapret(trap_cx: &mut TrapContext, user_satp: usize) -> ! {
    extern "C" {
        fn __trap_entry();
        fn __commit_and_restore(cx: *const TrapContext) -> !;
    }
    let (_, return_pcb, _) = crate::sched::process_sched::user_return_target();

    crate::mm::tlb::complete_current_pending_flush();
    // execve 原地换 mm 不经过 scheduler switch_to；return-to-user 是提交真实
    // SATP 的权威点，必须同步更新本 hart active_mm，否则新 mm 的 shootdown
    // 目标集合漏掉此 hart。RV mm key 为 SATP 低 44 位 PPN。
    let root_ppn = user_satp as u64 & 0xFFF_FFFF_FFFF;
    let reentering_mm =
        root_ppn != 0 && crate::mm::tlb::note_active_mm(root_ppn) != root_ppn;

    // 内核 → 用户的唯一出口：先把当前进程"截至此刻"还没结账的内核段时间
    // 推进 cpu_kernel_ticks 桶，并把 cpu_in_kernel 翻成 false，让下一段
    // 用户态运行可以被 switch_to / 下一个 ecall 正确算到 cpu_user_ticks。
    // 这条路径覆盖：syscall wrapper 返回后、suspend → 被唤醒切回再 ret 用户、
    // 时钟中断处理结束直回用户等所有"从内核态恢复用户态"的子路径。
    {
        let now = crate::arch::asm_helpers::rdtime();
        crate::sched::process_sched::account_exit_kernel_for(return_pcb, now);
    }

    unsafe {
        // 1. 关闭中断
        core::arch::asm!("csrc sstatus, {}", in(reg) 1usize << 1);

        // 2. finalizer 之前保持内核态 trap 入口；用户态入口由汇编在
        // __commit_and_restore 收尾后切回。
        core::arch::asm!("csrw stvec, {}", in(reg) __trap_entry as usize);
    }

    // 3. 设置 trapframe 中的内核信息
    trap_cx.kernel_satp = kernel_satp();
    trap_cx.trap_handler = trap_handler_rust as usize;

    // 4 & 5. 设置 sstatus: SPP=0, SPIE=1（写入 TrapContext，__restore 会加载）
    let mut sstatus: usize;
    unsafe { core::arch::asm!("csrr {}, sstatus", out(reg) sstatus); }
    sstatus &= !(1usize << 8); // SPP=0
    sstatus |= 1usize << 5;    // SPIE=1
    trap_cx.sstatus = sstatus;

    // 6. 切换到用户页表（仅在跨地址空间时才刷 TLB，见 prepare_switch_context
    //    的同款说明：同进程返回时 satp 不变、无需全量 sfence.vma）。
    {
        let cur_satp: usize;
        unsafe { core::arch::asm!("csrr {}, satp", out(reg) cur_satp); }
        // ASID-tagged TLB：跨地址空间切换给目标分配/复用 ASID 写入 satp.ASID，
        // 不再每次全刷（仅 ASID 代数回绕时全量 sfence.vma 一次）；同进程返回
        // （PPN 相同）satp 不变、TLB 仍有效，无需任何写入/失效。
        if !crate::arch::asm_helpers::satp_same_address_space(cur_satp, user_satp)
            || reentering_mm
        {
            crate::arch::asm_helpers::activate_user_satp(user_satp);
        }
    }
    // active_mm 发布后可能并发收到该 mm 的 shootdown；用户态前最终结账。
    crate::mm::tlb::complete_current_pending_flush();

    unsafe { __commit_and_restore(trap_cx as *const TrapContext) }

}

/// 使用进程 PID 返回用户态
///
/// 这是一个便捷函数，从进程表中获取进程信息后调用 usertrapret。
///
/// # 参数
/// - `pid`: 要切换到的进程 PID
///
/// # Panics
/// - 如果进程不存在
/// - 如果进程没有有效的 TrapContext
/// - 如果进程没有有效的地址空间
pub fn usertrapret_by_pid(pid: crate::sched::process::Pid) -> ! {
    use crate::sched::process::PROCESS_TABLE;

    // 本 hart 即将运行 `pid`：必须先在锁内完成"上 CPU"记账——置 Running、摘出
    // 就绪环、设本 hart 的 current。否则该进程仍是 Runnable 且留在就绪环里，
    // 另一个 hart 的 `select_next` 会同时选中它，两个 hart 便会共用同一份
    // TrapContext 与内核栈。对标 Linux：`schedule()` 选中任务后由
    // `context_switch` 完成 dequeue + `TASK_RUNNING` + per-CPU `current` 更新。
    //
    // 返回值必须检查：`switch_to` 只在目标仍是 `Runnable` 时成功。失败意味着它已
    // 被**另一个 hart** 抢先选中并置 `Running`（或已不可运行），此时本 hart 绝不
    // 能继续拿它的 TrapContext 跑，改为转入本 hart 的 idle 调度循环挑别的任务。
    let took_cpu = {
        let mut guard = PROCESS_TABLE.lock();
        guard
            .as_mut()
            .expect("Process table not initialized")
            .switch_to(pid, crate::sched::trace::SWITCH_YIELD)
    };
    if !took_cpu {
        crate::sched::enter_idle_scheduler_loop();
    }

    // 获取进程信息
    let (trap_cx_ptr, user_satp) = {
        let mut table = PROCESS_TABLE.lock();
        let table = table.as_mut().expect("Process table not initialized");
        let process = table.get_mut(pid).expect("Process not found");

        // 获取用户页表 SATP
        let user_satp = process.satp();
        if user_satp == 0 {
            panic!("Process {} has no valid address space", pid);
        }

        // 获取 TrapContext 指针
        let trap_cx = unsafe {
            process.trap_context_mut().expect("Process has no TrapContext")
        };

        let trap_cx_ptr = trap_cx as *mut TrapContext;

        (trap_cx_ptr, user_satp)
        // 锁在这里自动释放
    };

    // 调用 usertrapret（永不返回）
    unsafe {
        usertrapret(&mut *trap_cx_ptr, user_satp)
    }
}
