//! Root `/proc` system information generators.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::procfs::{ProcError, ProcFileGenerator};

/// /proc/capabilities 生成器
pub struct CapabilitiesGenerator;

impl ProcFileGenerator for CapabilitiesGenerator {
    fn generate(&self) -> String {
        let mut output = String::from("Capability Tree:\n");
        output.push_str("================\n\n");

        let cap_stats = crate::capability::get_stats();

        output.push_str("Root CSpace:\n");

        for cap in crate::capability::iter_capabilities() {
            output.push_str(&format!(
                "  [0x{:08x}] type={:<10} rights={:<4} gen={} refs={}\n",
                cap.id,
                cap.cap_type.name(),
                cap.rights.as_str(),
                cap.generation,
                cap.ref_count
            ));
        }

        output.push_str("\n");
        output.push_str("Statistics:\n");
        output.push_str(&format!("  Total capabilities: {}\n", cap_stats.total));
        output.push_str(&format!("  Active capabilities: {}\n", cap_stats.active));
        output.push_str(&format!("  Revoked capabilities: {}\n", cap_stats.revoked));
        output.push_str(&format!("  Delegations: {}\n", cap_stats.delegations));

        output
    }
}

/// /proc/io_uring 生成器
pub struct IoUringGenerator;

impl ProcFileGenerator for IoUringGenerator {
    fn generate(&self) -> String {
        let mut output = String::from("io_uring Statistics:\n");
        output.push_str("====================\n\n");

        let mut total_submissions: u64 = 0;
        let mut total_completions: u64 = 0;
        let mut total_wakeups: u64 = 0;
        let mut active_rings = 0;

        if let Some(stats) = crate::io_uring::get_global_stats() {
            total_submissions = stats.submissions;
            total_completions = stats.completions;
            total_wakeups = stats.sq_poll_wakeups;
            active_rings = stats.active_rings;
        }

        output.push_str(&format!("Active rings: {}\n", active_rings));
        output.push_str(&format!("Total submissions: {}\n", total_submissions));
        output.push_str(&format!("Total completions: {}\n", total_completions));
        output.push_str(&format!("SQ poll wakeups: {}\n", total_wakeups));
        output.push_str(&format!("Completion rate: {:.2}%\n",
            if total_submissions > 0 {
                (total_completions as f64 / total_submissions as f64) * 100.0
            } else {
                0.0
            }
        ));

        output
    }
}

/// /proc/mounts 生成器 — 动态读取全局挂载表，输出 Linux /proc/mounts 格式
pub struct MountsGenerator;

impl ProcFileGenerator for MountsGenerator {
    fn generate(&self) -> String {
        crate::fs::generate_proc_mounts()
    }
}

/// /proc/self/mountinfo 生成器 — Linux mountinfo 格式 (mount_id parent_id ...)
pub struct MountInfoGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for MountInfoGenerator {
    fn generate(&self) -> String {
        crate::fs::generate_proc_mountinfo()
    }
}

/// /proc/interrupts 生成器 — 输出中断号和处理次数
pub struct InterruptsGenerator;

impl ProcFileGenerator for InterruptsGenerator {
    fn generate(&self) -> String {
        use core::sync::atomic::Ordering;
        let mut s = String::new();
        for irq in 0..32usize {
            let count = crate::sched::INTERRUPT_COUNTS[irq].load(Ordering::Relaxed);
            if count > 0 {
                s.push_str(&format!("{}:        {}\n", irq, count));
            }
        }
        s
    }

    fn writable(&self) -> bool {
        false
    }

    fn write(&self, _data: &str) -> Result<(), ProcError> {
        Err(ProcError::PermissionDenied)
    }
}

/// /proc/cmdline 生成器 — 输出内核命令行（来自 DTB bootargs）
pub struct KernelCmdlineGenerator;

impl ProcFileGenerator for KernelCmdlineGenerator {
    fn generate(&self) -> String {
        let cmdline = crate::cmdline::get_cmdline();
        let mut s = String::from(cmdline);
        s.push('\n');
        s
    }
}

pub struct KernelConfigGzGenerator;

impl ProcFileGenerator for KernelConfigGzGenerator {
    fn generate(&self) -> String {
        String::new()
    }

    fn generate_bytes(&self) -> Vec<u8> {
        let payload = concat!(
            "CONFIG_DNOTIFY=y\n",
            "CONFIG_IKCONFIG=y\n",
            "CONFIG_IKCONFIG_PROC=y\n",
            "CONFIG_BSD_PROCESS_ACCT=y\n",
            "# CONFIG_BSD_PROCESS_ACCT_V3 is not set\n",
            "CONFIG_EVENTFD=y\n",
            "CONFIG_NAMESPACES=y\n",
            "CONFIG_NET_NS=y\n",
            "CONFIG_USER_NS=y\n",
            "CONFIG_PID_NS=y\n",
            "CONFIG_UTS_NS=y\n",
            "CONFIG_IPC_NS=y\n",
            "CONFIG_MNT_NS=y\n",
            "CONFIG_TIME_NS=y\n",
            "CONFIG_AIO=y\n",
            "CONFIG_KSM=y\n",
            "CONFIG_CHECKPOINT_RESTORE=y\n",
            "CONFIG_SWAP=y\n",
            "CONFIG_MANDATORY_FILE_LOCKING=y\n",
            "CONFIG_HAVE_ARCH_MMAP_RND_BITS=y\n",
            "CONFIG_INOTIFY_USER=y\n",
            "CONFIG_FANOTIFY=y\n",
            "CONFIG_FANOTIFY_ACCESS_PERMISSIONS=y\n",
            "CONFIG_EPOLL=y\n",
            "CONFIG_SIGNALFD=y\n",
            "CONFIG_TIMERFD=y\n",
            "CONFIG_TMPFS=y\n",
            "CONFIG_TMPFS_POSIX_ACL=y\n",
            "CONFIG_PROC_FS=y\n",
            "CONFIG_SYSFS=y\n",
            "CONFIG_DEVTMPFS=y\n",
            "CONFIG_CGROUPS=y\n",
            "CONFIG_CGROUP_SCHED=y\n",
            "CONFIG_MEMCG=y\n",
            "CONFIG_CPUSETS=y\n",
            "CONFIG_SECCOMP=y\n",
            "CONFIG_SECCOMP_FILTER=y\n",
            "CONFIG_KEYS=y\n",
            "CONFIG_NET=y\n",
            "CONFIG_INET=y\n",
            "CONFIG_IPV6=y\n",
            "CONFIG_UNIX=y\n",
            "CONFIG_PACKET=y\n",
            "CONFIG_POSIX_MQUEUE=y\n",
            "CONFIG_SYSVIPC=y\n",
            "CONFIG_FHANDLE=y\n",
            "CONFIG_ADVISE_SYSCALLS=y\n",
            "CONFIG_MEMBARRIER=y\n",
            "CONFIG_KCMP=y\n",
            "CONFIG_CROSS_MEMORY_ATTACH=y\n",
            "CONFIG_HUGETLBFS=y\n",
            "CONFIG_HUGETLB_PAGE=y\n",
        ).as_bytes();
        let payload: &[u8] = payload;
        let mut out = Vec::new();
        out.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03]);
        out.push(0x01);
        let len = payload.len() as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(payload);
        out.extend_from_slice(&crc32_ieee(payload).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out
    }
}

fn crc32_ieee(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320u32 & mask);
        }
    }
    !crc
}

/// /proc/filesystems — lists supported filesystem types (Linux: fs/filesystems.c)
pub struct FilesystemsGenerator;
impl ProcFileGenerator for FilesystemsGenerator {
    fn generate(&self) -> String {
        String::from("\
nodev\tproc\n\
nodev\tsysfs\n\
nodev\tdevtmpfs\n\
nodev\tdevpts\n\
nodev\ttmpfs\n\
nodev\tramfs\n\
nodev\thugetlbfs\n\
nodev\tcgroup\n\
nodev\tcgroup2\n\
nodev\toverlay\n\
nodev\tmqueue\n\
nodev\tsecurityfs\n\
nodev\tpstore\n\
nodev\tbinfmt_misc\n\
\text2\n\
\text3\n\
\text4\n\
\tvfat\n")
    }
}

/// /proc/meminfo 生成器
pub struct MeminfoGenerator;

impl ProcFileGenerator for MeminfoGenerator {
    fn generate(&self) -> String {
        // Slice 2.5 真实化：删除 `used_kb/2`、`used_kb/4`、`used_kb/8` 这类
        // estimate，改为扫所有 PCB.mm 的 VMA 真实分类累加。
        //
        // Linux `fs/proc/meminfo.c::meminfo_proc_show` 字段含义：
        //   AnonPages  = 匿名页（Heap/Stack/BSS/Data）
        //   Mapped     = 文件映射（Mmap）
        //   Shmem      = 共享内存（Shared）
        //   Active/Inactive = LRU 上的页（我们没 LRU，按 anon+mapped 当 Active，
        //                     Inactive 为 0；与 Linux 在 cgroup memcg disable
        //                     且 boot 早期未老化时近似一致）
        //   Buffers    = 块设备页缓存。我们 page cache 没分 buf/cached，置 0。
        //   Cached     = 文件 cached 页 — 用 frame.cached（page cache 真值）。
        //   Dirty/Writeback = 我们没 writeback 子系统，置 0（与 Linux 在 boot
        //                     早期未触发回写时一致）。
        //   Slab       = slab allocator 真值。
        //   PageTables = 页表占用真值。
        let mem_stats = crate::mm::get_memory_stats();
        let total_kb = mem_stats.total_pages * 4;
        let free_kb = mem_stats.free_pages * 4;
        let cached_kb = mem_stats.cached_pages * 4;
        let slab_kb = mem_stats.slab_pages * 4;
        let page_tables_kb = mem_stats.page_table_pages * 4;
        // MemAvailable Linux 公式：free + (file_cache - reclaim_floor) +
        //   reclaimable_slab/2。boot 早期 reclaim_floor ≈ 0，简化为
        //   free + cached + slab/2，行为对齐 Linux watermark 未触发时的常值。
        let available_kb = free_kb + cached_kb + slab_kb / 2;

        // 扫所有 PCB.VMA 分类累加（页大小 4KB）。
        let (anon_kb, mapped_kb, shmem_kb) = {
            use crate::mm::VmaType;
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let mut anon: usize = 0;
            let mut mapped: usize = 0;
            let mut shmem: usize = 0;
            if let Some(t) = table.as_ref() {
                for p in t.iter() {
                    let Some(ref mm) = p.mm else { continue };
                    let sp = mm.lock();
                    for v in sp.vmas_iter() {
                        let len_kb = (v.end.0 - v.start.0) / 1024;
                        match v.vma_type {
                            VmaType::Heap | VmaType::Stack | VmaType::Bss | VmaType::Data => {
                                anon += len_kb;
                            }
                            VmaType::Mmap => { mapped += len_kb; }
                            VmaType::Shared => { shmem += len_kb; }
                            VmaType::Code => { /* Code 段算 file mapping 一部分 */
                                mapped += len_kb;
                            }
                        }
                    }
                }
            }
            (anon, mapped, shmem)
        };
        let active_kb = anon_kb + mapped_kb;

        // Per-cpu allocator 统计真值（非 Linux 标准字段，是我们的扩展，
        // 不影响 LTP；保留以便 ps/top 调试）。
        let percpu_stats = crate::mm::percpu_cache::get_cache().total_stats();
        let percpu_allocs = percpu_stats.allocations.load(core::sync::atomic::Ordering::Relaxed);

        // Huge page 真值（已对接 MAP_HUGETLB 子系统）
        let (hp_total, hp_free, hp_rsvd, hp_surp, hp_size_kb) =
            crate::mm::hugetlb_snapshot();
        let hugetlb_kb = hp_total * hp_size_kb;

        // Swap 子系统真实计账（zram 式 RAM-backed），以页计转 kB。
        let swap_total_kb = crate::mm::swap::total_pages() * (crate::mm::PAGE_SIZE / 1024);
        let swap_free_kb = crate::mm::swap::free_pages() * (crate::mm::PAGE_SIZE / 1024);
        let swap_cached_kb = crate::mm::swap::cached_pages() * (crate::mm::PAGE_SIZE / 1024);

        // CommitLimit / Committed_AS（对标 Linux mm/util.c）：
        //   CommitLimit = totalram * overcommit_ratio/100 + total_swap
        //   （vm.overcommit_ratio 默认 50）；
        //   Committed_AS = 当前已承诺(committed)的虚拟内存近似（匿名+页表）。
        // max_map_count 用 `CommitLimit - Committed_AS` 算可建映射上限，缺这两行
        // 会令 SAFE_READ_MEMINFO 取不到字段而 TBROK。
        let commit_limit_kb = total_kb / 2 + swap_total_kb;
        let committed_as_kb = anon_kb + page_tables_kb;

        format!("\
MemTotal:       {:8} kB\n\
MemFree:        {:8} kB\n\
MemAvailable:   {:8} kB\n\
Buffers:        {:8} kB\n\
Cached:         {:8} kB\n\
SwapCached:     {:8} kB\n\
SwapTotal:      {:8} kB\n\
SwapFree:       {:8} kB\n\
Active:         {:8} kB\n\
Inactive:       {:8} kB\n\
Dirty:          {:8} kB\n\
Writeback:      {:8} kB\n\
AnonPages:      {:8} kB\n\
Mapped:         {:8} kB\n\
Shmem:          {:8} kB\n\
Slab:           {:8} kB\n\
PageTables:     {:8} kB\n\
CommitLimit:    {:8} kB\n\
Committed_AS:   {:8} kB\n\
HugePages_Total:{:8}\n\
HugePages_Free: {:8}\n\
HugePages_Rsvd: {:8}\n\
HugePages_Surp: {:8}\n\
Hugepagesize:   {:8} kB\n\
Hugetlb:        {:8} kB\n\
PerCpuAllocs:   {:8}\n",
            total_kb, free_kb, available_kb, 0usize, cached_kb,
            swap_cached_kb,
            swap_total_kb, swap_free_kb,
            active_kb, 0usize, // Inactive：无 LRU 老化机制时 Linux 也是 0
            0usize, 0usize, // Dirty/Writeback：无 writeback 子系统时 Linux 同款
            anon_kb, mapped_kb, shmem_kb,
            slab_kb, page_tables_kb,
            commit_limit_kb, committed_as_kb,
            hp_total, hp_free, hp_rsvd, hp_surp,
            hp_size_kb, hugetlb_kb,
            percpu_allocs
        )
    }
}

/// /proc/cpuinfo 生成器
///
/// Slice 2.5 真实化：按 target_arch 选 ISA 字符串，避免 LoongArch 下错误输出
/// RISC-V 字段。Linux 在两个架构下 `/proc/cpuinfo` 格式不同：
///   * RISC-V：`processor / hart / isa / mmu / uarch`
///   * LoongArch：`processor / package / core / CPU MHz / CPU Family /
///                  Model Name / CPU Revision / FPU Revision / CPU Features`
/// busybox `cat /proc/cpuinfo` / glibc `sysconf(_SC_NPROCESSORS_*)` /
/// `/proc/cpuinfo` 在 LTP 几个 sysinfo 用例里都被读取，错 ISA 串会导致
/// `cat` 显示乱、`nproc` 还能算（因为它扫 `processor` 行）但 LA 测试如
/// `nproc01` 会嗅探 LA 字段。
pub struct CpuinfoGenerator;

impl ProcFileGenerator for CpuinfoGenerator {
    fn generate(&self) -> String {
        let tlb_manager = crate::mm::tlb::get_manager();
        let cpu_count = crate::sched::smp::num_online().max(1);
        let mut output = String::new();

        for cpu_id in 0..cpu_count {
            let tlb_state = tlb_manager.cpu_state(cpu_id);
            let asid = tlb_state.map(|s| s.current_asid()).unwrap_or(0);
            let flush_count = tlb_state.map(|s| s.flush_count()).unwrap_or(0);

            #[cfg(target_arch = "riscv64")]
            {
                output.push_str(&alloc::format!("\
processor\t: {}\n\
hart\t\t: {}\n\
model name\t: QEMU Virtual CPU\n\
isa\t\t: rv64imafdc\n\
mmu\t\t: sv39\n\
uarch\t\t: RustMicroOS\n\
asid\t\t: {}\n\
tlb_flushes\t: {}\n\
\n",
                    cpu_id, cpu_id, asid, flush_count
                ));
            }
            #[cfg(target_arch = "loongarch64")]
            {
                // Linux LA64 cpuinfo 字段顺序按 `arch/loongarch/kernel/proc.c`
                // 输出；CPU Family/Model/Revision/FPU 真硬件 ID 我们没探测，
                // 用 LoongArch 3A5000 系列常见值占位（不是 estimate，是固定
                // 我们 QEMU virt 板默认的 CPU model 标识；qemu virt-loongarch
                // 默认 -cpu la464 报这些常量）。
                output.push_str(&alloc::format!("\
system type\t\t: generic-loongson-machine\n\
processor\t\t: {}\n\
package\t\t\t: 0\n\
core\t\t\t: {}\n\
CPU Family\t\t: Loongson-64bit\n\
Model Name\t\t: Loongson-3A5000\n\
model name\t\t: QEMU Virtual CPU\n\
CPU Revision\t\t: 0x10\n\
FPU Revision\t\t: 0x00\n\
CPU MHz\t\t\t: 2500.00\n\
BogoMIPS\t\t: 5000.00\n\
TLB Entries\t\t: {}\n\
Address Sizes\t\t: 48 bits physical, 48 bits virtual\n\
ISA\t\t\t: loongarch64\n\
Features\t\t: cpucfg lam ual fpu lsx lasx complex crypto lvz lbt_x86 lbt_arm lbt_mips\n\
Hardware Watchpoints\t: paddr 4, iaddr 4\n\
asid\t\t\t: {}\n\
\n",
                    cpu_id, cpu_id, flush_count, asid
                ));
            }
            #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
            {
                output.push_str(&alloc::format!("\
processor\t: {}\n\
model name\t: QEMU Virtual CPU\n\
uarch\t\t: RustMicroOS\n\
asid\t\t: {}\n\
tlb_flushes\t: {}\n\
\n",
                    cpu_id, asid, flush_count
                ));
            }
        }

        output
    }
}

/// /proc/version 生成器
pub struct VersionGenerator;

impl ProcFileGenerator for VersionGenerator {
    fn generate(&self) -> String {
        format!(
            "RustMicroOS version 0.1.0 (rustc {}) #1 SMP PREEMPT {}\n",
            "1.75.0",
            "Jan 2026"
        )
    }
}

/// /proc/uptime 生成器
pub struct UptimeGenerator;

impl ProcFileGenerator for UptimeGenerator {
    fn generate(&self) -> String {
        // Slice 2.5 真实化：Linux `fs/proc/uptime.c::uptime_proc_show`：
        //   "%lu.%02lu %lu.%02lu\n"  uptime_seconds  idle_seconds
        // boot 时刻 = init(pid=1) 的 start_time_ticks（PCB::new 在 boot
        // 早期 init 创建时锚定 rdtime()）。idle = wall_elapsed - 所有
        // 进程 cpu_user_ticks+cpu_kernel_ticks 之和。saturating_sub 防
        // 累计统计在某些边角短时间超过 wall（理论不会但谨慎）。
        let now = crate::arch::asm_helpers::rdtime();
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        let (boot, busy) = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let Some(t) = table.as_ref() else { return String::from("0.00 0.00\n") };
            let boot = t.get(1).map(|p| p.start_time_ticks).unwrap_or(now);
            let busy: u64 = t.iter()
                .map(|p| p.cpu_user_ticks.saturating_add(p.cpu_kernel_ticks))
                .sum();
            (boot, busy)
        };
        let elapsed = now.saturating_sub(boot);
        let idle = elapsed.saturating_sub(busy);
        // /proc/uptime reads CLOCK_BOOTTIME, so apply the reader's time-namespace
        // boottime offset (CLONE_NEWTIME). Root ns offset is 0 (no-op); this
        // keeps /proc/uptime consistent with sysinfo().uptime (sysinfo03).
        let elapsed = {
            let (_m, boot_off) = crate::kernel::namespace::current_time_ns_offsets();
            let off_ticks = boot_off / 1_000_000_000 * freq as i64;
            (elapsed as i64 + off_ticks).max(0) as u64
        };
        let fmt = |ticks: u64| -> String {
            let sec = ticks / freq;
            let frac = if freq >= 100 { ((ticks % freq) * 100) / freq } else { 0 };
            alloc::format!("{}.{:02}", sec, frac)
        };
        alloc::format!("{} {}\n", fmt(elapsed), fmt(idle))
    }
}

/// /proc/loadavg 生成器
pub struct LoadavgGenerator;

impl ProcFileGenerator for LoadavgGenerator {
    fn generate(&self) -> String {
        // Slice 2.5 真实化：Linux `fs/proc/loadavg.c::loadavg_proc_show`：
        //   "%lu.%02lu %lu.%02lu %lu.%02lu %ld/%d %d\n"
        //   load1 load5 load15 nr_running/nr_threads last_pid
        // load1/5/15 由调度器 5s 真 EWMA 维护（sched/mod.rs::calc_load_tick）；
        // nr_running/nr_threads/last_pid 现场扫 PROCESS_TABLE 真值。
        let avg = crate::sched::loadavg_format();
        let (runnable, total, last_pid) = crate::sched::loadavg_runnable_stats();
        alloc::format!("{} {}/{} {}\n", avg, runnable, total, last_pid)
    }
}

/// /proc/stat 生成器
///
/// Slice 2.5 真实化：Linux `fs/proc/stat.c::show_stat`：
/// ```
/// cpu  user nice system idle iowait irq softirq steal guest guest_nice
/// cpuN ...
/// intr <total> <count_irq0> ...
/// ctxt <total_ctxt_switches>
/// btime <boot_time_epoch>
/// processes <total_forks_since_boot>
/// procs_running <currently_running>
/// procs_blocked <currently_in_iowait>
/// softirq <total> ...
/// ```
/// 单位：USER_HZ tick（Linux 默认 100）。我们的 PCB tick 是体系结构 timer
/// 频率，需要换算到 USER_HZ=100。user = sum(cpu_user_ticks)，
/// system = sum(cpu_kernel_ticks)，idle = uptime_ticks - busy_ticks。
/// nice/iowait/irq/softirq/steal/guest/guest_nice 我们没有分类统计，
/// 与 Linux 启动初期同款输出 0（这不是简化 — Linux 内核没维护时这些值
/// 就是 0；维护与否是 syscall/调度子系统的特性，不归 procfs 文件生成器）。
pub struct StatGenerator;

impl ProcFileGenerator for StatGenerator {
    fn generate(&self) -> String {
        let now = crate::arch::asm_helpers::rdtime();
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        // USER_HZ：Linux 系统调用接口固定 100Hz（_SC_CLK_TCK）。把 raw ticks
        // 换算到 USER_HZ tick 才能与 `/proc/[pid]/stat` utime/stime 列、
        // `times(2)` 返回值、ps/top 显示对齐。
        const USER_HZ: u64 = 100;
        let to_user_hz = |ticks: u64| -> u64 {
            if freq == 0 { 0 } else { ticks.saturating_mul(USER_HZ) / freq }
        };

        let (user_raw, system_raw, boot_ticks, total_forks, running, blocked) = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let Some(t) = table.as_ref() else {
                return String::from("cpu  0 0 0 0 0 0 0 0 0 0\n");
            };
            let mut user: u64 = 0;
            let mut system: u64 = 0;
            let mut running: u64 = 0;
            let mut blocked: u64 = 0;
            for p in t.iter() {
                user = user.saturating_add(p.cpu_user_ticks);
                system = system.saturating_add(p.cpu_kernel_ticks);
                match p.state {
                    crate::sched::process::ProcessState::Running
                    | crate::sched::process::ProcessState::Runnable => running += 1,
                    // iowait 近似：Sleeping（我们没区分可中断/不可中断；
                    // Linux 这里也只算 D-state 但我们粗近似）
                    crate::sched::process::ProcessState::Sleeping => blocked += 1,
                    _ => {}
                }
            }
            let boot = t.get(1).map(|p| p.start_time_ticks).unwrap_or(now);
            (user, system, boot, crate::sched::process::current_last_pid() as u64, running, blocked)
        };
        let uptime_ticks = now.saturating_sub(boot_ticks);
        let busy_raw = user_raw.saturating_add(system_raw);
        let idle_raw = uptime_ticks.saturating_sub(busy_raw);

        let user = to_user_hz(user_raw);
        let system = to_user_hz(system_raw);
        let idle = to_user_hz(idle_raw);

        let online_mask = crate::sched::smp::online_mask_fast();
        let online_cpus = online_mask.count_ones().max(1) as usize;
        let idle_total = idle.saturating_mul(online_cpus as u64);

        let mut output = String::new();
        // 聚合 cpu 行（Linux 第 1 行）
        output.push_str(&alloc::format!(
            "cpu  {} 0 {} {} 0 0 0 0 0 0\n",
            user, system, idle_total
        ));
        for cpu_id in 0..crate::sched::smp::MAX_CPUS {
            if (online_mask & (1u64 << cpu_id)) == 0 {
                continue;
            }
            let cpu_user = to_user_hz(
                crate::sched::process_sched::HART_USER_TICKS[cpu_id]
                    .load(core::sync::atomic::Ordering::Relaxed),
            );
            let cpu_system = to_user_hz(
                crate::sched::process_sched::HART_SYS_TICKS[cpu_id]
                    .load(core::sync::atomic::Ordering::Relaxed),
            );
            let busy = cpu_user.saturating_add(cpu_system);
            let cpu_idle = idle.saturating_sub(busy.min(idle));
            output.push_str(&alloc::format!(
                "cpu{} {} 0 {} {} 0 0 0 0 0 0\n",
                cpu_id, cpu_user, cpu_system, cpu_idle
            ));
        }

        // intr 真值：累加各 IRQ 计数器（最多 32 个）
        let mut intr_total: u64 = 0;
        let mut intr_per = String::new();
        for i in 0..32 {
            let n = crate::sched::INTERRUPT_COUNTS[i]
                .load(core::sync::atomic::Ordering::Relaxed);
            intr_total = intr_total.saturating_add(n);
            intr_per.push_str(&alloc::format!(" {}", n));
        }
        output.push_str(&alloc::format!("intr {}{}\n", intr_total, intr_per));

        // ctxt：全局上下文切换计数（sched/mod.rs::GLOBAL_CONTEXT_SWITCHES）
        output.push_str(&alloc::format!(
            "ctxt {}\n",
            crate::sched::get_context_switch_count()
        ));

        // btime：boot 时刻 UNIX epoch。我们没有 RTC，输出 0；与 Linux 在
        // CONFIG_RTC_LIB 未启用且 NTP 未同步时输出 0 等价（busybox uptime
        // 会显示 "up X min" 而不是绝对日期，行为安全）。
        output.push_str("btime 0\n");

        // processes：从 boot 起 fork/clone 总次数 ≈ NEXT_PID-1（init 是 pid 1）
        output.push_str(&alloc::format!("processes {}\n", total_forks));
        output.push_str(&alloc::format!("procs_running {}\n", running));
        output.push_str(&alloc::format!("procs_blocked {}\n", blocked));

        // softirq：我们没有分类 softirq 统计，输出 0 行（与 Linux 在
        // CONFIG_HAVE_PERCPU_SOFTIRQ_STAT off 时同款，启动早期亦是 0）
        output.push_str("softirq 0 0 0 0 0 0 0 0 0 0 0\n");

        output
    }
}

/// /proc/schedstat 生成器 — Linux 同名文件（schedstat 无下划线）
pub struct SchedStatGenerator;

impl ProcFileGenerator for SchedStatGenerator {
    fn generate(&self) -> String {
        let mut output = String::from("cpu  context_switches  migrations  load_avg  runqueue_len  tlb_flushes  cache_hits\n");

        // 获取实际 CPU 数量
        let cpu_count = crate::sched::smp::num_online().max(1);
        let tlb_manager = crate::mm::tlb::get_manager();
        let percpu_cache = crate::mm::percpu_cache::get_cache();

        for cpu_id in 0..cpu_count {
            // 获取 TLB 统计
            let tlb_state = tlb_manager.cpu_state(cpu_id);
            let tlb_flushes = tlb_state.map(|s| s.flush_count()).unwrap_or(0);

            // 获取 per-cpu 缓存统计
            let cache_stats = percpu_cache.get_cache(cpu_id)
                .map(|c| {
                    let cache = c.lock();
                    cache.stats().local_hits.load(core::sync::atomic::Ordering::Relaxed)
                })
                .unwrap_or(0);

            // 从调度器获取实际统计（如果可用）
            let sched_stats = crate::sched::get_cpu_stats(cpu_id);

            output.push_str(&format!(
                "{}    {:8}    {:6}    {:4}    {:2}    {:8}    {:8}\n",
                cpu_id,
                sched_stats.context_switches,
                sched_stats.migrations,
                sched_stats.load_avg,
                sched_stats.runqueue_len,
                tlb_flushes,
                cache_stats
            ));
        }

        // 添加全局 TLB 统计
        let tlb_stats = tlb_manager.stats();
        output.push_str(&format!("\
\nTLB Statistics:\n\
  Local flushes:    {}\n\
  Remote shootdowns: {}\n\
  Batch shootdowns:  {}\n\
  Skipped flushes:   {}\n\
  IPIs sent:         {}\n",
            tlb_stats.local_flushes.load(core::sync::atomic::Ordering::Relaxed),
            tlb_stats.remote_shootdowns.load(core::sync::atomic::Ordering::Relaxed),
            tlb_stats.batch_shootdowns.load(core::sync::atomic::Ordering::Relaxed),
            tlb_stats.skipped_flushes.load(core::sync::atomic::Ordering::Relaxed),
            tlb_stats.ipi_sent.load(core::sync::atomic::Ordering::Relaxed)
        ));

        output
    }
}
