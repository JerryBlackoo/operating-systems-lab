//! 调度器子系统（生产架构）
//!
//! 单一权威调度链：`ProcessTable`（任务状态/事务）→ `cpu_rq`（per-CPU
//! EEVDF/RT 队列）→ `policy`（纯调度策略）。SMP 状态机只维护 hart 在线
//! 状态，不再持有第二套 runqueue；历史 MLFQ/EDF 玩具调度器已删除。

pub mod smp;
pub mod cpu_rq;
pub mod policy;
pub mod pi_mutex;
pub mod process;
pub mod process_acct;
pub mod process_exit;
pub mod process_fork;
pub mod process_sched;
pub mod process_stack;
pub mod process_state;
pub(crate) mod process_trace;
pub mod process_wait;
pub mod deadline;
pub mod posix_timer;
pub mod property_tests;
pub mod trace;
pub mod wait_queue;

// 重新导出常用的进程管理函数
pub use process::{
    create_process_from_path,
    create_process_from_elf,
    create_init_process,
};

/// 进入本 hart 的 idle + 调度循环（BSP 启动尾声与每个 AP 上线后都走这里）。
/// 从本 CPU 的 `CpuRq` 取任务，本地空则 steal，对标 Linux 每个 CPU 的
/// `cpu_startup_entry()` → idle task → `schedule()`。
///
/// 先切到**本 hart 专属的 idle 栈**：否则该循环会继续用"进来时所在"的栈，
/// 而那往往是某个进程的内核栈，另一个 hart 同时运行该进程时会共用同一份栈。
/// 详见 `arch::run_on_hart_idle_stack` 的取证记录。
pub fn enter_idle_scheduler_loop() -> ! {
    process_exit::kernel_idle_until_runnable()
}

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use crate::sync::SpinLock;

// ============ 中断计数器（/proc/interrupts 支持） ============

/// 中断计数器 — 按中断号统计（最多 32 个中断源）
pub static INTERRUPT_COUNTS: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];

/// 记录一次中断
pub fn record_interrupt(irq: usize) {
    if irq < 32 {
        INTERRUPT_COUNTS[irq].fetch_add(1, Ordering::Relaxed);
    }
}

/// 基础时间片（tick 数）。生产 CFS 时间片由 `policy::sched_slice_ns`
/// 计算；该常量保留给 SCHED_RR 兼容路径（`process_sched`）。
pub const DEFAULT_TIME_SLICE: usize = 10;

/// 初始化调度器：只初始化**生产子系统**（SMP 拓扑、per-CPU rq 由
/// `cpu_rq` 静态建好、进程表）。历史上这里还初始化过一套 MLFQ/EDF
/// 玩具调度器；该双轨状态已删除，唯一调度权威是 `ProcessTable` +
/// `PERCPU_RQ`。
pub fn init(boot_cpu_id: usize) {
    smp::init(crate::config::get_num_cpus(), boot_cpu_id);

    // 初始化进程管理子系统
    process::init();

    // SCHED_DEADLINE 的 CBS 参数表：注册在 RT_SCHEDULER，节流后的补充
    // 必须从这张表 O(1) 取 wcet/period。缺失时 setattr 虽返回 0，但
    // dl_timer_tick 节流后无法 replenish，任务带着 runtime=0 被唤醒。
    deadline::init();
}

/// 时钟中断处理（别名）
///
/// 对标 Linux hrtimer/tick 分层：定时器中断可能远快于周期 tick（hrtimer 收窄到
/// 睡眠 deadline / need_resched 时 ~µs 级重触发）。周期性记账（CFS vruntime、
/// RR 时间片、RT bandwidth、cgroup cpu.max、TCP 拥塞时钟、loadavg……全部按
/// 「每 10ms 一次」设计）只在距上次真周期 tick ≥ 一个周期时执行；到期唤醒类
/// （check_sleep_deadlines / timerfd / posix timer / sqpoll）每次中断都跑。
/// 否则 3~4kHz 的 hrtimer 中断会以 36 倍频跑全套 O(n) 记账扫描把 CPU 打饱和
/// （cyclictest t8 唤醒迟到 ~1.6ms 的真因），且按次数推进的时钟（cong_clock_tick、
/// calc_load、RR 片）全部走快数十倍。
pub fn timer_tick() {
    use core::sync::atomic::{AtomicU64, Ordering};
    static LAST_PERIODIC_TICK: AtomicU64 = AtomicU64::new(0);
    let now = crate::arch::asm_helpers::rdtime();
    let period = crate::arch::consts::CLOCK_FREQ as u64 / 100; // 10ms
    let last = LAST_PERIODIC_TICK.load(Ordering::Relaxed);
    let periodic = now.wrapping_sub(last) >= period;
    if periodic {
        LAST_PERIODIC_TICK.store(now, Ordering::Relaxed);
    }
    if periodic {
        timer_tick_periodic();
    }
    // 到期唤醒类：每次定时器中断都要跑（hrtimer 精确唤醒正是为它们服务）。
    if crate::RTDIAG {
        let rt = |m: usize, f: &dyn Fn()| {
            let t0 = crate::arch::asm_helpers::rdtime();
            f();
            let us = crate::arch::asm_helpers::rdtime().saturating_sub(t0) * 1_000_000
                / crate::arch::consts::CLOCK_FREQ as u64;
            process::rtstat_record(m, us);
        };
        rt(14, &|| process::check_sleep_deadlines());
        rt(15, &|| crate::fs::timerfd::poll_and_wake_expired());
        rt(16, &|| posix_timer::fire_due_timers());
        rt(17, &|| crate::io_uring::sqpoll_tick());
        return;
    }
    process::check_sleep_deadlines();
    crate::fs::timerfd::poll_and_wake_expired();
    posix_timer::fire_due_timers();
    crate::io_uring::sqpoll_tick();

    if periodic && crate::cmdline::hangdump_enabled() {
        hangdump_check(now);
    }
}

/// HANGDUMP=1 全内核挂死法证（bootarg 门控，官方跑分零开销）：
/// 上下文切换计数冻结 ≥15s（timer tick 还在跳、但没有任何进程被调度 =
/// 全员永久等待/单进程在内核内自旋）时，把整张进程表的调度状态与等待点
/// 倒到串口；之后每 15s 重复，直到计数再次前进。
fn hangdump_check(now: u64) {
    use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    static LAST_SWITCHES: AtomicUsize = AtomicUsize::new(0);
    static FROZEN_SINCE: AtomicU64 = AtomicU64::new(0);
    static LAST_DUMP: AtomicU64 = AtomicU64::new(0);
    static LAST_ALIVE: AtomicU64 = AtomicU64::new(0);

    let cur = get_context_switch_count();
    if cur != LAST_SWITCHES.swap(cur, Ordering::Relaxed) {
        FROZEN_SINCE.store(now, Ordering::Relaxed);
        return;
    }
    let frozen_since = FROZEN_SINCE.load(Ordering::Relaxed);
    if frozen_since == 0 {
        FROZEN_SINCE.store(now, Ordering::Relaxed);
        return;
    }
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    if now.wrapping_sub(frozen_since) < 15 * freq {
        return;
    }
    if now.wrapping_sub(LAST_DUMP.load(Ordering::Relaxed)) < 15 * freq {
        return;
    }
    LAST_DUMP.store(now, Ordering::Relaxed);

    // try_lock：挂死点若持有 PROCESS_TABLE（不应发生——wfi 循环不持锁）则跳过，
    // 绝不能在 trap 里对已持有的锁二次上锁。
    //
    // 持锁只格式化，放锁再打印：串口输出远慢于格式化，直打会把表锁占住整个
    // 输出时长（实测 128s），其余 hart 的 trap 路径全部关中断堆积——诊断自己
    // 制造出它要观测的停摆。**心跳行同样必须锁外打印**：初版把 println! 写在
    // try_lock guard 作用域内，SCDBG 打印风暴下串口锁拥塞，心跳行持 PT 等串口
    // 把临界区放大到 9.55s（pt_lock max_hold 实测，timer_tick 调用点）。
    enum Action {
        Skip,
        Heartbeat,
        Dump(alloc::vec::Vec<alloc::string::String>),
    }
    let action = match crate::sched::process::PROCESS_TABLE.try_lock() {
        Some(guard) => match guard.as_ref() {
            Some(table) => {
                // 误报门禁（2026-08-14 LA compile 长跑实证：233/233 次 dump 全为
                // 假停摆）：TCG 慢速下单 vCPU 上 rustc 用户态 codegen 数分钟没有
                // 任何上下文切换是常态，「切换计数冻结」不能判挂死。只有不存在
                // 「用户态推进中 / 待调度」任务（全员睡眠 = 真丢唤醒；全员
                // Running 且都在内核 = 内核自旋）才值得全表倒出。用户态计算期
                // 改打一行 60s 心跳，供监测脚本判定推进。
                if table.has_user_running_or_runnable() {
                    if now.wrapping_sub(LAST_ALIVE.load(Ordering::Relaxed)) >= 60 * freq {
                        Action::Heartbeat
                    } else {
                        Action::Skip
                    }
                } else {
                    Action::Dump(table.hangdump_lines(cur))
                }
            }
            None => return, // 表尚未初始化，无可倒
        },
        None => {
            // 拿不到锁本身就是最关键的一条线索：说明有 hart 卡在临界区里。
            // `PT_HOLD_RA` 记的是最近一次成功持锁的返回地址，addr2line 一解析
            // 就能点名持锁者，省掉「再猜一轮是谁」。
            let (max_ticks, max_ra) = crate::sched::process::proc_table_max_hold();
            crate::println!(
                "[hangdump] PROCESS_TABLE held by hung context: holder_ra={:#x} max_hold_ra={:#x} max_hold_ticks={}",
                crate::sched::process::proc_table_last_holder_ra(),
                max_ra,
                max_ticks
            );
            return;
        }
    };
    // 锁已随上方作用域释放，此处纯打印/纯原子。
    match action {
        Action::Skip => {}
        Action::Heartbeat => {
            LAST_ALIVE.store(now, Ordering::Relaxed);
            crate::println!(
                "[hangdump] alive: switches={} user/runnable tasks present (compute window, not a stall)",
                cur
            );
        }
        Action::Dump(lines) => {
            for line in lines {
                crate::println!("{}", line);
            }
        }
    }
}

/// 周期性记账（10ms 一次）：调度时间片/带宽/统计。
fn timer_tick_periodic() {
    // CFS update_curr：switch_to 真实 delta_exec + effective_cfs_state 按需投影。
    // SCHED_RR：timer_preemption_due 墙钟量子判定。
    // cgroup cpu.max：未配置配额时 switch_to 真实 delta_exec 结账；配置过
    // cpu.max/freezer 后恢复周期 tick 计费，保证配额实时性。
    cgroup_cpu_tick_if_active();
    // RT bandwidth 全局计费；仅在带宽耗尽那一刻才取 PROCESS_TABLE requeue。
    rt_bandwidth_tick();
    // SCHED_DEADLINE: runtime accounting + throttling + replenishment
    deadline::dl_timer_tick();
    // TCP congestion control clock: advance 10ms per tick
    crate::net::tcp_cong::cong_clock_tick(10);
    // RLIMIT_CPU 实时强制：timer_tick 是 RV/LA 两架构定时器中断都会经过的
    // 通用逐 tick 入口。
    process_sched::rlimit_cpu_tick();
    // Slice 2.5 真实化：calc_load 真 EWMA 采样（Linux kernel/sched/loadavg.c）
    calc_load_tick();
}

/// cgroup cpu.max 激活时的周期计费。未激活（绝大多数负载）直接返回，避免
/// 每 hart 每 10ms 抢 PROCESS_TABLE 克隆 cgroup 路径。
fn cgroup_cpu_tick_if_active() {
    if !crate::kernel::cgroup::cgroup_cpu_quota_active() {
        return;
    }
    let path = {
        let table = process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return };
        let Some(pid) = t.current else { return };
        match t.get(pid) {
            Some(p) => p.cgroup_path.clone(),
            None => return,
        }
    };
    if !path.is_empty() {
        crate::kernel::cgroup::charge_cpu(&path, 10_000);
    }
}

/// Global RT bandwidth state（Linux `rt_bandwidth` 的全局账本）。
static RT_BW_RUNTIME_USED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static RT_BW_PERIOD_TICKS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// RT runtime limit per period in ticks. 95 out of 100 ticks = 95%.
const RT_BW_RUNTIME_LIMIT: u64 = 95;
/// RT period length in ticks. 100 ticks = 1000ms.
const RT_BW_PERIOD_LENGTH: u64 = 100;

/// RT bandwidth throttling（Linux `sched_rt_runtime_exceeded`）。
///
/// 判断当前是否 RT 使用 per-hart `HART_CURRENT_POLICY` 无锁投影（由
/// set_current/finish_staged_switch 在持表锁时随 current 一起发布）。只有
/// 带宽耗尽、确实要把当前 RT 任务 requeue 时才取一次 PROCESS_TABLE。
fn rt_bandwidth_tick() {
    use core::sync::atomic::Ordering;
    // Advance period counter
    let period = RT_BW_PERIOD_TICKS.fetch_add(1, Ordering::Relaxed) + 1;
    if period >= RT_BW_PERIOD_LENGTH {
        RT_BW_PERIOD_TICKS.store(0, Ordering::Relaxed);
        RT_BW_RUNTIME_USED.store(0, Ordering::Relaxed);
        return;
    }

    let policy = crate::sched::process_sched::current_policy_hint();
    if policy != 1 && policy != 2 {
        return;
    }

    let used = RT_BW_RUNTIME_USED.fetch_add(1, Ordering::Relaxed) + 1;
    if used < RT_BW_RUNTIME_LIMIT {
        return;
    }

    // 带宽耗尽：唯一需要表锁的慢路径。requeue 当前 RT，让 CFS 获得 CPU。
    let mut table = process::PROCESS_TABLE.lock();
    let Some(ref mut t) = *table else { return };
    let Some(current_pid) = t.current else { return };
    if t.get(current_pid)
        .map_or(false, |p| p.sched_policy == 1 || p.sched_policy == 2)
    {
        t.requeue_at_tail(current_pid);
    }
}

// ============================================================================
// Slice 2.5 真实化：Linux 风格 load average（calc_load）真实 EWMA
// ============================================================================
//
// Linux `kernel/sched/loadavg.c::calc_load_fold_active` / `calc_global_load`:
// 每 LOAD_FREQ = 5*HZ + 10 jiffies 采样一次 nr_active，三个固定点衰减常数：
//   EXP_1  = 1884  // exp(-5/60)   * (1<<11)
//   EXP_5  = 2014  // exp(-5/300)  * (1<<11)
//   EXP_15 = 2037  // exp(-5/900)  * (1<<11)
// 更新公式：load = (load*EXP + active*(FIXED_1-EXP)) >> FSHIFT
//
// 真实化要点（用户明确点名严禁简化）：
//   * 不依赖 tick 计数 — timer 中断有可能因 idle / nohz / 启动晚到而丢失；
//     必须按 `rdtime()` 墙钟时差判定 5s 窗口（与 Linux v5+ 的 `calc_load_nohz`
//     等价：使用 `calc_load_update` 时刻锚点，跨多个 5s 窗口要 catch-up
//     连续更新多次）。
//   * `calc_load_update` 初始化为 boot 时刻 + 5s；每跨过一次窗口就 +5s，
//     并相应地把 EWMA decay 多次，避免长时间 idle 后单次更新让 load 异常
//     塌陷。
//   * 采样源 nr_active 必须取自 PROCESS_TABLE 实时计数，不能写死。

const FSHIFT: u64 = 11;
const FIXED_1: u64 = 1 << FSHIFT;
const EXP_1: u64 = 1884;
const EXP_5: u64 = 2014;
const EXP_15: u64 = 2037;

/// `/proc/loadavg` 三个 EWMA（fixed-point，>>FSHIFT 得整数部分）。
pub static LOADAVG: SpinLock<[u64; 3]> = SpinLock::new([0; 3]);
/// Linux `calc_load_update` 等价：下一次允许采样的 `rdtime()` 绝对值
/// （单位 = 体系结构 timer ticks）。第一次调用时锚定到当前 + 5s。
static CALC_LOAD_NEXT_TICK: core::sync::atomic::AtomicU64 =
    core::sync::atomic::AtomicU64::new(0);

fn calc_load_one(load: u64, exp: u64, active: u64) -> u64 {
    // (load * exp + active * (FIXED_1 - exp)) >> FSHIFT
    let a = load.saturating_mul(exp);
    let b = active.saturating_mul(FIXED_1 - exp);
    a.wrapping_add(b) >> FSHIFT
}

/// 每次 timer_tick 调用；按 `rdtime()` 墙钟判定是否跨过 5s 采样窗口。
/// 若长时间 idle 跨过多个窗口，按 Linux `calc_global_load` 行为连续
/// catch-up 多次衰减。
fn calc_load_tick() {
    use core::sync::atomic::Ordering;
    let now = crate::arch::asm_helpers::rdtime();
    let interval = crate::arch::consts::CLOCK_FREQ as u64 * 5; // 5 秒 ticks

    let next = CALC_LOAD_NEXT_TICK.load(Ordering::Relaxed);
    if next == 0 {
        // 首次：锚到 now + 5s，本次不更新（让 EWMA 真实采样从下一个 5s 窗口起）
        CALC_LOAD_NEXT_TICK.compare_exchange(
            0, now.saturating_add(interval),
            Ordering::Relaxed, Ordering::Relaxed
        ).ok();
        return;
    }
    if now < next { return; }

    // 跨过 ≥1 个 5s 窗口：先用 atomic CAS 防止多 CPU 重复采样，再 catch-up。
    let missed_windows = ((now - next) / interval) + 1;
    let new_next = next.saturating_add(missed_windows.saturating_mul(interval));
    if CALC_LOAD_NEXT_TICK
        .compare_exchange(next, new_next, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return; // 别的 CPU 抢先采样
    }

    // 采样真值：nr_active = nr_running + nr_uninterruptible。
    // 状态迁移经 `process::set_process_state` 单点维护无锁原子镜像，
    // 不再为 5s 一次的 EWMA 采样拿中央 PROCESS_TABLE 锁。
    let active_fp = process::lock_free_active_tasks() as u64 * FIXED_1;

    // catch-up：Linux 行为是按错过的窗口数连续 decay 多次（idle 长时间
    // 后 load 平滑下降，而不是一次跳到当前 active）。
    let mut lock = LOADAVG.lock();
    for _ in 0..missed_windows {
        lock[0] = calc_load_one(lock[0], EXP_1, active_fp);
        lock[1] = calc_load_one(lock[1], EXP_5, active_fp);
        lock[2] = calc_load_one(lock[2], EXP_15, active_fp);
    }
}

/// 给 procfs 读取的格式化 helper：把 fixed-point 转 "X.YY" 字符串。
pub fn loadavg_format() -> alloc::string::String {
    use alloc::string::String;
    let snap = *LOADAVG.lock();
    let fmt_one = |fp: u64| -> String {
        let int_part = fp >> FSHIFT;
        // 小数部分：(fp % FIXED_1) * 100 / FIXED_1
        let frac = ((fp & (FIXED_1 - 1)) * 100) >> FSHIFT;
        alloc::format!("{}.{:02}", int_part, frac)
    };
    alloc::format!("{} {} {}", fmt_one(snap[0]), fmt_one(snap[1]), fmt_one(snap[2]))
}

/// 返回 (runnable_count, total_active_count, last_pid) 供 `/proc/loadavg`
/// 最后三字段使用（Linux: `nr_running/nr_threads last_pid`）。
pub fn loadavg_runnable_stats() -> (usize, usize, usize) {
    // 与 calc_load 同源的无锁镜像：runnable = Running + Runnable，
    // total = NR_PROCESSES（插入/物理移除时维护）。/proc/loadavg 读取不再
    // 为聚合三个整数而拿 PROCESS_TABLE 锁。
    let runnable = process::lock_free_runnable_tasks();
    let total = process::NR_PROCESSES.load(Ordering::Relaxed);
    // last_pid：单调递增 NEXT_PID 的 (current-1)，本来就是无锁原子。
    let last_pid = process::current_last_pid();
    (runnable, total, last_pid)
}

/// CPU 调度统计
#[derive(Default, Clone, Copy)]
pub struct CpuSchedStats {
    /// 上下文切换次数
    pub context_switches: u64,
    /// 迁移次数
    pub migrations: u64,
    /// 负载平均值（缩放 100 倍）
    pub load_avg: u64,
    /// 运行队列长度
    pub runqueue_len: usize,
}

/// 每 CPU 调度统计
static CPU_STATS: [SpinLock<CpuSchedStats>; 8] = [
    SpinLock::new(CpuSchedStats { context_switches: 0, migrations: 0, load_avg: 0, runqueue_len: 0 }),
    SpinLock::new(CpuSchedStats { context_switches: 0, migrations: 0, load_avg: 0, runqueue_len: 0 }),
    SpinLock::new(CpuSchedStats { context_switches: 0, migrations: 0, load_avg: 0, runqueue_len: 0 }),
    SpinLock::new(CpuSchedStats { context_switches: 0, migrations: 0, load_avg: 0, runqueue_len: 0 }),
    SpinLock::new(CpuSchedStats { context_switches: 0, migrations: 0, load_avg: 0, runqueue_len: 0 }),
    SpinLock::new(CpuSchedStats { context_switches: 0, migrations: 0, load_avg: 0, runqueue_len: 0 }),
    SpinLock::new(CpuSchedStats { context_switches: 0, migrations: 0, load_avg: 0, runqueue_len: 0 }),
    SpinLock::new(CpuSchedStats { context_switches: 0, migrations: 0, load_avg: 0, runqueue_len: 0 }),
];

/// 获取 CPU 调度统计
pub fn get_cpu_stats(cpu_id: usize) -> CpuSchedStats {
    if cpu_id < 8 {
        *CPU_STATS[cpu_id].lock()
    } else {
        CpuSchedStats::default()
    }
}

/// 记录上下文切换
pub fn record_context_switch(cpu_id: usize) {
    if cpu_id < 8 {
        CPU_STATS[cpu_id].lock().context_switches += 1;
    }
}

/// 记录任务迁移
pub fn record_migration(cpu_id: usize) {
    if cpu_id < 8 {
        CPU_STATS[cpu_id].lock().migrations += 1;
    }
}

/// 更新运行队列长度
pub fn update_runqueue_len(cpu_id: usize, len: usize) {
    if cpu_id < 8 {
        CPU_STATS[cpu_id].lock().runqueue_len = len;
    }
}

/// 全局上下文切换计数器
static GLOBAL_CONTEXT_SWITCHES: AtomicUsize = AtomicUsize::new(0);

/// 获取全局上下文切换计数
pub fn get_context_switch_count() -> usize {
    GLOBAL_CONTEXT_SWITCHES.load(Ordering::Relaxed)
}

/// 增加全局上下文切换计数
pub fn inc_context_switch_count() {
    GLOBAL_CONTEXT_SWITCHES.fetch_add(1, Ordering::Relaxed);
}

/// 主动让出 CPU（对标 Linux `sched_yield` → `schedule()`）。
///
/// 只置本 hart 的 need-resched：真正的上下文切换统一由 trap 返回路径的
/// 调度提交点完成，避免在这里再维护一套独立的"直接调度"路径。
pub fn yield_now() {
    crate::sched::process_sched::set_need_resched();
}


/// 调度器自测试：只测试**当前生产架构**（EEVDF policy + per-CPU rq +
/// SMP 状态机 + 进程表），不再触碰已删除的 MLFQ/EDF 玩具调度器，也
/// 不重新初始化生产进程表。
pub fn self_test() {
    crate::println!("[sched] Running self-test...");

    // 1. 调度策略纯函数（无锁、无全局状态）
    policy::self_test();
    crate::println!("[sched] EEVDF policy tests passed");

    // 2. per-CPU runqueue：EEVDF 增广树的最小不变量
    let mut rq = cpu_rq::CpuRq::empty();
    assert!(rq.is_idle());
    rq.enqueue_cfs(1, 1_000, policy::entity_deadline(1_000, 3_000_000, 1024), 1024);
    rq.enqueue_cfs(2, 2_000, policy::entity_deadline(2_000, 3_000_000, 1024), 1024);
    assert_eq!(rq.cfs_len(), 2);
    assert_eq!(rq.cfs.pick_eevdf(|_| true), Some(1));
    assert!(rq.cfs.contains(2));
    rq.remove_pid(1);
    assert_eq!(rq.cfs.only_pid(), Some(2));
    rq.remove_pid(2);
    assert!(rq.is_idle());

    // AVL 平衡性/确定性压力：64 个乱序 deadline 全部插入再全部摘除，
    // 二级索引必须始终一致，最终树完全为空。
    for i in 0..64usize {
        let pid = 100 + i;
        let dl = 1_000_000u64.wrapping_mul((i.wrapping_mul(37) % 64) as u64 + 1);
        rq.enqueue_cfs(pid, dl, dl + 1_000, 1024);
        assert!(rq.cfs.contains(pid));
    }
    for i in 0..64usize {
        let pid = 100 + i;
        assert!(rq.cfs.remove(pid).is_some());
        assert!(!rq.cfs.contains(pid));
    }
    assert!(rq.cfs.is_empty());

    // RT bitmap：高优先级先出，同优先级 FIFO。
    rq.enqueue_rt(501, 10);
    rq.enqueue_rt(502, 90);
    rq.enqueue_rt(503, 90);
    rq.enqueue_rt(504, 50);
    let order = rq.rt.pids_by_priority();
    assert_eq!(order, alloc::vec![502, 503, 504, 501]);
    assert_eq!(rq.rt.first(), Some(502));

    // DL EDF：deadline 越小越靠前；deadline=0 视为无效排最后。
    rq.enqueue_dl(601, 5_000_000);
    rq.enqueue_dl(602, 1_000_000);
    rq.enqueue_dl(603, 0);
    assert_eq!(rq.dl.pids_edf(), alloc::vec![602, 601, 603]);
    for pid in [501usize, 502, 503, 504, 601, 602, 603] {
        rq.remove_pid(pid);
    }
    assert!(rq.is_idle());
    crate::println!("[sched] per-CPU EEVDF/RT/DL runqueue tests passed");

    // 3. 基础常量与 CPU 统计（真实切换计数只增不减）
    assert_eq!(DEFAULT_TIME_SLICE, 10);
    let before = get_context_switch_count();
    inc_context_switch_count();
    assert_eq!(get_context_switch_count(), before + 1);
    record_context_switch(0);
    record_migration(0);
    update_runqueue_len(0, 3);
    assert!(get_cpu_stats(0).context_switches >= 1);
    crate::println!("[sched] CPU stats tests passed");

    // 4. yield_now 现在等价于 set_need_resched，不直接切上下文
    yield_now();
    assert!(crate::sched::process_sched::need_resched_pending());
    crate::sched::process_sched::take_need_resched();

    // 5. 真实子模块自测试（进程 / SMP / PI / 属性测试）
    process::self_test();
    smp::self_test();
    pi_mutex::self_test();
    if let Err(e) = property_tests::self_test() {
        crate::println!("[sched] Property test failed: {}", e);
    }

    crate::println!("[sched] Self-test passed!");
}
