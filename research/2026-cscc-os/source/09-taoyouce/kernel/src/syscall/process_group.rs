pub(in crate::syscall) fn sys_setpgid(pid: isize, pgid: isize) -> isize {
    if pid < 0 || pgid < 0 { return -22; }
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(table) => table, None => return -3 };
    let caller = match table.current { Some(pid) => pid, None => return -3 };
    let target = if pid == 0 { caller } else { pid as usize };
    let target_sid = {
        let caller_proc = match table.get(caller) {
            Some(process) => process,
            None => return -3,
        };
        let target_proc = match table.get(target) {
            Some(process) => process,
            None => return -3,
        };
        if target != caller && !caller_proc.children.contains(&target) {
            return -3;
        }
        if target != caller && target_proc.did_exec {
            return -13;
        }
        if target_proc.sid != caller_proc.sid {
            return -1;
        }
        if target_proc.sid == target_proc.pid {
            return -1;
        }
        target_proc.sid
    };
    let new_pgid = if pgid == 0 { target } else { pgid as usize };
    if new_pgid != target {
        let group_exists = table.iter().any(|process| {
            process.sid == target_sid && process.pgid == new_pgid
        });
        if !group_exists {
            return -1;
        }
    }
    if let Some(process) = table.get_mut(target) {
        process.pgid = new_pgid;
    }
    0
}

pub(in crate::syscall) fn sys_getpgid(pid: isize) -> isize {
    if pid < 0 { return -3; }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let table = match table.as_ref() { Some(table) => table, None => return -3 };
    let target = if pid == 0 {
        match table.current { Some(pid) => pid, None => return -3 }
    } else { pid as usize };
    // process-group id 按调用者 PID namespace 翻译：pgrp leader 在祖先 ns
    // 时不可见返 0，否则按偏移翻译（pidns02）。
    let caller_root = table.current.and_then(|c| table.get(c)).map(|p| p.pidns_root).unwrap_or(0);
    match table.get(target) {
        Some(process) => super::process_info::ns_view_leader_or_zero(process.pgid, caller_root),
        None => -3,
    }
}

const PRIO_PROCESS: i32 = 0;
const PRIO_PGRP: i32 = 1;
const PRIO_USER: i32 = 2;

fn nice_clamp(prio: i32) -> i32 {
    prio.clamp(-20, 19)
}

pub(in crate::syscall) fn sys_setpriority(which: i32, who: i32, prio: i32) -> isize {
    if !(PRIO_PROCESS..=PRIO_USER).contains(&which) { return -22; }
    let new_nice = nice_clamp(prio);
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() { Some(table) => table, None => return -3 };
    let caller_pid = match table.current { Some(pid) => pid, None => return -3 };
    let (caller_uid, caller_euid, caller_pgrp) = match table.get(caller_pid) {
        Some(process) => (process.uid, process.euid, process.pgid),
        None => return -3,
    };
    let privileged = caller_euid == 0;

    let targets: alloc::vec::Vec<usize> = match which {
        PRIO_PROCESS => {
            let target = if who == 0 { caller_pid } else { who as usize };
            if table.get(target).is_some() { alloc::vec![target] } else { alloc::vec::Vec::new() }
        }
        PRIO_PGRP => {
            let group = if who == 0 { caller_pgrp } else { who as usize };
            table.pids().into_iter()
                .filter(|&pid| table.get(pid).map(|process| process.pgid == group).unwrap_or(false))
                .collect()
        }
        PRIO_USER => {
            let user = if who == 0 { caller_uid } else { who as u32 };
            table.pids().into_iter()
                .filter(|&pid| table.get(pid).map(|process| process.uid == user).unwrap_or(false))
                .collect()
        }
        _ => return -22,
    };

    let mut error: isize = -3;
    for pid in targets {
        let (target_uid, target_euid, target_nice) = match table.get(pid) {
            Some(process) => (process.uid, process.euid, process.nice),
            None => continue,
        };
        let permitted = privileged || target_uid == caller_euid || target_euid == caller_euid;
        if !permitted { error = -1; continue; }
        // Raising priority (lowering nice) without CAP_SYS_NICE is EACCES, not EPERM.
        if new_nice < target_nice && !privileged { error = -13; continue; }
        if let Some(process) = table.get_mut(pid) {
            process.nice = new_nice;
            process.sched_weight = crate::sched::process::nice_to_weight(new_nice);
        }
        if error == -3 { error = 0; }
    }
    error
}

pub(in crate::syscall) fn sys_getpriority(which: i32, who: i32) -> isize {
    if !(PRIO_PROCESS..=PRIO_USER).contains(&which) { return -22; }
    let guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_ref() { Some(table) => table, None => return -3 };
    let caller_pid = match table.current { Some(pid) => pid, None => return -3 };
    let (caller_uid, caller_pgrp) = match table.get(caller_pid) {
        Some(process) => (process.uid, process.pgid),
        None => return -3,
    };
    let highest_nice = match which {
        PRIO_PROCESS => {
            let target = if who == 0 { caller_pid } else { who as usize };
            match table.get(target) { Some(process) => Some(process.nice), None => None }
        }
        PRIO_PGRP => {
            let target_pgrp = if who == 0 { caller_pgrp } else { who as usize };
            table.pids().into_iter()
                .filter_map(|pid| table.get(pid))
                .filter(|process| process.pgid == target_pgrp)
                .map(|process| process.nice)
                .min()
        }
        PRIO_USER => {
            let target_uid = if who == 0 { caller_uid } else { who as u32 };
            table.pids().into_iter()
                .filter_map(|pid| table.get(pid))
                .filter(|process| process.uid == target_uid)
                .map(|process| process.nice)
                .min()
        }
        _ => return -22,
    };
    match highest_nice {
        Some(nice) => (20 - nice) as isize,
        None => -3,
    }
}

// ---- ioprio_set / ioprio_get — I/O scheduling priority (Linux 2.6.13+) ----

const IOPRIO_WHO_PROCESS: usize = 1;
const IOPRIO_WHO_PGRP: usize = 2;
const IOPRIO_WHO_USER: usize = 3;
const IOPRIO_CLASS_SHIFT: u16 = 13;
const IOPRIO_CLASS_MASK: u16 = 0x07;
const IOPRIO_NR_CLASSES: u16 = 4; // NONE, RT, BE, IDLE
const IOPRIO_LEVEL_MASK: u16 = (1 << IOPRIO_CLASS_SHIFT) - 1;
const IOPRIO_BE_NR: u16 = 8; // 0..7

pub(in crate::syscall) fn sys_ioprio_set(which: usize, who: usize, ioprio: usize) -> isize {
    let ioprio = ioprio as u16;
    let class = (ioprio >> IOPRIO_CLASS_SHIFT) & IOPRIO_CLASS_MASK;
    let level = ioprio & IOPRIO_LEVEL_MASK;

    // Validate class
    if class >= IOPRIO_NR_CLASSES {
        return -22; // -EINVAL
    }
    // Validate level: RT and BE allow 0..7, IDLE allows 0..7 (level
    // ignored but set succeeds), NONE requires level == 0.
    match class {
        1 | 2 | 3 => { // RT, BE, IDLE
            if level >= IOPRIO_BE_NR { return -22; }
        }
        0 => { // NONE
            if level != 0 { return -22; }
        }
        _ => return -22,
    }

    let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match table_guard.as_mut() {
        Some(t) => t,
        None => return -3,
    };
    let current = match table.current {
        Some(c) => c,
        None => return -3,
    };
    let current_proc = match table.get(current) {
        Some(process) => process,
        None => return -3,
    };
    let (caller_uid, caller_euid, caller_pgrp) = (current_proc.uid, current_proc.euid, current_proc.pgid);
    let privileged = caller_euid == 0;
    if class == 1 && !privileged {
        return -1;
    }

    let targets: alloc::vec::Vec<usize> = match which {
        IOPRIO_WHO_PROCESS => {
            let target_pid = if who == 0 { current } else { who };
            if table.get(target_pid).is_some() { alloc::vec![target_pid] } else { alloc::vec::Vec::new() }
        }
        IOPRIO_WHO_PGRP => {
            let target_pgrp = if who == 0 { caller_pgrp } else { who };
            table.pids().into_iter()
                .filter(|&pid| table.get(pid).map(|process| process.pgid == target_pgrp).unwrap_or(false))
                .collect()
        }
        IOPRIO_WHO_USER => {
            let target_uid = if who == 0 { caller_uid } else { who as u32 };
            table.pids().into_iter()
                .filter(|&pid| table.get(pid).map(|process| process.uid == target_uid).unwrap_or(false))
                .collect()
        }
        _ => return -22,
    };
    let mut error = -3;
    for pid in targets {
        let (target_uid, target_euid) = match table.get(pid) {
            Some(process) => (process.uid, process.euid),
            None => continue,
        };
        let permitted = privileged || target_uid == caller_euid || target_euid == caller_euid;
        if !permitted {
            error = -1;
            continue;
        }
        if let Some(process) = table.get_mut(pid) {
            process.ioprio = Some(ioprio);
            if error == -3 {
                error = 0;
            }
        }
    }
    error
}

pub(in crate::syscall) fn sys_ioprio_get(which: usize, who: usize) -> isize {
    let default_be = ((2u16 << IOPRIO_CLASS_SHIFT) | 4) as isize;

    let table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match table_guard.as_ref() {
        Some(t) => t,
        None => return -3,
    };
    let current = match table.current {
        Some(c) => c,
        None => return -3,
    };
    let current_proc = match table.get(current) {
        Some(process) => process,
        None => return -3,
    };
    let candidates: alloc::vec::Vec<usize> = match which {
        IOPRIO_WHO_PROCESS => {
            let target_pid = if who == 0 { current } else { who };
            if table.get(target_pid).is_some() { alloc::vec![target_pid] } else { alloc::vec::Vec::new() }
        }
        IOPRIO_WHO_PGRP => {
            let target_pgrp = if who == 0 { current_proc.pgid } else { who };
            table.pids().into_iter()
                .filter(|&pid| table.get(pid).map(|process| process.pgid == target_pgrp).unwrap_or(false))
                .collect()
        }
        IOPRIO_WHO_USER => {
            let target_uid = if who == 0 { current_proc.uid } else { who as u32 };
            table.pids().into_iter()
                .filter(|&pid| table.get(pid).map(|process| process.uid == target_uid).unwrap_or(false))
                .collect()
        }
        _ => return -22,
    };
    candidates.into_iter()
        .filter_map(|pid| table.get(pid).map(|process| process.ioprio.unwrap_or(default_be as u16) as isize))
        .min()
        .unwrap_or(-3)
}
