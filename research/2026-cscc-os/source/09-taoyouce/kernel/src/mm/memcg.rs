//! cgroup v1 memory controller (`/sys/fs/cgroup/memory/`).
//!
//! 对标 Linux `mm/memcontrol.c` 的 v1 memcg：每个 cgroup 目录是一个内存控制域，
//! 持有 `memory.limit_in_bytes`（RAM 上限）与成员进程集合（`tasks`）。当域内某进程
//! 缺页使域 RSS 超过上限时，触发 `try_to_free_mem_cgroup_pages` 风格的回收：
//! 优先丢弃干净的 `MADV_FREE` 惰性页，不足再把脏的私有匿名页换出到 swap。
//!
//! 这是 LTP `madvise09` 的依赖：它在 `/sys/fs/cgroup/memory/<sub>` 下建子域、把
//! 自己加入 `tasks`、设 `memory.limit_in_bytes=8MiB`，再 fork 一个"内存压力"子进程
//! 不断分配，迫使内核回收——验证 `MADV_FREE` 页在低内存下被丢弃、脏页被换出
//! （`/proc/self/status` 的 `VmSwap` 增长）。
//!
//! # 设计要点
//! - **快路径零开销**：无任何"有限上限"域存在时（`ACTIVE==0`），每次缺页只多一次
//!   原子读即返回；仅 madvise09 运行期间 `ACTIVE>0`。
//! - **回收时机**：缺页解析完成、`PROCESS_TABLE` 与地址空间锁均已释放后，由 trap
//!   层调用 `account_and_reclaim(pid)`（见 `arch/*/trap.rs`），此时可安全地跨进程
//!   锁定各成员 `mm` 执行回收。
//! - **fork 继承 / exit 清理**：`on_fork` 把子进程加入父域（对标 v1 fork 自动继承），
//!   `on_exit` 从域中移除退出进程。

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sync::SpinLock;

/// Linux v1 "unlimited" 上限哨兵（`PAGE_COUNTER_MAX << PAGE_SHIFT` 截断值）。
/// `memory.limit_in_bytes` 未设置时报告此值。
pub const UNLIMITED: usize = 9223372036854771712;

struct MemcgV1 {
    /// RAM 上限（字节）。`UNLIMITED` 表示不限。
    limit: usize,
    /// 成员进程 pid 列表（写 `tasks` 加入，fork 继承，exit 移除）。
    members: Vec<usize>,
}

impl MemcgV1 {
    fn new() -> Self {
        Self { limit: UNLIMITED, members: Vec::new() }
    }
}

static MEMCG: SpinLock<BTreeMap<String, MemcgV1>> = SpinLock::new(BTreeMap::new());

/// 拥有**有限**上限的域数量。为 0 时缺页快路径直接跳过回收。
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
/// 全部域的成员总数。为 0 时 fork/exit 钩子直接跳过。
static MEMBERS_TOTAL: AtomicUsize = AtomicUsize::new(0);

fn recompute_active(map: &BTreeMap<String, MemcgV1>) {
    let n = map
        .values()
        .filter(|c| c.limit != UNLIMITED && !c.members.is_empty())
        .count();
    ACTIVE.store(n, Ordering::Relaxed);
}

// ---- 目录树操作（供 memcgfs 调用）------------------------------------------

/// 创建子域（`mkdir`）。已存在返回 false。
pub fn create(path: &str) -> bool {
    let mut map = MEMCG.lock();
    if map.contains_key(path) {
        return false;
    }
    map.insert(path.to_string(), MemcgV1::new());
    true
}

/// 删除子域（`rmdir`）。仍有成员时返回 false（-EBUSY）。
pub fn remove(path: &str) -> bool {
    let mut map = MEMCG.lock();
    match map.get(path) {
        Some(c) if c.members.is_empty() => {}
        Some(_) => return false,
        None => return false,
    }
    map.remove(path);
    recompute_active(&map);
    true
}

pub fn exists(path: &str) -> bool {
    MEMCG.lock().contains_key(path)
}

/// 列出 `parent` 的直接子域名（供 readdir）。
pub fn children(parent: &str) -> Vec<String> {
    let map = MEMCG.lock();
    let prefix = if parent == "/" { String::from("/") } else { alloc::format!("{}/", parent) };
    let mut out = Vec::new();
    for key in map.keys() {
        if key == parent {
            continue;
        }
        if let Some(rest) = key.strip_prefix(&prefix) {
            if !rest.is_empty() && !rest.contains('/') {
                out.push(rest.to_string());
            }
        }
    }
    out
}

// ---- 接口文件读写 ----------------------------------------------------------

pub fn get_limit(path: &str) -> usize {
    MEMCG.lock().get(path).map(|c| c.limit).unwrap_or(UNLIMITED)
}

pub fn set_limit(path: &str, bytes: usize) -> bool {
    let mut map = MEMCG.lock();
    let Some(c) = map.get_mut(path) else { return false };
    c.limit = bytes;
    recompute_active(&map);
    true
}

/// 加入成员（写 `tasks` / `cgroup.procs`）。
pub fn add_task(path: &str, pid: usize) -> bool {
    let mut map = MEMCG.lock();
    // 先把该 pid 从其它域移除（一个进程只属于一个 memcg）。
    for c in map.values_mut() {
        if let Some(i) = c.members.iter().position(|&p| p == pid) {
            c.members.remove(i);
            MEMBERS_TOTAL.fetch_sub(1, Ordering::Relaxed);
        }
    }
    let Some(c) = map.get_mut(path) else { return false };
    c.members.push(pid);
    MEMBERS_TOTAL.fetch_add(1, Ordering::Relaxed);
    recompute_active(&map);
    true
}

/// `tasks` 内容（每行一个成员 pid）。
pub fn read_tasks(path: &str) -> String {
    let map = MEMCG.lock();
    let Some(c) = map.get(path) else { return String::new() };
    let mut s = String::new();
    for p in &c.members {
        s.push_str(&p.to_string());
        s.push('\n');
    }
    s
}

/// 当前用量字节（域内成员 RSS 之和 × PAGE_SIZE）。
pub fn usage_bytes(path: &str) -> usize {
    let members = {
        let map = MEMCG.lock();
        match map.get(path) {
            Some(c) => c.members.clone(),
            None => return 0,
        }
    };
    sum_resident_pages(&members) * super::PAGE_SIZE
}

// ---- fork / exit 钩子 ------------------------------------------------------

/// fork：子进程继承父进程所属 memcg（对标 v1 fork 自动加入父 cgroup）。
pub fn on_fork(parent_pid: usize, child_pid: usize) {
    if MEMBERS_TOTAL.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut map = MEMCG.lock();
    let mut target: Option<String> = None;
    for (k, c) in map.iter() {
        if c.members.contains(&parent_pid) {
            target = Some(k.clone());
            break;
        }
    }
    if let Some(k) = target {
        if let Some(c) = map.get_mut(&k) {
            if !c.members.contains(&child_pid) {
                c.members.push(child_pid);
                MEMBERS_TOTAL.fetch_add(1, Ordering::Relaxed);
            }
        }
        recompute_active(&map);
    }
}

/// 进程退出：从所属 memcg 移除。
pub fn on_exit(pid: usize) {
    if MEMBERS_TOTAL.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut map = MEMCG.lock();
    let mut changed = false;
    for c in map.values_mut() {
        if let Some(i) = c.members.iter().position(|&p| p == pid) {
            c.members.remove(i);
            MEMBERS_TOTAL.fetch_sub(1, Ordering::Relaxed);
            changed = true;
        }
    }
    if changed {
        recompute_active(&map);
    }
}

// ---- 计账与回收（缺页后钩子）-----------------------------------------------

/// 求一组成员进程当前 resident 页数之和。锁定各成员 `mm` 读取（不得在持有
/// `PROCESS_TABLE` 或任一 `mm` 锁时调用）。
fn sum_resident_pages(members: &[usize]) -> usize {
    use crate::sched::process::PROCESS_TABLE;
    let mms = {
        let mut table = PROCESS_TABLE.lock();
        let Some(table) = table.as_mut() else { return 0 };
        members
            .iter()
            .filter_map(|&pid| table.get(pid).and_then(|p| p.mm.as_ref()).cloned())
            .collect::<Vec<_>>()
    };
    mms.iter().map(|mm| mm.lock().resident_pages()).sum()
}

/// 缺页后的 memcg 计账 + 回收钩子。`pid` 为刚缺页的进程。
///
/// 若 `pid` 属于某有限上限域、且域 RSS 已超上限，则跨成员回收（先丢弃干净
/// `MADV_FREE` 惰性页，不足再换出脏匿名页），把用量压回上限附近。
///
/// **调用约束**：必须在 `PROCESS_TABLE` 与所有 `mm` 锁均释放后调用（trap 层
/// 缺页解析完成处）。
pub fn account_and_reclaim(pid: usize) {
    if ACTIVE.load(Ordering::Relaxed) == 0 {
        return;
    }
    // 找到 pid 所属的有限上限域及其成员、上限。
    let (members, limit) = {
        let map = MEMCG.lock();
        let mut found: Option<(Vec<usize>, usize)> = None;
        for c in map.values() {
            if c.limit != UNLIMITED && c.members.contains(&pid) {
                found = Some((c.members.clone(), c.limit));
                break;
            }
        }
        match found {
            Some(v) => v,
            None => return,
        }
    };

    let limit_pages = limit / super::PAGE_SIZE;
    let usage_pages = sum_resident_pages(&members);
    if usage_pages <= limit_pages {
        return;
    }
    // 批量回收：每次把用量压到上限**之下**一个余量（margin），而非恰好压回上限。
    // 否则施压子进程每缺一页就触发一次回收，每次回收都要重建+排序候选页表
    // （O(n log n)），在 madvise09 这类紧密缺页循环里成为吞吐瓶颈，30s 内换不够
    // 100MB。对标 Linux：reclaim 以 `SWAP_CLUSTER_MAX` 批量推进、配合高/低水位，
    // 一次回收多页，后续若干次缺页无需再回收。
    const RECLAIM_MARGIN: usize = 64;
    let mut need = (usage_pages - limit_pages) + RECLAIM_MARGIN;
    let need0 = need;

    // 取各成员 mm（克隆 Arc），再逐个回收。
    use crate::sched::process::PROCESS_TABLE;
    let mms = {
        let mut table = PROCESS_TABLE.lock();
        let Some(table) = table.as_mut() else { return };
        members
            .iter()
            .filter_map(|&p| {
                table
                    .get(p)
                    .and_then(|pp| pp.mm.as_ref())
                    .cloned()
                    .map(|mm| (p, mm))
            })
            .collect::<Vec<_>>()
    };
    static N: AtomicUsize = AtomicUsize::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dbg = m9dbg() && n % 500 == 0;
    for (p, mm) in &mms {
        if need == 0 {
            break;
        }
        if dbg {
            let g = mm.lock();
            crate::println!(
                "[M9DBG] member pid={} resident={} lazy={} swapped={}",
                p, g.resident_pages(), g.lazy_free_pages(), g.swapped_pages()
            );
        }
        let got = mm.lock().reclaim(need);
        need = need.saturating_sub(got);
    }
    // 回收的帧在 quarantine：先等所有对端完成 TLB 失效，再归还 buddy——
    // 否则回收释放不出内存，且陈旧 TLB 可命中已复用帧（rustc SIGSEGV 家族）。
    crate::mm::tlb::drain_shootdown();
    for (_p, mm) in &mms {
        mm.lock().reap_retired_frames();
    }
    if dbg {
        let swapped: usize = mms.iter().map(|(_, mm)| mm.lock().swapped_pages()).sum();
        let (so_ok, so_cow, so_noslot, so_other, si, cand) =
            super::address_space::dbg_swap_stats();
        crate::println!(
            "[M9DBG] n={} pid={} members={} usage={} limit={} need0={} reclaimed={} swapped_total={}",
            n, pid, mms.len(), usage_pages, limit_pages, need0, need0 - need, swapped
        );
        crate::println!(
            "[M9DBG] swapstats so_ok={} so_cow={} so_noslot={} so_other={} swap_in={} cand={}",
            so_ok, so_cow, so_noslot, so_other, si, cand
        );
    }
}

fn m9dbg() -> bool {
    crate::cmdline::m9dbg_enabled()
}
