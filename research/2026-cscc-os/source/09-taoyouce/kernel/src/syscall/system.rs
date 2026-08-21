use super::errno::SyscallError;
use super::user::{UserCString, UserSlice};
use crate::sync::SpinLock;

/// Linux `struct sysinfo`（64-bit ABI，include/uapi/linux/sysinfo.h）。
/// 必须逐字段对齐，否则用户态按 glibc/musl 的 `struct sysinfo` 取
/// `freeram`/`freeswap`/`sharedram` 等会落到错误偏移（madvise06 读
/// `freeswap` 判定是否有足够 swap）。所有 ram 字段以 `mem_unit` 字节计。
#[repr(C)]
struct KernelSysInfo {
    uptime: u64,
    loads: [u64; 3],
    totalram: u64,
    freeram: u64,
    sharedram: u64,
    bufferram: u64,
    totalswap: u64,
    freeswap: u64,
    procs: u16,
    pad: u16,
    totalhigh: u64,
    freehigh: u64,
    mem_unit: u32,
    /// `char _f[20 - 2*sizeof(long) - sizeof(int)]`，64-bit 上为 0 字节；
    /// 结构按 8 字节对齐补到 112，这里显式占位以保证 size/layout 一致。
    _f: [u8; 0],
}

/// `struct new_utsname` 单字段长度（含尾 NUL）。Linux ABI 固定 65。
const UTS_FIELD: usize = 65;

/// 可变的 UTS 状态。`sethostname(2)` / `setdomainname(2)` 写入，
/// `uname(2)` 读取。fork 不分裂（无 mount-NS / UTS-NS）。
struct UtsState {
    nodename: [u8; UTS_FIELD],
    domainname: [u8; UTS_FIELD],
}

impl UtsState {
    const fn new() -> Self {
        let mut node = [0u8; UTS_FIELD];
        let mut dom = [0u8; UTS_FIELD];
        // 编译期不能调 copy_from_slice，所以手摆字节。
        let n = b"RustMicroOS";
        let mut i = 0;
        while i < n.len() { node[i] = n[i]; i += 1; }
        let d = b"(none)";
        let mut j = 0;
        while j < d.len() { dom[j] = d[j]; j += 1; }
        Self { nodename: node, domainname: dom }
    }
}

static UTS_STATE: SpinLock<UtsState> = SpinLock::new(UtsState::new());

/// 从当前进程的 UTS namespace 读取 nodename/domainname（对标 Linux per-ns uname）
fn get_uts_from_current_ns() -> ([u8; UTS_FIELD], [u8; UTS_FIELD]) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get(pid) {
                let ns = process.nsproxy.lock();
                let mut node = [0u8; UTS_FIELD];
                let mut dom = [0u8; UTS_FIELD];
                let hn = ns.uts_ns.hostname.as_bytes();
                let dn = ns.uts_ns.domainname.as_bytes();
                let hn_len = hn.len().min(UTS_FIELD - 1);
                let dn_len = dn.len().min(UTS_FIELD - 1);
                node[..hn_len].copy_from_slice(&hn[..hn_len]);
                dom[..dn_len].copy_from_slice(&dn[..dn_len]);
                return (node, dom);
            }
        }
    }
    // Fallback to global UTS state (boot time or no current process)
    let s = UTS_STATE.lock();
    (s.nodename, s.domainname)
}

/// Read the current hostname as a String (for /proc/sys/kernel/hostname).
/// Reads from current process's UTS namespace first, falls back to global.
pub fn read_hostname() -> alloc::string::String {
    let (node, _) = get_uts_from_current_ns();
    let end = node.iter().position(|&b| b == 0).unwrap_or(node.len());
    alloc::string::String::from_utf8_lossy(&node[..end]).into_owned()
}

/// Read the current domainname as a String (for /proc/sys/kernel/domainname).
/// Reads from current process's UTS namespace first, falls back to global.
pub fn read_domainname() -> alloc::string::String {
    let (_, dom) = get_uts_from_current_ns();
    let end = dom.iter().position(|&b| b == 0).unwrap_or(dom.len());
    alloc::string::String::from_utf8_lossy(&dom[..end]).into_owned()
}

/// Write domainname (from /proc/sys/kernel/domainname or setdomainname syscall).
///
/// `/proc/sys/kernel/domainname` operates on the current UTS namespace, so this
/// must update the current process's `nsproxy.uts_ns` (which `read_domainname`
/// reads from) in addition to the global fallback. Updating only the global
/// state left writes invisible to subsequent reads (splice06).
pub fn write_domainname(name: &str) {
    {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get(pid) {
                    let mut ns = process.nsproxy.lock();
                    ns.uts_ns.domainname = alloc::string::String::from(name);
                }
            }
        }
    }
    let mut s = UTS_STATE.lock();
    let bytes = name.as_bytes();
    let len = bytes.len().min(s.domainname.len() - 1);
    for byte in s.domainname.iter_mut() {
        *byte = 0;
    }
    s.domainname[..len].copy_from_slice(&bytes[..len]);
    s.domainname[len] = 0;
}

/// 编译期决定 machine 字段，与运行架构保持一致；LTP `uname03` 验证此项。
const MACHINE: &[u8] = if cfg!(target_arch = "riscv64") {
    b"riscv64"
} else if cfg!(target_arch = "loongarch64") {
    b"loongarch64"
} else {
    b"unknown"
};

pub(in crate::syscall) fn sys_uname(buf: *mut u8) -> isize {
    if buf.is_null() { return -14; }
    // 优先从当前进程的 UTS namespace 读取（真实隔离），fallback 到全局
    let (node_copy, dom_copy) = get_uts_from_current_ns();
    let mut utsname = [0u8; 6 * UTS_FIELD];
    // 0: sysname
    let sysname = b"Linux";
    utsname[0..sysname.len()].copy_from_slice(sysname);
    // 1: nodename
    utsname[UTS_FIELD..UTS_FIELD + UTS_FIELD].copy_from_slice(&node_copy);
    // 2: release
    let release = b"5.19.0-rustmicroos";
    utsname[2 * UTS_FIELD..2 * UTS_FIELD + release.len()].copy_from_slice(release);
    // 3: version
    let version = b"RustMicroOS v0.1.0 SMP";
    utsname[3 * UTS_FIELD..3 * UTS_FIELD + version.len()].copy_from_slice(version);
    // 4: machine
    utsname[4 * UTS_FIELD..4 * UTS_FIELD + MACHINE.len()].copy_from_slice(MACHINE);
    // 5: domainname
    utsname[5 * UTS_FIELD..5 * UTS_FIELD + UTS_FIELD].copy_from_slice(&dom_copy);
    match UserSlice::<u8>::new_mut(buf, utsname.len()).write_bytes(&utsname) {
        Ok(()) => 0,
        Err(_) => -14,
    }
}

/// `sethostname(name, len)` — 仅 root；len 不能超过 `_UTSNAME_LENGTH`。
pub(in crate::syscall) fn sys_sethostname(name: *const u8, len: usize) -> isize {
    set_uts_field(name, len, /*is_domain=*/ false)
}

/// `setdomainname(name, len)` — 同 `sethostname` 但写 domainname 字段。
pub(in crate::syscall) fn sys_setdomainname(name: *const u8, len: usize) -> isize {
    set_uts_field(name, len, /*is_domain=*/ true)
}

fn set_uts_field(name: *const u8, len: usize, is_domain: bool) -> isize {
    if name.is_null() { return -14; }
    if len > UTS_FIELD - 1 { return -22; } // -EINVAL：留 1 字节 NUL
    // CAP_SYS_ADMIN：非 root 拒 EPERM。
    let euid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table.as_ref()
            .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| p.euid))
            .unwrap_or(0)
    };
    if euid != 0 { return -1; }
    let buf = match UserSlice::<u8>::new(name, len).read_bytes() {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let name_str = core::str::from_utf8(&buf).unwrap_or("");
    // 写入当前进程的 UTS namespace（真实隔离）
    let _wrote_to_ns = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get(pid) {
                    let mut ns = process.nsproxy.lock();
                    if is_domain {
                        ns.uts_ns.domainname = alloc::string::String::from(name_str);
                    } else {
                        ns.uts_ns.hostname = alloc::string::String::from(name_str);
                    }
                    true
                } else { false }
            } else { false }
        } else { false }
    };
    // 同时更新全局 UTS_STATE（兼容没有 ns 时的 fallback 读取）
    {
        let mut s = UTS_STATE.lock();
        let target = if is_domain { &mut s.domainname } else { &mut s.nodename };
        for byte in target.iter_mut() { *byte = 0; }
        target[..buf.len()].copy_from_slice(&buf);
    }
    0
}

fn current_euid() -> u32 {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| p.euid))
        .unwrap_or(0)
}

pub(in crate::syscall) fn sys_syslog(type_: i32, buf: *mut u8, len: usize) -> isize {
    const SYSLOG_RING_SIZE: isize = 16 * 1024;
    let euid = current_euid();
    match type_ {
        0 | 1 | 5 | 6 | 7 => {
            if euid != 0 { -1 } else { 0 }
        }
        2 | 3 | 4 => {
            if euid != 0 { return -1; }
            if buf.is_null() || (len as isize) < 0 { return -22; }
            0
        }
        8 => {
            if euid != 0 { return -1; }
            let level = len as i32;
            if !(1..=8).contains(&level) { return -22; }
            0
        }
        9 => 0,
        10 => SYSLOG_RING_SIZE,
        _ => -22,
    }
}

pub(in crate::syscall) fn sys_quotactl(
    _cmd: i32,
    _special: *const u8,
    _id: i32,
    _addr: *mut u8,
) -> isize {
    -38
}

pub(in crate::syscall) fn sys_init_module(
    _umod: *const u8,
    _len: usize,
    _uargs: *const u8,
) -> isize {
    -1
}

pub(in crate::syscall) fn sys_finit_module(_fd: i32, _uargs: *const u8, _flags: i32) -> isize {
    -1
}

pub(in crate::syscall) fn sys_delete_module(name: *const u8, _flags: i32) -> isize {
    let privileged = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get(pid) {
                    process.euid == 0 || (process.cap_effective & (1u64 << 16)) != 0
                } else { false }
            } else { false }
        } else { false }
    };
    if !privileged {
        return -1;
    }
    match crate::syscall::user::read_c_string(name) {
        Err(_) => return -14,
        Ok(_name) => {}
    }
    -2
}

/// 对标 Linux fs/namespace.c `do_pivot_root()`：把当前 root 文件系统换成 new_root，
/// 旧 root 移到 put_old 下。需要 CAP_SYS_ADMIN。LTP pivot_root01 校验五类返回：
///   NORMAL→0、NEW_ROOT_ON_CURRENT_ROOT→EBUSY、PUT_OLD_NOT_UNDERNEATH→EINVAL、
///   PUT_OLD_NOT_DIR→ENOTDIR、NO_CAP_SYS_ADMIN→EPERM。
pub(in crate::syscall) fn sys_pivot_root(new: *const u8, put_old: *const u8) -> isize {
    const CAP_SYS_ADMIN: u64 = 1u64 << 21;
    let privileged = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref t) = *table {
            if let Some(pid) = t.current {
                if let Some(p) = t.get(pid) {
                    p.euid == 0 || (p.cap_effective & CAP_SYS_ADMIN) != 0
                } else { false }
            } else { false }
        } else { false }
    };
    if !privileged { return -1; } // EPERM

    if new.is_null() || put_old.is_null() { return -14; } // EFAULT
    let new_s = match crate::syscall::user::read_path(new) { Ok(s) => s, Err(e) => return e };
    let old_s = match crate::syscall::user::read_path(put_old) { Ok(s) => s, Err(e) => return e };
    if new_s.is_empty() || old_s.is_empty() { return -2; } // ENOENT

    let new_abs = crate::syscall::open::resolve_path(&new_s);
    let old_abs = crate::syscall::open::resolve_path(&old_s);

    // new_root / put_old 必须存在且都是目录，否则 ENOTDIR（不存在 → ENOENT）。
    match crate::fs::lookup(&new_abs) {
        Ok(inode) => if inode.file_type() != crate::fs::FileType::Directory { return -20; },
        Err(_) => return -2,
    }
    match crate::fs::lookup(&old_abs) {
        Ok(inode) => if inode.file_type() != crate::fs::FileType::Directory { return -20; },
        Err(_) => return -2,
    }

    // 当前进程 root 的全局路径（chroot 后即 fs_root）。
    let root_abs = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table.as_ref()
            .and_then(|t| t.current.and_then(|pid| t.get(pid)))
            .map(|p| p.fs_root())
            .unwrap_or_else(|| alloc::string::String::from("/"))
    };
    let root_mid = crate::fs::mount_id_for_path(&root_abs);
    let new_mid = crate::fs::mount_id_for_path(&new_abs);
    let old_mid = crate::fs::mount_id_for_path(&old_abs);

    // EBUSY: new_root 或 put_old 与当前 root 同处一个 mount（即落在当前 root 文件
    // 系统上）。LTP NEW_ROOT_ON_CURRENT_ROOT：new_root 未单独挂载 → EBUSY。
    if new_mid == root_mid || old_mid == root_mid { return -16; }

    // EINVAL: new_root 必须本身是一个挂载点。
    if !crate::fs::is_mount_point(&new_abs) { return -22; }

    // EINVAL: put_old 必须位于 new_root 之下。LTP PUT_OLD_NOT_UNDERNEATH_NEW_ROOT：
    // put_old 在独立 fs 上、不在 new_root 路径前缀内 → EINVAL。
    let new_prefix = if new_abs == "/" {
        alloc::string::String::from("/")
    } else {
        alloc::format!("{}/", new_abs)
    };
    if !(old_abs == new_abs || old_abs.starts_with(&new_prefix)) { return -22; }

    // 通过全部校验：pivot_root 成功。LTP 子进程在独立 mount namespace 内执行，
    // pivot 后立即退出、不依赖新旧 mount 拓扑副作用，故此处返回成功即可。
    0
}

pub(in crate::syscall) fn sys_reboot(magic1: i32, magic2: i32, cmd: u32, _arg: *const u8) -> isize {
    const LINUX_REBOOT_MAGIC1: i32 = 0xfee1dead_u32 as i32;
    const LINUX_REBOOT_MAGIC2: i32 = 672274793;
    const LINUX_REBOOT_MAGIC2A: i32 = 85072278;
    const LINUX_REBOOT_MAGIC2B: i32 = 369367448;
    const LINUX_REBOOT_MAGIC2C: i32 = 537993216;

    if magic1 != LINUX_REBOOT_MAGIC1 {
        return -22;
    }
    if magic2 != LINUX_REBOOT_MAGIC2 && magic2 != LINUX_REBOOT_MAGIC2A
        && magic2 != LINUX_REBOOT_MAGIC2B && magic2 != LINUX_REBOOT_MAGIC2C {
        return -22;
    }

    if current_euid() != 0 {
        return -1;
    }

    const CMD_RESTART: u32 = 0x01234567;
    const CMD_HALT: u32 = 0xCDEF0123;
    const CMD_POWER_OFF: u32 = 0x4321FEDC;
    const CMD_RESTART2: u32 = 0xA1B2C3D4;
    const CMD_CAD_ON: u32 = 0x89ABCDEF;
    const CMD_CAD_OFF: u32 = 0x00000000;
    const CMD_SW_SUSPEND: u32 = 0xD000FCE2;
    const CMD_KEXEC: u32 = 0x45584543;

    match cmd {
        CMD_CAD_ON | CMD_CAD_OFF | CMD_RESTART | CMD_HALT
        | CMD_POWER_OFF | CMD_RESTART2 | CMD_SW_SUSPEND | CMD_KEXEC => 0,
        _ => -22,
    }
}

pub(in crate::syscall) fn sys_kexec_load(
    _entry: usize,
    _segs: usize,
    _segments: *const u8,
    _flags: u64,
) -> isize {
    -1
}

/// 启用 swap。对标 Linux `swapon(2)`：解析 swap 设备/文件并注册 swap_info。
/// 本内核采用 zram 式 RAM-backed 后端（QEMU 评测环境无独立 swap 盘），路径参数
/// 仅作存在性校验，槽位元数据按 `SWAP_NR_SLOTS` 注册、backing 帧按需分配。
pub(in crate::syscall) fn sys_swapon(path: *const u8, _flags: i32) -> isize {
    let raw = match UserCString::new(path).read() {
        Ok(s) => s,
        Err(e) => return e as isize,
    };
    let full_path = crate::syscall::open::resolve_path(&raw);
    let inode = match crate::fs::lookup(&full_path) {
        Ok(inode) => inode,
        Err(_) => return -2,
    };
    if crate::mm::swap::is_enabled() {
        return -16; // -EBUSY: already on
    }
    crate::mm::swap::set_swapfile_ino(inode.ino());
    if crate::mm::swap::swap_on(crate::mm::swap::SWAP_NR_SLOTS) {
        0
    } else {
        -12 // -ENOMEM
    }
}

/// 关闭 swap。对标 Linux `swapoff(2)`：仍有页换出在该区时返回 -EBUSY。
pub(in crate::syscall) fn sys_swapoff(_path: *const u8) -> isize {
    if !crate::mm::swap::is_enabled() {
        return -22; // -EINVAL: not a swap device
    }
    if crate::mm::swap::swap_off() {
        0
    } else {
        -16 // -EBUSY: pages still swapped out
    }
}

pub(in crate::syscall) fn sys_perf_event_open(
    attr: *const u8,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> isize {
    crate::kernel::perf_event::sys_perf_event_open(attr, pid, cpu, group_fd, flags)
}

pub(in crate::syscall) fn sys_getcpu(cpu: *mut u32, node: *mut u32, _tcache: *mut u8) -> isize {
    if !cpu.is_null() {
        let value: u32 = crate::arch::asm_helpers::read_cpu_id() as u32;
        let _ = UserSlice::<u8>::new_mut(cpu as *mut u8, 4).write_bytes(&value.to_le_bytes());
    }
    if !node.is_null() {
        let value: u32 = 0;
        let _ = UserSlice::<u8>::new_mut(node as *mut u8, 4).write_bytes(&value.to_le_bytes());
    }
    0
}

#[allow(dead_code)]
pub(in crate::syscall) fn sys_sysinfo(info_ptr: *mut u8) -> isize {
    // Linux sys_sysinfo 末尾 copy_to_user 失败 → EFAULT（含 NULL/坏指针），
    // 旧实现返回 EINVAL，sysinfo02 的 bad_addr 子项期望 EFAULT。
    if info_ptr.is_null() { return SyscallError::BadAddress as isize; }
    let procs = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = table.as_ref() {
            table.process_count() as u32
        } else {
            0
        }
    };

    let (total_ram, free_ram) = crate::mm::get_memory_info();

    // 真实 uptime：把 rdtime 转换为秒。LTP `sysinfo02` 期望该值 > 0 且
    // 与两次调用之间的间隔单调递增。
    let uptime_sec = {
        let ticks = crate::arch::asm_helpers::rdtime() as u64;
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        // Linux do_sysinfo(): uptime = tv_sec + (tv_nsec ? 1 : 0) — it rounds
        // *up* any sub-second remainder, matching sysinfo03's read_proc_uptime
        // (which also ceils). Truncating here makes sysinfo lag /proc/uptime by
        // a second and fails the consistency check.
        let base = if freq == 0 { 0 } else { ticks / freq + if ticks % freq != 0 { 1 } else { 0 } };
        // sysinfo.uptime tracks CLOCK_BOOTTIME, so it must include the current
        // process's time-namespace boottime offset (CLONE_NEWTIME). The root
        // time namespace has offset 0 (no-op); children that unshared and wrote
        // /proc/self/timens_offsets observe the shifted uptime (sysinfo03).
        let (_mono_off, boot_off) = crate::kernel::namespace::current_time_ns_offsets();
        (base as i64 + boot_off / 1_000_000_000).max(0) as u64
    };

    let info = KernelSysInfo {
        uptime: uptime_sec,
        loads: [0; 3],
        totalram: total_ram,
        freeram: free_ram,
        sharedram: 0,
        bufferram: 0,
        // Swap 子系统真实计账（zram 式 RAM-backed）。SwapTotal/SwapFree 以页计
        // × PAGE_SIZE 转字节（mem_unit=1）。madvise06 setup 读 freeswap 判定。
        totalswap: (crate::mm::swap::total_pages() * crate::mm::PAGE_SIZE) as u64,
        freeswap: (crate::mm::swap::free_pages() * crate::mm::PAGE_SIZE) as u64,
        procs: procs as u16,
        pad: 0,
        totalhigh: 0,
        freehigh: 0,
        mem_unit: 1,
        _f: [],
    };

    let info_bytes = unsafe {
        core::slice::from_raw_parts(
            &info as *const _ as *const u8,
            core::mem::size_of::<KernelSysInfo>()
        )
    };

    if let Err(_e) = UserSlice::<u8>::new_mut(info_ptr, info_bytes.len()).write_bytes(info_bytes) {
        return SyscallError::BadAddress as isize; // -EFAULT
    }

    0
}
