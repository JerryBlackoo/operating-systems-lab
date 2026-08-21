//! Scheduler subsystem — per Linux kernel/sched/core.c
//!
//! Extracted from compat.rs. Implements:
//! * sched_setaffinity(2) — set CPU affinity mask
//! * sched_setattr(2) — set scheduling attributes (policy, priority, nice, DEADLINE params)
//! * sched_getattr(2) — get scheduling attributes

extern crate alloc;

// ============================================================================
// Constants — per Linux include/uapi/linux/sched.h
// ============================================================================

const SCHED_NORMAL:  u32 = 0;
const SCHED_FIFO:    u32 = 1;
const SCHED_RR:      u32 = 2;
const SCHED_BATCH:   u32 = 3;
const SCHED_IDLE:    u32 = 5;
const SCHED_DEADLINE: u32 = 6;
const SCHED_ATTR_SIZE_VER0: u32 = 48;

// ============================================================================
// sched_setaffinity(2) — per Linux kernel/sched/core.c
// ============================================================================

pub(in crate::syscall) fn sys_sched_setaffinity(
    pid: isize,
    cpusetsize: usize,
    cpuset: *const u8,
) -> isize {
    if cpuset.is_null() { return -14; }
    if cpusetsize == 0 { return -22; }
    if pid < 0 { return -22; }
    let bytes_to_read = cpusetsize.min(1024);
    let buf = match super::user::read_user_buffer(cpuset, bytes_to_read) {
        Ok(v) => v,
        Err(_) => return -14,
    };
    let mut requested_mask = 0u64;
    for (index, byte) in buf.iter().take(core::mem::size_of::<u64>()).enumerate() {
        requested_mask |= (*byte as u64) << (index * 8);
    }
    if requested_mask & crate::sched::smp::online_mask() == 0 { return -22; }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_ref() {
        Some(t) => t,
        None => return -3,
    };
    let caller_pid = match t.current {
        Some(pid) => pid,
        None => return -3,
    };
    let target_pid = if pid == 0 { caller_pid } else { pid as usize };
    let caller_euid = match t.get(caller_pid) {
        Some(process) => process.euid,
        None => return -3,
    };
    let target = match t.get(target_pid) {
        Some(process) => process,
        None => return -3,
    };
    if caller_euid != 0 && caller_euid != target.uid && caller_euid != target.euid {
        return -1;
    }
    drop(table);
    let online = crate::sched::smp::online_mask_fast();
    let allowed = requested_mask & online;
    if allowed == 0 {
        return -22;
    }
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() {
        Some(t) => t,
        None => return -3,
    };
    let on_cpu = t.get(target_pid).and_then(|p| p.on_cpu);
    let rq_cpu = t.get(target_pid).and_then(|p| p.rq_cpu);
    if let Some(process) = t.get_mut(target_pid) {
        process.cpus_allowed = allowed;
    } else {
        return -3;
    }
    if let Some(hart) = on_cpu {
        if (allowed & (1u64 << hart)) == 0 {
            crate::sched::process::set_need_resched_on(hart);
            crate::sched::smp::kick_hart(hart);
        }
    } else if let Some(cpu) = rq_cpu {
        if (allowed & (1u64 << cpu)) == 0 {
            t.unlink_ready(target_pid);
            t.enqueue_ready(target_pid);
        }
    }
    0
}

// ============================================================================
// sched_setattr(2) — per Linux kernel/sched/core.c __sched_setscheduler()
// ============================================================================

pub(in crate::syscall) fn sys_sched_setattr(pid: i32, attr: *const u8, flags: u32) -> isize {
    if attr.is_null() { return -22; }
    if flags != 0 { return -22; }
    let head = match super::user::read_user_buffer(attr, 4) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let mut user_size = u32::from_le_bytes(head[..4].try_into().unwrap());
    if user_size == 0 { user_size = SCHED_ATTR_SIZE_VER0; }
    if user_size < SCHED_ATTR_SIZE_VER0 { return -22; }
    let to_read = user_size.min(64) as usize;
    let raw = match super::user::read_user_buffer(attr, to_read) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let policy = u32::from_le_bytes(raw[4..8].try_into().unwrap());
    let nice   = i32::from_le_bytes(raw[16..20].try_into().unwrap());
    let rt_pri = u32::from_le_bytes(raw[20..24].try_into().unwrap()) as i32;

    if !(policy == SCHED_NORMAL || policy == SCHED_FIFO || policy == SCHED_RR
        || policy == SCHED_BATCH || policy == SCHED_IDLE || policy == SCHED_DEADLINE) {
        return -22;
    }
    let (dl_runtime, dl_deadline, dl_period) = if policy == SCHED_DEADLINE {
        let runtime  = u64::from_le_bytes(raw[24..32].try_into().unwrap());
        let deadline = u64::from_le_bytes(raw[32..40].try_into().unwrap());
        let period   = u64::from_le_bytes(raw[40..48].try_into().unwrap());
        if runtime == 0 || deadline == 0 { return -22; }
        // Linux: period==0 → period=deadline (implicit period)
        let effective_period = if period == 0 { deadline } else { period };
        if runtime > deadline { return -22; }
        if deadline > effective_period { return -22; }
        (runtime, deadline, effective_period)
    } else {
        (0, 0, 0)
    };
    let nice_clamped = nice.clamp(-20, 19);
    let prio_clamped = if policy == SCHED_FIFO || policy == SCHED_RR {
        rt_pri.clamp(1, 99)
    } else { 0 };

    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(t) => t, None => return -3 };
    let target = if pid == 0 {
        match table.current { Some(p) => p, None => return -3 }
    } else if pid > 0 { pid as usize } else { return -22 };
    let caller_pid = match table.current { Some(p) => p, None => return -3 };
    let caller_euid = table.get(caller_pid).map(|p| p.euid).unwrap_or(0);

    let p = match table.get_mut(target) { Some(p) => p, None => return -3 };
    if caller_euid != 0 {
        if nice_clamped < p.nice { return -1; }
        if (policy == SCHED_FIFO || policy == SCHED_RR || policy == SCHED_DEADLINE)
            && p.sched_policy == SCHED_NORMAL as i32 {
            return -1;
        }
    }
    p.sched_policy = policy as i32;
    p.sched_priority = prio_clamped;
    let promote_ready =
        (policy == SCHED_FIFO || policy == SCHED_RR || policy == SCHED_DEADLINE) && p.on_ready_queue;
    p.nice = nice_clamped;
    p.sched_weight = crate::sched::process::nice_to_weight(nice_clamped);
    p.dl_runtime = dl_runtime;
    p.dl_deadline = dl_deadline;
    p.dl_period = dl_period;
    // If switching to SCHED_DEADLINE, register with the RT scheduler
    if policy == SCHED_DEADLINE {
        crate::sched::process::note_deadline_active();
        crate::sched::deadline::register_rt_task(crate::sched::deadline::RtTaskParams {
            task_id: target as u64,
            period: dl_period,
            wcet: dl_runtime,
            relative_deadline: dl_deadline,
            priority: 0,
        });
    }
    if promote_ready {
        crate::sched::process::RT_DL_PRESENT.store(true, core::sync::atomic::Ordering::Relaxed);
        table.note_rt_ready(target);
    }
    0
}

// ============================================================================
// sched_getattr(2) — per Linux kernel/sched/core.c
// ============================================================================

pub(in crate::syscall) fn sys_sched_getattr(pid: i32, attr: *mut u8, size: u32, flags: u32) -> isize {
    if attr.is_null() || size < SCHED_ATTR_SIZE_VER0 || flags != 0 {
        return -22;
    }
    let guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_ref() { Some(t) => t, None => return -3 };
    let target = if pid == 0 {
        match table.current { Some(p) => p, None => return -3 }
    } else if pid > 0 { pid as usize } else { return -22 };
    let (policy, priority, nice, dl_runtime, dl_deadline, dl_period) = match table.get(target) {
        Some(p) => (p.sched_policy as u32, p.sched_priority as u32, p.nice,
                    p.dl_runtime, p.dl_deadline, p.dl_period),
        None => return -3,
    };

    let mut buf = [0u8; 56]; // 56 bytes for full sched_attr with deadline fields
    buf[0..4].copy_from_slice(&SCHED_ATTR_SIZE_VER0.to_le_bytes());
    buf[4..8].copy_from_slice(&policy.to_le_bytes());
    // flags at [8..16] = 0
    buf[16..20].copy_from_slice(&nice.to_le_bytes());
    buf[20..24].copy_from_slice(&priority.to_le_bytes());
    // SCHED_DEADLINE fields at offsets 24, 32, 40
    buf[24..32].copy_from_slice(&dl_runtime.to_le_bytes());
    buf[32..40].copy_from_slice(&dl_deadline.to_le_bytes());
    buf[40..48].copy_from_slice(&dl_period.to_le_bytes());
    let want = (size as usize).min(buf.len());
    match super::user::write_user_buffer(attr, &buf[..want]) {
        Ok(_) => 0,
        Err(_) => -14,
    }
}
