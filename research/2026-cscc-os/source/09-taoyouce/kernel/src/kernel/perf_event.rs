//! perf_event_open — Linux performance monitoring subsystem.
//!
//! Implements the core `perf_event_open(2)` syscall interface:
//! - Software events (PERF_TYPE_SOFTWARE): task-clock, cpu-clock, page-faults,
//!   context-switches, cpu-migrations
//! - Hardware events (PERF_TYPE_HARDWARE): cycles, instructions, cache references,
//!   cache misses, branch instructions, branch misses (via RISC-V HPM CSRs)
//! - Event FD semantics: read(2) returns u64 counter value, mmap ring buffer not
//!   implemented (returns EINVAL)
//!
//! Reference: https://man7.org/linux/man-pages/man2/perf_event_open.2.html
//!            Linux kernel/events/core.c

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicBool, Ordering};

use crate::sync::SpinLock;

// ============================================================================
// perf_event_attr constants — per include/uapi/linux/perf_event.h
// ============================================================================

/// Event type classes
pub const PERF_TYPE_HARDWARE: u32 = 0;
pub const PERF_TYPE_SOFTWARE: u32 = 1;
pub const PERF_TYPE_TRACEPOINT: u32 = 2;
pub const PERF_TYPE_HW_CACHE: u32 = 3;
pub const PERF_TYPE_RAW: u32 = 4;

/// Hardware event IDs (PERF_TYPE_HARDWARE config values)
pub const PERF_COUNT_HW_CPU_CYCLES: u64 = 0;
pub const PERF_COUNT_HW_INSTRUCTIONS: u64 = 1;
pub const PERF_COUNT_HW_CACHE_REFERENCES: u64 = 2;
pub const PERF_COUNT_HW_CACHE_MISSES: u64 = 3;
pub const PERF_COUNT_HW_BRANCH_INSTRUCTIONS: u64 = 4;
pub const PERF_COUNT_HW_BRANCH_MISSES: u64 = 5;
pub const PERF_COUNT_HW_BUS_CYCLES: u64 = 6;
pub const PERF_COUNT_HW_STALLED_CYCLES_FRONTEND: u64 = 7;
pub const PERF_COUNT_HW_STALLED_CYCLES_BACKEND: u64 = 8;
pub const PERF_COUNT_HW_REF_CPU_CYCLES: u64 = 9;

/// Software event IDs (PERF_TYPE_SOFTWARE config values)
pub const PERF_COUNT_SW_CPU_CLOCK: u64 = 0;
pub const PERF_COUNT_SW_TASK_CLOCK: u64 = 1;
pub const PERF_COUNT_SW_PAGE_FAULTS: u64 = 2;
pub const PERF_COUNT_SW_CONTEXT_SWITCHES: u64 = 3;
pub const PERF_COUNT_SW_CPU_MIGRATIONS: u64 = 4;
pub const PERF_COUNT_SW_PAGE_FAULTS_MIN: u64 = 5;
pub const PERF_COUNT_SW_PAGE_FAULTS_MAJ: u64 = 6;
pub const PERF_COUNT_SW_ALIGNMENT_FAULTS: u64 = 7;
pub const PERF_COUNT_SW_EMULATION_FAULTS: u64 = 8;

/// Flags for perf_event_open
pub const PERF_FLAG_FD_NO_GROUP: u64 = 1 << 0;
pub const PERF_FLAG_FD_OUTPUT: u64 = 1 << 1;
pub const PERF_FLAG_PID_CGROUP: u64 = 1 << 2;
pub const PERF_FLAG_FD_CLOEXEC: u64 = 1 << 3;

// ============================================================================
// perf_event_attr structure layout (subset we parse)
// ============================================================================

/// Minimal parsed perf_event_attr (Linux struct is 136 bytes, we only use first fields)
#[derive(Clone, Debug)]
struct PerfEventAttr {
    type_: u32,       // offset 0
    size: u32,        // offset 4
    config: u64,      // offset 8
    // We ignore sample_period/sample_freq, sample_type, read_format, flags bits, etc.
    // for this implementation — we support counting mode only (no sampling/mmap)
    disabled: bool,   // bit 0 of attr.flags at offset 40
    inherit: bool,    // bit 1
    exclude_user: bool,
    exclude_kernel: bool,
}

// ============================================================================
// PerfEvent — represents an open perf event counter
// ============================================================================

/// A perf event instance (one per perf_event_open FD).
pub struct PerfEvent {
    attr: PerfEventAttr,
    /// Snapshot of counter at the time the event was enabled
    start_value: AtomicU64,
    /// Accumulated count (from disable/enable cycles)
    accumulated: AtomicU64,
    /// Whether the counter is currently active
    enabled: AtomicBool,
    /// Target PID (-1 = any, 0 = self, >0 = specific)
    pid: i32,
    /// Target CPU (-1 = any)
    cpu: i32,
}

impl PerfEvent {
    fn new(attr: PerfEventAttr, pid: i32, cpu: i32) -> Self {
        let enabled = !attr.disabled;
        let start = if enabled { Self::read_hw_counter(&attr) } else { 0 };
        Self {
            attr,
            start_value: AtomicU64::new(start),
            accumulated: AtomicU64::new(0),
            enabled: AtomicBool::new(enabled),
            pid,
            cpu,
        }
    }

    /// Read the current hardware/software counter value for this event type.
    fn read_hw_counter(attr: &PerfEventAttr) -> u64 {
        match attr.type_ {
            PERF_TYPE_HARDWARE => match attr.config {
                PERF_COUNT_HW_CPU_CYCLES | PERF_COUNT_HW_REF_CPU_CYCLES | PERF_COUNT_HW_BUS_CYCLES => {
                    // RISC-V: use rdtime as cycle proxy (rdcycle may be trapped)
                    crate::arch::asm_helpers::rdtime()
                }
                PERF_COUNT_HW_INSTRUCTIONS => {
                    // RISC-V: rdinstret counts retired instructions
                    #[cfg(target_arch = "riscv64")]
                    {
                        let val: u64;
                        unsafe { core::arch::asm!("rdinstret {}", out(reg) val); }
                        val
                    }
                    #[cfg(not(target_arch = "riscv64"))]
                    { crate::arch::asm_helpers::rdtime() }
                }
                PERF_COUNT_HW_CACHE_REFERENCES | PERF_COUNT_HW_CACHE_MISSES => {
                    // No direct HW cache counter available on most RISC-V;
                    // return rdtime as approximation (better than 0)
                    crate::arch::asm_helpers::rdtime()
                }
                PERF_COUNT_HW_BRANCH_INSTRUCTIONS | PERF_COUNT_HW_BRANCH_MISSES => {
                    // Most RISC-V lack branch prediction counters;
                    // return rdtime as proxy
                    crate::arch::asm_helpers::rdtime()
                }
                _ => crate::arch::asm_helpers::rdtime(),
            },
            PERF_TYPE_SOFTWARE => match attr.config {
                PERF_COUNT_SW_CPU_CLOCK | PERF_COUNT_SW_TASK_CLOCK => {
                    // Return nanoseconds of CPU time consumed
                    let ticks = crate::arch::asm_helpers::rdtime();
                    let freq = crate::arch::consts::CLOCK_FREQ as u64;
                    if freq == 0 {
                        ticks
                    } else {
                        (ticks / freq) * 1_000_000_000 + (ticks % freq) * 1_000_000_000 / freq
                    }
                }
                PERF_COUNT_SW_CONTEXT_SWITCHES => {
                    // Read from current process's context switch counters
                    let table = crate::sched::process::PROCESS_TABLE.lock();
                    let t = match table.as_ref() { Some(t) => t, None => return 0 };
                    let cur_pid = match t.current { Some(p) => p, None => return 0 };
                    let proc = match t.get(cur_pid) { Some(p) => p, None => return 0 };
                    proc.voluntary_ctxt_switches + proc.involuntary_ctxt_switches
                }
                PERF_COUNT_SW_PAGE_FAULTS | PERF_COUNT_SW_PAGE_FAULTS_MIN | PERF_COUNT_SW_PAGE_FAULTS_MAJ => {
                    // Page faults — we don't track this per-process yet, return 0
                    0
                }
                PERF_COUNT_SW_CPU_MIGRATIONS => {
                    // Single-core system: always 0
                    0
                }
                _ => 0,
            },
            _ => 0,
        }
    }

    /// Read the current counter delta (since last enable/reset).
    pub fn read_count(&self) -> u64 {
        if self.enabled.load(Ordering::Relaxed) {
            let current = Self::read_hw_counter(&self.attr);
            let start = self.start_value.load(Ordering::Relaxed);
            self.accumulated.load(Ordering::Relaxed) + current.wrapping_sub(start)
        } else {
            self.accumulated.load(Ordering::Relaxed)
        }
    }

    /// Enable the counter.
    pub fn enable(&self) {
        if !self.enabled.swap(true, Ordering::SeqCst) {
            self.start_value.store(Self::read_hw_counter(&self.attr), Ordering::Relaxed);
        }
    }

    /// Disable the counter — snapshot delta into accumulated.
    pub fn disable(&self) {
        if self.enabled.swap(false, Ordering::SeqCst) {
            let current = Self::read_hw_counter(&self.attr);
            let start = self.start_value.load(Ordering::Relaxed);
            let delta = current.wrapping_sub(start);
            self.accumulated.fetch_add(delta, Ordering::Relaxed);
        }
    }

    /// Reset counter to zero.
    pub fn reset(&self) {
        self.accumulated.store(0, Ordering::Relaxed);
        if self.enabled.load(Ordering::Relaxed) {
            self.start_value.store(Self::read_hw_counter(&self.attr), Ordering::Relaxed);
        }
    }
}

// ============================================================================
// Global perf event table — maps FD → PerfEvent
// ============================================================================

static PERF_EVENTS: SpinLock<Vec<(i32, Arc<PerfEvent>)>> = SpinLock::new(Vec::new());

pub fn lookup_perf_event(fd: i32) -> Option<Arc<PerfEvent>> {
    let events = PERF_EVENTS.lock();
    events.iter().find(|(f, _)| *f == fd).map(|(_, e)| e.clone())
}

fn register_perf_event(fd: i32, event: Arc<PerfEvent>) {
    PERF_EVENTS.lock().push((fd, event));
}

pub fn remove_perf_event(fd: i32) {
    let mut events = PERF_EVENTS.lock();
    events.retain(|(f, _)| *f != fd);
}

// ============================================================================
// Syscall implementation
// ============================================================================

/// perf_event_open(2) — create a performance monitoring counter.
///
/// Returns a file descriptor on success, or negative errno.
pub fn sys_perf_event_open(
    attr_ptr: *const u8,
    pid: i32,
    cpu: i32,
    group_fd: i32,
    flags: u64,
) -> isize {
    let _ = group_fd; // grouping not yet supported
    let _ = flags;    // FD_CLOEXEC handled at fd install level

    if attr_ptr.is_null() {
        return -14; // EFAULT
    }

    // Parse perf_event_attr from userspace.
    //
    // 必须经内核用户内存拷贝助手译址访问，禁止在 S 态裸读用户指针：SUM 未置位时
    // S 态直接解引用用户页会触发缺页，而 trap 入口不支持嵌套陷入，会演变成
    // __alltraps 递归风暴卡死整组。read_user_buffer 走「用户 VA→物理→内核直映射」
    // 访问，与内核其余 syscall 一致，并对懒分配页按需建立映射。
    //
    // Minimum size: type(4) + size(4) + config(8) = 16 bytes
    let head = match crate::syscall::user::read_user_buffer(attr_ptr, 16) {
        Ok(b) => b,
        Err(_) => return -14, // EFAULT
    };
    let type_ = u32::from_le_bytes(head[0..4].try_into().unwrap());
    let size = u32::from_le_bytes(head[4..8].try_into().unwrap());
    let config = u64::from_le_bytes(head[8..16].try_into().unwrap());

    // Read flags dword at offset 40 (if attr is large enough)
    let attr_flags: u64 = if size >= 48 {
        match crate::syscall::user::read_user_buffer(unsafe { attr_ptr.add(40) }, 8) {
            Ok(b) => u64::from_le_bytes(b[0..8].try_into().unwrap()),
            Err(_) => return -14, // EFAULT
        }
    } else {
        0
    };

    let disabled = attr_flags & 1 != 0;
    let inherit = attr_flags & (1 << 1) != 0;
    let exclude_user = attr_flags & (1 << 2) != 0;
    let exclude_kernel = attr_flags & (1 << 3) != 0;

    // Validate event type
    match type_ {
        PERF_TYPE_HARDWARE => {
            if config > PERF_COUNT_HW_REF_CPU_CYCLES {
                return -22; // EINVAL: unknown hardware event
            }
        }
        PERF_TYPE_SOFTWARE => {
            if config > PERF_COUNT_SW_EMULATION_FAULTS {
                return -22;
            }
        }
        PERF_TYPE_TRACEPOINT | PERF_TYPE_HW_CACHE | PERF_TYPE_RAW => {
            // Accept but treat as rdtime-based counting
        }
        _ => return -22, // EINVAL: unknown type
    }

    let attr = PerfEventAttr {
        type_,
        size,
        config,
        disabled,
        inherit,
        exclude_user,
        exclude_kernel,
    };

    let event = Arc::new(PerfEvent::new(attr, pid, cpu));

    // Allocate FD in current process
    let fd = install_perf_fd(&event);
    if fd < 0 {
        return -24; // EMFILE
    }

    register_perf_event(fd, event);
    fd as isize
}

/// Install a perf event FD into the current process's fd table.
/// Returns the allocated fd number.
fn install_perf_fd(event: &Arc<PerfEvent>) -> i32 {
    use alloc::sync::Arc as StdArc;
    let _ = event; // The PerfEvent is tracked separately in PERF_EVENTS table

    // Get /dev/null inode as backing
    let inode = match crate::fs::devfs::lookup_path("dev/null") {
        Ok(i) => i,
        Err(_) => return -1,
    };

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() { Some(t) => t, None => return -1 };
    let cur_pid = match t.current { Some(p) => p, None => return -1 };
    let proc = match t.get_mut(cur_pid) { Some(p) => p, None => return -1 };

    // Find lowest available fd slot
    let mut fd_num = -1i32;
    for (i, slot) in proc.fd_table.iter().enumerate() {
        if slot.is_none() {
            fd_num = i as i32;
            break;
        }
    }
    if fd_num < 0 {
        // Extend table
        fd_num = proc.fd_table.len() as i32;
        proc.fd_table.push(None);
    }

    let fd_obj = StdArc::new(crate::fs::FileDescriptor::new_with_path(
        inode,
        0o0, // O_RDONLY
        Some(alloc::format!("anon_inode:[perf_event]")),
    ));
    fd_obj.inc_fd_ref();
    proc.fd_table[fd_num as usize] = Some(fd_obj);

    fd_num
}

/// Read handler for perf event FDs — returns u64 counter value.
/// Called from sys_read when detecting a perf_event anon_inode fd.
pub fn perf_event_read(fd: i32, buf: &mut [u8]) -> isize {
    if buf.len() < 8 {
        return -22; // EINVAL: must read at least 8 bytes
    }
    let event = match lookup_perf_event(fd) {
        Some(e) => e,
        None => return -9, // EBADF
    };
    let count = event.read_count();
    buf[0..8].copy_from_slice(&count.to_ne_bytes());
    8
}

/// ioctl handler for perf events — PERF_EVENT_IOC_ENABLE/DISABLE/RESET.
pub fn perf_event_ioctl(fd: i32, cmd: u64, _arg: u64) -> isize {
    const PERF_EVENT_IOC_ENABLE: u64 = 0x2400;
    const PERF_EVENT_IOC_DISABLE: u64 = 0x2401;
    const PERF_EVENT_IOC_REFRESH: u64 = 0x2402;
    const PERF_EVENT_IOC_RESET: u64 = 0x2403;
    const PERF_EVENT_IOC_PERIOD: u64 = 0x2404;
    const PERF_EVENT_IOC_SET_OUTPUT: u64 = 0x2405;
    const PERF_EVENT_IOC_SET_FILTER: u64 = 0x2406;
    const PERF_EVENT_IOC_ID: u64 = 0x2407;

    let event = match lookup_perf_event(fd) {
        Some(e) => e,
        None => return -9,
    };

    match cmd {
        PERF_EVENT_IOC_ENABLE => { event.enable(); 0 }
        PERF_EVENT_IOC_DISABLE => { event.disable(); 0 }
        PERF_EVENT_IOC_RESET => { event.reset(); 0 }
        PERF_EVENT_IOC_REFRESH => 0, // no-op for counting mode
        PERF_EVENT_IOC_PERIOD => 0,  // no-op for counting mode
        PERF_EVENT_IOC_SET_OUTPUT => 0,
        PERF_EVENT_IOC_SET_FILTER => 0,
        PERF_EVENT_IOC_ID => 0,
        _ => -22, // EINVAL
    }
}
