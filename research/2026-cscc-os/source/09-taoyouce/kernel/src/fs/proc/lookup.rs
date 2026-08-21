//! Dynamic /proc/[pid] and /proc/[pid]/task/[tid] directory creation.

use alloc::sync::Arc;
use alloc::boxed::Box;
use alloc::format;

use crate::fs::procfs::ProcInode;
use super::pid::{
    FdInfoGenerator, IdMapGenerator, NsFileGenerator, ProcessAuxvGenerator,
    ProcessCmdlineGenerator, ProcessCommGenerator, ProcessCoredumpFilterGenerator,
    ProcessEnvironGenerator,
    ProcessExeGenerator, ProcessIoGenerator, ProcessLimitsGenerator, ProcessLoginuidGenerator,
    ProcessMapsGenerator, ProcessOomScoreAdjGenerator, ProcessOomScoreGenerator,
    ProcessPagemapGenerator, ProcessPersonalityGenerator, ProcessSessionidGenerator,
    ProcessSmapsGenerator, ProcessStatGenerator, ProcessStatusGenerator,
    ProcessSyscallGenerator, ProcessTimerslackNsGenerator, ProcessWchanGenerator,
    SetgroupsGenerator, TimensOffsetsGenerator, pid_tgid,
};
use super::system::{MountInfoGenerator, MountsGenerator};

/// /proc/[pid] 目录缓存：目录骨架（~50 个 generator 子节点）只依赖 pid 数值，
/// 内容全部在读时动态生成，PID 复用时骨架不变，永不失效；存在性检查
/// 每次仍做。避免每次 open /proc/self/status 都重建整棵目录（数十次堆分配，
/// 高负载下单次可达数十万指令，是 cyclictest STRESS 关中断长 syscall 尖峰根因之一）。
static PID_DIR_CACHE: crate::sync::SpinLock<
    Option<alloc::collections::BTreeMap<usize, Arc<ProcInode>>>,
> = crate::sync::SpinLock::new(None);

/// 动态创建 /proc/[pid] 目录
pub fn create_pid_dir(pid: usize) -> Option<Arc<ProcInode>> {
    {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = table.as_ref()?;
        if t.get(pid).is_none() {
            return None;
        }
    }
    if let Some(dir) = PID_DIR_CACHE
        .lock()
        .as_ref()
        .and_then(|m| m.get(&pid).cloned())
    {
        return Some(dir);
    }
    let p = pid as u32;
    let mut dir = ProcInode::directory(&format!("{}", pid));
    dir.add_child(Arc::new(ProcInode::file("status", Box::new(ProcessStatusGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("stat", Box::new(ProcessStatGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("maps", Box::new(ProcessMapsGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("smaps", Box::new(ProcessSmapsGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("pagemap", Box::new(ProcessPagemapGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("cmdline", Box::new(ProcessCmdlineGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("comm", Box::new(ProcessCommGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("exe", Box::new(ProcessExeGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("io", Box::new(ProcessIoGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("limits", Box::new(ProcessLimitsGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("wchan", Box::new(ProcessWchanGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("loginuid", Box::new(ProcessLoginuidGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("oom_score", Box::new(ProcessOomScoreGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("oom_score_adj", Box::new(ProcessOomScoreAdjGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("personality", Box::new(ProcessPersonalityGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("coredump_filter", Box::new(ProcessCoredumpFilterGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("sessionid", Box::new(ProcessSessionidGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("timerslack_ns", Box::new(ProcessTimerslackNsGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("auxv", Box::new(ProcessAuxvGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("environ", Box::new(ProcessEnvironGenerator { pid: p }))));
    // 暴露 /proc/[pid]/syscall 观测文件即激活逐 syscall 记录（在 open 时点，
    // 保证随后那次 read syscall 的入口已被记录）。lmbench 等不访问
    // /proc/[pid] 树的场景不会触发，保留热路径零锁优化。
    crate::sched::process_sched::mark_syscall_track_active();
    dir.add_child(Arc::new(ProcInode::file("syscall", Box::new(ProcessSyscallGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("mounts", Box::new(MountsGenerator))));
    dir.add_child(Arc::new(ProcInode::file("mountinfo", Box::new(MountInfoGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("mountstats", Box::new(MountsGenerator))));
    dir.add_child(Arc::new(ProcInode::directory("task")));
    dir.add_child(Arc::new(ProcInode::directory("fd")));
    dir.add_child(Arc::new(ProcInode::directory("fdinfo")));
    {
        let mut ns_dir = ProcInode::directory("ns");
        let ns_names = [
            "cgroup", "ipc", "mnt", "net", "pid", "pid_for_children",
            "time", "time_for_children", "user", "uts",
        ];
        for name in ns_names {
            ns_dir.add_child(Arc::new(ProcInode::file(
                name,
                Box::new(NsFileGenerator { name: alloc::string::String::from(name), pid: p }),
            )));
        }
        dir.add_child(Arc::new(ns_dir));
    }
    dir.add_child(Arc::new(ProcInode::file("setgroups", Box::new(SetgroupsGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("uid_map", Box::new(IdMapGenerator { kind: "uid", pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("gid_map", Box::new(IdMapGenerator { kind: "gid", pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("timens_offsets", Box::new(TimensOffsetsGenerator))));
    let dir = Arc::new(dir);
    {
        let mut guard = PID_DIR_CACHE.lock();
        let map = guard.get_or_insert_with(alloc::collections::BTreeMap::new);
        // 容量上限防无界增长（长跑批量 fork 场景）：超限整体清空重建
        if map.len() >= 1024 {
            map.clear();
        }
        map.insert(pid, dir.clone());
    }
    Some(dir)
}

/// 动态创建 /proc/[pid]/task/[tid] per-thread 目录
pub fn create_tid_dir(tid: usize) -> Option<Arc<ProcInode>> {
    let tgid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = table.as_ref()?;
        let p = t.get(tid)?;
        if matches!(
            p.state,
            crate::sched::process::ProcessState::Zombie
                | crate::sched::process::ProcessState::Unused
        ) {
            return None;
        }
        p.tgid as u32
    };
    let p = tid as u32;
    let mut dir = ProcInode::directory(&format!("{}", tid));
    // per-tid views
    dir.add_child(Arc::new(ProcInode::file("status", Box::new(ProcessStatusGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("stat", Box::new(ProcessStatGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("comm", Box::new(ProcessCommGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("wchan", Box::new(ProcessWchanGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("io", Box::new(ProcessIoGenerator { pid: p }))));
    crate::sched::process_sched::mark_syscall_track_active();
    dir.add_child(Arc::new(ProcInode::file("syscall", Box::new(ProcessSyscallGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("oom_score", Box::new(ProcessOomScoreGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("oom_score_adj", Box::new(ProcessOomScoreAdjGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("personality", Box::new(ProcessPersonalityGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("sessionid", Box::new(ProcessSessionidGenerator { pid: p }))));
    dir.add_child(Arc::new(ProcInode::file("timerslack_ns", Box::new(ProcessTimerslackNsGenerator { pid: p }))));
    // tgid-shared views
    dir.add_child(Arc::new(ProcInode::file("loginuid", Box::new(ProcessLoginuidGenerator { pid: tgid }))));
    dir.add_child(Arc::new(ProcInode::file("limits", Box::new(ProcessLimitsGenerator { pid: tgid }))));
    dir.add_child(Arc::new(ProcInode::file("cmdline", Box::new(ProcessCmdlineGenerator { pid: tgid }))));
    dir.add_child(Arc::new(ProcInode::file("maps", Box::new(ProcessMapsGenerator { pid: tgid }))));
    dir.add_child(Arc::new(ProcInode::file("exe", Box::new(ProcessExeGenerator { pid: tgid }))));
    dir.add_child(Arc::new(ProcInode::file("auxv", Box::new(ProcessAuxvGenerator { pid: tgid }))));
    dir.add_child(Arc::new(ProcInode::file("environ", Box::new(ProcessEnvironGenerator { pid: tgid }))));
    dir.add_child(Arc::new(ProcInode::file("mounts", Box::new(MountsGenerator))));
    dir.add_child(Arc::new(ProcInode::file("mountinfo", Box::new(MountInfoGenerator { pid: tgid }))));
    dir.add_child(Arc::new(ProcInode::file("mountstats", Box::new(MountsGenerator))));
    Some(Arc::new(dir))
}
