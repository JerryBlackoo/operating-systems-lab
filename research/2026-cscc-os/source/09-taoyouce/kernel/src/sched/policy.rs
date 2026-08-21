//! CFS/EEVDF 调度**策略**层（对标 Linux `kernel/sched/fair.c` 纯策略函数）
//!
//! 本模块与 `cpu_rq`（数据结构与锁）解耦：
//! - `cpu_rq`：`struct rq` / `cfs_rq` 增广树、wakelist、无锁负载镜像；
//! - `policy`：`calc_delta_fair` / `place_entity` / `select_task_rq` /
//!   `busiest_cpus` 等不依赖任何锁持有方式的纯策略。
//!
//! 依据（Linux v6.16 `kernel/sched/fair.c`）：
//! - `sysctl_sched_latency=6ms`、`sysctl_sched_min_granularity=0.75ms`、
//!   `sysctl_sched_base_slice_ns=3ms`、`sysctl_sched_wakeup_granularity=1ms`；
//! - EEVDF `place_entity` 的 lag 放置与 `entity_deadline =
//!   vruntime + calc_delta_fair(slice)`；
//! - `select_task_rq_fair` 的 idle → sync wake → wake_affine(100/110) →
//!   最轻载回退序列。

use super::cpu_rq::{rq_load, rq_nr_running};
use super::process::NICE_0_WEIGHT;
use super::smp::MAX_CPUS;

/// Linux `sysctl_sched_latency` 默认 6ms（与 `rdtime` 同一单位）。
pub const SCHED_LATENCY_NS: u64 = 6_000_000;
/// Linux `sysctl_sched_min_granularity` 默认 ~0.75ms。
pub const SCHED_MIN_GRAN_NS: u64 = 750_000;
/// Linux `sysctl_sched_base_slice_ns` 默认 3ms（EEVDF `se->slice`）。
pub const SCHED_BASE_SLICE_NS: u64 = 3_000_000;
/// Linux `sysctl_sched_wakeup_granularity` 默认 1ms。
pub const WAKEUP_GRAN_NS: u64 = 1_000_000;

/// Linux `calc_delta_fair`：`delta * NICE_0_WEIGHT / weight`。
#[inline]
pub fn calc_delta_fair(delta_ns: u64, weight: u32) -> u64 {
    let w = weight.max(1) as u64;
    delta_ns.saturating_mul(NICE_0_WEIGHT as u64) / w
}

/// 时间片：`sched_latency * weight / load`，夹在 `[min_gran, latency]`。
pub fn sched_slice_ns(weight: u32, cfs_load: u64) -> u64 {
    let w = weight.max(1) as u64;
    let load = cfs_load.max(NICE_0_WEIGHT as u64);
    let slice = SCHED_LATENCY_NS.saturating_mul(w) / load;
    slice.clamp(SCHED_MIN_GRAN_NS, SCHED_LATENCY_NS)
}

/// `se->deadline = se->vruntime + calc_delta_fair(se->slice)`。
#[inline]
pub fn entity_deadline(vruntime: u64, slice_ns: u64, weight: u32) -> u64 {
    vruntime.saturating_add(calc_delta_fair(slice_ns.max(SCHED_MIN_GRAN_NS), weight))
}

/// EEVDF `place_entity`：用 lag 把醒来的任务放到 `avg_vruntime` 附近。
/// 返回 `(vruntime, deadline, vlag)`。
pub fn place_entity(avg: u64, se_vruntime: u64, weight: u32, cfs_load: u64) -> (u64, u64, i64) {
    let slice = sched_slice_ns(weight, cfs_load);
    let mut lag = avg as i64 - se_vruntime as i64;
    let limit = calc_delta_fair(slice.saturating_mul(2).max(SCHED_MIN_GRAN_NS), weight) as i64;
    if lag > limit {
        lag = limit;
    } else if lag < -limit {
        lag = -limit;
    }
    let vrt = (avg as i64 - lag).max(0) as u64;
    let deadline = entity_deadline(vrt, slice, weight);
    (vrt, deadline, lag)
}

/// 唤醒抢占粒度（Linux `wakeup_gran`）：`WAKEUP_GRAN * NICE_0 / weight`。
pub fn wakeup_gran_ns(weight: u32) -> u64 {
    let w = weight.max(1) as u64;
    WAKEUP_GRAN_NS.saturating_mul(NICE_0_WEIGHT as u64) / w
}

/// Linux `select_task_rq_fair` + `wake_affine` + `select_idle_sibling`。
///
/// 顺序：空闲核 → `WF_SYNC` 且本核即将空 → prev 轻载 → wake_affine 100/110 → 最轻载。
pub fn select_task_rq(allowed: u64, this_cpu: usize, prev_cpu: Option<usize>, flags: u32) -> usize {
    if let Some(h) = super::process_sched::first_idle_hart_in_mask(allowed) {
        return h;
    }
    let this_ok = this_cpu < MAX_CPUS && (allowed & (1u64 << this_cpu)) != 0;
    if flags & super::cpu_rq::WF_SYNC != 0 && this_ok && rq_nr_running(this_cpu) <= 1 {
        return this_cpu;
    }
    if let Some(prev) = prev_cpu {
        if prev < MAX_CPUS && (allowed & (1u64 << prev)) != 0 {
            if rq_nr_running(prev) == 0 {
                return prev;
            }
            if this_ok {
                // wake_affine_weight：对本核乘 100，对 prev 乘 110（imbalance 偏置）。
                let this_eff = rq_load(this_cpu).saturating_mul(100);
                let prev_eff = rq_load(prev).saturating_mul(110);
                if this_eff <= prev_eff {
                    return this_cpu;
                }
                return prev;
            }
            return prev;
        }
    }
    if this_ok {
        return this_cpu;
    }
    let mut best = allowed.trailing_zeros() as usize;
    let mut best_load = u64::MAX;
    for h in 0..MAX_CPUS {
        if (allowed & (1u64 << h)) == 0 {
            continue;
        }
        let n = rq_load(h);
        if n < best_load {
            best_load = n;
            best = h;
        }
    }
    if best >= MAX_CPUS || (allowed & (1u64 << best)) == 0 {
        allowed.trailing_zeros() as usize
    } else {
        best
    }
}

/// `sched_balance_newidle`：按 `load_avg` 从高到低给出可偷源。
///
/// 输出到调用方栈上固定数组，**不在调度热路径做堆分配**（Linux
/// `find_busiest_queue` 同样只遍历在线 rq，不产生内存分配）。返回有效项数。
pub fn busiest_cpus(
    dest: usize,
    online: u64,
    out: &mut [(usize, u64); MAX_CPUS],
) -> usize {
    let mut n = 0usize;
    for h in 0..MAX_CPUS {
        if h == dest || (online & (1u64 << h)) == 0 {
            continue;
        }
        let load = rq_load(h);
        let nr = rq_nr_running(h);
        if nr == 0 {
            continue;
        }
        out[n] = (h, load.max(nr as u64));
        n += 1;
    }
    // 插入排序：CPU 数 ≤16，固定小规模下比堆分配 + sort 更稳。
    for i in 1..n {
        let mut j = i;
        while j > 0 && out[j].1 > out[j - 1].1 {
            out.swap(j, j - 1);
            j -= 1;
        }
    }
    n
}

/// 调度器时间源，统一换算为纳秒。
///
/// 历史实现直接返回 `rdtime()`，而 RV `CLOCK_FREQ=10MHz`、LA `CLOCK_FREQ=100MHz`，
/// 导致同一组 EEVDF `*_NS` 常量在两架构上语义相差 10 倍——这不是“常数差异”，
/// 是单位错误。Linux `rq_clock_task` 恒为 ns，这里同样换算：
/// `rdtime * 1_000_000_000 / CLOCK_FREQ`。
#[inline]
pub fn now_ns() -> u64 {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    crate::arch::asm_helpers::rdtime()
        .saturating_mul(1_000_000_000)
        / freq.max(1)
}

/// 策略层无锁自测试（不触碰全局 rq / PROCESS_TABLE）。
pub fn self_test() {
    assert!(calc_delta_fair(6_000_000, NICE_0_WEIGHT) == 6_000_000);
    assert!(calc_delta_fair(1_000_000, NICE_0_WEIGHT * 2) == 500_000);
    assert!(sched_slice_ns(NICE_0_WEIGHT, NICE_0_WEIGHT as u64) == SCHED_LATENCY_NS);
    let (vrt, dl, _lag) = place_entity(10_000_000, 9_000_000, NICE_0_WEIGHT, NICE_0_WEIGHT as u64);
    assert!(vrt <= 10_000_000);
    assert!(dl >= vrt);
    assert!(wakeup_gran_ns(NICE_0_WEIGHT) == WAKEUP_GRAN_NS);
}
