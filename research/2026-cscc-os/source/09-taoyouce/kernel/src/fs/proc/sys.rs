use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;

use crate::fs::proc::ipc::{
    KernelSemGenerator, KeysGcDelayGenerator, KeysMaxbytesGenerator, KeysMaxkeysGenerator,
    KeysPersistentExpiryGenerator, KeysRootMaxbytesGenerator, KeysRootMaxkeysGenerator,
    MsgmaxGenerator, MsgmnbGenerator, MsgmniGenerator, MsgNextIdGenerator, SemNextIdGenerator,
    ShmallGenerator, ShmmaxGenerator, ShmmniGenerator, ShmNextIdGenerator,
};
use crate::fs::procfs::ProcInode;
use crate::fs::procfs::{ProcError, ProcFileGenerator};
use crate::sync::SpinLock;

pub fn build_proc_sys_dir() -> ProcInode {
    let mut sys_dir = ProcInode::directory("sys");
    let mut sys_kernel_dir = ProcInode::directory("kernel");
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("pid_max", Box::new(PidMaxGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("tainted", Box::new(TaintedGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("hostname", Box::new(HostnameGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("domainname", Box::new(DomainnameGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("osrelease", Box::new(OsReleaseGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file(
        "threads-max", Box::new(ThreadsMaxGenerator),
    )));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file(
        "cap_last_cap", Box::new(CapLastCapGenerator),
    )));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file(
        "ostype", Box::new(OstypeGenerator),
    )));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file(
        "version", Box::new(KernelSysVersionGenerator),
    )));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file(
        "core_pattern", Box::new(CorePatternGenerator),
    )));

    let mut sys_kernel_random = ProcInode::directory("random");
    sys_kernel_random.add_child(Arc::new(ProcInode::file(
        "boot_id", Box::new(BootIdGenerator),
    )));
    sys_kernel_random.add_child(Arc::new(ProcInode::file(
        "uuid", Box::new(RandomUuidGenerator),
    )));
    sys_kernel_random.add_child(Arc::new(ProcInode::file(
        "entropy_avail", Box::new(EntropyAvailGenerator),
    )));
    sys_kernel_random.add_child(Arc::new(ProcInode::file(
        "poolsize", Box::new(PoolSizeGenerator),
    )));
    sys_kernel_dir.add_child(Arc::new(sys_kernel_random));

    sys_kernel_dir.add_child(Arc::new(ProcInode::file("shmmax", Box::new(ShmmaxGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("shmmni", Box::new(ShmmniGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("shmall", Box::new(ShmallGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("sem", Box::new(KernelSemGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("msgmax", Box::new(MsgmaxGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("msgmnb", Box::new(MsgmnbGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("msgmni", Box::new(MsgmniGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("shm_next_id", Box::new(ShmNextIdGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("msg_next_id", Box::new(MsgNextIdGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("sem_next_id", Box::new(SemNextIdGenerator))));

    let mut keys_dir = ProcInode::directory("keys");
    keys_dir.add_child(Arc::new(ProcInode::file("maxbytes", Box::new(KeysMaxbytesGenerator))));
    keys_dir.add_child(Arc::new(ProcInode::file("maxkeys", Box::new(KeysMaxkeysGenerator))));
    keys_dir.add_child(Arc::new(ProcInode::file("root_maxbytes", Box::new(KeysRootMaxbytesGenerator))));
    keys_dir.add_child(Arc::new(ProcInode::file("root_maxkeys", Box::new(KeysRootMaxkeysGenerator))));
    keys_dir.add_child(Arc::new(ProcInode::file("gc_delay", Box::new(KeysGcDelayGenerator))));
    keys_dir.add_child(Arc::new(ProcInode::file("persistent_keyring_expiry", Box::new(KeysPersistentExpiryGenerator))));
    sys_kernel_dir.add_child(Arc::new(keys_dir));

    sys_kernel_dir.add_child(Arc::new(ProcInode::file("ns_last_pid", Box::new(NsLastPidGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("printk", Box::new(PrintkGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("io_uring_disabled", Box::new(IoUringDisabledGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("sched_rt_period_us", Box::new(SchedRtPeriodGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("sched_rt_runtime_us", Box::new(SchedRtRuntimeGenerator))));
    sys_kernel_dir.add_child(Arc::new(ProcInode::file("sched_rr_timeslice_ms", Box::new(SchedRrTimesliceGenerator))));
    sys_dir.add_child(Arc::new(sys_kernel_dir));

    let mut sys_fs_dir = ProcInode::directory("fs");
    sys_fs_dir.add_child(Arc::new(ProcInode::file("suid_dumpable", Box::new(ZeroIntGenerator))));
    sys_fs_dir.add_child(Arc::new(ProcInode::file(
        "lease-break-time",
        Box::new(LeaseBreakTimeGenerator),
    )));
    sys_fs_dir.add_child(Arc::new(ProcInode::file(
        "file-max", Box::new(FileMaxGenerator),
    )));
    sys_fs_dir.add_child(Arc::new(ProcInode::file(
        "nr_open", Box::new(NrOpenGenerator),
    )));
    sys_fs_dir.add_child(Arc::new(ProcInode::file(
        "file-nr", Box::new(FileNrGenerator),
    )));
    sys_fs_dir.add_child(Arc::new(ProcInode::file(
        "pipe-max-size", Box::new(PipeMaxSizeGenerator),
    )));
    sys_fs_dir.add_child(Arc::new(ProcInode::file(
        "pipe-user-pages-soft", Box::new(PipeUserPagesSoftGenerator),
    )));
    sys_fs_dir.add_child(Arc::new(ProcInode::file(
        "pipe-user-pages-hard", Box::new(PipeUserPagesHardGenerator),
    )));

    let mut inotify_dir = ProcInode::directory("inotify");
    inotify_dir.add_child(Arc::new(ProcInode::file(
        "max_queued_events", Box::new(InotifyMaxQueuedEventsGenerator),
    )));
    inotify_dir.add_child(Arc::new(ProcInode::file(
        "max_user_instances", Box::new(InotifyMaxUserInstancesGenerator),
    )));
    inotify_dir.add_child(Arc::new(ProcInode::file(
        "max_user_watches", Box::new(InotifyMaxUserWatchesGenerator),
    )));
    sys_fs_dir.add_child(Arc::new(inotify_dir));

    let mut mqueue_dir = ProcInode::directory("mqueue");
    mqueue_dir.add_child(Arc::new(ProcInode::file(
        "queues_max", Box::new(MqueueQueuesMaxGenerator),
    )));
    mqueue_dir.add_child(Arc::new(ProcInode::file(
        "msg_max", Box::new(MqueueMsgMaxGenerator),
    )));
    mqueue_dir.add_child(Arc::new(ProcInode::file(
        "msgsize_max", Box::new(MqueueMsgsizeMaxGenerator),
    )));
    mqueue_dir.add_child(Arc::new(ProcInode::file(
        "msg_default", Box::new(MqueueMsgDefaultGenerator),
    )));
    mqueue_dir.add_child(Arc::new(ProcInode::file(
        "msgsize_default", Box::new(MqueueMsgsizeDefaultGenerator),
    )));
    sys_fs_dir.add_child(Arc::new(mqueue_dir));

    sys_fs_dir.add_child(Arc::new(ProcInode::file(
        "aio-max-nr", Box::new(AioMaxNrGenerator),
    )));
    sys_fs_dir.add_child(Arc::new(ProcInode::file(
        "aio-nr", Box::new(AioNrGenerator),
    )));
    sys_dir.add_child(Arc::new(sys_fs_dir));

    let mut sys_net_dir = ProcInode::directory("net");
    let mut sys_net_core = ProcInode::directory("core");
    sys_net_core.add_child(Arc::new(ProcInode::file(
        "somaxconn", Box::new(crate::net::procnet::SomaxconnGenerator),
    )));
    sys_net_dir.add_child(Arc::new(sys_net_core));
    // net/ipv4/conf/{lo,default,all}/tag — per-netns IPv4 device tag sysctl.
    // clone09 verifies that a tag written in the parent netns is NOT visible
    // in a freshly CLONE_NEWNET'd child (which must read back the default).
    let mut sys_net_ipv4 = ProcInode::directory("ipv4");
    let mut ipv4_conf = ProcInode::directory("conf");
    let mut conf_lo = ProcInode::directory("lo");
    conf_lo.add_child(Arc::new(ProcInode::file("tag", Box::new(NetConfTagGenerator))));
    ipv4_conf.add_child(Arc::new(conf_lo));
    let mut conf_default = ProcInode::directory("default");
    conf_default.add_child(Arc::new(ProcInode::file("tag", Box::new(NetConfDefaultTagGenerator))));
    ipv4_conf.add_child(Arc::new(conf_default));
    let mut conf_all = ProcInode::directory("all");
    conf_all.add_child(Arc::new(ProcInode::file("tag", Box::new(NetConfTagGenerator))));
    ipv4_conf.add_child(Arc::new(conf_all));
    sys_net_ipv4.add_child(Arc::new(ipv4_conf));
    sys_net_dir.add_child(Arc::new(sys_net_ipv4));
    sys_dir.add_child(Arc::new(sys_net_dir));

    let mut sys_vm_dir = ProcInode::directory("vm");
    sys_vm_dir.add_child(Arc::new(ProcInode::file(
        "nr_hugepages",
        Box::new(NrHugePagesGenerator),
    )));
    sys_vm_dir.add_child(Arc::new(ProcInode::file(
        "overcommit_memory",
        Box::new(OvercommitMemoryGenerator),
    )));
    sys_vm_dir.add_child(Arc::new(ProcInode::file(
        "hugetlb_shm_group",
        Box::new(HugetlbShmGroupGenerator),
    )));
    sys_vm_dir.add_child(Arc::new(ProcInode::file(
        "drop_caches",
        Box::new(DropCachesGenerator),
    )));
    sys_vm_dir.add_child(Arc::new(ProcInode::file(
        "max_map_count",
        Box::new(MaxMapCountGenerator),
    )));
    sys_vm_dir.add_child(Arc::new(ProcInode::file(
        "nr_overcommit_hugepages",
        Box::new(NrOvercommitHugePagesGenerator),
    )));
    sys_vm_dir.add_child(Arc::new(ProcInode::file(
        "compact_memory",
        Box::new(CompactMemoryGenerator),
    )));
    sys_vm_dir.add_child(Arc::new(ProcInode::file(
        "vfs_cache_pressure",
        Box::new(VfsCachePressureGenerator),
    )));
    sys_vm_dir.add_child(Arc::new(ProcInode::file(
        "swappiness",
        Box::new(SwappinessGenerator),
    )));
    sys_dir.add_child(Arc::new(sys_vm_dir));
    sys_dir
}

/// net/ipv4/conf/<dev>/tag — per-netns device tag. Keyed by net-namespace id;
/// a netns absent from the map reads back the global default (matching Linux,
/// where a new netns initialises its per-device conf from the default
/// template). Used by clone09.
static NET_CONF_TAG: SpinLock<alloc::collections::BTreeMap<usize, i32>> =
    SpinLock::new(alloc::collections::BTreeMap::new());
static NET_CONF_DEFAULT_TAG: core::sync::atomic::AtomicI32 =
    core::sync::atomic::AtomicI32::new(0);

/// net/ipv4/conf/{lo,all}/tag — scoped to the calling task's network namespace.
pub struct NetConfTagGenerator;

impl ProcFileGenerator for NetConfTagGenerator {
    fn generate(&self) -> String {
        let ns = crate::kernel::namespace::current_net_ns_id();
        let v = NET_CONF_TAG
            .lock()
            .get(&ns)
            .copied()
            .unwrap_or_else(|| NET_CONF_DEFAULT_TAG.load(core::sync::atomic::Ordering::Relaxed));
        format!("{}\n", v)
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let value = data.trim().parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        let ns = crate::kernel::namespace::current_net_ns_id();
        NET_CONF_TAG.lock().insert(ns, value);
        Ok(())
    }
}

/// net/ipv4/conf/default/tag — the global default new netns devices inherit.
pub struct NetConfDefaultTagGenerator;

impl ProcFileGenerator for NetConfDefaultTagGenerator {
    fn generate(&self) -> String {
        format!("{}\n", NET_CONF_DEFAULT_TAG.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let value = data.trim().parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        NET_CONF_DEFAULT_TAG.store(value, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/kernel/pid_max — Linux 默认 4194304；LTP capget02/fork13 等需要此值
pub struct PidMaxGenerator;

static PID_MAX: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(4194304);

pub fn pid_max() -> usize {
    PID_MAX.load(core::sync::atomic::Ordering::Relaxed)
}

impl ProcFileGenerator for PidMaxGenerator {
    fn generate(&self) -> String {
        format!("{}\n", PID_MAX.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let value = data.trim().parse::<usize>().map_err(|_| ProcError::InvalidArgument)?;
        if value < 301 || value > 4194304 {
            return Err(ProcError::InvalidArgument);
        }
        PID_MAX.store(value, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/kernel/ns_last_pid — writable; LTP pidfd_send_signal03 uses it.
/// Linux: the last PID allocated in the current PID namespace.
pub struct NsLastPidGenerator;

impl ProcFileGenerator for NsLastPidGenerator {
    fn generate(&self) -> String {
        format!("{}\n", crate::sched::process::current_last_pid())
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let val = data.trim().parse::<usize>().map_err(|_| ProcError::InvalidArgument)?;
        crate::sched::process::set_last_pid(val);
        Ok(())
    }
}

/// /proc/sys/kernel/printk — console loglevel; format: "current default minimum boot-time"
pub struct PrintkGenerator;
impl ProcFileGenerator for PrintkGenerator {
    fn generate(&self) -> String { String::from("4\t4\t1\t7\n") }
}

/// /proc/sys/kernel/io_uring_disabled — 0 = enabled, 1 = restricted, 2 = disabled.
/// Writable like the real sysctl so the LTP save_restore harness can set it
/// (io_uring01/02); we keep io_uring always available regardless of the value.
static IO_URING_DISABLED: core::sync::atomic::AtomicI32 =
    core::sync::atomic::AtomicI32::new(0);
pub struct IoUringDisabledGenerator;
impl ProcFileGenerator for IoUringDisabledGenerator {
    fn generate(&self) -> String {
        alloc::format!("{}\n", IO_URING_DISABLED.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v = data.trim().parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        if !(0..=2).contains(&v) {
            return Err(ProcError::InvalidArgument);
        }
        IO_URING_DISABLED.store(v, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

// ---- RT 调度 sysctl（LTP proc_sched_rt01）----
// Linux 默认：sched_rt_period_us=1000000，sched_rt_runtime_us=950000，
// sched_rr_timeslice_ms=100（HZ=100 时 RR_TIMESLICE=100ms）。
const SCHED_RT_PERIOD_DEFAULT: i64 = 1_000_000;
const SCHED_RT_RUNTIME_DEFAULT: i64 = 950_000;
const SCHED_RR_TIMESLICE_DEFAULT: i64 = 100;
static SCHED_RT_PERIOD_US: core::sync::atomic::AtomicI64 =
    core::sync::atomic::AtomicI64::new(SCHED_RT_PERIOD_DEFAULT);
static SCHED_RT_RUNTIME_US: core::sync::atomic::AtomicI64 =
    core::sync::atomic::AtomicI64::new(SCHED_RT_RUNTIME_DEFAULT);
static SCHED_RR_TIMESLICE_MS: core::sync::atomic::AtomicI64 =
    core::sync::atomic::AtomicI64::new(SCHED_RR_TIMESLICE_DEFAULT);

/// /proc/sys/kernel/sched_rt_period_us — 合法范围 [1, INT_MAX]。
pub struct SchedRtPeriodGenerator;
impl ProcFileGenerator for SchedRtPeriodGenerator {
    fn generate(&self) -> String {
        format!("{}\n", SCHED_RT_PERIOD_US.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v = data.trim().parse::<i64>().map_err(|_| ProcError::InvalidArgument)?;
        // Linux sched_rt_period_us 范围 1..=INT_MAX，越界返回 EINVAL。
        if v < 1 || v > i32::MAX as i64 {
            return Err(ProcError::InvalidArgument);
        }
        // runtime 必须 <= period，否则该写入会破坏不变式。
        if SCHED_RT_RUNTIME_US.load(core::sync::atomic::Ordering::Relaxed) > v {
            return Err(ProcError::InvalidArgument);
        }
        SCHED_RT_PERIOD_US.store(v, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/kernel/sched_rt_runtime_us — 合法范围 [-1, INT_MAX] 且 <= period。
pub struct SchedRtRuntimeGenerator;
impl ProcFileGenerator for SchedRtRuntimeGenerator {
    fn generate(&self) -> String {
        format!("{}\n", SCHED_RT_RUNTIME_US.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v = data.trim().parse::<i64>().map_err(|_| ProcError::InvalidArgument)?;
        // Linux sched_rt_runtime_us 范围 -1..=INT_MAX。
        if v < -1 || v > i32::MAX as i64 {
            return Err(ProcError::InvalidArgument);
        }
        // runtime 必须 <= period（-1 表示无限制，豁免该约束）。
        if v != -1 && v > SCHED_RT_PERIOD_US.load(core::sync::atomic::Ordering::Relaxed) {
            return Err(ProcError::InvalidArgument);
        }
        SCHED_RT_RUNTIME_US.store(v, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/kernel/sched_rr_timeslice_ms — 写 -1 重置为默认；读回默认 (>0)。
pub struct SchedRrTimesliceGenerator;
impl ProcFileGenerator for SchedRrTimesliceGenerator {
    fn generate(&self) -> String {
        format!("{}\n", SCHED_RR_TIMESLICE_MS.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v = data.trim().parse::<i64>().map_err(|_| ProcError::InvalidArgument)?;
        // 写 0 或 -1 → 重置为默认时间片（与 Linux sched_rr_handler 一致）。
        let new = if v <= 0 { SCHED_RR_TIMESLICE_DEFAULT } else { v };
        SCHED_RR_TIMESLICE_MS.store(new, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/kernel/tainted — Linux 内核污染位；未污染时输出 0
pub struct TaintedGenerator;

impl ProcFileGenerator for TaintedGenerator {
    fn generate(&self) -> String {
        String::from("0\n")
    }
}

/// /proc/sys/kernel/hostname — reads dynamic hostname from UTS_STATE
pub struct HostnameGenerator;

impl ProcFileGenerator for HostnameGenerator {
    fn generate(&self) -> String {
        use crate::syscall::system::read_hostname;
        let mut s = read_hostname();
        s.push('\n');
        s
    }
}

/// /proc/sys/kernel/domainname — reads/writes dynamic domainname from UTS_STATE
pub struct DomainnameGenerator;

impl ProcFileGenerator for DomainnameGenerator {
    fn generate(&self) -> String {
        use crate::syscall::system::read_domainname;
        let mut s = read_domainname();
        s.push('\n');
        s
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        use crate::syscall::system::write_domainname;
        write_domainname(data.trim());
        Ok(())
    }
}

/// /proc/sys/kernel/osrelease — uname -r 兼容值，匹配 LTP modules.dep 期望
pub struct OsReleaseGenerator;

impl ProcFileGenerator for OsReleaseGenerator {
    fn generate(&self) -> String {
        String::from("5.10.0\n")
    }
}

/// /proc/sys/kernel/threads-max —— 同时 task_struct 数上限。
pub struct ThreadsMaxGenerator;
impl ProcFileGenerator for ThreadsMaxGenerator {
    fn generate(&self) -> String {
        alloc::format!("{}\n", crate::sched::process::MAX_PROCESSES)
    }
}

/// /proc/sys/kernel/cap_last_cap —— 最大支持的 capability 号。
pub struct CapLastCapGenerator;
impl ProcFileGenerator for CapLastCapGenerator {
    fn generate(&self) -> String { String::from("40\n") }
}

/// /proc/sys/kernel/ostype —— `uname -s` 同款，固定 "Linux"。
pub struct OstypeGenerator;
impl ProcFileGenerator for OstypeGenerator {
    fn generate(&self) -> String { String::from("Linux\n") }
}

/// /proc/sys/kernel/version —— uname -v 同款（构建时刻字符串）。
pub struct KernelSysVersionGenerator;
impl ProcFileGenerator for KernelSysVersionGenerator {
    fn generate(&self) -> String {
        String::from("#1 SMP PREEMPT RustMicroOS Slice 2.5\n")
    }
}

/// `/proc/sys/kernel/core_pattern` — template for the core-dump file name.
/// Default "core". Writable (LTP `madvise08` sets it to `<cwd>/dump-%p` and
/// restores it via save_restore). Consulted by `crate::kernel::coredump`.
static CORE_PATTERN: SpinLock<Option<String>> = SpinLock::new(None);

/// Current `core_pattern` (default "core").
pub fn core_pattern() -> String {
    CORE_PATTERN.lock().clone().unwrap_or_else(|| String::from("core"))
}

pub struct CorePatternGenerator;
impl ProcFileGenerator for CorePatternGenerator {
    fn generate(&self) -> String { format!("{}\n", core_pattern()) }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        *CORE_PATTERN.lock() = Some(String::from(data.trim_end_matches('\n')));
        Ok(())
    }
}

/// /proc/sys/kernel/random/boot_id —— Linux 启动时一次性生成的 UUIDv4 字符串
pub struct BootIdGenerator;

static BOOT_ID_INIT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static BOOT_ID_BYTES: SpinLock<[u8; 16]> = SpinLock::new([0; 16]);

fn ensure_boot_id() {
    use core::sync::atomic::Ordering;
    if BOOT_ID_INIT.load(Ordering::Acquire) { return; }
    let mut buf = [0u8; 16];
    crate::random::fill_bytes(&mut buf);
    buf[6] = (buf[6] & 0x0f) | 0x40;
    buf[8] = (buf[8] & 0x3f) | 0x80;
    *BOOT_ID_BYTES.lock() = buf;
    BOOT_ID_INIT.store(true, Ordering::Release);
}

fn format_uuid(buf: &[u8; 16]) -> String {
    alloc::format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}\n",
        buf[0], buf[1], buf[2], buf[3],
        buf[4], buf[5],
        buf[6], buf[7],
        buf[8], buf[9],
        buf[10], buf[11], buf[12], buf[13], buf[14], buf[15]
    )
}

impl ProcFileGenerator for BootIdGenerator {
    fn generate(&self) -> String {
        ensure_boot_id();
        let buf = *BOOT_ID_BYTES.lock();
        format_uuid(&buf)
    }
}

/// /proc/sys/kernel/random/uuid —— 每次读取都生成新的 UUIDv4。
pub struct RandomUuidGenerator;
impl ProcFileGenerator for RandomUuidGenerator {
    fn generate(&self) -> String {
        let mut buf = [0u8; 16];
        crate::random::fill_bytes(&mut buf);
        buf[6] = (buf[6] & 0x0f) | 0x40;
        buf[8] = (buf[8] & 0x3f) | 0x80;
        format_uuid(&buf)
    }
}

/// /proc/sys/kernel/random/entropy_avail —— 估算可用熵 bit 数。
pub struct EntropyAvailGenerator;
impl ProcFileGenerator for EntropyAvailGenerator {
    fn generate(&self) -> String { String::from("256\n") }
}

/// /proc/sys/kernel/random/poolsize —— 熵池容量 bit 数，Linux 默认 256。
pub struct PoolSizeGenerator;
impl ProcFileGenerator for PoolSizeGenerator {
    fn generate(&self) -> String { String::from("256\n") }
}

/// /proc/sys/fs/file-max —— 系统级打开文件总数硬上限。
pub struct FileMaxGenerator;
impl ProcFileGenerator for FileMaxGenerator {
    fn generate(&self) -> String {
        alloc::format!("{}\n", 1024 * 1024)
    }
}

/// /proc/sys/fs/nr_open —— 单进程 fd 数硬上限。
pub struct NrOpenGenerator;
impl ProcFileGenerator for NrOpenGenerator {
    fn generate(&self) -> String {
        alloc::format!("{}\n", 1024 * 1024)
    }
}

/// /proc/sys/fs/file-nr —— "allocated  unused  max"。
pub struct FileNrGenerator;
impl ProcFileGenerator for FileNrGenerator {
    fn generate(&self) -> String {
        let allocated: usize = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table.as_ref().map(|t| {
                t.iter()
                    .map(|p| p.fd_table.iter().filter(|slot| slot.is_some()).count())
                    .sum()
            }).unwrap_or(0)
        };
        alloc::format!("{}\t0\t{}\n", allocated, 1024 * 1024)
    }
}

/// /proc/sys/fs/pipe-max-size —— 单个 pipe 缓冲最大字节数。
pub struct PipeMaxSizeGenerator;
static PIPE_MAX_SIZE: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(crate::fs::pipe::PIPE_MAX_SIZE_DEFAULT);

pub fn pipe_max_size() -> usize {
    PIPE_MAX_SIZE.load(core::sync::atomic::Ordering::Relaxed)
}

impl ProcFileGenerator for PipeMaxSizeGenerator {
    fn generate(&self) -> String {
        format!("{}\n", pipe_max_size())
    }

    fn writable(&self) -> bool { true }

    fn write(&self, data: &str) -> Result<(), ProcError> {
        let value = data.trim().parse::<usize>().map_err(|_| ProcError::InvalidArgument)?;
        if value < crate::fs::pipe::PIPE_MIN_SIZE || value > i32::MAX as usize {
            return Err(ProcError::InvalidArgument);
        }
        PIPE_MAX_SIZE.store(value, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/fs/pipe-user-pages-soft —— 单个用户所有 pipe 占用的总页数软上限。
pub struct PipeUserPagesSoftGenerator;

pub fn pipe_user_pages_soft() -> usize {
    crate::fs::pipe::pipe_user_pages_soft()
}

impl ProcFileGenerator for PipeUserPagesSoftGenerator {
    fn generate(&self) -> String {
        format!("{}\n", crate::fs::pipe::pipe_user_pages_soft())
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let value = data.trim().parse::<usize>().map_err(|_| ProcError::InvalidArgument)?;
        crate::fs::pipe::set_pipe_user_pages_soft(value);
        Ok(())
    }
}

/// /proc/sys/fs/pipe-user-pages-hard —— 单个用户所有 pipe 占用的总页数硬上限。
pub struct PipeUserPagesHardGenerator;

impl ProcFileGenerator for PipeUserPagesHardGenerator {
    fn generate(&self) -> String {
        format!("{}\n", crate::fs::pipe::pipe_user_pages_hard())
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let value = data.trim().parse::<usize>().map_err(|_| ProcError::InvalidArgument)?;
        crate::fs::pipe::set_pipe_user_pages_hard(value);
        Ok(())
    }
}

/// /proc/sys/fs/inotify/max_queued_events — Linux default 16384.
pub struct InotifyMaxQueuedEventsGenerator;
impl ProcFileGenerator for InotifyMaxQueuedEventsGenerator {
    fn generate(&self) -> String { String::from("16384\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/fs/inotify/max_user_instances — Linux default 128.
pub struct InotifyMaxUserInstancesGenerator;
impl ProcFileGenerator for InotifyMaxUserInstancesGenerator {
    fn generate(&self) -> String { String::from("128\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/fs/inotify/max_user_watches — Linux default 65536.
pub struct InotifyMaxUserWatchesGenerator;
impl ProcFileGenerator for InotifyMaxUserWatchesGenerator {
    fn generate(&self) -> String { String::from("65536\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/fs/mqueue/queues_max — Linux default 256; settable, enforced by
/// sys_mq_open (ENOSPC for unprivileged callers once the cap is reached).
pub struct MqueueQueuesMaxGenerator;
impl ProcFileGenerator for MqueueQueuesMaxGenerator {
    fn generate(&self) -> String {
        let mut s = String::new();
        let _ = core::fmt::write(&mut s, format_args!("{}\n", crate::syscall::mqueue::mq_queues_max_get()));
        s
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        match data.trim().parse::<usize>() {
            Ok(v) => { crate::syscall::mqueue::mq_queues_max_set(v); Ok(()) }
            Err(_) => Err(ProcError::InvalidArgument),
        }
    }
}

/// /proc/sys/fs/mqueue/msg_max — Linux default 10.
pub struct MqueueMsgMaxGenerator;
impl ProcFileGenerator for MqueueMsgMaxGenerator {
    fn generate(&self) -> String { String::from("10\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/fs/mqueue/msgsize_max — Linux default 8192.
pub struct MqueueMsgsizeMaxGenerator;
impl ProcFileGenerator for MqueueMsgsizeMaxGenerator {
    fn generate(&self) -> String { String::from("8192\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/fs/mqueue/msg_default — Linux default 10.
pub struct MqueueMsgDefaultGenerator;
impl ProcFileGenerator for MqueueMsgDefaultGenerator {
    fn generate(&self) -> String { String::from("10\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/fs/mqueue/msgsize_default — Linux default 8192.
pub struct MqueueMsgsizeDefaultGenerator;
impl ProcFileGenerator for MqueueMsgsizeDefaultGenerator {
    fn generate(&self) -> String { String::from("8192\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/fs/aio-max-nr — max number of AIO requests. Linux default 65536.
pub struct AioMaxNrGenerator;
impl ProcFileGenerator for AioMaxNrGenerator {
    fn generate(&self) -> String { String::from("65536\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/fs/aio-nr — current number of AIO contexts system-wide.
pub struct AioNrGenerator;
impl ProcFileGenerator for AioNrGenerator {
    fn generate(&self) -> String { String::from("0\n") }
}

/// 通用 0 整数 procfs 节点（用于 /proc/sys/fs/suid_dumpable 等只读 0 值）
pub struct ZeroIntGenerator;

impl ProcFileGenerator for ZeroIntGenerator {
    fn generate(&self) -> String {
        String::from("0\n")
    }
}

/// /proc/sys/vm/nr_hugepages —— default hugetlb pool size.
pub struct NrHugePagesGenerator;

impl ProcFileGenerator for NrHugePagesGenerator {
    fn generate(&self) -> String {
        format!("{}\n", crate::mm::hugepages_total())
    }

    fn writable(&self) -> bool { true }

    fn write(&self, data: &str) -> Result<(), ProcError> {
        let trimmed = data.trim();
        let value = trimmed.parse::<usize>().map_err(|_| ProcError::InvalidArgument)?;
        crate::mm::set_hugepages_total(value);
        Ok(())
    }
}

/// /proc/sys/vm/overcommit_memory —— 0/1/2, affecting MAP_NORESERVE policy.
pub struct OvercommitMemoryGenerator;

static OVERCOMMIT_MEMORY: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);

#[allow(dead_code)]
pub fn overcommit_memory_policy() -> i32 {
    OVERCOMMIT_MEMORY.load(core::sync::atomic::Ordering::Relaxed)
}

impl ProcFileGenerator for OvercommitMemoryGenerator {
    fn generate(&self) -> String {
        format!("{}\n", OVERCOMMIT_MEMORY.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let value = data.trim().parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        if !(0..=2).contains(&value) {
            return Err(ProcError::InvalidArgument);
        }
        OVERCOMMIT_MEMORY.store(value, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/vm/hugetlb_shm_group —— allowed SHM_HUGETLB gid, default 0.
pub struct HugetlbShmGroupGenerator;

static HUGETLB_SHM_GROUP: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(0);

#[allow(dead_code)]
pub fn hugetlb_shm_group() -> i32 {
    HUGETLB_SHM_GROUP.load(core::sync::atomic::Ordering::Relaxed)
}

impl ProcFileGenerator for HugetlbShmGroupGenerator {
    fn generate(&self) -> String {
        format!("{}\n", HUGETLB_SHM_GROUP.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let value = data.trim().parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        if value < 0 {
            return Err(ProcError::InvalidArgument);
        }
        HUGETLB_SHM_GROUP.store(value, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/vm/drop_caches — writable; writing 1/2/3 triggers cache drops.
pub struct DropCachesGenerator;
impl ProcFileGenerator for DropCachesGenerator {
    fn generate(&self) -> String { String::from("0\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v = data.trim().parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        if !(1..=4).contains(&v) { return Err(ProcError::InvalidArgument); }
        // Dropping the dentry/inode caches (echo 2 or 3) reclaims the inodes
        // that evictable fanotify marks were pinned to, so those marks are
        // destroyed too (Linux drops FAN_MARK_EVICTABLE marks on inode evict).
        if v == 2 || v == 3 {
            crate::fs::notify::fanotify::drop_caches_evict_marks();
        }
        Ok(())
    }
}

/// /proc/sys/vm/max_map_count — default 65530.
pub struct MaxMapCountGenerator;
static MAX_MAP_COUNT: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(65530);

/// 当前 `vm.max_map_count` 上限（供 mmap 强制单进程 VMA 数上限，max_map_count）。
pub fn max_map_count() -> i32 {
    MAX_MAP_COUNT.load(core::sync::atomic::Ordering::Relaxed)
}
impl ProcFileGenerator for MaxMapCountGenerator {
    fn generate(&self) -> String {
        format!("{}\n", MAX_MAP_COUNT.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v = data.trim().parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        if v < 0 { return Err(ProcError::InvalidArgument); }
        MAX_MAP_COUNT.store(v, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/vm/nr_overcommit_hugepages — surplus 大页配额上限（default 0）。
pub struct NrOvercommitHugePagesGenerator;
impl ProcFileGenerator for NrOvercommitHugePagesGenerator {
    fn generate(&self) -> String { format!("{}\n", crate::mm::hugetlb_overcommit()) }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let value = data.trim().parse::<usize>().map_err(|_| ProcError::InvalidArgument)?;
        crate::mm::hugetlb_set_overcommit(value);
        Ok(())
    }
}

/// /proc/sys/vm/compact_memory — write-only trigger for memory compaction.
pub struct CompactMemoryGenerator;
impl ProcFileGenerator for CompactMemoryGenerator {
    fn generate(&self) -> String { String::from("0\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/vm/vfs_cache_pressure — default 100.
pub struct VfsCachePressureGenerator;
static VFS_CACHE_PRESSURE: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(100);
impl ProcFileGenerator for VfsCachePressureGenerator {
    fn generate(&self) -> String {
        format!("{}\n", VFS_CACHE_PRESSURE.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v = data.trim().parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        if v < 0 { return Err(ProcError::InvalidArgument); }
        VFS_CACHE_PRESSURE.store(v, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/vm/swappiness — 内核换页倾向，0..=200，默认 60（对标 Linux）。
/// LTP madvise06/09 在 setup 阶段读写它来控制 reclaim 行为。
pub struct SwappinessGenerator;
static SWAPPINESS: core::sync::atomic::AtomicI32 = core::sync::atomic::AtomicI32::new(60);
pub fn swappiness() -> i32 {
    SWAPPINESS.load(core::sync::atomic::Ordering::Relaxed)
}
impl ProcFileGenerator for SwappinessGenerator {
    fn generate(&self) -> String {
        format!("{}\n", SWAPPINESS.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v = data.trim().parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        if !(0..=200).contains(&v) { return Err(ProcError::InvalidArgument); }
        SWAPPINESS.store(v, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/fs/lease-break-time —— current lease-break timeout in seconds.
pub struct LeaseBreakTimeGenerator;

impl ProcFileGenerator for LeaseBreakTimeGenerator {
    fn generate(&self) -> String {
        format!("{}\n", crate::fs::file_lease::break_seconds())
    }

    fn writable(&self) -> bool {
        true
    }

    fn write(&self, data: &str) -> Result<(), ProcError> {
        let trimmed = data.trim();
        let value = trimmed.parse::<i32>().map_err(|_| ProcError::InvalidArgument)?;
        if !(0..=255).contains(&value) {
            return Err(ProcError::InvalidArgument);
        }
        crate::fs::file_lease::set_break_seconds(value);
        Ok(())
    }
}
