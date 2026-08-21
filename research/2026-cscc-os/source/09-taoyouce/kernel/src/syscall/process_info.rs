/// 把一个真实的 pid 转成当前进程所在 PID namespace 中的可见 pid。
/// pidns_root==0 表示全局 namespace（无偏移）。
#[inline]
fn ns_view(pid: crate::sched::process::Pid, pidns_root: crate::sched::process::Pid) -> isize {
    if pidns_root == 0 || pid <= pidns_root {
        pid as isize
    } else {
        (pid - pidns_root) as isize
    }
}

/// 把一个全局 session/process-group leader pid 翻译到调用者所在 PID
/// namespace 的可见值：若该 leader 处于调用者 ns 之外（祖先 ns，即
/// `leader <= pidns_root`），它在调用者 ns 内不可见，按 Linux
/// `pid_vnr` 返回 0；否则按偏移翻译（getsid/getpgid 用，pidns02）。
#[inline]
pub(in crate::syscall) fn ns_view_leader_or_zero(
    pid: crate::sched::process::Pid,
    pidns_root: crate::sched::process::Pid,
) -> isize {
    if pidns_root != 0 && pid <= pidns_root {
        0
    } else {
        ns_view(pid, pidns_root)
    }
}

/// kill(2) 的 pid 实参在「调用者 PID namespace 视图」下解释（Linux：pid 参数
/// 走 `find_task_by_vpid`，即调用者 ns 的 vpid）。本内核为扁平偏移模型：调用者
/// 位于子 ns（pidns_root R>0）时，ns 相对 pid 实参的全局值 = pid + R。这样祖先
/// ns 的进程（全局 pid <= R）在子 ns 内永远不可命名 → kill 自然返回 ESRCH
/// （pidns06：容器内 kill 宿主父进程必须失败）。全局 ns（R==0）恒等。
/// 仅翻译正 pid；进程组(pid<=0)/广播另行处理。
pub(in crate::syscall) fn ns_translate_incoming_pid(pid: isize) -> isize {
    if pid <= 0 {
        return pid;
    }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(t) = table.as_ref() {
        if let Some(cur) = t.current {
            if let Some(p) = t.get(cur) {
                if p.pidns_root != 0 {
                    return pid + p.pidns_root as isize;
                }
            }
        }
    }
    pid
}

/// 设置 clear_child_tid 地址
/// musl 在 __init_tp 中调用，返回当前 tid 即可
#[allow(dead_code)]
pub(in crate::syscall) fn sys_set_tid_address(tidptr: usize) -> isize {
    let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(table) = table_guard.as_mut() {
        if let Some(pid) = table.current {
            if let Some(process) = table.get_mut(pid) {
                process.clear_child_tid = tidptr;
                let root = process.pidns_root;
                return ns_view(pid, root);
            }
        }
    }
    1
}

/// getppid
pub(in crate::syscall) fn sys_getppid() -> isize {
    // Lock-free fast path via HART_CURRENT_PCB (see its safety contract):
    // smp=1 and syscalls run with interrupts disabled, so the current PCB
    // is stable — reading its identity fields needs no PROCESS_TABLE lock.
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &*pcb_ptr };
        let parent = p.parent_pid.unwrap_or(1);
        let root = p.pidns_root;
        if root != 0 && parent <= root {
            return 0;
        }
        return ns_view(parent, root);
    }
    let table_guard = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref table) = *table_guard {
        if let Some(process) = table.current() {
            let parent = process.parent_pid.unwrap_or(1);
            let root = process.pidns_root;
            // In a new pid namespace, the parent of pid 1 is itself reported as 0.
            if root != 0 && parent <= root {
                return 0;
            }
            return ns_view(parent, root);
        }
    }
    1
}

/// 获取进程 ID（返回 tgid，即线程组 leader PID）
#[allow(dead_code)]
pub(in crate::syscall) fn sys_getpid() -> isize {
    use crate::sched::process::PROCESS_TABLE;

    // Lock-free fast path via HART_CURRENT_PCB (see its safety contract).
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &*pcb_ptr };
        return ns_view(p.tgid, p.pidns_root);
    }
    let table = PROCESS_TABLE.lock();
    if let Some(table) = table.as_ref() {
        if let Some(pid) = table.current {
            if let Some(process) = table.get(pid) {
                return ns_view(process.tgid, process.pidns_root);
            }
            return pid as isize;
        }
    }
    -1
}

/// 获取线程 ID（返回实际 tid == pid，按 ns 视图）
#[allow(dead_code)]
pub(in crate::syscall) fn sys_gettid() -> isize {
    use crate::sched::process::PROCESS_TABLE;

    // Lock-free fast path via HART_CURRENT_PCB (see its safety contract).
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &*pcb_ptr };
        return ns_view(p.pid, p.pidns_root);
    }
    let table = PROCESS_TABLE.lock();
    if let Some(table) = table.as_ref() {
        if let Some(pid) = table.current {
            let root = table.get(pid).map(|p| p.pidns_root).unwrap_or(0);
            return ns_view(pid, root);
        }
    }
    -1
}
/// setsid(157) — 创建新会话
/// Linux: fails with EPERM if ANY process has pgid == caller's pid.
/// This prevents creating a session whose new group ID conflicts with
/// an existing process group. Otherwise creates new session: sid=pid, pgid=pid.
pub(in crate::syscall) fn sys_setsid() -> isize {
    use crate::sched::process::PROCESS_TABLE;
    let mut table = PROCESS_TABLE.lock();
    if let Some(table) = table.as_mut() {
        if let Some(pid) = table.current {
            let (tgid, pidns_root) = {
                let p = match table.get(pid) { Some(p) => p, None => return -1 };
                (p.tgid, p.pidns_root)
            };
            // EPERM if any process has pgid == our tgid (man 2 setsid)
            let conflict = table.iter().any(|p| p.pgid == tgid);
            if conflict {
                return -1; // -EPERM
            }
            // Create new session: sid=tgid, pgid=tgid
            if let Some(p) = table.get_mut(pid) {
                p.sid = tgid;
                p.pgid = tgid;
            }
            return ns_view(tgid, pidns_root);
        }
    }
    -1
}
