//! ptrace subsystem — per Linux kernel/ptrace.c
//!
//! Extracted from compat.rs. Implements:
//! * ptrace(2) — TRACEME, ATTACH/SEIZE, PEEK/POKE, CONT/KILL/DETACH, etc.
//! * process_vm_readv(2) / process_vm_writev(2) — cross-process memory access
//! * kcmp(2) — compare kernel objects between processes

extern crate alloc;

use alloc::collections::BTreeSet;

// ============================================================================
// Traced-pid tracking
// ============================================================================

static PTRACE_TRACED: crate::sync::SpinLock<BTreeSet<usize>> =
    crate::sync::SpinLock::new(BTreeSet::new());

// ============================================================================
// ptrace(2) — per Linux kernel/ptrace.c
// ============================================================================

pub(crate) fn sys_ptrace(request: usize, pid: i32, addr: usize, data: usize) -> isize {
    const PTRACE_TRACEME:     usize = 0;
    const PTRACE_PEEKTEXT:    usize = 1;
    const PTRACE_PEEKDATA:    usize = 2;
    const PTRACE_PEEKUSER:    usize = 3;
    const PTRACE_POKETEXT:    usize = 4;
    const PTRACE_POKEDATA:    usize = 5;
    const PTRACE_POKEUSER:    usize = 6;
    const PTRACE_CONT:        usize = 7;
    const PTRACE_KILL:        usize = 8;
    const PTRACE_SINGLESTEP:  usize = 9;
    const PTRACE_ATTACH:      usize = 16;
    const PTRACE_DETACH:      usize = 17;
    const PTRACE_GETREGS:     usize = 12;
    const PTRACE_SETREGS:     usize = 13;
    const PTRACE_GETFPREGS:   usize = 14;
    const PTRACE_SETFPREGS:   usize = 15;
    const PTRACE_SYSCALL:     usize = 24;
    const PTRACE_SETOPTIONS:  usize = 0x4200;
    const PTRACE_GETEVENTMSG: usize = 0x4201;
    const PTRACE_GETSIGINFO:  usize = 0x4202;
    const PTRACE_SETSIGINFO:  usize = 0x4203;
    const PTRACE_GETREGSET:   usize = 0x4204;
    const PTRACE_SETREGSET:   usize = 0x4205;
    const PTRACE_SEIZE:       usize = 0x4206;
    const PTRACE_INTERRUPT:   usize = 0x4207;
    const PTRACE_LISTEN:      usize = 0x4208;
    const PTRACE_PEEKSIGINFO: usize = 0x4209;
    const PTRACE_GET_SYSCALL_INFO: usize = 0x420e;

    // PTRACE_SETOPTIONS option flags
    const _PTRACE_O_TRACESYSGOOD:  usize = 1;
    const _PTRACE_O_TRACEFORK:     usize = 2;
    const _PTRACE_O_TRACEVFORK:    usize = 4;
    const _PTRACE_O_TRACECLONE:    usize = 8;
    const _PTRACE_O_TRACEEXEC:     usize = 16;
    const _PTRACE_O_TRACEVFORKDONE:usize = 32;
    const _PTRACE_O_TRACEEXIT:     usize = 64;
    const _PTRACE_O_TRACESECCOMP:  usize = 128;
    const _PTRACE_O_EXITKILL:      usize = 1 << 20;
    const _PTRACE_O_SUSPEND_SECCOMP: usize = 1 << 21;

    // ptrace stop reason constants
    const PTRACE_STOP_SIGNAL: u8 = 1;
    const PTRACE_STOP_SYSCALL: u8 = 2;
    const PTRACE_STOP_SINGLESTEP: u8 = 3;

    match request {
        PTRACE_TRACEME => {
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            let table = match guard.as_mut() { Some(t) => t, None => return -3 };
            let cur_pid = match table.current { Some(p) => p, None => return -3 };
            let proc = match table.get_mut(cur_pid) { Some(p) => p, None => return -3 };
            if proc.ptrace_tracer != 0 {
                return -1; // -EPERM: already traced
            }
            // Set tracer to parent (Linux semantics: TRACEME allows parent to trace)
            proc.ptrace_tracer = proc.parent_pid.unwrap_or(0);
            let mut traced = PTRACE_TRACED.lock();
            traced.insert(cur_pid);
            0
        }
        PTRACE_CONT => {
            // Resume tracee, optionally re-injecting the signal named in `data`.
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            let table = match guard.as_mut() { Some(t) => t, None => return -3 };
            match table.get(pid as usize) {
                Some(p) if p.ptrace_stopped => {}
                Some(_) => return -3, // not stopped
                None => return -3,
            }
            table.ptrace_resume_tracee(pid as usize, data as u32);
            0
        }
        PTRACE_SINGLESTEP => {
            // Resume tracee for one instruction, then stop again
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            let table = match guard.as_mut() { Some(t) => t, None => return -3 };
            let tracee = match table.get_mut(pid as usize) { Some(p) => p, None => return -3 };
            if !tracee.ptrace_stopped { return -3; }
            tracee.ptrace_stopped = false;
            tracee.ptrace_stop_reason = PTRACE_STOP_SINGLESTEP; // flag: stop after 1 insn
            tracee.ptrace_pending_sig = data as u32;
            tracee.state = crate::sched::process::ProcessState::Runnable;
            0
        }
        PTRACE_SYSCALL => {
            // Resume tracee, stop at next syscall entry/exit
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            let table = match guard.as_mut() { Some(t) => t, None => return -3 };
            let tracee = match table.get_mut(pid as usize) { Some(p) => p, None => return -3 };
            if !tracee.ptrace_stopped { return -3; }
            tracee.ptrace_stopped = false;
            tracee.ptrace_stop_reason = PTRACE_STOP_SYSCALL; // flag: stop at next syscall
            tracee.ptrace_pending_sig = data as u32;
            tracee.state = crate::sched::process::ProcessState::Runnable;
            0
        }
        PTRACE_KILL => {
            // Kill tracee (deprecated but still used): clear the trace stop,
            // re-inject SIGKILL and make the tracee runnable so the fatal
            // signal is actually dequeued and acted on.
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            let table = match guard.as_mut() { Some(t) => t, None => return -3 };
            if table.get(pid as usize).is_none() { return -3; }
            table.ptrace_resume_tracee(pid as usize, 9);
            if let Some(tracee) = table.get_mut(pid as usize) {
                tracee.ptrace_tracer = 0;
            }
            drop(guard);
            super::signal::sys_kill(pid as isize, 9)
        }
        PTRACE_DETACH => {
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            let table = match guard.as_mut() { Some(t) => t, None => return -3 };
            let tracee = match table.get_mut(pid as usize) { Some(p) => p, None => return -3 };
            tracee.ptrace_tracer = 0;
            tracee.ptrace_options = 0;
            if tracee.ptrace_stopped {
                tracee.ptrace_stopped = false;
                tracee.ptrace_stop_reason = 0;
                tracee.ptrace_pending_sig = data as u32;
                tracee.state = crate::sched::process::ProcessState::Runnable;
            }
            let mut traced = PTRACE_TRACED.lock();
            traced.remove(&(pid as usize));
            0
        }
        PTRACE_ATTACH | PTRACE_SEIZE => {
            const CAP_SYS_PTRACE: u64 = 1u64 << 19;
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            let table = match guard.as_mut() { Some(t) => t, None => return -3 };
            let tracer_pid = match table.current { Some(p) => p, None => return -3 };
            // Permission check
            let allowed = {
                let tracer = table.get(tracer_pid);
                let target = table.get(pid as usize);
                match (tracer, target) {
                    (_, None) => return -3, // -ESRCH
                    (Some(tr), Some(tg)) => {
                        if tg.ptrace_tracer != 0 {
                            return -1; // already traced
                        }
                        tr.euid == 0
                            || (tr.cap_effective & CAP_SYS_PTRACE) != 0
                            || (tr.euid == tg.uid && tr.euid == tg.euid
                                && tr.euid == tg.suid)
                    }
                    _ => false,
                }
            };
            if !allowed { return -1; } // -EPERM
            // Set tracer on target
            let tracee = table.get_mut(pid as usize).unwrap();
            tracee.ptrace_tracer = tracer_pid;
            let mut traced = PTRACE_TRACED.lock();
            traced.insert(pid as usize);
            drop(guard);
            if request == PTRACE_ATTACH {
                // ATTACH sends SIGSTOP to tracee (Linux ptrace_attach)
                super::signal::sys_kill(pid as isize, 19) // SIGSTOP
            } else {
                // SEIZE doesn't stop the tracee immediately
                0
            }
        }
        PTRACE_PEEKTEXT | PTRACE_PEEKDATA | PTRACE_PEEKUSER => {
            let cur_pid = {
                let t = crate::sched::process::PROCESS_TABLE.lock();
                t.as_ref().and_then(|tt| tt.current).unwrap_or(0)
            };
            if pid as usize != cur_pid {
                return -3; // -ESRCH
            }
            let bytes = match super::user::read_user_buffer(addr as *const u8, 8) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let word = u64::from_le_bytes(bytes[..8].try_into().unwrap());
            if data != 0 {
                let _ = super::user::write_user_buffer(data as *mut u8, &word.to_le_bytes());
                return 0;
            }
            word as isize
        }
        PTRACE_POKETEXT | PTRACE_POKEDATA | PTRACE_POKEUSER => {
            let cur_pid = {
                let t = crate::sched::process::PROCESS_TABLE.lock();
                t.as_ref().and_then(|tt| tt.current).unwrap_or(0)
            };
            if pid as usize != cur_pid {
                return -3;
            }
            let word = data as u64;
            let _ = super::user::write_user_buffer(addr as *mut u8, &word.to_le_bytes());
            0
        }
        // PTRACE_GETREGS: copy all general-purpose registers to data buffer.
        // On riscv64/loongarch64 we return the trap context (32 GP regs × 8 bytes = 256 bytes).
        PTRACE_GETREGS => {
            if data == 0 { return -14; } // EFAULT
            let cur_pid = {
                let t = crate::sched::process::PROCESS_TABLE.lock();
                t.as_ref().and_then(|tt| tt.current).unwrap_or(0)
            };
            if pid as usize != cur_pid { return -3; }
            // Return zeros for registers we don't track (safe stub)
            let regs = [0u8; 256];
            match super::user::write_user_buffer(data as *mut u8, &regs) {
                Ok(_) => 0,
                Err(_) => -14,
            }
        }
        // PTRACE_SETREGS: set general-purpose registers from data buffer
        PTRACE_SETREGS => {
            if data == 0 { return -14; }
            let cur_pid = {
                let t = crate::sched::process::PROCESS_TABLE.lock();
                t.as_ref().and_then(|tt| tt.current).unwrap_or(0)
            };
            if pid as usize != cur_pid { return -3; }
            // Accept the register data (we don't actually apply it to the tracee)
            match super::user::read_user_buffer(data as *const u8, 256) {
                Ok(_) => 0,
                Err(_) => -14,
            }
        }
        // PTRACE_GETFPREGS / SETFPREGS: floating-point registers
        PTRACE_GETFPREGS => {
            if data == 0 { return -14; }
            let regs = [0u8; 256];
            match super::user::write_user_buffer(data as *mut u8, &regs) {
                Ok(_) => 0,
                Err(_) => -14,
            }
        }
        PTRACE_SETFPREGS => {
            if data == 0 { return -14; }
            match super::user::read_user_buffer(data as *const u8, 256) {
                Ok(_) => 0,
                Err(_) => -14,
            }
        }
        // PTRACE_SETOPTIONS: set ptrace options for the tracee
        PTRACE_SETOPTIONS => {
            let known_opts = _PTRACE_O_TRACESYSGOOD | _PTRACE_O_TRACEFORK
                | _PTRACE_O_TRACEVFORK | _PTRACE_O_TRACECLONE | _PTRACE_O_TRACEEXEC
                | _PTRACE_O_TRACEVFORKDONE | _PTRACE_O_TRACEEXIT | _PTRACE_O_TRACESECCOMP
                | _PTRACE_O_EXITKILL | _PTRACE_O_SUSPEND_SECCOMP;
            if data & !known_opts != 0 { return -22; } // EINVAL
            // Store options on tracee PCB
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            let table = match guard.as_mut() { Some(t) => t, None => return -3 };
            let tracee = match table.get_mut(pid as usize) { Some(p) => p, None => return -3 };
            tracee.ptrace_options = data as u32;
            0
        }
        // PTRACE_GETEVENTMSG: retrieve message about last ptrace event
        PTRACE_GETEVENTMSG => {
            if data == 0 { return -14; }
            // Return 0 as event message (no event tracked)
            let msg = 0u64.to_le_bytes();
            match super::user::write_user_buffer(data as *mut u8, &msg) {
                Ok(_) => 0,
                Err(_) => -14,
            }
        }
        // PTRACE_GETSIGINFO: retrieve siginfo_t for last signal
        PTRACE_GETSIGINFO => {
            if data == 0 { return -14; }
            // Return zeroed siginfo (128 bytes)
            let info = [0u8; 128];
            match super::user::write_user_buffer(data as *mut u8, &info) {
                Ok(_) => 0,
                Err(_) => -14,
            }
        }
        // PTRACE_SETSIGINFO: modify siginfo for tracee
        PTRACE_SETSIGINFO => {
            if data == 0 { return -14; }
            match super::user::read_user_buffer(data as *const u8, 128) {
                Ok(_) => 0,
                Err(_) => -14,
            }
        }
        // PTRACE_GETREGSET / SETREGSET: iov-based register access (NT_PRSTATUS etc.)
        PTRACE_GETREGSET => {
            if data == 0 { return -14; }
            // Read iovec from data: { iov_base: *u8, iov_len: usize }
            let iov = match super::user::read_user_buffer(data as *const u8, 16) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let iov_base = usize::from_le_bytes(iov[0..8].try_into().unwrap());
            let iov_len = usize::from_le_bytes(iov[8..16].try_into().unwrap());
            if iov_base == 0 || iov_len == 0 { return -14; }
            let write_len = iov_len.min(256);
            let regs = [0u8; 256];
            match super::user::write_user_buffer(iov_base as *mut u8, &regs[..write_len]) {
                Ok(_) => {
                    // Update iov_len to actual bytes written
                    let new_iov_len = write_len.to_le_bytes();
                    let _ = super::user::write_user_buffer(
                        unsafe { (data as *mut u8).add(8) }, &new_iov_len
                    );
                    0
                }
                Err(_) => -14,
            }
        }
        PTRACE_SETREGSET => {
            if data == 0 { return -14; }
            let iov = match super::user::read_user_buffer(data as *const u8, 16) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let iov_base = usize::from_le_bytes(iov[0..8].try_into().unwrap());
            let iov_len = usize::from_le_bytes(iov[8..16].try_into().unwrap());
            if iov_base == 0 || iov_len == 0 { return -14; }
            match super::user::read_user_buffer(iov_base as *const u8, iov_len.min(256)) {
                Ok(_) => 0,
                Err(_) => -14,
            }
        }
        // PTRACE_INTERRUPT / LISTEN / PEEKSIGINFO / GET_SYSCALL_INFO
        PTRACE_INTERRUPT | PTRACE_LISTEN => 0,
        PTRACE_PEEKSIGINFO => {
            if data == 0 { return -14; }
            let info = [0u8; 128];
            match super::user::write_user_buffer(data as *mut u8, &info) {
                Ok(_) => 0,
                Err(_) => -14,
            }
        }
        PTRACE_GET_SYSCALL_INFO => {
            if data == 0 { return -14; }
            // struct ptrace_syscall_info: op(1) + pad(3) + arch(4) + instruction_pointer(8) + stack_pointer(8) + ...
            // Minimum size we return is 24 bytes
            let info = [0u8; 24];
            let write_len = (addr).min(24); // addr = size in GET_SYSCALL_INFO
            match super::user::write_user_buffer(data as *mut u8, &info[..write_len]) {
                Ok(_) => write_len as isize,
                Err(_) => -14,
            }
        }
        _ => -22, // -EINVAL
    }
}

// ============================================================================
// ptrace hooks — called from syscall dispatch and trap handler
// ============================================================================

/// ptrace stop reason constants (public for use in trap handler)
pub const PTRACE_STOP_SIGNAL: u8 = 1;
pub const PTRACE_STOP_SYSCALL: u8 = 2;
pub const PTRACE_STOP_SINGLESTEP: u8 = 3;

/// Called at syscall entry when PTRACE_SYSCALL is active.
/// Returns true if the current process should be stopped (tracer notified).
/// Per Linux `ptrace_report_syscall_entry` in kernel/ptrace.c
pub fn ptrace_syscall_entry_check() -> bool {
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return false };
    let pid = match table.current { Some(p) => p, None => return false };
    let proc = match table.get_mut(pid) { Some(p) => p, None => return false };
    if proc.ptrace_tracer == 0 { return false; }
    if proc.ptrace_stop_reason == PTRACE_STOP_SYSCALL {
        // Stop at syscall entry
        proc.ptrace_stopped = true;
        proc.ptrace_syscall_entry = true;
        proc.ptrace_stop_reason = PTRACE_STOP_SIGNAL; // next resume needs new SYSCALL
        proc.state = crate::sched::process::ProcessState::Stopped;
        return true;
    }
    false
}

/// Called at syscall exit when PTRACE_SYSCALL is active.
/// Per Linux `ptrace_report_syscall_exit`
pub fn ptrace_syscall_exit_check() -> bool {
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return false };
    let pid = match table.current { Some(p) => p, None => return false };
    let proc = match table.get_mut(pid) { Some(p) => p, None => return false };
    if proc.ptrace_tracer == 0 { return false; }
    // After a PTRACE_SYSCALL resume, we stop at both entry and exit.
    // If we just did entry, the next stop is exit.
    if proc.ptrace_stop_reason == PTRACE_STOP_SYSCALL && !proc.ptrace_syscall_entry {
        proc.ptrace_stopped = true;
        proc.ptrace_stop_reason = PTRACE_STOP_SIGNAL;
        proc.state = crate::sched::process::ProcessState::Stopped;
        return true;
    }
    // Flip entry flag for next time
    if proc.ptrace_syscall_entry {
        proc.ptrace_syscall_entry = false;
    }
    false
}

/// Called after each instruction execution for PTRACE_SINGLESTEP.
/// Returns true if the process should be stopped.
pub fn ptrace_singlestep_check() -> bool {
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return false };
    let pid = match table.current { Some(p) => p, None => return false };
    let proc = match table.get_mut(pid) { Some(p) => p, None => return false };
    if proc.ptrace_tracer == 0 { return false; }
    if proc.ptrace_stop_reason == PTRACE_STOP_SINGLESTEP {
        proc.ptrace_stopped = true;
        proc.ptrace_stop_reason = PTRACE_STOP_SIGNAL;
        proc.state = crate::sched::process::ProcessState::Stopped;
        return true;
    }
    false
}

/// Check if a PID is being ptrace-stopped (for waitpid reporting).
/// Returns (is_stopped, stop_signal) where stop_signal encodes the wait status.
pub fn ptrace_check_stopped(pid: usize) -> Option<i32> {
    let guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = guard.as_ref()?;
    let proc = table.get(pid)?;
    if proc.ptrace_stopped {
        // waitpid reports WIFSTOPPED with signal SIGTRAP (5)
        // Status encoding: (SIGTRAP << 8) | 0x7f
        Some((5 << 8) | 0x7f)
    } else {
        None
    }
}

// ============================================================================
// process_vm_readv(2) — per Linux mm/process_vm_access.c
// ============================================================================

/// INT_MAX & PAGE_MASK — Linux `MAX_RW_COUNT`, upper bound on a single transfer.
const PVM_MAX_RW_COUNT: usize = 0x7fff_f000;
/// UIO_MAXIOV — maximum iovec count.
const PVM_UIO_MAXIOV: usize = 1024;
#[cfg(target_arch = "riscv64")]
const PVM_USER_VA_END: usize = 1usize << 38; // Sv39 user half
#[cfg(target_arch = "loongarch64")]
const PVM_USER_VA_END: usize = 1usize << 48;

/// Parse and validate an iovec array (16 bytes/element) already copied into
/// `raw` from the caller's address space. Mirrors Linux `rw_copy_check_uvector`:
/// a negative `iov_len` is EINVAL, and an out-of-range `iov_base`/range is EFAULT.
fn pvm_parse_iov(raw: &[u8], cnt: usize) -> Result<alloc::vec::Vec<(usize, usize)>, isize> {
    let mut v = alloc::vec::Vec::with_capacity(cnt);
    for i in 0..cnt {
        let base = usize::from_le_bytes(raw[i * 16..i * 16 + 8].try_into().unwrap());
        let len = usize::from_le_bytes(raw[i * 16 + 8..i * 16 + 16].try_into().unwrap());
        if (len as isize) < 0 {
            return Err(-22); // EINVAL: iov_len negative as ssize_t
        }
        if len != 0 {
            let end = match base.checked_add(len) {
                Some(e) => e,
                None => return Err(-14),
            };
            if base >= PVM_USER_VA_END || end > PVM_USER_VA_END {
                return Err(-14); // EFAULT: access_ok
            }
        }
        v.push((base, len));
    }
    Ok(v)
}

/// `ptrace_may_access` for PTRACE_MODE_ATTACH_REALCREDS (used by process_vm_*):
/// allowed if caller holds CAP_SYS_PTRACE, otherwise caller's uid/gid must match
/// the target's real, effective and saved uid/gid.
fn pvm_may_access(
    cur_uid: u32, cur_gid: u32, cur_caps: u64,
    t_uid: u32, t_euid: u32, t_suid: u32, t_gid: u32, t_egid: u32, t_sgid: u32,
) -> bool {
    const CAP_SYS_PTRACE: u64 = 1 << 19;
    if cur_caps & CAP_SYS_PTRACE != 0 {
        return true;
    }
    cur_uid == t_uid && cur_uid == t_euid && cur_uid == t_suid
        && cur_gid == t_gid && cur_gid == t_egid && cur_gid == t_sgid
}

/// Read `len` bytes from `va` in the target address space `mm`.
fn pvm_read_remote(
    mm: &alloc::sync::Arc<crate::mm::mm_struct::MmStruct>, va: usize, len: usize,
) -> Result<alloc::vec::Vec<u8>, isize> {
    use crate::mm::{VirtAddr, PAGE_SIZE};
    let mut out = alloc::vec![0u8; len];
    let mut as_ = mm.lock();
    let mut done = 0usize;
    while done < len {
        let cur = va + done;
        let off = cur & (PAGE_SIZE - 1);
        let chunk = (PAGE_SIZE - off).min(len - done);
        let vpn = VirtAddr(cur).floor();
        let ppn = match as_.page_table().translate_readable(vpn) {
            Some(p) => p,
            None => {
                if as_.page_table().translate(vpn).is_some() {
                    return Err(-14); // PROT_NONE
                }
                if as_.handle_page_fault(VirtAddr(cur & !(PAGE_SIZE - 1)), false) {
                    match as_.page_table().translate_readable(vpn) {
                        Some(p) => p,
                        None => return Err(-14),
                    }
                } else {
                    return Err(-14);
                }
            }
        };
        let kptr = crate::mm::phys_to_kernel_vaddr((ppn.0 << 12) | off) as *const u8;
        unsafe {
            core::ptr::copy_nonoverlapping(kptr, out.as_mut_ptr().add(done), chunk);
        }
        done += chunk;
    }
    Ok(out)
}

/// Write `data` to `va` in the target address space `mm`.
fn pvm_write_remote(
    mm: &alloc::sync::Arc<crate::mm::mm_struct::MmStruct>, va: usize, data: &[u8],
) -> Result<(), isize> {
    use crate::mm::{PTEFlags, VirtAddr, PAGE_SIZE};
    let mut as_ = mm.lock();
    let mut done = 0usize;
    while done < data.len() {
        let cur = va + done;
        let off = cur & (PAGE_SIZE - 1);
        let chunk = (PAGE_SIZE - off).min(data.len() - done);
        let vpn = VirtAddr(cur).floor();
        let writable = as_
            .page_table()
            .get_pte_flags(vpn)
            .map(|f| f.contains(PTEFlags::W))
            .unwrap_or(false);
        let ppn = if writable {
            as_.page_table().translate(vpn).ok_or(-14isize)?
        } else {
            // Lazy/CoW page: fault it in for write, then re-check writability.
            if as_.handle_page_fault(VirtAddr(cur & !(PAGE_SIZE - 1)), true)
                && as_
                    .page_table()
                    .get_pte_flags(vpn)
                    .map(|f| f.contains(PTEFlags::W))
                    .unwrap_or(false)
            {
                as_.page_table().translate(vpn).ok_or(-14isize)?
            } else {
                return Err(-14);
            }
        };
        let kptr = crate::mm::phys_to_kernel_vaddr((ppn.0 << 12) | off) as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(data.as_ptr().add(done), kptr, chunk);
        }
        done += chunk;
    }
    Ok(())
}

/// Shared core of process_vm_readv(2) / process_vm_writev(2).
/// Per Linux mm/process_vm_access.c: validates iovecs and credentials, then
/// transfers between the caller's and the target's address spaces.
fn process_vm_rw(
    pid: i32, local_iov: *const u8, liovcnt: usize, remote_iov: *const u8, riovcnt: usize,
    flags: u64, write_to_remote: bool,
) -> isize {
    if flags != 0 {
        return -22; // EINVAL
    }
    if liovcnt > PVM_UIO_MAXIOV || riovcnt > PVM_UIO_MAXIOV {
        return -22; // EINVAL
    }

    let (cur_uid, cur_gid, cur_caps) = {
        let t = crate::sched::process::PROCESS_TABLE.lock();
        let table = match t.as_ref() {
            Some(x) => x,
            None => return -3,
        };
        let cp = match table.current {
            Some(p) => p,
            None => return -3,
        };
        match table.get(cp) {
            Some(p) => (p.uid, p.gid, p.cap_effective),
            None => return -3,
        }
    };

    // Both iovec arrays live in the caller's address space.
    let local_raw = match super::user::read_user_buffer(local_iov, liovcnt.saturating_mul(16)) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let remote_raw = match super::user::read_user_buffer(remote_iov, riovcnt.saturating_mul(16)) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let local = match pvm_parse_iov(&local_raw, liovcnt) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let remote = match pvm_parse_iov(&remote_raw, riovcnt) {
        Ok(v) => v,
        Err(e) => return e,
    };

    // Locate target, check permission, clone its mm (thread → tgid leader).
    let target_mm = {
        let t = crate::sched::process::PROCESS_TABLE.lock();
        let table = match t.as_ref() {
            Some(x) => x,
            None => return -3,
        };
        let tp = match table.get(pid as usize) {
            Some(p) => p,
            None => return -3, // ESRCH
        };
        if !pvm_may_access(
            cur_uid, cur_gid, cur_caps, tp.uid, tp.euid, tp.suid, tp.gid, tp.egid, tp.sgid,
        ) {
            return -1; // EPERM
        }
        let mm_owner = if tp.mm.is_some() { pid as usize } else { tp.tgid };
        match table.get(mm_owner).and_then(|p| p.mm.clone()) {
            Some(m) => m,
            None => return -3,
        }
    };

    let mut total: usize = 0;
    let mut errno: isize = 0;
    let (mut li, mut ri, mut lo, mut ro) = (0usize, 0usize, 0usize, 0usize);
    while li < local.len() && ri < remote.len() {
        let (lb, ll) = local[li];
        let (rb, rl) = remote[ri];
        if ll == 0 {
            li += 1;
            lo = 0;
            continue;
        }
        if rl == 0 {
            ri += 1;
            ro = 0;
            continue;
        }
        let chunk = (ll - lo).min(rl - ro).min(PVM_MAX_RW_COUNT - total);
        if chunk == 0 {
            break;
        }
        let l_addr = lb + lo;
        let r_addr = rb + ro;
        if write_to_remote {
            let buf = match super::user::read_user_buffer(l_addr as *const u8, chunk) {
                Ok(b) => b,
                Err(_) => {
                    errno = -14;
                    break;
                }
            };
            if let Err(e) = pvm_write_remote(&target_mm, r_addr, &buf) {
                errno = e;
                break;
            }
        } else {
            let buf = match pvm_read_remote(&target_mm, r_addr, chunk) {
                Ok(b) => b,
                Err(e) => {
                    errno = e;
                    break;
                }
            };
            if super::user::write_user_buffer(l_addr as *mut u8, &buf).is_err() {
                errno = -14;
                break;
            }
        }
        total += chunk;
        lo += chunk;
        ro += chunk;
        if lo == ll {
            li += 1;
            lo = 0;
        }
        if ro == rl {
            ri += 1;
            ro = 0;
        }
    }
    if total == 0 && errno != 0 {
        errno
    } else {
        total as isize
    }
}

pub(crate) fn sys_process_vm_readv(
    pid: i32, local_iov: *const u8, liovcnt: usize, remote_iov: *const u8, riovcnt: usize, flags: u64,
) -> isize {
    process_vm_rw(pid, local_iov, liovcnt, remote_iov, riovcnt, flags, false)
}

pub(crate) fn sys_process_vm_writev(
    pid: i32, local_iov: *const u8, liovcnt: usize, remote_iov: *const u8, riovcnt: usize, flags: u64,
) -> isize {
    process_vm_rw(pid, local_iov, liovcnt, remote_iov, riovcnt, flags, true)
}

// ============================================================================
// kcmp(2) — per Linux kernel/kcmp.c
// ============================================================================

pub(crate) fn sys_kcmp(pid1: i32, pid2: i32, typ: i32, idx1: usize, idx2: usize) -> isize {
    use alloc::sync::Arc;
    use crate::sched::process::{ProcessTable, PROCESS_TABLE};

    const KCMP_FILE: i32 = 0;
    const KCMP_VM: i32 = 1;
    const KCMP_FILES: i32 = 2;
    const KCMP_FS: i32 = 3;
    const KCMP_SIGHAND: i32 = 4;
    const KCMP_IO: i32 = 5;
    const KCMP_SYSVSEM: i32 = 6;
    const KCMP_EPOLL_TFD: i32 = 7;
    const KCMP_TYPES: i32 = 8;

    let mut guard = PROCESS_TABLE.lock();
    let table = match guard.as_mut() {
        Some(t) => t,
        None => return -3,
    };

    if table.get(pid1 as usize).is_none() || table.get(pid2 as usize).is_none() {
        return -3;
    }

    if typ < 0 || typ >= KCMP_TYPES {
        return -22;
    }

    let file_id = |t: &ProcessTable, pid: usize, fd: usize| -> Option<usize> {
        let p = t.get_fd_process(pid)?;
        let fdo = crate::fs::fd::get(p, fd)?;
        Some(Arc::as_ptr(&fdo) as *const () as usize)
    };
    let vm_id = |t: &ProcessTable, pid: usize| -> usize {
        t.get(pid).map(|p| p.page_table_root.0).unwrap_or(0)
    };
    let files_id = |t: &ProcessTable, pid: usize| -> usize {
        t.get(pid).map(|p| p.fd_table_owner as usize).unwrap_or(0)
    };

    let (v1, v2) = match typ {
        KCMP_FILE => {
            match (file_id(table, pid1 as usize, idx1), file_id(table, pid2 as usize, idx2)) {
                (Some(a), Some(b)) => (a, b),
                _ => return -9,
            }
        }
        KCMP_VM => (vm_id(table, pid1 as usize), vm_id(table, pid2 as usize)),
        KCMP_FILES => (files_id(table, pid1 as usize), files_id(table, pid2 as usize)),
        KCMP_FS => {
            let a = table.get(pid1 as usize).map(|p| p.fs_share_id as usize).unwrap_or(0);
            let b = table.get(pid2 as usize).map(|p| p.fs_share_id as usize).unwrap_or(0);
            (a, b)
        }
        KCMP_IO => {
            let a = table.get(pid1 as usize).map(|p| p.io_share_id as usize).unwrap_or(0);
            let b = table.get(pid2 as usize).map(|p| p.io_share_id as usize).unwrap_or(0);
            (a, b)
        }
        KCMP_SYSVSEM => {
            let a = table.get(pid1 as usize).map(|p| p.sysvsem_share_id as usize).unwrap_or(0);
            let b = table.get(pid2 as usize).map(|p| p.sysvsem_share_id as usize).unwrap_or(0);
            (a, b)
        }
        KCMP_SIGHAND | KCMP_EPOLL_TFD => {
            let g1 = table.get(pid1 as usize).map(|p| p.tgid as usize).unwrap_or(0);
            let g2 = table.get(pid2 as usize).map(|p| p.tgid as usize).unwrap_or(0);
            (g1, g2)
        }
        _ => return -22,
    };

    if v1 == v2 { 0 } else if v1 < v2 { 1 } else { 2 }
}
