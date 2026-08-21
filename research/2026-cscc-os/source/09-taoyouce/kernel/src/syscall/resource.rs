use super::user::UserSlice;

/// 最大 RLIMIT 槽数（与 `ProcessControlBlock::rlimits` 长度一致）。
const RLIMIT_NLIMITS: usize = 16;
const RLIM_INFINITY: u64 = u64::MAX;

/// 对单个 RLIMIT 槽做"new_cur ≤ new_max ≤ 旧 hard"校验，并返回应当写入的
/// `(soft, hard)`。任何越界写到旧 hard 之上属于"提升硬上限"动作，需要
/// `CAP_SYS_RESOURCE`——本项目暂未实现完整 cap 体系，按"非 root 用户拒绝"。
fn validate_new_limit(old_hard: u64, new_cur: u64, new_max: u64, uid: u32) -> Result<(u64, u64), isize> {
    // POSIX：rlim_cur > rlim_max 直接 -EINVAL
    if new_cur != RLIM_INFINITY && new_max != RLIM_INFINITY && new_cur > new_max {
        return Err(-22);
    }
    // 试图把 hard 抬高（仅 root 允许）
    if new_max > old_hard {
        if uid != 0 {
            return Err(-1); // -EPERM
        }
    }
    let cur = if new_cur == RLIM_INFINITY { new_max } else { new_cur.min(new_max) };
    Ok((cur, new_max))
}

#[allow(dead_code)]
pub(in crate::syscall) fn sys_prlimit64(
    pid: usize,
    resource: usize,
    new_limit: usize,
    old_limit: *mut u8,
) -> isize {
    if resource >= RLIMIT_NLIMITS {
        return -22; // -EINVAL
    }

    // 在锁外预读 new_limit 用户缓冲，避免 PROCESS_TABLE 锁与用户内存交叉。
    let new_pair = if new_limit != 0 {
        match UserSlice::<u8>::new(new_limit as *const u8, 16).read_bytes() {
            Ok(buf) => {
                let nc = u64::from_le_bytes(buf[0..8].try_into().unwrap());
                let nm = u64::from_le_bytes(buf[8..16].try_into().unwrap());
                Some((nc, nm))
            }
            Err(_) => return -14, // -EFAULT
        }
    } else {
        None
    };

    let (target_pid, old_cur, old_hard, update_pair) = {
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        let table = match guard.as_mut() { Some(t) => t, None => return -3 };
        let caller_pid = match table.current { Some(p) => p, None => return -3 };
        let caller_uid = table.get(caller_pid).map(|p| p.uid).unwrap_or(0);

        // pid==0 → 当前进程；否则要求"同 uid 或 root"
        let target_pid = if pid == 0 { caller_pid } else { pid };
        let target_uid = match table.get(target_pid) { Some(p) => p.uid, None => return -3 };
        if target_pid != caller_pid && caller_uid != 0 && caller_uid != target_uid {
            return -1; // -EPERM
        }

        // 读旧值（NOFILE 槽以 rlimit_nofile/_max 为真值，覆盖 rlimits[7]）
        let (old_cur, old_hard) = {
            let p = match table.get(target_pid) { Some(p) => p, None => return -3 };
            if resource == 7 {
                (p.rlimit_nofile as u64, p.rlimit_nofile_max as u64)
            } else {
                p.rlimits[resource]
            }
        };

        let update_pair = if let Some((nc, nm)) = new_pair {
            // RLIMIT_NOFILE (resource 7): the kernel refuses to raise the limit
            // above /proc/sys/fs/nr_open (NR_OPEN = 1M) with EPERM, even for
            // the super-user (matches setrlimit03's EPERM case).
            const NR_OPEN: u64 = 1 << 20;
            if resource == 7 && (nc > NR_OPEN || nm > NR_OPEN) {
                return -1; // -EPERM
            }
            match validate_new_limit(old_hard, nc, nm, caller_uid) {
                Ok(v) => Some(v),
                Err(e) => return e,
            }
        } else {
            None
        };

        (target_pid, old_cur, old_hard, update_pair)
    };

    if !old_limit.is_null() {
        let mut buf = [0u8; 16];
        buf[0..8].copy_from_slice(&old_cur.to_le_bytes());
        buf[8..16].copy_from_slice(&old_hard.to_le_bytes());
        if UserSlice::<u8>::new_mut(old_limit, buf.len()).write_bytes(&buf).is_err() {
            return -14;
        }
    }

    if let Some((cur, hard)) = update_pair {
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        let table = match guard.as_mut() { Some(t) => t, None => return -3 };
        let is_current = table.current == Some(target_pid);
        let p = match table.get_mut(target_pid) { Some(p) => p, None => return -3 };
        p.rlimits[resource] = (cur, hard);
        // RLIMIT_CPU 有限值依赖 cputime 逐 syscall 结算才能及时触发 SIGXCPU。
        let enable_precise_current =
            resource == 0 && (cur != u64::MAX || hard != u64::MAX);
        if enable_precise_current {
            p.cputime_precise = true;
            crate::sched::process_sched::RLIMIT_CPU_EVER_ACTIVE
                .store(true, core::sync::atomic::Ordering::Relaxed);
        }
        if resource == 7 {
            // 同步 NOFILE 镜像：clamp 到 usize，防止 fd_table 误以为有 u64::MAX 槽。
            const NOFILE_CAP: u64 = 1 << 20; // 1M，与 Linux NR_OPEN 默认一致
            let hard_us = hard.min(NOFILE_CAP) as usize;
            let cur_us  = cur.min(hard).min(NOFILE_CAP) as usize;
            p.rlimit_nofile     = cur_us;
            p.rlimit_nofile_max = hard_us;
        }
        drop(guard);
        if enable_precise_current && is_current {
            // 目标是当前进程：本次 syscall 出口即走精确分账，须刷新
            // enter tick 防止用陈旧值结算出巨额 kernel 段。
            crate::sched::process_sched::enable_cputime_precise_current(
                crate::arch::asm_helpers::rdtime(),
            );
        }
    }
    0
}

/// `sys_getrlimit(resource, struct rlimit*)` — 老 API，等价于 prlimit64(0, …, NULL, rlim)。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_getrlimit(resource: usize, rlim: *mut u8) -> isize {
    sys_prlimit64(0, resource, 0, rlim)
}

/// `sys_setrlimit(resource, const struct rlimit*)` — 老 API，等价于
/// prlimit64(0, …, rlim, NULL)。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_setrlimit(resource: usize, rlim: *const u8) -> isize {
    sys_prlimit64(0, resource, rlim as usize, core::ptr::null_mut())
}

/// `sys_getrusage(who, struct rusage*)` — 真实化版本。
///
/// `struct rusage` 在 Linux RV64/LA64 上共 144 字节：
/// ```
/// ru_utime (16) ru_stime (16)
/// ru_maxrss (8) ru_ixrss (8) ru_idrss (8) ru_isrss (8)
/// ru_minflt (8) ru_majflt (8) ru_nswap (8)
/// ru_inblock (8) ru_oublock (8)
/// ru_msgsnd (8) ru_msgrcv (8)
/// ru_nsignals (8) ru_nvcsw (8) ru_nivcsw (8)
/// ```
///
/// 我们真实填 ru_utime / ru_stime：
///   * `RUSAGE_SELF`     →  当前线程组累计 cpu_user_ticks / cpu_kernel_ticks
///   * `RUSAGE_CHILDREN` → 父进程已 reap 子进程贡献 cpu_children_user_ticks /
///                        cpu_children_kernel_ticks
///   * `RUSAGE_THREAD`   → 与 SELF 同源（我们目前未做 per-thread CPU 统计，
///                        但同 tgid 的进程共享同一 PCB——所以等价）
///   * 其它 `who` 按 Linux 行为返回 -EINVAL。
///
/// 其它字段（maxrss / inblock / nvcsw / ...）项目里没有完整 accounting
/// 基础设施，全填 0，与 Linux 在 `CONFIG_TASK_XACCT=n` 下的行为一致。
pub(in crate::syscall) fn sys_getrusage(who: i32, usage: *mut u8) -> isize {
    const RUSAGE_SELF: i32 = 0;
    const RUSAGE_CHILDREN: i32 = -1;
    const RUSAGE_THREAD: i32 = 1;
    if usage.is_null() {
        return -14;
    }
    if !matches!(who, RUSAGE_SELF | RUSAGE_CHILDREN | RUSAGE_THREAD) {
        return -22;
    }
    // 先把当前正在运行的段时间结算进 cpu_kernel_ticks（我们身处 syscall 内）。
    let now = crate::arch::asm_helpers::rdtime();
    crate::sched::process::account_exit_kernel(now);
    crate::sched::process::account_enter_kernel(now);
    // cputime 消费者：此后逐 syscall 精确分账 utime/stime 拆分。
    crate::sched::process_sched::enable_cputime_precise_current(now);

    let (utime_raw, stime_raw, maxrss_kb) = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -14 };
        let pid = match t.current { Some(p) => p, None => return -14 };
        let p = match t.get(pid) { Some(p) => p, None => return -14 };
        match who {
            RUSAGE_CHILDREN => (
                p.cpu_children_user_ticks,
                p.cpu_children_kernel_ticks,
                p.children_maxrss_kb,
            ),
            // SELF/THREAD：ru_maxrss 取「当前驻留集」与已记录峰值的较大者。
            _ => (
                p.cpu_user_ticks,
                p.cpu_kernel_ticks,
                p.maxrss_kb.max(p.compute_rss_kb()),
            ),
        }
    };

    let (u_sec, u_usec) = ticks_to_sec_usec(utime_raw);
    let (s_sec, s_usec) = ticks_to_sec_usec(stime_raw);
    let mut buf = [0u8; 144];
    buf[0..8].copy_from_slice(&u_sec.to_le_bytes());
    buf[8..16].copy_from_slice(&u_usec.to_le_bytes());
    buf[16..24].copy_from_slice(&s_sec.to_le_bytes());
    buf[24..32].copy_from_slice(&s_usec.to_le_bytes());
    // ru_maxrss (offset 32, i64, KiB)：SELF/THREAD = 本进程驻留集峰值；
    // CHILDREN = 已 reap 子进程(及后代)的 ru_maxrss 最大值。LTP getrusage03
    // 据此校验 fork/wait 后子进程 RSS 计入父 RUSAGE_CHILDREN。
    buf[32..40].copy_from_slice(&(maxrss_kb as i64).to_le_bytes());
    // 40..144 = ixrss / minflt / majflt / inblock / oublock / nvcsw / nivcsw 等：
    // 项目暂无 per-task 计费，保持 0，与 Linux 关闭 BSDPROCESS_ACCT 时一致。
    match UserSlice::<u8>::new_mut(usage, buf.len()).write_bytes(&buf) {
        Ok(_) => 0,
        Err(_) => -14,
    }
}

#[inline]
fn ticks_to_sec_usec(ticks: u64) -> (i64, i64) {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    let sec = (ticks / freq) as i64;
    let usec = ((ticks % freq) * 1_000_000 / freq) as i64;
    (sec, usec)
}
