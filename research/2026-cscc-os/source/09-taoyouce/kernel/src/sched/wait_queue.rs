use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::sched::cpu_rq::{WF_SYNC, WF_TTWU};
use crate::sched::process::{note_waitqueue_wake, Pid, ProcessState, PROCESS_TABLE};
use crate::sync::SpinLock;

/// Linux `wake_up_interruptible_sync_poll`：pipe 读写对端马上要睡，必须 WF_SYNC。
fn wake_flags(reason: WaitReason) -> u32 {
    match reason {
        WaitReason::PipeRead | WaitReason::PipeWrite => WF_TTWU | WF_SYNC,
        _ => WF_TTWU,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitReason {
    Io,
    PipeRead,
    PipeWrite,
    Futex(usize),
    Poll,
    Signal,
    Deadline,
    Custom(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitEntry {
    pub pid: Pid,
    pub reason: WaitReason,
    pub deadline: Option<u64>,
}

/// Linux-style wait-queue function entry (see `ep_ptable_queue_proc` /
/// `ep_poll_callback` in `fs/eventpoll.c`).
///
/// Unlike task waiters, poll hooks are **not** drained by `drain` /
/// `wake_all` — they stay attached from `epoll_ctl(ADD)` until
/// `epoll_ctl(DEL)` / file release, matching Linux `eppoll_entry` lifetime.
pub trait PollWakeHook: Send + Sync {
    fn hook_id(&self) -> usize;
    /// `key` is a Linux `__poll_t` bitset (EPOLLIN/EPOLLOUT/EPOLLHUP/…).
    fn wake(&self, key: u32);
}

pub struct WaitQueue {
    entries: SpinLock<Vec<WaitEntry>>,
    /// Persistent poll/epoll hooks (not removed by task wakeups).
    hooks: SpinLock<Vec<Arc<dyn PollWakeHook>>>,
}

impl WaitQueue {
    pub fn new() -> Self {
        Self {
            entries: SpinLock::new(Vec::new()),
            hooks: SpinLock::new(Vec::new()),
        }
    }

    pub fn add_poll_hook(&self, hook: Arc<dyn PollWakeHook>) {
        let id = hook.hook_id();
        let mut hooks = self.hooks.lock();
        if !hooks.iter().any(|h| h.hook_id() == id) {
            hooks.push(hook);
        }
    }

    pub fn remove_poll_hook(&self, id: usize) {
        self.hooks.lock().retain(|h| h.hook_id() != id);
    }

    pub fn poll_hook_snapshot(&self) -> Vec<Arc<dyn PollWakeHook>> {
        self.hooks.lock().clone()
    }

    /// Fire persistent hooks with a poll key. Safe to call after dropping
    /// file locks — hooks may take PROCESS_TABLE / epoll locks.
    pub fn notify_poll_hooks(&self, key: u32) {
        fire_poll_hooks(&self.poll_hook_snapshot(), key);
    }

    /// Drain task waiters + snapshot poll hooks. Caller must drop file locks
    /// before `complete_wake` (hooks may take PROCESS_TABLE / epoll locks).
    pub fn prepare_wake(&self) -> (DrainedWaiters, Vec<Arc<dyn PollWakeHook>>) {
        (self.drain_batch(), self.poll_hook_snapshot())
    }

    /// Full drain of task waiters + hook snapshot (close / HUP paths).
    pub fn prepare_wake_all(&self) -> (Vec<WaitEntry>, Vec<Arc<dyn PollWakeHook>>) {
        (self.drain(), self.poll_hook_snapshot())
    }

    /// `wake_all_with` for task waiters, then fire persistent poll hooks.
    pub fn wake_all_with_key(&self, key: u32, clear_wait: bool) -> usize {
        let hooks = self.poll_hook_snapshot();
        let n = self.wake_all_with(clear_wait);
        fire_poll_hooks(&hooks, key);
        n
    }

    pub fn len(&self) -> usize {
        self.entries.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.lock().is_empty()
    }

    pub fn enqueue(&self, entry: WaitEntry) -> bool {
        let mut entries = self.entries.lock();
        if let Some(existing) = entries.iter_mut().find(|item| item.pid == entry.pid) {
            *existing = entry;
            false
        } else {
            entries.push(entry);
            true
        }
    }

    pub fn enqueue_pid(&self, pid: Pid, reason: WaitReason, deadline: Option<u64>) -> bool {
        self.enqueue(WaitEntry { pid, reason, deadline })
    }

    pub fn remove_pid(&self, pid: Pid) -> bool {
        let mut entries = self.entries.lock();
        let old_len = entries.len();
        entries.retain(|entry| entry.pid != pid);
        entries.len() != old_len
    }

    /// 摘出第一个指定 reason 的 entry（保持其余顺序）。pipe 直递路径用它
    /// 只挑真正阻塞在 read(2) 的 waiter，跳过 Poll 等其他等待者。
    pub fn dequeue_reason(&self, reason: WaitReason) -> Option<WaitEntry> {
        let mut entries = self.entries.lock();
        let idx = entries.iter().position(|e| e.reason == reason)?;
        Some(entries.remove(idx))
    }

    /// 只读列举等待者 pid（取证用，不改变队列状态）。
    pub fn snapshot_pids(&self) -> Vec<usize> {
        self.entries.lock().iter().map(|e| e.pid).collect()
    }

    pub fn dequeue_one(&self) -> Option<WaitEntry> {
        let mut entries = self.entries.lock();
        if entries.is_empty() {
            None
        } else {
            Some(entries.remove(0))
        }
    }

    pub fn drain(&self) -> Vec<WaitEntry> {
        let mut entries = self.entries.lock();
        let mut drained = Vec::new();
        core::mem::swap(&mut *entries, &mut drained);
        drained
    }

    /// 免堆分配 drain：前 4 个 entry 进栈上数组（`Vec::drain` 保留队列
    /// 容量，后续 enqueue 不再触发堆分配），超出部分才走堆 Vec。
    /// pipe/eventfd 这类每回合"读/写成功→唤醒对端"的热路径几乎总是
    /// 0-1 个 waiter，用它替代 `drain()` 可消除每回合 alloc/free churn。
    pub fn drain_batch(&self) -> DrainedWaiters {
        let mut entries = self.entries.lock();
        let n = entries.len();
        // 0/1 waiter 特化：pipe 每回合“读/写成功→唤醒对端”几乎总是
        // 0-1 个 waiter，免构造/搬运批结构（lat_ctx 热路径）。
        if n == 0 {
            return DrainedWaiters::None;
        }
        if n == 1 {
            return DrainedWaiters::One(entries.remove(0));
        }
        let mut inline = [None; 4];
        let take = n.min(4);
        for (slot, entry) in inline.iter_mut().zip(entries.drain(..take)) {
            *slot = Some(entry);
        }
        let overflow = if n > take { core::mem::take(&mut *entries) } else { Vec::new() };
        DrainedWaiters::Batch { inline, overflow }
    }

    /// Dequeue the entry with the highest score (per the given function).
    /// Used by PI futex to select the highest-priority waiter for ownership
    /// transfer (per Linux kernel/futex/pi.c top-waiter selection).
    pub fn dequeue_highest<F: Fn(Pid) -> i32>(&self, score_fn: F) -> Option<WaitEntry> {
        let mut entries = self.entries.lock();
        if entries.is_empty() {
            return None;
        }
        let mut best_idx = 0usize;
        let mut best_score = score_fn(entries[0].pid);
        for (idx, entry) in entries.iter().enumerate().skip(1) {
            let s = score_fn(entry.pid);
            if s > best_score {
                best_score = s;
                best_idx = idx;
            }
        }
        Some(entries.remove(best_idx))
    }

    /// 真精确计数：在不破坏队列顺序的情况下统计满足 `pred` 的 entry 数。
    /// 用于 Linux SysV `semctl(GETNCNT/GETZCNT)` 这类需要 introspect "有多少
    /// 进程正在 sem_num 上等待 sem<0 / sem==0" 的命令，对应内核内
    /// `count_semncnt` / `count_semzcnt`（`ipc/sem.c`）逻辑：扫该 semid 的
    /// pending operation 链，按 `sem_num + 等待条件` 计数。
    pub fn count_matching<F: Fn(&WaitEntry) -> bool>(&self, pred: F) -> usize {
        let entries = self.entries.lock();
        entries.iter().filter(|e| pred(*e)).count()
    }

    /// 真精确快照：拷贝当前所有 entry，**不**改变队列状态。供
    /// `GETALL` / `GETZCNT` 等 introspection 用，与 Linux `count_*` 一致。
    pub fn snapshot(&self) -> Vec<WaitEntry> {
        self.entries.lock().clone()
    }

    pub fn wake_one(&self) -> Option<Pid> {
        self.wake_one_with(true)
    }

    pub fn wake_one_with(&self, clear_wait: bool) -> Option<Pid> {
        while let Some(entry) = self.dequeue_one() {
            if wake_entry(entry, clear_wait) {
                return Some(entry.pid);
            }
        }
        None
    }

    pub fn wake_all(&self) -> usize {
        self.wake_all_with(true)
    }

    pub fn wake_all_with(&self, clear_wait: bool) -> usize {
        // Small-batch fast path: pull waiters into a stack buffer (FIFO,
        // `Vec::drain` keeps the queue's capacity) so the per-wake path does
        // no heap alloc/free churn. Almost every queue has 0-4 waiters.
        let mut woken = 0usize;
        loop {
            let mut batch = [None::<WaitEntry>; 4];
            let n = {
                let mut entries = self.entries.lock();
                let take = entries.len().min(4);
                for (slot, entry) in batch.iter_mut().zip(entries.drain(..take)) {
                    *slot = Some(entry);
                }
                take
            };
            if n == 0 {
                return woken;
            }
            // 整批只拿一次 PROCESS_TABLE 锁（hackbench 类多写者被同一次
            // recv 唤醒时，逐 entry 反复拿全局锁在锁竞争下是毫秒级尾延）。
            let mut missed: [Option<WaitEntry>; 4] = [None; 4];
            {
                let mut guard = PROCESS_TABLE.lock();
                if let Some(table) = guard.as_mut() {
                    for (i, entry) in batch.iter().flatten().enumerate() {
                        match table.get(entry.pid) {
                            Some(p) if p.state == ProcessState::Sleeping => {
                                if table.mark_runnable_wf(entry.pid, clear_wait, wake_flags(entry.reason)) {
                                    woken += 1;
                                }
                            }
                            Some(_) => missed[i] = Some(*entry),
                            None => {}
                        }
                    }
                }
            }
            for entry in missed.iter().flatten() {
                if note_waitqueue_wake(entry.pid, clear_wait) {
                    woken += 1;
                }
            }
            if n < 4 {
                return woken;
            }
        }
    }
}

/// `drain_batch` 的免堆分配产物：0/1 个 waiter 时零搬运，≤4 个全在栈上。
pub enum DrainedWaiters {
    None,
    One(WaitEntry),
    Batch { inline: [Option<WaitEntry>; 4], overflow: Vec<WaitEntry> },
}

/// 免堆分配唤醒（配合 `drain_batch`）：语义与 `wake_entries` 一致——
/// 整批只拿一次 PROCESS_TABLE 锁，未处于 Sleeping 的 entry 锁外走
/// note_waitqueue_wake；空批零锁直接返回；单 waiter 走最短路径。
pub fn wake_drained(d: DrainedWaiters, clear_wait: bool) -> usize {
    let (inline, overflow) = match d {
        DrainedWaiters::None => return 0,
        DrainedWaiters::One(entry) => {
            let missed = {
                let mut guard = PROCESS_TABLE.lock();
                match guard.as_mut() {
                    Some(table) => match table.get(entry.pid) {
                        Some(p) if p.state == ProcessState::Sleeping => {
                            return table.mark_runnable_wf(
                                entry.pid,
                                clear_wait,
                                wake_flags(entry.reason),
                            ) as usize;
                        }
                        Some(_) => true,
                        None => false,
                    },
                    None => false,
                }
            };
            if missed && note_waitqueue_wake(entry.pid, clear_wait) {
                return 1;
            }
            return 0;
        }
        DrainedWaiters::Batch { inline, overflow } => (inline, overflow),
    };
    let mut woken = 0usize;
    let mut missed: [Option<WaitEntry>; 4] = [None; 4];
    let mut missed_overflow: Vec<WaitEntry> = Vec::with_capacity(overflow.len());
    {
        let mut guard = PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            for (i, entry) in inline.iter().flatten().enumerate() {
                match table.get(entry.pid) {
                    Some(p) if p.state == ProcessState::Sleeping => {
                        if table.mark_runnable_wf(entry.pid, clear_wait, wake_flags(entry.reason)) {
                            woken += 1;
                        }
                    }
                    Some(_) => missed[i] = Some(*entry),
                    None => {}
                }
            }
            for entry in &overflow {
                match table.get(entry.pid) {
                    Some(p) if p.state == ProcessState::Sleeping => {
                        if table.mark_runnable_wf(entry.pid, clear_wait, wake_flags(entry.reason)) {
                            woken += 1;
                        }
                    }
                    Some(_) => missed_overflow.push(*entry),
                    None => {}
                }
            }
        }
    }
    for entry in missed.iter().flatten() {
        if note_waitqueue_wake(entry.pid, clear_wait) {
            woken += 1;
        }
    }
    for entry in missed_overflow {
        if note_waitqueue_wake(entry.pid, clear_wait) {
            woken += 1;
        }
    }
    woken
}

pub fn wake_entries(entries: Vec<WaitEntry>, clear_wait: bool) -> usize {
    // 整批只拿一次 PROCESS_TABLE 锁（与 `wake_all_with` 同理）；未处于
    // Sleeping 的 entry 记入 missed，锁外走 note_waitqueue_wake。
    // 空列表必须零锁直接返回：调用方（pipe drop 路径）可能已持有
    // PROCESS_TABLE。missed 预分配容量，锁内 push 不触发堆分配。
    if entries.is_empty() {
        return 0;
    }
    let mut woken = 0usize;
    let mut missed: Vec<WaitEntry> = Vec::with_capacity(entries.len());
    {
        let mut guard = PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            for entry in &entries {
                match table.get(entry.pid) {
                    Some(p) if p.state == ProcessState::Sleeping => {
                        if table.mark_runnable_wf(entry.pid, clear_wait, wake_flags(entry.reason)) {
                            woken += 1;
                        }
                    }
                    Some(_) => missed.push(*entry),
                    None => {}
                }
            }
        }
    }
    for entry in missed {
        if note_waitqueue_wake(entry.pid, clear_wait) {
            woken += 1;
        }
    }
    woken
}

/// Invoke poll hooks outside file locks (Linux `wake_up` → `func` entries).
pub fn fire_poll_hooks(hooks: &[Arc<dyn PollWakeHook>], key: u32) {
    for hook in hooks {
        hook.wake(key);
    }
}

/// Finish a `prepare_wake` after file locks are dropped.
pub fn complete_wake(
    drained: DrainedWaiters,
    hooks: &[Arc<dyn PollWakeHook>],
    key: u32,
    clear_wait: bool,
) -> usize {
    let n = wake_drained(drained, clear_wait);
    fire_poll_hooks(hooks, key);
    n
}

/// Finish a `prepare_wake_all` after file locks are dropped.
pub fn complete_wake_all(
    entries: Vec<WaitEntry>,
    hooks: &[Arc<dyn PollWakeHook>],
    key: u32,
    clear_wait: bool,
) -> usize {
    let n = wake_entries(entries, clear_wait);
    fire_poll_hooks(hooks, key);
    n
}

impl Default for WaitQueue {
    fn default() -> Self {
        Self::new()
    }
}

fn wake_entry(entry: WaitEntry, clear_wait: bool) -> bool {
    let mut guard = PROCESS_TABLE.lock();
    let Some(table) = guard.as_mut() else {
        return false;
    };
    let Some(process) = table.get(entry.pid) else {
        return false;
    };
    if process.state != ProcessState::Sleeping {
        drop(guard);
        return note_waitqueue_wake(entry.pid, clear_wait);
    }
    table.mark_runnable_wf(entry.pid, clear_wait, wake_flags(entry.reason))
}
