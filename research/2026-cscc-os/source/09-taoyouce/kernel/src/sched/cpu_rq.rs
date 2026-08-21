//! 生产路径的 per-CPU runqueue，对标 Linux `struct rq` / `cfs_rq`（v6.8+ EEVDF）。
//!
//! 权威对照（2026-08 检索，Linux v6.16 `kernel/sched/{core,fair}.c`）：
//! - `__schedule()` **只锁本地** `rq->lock`，在本地数据结构上选下一个任务。
//! - 多 rq 加锁按地址/cpu 升序（`double_rq_lock` / `rq_order_less`）。
//! - 6.8：`tasks_timeline` **按 virtual deadline 排序**，子树缓存 `min_vruntime`
//!   以便 `__pick_eevdf` 在 O(log n) 内剪掉不可 eligible 的左子树。
//! - 6.8 O(1) 快路径：最左节点若 eligible 直接取（Abel Wu）。
//! - `try_to_wake_up` → `select_task_rq_fair` → `wake_affine` /
//!   `select_idle_sibling`；`WF_SYNC` = 唤醒者马上睡（Linus 2019-12
//!   `1b6b26ae7053`：GNU make jobserver / rustc `-j` 丢同步唤醒会丢并行度）。
//! - `ttwu_queue_wakelist`：跨核只挂 `wake_list` + IPI，目标 CPU 在
//!   `sched_ttwu_pending` 本地入树。
//! - `sched_balance_newidle` + scx_rusty greedy steal：空闲核立刻从忙源拉活。
//! - Redox `context/switch.rs`：同样 EEVDF（vtime + vd + eligible）。
//!
//! 下标 = 硬件 hart id。队列不进 `ProcessTable`。锁序：
//! `PROCESS_TABLE` → `PERCPU_RQ[lo]` → `PERCPU_RQ[hi]`（`lo < hi`）。
//! 持 rq 锁时禁止再取 `PROCESS_TABLE`。

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::sync::SpinLock;

use super::process::{Pid, INVALID_PID, NICE_0_WEIGHT};
use super::smp::MAX_CPUS;

/// `try_to_wake_up` 风格标志（Linux `WF_*`，`include/linux/sched.h`）。
pub const WF_TTWU: u32 = 1 << 0;
pub const WF_SYNC: u32 = 1 << 1;
pub const WF_FORK: u32 = 1 << 2;
pub const WF_EXEC: u32 = 1 << 3;
pub const WF_MIGRATED: u32 = 1 << 4;

// 调度策略常量与纯函数见 `super::policy`；此处只保留 runqueue 数据结构
// 自身需要的 PELT 衰减常量。
/// PELT 周期 1024µs；`y^32 = 0.5` ≈ 每周期 ×984/1024。
const PELT_PERIOD_NS: u64 = 1_024_000;
const PELT_DECAY_NUM: u64 = 984;
const PELT_DECAY_DEN: u64 = 1024;

/// CFS 树上的调度实体（Linux `sched_entity` 的可运行子集）。
#[derive(Clone, Copy)]
pub struct CfsEntity {
    pub pid: Pid,
    pub vruntime: u64,
    pub deadline: u64,
    pub weight: u32,
}

/// deadline 序节点（Linux 6.8 `tasks_timeline` 增广红黑树的 AVL 等价实现）。
///
/// 旧实现用随机优先级 treap：仅**期望** O(log n)，存在确定性退化与随机性
/// 不透明两个简化。这里改成严格高度平衡的 AVL 树：
/// - 查找/插入/删除最坏 O(log n)；
/// - 每个子树缓存 `min_vruntime`，`pick_eevdf` 按 eligibility 阈值剪枝；
/// - 排序键 `(virtual_deadline, pid)` 唯一，与 Linux `__dequeue_entity` /
///   `__enqueue_entity` 的 rb-tree 语义一致。
struct DlNode {
    key: (u64, Pid),
    ent: CfsEntity,
    min_vruntime: u64,
    height: u8,
    left: Option<Box<DlNode>>,
    right: Option<Box<DlNode>>,
}

#[inline]
fn dl_height(n: &Option<Box<DlNode>>) -> u8 {
    n.as_ref().map_or(0, |x| x.height)
}

fn dl_pull(n: &mut DlNode) {
    n.min_vruntime = n.ent.vruntime;
    if let Some(ref l) = n.left {
        n.min_vruntime = n.min_vruntime.min(l.min_vruntime);
    }
    if let Some(ref r) = n.right {
        n.min_vruntime = n.min_vruntime.min(r.min_vruntime);
    }
    let lh = dl_height(&n.left);
    let rh = dl_height(&n.right);
    n.height = 1 + lh.max(rh);
}

#[inline]
fn dl_balance_factor(n: &DlNode) -> i8 {
    dl_height(&n.left) as i8 - dl_height(&n.right) as i8
}

fn dl_rotate_left(mut x: Box<DlNode>) -> Box<DlNode> {
    let mut y = x.right.take().expect("AVL rotate-left without right child");
    x.right = y.left.take();
    dl_pull(&mut x);
    y.left = Some(x);
    dl_pull(&mut y);
    y
}

fn dl_rotate_right(mut x: Box<DlNode>) -> Box<DlNode> {
    let mut y = x.left.take().expect("AVL rotate-right without left child");
    x.left = y.right.take();
    dl_pull(&mut x);
    y.right = Some(x);
    dl_pull(&mut y);
    y
}

fn dl_balance(mut n: Box<DlNode>) -> Box<DlNode> {
    dl_pull(&mut n);
    let bf = dl_balance_factor(&n);
    if bf > 1 {
        let lb = n
            .left
            .as_ref()
            .map_or(0, |l| dl_balance_factor(l));
        if lb < 0 {
            let l = n.left.take().expect("AVL left child");
            n.left = Some(dl_rotate_left(l));
        }
        return dl_rotate_right(n);
    }
    if bf < -1 {
        let rb = n
            .right
            .as_ref()
            .map_or(0, |r| dl_balance_factor(r));
        if rb > 0 {
            let r = n.right.take().expect("AVL right child");
            n.right = Some(dl_rotate_right(r));
        }
        return dl_rotate_left(n);
    }
    n
}

fn dl_insert(t: Option<Box<DlNode>>, node: Box<DlNode>) -> Box<DlNode> {
    let Some(mut n) = t else {
        return node;
    };
    if node.key < n.key {
        n.left = Some(dl_insert(n.left.take(), node));
    } else {
        n.right = Some(dl_insert(n.right.take(), node));
    }
    dl_balance(n)
}

/// 删除 `key`，返回 `(新根, 被摘除节点)`。调用方保证 key 存在。
fn dl_remove(
    t: Option<Box<DlNode>>,
    key: (u64, Pid),
) -> (Option<Box<DlNode>>, Option<Box<DlNode>>) {
    let Some(mut n) = t else {
        return (None, None);
    };
    if key < n.key {
        let (new_left, removed) = dl_remove(n.left.take(), key);
        n.left = new_left;
        (Some(dl_balance(n)), removed)
    } else if key > n.key {
        let (new_right, removed) = dl_remove(n.right.take(), key);
        n.right = new_right;
        (Some(dl_balance(n)), removed)
    } else {
        let left = n.left.take();
        let right = n.right.take();
        let new_root = match (left, right) {
            (None, r) => r,
            (l, None) => l,
            (Some(l), Some(r)) => {
                let (new_right, mut succ) = dl_take_min(r);
                succ.left = Some(l);
                succ.right = new_right;
                Some(dl_balance(succ))
            }
        };
        (new_root, Some(n))
    }
}

/// 取子树最小节点，返回 `(移除最小节点后的子树, 最小节点)`。
fn dl_take_min(mut n: Box<DlNode>) -> (Option<Box<DlNode>>, Box<DlNode>) {
    if n.left.is_some() {
        let l = n.left.take().expect("AVL left child");
        let (new_left, min) = dl_take_min(l);
        n.left = new_left;
        (Some(dl_balance(n)), min)
    } else {
        (n.right.take(), n)
    }
}

fn dl_leftmost(n: &DlNode) -> &CfsEntity {
    match n.left {
        Some(ref l) => dl_leftmost(l),
        None => &n.ent,
    }
}

/// 单个 CPU 的 CFS/EEVDF 子队列（Linux `struct cfs_rq`）。
pub struct CfsRq {
    /// deadline 序时间线（Linux `tasks_timeline`，6.8+ 按 vd 排序）。
    root: Option<Box<DlNode>>,
    /// pid → (deadline, vruntime)，O(log n) 确定性索引（BTreeMap）。
    pid_index: BTreeMap<Pid, (u64, u64)>,
    count: usize,
    pub min_vruntime: u64,
    pub sum_weight: u64,
    /// `Σ (vruntime - min_vruntime) * weight`（Linux `cfs_rq->avg_vruntime`，
    /// 带符号 lag 和）。用它计算 `avg_vruntime = min_vruntime + lag_sum/load`，
    /// 避免旧实现 `Σ(vruntime*weight)/Σweight` 在 min_vruntime 推进后的
    /// 大数漂移。
    lag_sum: i128,
}

impl CfsRq {
    pub const fn empty() -> Self {
        Self {
            root: None,
            pid_index: BTreeMap::new(),
            count: 0,
            min_vruntime: 0,
            sum_weight: 0,
            lag_sum: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    #[inline]
    pub fn contains(&self, pid: Pid) -> bool {
        self.pid_index.contains_key(&pid)
    }

    pub fn contains_other(&self, current: Pid) -> bool {
        self.pid_index
            .keys()
            .any(|&pid| pid != current && pid != 0)
    }

    #[inline]
    fn lookup_pid(&self, pid: Pid) -> Option<(u64, u64)> {
        self.pid_index.get(&pid).copied()
    }

    /// Linux `avg_vruntime()`：`min_vruntime + div_s64(avg_vruntime, avg_load)`。
    #[inline]
    pub fn avg_vruntime(&self) -> u64 {
        if self.sum_weight == 0 {
            return self.min_vruntime;
        }
        let avg = self.min_vruntime as i128 + self.lag_sum / self.sum_weight as i128;
        if avg < 0 {
            0
        } else {
            avg as u64
        }
    }

    /// `pick_eevdf` 剪枝阈值：子树 `min_vruntime <= threshold` 才可能 eligible。
    /// 由 `entity_eligible` 的整数形式 `avg_vruntime >= (v - min)*load` 反解：
    /// `v <= min + ceil(avg_vruntime / load)`。向上取整保证只多剪枝、不漏剪枝。
    #[inline]
    fn eligibility_threshold(&self) -> u64 {
        let load = self.sum_weight;
        if load == 0 {
            return u64::MAX;
        }
        let avg = self.avg_vruntime() as i128;
        let load = load as i128;
        let q = (avg + load - 1) / load;
        let thr = self.min_vruntime as i128 + q;
        if thr < 0 {
            0
        } else {
            thr as u64
        }
    }

    /// Linux `vruntime_eligible(cfs_rq, se)` 的整数形式：
    /// `avg_vruntime >= (vruntime - min_vruntime) * avg_load`。
    ///
    /// 旧实现简化为 `vruntime <= avg_vruntime()`，丢失了 `min_vruntime`
    /// 基准与 load 权重，导致高负载下 eligibility 判定偏移一个量级。
    #[inline]
    pub fn entity_eligible(&self, vruntime: u64) -> bool {
        let load = self.sum_weight;
        if load == 0 {
            return true;
        }
        let avg = self.avg_vruntime();
        let key = vruntime.saturating_sub(self.min_vruntime);
        avg >= key.saturating_mul(load)
    }

    /// 推进 `min_vruntime` 并保持 lag_sum 不变式：
    /// `lag_sum' = lag_sum - (new_min - old_min) * load`。
    fn set_min_vruntime(&mut self, new_min: u64) {
        if new_min == self.min_vruntime {
            return;
        }
        let delta = new_min as i128 - self.min_vruntime as i128;
        self.lag_sum -= delta * self.sum_weight as i128;
        self.min_vruntime = new_min;
    }

    pub fn enqueue(&mut self, ent: CfsEntity) {
        if ent.pid == INVALID_PID || self.contains(ent.pid) {
            return;
        }
        let old_min = self.min_vruntime;
        self.lag_sum += (ent.vruntime as i128 - old_min as i128) * ent.weight as i128;
        self.sum_weight = self.sum_weight.saturating_add(ent.weight as u64);

        let key = (ent.deadline, ent.pid);
        let node = Box::new(DlNode {
            key,
            ent,
            min_vruntime: 0,
            height: 1,
            left: None,
            right: None,
        });
        self.root = Some(dl_insert(self.root.take(), node));
        self.pid_index.insert(ent.pid, (ent.deadline, ent.vruntime));
        self.count = self.count.saturating_add(1);

        // 保持 min_vruntime = min(左边界 vruntime)。ent 比当前基准更小
        // 时必须连同 lag_sum 一起重定基准。
        if self.min_vruntime == 0 || ent.vruntime < self.min_vruntime {
            self.set_min_vruntime(ent.vruntime);
        }
    }

    pub fn remove(&mut self, pid: Pid) -> Option<CfsEntity> {
        let (deadline, _vrt) = self.lookup_pid(pid)?;
        let key = (deadline, pid);
        let (new_root, removed) = dl_remove(self.root.take(), key);
        self.root = new_root;
        let ent = removed.map(|n| n.ent)?;

        self.pid_index.remove(&pid);
        self.count = self.count.saturating_sub(1);
        let old_min = self.min_vruntime;
        self.lag_sum -= (ent.vruntime as i128 - old_min as i128) * ent.weight as i128;
        self.sum_weight = self.sum_weight.saturating_sub(ent.weight as u64);

        // 删除后以新树最左节点重定 min_vruntime；树空归零。
        let new_min = self.root.as_ref().map_or(0, |r| r.min_vruntime);
        if new_min != self.min_vruntime {
            self.set_min_vruntime(new_min);
        }
        Some(ent)
    }

    pub fn only_pid(&self) -> Option<Pid> {
        if self.count != 1 {
            return None;
        }
        self.root.as_ref().map(|n| n.ent.pid)
    }

    pub fn pids(&self) -> Vec<Pid> {
        self.pid_index.keys().copied().collect()
    }

    /// Linux 6.8+ `__pick_eevdf`：
    /// 1) 最左（最早 deadline）若 eligible → O(1)；
    /// 2) 否则用增广 `min_vruntime` 按 eligibility 阈值剪枝搜索；
    /// 3) 理论不变量下第 2 步总能命中；保留最左回退仅防御数值不变量破坏。
    pub fn pick_eevdf<F>(&self, mut ok: F) -> Option<Pid>
    where
        F: FnMut(Pid) -> bool,
    {
        let root = self.root.as_deref()?;
        let first = dl_leftmost(root);
        if self.entity_eligible(first.vruntime) && ok(first.pid) {
            return Some(first.pid);
        }
        if let Some(pid) = self.pick_heap(root, &mut ok) {
            return Some(pid);
        }
        if ok(first.pid) {
            return Some(first.pid);
        }
        self.pick_any(root, &mut ok)
    }

    fn pick_heap<F>(&self, n: &DlNode, ok: &mut F) -> Option<Pid>
    where
        F: FnMut(Pid) -> bool,
    {
        let threshold = self.eligibility_threshold();
        if let Some(ref left) = n.left {
            if left.min_vruntime <= threshold {
                if let Some(pid) = self.pick_heap(left, ok) {
                    return Some(pid);
                }
            }
        }
        if self.entity_eligible(n.ent.vruntime) && ok(n.ent.pid) {
            return Some(n.ent.pid);
        }
        if let Some(ref right) = n.right {
            if right.min_vruntime <= threshold {
                return self.pick_heap(right, ok);
            }
        }
        None
    }

    fn pick_any<F>(&self, n: &DlNode, ok: &mut F) -> Option<Pid>
    where
        F: FnMut(Pid) -> bool,
    {
        if let Some(ref left) = n.left {
            if let Some(pid) = self.pick_any(left, ok) {
                return Some(pid);
            }
        }
        if ok(n.ent.pid) {
            return Some(n.ent.pid);
        }
        n.right.as_deref().and_then(|r| self.pick_any(r, ok))
    }
}

/// Linux `MAX_RT_PRIO`：0..=99，bitmap bit 0 = 最高优先级。
const RT_QUEUE_LEVELS: usize = 100;

/// SCHED_FIFO / SCHED_RR 优先级队列（Linux `rt_prio_array` 的 bitmap+list
/// 结构等价实现）。队列下标 `99 - effective_priority`，最高优先级下标 0。
pub struct RtRq {
    bitmap: u128,
    queues: [VecDeque<Pid>; RT_QUEUE_LEVELS],
    /// pid → effective priority，O(log n) 去重/摘除。
    index: BTreeMap<Pid, u8>,
    count: usize,
}

impl RtRq {
    pub const fn empty() -> Self {
        Self {
            bitmap: 0,
            queues: [const { VecDeque::new() }; RT_QUEUE_LEVELS],
            index: BTreeMap::new(),
            count: 0,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.count
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn queue_idx(priority: u8) -> usize {
        (RT_QUEUE_LEVELS - 1).saturating_sub(priority.min(99) as usize)
    }

    pub fn enqueue(&mut self, pid: Pid, priority: u8) -> bool {
        if pid == INVALID_PID || self.index.contains_key(&pid) {
            return false;
        }
        let prio = priority.min(99);
        let idx = Self::queue_idx(prio);
        if self.queues[idx].is_empty() {
            self.bitmap |= 1u128 << idx;
        }
        self.queues[idx].push_back(pid);
        self.index.insert(pid, prio);
        self.count += 1;
        true
    }

    pub fn remove(&mut self, pid: Pid) -> bool {
        let Some(prio) = self.index.remove(&pid) else {
            return false;
        };
        let idx = Self::queue_idx(prio);
        self.queues[idx].retain(|&p| p != pid);
        if self.queues[idx].is_empty() {
            self.bitmap &= !(1u128 << idx);
        }
        self.count -= 1;
        true
    }

    #[inline]
    pub fn contains(&self, pid: Pid) -> bool {
        self.index.contains_key(&pid)
    }

    #[inline]
    pub fn first(&self) -> Option<Pid> {
        let idx = self.bitmap.trailing_zeros() as usize;
        if idx >= RT_QUEUE_LEVELS {
            None
        } else {
            self.queues[idx].front().copied()
        }
    }

    #[inline]
    pub fn contains_other(&self, current: Pid) -> bool {
        self.count > 0 && (self.count > 1 || !self.index.contains_key(&current))
    }

    /// 按优先级 bitmap 遍历全部 RT pid，`f(pid)` 命中即停。零堆分配版本，
    /// 供持 PT 的 RT 抢占检查在锁内直接扫描。
    #[inline]
    pub fn any_priority(&self, mut f: impl FnMut(Pid) -> bool) -> bool {
        let mut bits = self.bitmap;
        while bits != 0 {
            let idx = bits.trailing_zeros() as usize;
            for &pid in &self.queues[idx] {
                if f(pid) {
                    return true;
                }
            }
            bits &= bits - 1;
        }
        false
    }

    /// 按 Linux RT 语义（高优先级优先、同优先级 FIFO）输出 pid 快照。
    pub fn pids_by_priority(&self) -> Vec<Pid> {
        let mut out = Vec::with_capacity(self.count);
        let mut bits = self.bitmap;
        while bits != 0 {
            let idx = bits.trailing_zeros() as usize;
            out.extend(self.queues[idx].iter().copied());
            bits &= bits - 1;
        }
        out
    }
}

/// SCHED_DEADLINE EDF 队列（Linux `dl_rq` 红黑树的确定性 BTreeMap 等价实现）。
pub struct DlRq {
    /// 键 = (absolute_deadline, pid)。deadline=0 视为无效，排到 u64::MAX。
    by_deadline: BTreeMap<(u64, Pid), ()>,
    /// pid → deadline 二级索引，删除/去重 O(log n)，不再扫描 EDF 树。
    index: BTreeMap<Pid, u64>,
}

impl DlRq {
    pub const fn empty() -> Self {
        Self {
            by_deadline: BTreeMap::new(),
            index: BTreeMap::new(),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.by_deadline.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.by_deadline.is_empty()
    }

    pub fn enqueue(&mut self, pid: Pid, deadline: u64) -> bool {
        if pid == INVALID_PID || self.index.contains_key(&pid) {
            return false;
        }
        let key = (if deadline == 0 { u64::MAX } else { deadline }, pid);
        self.by_deadline.insert(key, ());
        self.index.insert(pid, deadline);
        true
    }

    pub fn remove(&mut self, pid: Pid, deadline: u64) -> bool {
        let key = (if deadline == 0 { u64::MAX } else { deadline }, pid);
        if self.by_deadline.remove(&key).is_some() {
            self.index.remove(&pid);
            true
        } else {
            false
        }
    }

    #[inline]
    pub fn first(&self) -> Option<Pid> {
        self.by_deadline.keys().next().map(|&(_, pid)| pid)
    }

    pub fn pids_edf(&self) -> Vec<Pid> {
        self.by_deadline.keys().map(|&(_, pid)| pid).collect()
    }

    /// EDF 序遍历全部 DL pid，`f(pid)` 命中即停。零堆分配版本。
    #[inline]
    pub fn any_edf(&self, mut f: impl FnMut(Pid) -> bool) -> bool {
        self.by_deadline
            .keys()
            .any(|&(_, pid)| f(pid))
    }

    pub fn remove_by_pid(&mut self, pid: Pid) -> bool {
        let Some(deadline) = self.index.remove(&pid) else {
            return false;
        };
        let key = (if deadline == 0 { u64::MAX } else { deadline }, pid);
        self.by_deadline.remove(&key).is_some()
    }

    #[inline]
    pub fn contains_other(&self, current: Pid) -> bool {
        self.index.len() > 0 && (self.index.len() > 1 || !self.index.contains_key(&current))
    }
}

/// 单个 CPU 的 runqueue（Linux `struct rq`）。
pub struct CpuRq {
    pub cfs: CfsRq,
    pub rt: RtRq,
    pub dl: DlRq,
    pub wake_handoff: Pid,
    pub load_weight: u64,
    pub load_avg: u64,
    pub last_update_ns: u64,
}

impl CpuRq {
    pub const fn empty() -> Self {
        Self {
            cfs: CfsRq::empty(),
            rt: RtRq::empty(),
            dl: DlRq::empty(),
            wake_handoff: INVALID_PID,
            load_weight: 0,
            load_avg: 0,
            last_update_ns: 0,
        }
    }

    #[inline]
    pub fn cfs_len(&self) -> usize {
        self.cfs.len()
    }

    #[inline]
    pub fn queued(&self) -> usize {
        self.cfs.len()
            + self.rt.len()
            + self.dl.len()
            + usize::from(self.wake_handoff != INVALID_PID)
    }

    #[inline]
    pub fn load(&self) -> usize {
        self.queued()
    }

    #[inline]
    pub fn is_idle(&self) -> bool {
        self.cfs.is_empty()
            && self.rt.is_empty()
            && self.dl.is_empty()
            && self.wake_handoff == INVALID_PID
    }

    pub fn enqueue_cfs(&mut self, pid: Pid, vruntime: u64, deadline: u64, weight: u32) {
        self.cfs.enqueue(CfsEntity {
            pid,
            vruntime,
            deadline,
            weight: weight.max(1),
        });
        self.recompute_load();
    }

    /// 入队 SCHED_FIFO/RR。`effective_priority` = max(pi_boost, rt_priority)，
    /// 1..=99（Linux `rt_prio_array`）。
    pub fn enqueue_rt(&mut self, pid: Pid, effective_priority: u8) {
        if self.rt.enqueue(pid, effective_priority.max(1)) {
            self.recompute_load();
        }
    }

    /// 入队 SCHED_DEADLINE（EDF）。deadline=0 视为无效，排到队尾。
    pub fn enqueue_dl(&mut self, pid: Pid, deadline: u64) {
        if self.dl.enqueue(pid, deadline) {
            self.recompute_load();
        }
    }

    pub fn remove_pid(&mut self, pid: Pid) {
        if pid == INVALID_PID {
            return;
        }
        self.cfs.remove(pid);
        self.dl.remove_by_pid(pid);
        self.rt.remove(pid);
        if self.wake_handoff == pid {
            self.wake_handoff = INVALID_PID;
        }
        self.recompute_load();
    }

    pub fn take_handoff(&mut self) -> Pid {
        let pid = self.wake_handoff;
        self.wake_handoff = INVALID_PID;
        self.recompute_load();
        pid
    }

    pub fn has_other(&self, current: Pid) -> bool {
        if self.wake_handoff != INVALID_PID && self.wake_handoff != current {
            return true;
        }
        self.cfs.contains_other(current)
            || self.rt.contains_other(current)
            || self.dl.contains_other(current)
    }

    fn recompute_load(&mut self) {
        self.load_weight = self.cfs.sum_weight
            + (self.rt.len() as u64 + self.dl.len() as u64)
                .saturating_mul(NICE_0_WEIGHT as u64);
        if self.wake_handoff != INVALID_PID {
            self.load_weight = self
                .load_weight
                .saturating_add(NICE_0_WEIGHT as u64);
        }
    }

    pub fn update_pelt(&mut self, now_ns: u64) {
        if self.last_update_ns == 0 {
            self.last_update_ns = now_ns;
            self.load_avg = self.load_weight;
            return;
        }
        let delta = now_ns.saturating_sub(self.last_update_ns);
        let periods = (delta / PELT_PERIOD_NS) as u32;
        if periods == 0 {
            return;
        }
        self.last_update_ns = now_ns;
        let mut avg = self.load_avg;
        for _ in 0..periods.min(64) {
            avg = avg.saturating_mul(PELT_DECAY_NUM) / PELT_DECAY_DEN;
        }
        self.load_avg = avg.saturating_add(self.load_weight);
    }
}

/// 每 hart 一把 rq 锁。`schedule()` 只锁本地；steal / migrate 按 cpu id 升序双锁。
pub static PERCPU_RQ: [SpinLock<CpuRq>; MAX_CPUS] =
    [const { SpinLock::new(CpuRq::empty()) }; MAX_CPUS];

/// 跨核 wakelist（Linux `rq->wake_list`）。与 CFS 树分开，避免唤醒者改远程红黑树。
static WAKE_LIST: [SpinLock<Vec<Pid>>; MAX_CPUS] =
    [const { SpinLock::new(Vec::new()) }; MAX_CPUS];

/// Linux `rq->ttwu_pending`：无锁提示「有未 drain 的远程唤醒」。
pub static TTWU_PENDING: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_CPUS];

/// 无锁镜像，供 `select_task_rq` / idle 探测，不取 rq 锁。
pub static RQ_NR_RUNNING: [AtomicUsize; MAX_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_CPUS];
pub static RQ_LOAD_AVG: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
pub static RQ_LOAD_WEIGHT: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

#[inline]
pub fn lock_rq(cpu: usize) -> crate::sync::SpinLockGuard<'static, CpuRq> {
    assert!(cpu < MAX_CPUS, "lock_rq with out-of-range cpu {}", cpu);
    PERCPU_RQ[cpu].lock()
}

/// Linux `double_rq_lock`：永远先锁较小 cpu id，避免 ABBA。
#[allow(dead_code)]
pub fn lock_two_rq(
    a: usize,
    b: usize,
) -> (
    crate::sync::SpinLockGuard<'static, CpuRq>,
    Option<crate::sync::SpinLockGuard<'static, CpuRq>>,
) {
    if a == b {
        return (lock_rq(a), None);
    }
    assert!(a < MAX_CPUS && b < MAX_CPUS, "lock_two_rq out-of-range {}/{}", a, b);
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    (lock_rq(lo), Some(lock_rq(hi)))
}

pub fn publish_rq(cpu: usize, rq: &CpuRq) {
    if cpu >= MAX_CPUS {
        return;
    }
    let pending = TTWU_PENDING[cpu].load(Ordering::Acquire);
    RQ_NR_RUNNING[cpu].store(rq.queued().saturating_add(pending), Ordering::Release);
    RQ_LOAD_AVG[cpu].store(rq.load_avg.max(rq.load_weight), Ordering::Relaxed);
    RQ_LOAD_WEIGHT[cpu].store(rq.load_weight, Ordering::Relaxed);
}

#[inline]
pub fn rq_nr_running(cpu: usize) -> usize {
    if cpu >= MAX_CPUS {
        0
    } else {
        RQ_NR_RUNNING[cpu].load(Ordering::Acquire)
    }
}

#[inline]
pub fn rq_load(cpu: usize) -> u64 {
    if cpu >= MAX_CPUS {
        0
    } else {
        let avg = RQ_LOAD_AVG[cpu].load(Ordering::Relaxed);
        let w = RQ_LOAD_WEIGHT[cpu].load(Ordering::Relaxed);
        avg.max(w)
    }
}

pub fn any_queued() -> bool {
    RQ_NR_RUNNING.iter().any(|n| n.load(Ordering::Relaxed) != 0)
        || TTWU_PENDING.iter().any(|n| n.load(Ordering::Relaxed) != 0)
}

pub fn push_wakelist(cpu: usize, pid: Pid) {
    if cpu >= MAX_CPUS || pid == INVALID_PID {
        return;
    }
    let mut g = WAKE_LIST[cpu].lock();
    if !g.contains(&pid) {
        g.push(pid);
        TTWU_PENDING[cpu].fetch_add(1, Ordering::Release);
        RQ_NR_RUNNING[cpu].fetch_add(1, Ordering::Release);
    }
}

pub fn take_wakelist(cpu: usize) -> Vec<Pid> {
    if cpu >= MAX_CPUS {
        return Vec::new();
    }
    let mut g = WAKE_LIST[cpu].lock();
    let v = core::mem::take(&mut *g);
    TTWU_PENDING[cpu].store(0, Ordering::Release);
    v
}

pub fn unlink_wakelist(pid: Pid) {
    if pid == INVALID_PID {
        return;
    }
    for cpu in 0..MAX_CPUS {
        let mut g = WAKE_LIST[cpu].lock();
        let before = g.len();
        g.retain(|p| *p != pid);
        if g.len() != before {
            TTWU_PENDING[cpu].fetch_sub(1, Ordering::Release);
            RQ_NR_RUNNING[cpu].fetch_sub(1, Ordering::Release);
        }
    }
}

/// 兼容旧构造（测试 / 诊断）。生产路径用 `PERCPU_RQ`。
#[allow(dead_code)]
pub fn new_cpu_rq_array() -> [CpuRq; MAX_CPUS] {
    core::array::from_fn(|_| CpuRq::empty())
}
