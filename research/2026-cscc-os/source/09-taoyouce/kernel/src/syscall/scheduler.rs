use super::user::UserSlice;

/// 让出 CPU
#[allow(dead_code)]
pub(in crate::syscall) fn sys_yield() -> isize {
    super::SYSCALL_YIELD_SENTINEL
}

fn resolve_sched_pid(table: &crate::sched::process::ProcessTable, pid: isize) -> Option<usize> {
    if pid == 0 {
        table.current
    } else if pid > 0 {
        Some(pid as usize)
    } else {
        None
    }
}

fn read_sched_priority(param: *const u8) -> Result<i32, isize> {
    if param.is_null() {
        return Err(-22); // EINVAL: Linux returns EINVAL for NULL param
    }
    let data = UserSlice::<u8>::new(param, 4).read_bytes().map_err(|_| -14isize)?;
    Ok(i32::from_le_bytes(data[..4].try_into().unwrap()))
}

pub(in crate::syscall) fn sys_sched_getscheduler(pid: isize) -> isize {
    let table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match table_guard.as_ref() {
        Some(t) => t,
        None => return -3,
    };
    let target = match resolve_sched_pid(table, pid) {
        Some(pid) => pid,
        None => return -22,
    };
    table
        .get(target)
        .map(|p| {
            // Linux: 返回值带 SCHED_RESET_ON_FORK 位（do_sched_getscheduler）。
            p.sched_policy as isize | if p.sched_reset_on_fork { 0x4000_0000 } else { 0 }
        })
        .unwrap_or(-3)
}

/// sched_rr_get_interval(pid, *tp) — 写回该进程的 RR 时间片到 `timespec`。
///
/// Linux 语义：
///   - pid < 0 → EINVAL；pid==0 表示调用者自身。
///   - 目标进程不存在 → ESRCH。
///   - tp 指针非法（NULL / 页未映射）→ EFAULT。
///   - SCHED_RR 进程 → 写回非零量子（默认 100ms）；其它策略 → 写回 0。
/// 写出的是 `__kernel_old_timespec`（64 位下两个 8 字节字段，共 16 字节），
/// 与 time64 变体在 64 位 ABI 下二进制等价。
pub(in crate::syscall) fn sys_sched_rr_get_interval(pid: isize, tp: *mut u8) -> isize {
    if pid < 0 {
        return -22; // EINVAL
    }
    let policy = {
        let table_guard = crate::sched::process::PROCESS_TABLE.lock();
        let table = match table_guard.as_ref() {
            Some(t) => t,
            None => return -3,
        };
        let target = match resolve_sched_pid(table, pid) {
            Some(p) => p,
            None => return -22,
        };
        match table.get(target) {
            Some(p) => p.sched_policy,
            None => return -3, // ESRCH
        }
    };
    // SCHED_RR(=2) → 默认 100ms 量子；其它策略写回 0。
    let (sec, nsec): (i64, i64) = if policy == 2 {
        (0, 100_000_000)
    } else {
        (0, 0)
    };
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&sec.to_le_bytes());
    buf[8..16].copy_from_slice(&nsec.to_le_bytes());
    if tp.is_null() {
        return -14; // EFAULT
    }
    match UserSlice::<u8>::new_mut(tp, 16).write_bytes(&buf) {
        Ok(_) => 0,
        Err(_) => -14, // EFAULT
    }
}

pub(in crate::syscall) fn sys_sched_getparam(pid: isize, param: *mut u8) -> isize {
    if param.is_null() {
        return -22; // EINVAL: Linux returns EINVAL for NULL param
    }
    let priority = {
        let table_guard = crate::sched::process::PROCESS_TABLE.lock();
        let table = match table_guard.as_ref() {
            Some(t) => t,
            None => return -3,
        };
        let target = match resolve_sched_pid(table, pid) {
            Some(pid) => pid,
            None => return -22,
        };
        match table.get(target) {
            Some(p) => p.sched_priority,
            None => return -3,
        }
    };
    let buf = priority.to_le_bytes();
    match UserSlice::<u8>::new_mut(param, buf.len()).write_bytes(&buf) {
        Ok(_) => 0,
        Err(_) => -14,
    }
}

pub(in crate::syscall) fn sys_sched_setparam(pid: isize, param: *const u8) -> isize {
    let priority = match read_sched_priority(param) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match table_guard.as_mut() {
        Some(t) => t,
        None => return -3,
    };
    let target = match resolve_sched_pid(table, pid) {
        Some(pid) => pid,
        None => return -22,
    };
    // Permission check: non-root caller cannot change another process's params
    // unless it has the same effective UID (Linux: check_same_owner or CAP_SYS_NICE).
    if let Some(cur_pid) = table.current {
        if cur_pid != target {
            let caller_uid = table.get(cur_pid).map(|p| p.euid).unwrap_or(0);
            let target_uid = table.get(target).map(|p| p.euid).unwrap_or(u32::MAX);
            if caller_uid != 0 && caller_uid != target_uid {
                return -1; // EPERM
            }
        }
    }
    match table.get_mut(target) {
        Some(p) => {
            // Linux validates priority against the scheduling policy:
            // SCHED_OTHER/BATCH/IDLE(0,3,5): only priority 0 is valid
            // SCHED_FIFO/RR(1,2): valid range is 1-99
            if p.sched_policy == 0 || p.sched_policy == 3 || p.sched_policy == 5 {
                if priority != 0 { return -22; } // EINVAL
                p.sched_priority = 0;
            } else {
                if priority < 1 || priority > 99 { return -22; } // EINVAL
                p.sched_priority = priority;
            }
            0
        }
        None => -3,
    }
}

pub(in crate::syscall) fn sys_sched_setscheduler(pid: isize, policy: i32, param: *const u8) -> isize {
    const SCHED_RESET_ON_FORK: i32 = 0x4000_0000;
    // Linux: 负 policy 是 EINVAL，SCHED_RESET_ON_FORK 是与 policy 同参的标志位。
    if policy < 0 {
        return -22;
    }
    let reset_on_fork = policy & SCHED_RESET_ON_FORK != 0;
    let policy = policy & !SCHED_RESET_ON_FORK;
    const SCHED_NORMAL: i32 = 0;
    const SCHED_FIFO: i32 = 1;
    const SCHED_RR: i32 = 2;
    const SCHED_BATCH: i32 = 3;
    const SCHED_IDLE: i32 = 5;

    // Linux __sched_setscheduler: copy sched_param first (EFAULT), then reject
    // an unknown policy (EINVAL). SCHED_BATCH/SCHED_IDLE are fair-class policies
    // and must be accepted (sched_setscheduler03).
    let priority = match read_sched_priority(param) {
        Ok(p) => p,
        Err(e) => return e,
    };
    if policy != SCHED_NORMAL
        && policy != SCHED_FIFO
        && policy != SCHED_RR
        && policy != SCHED_BATCH
        && policy != SCHED_IDLE
    {
        return -22; // -EINVAL: invalid scheduling policy
    }
    let is_rt = policy == SCHED_FIFO || policy == SCHED_RR;
    // sched_priority must be 0 for fair policies and within [1,99] for RT;
    // anything else is EINVAL (sched_setscheduler01 invalid-priority case).
    if is_rt {
        if !(1..=99).contains(&priority) {
            return -22;
        }
    } else if priority != 0 {
        return -22;
    }

    let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match table_guard.as_mut() {
        Some(t) => t,
        None => return -3,
    };
    let target = match resolve_sched_pid(table, pid) {
        Some(pid) => pid,
        None => return -22,
    };
    let caller_pid = match table.current {
        Some(p) => p,
        None => return -3,
    };
    let caller_euid = table.get(caller_pid).map(|p| p.euid).unwrap_or(0);

    let (old_policy, old_priority, tgid) = match table.get(target) {
        Some(p) => (p.sched_policy, p.sched_priority, p.tgid),
        None => return -3, // -ESRCH: no such task
    };
    // Unprivileged callers may not move a task into a real-time policy.
    if caller_euid != 0 && is_rt {
        return -1; // -EPERM
    }
    // Permission check: non-root cannot modify another process's scheduler
    // unless same effective UID (sched_setparam05 semantics).
    if caller_euid != 0 && caller_pid != target {
        let target_euid = table.get(target).map(|p| p.euid).unwrap_or(u32::MAX);
        if caller_euid != target_euid {
            return -1; // EPERM
        }
    }

    let new_priority = if is_rt { priority } else { 0 };
    let p = match table.get_mut(target) {
        Some(p) => p,
        None => return -3,
    };
    p.sched_policy = policy;
    p.sched_priority = new_priority;
    p.sched_reset_on_fork = reset_on_fork;
    let promote_ready = is_rt && p.on_ready_queue;
    if !is_rt {
        p.startup_credits = 0;
    }
    crate::sched::trace::focus_if_rt(tgid, policy, new_priority);
    crate::sched::trace::record_event(
        crate::sched::trace::EV_SETSCHED,
        target,
        tgid,
        p.sched_policy,
        p.sched_priority,
        ((old_policy as u64) << 32) | (old_priority as u32 as u64),
        ((policy as u64) << 32) | (new_priority as u32 as u64),
    );
    if promote_ready {
        crate::sched::process::RT_DL_PRESENT.store(true, core::sync::atomic::Ordering::Relaxed);
        table.note_rt_ready(target);
    }
    0
}

pub(in crate::syscall) fn sys_sched_get_priority(policy: i32, max: bool) -> isize {
    // Linux kernel/sched/syscalls.c：SCHED_FIFO/RR 取 [1,99]，
    // SCHED_OTHER(0)/BATCH(3)/IDLE(5)/DEADLINE(6) 的 max=min=0，其余 EINVAL。
    // 旧实现只认 0/1/2，导致 sched_get_priority_max01/min01 的 BATCH/IDLE/
    // DEADLINE 子项错返 EINVAL。
    const SCHED_OTHER: i32 = 0;
    const SCHED_FIFO: i32 = 1;
    const SCHED_RR: i32 = 2;
    const SCHED_BATCH: i32 = 3;
    const SCHED_IDLE: i32 = 5;
    const SCHED_DEADLINE: i32 = 6;
    match policy {
        SCHED_FIFO | SCHED_RR => {
            if max { 99 } else { 1 }
        }
        SCHED_OTHER | SCHED_BATCH | SCHED_IDLE | SCHED_DEADLINE => 0,
        _ => -22,
    }
}

pub(in crate::syscall) fn sys_sched_getaffinity(pid: isize, cpusetsize: usize, cpuset: *mut u8) -> isize {
    if cpuset.is_null() {
        return -14;
    }
    if cpusetsize == 0 {
        return -22;
    }
    // Linux: pid==0 means current process. Non-zero pid must exist.
    let mask = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(t) = table.as_ref() else {
            return -3;
        };
        let target = if pid == 0 {
            t.current
        } else {
            Some(pid as usize)
        };
        let Some(target) = target else {
            return -3;
        };
        let Some(p) = t.get(target) else {
            return -3; // -ESRCH
        };
        p.cpus_allowed & crate::sched::smp::online_mask_fast()
    };
    let mut buf = alloc::vec![0u8; cpusetsize];
    let mask_bytes = mask.to_le_bytes();
    let copy_len = core::cmp::min(buf.len(), mask_bytes.len());
    buf[..copy_len].copy_from_slice(&mask_bytes[..copy_len]);
    match UserSlice::<u8>::new_mut(cpuset, buf.len()).write_bytes(&buf) {
        Ok(_) => cpusetsize as isize,
        Err(_) => -14,
    }
}
