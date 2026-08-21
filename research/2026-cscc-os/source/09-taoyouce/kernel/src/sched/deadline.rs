//! 实时调度截止时间管理
//!
//! 实现截止时间错过检测和处理机制

use alloc::collections::BinaryHeap;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use crate::sync::SpinLock;

/// 当前系统时间（纳秒）
static SYSTEM_TIME_NS: AtomicU64 = AtomicU64::new(0);

/// 截止时间错过统计
static DEADLINE_MISSES: AtomicU64 = AtomicU64::new(0);

/// 任务 ID
pub type TaskId = u64;

/// 时间戳（纳秒）
pub type Timestamp = u64;

/// 实时任务参数
#[derive(Debug, Clone, Copy)]
pub struct RtTaskParams {
    /// 任务 ID
    pub task_id: TaskId,
    /// 周期（纳秒）
    pub period: Timestamp,
    /// 最坏情况执行时间（WCET）
    pub wcet: Timestamp,
    /// 相对截止时间
    pub relative_deadline: Timestamp,
    /// 优先级（用于固定优先级调度）
    pub priority: u8,
}

/// 实时任务实例（作业）
#[derive(Debug, Clone, Copy)]
pub struct RtJob {
    /// 任务 ID
    pub task_id: TaskId,
    /// 释放时间
    pub release_time: Timestamp,
    /// 绝对截止时间
    pub absolute_deadline: Timestamp,
    /// 剩余执行时间
    pub remaining_time: Timestamp,
    /// 是否已错过截止时间
    pub deadline_missed: bool,
}

impl RtJob {
    /// 创建新作业
    pub fn new(params: &RtTaskParams, release_time: Timestamp) -> Self {
        Self {
            task_id: params.task_id,
            release_time,
            absolute_deadline: release_time + params.relative_deadline,
            remaining_time: params.wcet,
            deadline_missed: false,
        }
    }
    
    /// 检查是否错过截止时间
    pub fn check_deadline(&mut self, current_time: Timestamp) -> bool {
        if !self.deadline_missed && current_time > self.absolute_deadline {
            self.deadline_missed = true;
            DEADLINE_MISSES.fetch_add(1, AtomicOrdering::SeqCst);
            true
        } else {
            false
        }
    }
}

/// 用于 BinaryHeap 的包装器（按截止时间排序）
#[derive(Debug, Clone, Copy)]
pub struct DeadlineEntry(pub RtJob);

impl PartialEq for DeadlineEntry {
    fn eq(&self, other: &Self) -> bool {
        self.0.absolute_deadline == other.0.absolute_deadline
    }
}

impl Eq for DeadlineEntry {}

impl PartialOrd for DeadlineEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeadlineEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // 反转顺序，使最早截止时间在堆顶
        other.0.absolute_deadline.cmp(&self.0.absolute_deadline)
    }
}

/// 截止时间错过处理策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlineMissPolicy {
    /// 继续执行（尽力而为）
    Continue,
    /// 跳过当前作业
    Skip,
    /// 终止任务
    Terminate,
    /// 降级到非实时
    Demote,
    /// 触发异常处理
    Exception,
}

/// 截止时间错过处理器
pub struct DeadlineMissHandler {
    /// 默认策略
    pub default_policy: DeadlineMissPolicy,
    /// 每任务策略
    pub task_policies: Vec<(TaskId, DeadlineMissPolicy)>,
    /// 错过回调函数
    pub callbacks: Vec<fn(TaskId, Timestamp)>,
}

impl DeadlineMissHandler {
    pub fn new(default_policy: DeadlineMissPolicy) -> Self {
        Self {
            default_policy,
            task_policies: Vec::new(),
            callbacks: Vec::new(),
        }
    }
    
    /// 设置任务特定策略
    pub fn set_task_policy(&mut self, task_id: TaskId, policy: DeadlineMissPolicy) {
        // 移除旧策略
        self.task_policies.retain(|(id, _)| *id != task_id);
        self.task_policies.push((task_id, policy));
    }
    
    /// 注册回调
    pub fn register_callback(&mut self, callback: fn(TaskId, Timestamp)) {
        self.callbacks.push(callback);
    }
    
    /// 获取任务策略
    pub fn get_policy(&self, task_id: TaskId) -> DeadlineMissPolicy {
        self.task_policies
            .iter()
            .find(|(id, _)| *id == task_id)
            .map(|(_, policy)| *policy)
            .unwrap_or(self.default_policy)
    }
    
    /// 处理截止时间错过
    pub fn handle_miss(&self, job: &RtJob) -> DeadlineMissAction {
        let policy = self.get_policy(job.task_id);
        let current_time = get_system_time();
        
        // 调用回调
        for callback in &self.callbacks {
            callback(job.task_id, current_time);
        }
        
        // 记录日志
        crate::println!(
            "[deadline] Task {} missed deadline by {} ns (policy: {:?})",
            job.task_id,
            current_time.saturating_sub(job.absolute_deadline),
            policy
        );
        
        match policy {
            DeadlineMissPolicy::Continue => DeadlineMissAction::Continue,
            DeadlineMissPolicy::Skip => DeadlineMissAction::SkipJob,
            DeadlineMissPolicy::Terminate => DeadlineMissAction::TerminateTask,
            DeadlineMissPolicy::Demote => DeadlineMissAction::DemoteToNonRt,
            DeadlineMissPolicy::Exception => DeadlineMissAction::RaiseException,
        }
    }
}

/// 截止时间错过后的动作
#[derive(Debug, Clone, Copy)]
pub enum DeadlineMissAction {
    /// 继续执行
    Continue,
    /// 跳过当前作业
    SkipJob,
    /// 终止任务
    TerminateTask,
    /// 降级为非实时任务
    DemoteToNonRt,
    /// 触发异常
    RaiseException,
}

/// 实时调度器（带截止时间管理）
pub struct RtScheduler {
    /// 就绪队列（按截止时间排序）
    pub ready_queue: BinaryHeap<DeadlineEntry>,
    /// 任务参数表
    pub task_params: Vec<RtTaskParams>,
    /// 截止时间错过处理器
    pub miss_handler: DeadlineMissHandler,
    /// 当前运行作业
    pub current_job: Option<RtJob>,
}

impl RtScheduler {
    pub fn new() -> Self {
        Self {
            ready_queue: BinaryHeap::new(),
            task_params: Vec::new(),
            miss_handler: DeadlineMissHandler::new(DeadlineMissPolicy::Continue),
            current_job: None,
        }
    }
    
    /// 注册实时任务
    pub fn register_task(&mut self, params: RtTaskParams) {
        self.task_params.push(params);
    }
    
    /// 释放作业（周期性任务到达）
    pub fn release_job(&mut self, task_id: TaskId) {
        if let Some(params) = self.task_params.iter().find(|p| p.task_id == task_id) {
            let job = RtJob::new(params, get_system_time());
            self.ready_queue.push(DeadlineEntry(job));
        }
    }
    
    /// 选择下一个作业（EDF）
    pub fn schedule(&mut self) -> Option<RtJob> {
        // 检查当前作业的截止时间
        if let Some(ref mut job) = self.current_job {
            if job.check_deadline(get_system_time()) {
                let action = self.miss_handler.handle_miss(job);
                match action {
                    DeadlineMissAction::SkipJob | 
                    DeadlineMissAction::TerminateTask => {
                        self.current_job = None;
                    }
                    _ => {}
                }
            }
        }
        
        // 检查就绪队列中的截止时间
        self.check_ready_queue_deadlines();
        
        // 选择最早截止时间的作业
        if let Some(DeadlineEntry(job)) = self.ready_queue.pop() {
            self.current_job = Some(job);
            return Some(job);
        }
        
        None
    }
    
    /// 检查就绪队列中的截止时间
    fn check_ready_queue_deadlines(&mut self) {
        let current_time = get_system_time();
        let mut new_queue = BinaryHeap::new();
        
        while let Some(DeadlineEntry(mut job)) = self.ready_queue.pop() {
            if job.check_deadline(current_time) {
                let action = self.miss_handler.handle_miss(&job);
                match action {
                    DeadlineMissAction::SkipJob => continue,
                    DeadlineMissAction::TerminateTask => {
                        // 移除该任务的所有作业
                        continue;
                    }
                    _ => {}
                }
            }
            new_queue.push(DeadlineEntry(job));
        }
        
        self.ready_queue = new_queue;
    }
    
    /// 作业完成
    pub fn job_complete(&mut self, task_id: TaskId) {
        if let Some(ref job) = self.current_job {
            if job.task_id == task_id {
                self.current_job = None;
            }
        }
    }
    
    /// 时钟 tick
    pub fn tick(&mut self, elapsed_ns: Timestamp) {
        // 更新当前作业的剩余时间
        if let Some(ref mut job) = self.current_job {
            job.remaining_time = job.remaining_time.saturating_sub(elapsed_ns);
            
            // 检查截止时间
            if job.check_deadline(get_system_time()) {
                let action = self.miss_handler.handle_miss(job);
                match action {
                    DeadlineMissAction::SkipJob => {
                        self.current_job = None;
                    }
                    _ => {}
                }
            }
        }
    }
    
    /// 设置截止时间错过策略
    pub fn set_miss_policy(&mut self, task_id: TaskId, policy: DeadlineMissPolicy) {
        self.miss_handler.set_task_policy(task_id, policy);
    }
    
    /// 获取统计信息
    pub fn get_stats(&self) -> RtStats {
        RtStats {
            total_deadline_misses: DEADLINE_MISSES.load(AtomicOrdering::SeqCst),
            ready_queue_length: self.ready_queue.len(),
            registered_tasks: self.task_params.len(),
        }
    }
}

/// 实时调度统计
#[derive(Debug, Clone, Copy)]
pub struct RtStats {
    /// 总截止时间错过次数
    pub total_deadline_misses: u64,
    /// 就绪队列长度
    pub ready_queue_length: usize,
    /// 注册的任务数
    pub registered_tasks: usize,
}

/// 全局实时调度器
static RT_SCHEDULER: SpinLock<Option<RtScheduler>> = SpinLock::new(None);

/// 初始化实时调度器
pub fn init() {
    let mut sched = RT_SCHEDULER.lock();
    *sched = Some(RtScheduler::new());
    crate::println!("[deadline] Real-time scheduler initialized");
}

/// 获取系统时间
pub fn get_system_time() -> Timestamp {
    SYSTEM_TIME_NS.load(AtomicOrdering::SeqCst)
}

/// 更新系统时间
pub fn update_system_time(ns: Timestamp) {
    SYSTEM_TIME_NS.store(ns, AtomicOrdering::SeqCst);
}

/// 增加系统时间
pub fn advance_system_time(delta_ns: Timestamp) {
    SYSTEM_TIME_NS.fetch_add(delta_ns, AtomicOrdering::SeqCst);
}

/// 注册实时任务
pub fn register_rt_task(params: RtTaskParams) {
    if let Some(ref mut sched) = *RT_SCHEDULER.lock() {
        sched.register_task(params);
    }
}

/// 释放作业
pub fn release_rt_job(task_id: TaskId) {
    if let Some(ref mut sched) = *RT_SCHEDULER.lock() {
        sched.release_job(task_id);
    }
}

/// 调度
pub fn schedule_rt() -> Option<RtJob> {
    RT_SCHEDULER.lock()
        .as_mut()
        .and_then(|s| s.schedule())
}

/// 设置截止时间错过策略
pub fn set_deadline_miss_policy(task_id: TaskId, policy: DeadlineMissPolicy) {
    if let Some(ref mut sched) = *RT_SCHEDULER.lock() {
        sched.set_miss_policy(task_id, policy);
    }
}

/// 获取截止时间错过次数
pub fn get_deadline_miss_count() -> u64 {
    DEADLINE_MISSES.load(AtomicOrdering::SeqCst)
}

// ============================================================================
// CBS/EDF runtime accounting integrated with ProcessTable
// (对标 Linux kernel/sched/deadline.c — update_curr_dl, dl_task_timer)
// ============================================================================

use crate::sched::process::{
    ProcessTable, PROCESS_TABLE, DEADLINE_RETURN_SENTINEL,
};
use crate::sched::process_sched::HART_CURRENT_PCB;

/// 每 hart 上次 SCHED_DEADLINE runtime 记账的 rdtime 锚点。
static LAST_DL_TICK: [AtomicU64; crate::sched::smp::MAX_CPUS] =
    [const { AtomicU64::new(0) }; crate::sched::smp::MAX_CPUS];

#[inline]
fn rdtime_to_ns(ticks: u64) -> u64 {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    if freq == 0 {
        0
    } else {
        ticks.saturating_mul(1_000_000_000) / freq
    }
}

#[inline]
fn ns_to_rdtime(ns: u64) -> u64 {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    if freq == 0 {
        return 0;
    }
    ns.saturating_mul(freq).saturating_div(1_000_000_000).max(1)
}

fn dl_params_for(pid: usize) -> Option<RtTaskParams> {
    RT_SCHEDULER
        .lock()
        .as_ref()
        .and_then(|s| s.task_params.iter().find(|p| p.task_id == pid as u64).copied())
}

/// 已被 deadline 节流睡眠的任务到期时，恢复 CBS 预算。
/// 调用方必须持有 PROCESS_TABLE；本函数只做 O(1) pid 查找，绝不扫描进程表。
/// 锁序：PROCESS_TABLE → RT_SCHEDULER（与 sys_sched_setattr 一致）。
pub(crate) fn replenish_deadline_task(table: &mut ProcessTable, pid: usize) {
    let should_replenish = table.get(pid).map_or(false, |p| {
        p.sched_policy == 6 && p.waiting_for == Some(DEADLINE_RETURN_SENTINEL)
    });
    if !should_replenish {
        return;
    }
    let Some(params) = dl_params_for(pid) else {
        return;
    };
    if let Some(proc) = table.get_mut(pid) {
        proc.dl_runtime = params.wcet;
        proc.dl_deadline = params.period;
    }
}

/// 当前 DEADLINE 任务预算耗尽：挂到 hrtimer 到期树，到下一个周期边界由
/// `check_sleep_deadlines` 定点唤醒并调用 `replenish_deadline_task`。
/// 调用方必须持有 PROCESS_TABLE。
fn throttle_current_locked(table: &mut ProcessTable, now_ticks: u64) {
    let Some(current_pid) = table.current else {
        return;
    };
    let Some(params) = dl_params_for(current_pid) else {
        return;
    };
    let exhausted = table.get(current_pid).map_or(false, |p| {
        p.sched_policy == 6 && p.dl_runtime == 0
    });
    if !exhausted {
        return;
    }
    let wake_ticks = now_ticks.saturating_add(ns_to_rdtime(params.period));
    DEADLINE_MISSES.fetch_add(1, AtomicOrdering::Relaxed);
    // mark_sleeping 会经 set_process_state 更新无锁负载镜像、登记 sleep_timer
    // 并给 RT/DEADLINE 任务写 RT_SLEEP 快查槽。
    table.mark_sleeping(current_pid, Some(DEADLINE_RETURN_SENTINEL), wake_ticks as usize);
    crate::sched::process::set_need_resched();
}

/// Called from timer_tick().
///
/// 旧实现每次 tick 持 PROCESS_TABLE 全表扫描所有 `sched_policy == 6` 的任务；
/// 本实现对标 Linux `update_curr_dl` + `dl_task_timer`：
/// - 当前任务 runtime 通过 per-hart `HART_CURRENT_PCB` 无锁记账；
/// - 节流/恢复走 `sleep_timers` 到期树，只 O(1) 触碰到期 pid；
/// - 稳态无 DEADLINE 任务时由 `DL_EVER_ACTIVE` sticky 位继续短路。
pub fn dl_timer_tick() {
    if !crate::sched::process::DL_EVER_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
        return;
    }
    let now_ticks = crate::arch::asm_helpers::rdtime();
    let hart = crate::sched::process::current_hart_slot();
    let last = LAST_DL_TICK[hart].swap(now_ticks, AtomicOrdering::Relaxed);
    let delta_ns = if last == 0 {
        10_000_000
    } else {
        rdtime_to_ns(now_ticks.saturating_sub(last))
    };
    advance_system_time(delta_ns);

    // 1) 当前 Running 的 DEADLINE 任务 runtime 记账（无表锁快路径）。
    let pcb_ptr = HART_CURRENT_PCB.load(core::sync::atomic::Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let proc = unsafe { &mut *pcb_ptr };
        if proc.sched_policy == 6 && proc.dl_runtime > 0 {
            proc.dl_runtime = proc.dl_runtime.saturating_sub(delta_ns);
            if proc.dl_runtime == 0 {
                let mut guard = PROCESS_TABLE.lock();
                if let Some(table) = guard.as_mut() {
                    throttle_current_locked(table, now_ticks);
                }
            }
        }
        return;
    }

    // 2) 极早期影子指针尚未发布：只对 current 做一次点查，依然不扫描全表。
    let mut guard = PROCESS_TABLE.lock();
    let Some(table) = guard.as_mut() else {
        return;
    };
    let Some(current_pid) = table.current else {
        return;
    };
    let exhausted = match table.get_mut(current_pid) {
        Some(proc) if proc.sched_policy == 6 && proc.dl_runtime > 0 => {
            proc.dl_runtime = proc.dl_runtime.saturating_sub(delta_ns);
            proc.dl_runtime == 0
        }
        _ => false,
    };
    if exhausted {
        throttle_current_locked(table, now_ticks);
    }
}
