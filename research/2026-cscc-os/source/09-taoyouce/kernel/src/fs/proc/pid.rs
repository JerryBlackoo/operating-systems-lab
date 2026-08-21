use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::procfs::{ProcError, ProcFileGenerator};

/// 获取当前进程 PID
pub fn get_current_pid() -> Option<usize> {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table.as_ref()?.current
}

/// 获取所有活跃进程 PID
pub fn get_all_pids() -> Vec<usize> {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(t) = table.as_ref() {
        t.pids()
    } else {
        Vec::new()
    }
}

/// 返回 pid 所属 tgid（如 pid 不存在则 None）。
pub fn pid_tgid(pid: usize) -> Option<usize> {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table.as_ref()?.get(pid).map(|p| p.tgid)
}

/// `/proc/[pid]/fd` and `/proc/[pid]/fdinfo` directory entries.
pub fn open_fd_names(pid: usize) -> Option<Vec<String>> {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_ref()?;
    let p = t.get(pid)?;
    let mut names = Vec::new();
    for (fd, slot) in p.fd_table.iter().enumerate() {
        if slot.is_some() {
            names.push(format!("{}", fd));
        }
    }
    Some(names)
}

/// /proc/[pid]/status 生成器 — 从进程表读取真实数据
pub struct ProcessStatusGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessStatusGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_ref() {
            if let Some(p) = t.get(self.pid as usize) {
                let (sc, sn) = match p.state {
                    crate::sched::process::ProcessState::Running |
                    crate::sched::process::ProcessState::Runnable => ('R', "running"),
                    crate::sched::process::ProcessState::Sleeping => ('S', "sleeping"),
                    crate::sched::process::ProcessState::Stopped => ('T', "stopped"),
                    crate::sched::process::ProcessState::Zombie => ('Z', "zombie"),
                    _ => ('S', "sleeping"),
                };
                let ppid = p.parent_pid.unwrap_or(0);
                let fd_count = p.fd_table.len();
                let vm_size_bytes: u64 = p
                    .mm
                    .as_ref()
                    .map(|mm| { let sp = mm.lock(); sp.vmas_iter().map(|v| (v.end.0 - v.start.0) as u64).sum() })
                    .unwrap_or(0);
                let vm_kb = (vm_size_bytes + 1023) / 1024;
                let vm_lck_kb = p
                    .mm
                    .as_ref()
                    .map(|mm| { let sp = mm.lock(); (sp.locked_vm_bytes() + 1023) / 1024 })
                    .unwrap_or(0);
                let vm_swap_kb = p
                    .mm
                    .as_ref()
                    .map(|mm| { let sp = mm.lock(); (sp.swapped_pages() * crate::mm::PAGE_SIZE) as u64 / 1024 })
                    .unwrap_or(0);
                let threads = t.iter().filter(|q| q.tgid == p.tgid).count();
                let mut sig_ign: u64 = 0;
                let mut sig_cgt: u64 = 0;
                for (i, h) in p.signal_handlers.iter().enumerate() {
                    let bit = 1u64 << i;
                    match h.handler {
                        0 => {}
                        1 => sig_ign |= bit,
                        _ => sig_cgt |= bit,
                    }
                }
                let no_new_privs = if p.no_new_privs { 1 } else { 0 };
                // Cpus_allowed / Mems_allowed（Linux /proc/PID/status 标准字段）。
                // 与 sched_getaffinity 报告的在线掩码一致；缺失会让 rt-tests
                // cyclictest 等程序无法确定可用 CPU 集合而早退（la-musl 复现）。
                let cpu_mask = p.cpus_allowed & crate::sched::smp::online_mask_fast();
                let cpus_allowed_hex = format!("{:x}", cpu_mask);
                let cpus_allowed_list = {
                    let mut s = alloc::string::String::new();
                    let bits = core::mem::size_of::<usize>() * 8;
                    let mut i = 0usize;
                    while i < bits {
                        if (cpu_mask >> i) & 1 == 1 {
                            let start = i;
                            while i + 1 < bits && (cpu_mask >> (i + 1)) & 1 == 1 {
                                i += 1;
                            }
                            if !s.is_empty() {
                                s.push(',');
                            }
                            if start == i {
                                s.push_str(&format!("{}", start));
                            } else {
                                s.push_str(&format!("{}-{}", start, i));
                            }
                        }
                        i += 1;
                    }
                    if s.is_empty() {
                        s.push('0');
                    }
                    s
                };
                return format!("\
Name:\t{}\n\
Umask:\t0022\n\
State:\t{} ({})\n\
Tgid:\t{}\n\
Pid:\t{}\n\
PPid:\t{}\n\
Uid:\t{}\t{}\t{}\t{}\n\
Gid:\t{}\t{}\t{}\t{}\n\
FDSize:\t{}\n\
Threads:\t{}\n\
SigQ:\t0/{}\n\
SigPnd:\t{:016x}\n\
ShdPnd:\t{:016x}\n\
SigBlk:\t{:016x}\n\
SigIgn:\t{:016x}\n\
SigCgt:\t{:016x}\n\
CapInh:\t{:016x}\n\
CapPrm:\t{:016x}\n\
CapEff:\t{:016x}\n\
CapBnd:\t{:016x}\n\
CapAmb:\t{:016x}\n\
NoNewPrivs:\t{}\n\
Cpus_allowed:\t{}\n\
Cpus_allowed_list:\t{}\n\
Mems_allowed:\t00000000,00000001\n\
Mems_allowed_list:\t0\n\
voluntary_ctxt_switches:\t{}\n\
nonvoluntary_ctxt_switches:\t{}\n\
VmSize:\t{:>8} kB\n\
VmLck:\t{:>8} kB\n\
VmRSS:\t{:>8} kB\n\
VmSwap:\t{:>8} kB\n",
                    p.comm, sc, sn, p.tgid, self.pid, ppid,
                    p.uid, p.euid, p.suid, p.euid,
                    p.gid, p.egid, p.sgid, p.egid,
                    fd_count, threads,
                    p.rlimits[11].0,
                    p.pending_signals, p.pending_signals,
                    p.signal_mask, sig_ign, sig_cgt,
                    p.cap_inheritable, p.cap_permitted, p.cap_effective,
                    p.cap_bset, p.cap_ambient,
                    no_new_privs,
                    cpus_allowed_hex, cpus_allowed_list,
                    p.voluntary_ctxt_switches, p.involuntary_ctxt_switches,
                    vm_kb, vm_lck_kb, vm_kb, vm_swap_kb,
                );
            }
        }
        format!("Name:\tunknown\nState:\tX (dead)\nPid:\t{}\nPPid:\t0\n", self.pid)
    }
}

/// /proc/[pid]/stat 生成器 — 单行格式供 busybox ps 使用
pub struct ProcessStatGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessStatGenerator {
    fn generate(&self) -> String {
        let now = crate::arch::asm_helpers::rdtime();
        crate::sched::process::account_exit_kernel(now);
        crate::sched::process::account_enter_kernel(now);
        // utime/stime 消费者：与 sys_times/getrusage 同款，为目标进程启用
        // 逐 syscall 精确分账。非精确进程的 syscall 快路径不结算 cputime 且
        // cpu_in_kernel 长期为 true，syscall 密集型进程的 utime 可能永远为 0
        //（LTP clock_gettime01 setup 对 /proc/self/stat 第 14 列自旋等非零）。
        crate::sched::process_sched::enable_cputime_precise_current(now);
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            if let Some(p) = t.get_mut(self.pid as usize) {
                p.cputime_precise = true;
            }
        }
        if let Some(t) = table.as_ref() {
            if let Some(p) = t.get(self.pid as usize) {
                let state = match p.state {
                    crate::sched::process::ProcessState::Running |
                    crate::sched::process::ProcessState::Runnable => 'R',
                    crate::sched::process::ProcessState::Sleeping => 'S',
                    crate::sched::process::ProcessState::Stopped => 'T',
                    crate::sched::process::ProcessState::Zombie => 'Z',
                    _ => 'S',
                };
                let ppid = p.parent_pid.unwrap_or(0);
                let freq = crate::arch::consts::CLOCK_FREQ as u64;
                let scale = if freq >= 100 { freq / 100 } else { 1 };
                let utime = p.cpu_user_ticks / scale;
                let stime = p.cpu_kernel_ticks / scale;
                let cutime = p.cpu_children_user_ticks / scale;
                let cstime = p.cpu_children_kernel_ticks / scale;
                let starttime = p.start_time_ticks / scale;
                let vm_size_bytes: u64 = p
                    .mm
                    .as_ref()
                    .map(|mm| { let sp = mm.lock(); sp.vmas_iter().map(|v| (v.end.0 - v.start.0) as u64).sum() })
                    .unwrap_or(0);
                let rss_pages = vm_size_bytes / 4096;
                let priority: i32 = match p.sched_policy {
                    1 | 2 => -1 - p.sched_priority,
                    _ => 20 + p.nice,
                };
                let nice = p.nice;
                let num_threads = t
                    .iter()
                    .filter(|q| {
                        q.tgid == p.tgid
                            && !matches!(
                                q.state,
                                crate::sched::process::ProcessState::Zombie
                                    | crate::sched::process::ProcessState::Unused
                            )
                    })
                    .count() as u64;
                let num_threads = if num_threads == 0 { 1 } else { num_threads };
                return format!(
                    "{} ({}) {} {} {} {} 0 0 0 0 0 0 0 \
                     {} {} {} {} \
                     {} {} {} 0 {} \
                     {} {} 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 0 {} {} 0 0 0 0 0 0 0 0 0 0\n",
                    self.pid, p.comm, state, ppid, p.pgid, p.sid,
                    utime, stime, cutime, cstime,
                    priority, nice, num_threads,
                    starttime,
                    vm_size_bytes, rss_pages,
                    p.sched_priority, p.sched_policy,
                );
            }
        }
        format!("{} (unknown) X 0 0 0 0 0 0 0 0 0 0 0 0 0 0 20 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 17 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n", self.pid)
    }
}

/// /proc/[pid]/maps 生成器.
pub struct ProcessMapsGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessMapsGenerator {
    fn generate(&self) -> String {
        use crate::mm::VmaType;
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(tbl) = table.as_ref() else { return String::new() };
        let Some(p) = tbl.get(self.pid as usize) else { return String::new() };
        let Some(ref mm) = p.mm else { return String::new() };
        let space = mm.lock();
        let mut out = String::new();
        for vma in space.vmas_iter() {
            let r = if vma.permission.read { 'r' } else { '-' };
            let w = if vma.permission.write { 'w' } else { '-' };
            let x = if vma.permission.execute { 'x' } else { '-' };
            let pflag = if matches!(vma.vma_type, VmaType::Shared) { 's' } else { 'p' };
            let path: &str = if let Some(path) = vma.file_path.as_deref() {
                path
            } else {
                match vma.vma_type {
                    VmaType::Code => &p.exec_path,
                    VmaType::Heap => "[heap]",
                    VmaType::Stack => "[stack]",
                    VmaType::Data => "",
                    _ => "",
                }
            };
            out.push_str(&format!(
                "{:08x}-{:08x} {}{}{}{} {:08x} 00:00 0 {}\n",
                vma.start.0, vma.end.0, r, w, x, pflag, vma.file_offset, path
            ));
        }
        out
    }
}

/// /proc/[pid]/smaps 生成器.
pub struct ProcessSmapsGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessSmapsGenerator {
    fn generate(&self) -> String {
        use crate::mm::VirtAddr;
        use crate::mm::VmaType;
        let page_sz = crate::arch::consts::PAGE_SIZE;
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(tbl) = table.as_ref() else { return String::new() };
        let Some(p) = tbl.get(self.pid as usize) else { return String::new() };
        let Some(ref mm) = p.mm else { return String::new() };
        let space = mm.lock();
        let mut out = String::new();
        for vma in space.vmas_iter() {
            let r = if vma.permission.read { 'r' } else { '-' };
            let w = if vma.permission.write { 'w' } else { '-' };
            let x = if vma.permission.execute { 'x' } else { '-' };
            let pflag = if matches!(vma.vma_type, VmaType::Shared) { 's' } else { 'p' };
            let path: &str = if let Some(path) = vma.file_path.as_deref() {
                path
            } else {
                match vma.vma_type {
                    VmaType::Code => &p.exec_path,
                    VmaType::Heap => "[heap]",
                    VmaType::Stack => "[stack]",
                    _ => "",
                }
            };
            // 直接数 frames 集合（与 cgroup memory.current 同口径），避免对
            // 大 VMA 逐页做三级页表遍历（smaps 被高频读时是显著热点）。
            let rss_pages: usize = vma.resident_pages();
            let size_kb = (vma.end.0 - vma.start.0) / 1024;
            let rss_kb = rss_pages * page_sz / 1024;
            let anon_kb = if vma.file_backed { 0 } else { rss_kb };
            let locked_kb = if vma.locked { rss_kb } else { 0 };
            out.push_str(&format!(
                "{:08x}-{:08x} {}{}{}{} {:08x} 00:00 0 {}\n",
                vma.start.0, vma.end.0, r, w, x, pflag, vma.file_offset, path
            ));
            out.push_str(&format!("Size:           {:8} kB\n", size_kb));
            out.push_str("KernelPageSize:        4 kB\n");
            out.push_str("MMUPageSize:           4 kB\n");
            out.push_str(&format!("Rss:            {:8} kB\n", rss_kb));
            out.push_str(&format!("Pss:            {:8} kB\n", rss_kb));
            out.push_str("Shared_Clean:          0 kB\n");
            out.push_str("Shared_Dirty:          0 kB\n");
            out.push_str(&format!("Private_Clean:  {:8} kB\n", 0));
            out.push_str(&format!("Private_Dirty:  {:8} kB\n", rss_kb));
            out.push_str(&format!("Referenced:     {:8} kB\n", rss_kb));
            out.push_str(&format!("Anonymous:      {:8} kB\n", anon_kb));
            out.push_str("LazyFree:              0 kB\n");
            out.push_str("AnonHugePages:         0 kB\n");
            out.push_str("ShmemPmdMapped:        0 kB\n");
            out.push_str("Shared_Hugetlb:        0 kB\n");
            out.push_str("Private_Hugetlb:       0 kB\n");
            out.push_str("Swap:                  0 kB\n");
            out.push_str("SwapPss:               0 kB\n");
            out.push_str(&format!("Locked:         {:8} kB\n", locked_kb));
            out.push_str("THPeligible:           0\n");
            let mut flags = String::new();
            if vma.permission.read { flags.push_str("rd "); }
            if vma.permission.write { flags.push_str("wr "); }
            if vma.permission.execute { flags.push_str("ex "); }
            if matches!(vma.vma_type, VmaType::Shared) { flags.push_str("sh "); }
            flags.push_str("mr mw me ");
            if vma.locked { flags.push_str("lo "); }
            flags.push_str("ac");
            out.push_str(&format!("VmFlags: {}\n", flags));
        }
        out
    }
}

/// /proc/[pid]/pagemap 生成器.
pub struct ProcessPagemapGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessPagemapGenerator {
    fn generate(&self) -> String {
        String::new()
    }

    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Option<usize> {
        use crate::mm::VirtAddr;
        const ENTRY: usize = 8;
        let page_sz = crate::arch::consts::PAGE_SIZE;
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let tbl = table.as_ref()?;
        let p = tbl.get(self.pid as usize)?;
        let mm = p.mm.as_ref()?;
        let space = mm.lock();

        let want = buf.len();
        let start_vpn = offset / ENTRY;
        let end_vpn = (offset + want + ENTRY - 1) / ENTRY;
        let mut written = 0usize;
        for vpn in start_vpn..end_vpn {
            let entry: u64 = match space.page_pfn(VirtAddr(vpn * page_sz)) {
                Some(pfn) => (1u64 << 63) | ((pfn as u64) & 0x007f_ffff_ffff_ffff),
                None => 0,
            };
            let bytes = entry.to_le_bytes();
            let entry_base = vpn * ENTRY;
            for (i, b) in bytes.iter().enumerate() {
                let pos = entry_base + i;
                if pos < offset { continue; }
                let dst = pos - offset;
                if dst >= want { break; }
                buf[dst] = *b;
                if dst + 1 > written { written = dst + 1; }
            }
        }
        Some(written)
    }
}

/// /proc/kpageflags 生成器（全局，按 PFN 索引，每帧 8 字节）。
///
/// Linux 把 struct page 的标志位导出于此；本内核只实现 msync04 依赖的
/// `KPF_DIRTY`（bit 4）：从读取者（当前进程）地址空间反查该 PFN 的 PTE 脏位。
/// 其余位返回 0。offset = pfn*8。
pub struct KpageflagsGenerator;

impl ProcFileGenerator for KpageflagsGenerator {
    fn generate(&self) -> String {
        String::new()
    }

    fn read_at(&self, offset: usize, buf: &mut [u8]) -> Option<usize> {
        const ENTRY: usize = 8;
        const KPF_DIRTY: u64 = 1 << 4;
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let tbl = table.as_ref()?;
        let cur = tbl.current?;
        let as_pid = tbl
            .get(cur)
            .map(|p| if p.mm.is_some() { cur } else { p.tgid as usize })
            .unwrap_or(cur);
        let p = tbl.get(as_pid)?;
        let mm = p.mm.as_ref()?;
        let space = mm.lock();

        let want = buf.len();
        let start_pfn = offset / ENTRY;
        let end_pfn = (offset + want + ENTRY - 1) / ENTRY;
        let mut written = 0usize;
        for pfn in start_pfn..end_pfn {
            let mut flags: u64 = 0;
            if space.pfn_dirty(pfn) == Some(true) {
                flags |= KPF_DIRTY;
            }
            let bytes = flags.to_le_bytes();
            let entry_base = pfn * ENTRY;
            for (i, b) in bytes.iter().enumerate() {
                let pos = entry_base + i;
                if pos < offset { continue; }
                let dst = pos - offset;
                if dst >= want { break; }
                buf[dst] = *b;
                if dst + 1 > written { written = dst + 1; }
            }
        }
        Some(written)
    }
}

pub struct ProcessCmdlineGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessCmdlineGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::new() };
        let Some(p) = t.get(self.pid as usize) else { return String::new() };
        if p.saved_argv.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for arg in p.saved_argv.iter() {
            out.push_str(arg);
            out.push('\0');
        }
        out
    }
}

pub struct ProcessCommGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessCommGenerator {
    fn generate(&self) -> String {
        const TASK_COMM_LEN_USER: usize = 15;
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::from("\n") };
        let Some(p) = t.get(self.pid as usize) else { return String::from("\n") };
        let truncated: String = p.comm.chars().take(TASK_COMM_LEN_USER).collect();
        let mut out = truncated;
        out.push('\n');
        out
    }
}

pub struct ProcessIoGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessIoGenerator {
    fn generate(&self) -> String {
        // 出现 /proc/[pid]/io 读者 ⇒ 开启逐 syscall I/O 记账（此前 lmbench/
        // libcbench/iozone 等无读者场景走 lock-free 快路径）。
        crate::sched::process_sched::mark_io_account_active();
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::new() };
        let Some(p) = t.get(self.pid as usize) else { return String::new() };
        format!(
            "rchar: {}\nwchar: {}\nsyscr: {}\nsyscw: {}\nread_bytes: {}\nwrite_bytes: {}\ncancelled_write_bytes: 0\n",
            p.io_rchar, p.io_wchar, p.io_syscr, p.io_syscw, p.io_read_bytes, p.io_write_bytes,
        )
    }
}

pub struct ProcessLimitsGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessLimitsGenerator {
    fn generate(&self) -> String {
        const RLIM_NAMES: [(&str, &str); 16] = [
            ("Max cpu time", "seconds"),
            ("Max file size", "bytes"),
            ("Max data size", "bytes"),
            ("Max stack size", "bytes"),
            ("Max core file size", "bytes"),
            ("Max resident set", "bytes"),
            ("Max processes", "processes"),
            ("Max open files", "files"),
            ("Max locked memory", "bytes"),
            ("Max address space", "bytes"),
            ("Max file locks", "locks"),
            ("Max pending signals", "signals"),
            ("Max msgqueue size", "bytes"),
            ("Max nice priority", ""),
            ("Max realtime priority", ""),
            ("Max realtime timeout", "us"),
        ];
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::new() };
        let Some(p) = t.get(self.pid as usize) else { return String::new() };
        let mut out = String::new();
        out.push_str("Limit                     Soft Limit           Hard Limit           Units\n");
        for (idx, (name, unit)) in RLIM_NAMES.iter().enumerate() {
            let (soft, hard) = p.rlimits[idx];
            let soft_s = if soft == u64::MAX { String::from("unlimited") } else { format!("{}", soft) };
            let hard_s = if hard == u64::MAX { String::from("unlimited") } else { format!("{}", hard) };
            out.push_str(&format!(
                "{:<25} {:<20} {:<20} {}\n",
                name, soft_s, hard_s, unit
            ));
        }
        out
    }
}

pub struct ProcessWchanGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessWchanGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::from("0") };
        let Some(p) = t.get(self.pid as usize) else { return String::from("0") };
        match p.wait_channel {
            Some(name) => String::from(name),
            None => String::from("0"),
        }
    }
}

pub struct ProcessLoginuidGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessLoginuidGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::from("4294967295") };
        let Some(p) = t.get(self.pid as usize) else { return String::from("4294967295") };
        format!("{}", p.loginuid)
    }
}

pub struct ProcessOomScoreGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessOomScoreGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::from("0\n") };
        let Some(p) = t.get(self.pid as usize) else { return String::from("0\n") };
        let vm_size_bytes: u64 = p
            .mm
            .as_ref()
            .map(|mm| { let sp = mm.lock(); sp.vmas_iter().map(|v| (v.end.0 - v.start.0) as u64).sum() })
            .unwrap_or(0);
        let pages = vm_size_bytes / 4096;
        let base = (pages * 1000) / 16384;
        let raw = base as i64 + p.oom_score_adj as i64;
        let score = raw.clamp(0, 1000);
        format!("{}\n", score)
    }
}

pub struct ProcessOomScoreAdjGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessOomScoreAdjGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::from("0\n") };
        let Some(p) = t.get(self.pid as usize) else { return String::from("0\n") };
        format!("{}\n", p.oom_score_adj)
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v: i32 = data.trim().parse().map_err(|_| ProcError::InvalidArgument)?;
        if !(-1000..=1000).contains(&v) {
            return Err(ProcError::InvalidArgument);
        }
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_mut() else { return Err(ProcError::NotFound) };
        let Some(p) = t.get_mut(self.pid as usize) else { return Err(ProcError::NotFound) };
        if p.euid != 0 && v < p.oom_score_adj {
            return Err(ProcError::PermissionDenied);
        }
        p.oom_score_adj = v;
        Ok(())
    }
}

/// `/proc/[pid]/coredump_filter` — bitmask of which memory types are written
/// to a core dump. Linux default `0x33` (bit0 anon-private, bit1 anon-shared,
/// bit4 ELF headers, bit5 hugetlb-private). madvise08 reads it and requires
/// bit0 (anonymous private memory dumpable).
pub struct ProcessCoredumpFilterGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessCoredumpFilterGenerator {
    fn generate(&self) -> String {
        String::from("00000033\n")
    }
}

pub struct ProcessPersonalityGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessPersonalityGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::from("00000000\n") };
        let Some(p) = t.get(self.pid as usize) else { return String::from("00000000\n") };
        format!("{:08x}\n", p.personality)
    }
}

pub struct ProcessSessionidGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessSessionidGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::from("4294967295\n") };
        let Some(p) = t.get(self.pid as usize) else { return String::from("4294967295\n") };
        format!("{}\n", p.audit_sessionid)
    }
}

pub struct ProcessTimerslackNsGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessTimerslackNsGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::from("50000\n") };
        let Some(p) = t.get(self.pid as usize) else { return String::from("50000\n") };
        format!("{}\n", p.timerslack_ns)
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let v: usize = data.trim().parse().map_err(|_| ProcError::InvalidArgument)?;
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_mut() else { return Err(ProcError::NotFound) };
        let Some(p) = t.get_mut(self.pid as usize) else { return Err(ProcError::NotFound) };
        p.timerslack_ns = if v == 0 { p.default_timerslack_ns } else { v };
        Ok(())
    }
}

pub struct ProcessAuxvGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessAuxvGenerator {
    fn generate(&self) -> String {
        String::new()
    }
    fn generate_bytes(&self) -> Vec<u8> {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return Vec::new() };
        let Some(p) = t.get(self.pid as usize) else { return Vec::new() };
        let mut out = Vec::with_capacity(p.saved_auxv.len() * 16);
        for &(typ, val) in p.saved_auxv.iter() {
            out.extend_from_slice(&typ.to_le_bytes());
            out.extend_from_slice(&val.to_le_bytes());
        }
        out
    }
}

pub struct ProcessEnvironGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessEnvironGenerator {
    fn generate(&self) -> String {
        String::new()
    }
    fn generate_bytes(&self) -> Vec<u8> {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return Vec::new() };
        let Some(p) = t.get(self.pid as usize) else { return Vec::new() };
        let mut out = Vec::new();
        for env in p.saved_envp.iter() {
            out.extend_from_slice(env.as_bytes());
            out.push(0);
        }
        out
    }
}

pub struct ProcessSyscallGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessSyscallGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::from("running\n") };
        let Some(p) = t.get(self.pid as usize) else { return String::from("running\n") };
        let (nr, args) = p.last_syscall;
        if nr == u64::MAX {
            return String::from("running\n");
        }
        format!(
            "{} 0x{:x} 0x{:x} 0x{:x} 0x{:x} 0x{:x} 0x{:x} 0x{:x} 0x{:x}\n",
            nr,
            args[0], args[1], args[2], args[3], args[4], args[5],
            p.user_stack_top.0,
            p.entry_point.0,
        )
    }
}

pub struct ProcessExeGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for ProcessExeGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else { return String::new() };
        let Some(p) = t.get(self.pid as usize) else { return String::new() };
        if p.exec_path.is_empty() {
            return String::new();
        }
        p.exec_path.clone()
    }
}

pub struct NsFileGenerator {
    pub name: String,
    pub pid: u32,
}

impl ProcFileGenerator for NsFileGenerator {
    fn generate(&self) -> String {
        // 真实化：从进程 NsProxy 读取实际 namespace inode 号
        let ns_ino = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table.as_ref().and_then(|t| {
                t.get(self.pid as usize).map(|p| {
                    let ns = p.nsproxy.lock();
                    match self.name.as_str() {
                        "cgroup" => ns.cgroup_ns.id.0,
                        "ipc" => ns.ipc_ns.id.0,
                        "mnt" => ns.mnt_ns.id.0,
                        "net" => ns.net_ns.id.0,
                        "pid" | "pid_for_children" => ns.pid_ns.id.0,
                        "time" | "time_for_children" => ns.time_ns.id.0,
                        "user" => ns.user_ns.id.0,
                        "uts" => ns.uts_ns.id.0,
                        _ => 0,
                    }
                })
            }).unwrap_or(0)
        };
        format!("{}:[{}]\n", self.name, ns_ino)
    }
}

pub struct SetgroupsGenerator {
    pub pid: u32,
}

impl ProcFileGenerator for SetgroupsGenerator {
    fn generate(&self) -> String {
        crate::kernel::namespace::read_setgroups(self.pid as usize)
    }
    fn writable(&self) -> bool {
        true
    }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        match crate::kernel::namespace::write_setgroups(self.pid as usize, data) {
            Ok(()) => Ok(()),
            // EPERM: "allow" after "deny" is irreversible (userns03).
            Err(true) => Err(ProcError::OperationNotPermitted),
            Err(false) => Err(ProcError::InvalidArgument),
        }
    }
}

pub struct IdMapGenerator {
    pub kind: &'static str,
    pub pid: u32,
}

impl ProcFileGenerator for IdMapGenerator {
    fn generate(&self) -> String {
        crate::kernel::namespace::read_id_map(self.pid as usize, self.kind)
    }
    fn writable(&self) -> bool {
        true
    }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        if crate::kernel::namespace::write_id_map(self.pid as usize, self.kind, data) {
            Ok(())
        } else {
            Err(ProcError::InvalidArgument)
        }
    }
}

pub struct TimensOffsetsGenerator;

impl ProcFileGenerator for TimensOffsetsGenerator {
    fn generate(&self) -> String {
        let (mono, boot) =
            crate::kernel::namespace::current_timens_for_children_offsets();
        let (m_sec, m_nsec) = split_offset(mono);
        let (b_sec, b_nsec) = split_offset(boot);
        alloc::format!(
            "monotonic       {:>11} {:>10}\nboottime        {:>11} {:>10}\n",
            m_sec, m_nsec, b_sec, b_nsec
        )
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        // Format: one or more lines, each "<clk> <sec> <nsec>" where <clk> is
        // either the textual name ("monotonic"/"boottime") or its POSIX id
        // (1 = CLOCK_MONOTONIC, 7 = CLOCK_BOOTTIME). Linux parses every line
        // strictly (proc_timens_set_offset): all three fields are mandatory,
        // nsec must be in [0, 1e9), only MONOTONIC/BOOTTIME are accepted, and
        // the whole buffer is validated before *any* offset is applied — a
        // malformed line anywhere fails the entire write with EINVAL (timens01).
        // Offsets land in the caller's time_ns_for_children; the caller stays
        // in its current time namespace and only forked children observe them.
        let mut pending: alloc::vec::Vec<(usize, i64)> = alloc::vec::Vec::new();
        for line in data.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let mut it = line.split_whitespace();
            let clk_tok = it.next().ok_or(ProcError::InvalidArgument)?;
            let sec_tok = it.next().ok_or(ProcError::InvalidArgument)?;
            let nsec_tok = it.next().ok_or(ProcError::InvalidArgument)?;
            // No trailing garbage permitted on the line.
            if it.next().is_some() {
                return Err(ProcError::InvalidArgument);
            }
            let clock_id: i64 = match clk_tok {
                "monotonic" => 1,
                "boottime" => 7,
                s => s.parse().map_err(|_| ProcError::InvalidArgument)?,
            };
            // Only CLOCK_MONOTONIC / CLOCK_BOOTTIME may be offset.
            if clock_id != 1 && clock_id != 7 {
                return Err(ProcError::InvalidArgument);
            }
            let sec: i64 = sec_tok.parse().map_err(|_| ProcError::InvalidArgument)?;
            let nsec: i64 = nsec_tok.parse().map_err(|_| ProcError::InvalidArgument)?;
            if nsec < 0 || nsec >= 1_000_000_000 {
                return Err(ProcError::InvalidArgument);
            }
            // Linux rejects offsets whose seconds reach KTIME_SEC_MAX
            // (= KTIME_MAX / NSEC_PER_SEC), i.e. a 64-bit ns timer overflow,
            // with ERANGE (not EINVAL) even when sec*1e9 still fits i64
            // (timens01 "Overflow kernel 64bit ns timer" cases).
            const KTIME_SEC_MAX: i64 = i64::MAX / 1_000_000_000;
            if sec >= KTIME_SEC_MAX || sec <= -KTIME_SEC_MAX {
                return Err(ProcError::OutOfRange);
            }
            let offset_ns = sec
                .checked_mul(1_000_000_000)
                .and_then(|v| v.checked_add(nsec))
                .ok_or(ProcError::InvalidArgument)?;
            pending.push((clock_id as usize, offset_ns));
        }
        if pending.is_empty() {
            return Err(ProcError::InvalidArgument);
        }
        for (clock_id, offset_ns) in pending {
            if !crate::kernel::namespace::set_current_timens_for_children_offset(clock_id, offset_ns) {
                return Err(ProcError::InvalidArgument);
            }
        }
        Ok(())
    }
}

/// Split a signed nanosecond offset into (seconds, nanoseconds) where the
/// nanosecond part is non-negative, matching the kernel's timens_offsets format.
fn split_offset(offset_ns: i64) -> (i64, i64) {
    let mut sec = offset_ns / 1_000_000_000;
    let mut nsec = offset_ns % 1_000_000_000;
    if nsec < 0 {
        nsec += 1_000_000_000;
        sec -= 1;
    }
    (sec, nsec)
}

/// /proc/[pid]/fdinfo/[fd] generator — Linux fdinfo format.
pub struct FdInfoGenerator {
    pub pid: u32,
    pub fd: u32,
}

impl ProcFileGenerator for FdInfoGenerator {
    fn generate(&self) -> String {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return String::new() };
        let process = match t.get_fd_process(self.pid as usize) { Some(p) => p, None => return String::new() };
        let fd_obj = match process.fd_table.get(self.fd as usize).and_then(|o| o.as_ref()) {
            Some(f) => f.clone(),
            None => return String::new(),
        };
        let flags = fd_obj.flags;
        let offset = fd_obj.offset.load(core::sync::atomic::Ordering::Relaxed);
        let inotify_watches = fd_obj.file.inotify_fdinfo();
        let fanotify_marks = fd_obj.file.fanotify_fdinfo();
        let fd_path = fd_obj.path.clone();
        drop(table);
        let mut s = String::new();
        s.push_str(&format!("pos:\t{}\nflags:\t{:o}\nmnt_id:\t0\n", offset, flags));
        if let Some(watches) = inotify_watches {
            for (wd, mask) in watches {
                s.push_str(&format!(
                    "inotify wd:{:x} ino:0 sdev:0 mask:{:x} ignored_mask:0\n",
                    wd, mask,
                ));
            }
        }
        if let Some(marks) = fanotify_marks {
            // Group-level header (Linux: fanotify flags / event-flags).
            s.push_str("fanotify flags:0 event-flags:0\n");
            for (mask, ignored_mask, mflags) in marks {
                s.push_str(&format!(
                    "fanotify ino:0 sdev:0 mflags: {:x} mask:{:x} ignored_mask:{:x}\n",
                    mflags, mask, ignored_mask,
                ));
            }
        }
        // pidfd fdinfo: Linux reports the target Pid: and NSpid: lines so that
        // tools (and LTP fanotify21) can resolve the process behind a pidfd.
        if let Some(path) = fd_path.as_ref() {
            if let Some(rest) = path.strip_prefix("pidfd:") {
                let pid_str = rest.split(':').next().unwrap_or(rest);
                if let Ok(pid) = pid_str.parse::<i64>() {
                    s.push_str(&format!("Pid:\t{}\nNSpid:\t{}\n", pid, pid));
                }
            }
        }
        s
    }
}
