//! cgroup v2 subsystem — real controllers (cpu, memory, pids, io)
//!
//! Per Linux `kernel/cgroup/cgroup.c`, `kernel/cgroup/cgroup-v2.c`.
//! Implements the cgroup2 unified hierarchy with actual resource tracking.
//!
//! Controllers:
//!   - cpu: cpu.max, cpu.weight, cpu.stat
//!   - memory: memory.max, memory.current, memory.high, memory.low, memory.stat
//!   - pids: pids.max, pids.current
//!   - io: io.max, io.stat

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::sync::TicketSpinLock;

// ---- Cgroup hierarchy state ------------------------------------------------

static CGROUP_TREE: TicketSpinLock<CgroupTree> = TicketSpinLock::new(CgroupTree::new_empty());

/// Fast-path: set when any cgroup has cpu.max quota or freezer configured.
/// When 0, `cgroup_cpu_can_run` and `cgroup_is_frozen` skip the lock entirely.
static CGROUP_SCHED_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

/// cgroup cpu.max quota / freezer 是否曾激活（调度选核/冻结快路径用）。
#[inline]
pub fn cgroup_sched_active() -> bool {
    CGROUP_SCHED_ACTIVE.load(core::sync::atomic::Ordering::Relaxed)
}

/// 是否曾配置过 cpu.max 配额。只有配额需要 10ms 粒度的 usage_usec 更新；
/// 未配置时 cpu.stat 由 switch-time 精确结账，周期 tick 不抢表锁。
static CPU_QUOTA_ACTIVE: core::sync::atomic::AtomicBool =
    core::sync::atomic::AtomicBool::new(false);

#[inline]
pub fn cgroup_cpu_quota_active() -> bool {
    CPU_QUOTA_ACTIVE.load(core::sync::atomic::Ordering::Relaxed)
}

/// 由 cpu.max 写入路径调用（配额 >= 0 时置位）。
pub(super) fn note_cpu_quota_active() {
    CPU_QUOTA_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
}

/// cgroup2 hierarchy root
pub struct CgroupTree {
    groups: BTreeMap<String, CgroupNode>,
    initialized: bool,
}

impl CgroupTree {
    const fn new_empty() -> Self {
        Self {
            groups: BTreeMap::new(),
            initialized: false,
        }
    }

    fn ensure_init(&mut self) {
        if self.initialized { return; }
        self.initialized = true;
        // Create root cgroup
        self.groups.insert(String::from("/"), CgroupNode::new_root());
    }
}

/// A single cgroup node in the hierarchy
pub struct CgroupNode {
    /// Enabled controllers (bitflags)
    pub controllers: u32,
    /// Processes in this cgroup
    pub pids: Vec<usize>,
    /// CPU controller state
    pub cpu: CpuController,
    /// Memory controller state
    pub memory: MemoryController,
    /// PIDs controller state
    pub pids_ctrl: PidsController,
    /// IO controller state
    pub io: IoController,
    /// cpuset controller state
    pub cpuset: CpusetController,
    /// freezer controller state
    pub freezer: FreezerController,
    /// Subtree control mask (which controllers are delegated to children)
    pub subtree_control: u32,
}

/// Controller enable bits
pub const CTRL_CPU:    u32 = 1 << 0;
pub const CTRL_MEMORY: u32 = 1 << 1;
pub const CTRL_PIDS:   u32 = 1 << 2;
pub const CTRL_IO:     u32 = 1 << 3;
pub const CTRL_CPUSET: u32 = 1 << 4;
pub const CTRL_FREEZER: u32 = 1 << 5;

const ALL_CONTROLLERS: u32 = CTRL_CPU | CTRL_MEMORY | CTRL_PIDS | CTRL_IO | CTRL_CPUSET | CTRL_FREEZER;

impl CgroupNode {
    fn new_root() -> Self {
        Self {
            controllers: ALL_CONTROLLERS,
            pids: Vec::new(),
            cpu: CpuController::new(),
            memory: MemoryController::new(),
            pids_ctrl: PidsController::new(),
            io: IoController::new(),
            cpuset: CpusetController::new(),
            freezer: FreezerController::new(),
            subtree_control: ALL_CONTROLLERS,
        }
    }

    fn new_child(parent_subtree: u32) -> Self {
        Self {
            controllers: parent_subtree,
            pids: Vec::new(),
            cpu: CpuController::new(),
            memory: MemoryController::new(),
            pids_ctrl: PidsController::new(),
            io: IoController::new(),
            cpuset: CpusetController::new(),
            freezer: FreezerController::new(),
            subtree_control: 0,
        }
    }
}

// ---- CPU controller --------------------------------------------------------

pub struct CpuController {
    /// cpu.weight (1-10000, default 100)
    pub weight: u32,
    /// cpu.max: quota_us / period_us ("max" = unlimited)
    pub quota_us: i64,  // -1 = max (unlimited)
    pub period_us: u64, // default 100000 (100ms)
    /// Stats
    pub usage_usec: AtomicU64,
    pub user_usec: AtomicU64,
    pub system_usec: AtomicU64,
    pub nr_periods: AtomicU64,
    pub nr_throttled: AtomicU64,
    pub throttled_usec: AtomicU64,
}

impl CpuController {
    fn new() -> Self {
        Self {
            weight: 100,
            quota_us: -1, // unlimited
            period_us: 100_000,
            usage_usec: AtomicU64::new(0),
            user_usec: AtomicU64::new(0),
            system_usec: AtomicU64::new(0),
            nr_periods: AtomicU64::new(0),
            nr_throttled: AtomicU64::new(0),
            throttled_usec: AtomicU64::new(0),
        }
    }

    /// Format cpu.stat output
    pub fn format_stat(&self) -> String {
        alloc::format!(
            "usage_usec {}\nuser_usec {}\nsystem_usec {}\nnr_periods {}\nnr_throttled {}\nthrottled_usec {}\n",
            self.usage_usec.load(Ordering::Relaxed),
            self.user_usec.load(Ordering::Relaxed),
            self.system_usec.load(Ordering::Relaxed),
            self.nr_periods.load(Ordering::Relaxed),
            self.nr_throttled.load(Ordering::Relaxed),
            self.throttled_usec.load(Ordering::Relaxed),
        )
    }

    /// Format cpu.max output: "quota period" or "max period"
    pub fn format_max(&self) -> String {
        if self.quota_us < 0 {
            alloc::format!("max {}\n", self.period_us)
        } else {
            alloc::format!("{} {}\n", self.quota_us, self.period_us)
        }
    }
}

// ---- Memory controller -----------------------------------------------------

pub struct MemoryController {
    /// memory.max: hard limit in bytes (-1 = max/unlimited)
    pub max: i64,
    /// memory.high: high watermark for reclaim pressure
    pub high: i64,
    /// memory.low: best-effort memory protection
    pub low: i64,
    /// memory.min: hard memory protection
    pub min: i64,
    /// Current usage in bytes
    pub current: AtomicU64,
    /// memory.swap.max
    pub swap_max: i64,
    /// memory.swap.current
    pub swap_current: AtomicU64,
    /// Stats
    pub cache: AtomicU64,
    pub rss: AtomicU64,
    pub rss_huge: AtomicU64,
    pub shmem: AtomicU64,
    pub pgfault: AtomicU64,
    pub pgmajfault: AtomicU64,
}

impl MemoryController {
    fn new() -> Self {
        Self {
            max: -1,  // unlimited
            high: -1,
            low: 0,
            min: 0,
            current: AtomicU64::new(0),
            swap_max: -1,
            swap_current: AtomicU64::new(0),
            cache: AtomicU64::new(0),
            rss: AtomicU64::new(0),
            rss_huge: AtomicU64::new(0),
            shmem: AtomicU64::new(0),
            pgfault: AtomicU64::new(0),
            pgmajfault: AtomicU64::new(0),
        }
    }

    pub fn format_stat(&self) -> String {
        alloc::format!(
            "cache {}\nrss {}\nrss_huge {}\nshmem {}\npgfault {}\npgmajfault {}\n\
             inactive_anon 0\nactive_anon 0\ninactive_file 0\nactive_file 0\n\
             unevictable 0\n",
            self.cache.load(Ordering::Relaxed),
            self.rss.load(Ordering::Relaxed),
            self.rss_huge.load(Ordering::Relaxed),
            self.shmem.load(Ordering::Relaxed),
            self.pgfault.load(Ordering::Relaxed),
            self.pgmajfault.load(Ordering::Relaxed),
        )
    }

    pub fn format_max(&self) -> String {
        if self.max < 0 { String::from("max\n") }
        else { alloc::format!("{}\n", self.max) }
    }

    pub fn format_current(&self) -> String {
        alloc::format!("{}\n", self.current.load(Ordering::Relaxed))
    }
}

// ---- PIDs controller -------------------------------------------------------

pub struct PidsController {
    /// pids.max: max number of processes (-1 = max/unlimited)
    pub max: i64,
    /// pids.current: current count
    pub current: AtomicUsize,
}

impl PidsController {
    fn new() -> Self {
        Self {
            max: -1, // unlimited
            current: AtomicUsize::new(0),
        }
    }

    pub fn format_max(&self) -> String {
        if self.max < 0 { String::from("max\n") }
        else { alloc::format!("{}\n", self.max) }
    }

    pub fn format_current(&self) -> String {
        alloc::format!("{}\n", self.current.load(Ordering::Relaxed))
    }

    /// Check if forking is allowed (returns false if at limit)
    pub fn can_fork(&self) -> bool {
        if self.max < 0 { return true; }
        (self.current.load(Ordering::Relaxed) as i64) < self.max
    }

    pub fn charge(&self) {
        self.current.fetch_add(1, Ordering::Relaxed);
    }

    pub fn uncharge(&self) {
        let _ = self.current.fetch_sub(1, Ordering::Relaxed);
    }
}

// ---- IO controller ---------------------------------------------------------

pub struct IoController {
    /// io.max entries: (major, minor) -> IoLimit
    pub limits: BTreeMap<(u32, u32), IoLimit>,
    /// io.stat entries
    pub stats: BTreeMap<(u32, u32), IoStat>,
}

pub struct IoLimit {
    pub rbps: i64,  // read bytes per second (-1 = max)
    pub wbps: i64,  // write bytes per second
    pub riops: i64, // read IO operations per second
    pub wiops: i64, // write IO operations per second
}

pub struct IoStat {
    pub rbytes: AtomicU64,
    pub wbytes: AtomicU64,
    pub rios: AtomicU64,
    pub wios: AtomicU64,
}

impl IoController {
    fn new() -> Self {
        Self {
            limits: BTreeMap::new(),
            stats: BTreeMap::new(),
        }
    }

    pub fn format_max(&self) -> String {
        let mut out = String::new();
        for ((major, minor), limit) in &self.limits {
            out.push_str(&alloc::format!(
                "{}:{} rbps={} wbps={} riops={} wiops={}\n",
                major, minor,
                if limit.rbps < 0 { String::from("max") } else { alloc::format!("{}", limit.rbps) },
                if limit.wbps < 0 { String::from("max") } else { alloc::format!("{}", limit.wbps) },
                if limit.riops < 0 { String::from("max") } else { alloc::format!("{}", limit.riops) },
                if limit.wiops < 0 { String::from("max") } else { alloc::format!("{}", limit.wiops) },
            ));
        }
        out
    }

    pub fn format_stat(&self) -> String {
        let mut out = String::new();
        for ((major, minor), stat) in &self.stats {
            out.push_str(&alloc::format!(
                "{}:{} rbytes={} wbytes={} rios={} wios={}\n",
                major, minor,
                stat.rbytes.load(Ordering::Relaxed),
                stat.wbytes.load(Ordering::Relaxed),
                stat.rios.load(Ordering::Relaxed),
                stat.wios.load(Ordering::Relaxed),
            ));
        }
        out
    }
}

// ---- cpuset controller -----------------------------------------------------
//
// Per Linux kernel/cgroup/cpuset.c: cpuset constrains which CPUs and memory
// nodes a cgroup's processes can use.
// Files: cpuset.cpus, cpuset.mems, cpuset.cpus.effective, cpuset.mems.effective,
//        cpuset.cpus.partition

pub struct CpusetController {
    /// cpuset.cpus — user-configured CPU mask string (e.g. "0-3")
    pub cpus: String,
    /// cpuset.mems — user-configured memory node string (e.g. "0")
    pub mems: String,
    /// cpuset.cpus.partition — "member" (default), "root", "isolated"
    pub partition: String,
}

impl CpusetController {
    fn new() -> Self {
        // Default: all CPUs and all memory nodes available
        Self {
            cpus: String::from("0-7"),
            mems: String::from("0"),
            partition: String::from("member"),
        }
    }

    pub fn format_cpus(&self) -> String {
        alloc::format!("{}\n", self.cpus)
    }

    pub fn format_mems(&self) -> String {
        alloc::format!("{}\n", self.mems)
    }

    pub fn format_cpus_effective(&self) -> String {
        // effective = intersection of parent's effective and this node's cpus
        // For simplicity in single-NUMA microkernel, effective == configured
        alloc::format!("{}\n", self.cpus)
    }

    pub fn format_mems_effective(&self) -> String {
        alloc::format!("{}\n", self.mems)
    }

    pub fn format_partition(&self) -> String {
        alloc::format!("{}\n", self.partition)
    }
}

// ---- freezer controller ----------------------------------------------------
//
// Per Linux kernel/cgroup/freezer.c (cgroup v2):
// cgroup.freeze — write "1" to freeze all processes, "0" to thaw.
// cgroup.events reports "frozen 0" or "frozen 1".

pub struct FreezerController {
    /// Whether this cgroup is frozen (cgroup.freeze)
    pub frozen: bool,
}

impl FreezerController {
    fn new() -> Self {
        Self { frozen: false }
    }

    pub fn format_freeze(&self) -> String {
        alloc::format!("{}\n", if self.frozen { 1 } else { 0 })
    }
}

// ---- Public API for cgroup operations from procfs/sysfs --------------------

/// Read a cgroup control file. Returns the content string.
pub fn read_cgroup_file(cgroup_path: &str, filename: &str) -> Option<String> {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    let node = tree.groups.get(cgroup_path)?;
    Some(match filename {
        "cgroup.controllers" => {
            let mut s = String::new();
            if node.controllers & CTRL_CPU != 0 { s.push_str("cpu "); }
            if node.controllers & CTRL_CPUSET != 0 { s.push_str("cpuset "); }
            if node.controllers & CTRL_FREEZER != 0 { s.push_str("freezer "); }
            if node.controllers & CTRL_MEMORY != 0 { s.push_str("memory "); }
            if node.controllers & CTRL_PIDS != 0 { s.push_str("pids "); }
            if node.controllers & CTRL_IO != 0 { s.push_str("io "); }
            s.trim_end().to_string() + "\n"
        }
        "cgroup.subtree_control" => {
            let mut s = String::new();
            if node.subtree_control & CTRL_CPU != 0 { s.push_str("cpu "); }
            if node.subtree_control & CTRL_CPUSET != 0 { s.push_str("cpuset "); }
            if node.subtree_control & CTRL_FREEZER != 0 { s.push_str("freezer "); }
            if node.subtree_control & CTRL_MEMORY != 0 { s.push_str("memory "); }
            if node.subtree_control & CTRL_PIDS != 0 { s.push_str("pids "); }
            if node.subtree_control & CTRL_IO != 0 { s.push_str("io "); }
            s.trim_end().to_string() + "\n"
        }
        "cgroup.procs" | "cgroup.threads" => {
            let mut s = String::new();
            for pid in &node.pids {
                s.push_str(&alloc::format!("{}\n", pid));
            }
            s
        }
        "cgroup.type" => String::from("domain\n"),
        "cgroup.events" => alloc::format!(
            "populated 1\nfrozen {}\n",
            if node.freezer.frozen { 1 } else { 0 }
        ),
        "cgroup.freeze" => node.freezer.format_freeze(),
        "cgroup.max.descendants" => String::from("max\n"),
        "cgroup.max.depth" => String::from("max\n"),
        "cgroup.stat" => String::from("nr_descendants 0\nnr_dying_descendants 0\n"),
        "cpu.stat" => node.cpu.format_stat(),
        "cpu.max" => node.cpu.format_max(),
        "cpu.weight" => alloc::format!("{}\n", node.cpu.weight),
        "memory.max" => node.memory.format_max(),
        "memory.current" => node.memory.format_current(),
        "memory.high" => if node.memory.high < 0 { String::from("max\n") }
            else { alloc::format!("{}\n", node.memory.high) },
        "memory.low" => alloc::format!("{}\n", node.memory.low),
        "memory.min" => alloc::format!("{}\n", node.memory.min),
        "memory.stat" => node.memory.format_stat(),
        "memory.swap.max" => if node.memory.swap_max < 0 { String::from("max\n") }
            else { alloc::format!("{}\n", node.memory.swap_max) },
        "memory.swap.current" => alloc::format!("{}\n", node.memory.swap_current.load(Ordering::Relaxed)),
        "pids.max" => node.pids_ctrl.format_max(),
        "pids.current" => node.pids_ctrl.format_current(),
        "io.max" => node.io.format_max(),
        "io.stat" => node.io.format_stat(),
        // cpuset controller
        "cpuset.cpus" => node.cpuset.format_cpus(),
        "cpuset.mems" => node.cpuset.format_mems(),
        "cpuset.cpus.effective" => node.cpuset.format_cpus_effective(),
        "cpuset.mems.effective" => node.cpuset.format_mems_effective(),
        "cpuset.cpus.partition" => node.cpuset.format_partition(),
        _ => return None,
    })
}

/// Write to a cgroup control file
pub fn write_cgroup_file(cgroup_path: &str, filename: &str, value: &str) -> Result<(), isize> {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    // cgroup.procs / cgroup.threads move a task into this cgroup. A task lives
    // in exactly one cgroup of the v2 hierarchy (per Linux cgroup_attach_task),
    // so moving it here must first detach it from whatever cgroup it was in —
    // otherwise the old cgroup stays non-empty and rmdir() would wrongly EBUSY.
    if filename == "cgroup.procs" || filename == "cgroup.threads" {
        let pid: usize = value.trim().parse().map_err(|_| -22isize)?;
        if !tree.groups.contains_key(cgroup_path) {
            return Err(-2); // ENOENT
        }
        for (path, n) in tree.groups.iter_mut() {
            if path != cgroup_path {
                let before = n.pids.len();
                n.pids.retain(|&p| p != pid);
                if n.pids.len() != before && n.controllers & CTRL_PIDS != 0 {
                    n.pids_ctrl.uncharge();
                }
            }
        }
        if let Some(n) = tree.groups.get_mut(cgroup_path) {
            if !n.pids.contains(&pid) {
                n.pids.push(pid);
                if n.controllers & CTRL_PIDS != 0 {
                    n.pids_ctrl.charge();
                }
            }
        }
        return Ok(());
    }
    let node = match tree.groups.get_mut(cgroup_path) {
        Some(n) => n,
        None => return Err(-2), // ENOENT
    };
    match filename {
        "cgroup.subtree_control" => {
            // Parse "+cpu -memory +pids" etc
            for token in value.split_whitespace() {
                let (add, name) = if let Some(n) = token.strip_prefix('+') {
                    (true, n)
                } else if let Some(n) = token.strip_prefix('-') {
                    (false, n)
                } else {
                    continue;
                };
                let bit = match name {
                    "cpu" => CTRL_CPU,
                    "memory" => CTRL_MEMORY,
                    "pids" => CTRL_PIDS,
                    "io" => CTRL_IO,
                    "cpuset" => CTRL_CPUSET,
                    "freezer" => CTRL_FREEZER,
                    _ => return Err(-22), // EINVAL
                };
                if add {
                    node.subtree_control |= bit;
                } else {
                    node.subtree_control &= !bit;
                }
            }
            Ok(())
        }
        "cpu.max" => {
            let parts: Vec<&str> = value.trim().split_whitespace().collect();
            if parts.is_empty() { return Err(-22); }
            node.cpu.quota_us = if parts[0] == "max" { -1 }
                else { parts[0].parse().map_err(|_| -22isize)? };
            if parts.len() > 1 {
                node.cpu.period_us = parts[1].parse().map_err(|_| -22isize)?;
            }
            if node.cpu.quota_us >= 0 {
                CGROUP_SCHED_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
                note_cpu_quota_active();
            }
            Ok(())
        }
        "cpu.weight" => {
            let w: u32 = value.trim().parse().map_err(|_| -22isize)?;
            if !(1..=10000).contains(&w) { return Err(-22); }
            node.cpu.weight = w;
            Ok(())
        }
        "memory.max" => {
            node.memory.max = if value.trim() == "max" { -1 }
                else { value.trim().parse().map_err(|_| -22isize)? };
            Ok(())
        }
        "memory.high" => {
            node.memory.high = if value.trim() == "max" { -1 }
                else { value.trim().parse().map_err(|_| -22isize)? };
            Ok(())
        }
        "memory.swap.max" => {
            node.memory.swap_max = if value.trim() == "max" { -1 }
                else { value.trim().parse().map_err(|_| -22isize)? };
            Ok(())
        }
        "memory.low" => {
            node.memory.low = value.trim().parse().map_err(|_| -22isize)?;
            Ok(())
        }
        "memory.min" => {
            node.memory.min = value.trim().parse().map_err(|_| -22isize)?;
            Ok(())
        }
        "pids.max" => {
            node.pids_ctrl.max = if value.trim() == "max" { -1 }
                else { value.trim().parse().map_err(|_| -22isize)? };
            Ok(())
        }
        // cpuset controller
        "cpuset.cpus" => {
            let v = value.trim();
            if v.is_empty() { return Err(-22); }
            node.cpuset.cpus = String::from(v);
            Ok(())
        }
        "cpuset.mems" => {
            let v = value.trim();
            if v.is_empty() { return Err(-22); }
            node.cpuset.mems = String::from(v);
            Ok(())
        }
        "cpuset.cpus.partition" => {
            let v = value.trim();
            match v {
                "member" | "root" | "isolated" => {
                    node.cpuset.partition = String::from(v);
                    Ok(())
                }
                _ => Err(-22),
            }
        }
        // freezer controller (cgroup v2: cgroup.freeze)
        "cgroup.freeze" => {
            match value.trim() {
                "0" => { node.freezer.frozen = false; Ok(()) }
                "1" => {
                    node.freezer.frozen = true;
                    CGROUP_SCHED_ACTIVE.store(true, core::sync::atomic::Ordering::Relaxed);
                    Ok(())
                }
                _ => Err(-22),
            }
        }
        _ => Err(-22), // EINVAL
    }
}

/// Create a new child cgroup directory
pub fn mkdir_cgroup(parent_path: &str, name: &str) -> Result<(), isize> {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    let parent_subtree = match tree.groups.get(parent_path) {
        Some(p) => p.subtree_control,
        None => return Err(-2), // ENOENT
    };
    let child_path = if parent_path == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", parent_path, name)
    };
    if tree.groups.contains_key(&child_path) {
        return Err(-17); // EEXIST
    }
    tree.groups.insert(child_path, CgroupNode::new_child(parent_subtree));
    Ok(())
}

/// Remove a cgroup directory (must be empty)
pub fn rmdir_cgroup(path: &str) -> Result<(), isize> {
    let mut tree = CGROUP_TREE.lock();
    let node = match tree.groups.get(path) {
        Some(n) => n,
        None => return Err(-2),
    };
    if !node.pids.is_empty() {
        return Err(-16); // EBUSY
    }
    tree.groups.remove(path);
    Ok(())
}

/// List children of a cgroup
pub fn list_cgroup_children(path: &str) -> Vec<String> {
    let tree = CGROUP_TREE.lock();
    let prefix = if path == "/" {
        String::from("/")
    } else {
        alloc::format!("{}/", path)
    };
    let mut children = Vec::new();
    for key in tree.groups.keys() {
        if key == path { continue; }
        if key.starts_with(&prefix) {
            // Only direct children (no further '/' after prefix)
            let rest = &key[prefix.len()..];
            if !rest.contains('/') {
                children.push(rest.to_string());
            }
        }
    }
    children
}

/// Account memory usage for a cgroup (called from mm subsystem)
pub fn charge_memory(cgroup_path: &str, bytes: u64) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    if let Some(node) = tree.groups.get(cgroup_path) {
        node.memory.current.fetch_add(bytes, Ordering::Relaxed);
        node.memory.rss.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// Account CPU usage for a cgroup (called from scheduler)
pub fn charge_cpu(cgroup_path: &str, usec: u64) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    if let Some(node) = tree.groups.get(cgroup_path) {
        node.cpu.usage_usec.fetch_add(usec, Ordering::Relaxed);
    }
}

// ---- Enforcement hooks (called from kernel subsystems) ----------------------

/// Check if a fork is allowed by the pids controller for the given cgroup path.
/// Returns true if allowed, false if pids.max would be exceeded.
/// Per Linux `cgroup_can_fork()` → `pids_can_fork()`.
pub fn cgroup_pids_can_fork(cgroup_path: &str) -> bool {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    match tree.groups.get(cgroup_path) {
        Some(node) => {
            if node.controllers & CTRL_PIDS == 0 { return true; }
            node.pids_ctrl.can_fork()
        }
        None => true, // unknown cgroup → allow (root default)
    }
}

/// Charge a new process to the pids controller.
/// Called after fork succeeds.
pub fn cgroup_pids_charge(cgroup_path: &str) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    if let Some(node) = tree.groups.get(cgroup_path) {
        if node.controllers & CTRL_PIDS != 0 {
            node.pids_ctrl.charge();
        }
    }
}

/// Uncharge a process from the pids controller.
/// Called on process exit.
pub fn cgroup_pids_uncharge(cgroup_path: &str) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    if let Some(node) = tree.groups.get(cgroup_path) {
        if node.controllers & CTRL_PIDS != 0 {
            node.pids_ctrl.uncharge();
        }
    }
}

/// Remove a task from every cgroup's membership set (its `cgroup.procs`
/// listing). Called when a task exits — per Linux `cgroup_exit()`, an exited
/// task is unlinked from its cgroup so it no longer appears in `cgroup.procs`
/// and no longer blocks `rmdir()` of that cgroup. Without this, a process that
/// was explicitly attached via a `cgroup.procs` write (e.g. clone3
/// CLONE_INTO_CGROUP) would linger in the membership list after death and make
/// the cgroup permanently un-removable (EBUSY → cleanup ENOTEMPTY loop).
pub fn cgroup_remove_pid(pid: usize) {
    let mut tree = CGROUP_TREE.lock();
    for node in tree.groups.values_mut() {
        node.pids.retain(|&p| p != pid);
    }
}

/// Return the cgroup path a task currently belongs to (its `cgroup.procs`
/// membership), if any. Used for migration permission checks, which authorise
/// against the common ancestor of the source and destination cgroups.
pub fn cgroup_of_pid(pid: usize) -> Option<String> {
    let tree = CGROUP_TREE.lock();
    for (path, node) in tree.groups.iter() {
        if node.pids.contains(&pid) {
            return Some(path.clone());
        }
    }
    None
}

/// Check if a memory allocation is allowed by the memory controller.
/// Returns true if allowed, false if memory.max would be exceeded.
/// Per Linux `mem_cgroup_charge()` → `try_charge()`.
pub fn cgroup_memory_can_charge(cgroup_path: &str, bytes: u64) -> bool {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    match tree.groups.get(cgroup_path) {
        Some(node) => {
            if node.controllers & CTRL_MEMORY == 0 { return true; }
            if node.memory.max < 0 { return true; } // unlimited
            let current = node.memory.current.load(Ordering::Relaxed);
            (current + bytes) <= node.memory.max as u64
        }
        None => true,
    }
}

/// Charge memory to the memory controller.
pub fn cgroup_memory_charge(cgroup_path: &str, bytes: u64) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    if let Some(node) = tree.groups.get(cgroup_path) {
        if node.controllers & CTRL_MEMORY != 0 {
            node.memory.current.fetch_add(bytes, Ordering::Relaxed);
            node.memory.rss.fetch_add(bytes, Ordering::Relaxed);
            node.memory.pgfault.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Uncharge memory from the memory controller.
pub fn cgroup_memory_uncharge(cgroup_path: &str, bytes: u64) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    if let Some(node) = tree.groups.get(cgroup_path) {
        if node.controllers & CTRL_MEMORY != 0 {
            let current = node.memory.current.load(Ordering::Relaxed);
            let new_val = current.saturating_sub(bytes);
            node.memory.current.store(new_val, Ordering::Relaxed);
            let rss = node.memory.rss.load(Ordering::Relaxed);
            node.memory.rss.store(rss.saturating_sub(bytes), Ordering::Relaxed);
        }
    }
}

/// Check if the cpu.max quota allows scheduling for the given cgroup.
/// Returns true if the process can run, false if throttled.
/// Per Linux `cpu_cfs_throttled()` — checks if group consumed its quota in current period.
pub fn cgroup_cpu_can_run(cgroup_path: &str) -> bool {
    if !CGROUP_SCHED_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
        return true;
    }
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    match tree.groups.get(cgroup_path) {
        Some(node) => {
            if node.controllers & CTRL_CPU == 0 { return true; }
            if node.cpu.quota_us < 0 { return true; } // unlimited
            let usage = node.cpu.usage_usec.load(Ordering::Relaxed);
            let period_count = node.cpu.nr_periods.load(Ordering::Relaxed);
            // Per-period budget: quota_us per period_us
            // Current period usage = total usage - (completed periods * quota)
            let quota = node.cpu.quota_us as u64;
            let period_budget_used = usage.saturating_sub(period_count * quota);
            period_budget_used < quota
        }
        None => true,
    }
}

/// Advance the CPU period counter and reset throttle state.
/// Called from the scheduler timer tick when a period elapses.
pub fn cgroup_cpu_period_tick(cgroup_path: &str, elapsed_us: u64) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    if let Some(node) = tree.groups.get(cgroup_path) {
        if node.controllers & CTRL_CPU == 0 { return; }
        if node.cpu.quota_us < 0 { return; }
        // Check if a full period has elapsed based on total usage accounting
        let usage = node.cpu.usage_usec.load(Ordering::Relaxed);
        let periods = node.cpu.nr_periods.load(Ordering::Relaxed);
        let quota = node.cpu.quota_us as u64;
        let period_us = node.cpu.period_us;
        // Advance period counter if enough wall-clock time has passed
        let expected_periods = elapsed_us / period_us;
        if expected_periods > periods {
            node.cpu.nr_periods.store(expected_periods, Ordering::Relaxed);
            // Check if we were throttled in the previous period
            let prev_period_usage = usage.saturating_sub(periods * quota);
            if prev_period_usage >= quota {
                node.cpu.nr_throttled.fetch_add(1, Ordering::Relaxed);
                node.cpu.throttled_usec.fetch_add(
                    prev_period_usage.saturating_sub(quota),
                    Ordering::Relaxed,
                );
            }
        }
    }
}

/// Check if a cgroup is frozen (cgroup.freeze == 1).
/// Per Linux kernel/cgroup/freezer.c: frozen cgroups' processes are not scheduled.
pub fn cgroup_is_frozen(cgroup_path: &str) -> bool {
    if !CGROUP_SCHED_ACTIVE.load(core::sync::atomic::Ordering::Relaxed) {
        return false;
    }
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    match tree.groups.get(cgroup_path) {
        Some(node) => {
            if node.controllers & CTRL_FREEZER == 0 { return false; }
            node.freezer.frozen
        }
        None => false,
    }
}

// ---- io.max throttling — per Linux block/blk-throttle.c -----------------------

/// Charge IO bytes to a cgroup's io controller.
/// `major`/`minor` identify the block device; `bytes` = transfer size;
/// `is_write` = true for writes, false for reads.
/// Per Linux blk_throtl_bio() → io_cost charge path.
pub fn charge_io(cgroup_path: &str, major: u32, minor: u32, bytes: u64, is_write: bool) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    if let Some(node) = tree.groups.get_mut(cgroup_path) {
        if node.controllers & CTRL_IO == 0 { return; }
        let key = (major, minor);
        let stat = node.io.stats.entry(key)
            .or_insert_with(|| IoStat {
                rbytes: AtomicU64::new(0),
                wbytes: AtomicU64::new(0),
                rios: AtomicU64::new(0),
                wios: AtomicU64::new(0),
            });
        if is_write {
            stat.wbytes.fetch_add(bytes, Ordering::Relaxed);
            stat.wios.fetch_add(1, Ordering::Relaxed);
        } else {
            stat.rbytes.fetch_add(bytes, Ordering::Relaxed);
            stat.rios.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Check if an IO operation is allowed by io.max limits.
/// Returns true if under limit, false if throttled.
/// Per Linux blk_throtl_bio() threshold check.
pub fn cgroup_io_can_proceed(cgroup_path: &str, major: u32, minor: u32, is_write: bool) -> bool {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    match tree.groups.get(cgroup_path) {
        Some(node) => {
            if node.controllers & CTRL_IO == 0 { return true; }
            let key = (major, minor);
            let limit = match node.io.limits.get(&key) {
                Some(l) => l,
                None => return true,
            };
            let stat = match node.io.stats.get(&key) {
                Some(s) => s,
                None => return true,
            };
            if is_write {
                // Check wbps (bytes/sec) and wiops (ops/sec)
                if limit.wbps >= 0 {
                    let used = stat.wbytes.load(Ordering::Relaxed);
                    if used >= limit.wbps as u64 { return false; }
                }
                if limit.wiops >= 0 {
                    let used = stat.wios.load(Ordering::Relaxed);
                    if used >= limit.wiops as u64 { return false; }
                }
            } else {
                if limit.rbps >= 0 {
                    let used = stat.rbytes.load(Ordering::Relaxed);
                    if used >= limit.rbps as u64 { return false; }
                }
                if limit.riops >= 0 {
                    let used = stat.rios.load(Ordering::Relaxed);
                    if used >= limit.riops as u64 { return false; }
                }
            }
            true
        }
        None => true,
    }
}

/// Reset IO stats per period (called from timer tick, similar to cpu period reset).
/// Per Linux blk_throtl_dispatch_work_fn() — resets counters per slice.
pub fn cgroup_io_period_reset(cgroup_path: &str) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    if let Some(node) = tree.groups.get_mut(cgroup_path) {
        if node.controllers & CTRL_IO == 0 { return; }
        for (_key, stat) in node.io.stats.iter() {
            stat.rbytes.store(0, Ordering::Relaxed);
            stat.wbytes.store(0, Ordering::Relaxed);
            stat.rios.store(0, Ordering::Relaxed);
            stat.wios.store(0, Ordering::Relaxed);
        }
    }
}

/// Whether a cgroup node exists at `path` (used by cgroupfs lookup/readdir).
pub fn cgroup_node_exists(path: &str) -> bool {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    tree.groups.contains_key(path)
}

/// The enabled-controller bitmask of the node at `path` (None if absent).
/// cgroupfs uses this to decide which controller interface files (memory.*,
/// cpu.*, …) a directory exposes.
pub fn cgroup_node_controllers(path: &str) -> Option<u32> {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    tree.groups.get(path).map(|n| n.controllers)
}

/// Move a process to a different cgroup. Adjusts pids.current for both source and dest.
pub fn cgroup_move_process(pid: usize, from_path: &str, to_path: &str) {
    let mut tree = CGROUP_TREE.lock();
    tree.ensure_init();
    // Remove from source
    if let Some(node) = tree.groups.get_mut(from_path) {
        node.pids.retain(|&p| p != pid);
        if node.controllers & CTRL_PIDS != 0 {
            node.pids_ctrl.uncharge();
        }
    }
    // Add to destination
    if let Some(node) = tree.groups.get_mut(to_path) {
        if !node.pids.contains(&pid) {
            node.pids.push(pid);
        }
        if node.controllers & CTRL_PIDS != 0 {
            node.pids_ctrl.charge();
        }
    }
}
