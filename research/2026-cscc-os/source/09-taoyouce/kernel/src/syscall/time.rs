use super::errno::SyscallError;
use super::user::{UserPtr, UserSlice};

fn read_timespec(ptr: usize) -> Result<(i64, i64), SyscallError> {
    let buf = UserSlice::<u8>::new(ptr as *const u8, 16).read_bytes()?;
    let sec = i64::from_le_bytes(buf[0..8].try_into().unwrap());
    let nsec = i64::from_le_bytes(buf[8..16].try_into().unwrap());
    Ok((sec, nsec))
}

fn write_two_i64(ptr: UserPtr<u8>, first: i64, second: i64) -> Result<(), SyscallError> {
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&first.to_le_bytes());
    buf[8..16].copy_from_slice(&second.to_le_bytes());
    UserSlice::new_mut(ptr.as_mut_u8_ptr(), buf.len()).write_bytes(&buf)
}

fn current_pid() -> Option<usize> {
    let pid = crate::sched::process::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed);
    if pid == 0 { None } else { Some(pid) }
}

fn current_nanosleep_state(pid: usize) -> Option<(u64, usize)> {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let process = table.as_ref().and_then(|t| t.get(pid))?;
    if process.waiting_for == Some(crate::sched::process::NANOSLEEP_SENTINEL) {
        Some((process.wakeup_channel as u64, process.nanosleep_rem_ptr))
    } else {
        None
    }
}

fn set_current_nanosleep_rem_ptr(pid: usize, rem: usize) {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(process) = t.get_mut(pid) {
            process.nanosleep_rem_ptr = rem;
        }
    }
}

fn clear_current_nanosleep_state(pid: usize) {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(process) = t.get_mut(pid) {
            if process.waiting_for == Some(crate::sched::process::NANOSLEEP_SENTINEL) {
                process.clear_wait_state();
            }
        }
    }
}

fn nsec_to_ticks_ceil(total_nsec: i128) -> u64 {
    if total_nsec <= 0 {
        return 0;
    }
    let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
    let total = total_nsec as u128;
    // 快路径：CLOCK_FREQ 整除 1e9（RV 10MHz→100, LA 100MHz→10）时，
    // ceil(ns*freq/1e9) == ceil(ns/(1e9/freq))，退化成对小常量的 u64 除法
    // （编译器折叠成乘倒数），彻底避开 __udivti3 的 128-bit 软件除法。
    if 1_000_000_000 % clock_freq == 0 && total <= u64::MAX as u128 {
        let divisor = 1_000_000_000 / clock_freq;
        let ns = total as u64;
        return ns / divisor + u64::from(ns % divisor != 0);
    }
    let ticks = total
        .saturating_mul(clock_freq as u128)
        .saturating_add(999_999_999)
        / 1_000_000_000;
    ticks.min(u64::MAX as u128) as u64
}

fn timespec_duration_to_ticks(sec: i64, nsec: i64) -> u64 {
    nsec_to_ticks_ceil((sec as i128) * 1_000_000_000 + nsec as i128)
}

fn write_remaining_timespec(rem: usize, deadline: u64, now: u64) -> Result<(), SyscallError> {
    if rem == 0 {
        return Ok(());
    }
    let remaining = deadline.saturating_sub(now);
    let (sec, nsec) = ticks_to_sec_nsec(remaining);
    write_two_i64(UserPtr::from_addr(rem), sec, nsec)
}

static NSDBG_BUDGET: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(120);
fn nsdbg(args: core::fmt::Arguments) {
    if !crate::cmdline::nsdbg_enabled() {
        return;
    }
    if NSDBG_BUDGET
        .fetch_update(
            core::sync::atomic::Ordering::Relaxed,
            core::sync::atomic::Ordering::Relaxed,
            |b| if b > 0 { Some(b - 1) } else { None },
        )
        .is_ok()
    {
        crate::println!("{}", args);
    }
}

fn handle_nanosleep_reentry(rem: usize, write_rem: bool) -> Option<isize> {
    let pid = current_pid()?;
    let (deadline, saved_rem) = current_nanosleep_state(pid)?;
    let now = crate::arch::asm_helpers::rdtime();
    if super::signal::pending_interrupting_signal(pid) {
        let rem_ptr = if saved_rem != 0 { saved_rem } else { rem };
        nsdbg(format_args!(
            "[NSDBG] pid={} reentry EINTR now={} deadline={} (sig pending)",
            pid, now, deadline
        ));
        clear_current_nanosleep_state(pid);
        if write_rem && write_remaining_timespec(rem_ptr, deadline, now).is_err() {
            return Some(-14);
        }
        return Some(-4);
    }
    if now >= deadline {
        nsdbg(format_args!(
            "[NSDBG] pid={} reentry DONE now={} deadline={}",
            pid, now, deadline
        ));
        clear_current_nanosleep_state(pid);
        return Some(0);
    }
    nsdbg(format_args!(
        "[NSDBG] pid={} reentry RESLEEP now={} deadline={} left={}",
        pid, now, deadline, deadline - now
    ));
    crate::sched::process::enter_wait_channel("hrtimer_nanosleep");
    crate::sched::process::suspend_current_for_nanosleep(deadline);
}

fn itdbg_enabled() -> bool {
    crate::cmdline::itdbg_enabled()
}

fn itdbg_dump_user_words(label: &str, pid: usize, center: usize) {
    if center == 0 {
        crate::println!("[itdbg] {} pid={} center=0", label, pid);
        return;
    }
    let start = center.saturating_sub(16);
    crate::println!("[itdbg] {} pid={} center={:#x} start={:#x}", label, pid, center, start);
    for off in (0..80usize).step_by(8) {
        let addr = start.saturating_add(off);
        match UserSlice::<u8>::new(addr as *const u8, 8).read_bytes() {
            Ok(buf) if buf.len() == 8 => {
                let v = u64::from_le_bytes(buf[0..8].try_into().unwrap());
                crate::println!("[itdbg]   {:#x}: {:#018x}", addr, v);
            }
            _ => {
                crate::println!("[itdbg]   {:#x}: <fault>", addr);
            }
        }
    }
}

/// nanosleep — 简单的忙等待实现
pub(in crate::syscall) fn sys_nanosleep(req: usize, rem: usize) -> isize {
    // req 指向 struct timespec { tv_sec: i64, tv_nsec: i64 }
    if let Some(ret) = handle_nanosleep_reentry(rem, true) {
        return ret;
    }
    if req == 0 { return -14; }

    // Linux validates the rem pointer upfront (access_ok). If rem is non-null
    // but points to an unmapped page, return -EFAULT before sleeping.
    if rem != 0 {
        if crate::syscall::user::UserSlice::<u8>::new(rem as *const u8, 16)
            .read_bytes()
            .is_err()
        {
            return -14; // -EFAULT
        }
    }

    let current_pid = match current_pid() {
        Some(p) => p,
        None => return 0,
    };

    // 检查 pending signals（信号唤醒 re-entry 在此捕获）
    if super::signal::pending_interrupting_signal(current_pid) {
        nsdbg(format_args!("[NSDBG] pid={} first-entry EINTR (sig pending)", current_pid));
        return -4; // -EINTR
    }

    // 首次进入：从用户态读取 timespec 并计算 deadline
    let (sec, nsec) = match read_timespec(req) {
        Ok(t) => t,
        Err(_) => return -14, // -EFAULT
    };
    nsdbg(format_args!("[NSDBG] pid={} first-entry req sec={} nsec={}", current_pid, sec, nsec));
    if sec < 0 || nsec < 0 || nsec >= 1_000_000_000 {
        return -22;
    }

    let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
    let now: u64;
    now = crate::arch::asm_helpers::rdtime();
    let ticks = timespec_duration_to_ticks(sec, nsec);
    if ticks == 0 { return 0; }
    let deadline = now + ticks;

    // hrtimer 风格的「亚 tick 短睡眠」：请求时长不足一个周期 timer tick 时，自旋
    // 等待到期后直接返回，而不是挂起到下一个 10ms 周期 tick 才被重新评估。否则
    // `usleep(1)`（1µs）实际要等满一整个 tick，紧密 `usleep` 循环慢几个数量级
    // ——madvise09 的内存压力子进程正是每次缺页后 `usleep(sleep_between_faults)`，
    // 若每次 ~10ms 则 25600 次缺页需数百秒，远超测例 30s 超时。对标 Linux：亚
    // tick 的高精度 hrtimer 精确到期，不损失一个调度 tick。
    let one_tick = clock_freq / 100;
    if ticks < one_tick {
        // 仅当本线程是当前唯一就绪者时才直接自旋到期（与 clock_nanosleep ABSTIME
        // 的亚-tick 路径同一守卫）。若已有其它就绪任务，自旋会独占单 CPU 把它们
        // 饿死——clone301 实测：子进程 usleep(100)×1000 轮询期间父进程（已被
        // checkpoint futex 唤醒、Runnable）全程拿不到 CPU，直到子进程超时退出后
        // pidfd_send_signal 才发出、命中 Zombie。挂起让度后 deadline 仍被
        // NEAREST_DEADLINE arm，定时器亚 tick 精确唤醒，语义与 Linux hrtimer 一致。
        let other_runnable = crate::sched::cpu_rq::any_queued();
        if !other_runnable {
            loop {
                if super::signal::pending_interrupting_signal(current_pid) {
                    let now2 = crate::arch::asm_helpers::rdtime();
                    if rem != 0 {
                        let _ = write_remaining_timespec(rem, deadline, now2);
                    }
                    return -4; // -EINTR
                }
                if crate::arch::asm_helpers::rdtime() >= deadline {
                    return 0;
                }
                if crate::sched::process::need_resched_pending() {
                    break;
                }
                spin_service_expired();
            }
        }
    }

    set_current_nanosleep_rem_ptr(current_pid, rem);

    crate::sched::trace::record_current_event(crate::sched::trace::EV_SLEEP_ENTER, deadline, ticks);

    // Slice 2.5 wchan：nanosleep → Linux `hrtimer_nanosleep`
    crate::sched::process::enter_wait_channel("hrtimer_nanosleep");
    // 阻塞——永不返回，到期或信号唤醒后 re-execute ecall
    crate::sched::process::suspend_current_for_nanosleep(deadline);
    // unreachable
}

/// 亚-tick 自旋睡眠期间"兼职"服务其他睡眠者的到期唤醒。
///
/// syscall 全程 SIE=0：一个线程自旋睡 ~1ms 会把其他睡眠者的定时器中断拖迟
/// 同量时间（cyclictest -t8 平均迟到 ~1.6ms 的真因——几乎总有一个线程在自旋
/// 窗口内，其余线程的 hrtimer 到期被关中断屏蔽）。在 syscall 上下文开中断走
/// kernel trap entry 不安全（实测踩栈 crash），改为自旋者本人代行 tick 的
/// 到期唤醒：无锁轮询 NEAREST_DEADLINE 原子，到期才拿 PROCESS_TABLE 锁跑
/// check_sleep_deadlines()（关中断下拿锁安全）。被唤醒者若为 RT 会置
/// need_resched，外层自旋循环据此立刻让度。
#[inline]
fn spin_service_expired() {
    for _ in 0..32 {
        core::hint::spin_loop();
    }
    let dl = crate::sched::process::nearest_deadline();
    if dl != u64::MAX && crate::arch::asm_helpers::rdtime() >= dl {
        crate::sched::process::check_sleep_deadlines();
    }
}

pub(in crate::syscall) fn sys_clock_nanosleep(clock_id: usize, flags: i32, req: usize, rem: usize) -> isize {
    const TIMER_ABSTIME: i32 = 1;
    const CLOCK_REALTIME: usize = 0;
    const CLOCK_MONOTONIC: usize = 1;
    const CLOCK_PROCESS_CPUTIME_ID: usize = 2;
    const CLOCK_THREAD_CPUTIME_ID: usize = 3;
    const CLOCK_BOOTTIME: usize = 7;
    const CLOCK_TAI: usize = 11;

    if flags & !TIMER_ABSTIME != 0 {
        return -22;
    }
    match clock_id {
        CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME | CLOCK_TAI => {}
        CLOCK_PROCESS_CPUTIME_ID | CLOCK_THREAD_CPUTIME_ID => return -95,
        _ => return -22,
    }
    if flags & TIMER_ABSTIME == 0 {
        return sys_nanosleep(req, rem);
    }
    if let Some(ret) = handle_nanosleep_reentry(0, false) {
        return ret;
    }
    if req == 0 {
        return -14;
    }

    let (sec, nsec) = match read_timespec(req) {
        Ok(t) => t,
        Err(_) => return -14,
    };
    if sec < 0 || nsec < 0 || nsec >= 1_000_000_000 {
        return -22;
    }

    let total_nsec = (sec as i128) * 1_000_000_000 + nsec as i128;
    // The absolute deadline is expressed in the caller's clock domain, which for
    // CLOCK_MONOTONIC/CLOCK_BOOTTIME includes the time-namespace offset
    // (CLONE_NEWTIME). rdtime() below is the raw hardware monotonic, so convert
    // the deadline back to raw ticks by removing the relevant offset, mirroring
    // the WALLCLOCK_OFFSET handling for the wall clocks (clock_nanosleep03).
    let (mono_off, boot_off) = crate::kernel::namespace::current_time_ns_offsets();
    let target_nsec = match clock_id {
        CLOCK_REALTIME | CLOCK_TAI => {
            total_nsec - WALLCLOCK_OFFSET_NSEC.load(core::sync::atomic::Ordering::Relaxed) as i128
        }
        CLOCK_MONOTONIC => total_nsec - mono_off as i128,
        CLOCK_BOOTTIME => total_nsec - boot_off as i128,
        _ => total_nsec,
    };
    let target = nsec_to_ticks_ceil(target_nsec);
    let now = crate::arch::asm_helpers::rdtime();
    if now >= target {
        let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut table) = *table_guard {
            if let Some(current_pid) = table.current {
                if let Some(current) = table.get_mut(current_pid) {
                    current.startup_credits = 0;
                }
            }
        }
        return 0;
    }

    // hrtimer 风格「亚 tick 短睡眠」：绝对到期点距今不足一个周期 tick 时，自旋等到
    // 期后直接返回，而非挂起 + wfi 等定时器中断。理由（cyclictest 实测）：裸 QEMU
    // TCG 下挂起走 wfi → 由宿主定时器唤醒 vCPU，这条 host-timer→vCPU 唤醒路径抖动
    // 可达数 ms（isolated 单 VM 下 cyclictest Max 仍尖到 ~5-8ms，而 icount 确定性
    // 测得 Max≈430µs 证明内核 deadline-arm 逻辑本身正确）。短睡眠改自旋则全程不
    // halt，rdtime 到点即返回，消除该宿主唤醒抖动 → 精确到期。与相对 `sys_nanosleep`
    // 的亚-tick 自旋同源。cyclictest 间隔 1~4.5ms 远小于 10ms tick，全部走此路径。
    let one_tick = crate::arch::consts::CLOCK_FREQ as u64 / 100;
    if target - now < one_tick {
        // 仅当本线程是当前唯一就绪者时才直接自旋到期（替代挂起 + wfi，消除裸 QEMU TCG
        // 下 host-timer→vCPU 唤醒的数 ms 抖动）。若已有其它就绪任务（如 cyclictest -t8
        // 的其余 RT 线程），自旋会独占单 CPU 把它们饿死 → 走正常挂起让度。
        let current_pid = match current_pid() {
            Some(p) => p,
            None => return 0,
        };
        let other_runnable = crate::sched::cpu_rq::any_queued();
        if !other_runnable {
        loop {
            if super::signal::pending_interrupting_signal(current_pid) {
                return -4; // -EINTR
            }
            if crate::arch::asm_helpers::rdtime() >= target {
                return 0;
            }
            // 有其它任务就绪（如另一 RT 线程的 armed deadline 在 timer IRQ 中到期、
            // 置了 need_resched）→ 立刻停止自旋、走正常挂起让度。否则一个自旋的 P99
            // 线程会独占单 CPU 把其余 RT 线程饿死（cyclictest -t8：实测无此让度时其余
            // 线程延迟尖到数百 ms~秒、C 计数塌成个位数）。挂起后本线程的 deadline 仍
            // 被 arm，定时器精确唤醒；单线程场景（P1）永无他者就绪 → 始终纯自旋、零抖动。
            if crate::sched::process::need_resched_pending() {
                break;
            }
            spin_service_expired();
        }
        }
    }

    crate::sched::trace::record_current_event(crate::sched::trace::EV_SLEEP_ENTER, target, target.saturating_sub(now));
    // Slice 2.5 wchan：clock_nanosleep ABSTIME → Linux `hrtimer_nanosleep`
    crate::sched::process::enter_wait_channel("hrtimer_nanosleep");
    crate::sched::process::suspend_current_for_nanosleep(target);
}

/// setitimer(103) — 设置 ITIMER_REAL/VIRTUAL/PROF；到期投递相应信号。
///
/// Slice 2.x C13 真化（禁 no-op）：
/// * ITIMER_REAL：wall-clock alarm → SIGALRM(14)。沿用 `alarm_deadline`。
/// * ITIMER_VIRTUAL：用户态 CPU 时间 → SIGVTALRM(26)。基于
///   `cpu_user_ticks` 累积，到达 `itimer_virtual_deadline` 触发；若
///   `interval` 非 0 则重设为下一段。
/// * ITIMER_PROF：用户+内核 CPU 时间 → SIGPROF(27)。同上但基线
///   `cpu_user_ticks + cpu_kernel_ticks`。
/// 这三种 deadline 在 `timer_tick`（REAL）和 `account_exit_kernel`
/// （VIRTUAL/PROF，CPU 时间结算点）真检查。
pub(in crate::syscall) fn sys_setitimer(which: i32, new_value: usize, old_value: usize) -> isize {
    const CLOCK_FREQ: u64 = crate::arch::consts::CLOCK_FREQ as u64;
    const ITIMER_REAL: i32 = 0;
    const ITIMER_VIRTUAL: i32 = 1;
    const ITIMER_PROF: i32 = 2;
    if !(ITIMER_REAL..=ITIMER_PROF).contains(&which) { return -22; }
    let debug_itimer = itdbg_enabled();

    // 在锁外读用户 buffer 避免与 PROCESS_TABLE 死锁。
    // Linux `do_setitimer`：先 copy_from_user(new)，坏指针 → EFAULT（旧实现
    // 用 .ok() 把 EFAULT 静默成“无新值”后返回 0）。
    let new_buf = if new_value != 0 {
        match UserSlice::<u8>::new(new_value as *const u8, 32).read_bytes() {
            Ok(b) => Some(b),
            Err(_) => return -14,
        }
    } else {
        None
    };

    // 解析 it_interval (off 0..16) + it_value (off 16..32) 为 ticks
    let (interval_ticks, value_ticks, disable) = if let Some(ref buf) = new_buf {
        let it_sec = i64::from_le_bytes(buf[0..8].try_into().unwrap());
        let it_usec = i64::from_le_bytes(buf[8..16].try_into().unwrap());
        let tv_sec = i64::from_le_bytes(buf[16..24].try_into().unwrap());
        let tv_usec = i64::from_le_bytes(buf[24..32].try_into().unwrap());
        let interval = (it_sec as u64).saturating_mul(CLOCK_FREQ)
            .saturating_add((it_usec as u64).saturating_mul(CLOCK_FREQ) / 1_000_000);
        let value = (tv_sec as u64).saturating_mul(CLOCK_FREQ)
            .saturating_add((tv_usec as u64).saturating_mul(CLOCK_FREQ) / 1_000_000);
        (interval, value, tv_sec == 0 && tv_usec == 0)
    } else {
        (0u64, 0u64, false)
    };

    let now = crate::arch::asm_helpers::rdtime();
    let mut old_remaining_ticks = 0u64;
    let mut old_interval_ticks = 0u64;
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() { Some(t) => t, None => return -22 };
    let pid = match t.current { Some(p) => p, None => return -22 };

    // 1) 取旧值（用于回写 old_value）
    if let Some(p) = t.get(pid) {
        match which {
            ITIMER_REAL => {
                if p.alarm_deadline != 0 && p.alarm_deadline as u64 > now {
                    old_remaining_ticks = (p.alarm_deadline as u64).saturating_sub(now);
                }
                old_interval_ticks = p.itimer_real_interval;
            }
            ITIMER_VIRTUAL => {
                if p.itimer_virtual_deadline != 0 && p.itimer_virtual_deadline > p.cpu_user_ticks {
                    old_remaining_ticks = p.itimer_virtual_deadline - p.cpu_user_ticks;
                }
                old_interval_ticks = p.itimer_virtual_interval;
            }
            ITIMER_PROF => {
                let cpu_total = p.cpu_user_ticks.saturating_add(p.cpu_kernel_ticks);
                if p.itimer_prof_deadline != 0 && p.itimer_prof_deadline > cpu_total {
                    old_remaining_ticks = p.itimer_prof_deadline - cpu_total;
                }
                old_interval_ticks = p.itimer_prof_interval;
            }
            _ => {}
        }
    }

    // 2) 写入新值
    if new_buf.is_some() {
        let mut alarm_reg: Option<u64> = None;
        if let Some(p) = t.get_mut(pid) {
            match which {
                ITIMER_REAL => {
                    if disable {
                        p.alarm_deadline = 0;
                        p.itimer_real_interval = 0;
                    } else {
                        p.alarm_deadline = now.saturating_add(value_ticks) as usize;
                        p.itimer_real_interval = interval_ticks;
                        crate::sched::process_sched::note_sleep_deadline(p.alarm_deadline as u64);
                        alarm_reg = Some(p.alarm_deadline as u64);
                    }
                }
                ITIMER_VIRTUAL => {
                    if disable {
                        p.itimer_virtual_deadline = 0;
                        p.itimer_virtual_interval = 0;
                    } else {
                        p.itimer_virtual_deadline = p.cpu_user_ticks.saturating_add(value_ticks);
                        p.itimer_virtual_interval = interval_ticks;
                    }
                }
                ITIMER_PROF => {
                    if disable {
                        p.itimer_prof_deadline = 0;
                        p.itimer_prof_interval = 0;
                    } else {
                        let cpu_total = p.cpu_user_ticks.saturating_add(p.cpu_kernel_ticks);
                        p.itimer_prof_deadline = cpu_total.saturating_add(value_ticks);
                        p.itimer_prof_interval = interval_ticks;
                    }
                }
                _ => {}
            }
        }
        if let Some(dl) = alarm_reg {
            t.register_sleep_timer(pid, dl, true);
        }
    }
    drop(table);
    // ITIMER_VIRTUAL/PROF 基于 cpu_*_ticks 到期：启用逐 syscall 精确分账，
    // 否则非精确进程的 CPU 时间只在稀疏结算点累积、到期检查迟到。
    if matches!(which, ITIMER_VIRTUAL | ITIMER_PROF) {
        crate::sched::process_sched::enable_cputime_precise_current(now);
    }

    if debug_itimer {
        crate::println!(
            "[itdbg] setitimer pid={} which={} new={:#x} old={:#x} interval_ticks={} value_ticks={} disable={}",
            pid,
            which,
            new_value,
            old_value,
            interval_ticks,
            value_ticks,
            disable,
        );
        if new_value != 0 {
            itdbg_dump_user_words("new_value", pid, new_value);
        }
        if old_value != 0 {
            itdbg_dump_user_words("old_pre", pid, old_value);
        }
    }

    // 3) 回写 old_value
    if old_value != 0 {
        let mut old = [0u8; 32];
        let int_sec = (old_interval_ticks / CLOCK_FREQ) as i64;
        let int_usec = ((old_interval_ticks % CLOCK_FREQ).saturating_mul(1_000_000) / CLOCK_FREQ) as i64;
        let sec = (old_remaining_ticks / CLOCK_FREQ) as i64;
        let usec = ((old_remaining_ticks % CLOCK_FREQ).saturating_mul(1_000_000) / CLOCK_FREQ) as i64;
        old[0..8].copy_from_slice(&int_sec.to_le_bytes());
        old[8..16].copy_from_slice(&int_usec.to_le_bytes());
        old[16..24].copy_from_slice(&sec.to_le_bytes());
        old[24..32].copy_from_slice(&usec.to_le_bytes());
        let write_ok = UserSlice::<u8>::new_mut(old_value as *mut u8, 32)
            .write_bytes(&old)
            .is_ok();
        if debug_itimer {
            crate::println!(
                "[itdbg] old_write pid={} ptr={:#x} len=32 ok={}",
                pid,
                old_value,
                write_ok,
            );
            itdbg_dump_user_words("old_post", pid, old_value);
        }
        // Linux 末尾 copy_to_user(ovalue) 失败 → EFAULT（旧实现忽略写回错误
        // 直接返回 0，导致 setitimer02 的 ovalue=(void*)-1 子项“succeeded”）。
        if !write_ok {
            return -14;
        }
    }
    0
}

/// USER_HZ：Linux 用户接口约定的 clock tick 频率（与 `getconf CLK_TCK` 一致）。
/// 所有 `clock_t` 字段（`struct tms` / `clock()` / `times()` 返回值）都按 USER_HZ
/// 计数，与底层硬件 `rdtime` 频率无关。
const USER_HZ: u64 = 100;

#[inline]
fn ticks_to_clock_t(ticks: u64) -> i64 {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    ((ticks as u128 * USER_HZ as u128) / freq as u128) as i64
}

/// `sys_times(struct tms*)` — POSIX 进程时间报告。
///
/// `struct tms { clock_t tms_utime, tms_stime, tms_cutime, tms_cstime; }`，
/// 在 RV64 / LA64 上 `clock_t = i64`，共 32 字节。
///
/// 字段语义（与 Linux `fs/times.c` 一致）：
///   * `tms_utime`  ← 当前线程组在用户态消耗的 CPU 时间（USER_HZ）
///   * `tms_stime`  ← 当前线程组在内核态消耗的 CPU 时间
///   * `tms_cutime` ← 已被 wait4 回收的子进程在用户态消耗的 CPU 时间
///   * `tms_cstime` ← 已被 wait4 回收的子进程在内核态消耗的 CPU 时间
///
/// 数据来自 `ProcessControlBlock::cpu_user_ticks / cpu_kernel_ticks /
/// cpu_children_user_ticks / cpu_children_kernel_ticks`，由
/// `account_enter_kernel / account_exit_kernel / account_reap_child` 实时累积。
///
/// 返回值按 POSIX 是"任意单调递增的 clock_t"，我们直接返回 boot 起的 uptime
/// 折算成 USER_HZ —— 这与 glibc / busybox 的预期一致。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_times(buf: *mut u8) -> isize {
    if buf.is_null() {
        return -14; // -EFAULT
    }

    // 在拿数据前先做一次进/出内核分账，确保 cpu_*_ticks 把"本次 syscall 开始
    // 之前的运行段"也算进去。
    let now = crate::arch::asm_helpers::rdtime();
    crate::sched::process::account_exit_kernel(now);
    crate::sched::process::account_enter_kernel(now);
    // cputime 消费者：此后逐 syscall 精确分账 utime/stime 拆分。
    crate::sched::process_sched::enable_cputime_precise_current(now);

    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    // utime/stime 只由本 hart 在当前任务上下文更新，可走 PCB 无锁读；
    // cutime/cstime 会由**其他 hart 在 reap 子进程时**写父 PCB，必须回表锁。
    let (utime_raw, stime_raw) = if !pcb_ptr.is_null() {
        let p = unsafe { &*pcb_ptr };
        (p.cpu_user_ticks, p.cpu_kernel_ticks)
    } else {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() {
            Some(t) => t,
            None => return -14,
        };
        let pid = match t.current { Some(p) => p, None => return -14 };
        match t.get(pid) {
            Some(p) => (p.cpu_user_ticks, p.cpu_kernel_ticks),
            None => return -14,
        }
    };
    let (cutime_raw, cstime_raw) = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() {
            Some(t) => t,
            None => return -14,
        };
        let pid = match t.current { Some(p) => p, None => return -14 };
        match t.get(pid) {
            Some(p) => (p.cpu_children_user_ticks, p.cpu_children_kernel_ticks),
            None => return -14,
        }
    };

    let mut tms_buf = [0u8; 32];
    let utime  = ticks_to_clock_t(utime_raw);
    let stime  = ticks_to_clock_t(stime_raw);
    let cutime = ticks_to_clock_t(cutime_raw);
    let cstime = ticks_to_clock_t(cstime_raw);
    tms_buf[0..8].copy_from_slice(&utime.to_le_bytes());
    tms_buf[8..16].copy_from_slice(&stime.to_le_bytes());
    tms_buf[16..24].copy_from_slice(&cutime.to_le_bytes());
    tms_buf[24..32].copy_from_slice(&cstime.to_le_bytes());

    if UserSlice::<u8>::new_mut(buf, tms_buf.len()).write_bytes(&tms_buf).is_err() {
        return -14;
    }
    ticks_to_clock_t(now) as isize
}

/// 获取当前时间 (gettimeofday)
///
/// # 参数
/// - `tv_ptr`: timeval 结构体指针，如果为 NULL 则直接返回纳秒时间戳
///
/// # 返回
/// - 如果 tv_ptr 为 NULL: 返回纳秒时间戳
/// - 如果 tv_ptr 非 NULL: 返回 0 表示成功，负数表示错误
#[allow(dead_code)]
pub(in crate::syscall) fn sys_gettimeofday(tv_ptr: *mut u8, tz_ptr: *mut u8) -> isize {
    // 真 wallclock = uptime + WALLCLOCK_OFFSET_NSEC（与 CLOCK_REALTIME 同源）。
    let uptime = crate::arch::asm_helpers::rdtime();
    let (sec0, nsec0) = ticks_to_sec_nsec(uptime);
    let off = WALLCLOCK_OFFSET_NSEC.load(core::sync::atomic::Ordering::Relaxed);
    let (sec, nsec) = add_offset_nsec(sec0, nsec0, off);
    let usec = nsec / 1000;

    let tv = UserPtr::new(tv_ptr);
    if !tv.is_null() {
        // Linux 先写 tv，copy_to_user 失败 → EFAULT。
        if write_two_i64(tv, sec, usec).is_err() {
            return -14;
        }
    }
    // timezone 已废弃但仍需写回：Linux `do_sys_settimeofday`/`gettimeofday` 在
    // tz!=NULL 时 copy_to_user 一个清零的 struct timezone（8 字节），失败 →
    // EFAULT。gettimeofday01 的 tz=(void*)-1 子项期望 EFAULT，旧实现忽略 tz
    // 直接成功。
    if !tz_ptr.is_null() {
        let zero = [0u8; 8];
        if UserSlice::<u8>::new_mut(tz_ptr, 8).write_bytes(&zero).is_err() {
            return -14;
        }
    }
    0
}

/// 当前 CLOCK_REALTIME 的整秒值（对标 Linux `ktime_get_real_seconds`）。
/// SysV IPC 的 msg_rtime/shm_atime 等与文件系统时间戳都必须取这一时钟，
/// 与 clock_gettime(CLOCK_REALTIME*) 同源同截断，否则 settimeofday 设过
/// 偏移后两者相差 ±1 秒（msgrcv01 的 msg_rtime ∈ [before, after] 检查）。
pub fn realtime_sec() -> i64 {
    let uptime = crate::arch::asm_helpers::rdtime();
    let (sec, nsec) = ticks_to_sec_nsec(uptime);
    let off = WALLCLOCK_OFFSET_NSEC.load(core::sync::atomic::Ordering::Relaxed);
    add_offset_nsec(sec, nsec, off).0
}

/// `WALLCLOCK_OFFSET_NSEC`：CLOCK_REALTIME 与单调 uptime 之间的偏移（纳秒）。
/// 由 `settimeofday` / `clock_settime` 写入；boot 时为 0，意味着 wallclock
/// 等于 uptime（POSIX 允许 epoch 在 1970-01-01 之前的任意点）。
pub static WALLCLOCK_OFFSET_NSEC: core::sync::atomic::AtomicI64 =
    core::sync::atomic::AtomicI64::new(0);

/// 启动时以 RTC 读数初始化 wallclock（对标 Linux rtc_hctosys）。
/// 仅在偏移仍为 0（从未被 settimeofday/clock_settime 写过）时生效，
/// 不覆盖用户显式设置的系统时间。
pub fn init_wallclock_from_rtc(wall_nsec: i64) {
    if WALLCLOCK_OFFSET_NSEC.load(core::sync::atomic::Ordering::Acquire) == 0 {
        set_wallclock_nsec(wall_nsec);
    }
}

fn set_wallclock_nsec(target_wall_nsec: i64) -> isize {
    let uptime = crate::arch::asm_helpers::rdtime();
    let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
    let up_nsec = ((uptime as u128) * 1_000_000_000 / clock_freq as u128) as i64;
    let offset = target_wall_nsec.saturating_sub(up_nsec);
    WALLCLOCK_OFFSET_NSEC.store(offset, core::sync::atomic::Ordering::Release);
    0
}

fn current_has_sys_time_cap() -> bool {
    const CAP_SYS_TIME: u64 = 1u64 << 25;
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        let p = unsafe { &*pcb_ptr };
        return (p.cap_effective & CAP_SYS_TIME) != 0;
    }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid).map(|p| {
            (p.cap_effective & CAP_SYS_TIME) != 0
        })))
        .unwrap_or(false)
}

pub(in crate::syscall) fn sys_settimeofday(tv: *const u8, _tz: *const u8) -> isize {
    if tv.is_null() { return -14; }
    let buf = match UserSlice::<u8>::new(tv, 16).read_bytes() {
        Ok(buf) => buf,
        Err(_) => return -14,
    };
    let sec = i64::from_le_bytes(buf[0..8].try_into().unwrap());
    let usec = i64::from_le_bytes(buf[8..16].try_into().unwrap());
    if sec < 0 || usec < 0 || usec >= 1_000_000 { return -22; }
    if !current_has_sys_time_cap() { return -1; }
    set_wallclock_nsec(sec.saturating_mul(1_000_000_000) + usec.saturating_mul(1000))
}

pub(in crate::syscall) fn sys_clock_settime(clk: i32, ts: *const u8) -> isize {
    const CLOCK_REALTIME: i32 = 0;
    const CLOCK_MONOTONIC: i32 = 1;
    if ts.is_null() { return -14; }
    if clk == CLOCK_MONOTONIC { return -22; }
    if clk != CLOCK_REALTIME && clk != 8 && clk != 11 {
        return -22;
    }
    let buf = match UserSlice::<u8>::new(ts, 16).read_bytes() {
        Ok(buf) => buf,
        Err(_) => return -14,
    };
    let sec = i64::from_le_bytes(buf[0..8].try_into().unwrap());
    let nsec = i64::from_le_bytes(buf[8..16].try_into().unwrap());
    if sec < 0 { return -22; }
    if !(0..1_000_000_000).contains(&nsec) { return -22; }
    if !current_has_sys_time_cap() { return -1; }
    set_wallclock_nsec(sec.saturating_mul(1_000_000_000) + nsec)
}

pub(in crate::syscall) fn sys_clock_adjtime(clk: i32, tx: *mut u8) -> isize {
    const CLOCK_REALTIME: i32 = 0;
    if tx.is_null() { return -14; }
    if clk != CLOCK_REALTIME { return -22; }
    let raw = match UserSlice::<u8>::new(tx as *const u8, TIMEX_SIZE).read_bytes() {
        Ok(bytes) => bytes,
        Err(_) => return -14,
    };
    let modes = match validate_timex_adjustment(&raw) {
        Ok(modes) => modes,
        Err(errno) => return errno,
    };
    let out = build_timex_status(&raw, modes);
    match UserSlice::<u8>::new_mut(tx, out.len()).write_bytes(&out) {
        Ok(_) => 0,
        Err(_) => -14,
    }
}

/// 把 boot 起的 tick 数转换为 (sec, nsec)。
fn ticks_to_sec_nsec(ticks: u64) -> (i64, i64) {
    let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
    let sec = (ticks / clock_freq) as i64;
    let nsec = ((ticks % clock_freq) * 1_000_000_000 / clock_freq) as i64;
    (sec, nsec)
}

/// 把 (sec, nsec) + 偏移合并到 (sec, nsec)，正确处理跨秒进位。
fn add_offset_nsec(sec: i64, nsec: i64, offset_nsec: i64) -> (i64, i64) {
    // Fast path in i64: fits until year ~2262 (9.2e18 ns). i64 div/rem by a
    // constant compiles to a multiply, avoiding the ~330-insn u128_div_rem
    // that dominated clock_gettime/gettimeofday hot paths under TCG.
    if let Some(total) = sec
        .checked_mul(1_000_000_000)
        .and_then(|v| v.checked_add(nsec))
        .and_then(|v| v.checked_add(offset_nsec))
    {
        return (total.div_euclid(1_000_000_000), total.rem_euclid(1_000_000_000));
    }
    let total_nsec = (sec as i128) * 1_000_000_000 + nsec as i128 + offset_nsec as i128;
    let mut sec_out = (total_nsec / 1_000_000_000) as i64;
    let mut nsec_out = (total_nsec % 1_000_000_000) as i64;
    if nsec_out < 0 {
        nsec_out += 1_000_000_000;
        sec_out -= 1;
    }
    (sec_out, nsec_out)
}

/// 获取时钟时间 (clock_gettime) —— 按 clock_id 真实分流：
///   * CLOCK_REALTIME / REALTIME_COARSE → uptime + wallclock 偏移
///   * CLOCK_MONOTONIC / MONOTONIC_RAW / MONOTONIC_COARSE / BOOTTIME → uptime
///   * CLOCK_PROCESS_CPUTIME_ID / THREAD_CPUTIME_ID → uptime（无 per-task 计费
///     基础设施，POSIX 允许返回近似值）
/// 其它 clock_id 返回 -EINVAL，与 Linux `posix_clock_get` 一致。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_clock_gettime(clock_id: usize, tp_ptr: *mut u8) -> isize {
    if crate::RTDIAG {
        use core::sync::atomic::Ordering;
        let run_pid = crate::sched::process::RT_RUN_PID.load(Ordering::Relaxed);
        if run_pid != 0
            && crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed) == run_pid
        {
            crate::sched::process::RT_RUN_PID.store(0, Ordering::Relaxed);
            let t0 = crate::sched::process::RT_RUN_TS.load(Ordering::Relaxed);
            let now = crate::arch::asm_helpers::rdtime();
            let us = now.saturating_sub(t0) * 1_000_000 / crate::arch::consts::CLOCK_FREQ as u64;
            crate::sched::process::rtstat_record(3, us);
        }
    }
    const CLOCK_REALTIME:           usize = 0;
    const CLOCK_MONOTONIC:          usize = 1;
    const CLOCK_PROCESS_CPUTIME_ID: usize = 2;
    const CLOCK_THREAD_CPUTIME_ID:  usize = 3;
    const CLOCK_MONOTONIC_RAW:      usize = 4;
    const CLOCK_REALTIME_COARSE:    usize = 5;
    const CLOCK_MONOTONIC_COARSE:   usize = 6;
    const CLOCK_BOOTTIME:           usize = 7;
    const CLOCK_REALTIME_ALARM:     usize = 8;
    const CLOCK_BOOTTIME_ALARM:     usize = 9;
    const CLOCK_TAI:                usize = 11;

    let uptime = crate::arch::asm_helpers::rdtime();
    let (sec, nsec) = ticks_to_sec_nsec(uptime);

    // ---- 动态 POSIX CPU 时钟（musl/glibc pthread_getcpuclockid / clock_*_cpuid）----
    // clockid_t 是有符号 int；musl `pthread_getcpuclockid` 编码为
    // `(-tid-1)*8 + 6`（负值）。Linux posix-timers 用位域解码：
    //   CPUCLOCK_PERTHREAD_MASK = 4 → 置位表示 per-thread，否则 per-process；
    //   CPUCLOCK_WHICH = clock & 3 → 0=PROF(用户+内核) 1=VIRT(仅用户) 2=SCHED(用户+内核)；
    //   CPUCLOCK_PID(clock) = ~(clock >> 3) → tid（per-thread）或 tgid（per-process）。
    // 内核此前对未知 clockid 一律返回 EINVAL，导致 nice05 等依赖
    // pthread_getcpuclockid 的用例 TBROK。这里解码并返回该任务真实 CPU 时间。
    let cid = clock_id as i32 as i64;
    if cid < 0 {
        let perthread = (cid & 4) != 0;
        let which = cid & 3; // 0=PROF 1=VIRT 2=SCHED
        let target = (!(cid >> 3)) as usize;
        let ticks = if perthread {
            crate::sched::process::task_cpu_ticks(target)
        } else {
            crate::sched::process::process_cpu_ticks(target)
        };
        let (utk, ktk) = match ticks {
            Some(v) => v,
            None => return -22,
        };
        // VIRT 仅计用户态；PROF/SCHED 计用户+内核态总 CPU 时间。
        let total = if which == 1 { utk } else { utk.saturating_add(ktk) };
        let (cs, cn) = ticks_to_sec_nsec(total);
        return finish_clock_gettime(tp_ptr, cs as i64, cn as i64);
    }

    let (sec_out, nsec_out) = match clock_id {
        CLOCK_REALTIME | CLOCK_REALTIME_COARSE | CLOCK_REALTIME_ALARM | CLOCK_TAI => {
            let off = WALLCLOCK_OFFSET_NSEC.load(core::sync::atomic::Ordering::Relaxed);
            add_offset_nsec(sec, nsec, off)
        }
        CLOCK_MONOTONIC | CLOCK_MONOTONIC_RAW | CLOCK_MONOTONIC_COARSE
        | CLOCK_BOOTTIME | CLOCK_BOOTTIME_ALARM => {
            // Apply the current process's time-namespace offset (CLONE_NEWTIME).
            // The host (root) time namespace has offset 0, so this is a no-op
            // there; children created after `unshare(CLONE_NEWTIME)` + writing
            // /proc/self/timens_offsets observe the shifted clock.
            let (mono_off, boot_off) =
                crate::kernel::namespace::current_time_ns_offsets();
            let off = match clock_id {
                CLOCK_BOOTTIME | CLOCK_BOOTTIME_ALARM => boot_off,
                _ => mono_off,
            };
            let (rs, rn) = add_offset_nsec(sec, nsec, off);
            (rs, rn)
        }
        CLOCK_PROCESS_CPUTIME_ID | CLOCK_THREAD_CPUTIME_ID => {
            // 用真实 per-task CPU 记账（rdtime 单位累积的用户+内核态时间）。
            // cputime 消费者：先结算在途段并启用逐 syscall 精确分账。
            {
                let now = crate::arch::asm_helpers::rdtime();
                crate::sched::process::account_exit_kernel(now);
                crate::sched::process::account_enter_kernel(now);
                crate::sched::process_sched::enable_cputime_precise_current(now);
            }
            let ticks = match crate::sched::process::current_tid_tgid() {
                Some((tid, tgid)) => {
                    if clock_id == CLOCK_THREAD_CPUTIME_ID {
                        crate::sched::process::task_cpu_ticks(tid)
                    } else {
                        crate::sched::process::process_cpu_ticks(tgid)
                    }
                }
                None => None,
            };
            match ticks {
                Some((utk, ktk)) => ticks_to_sec_nsec(utk.saturating_add(ktk)),
                // 无记账数据时按当前 uptime 近似上界返回（POSIX 允许）。
                None => (sec, nsec),
            }
        }
        _ => return -22,
    };

    let tp = UserPtr::new(tp_ptr);
    if tp.is_null() {
        let nsec_total = (sec_out as i128) * 1_000_000_000 + nsec_out as i128;
        return nsec_total as isize;
    }

    match write_two_i64(tp, sec_out, nsec_out) {
        Ok(_) => 0,
        Err(_) => -14,
    }
}

/// 把 (sec, nsec) 写回用户 timespec；tp 为空时返回总纳秒数（与主路径一致）。
fn finish_clock_gettime(tp_ptr: *mut u8, sec_out: i64, nsec_out: i64) -> isize {
    let tp = UserPtr::new(tp_ptr);
    if tp.is_null() {
        let nsec_total = (sec_out as i128) * 1_000_000_000 + nsec_out as i128;
        return nsec_total as isize;
    }
    match write_two_i64(tp, sec_out, nsec_out) {
        Ok(_) => 0,
        Err(_) => -14,
    }
}

/// 真实 `clock_getres`：以底层 rdtime 一个 tick 等价的纳秒为分辨率。
/// 对 RV64/LA64 上 10MHz 计时器，结果 = 100 ns；任何"次纳秒"上报都是假的。
pub(in crate::syscall) fn sys_clock_getres(clock_id: usize, tp_ptr: *mut u8) -> isize {
    const CLOCK_REALTIME:           usize = 0;
    const CLOCK_MONOTONIC:          usize = 1;
    const CLOCK_PROCESS_CPUTIME_ID: usize = 2;
    const CLOCK_THREAD_CPUTIME_ID:  usize = 3;
    const CLOCK_MONOTONIC_RAW:      usize = 4;
    const CLOCK_REALTIME_COARSE:    usize = 5;
    const CLOCK_MONOTONIC_COARSE:   usize = 6;
    const CLOCK_BOOTTIME:           usize = 7;
    const CLOCK_REALTIME_ALARM:     usize = 8;
    const CLOCK_BOOTTIME_ALARM:     usize = 9;
    const CLOCK_TAI:                usize = 11;
    // 动态 POSIX CPU 时钟（pthread_getcpuclockid 编码为负 clockid）也合法，
    // 其分辨率按底层 rdtime tick 上报（与 CPUTIME 类一致）。
    // Linux 编码：低 3 位 = 时钟类型，0..2 为 CPU 时钟、3 为 CLOCKFD；
    // 类型 >3（如 clockid=-1 → 类型 7）非法 → EINVAL（LTP clock_getres01）。
    let is_dynamic_cpu_clock =
        (clock_id as i32 as i64) < 0 && (clock_id as i32 as i64 & 7) <= 3;
    if !is_dynamic_cpu_clock && !matches!(
        clock_id,
        CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_PROCESS_CPUTIME_ID
        | CLOCK_THREAD_CPUTIME_ID | CLOCK_MONOTONIC_RAW
        | CLOCK_REALTIME_COARSE | CLOCK_MONOTONIC_COARSE
        | CLOCK_BOOTTIME | CLOCK_REALTIME_ALARM | CLOCK_BOOTTIME_ALARM
        | CLOCK_TAI
    ) {
        return -22;
    }
    if tp_ptr.is_null() {
        return 0;
    }
    let freq = crate::arch::consts::CLOCK_FREQ as i64;
    // 1 tick 对应的纳秒数（向上取整避免 sub-ns 上报）
    let nsec_per_tick: i64 = ((1_000_000_000 + freq - 1) / freq).max(1);
    // COARSE 类按 jiffy = 1/USER_HZ 秒报告
    let (sec, nsec) = match clock_id {
        CLOCK_REALTIME_COARSE | CLOCK_MONOTONIC_COARSE => {
            (0i64, 1_000_000_000 / USER_HZ as i64)
        }
        _ => (0i64, nsec_per_tick),
    };
    let mut buf = [0u8; 16];
    buf[0..8].copy_from_slice(&sec.to_le_bytes());
    buf[8..16].copy_from_slice(&nsec.to_le_bytes());
    match UserSlice::<u8>::new_mut(tp_ptr, buf.len()).write_bytes(&buf) {
        Ok(_) => 0,
        Err(_) => -14,
    }
}
pub(in crate::syscall) const TIMEX_SIZE: usize = 208;

pub(in crate::syscall) fn validate_timex_adjustment(raw: &[u8]) -> Result<u32, isize> {
    if raw.len() < TIMEX_SIZE {
        return Err(-14);
    }
    let modes = u32::from_ne_bytes(raw[0..4].try_into().unwrap());
    let tick = i64::from_ne_bytes(raw[88..96].try_into().unwrap());

    const ADJ_OFFSET: u32 = 0x0001;
    const ADJ_FREQUENCY: u32 = 0x0002;
    const ADJ_MAXERROR: u32 = 0x0004;
    const ADJ_ESTERROR: u32 = 0x0008;
    const ADJ_STATUS: u32 = 0x0010;
    const ADJ_TIMECONST: u32 = 0x0020;
    const ADJ_TAI: u32 = 0x0080;
    const ADJ_SETOFFSET: u32 = 0x0100;
    const ADJ_MICRO: u32 = 0x1000;
    const ADJ_NANO: u32 = 0x2000;
    const ADJ_TICK: u32 = 0x4000;
    const ADJ_OFFSET_SINGLESHOT: u32 = 0x8001;
    const ADJ_OFFSET_SS_READ: u32 = 0xa001;
    const SUPPORTED: u32 = ADJ_OFFSET
        | ADJ_FREQUENCY
        | ADJ_MAXERROR
        | ADJ_ESTERROR
        | ADJ_STATUS
        | ADJ_TIMECONST
        | ADJ_TAI
        | ADJ_SETOFFSET
        | ADJ_MICRO
        | ADJ_NANO
        | ADJ_TICK;

    let current_euid = {
        let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
            .load(core::sync::atomic::Ordering::Relaxed);
        if !pcb_ptr.is_null() {
            unsafe { (&*pcb_ptr).euid }
        } else {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table
                .as_ref()
                .and_then(|t| t.current.and_then(|pid| t.get(pid)))
                .map(|p| p.euid)
                .unwrap_or(0)
        }
    };
    if modes != 0 && modes != ADJ_OFFSET_SS_READ && current_euid != 0 {
        return Err(-1);
    }

    if modes != 0
        && modes != ADJ_OFFSET_SINGLESHOT
        && modes != ADJ_OFFSET_SS_READ
        && (modes & !SUPPORTED) != 0
    {
        return Err(-22);
    }
    if modes & ADJ_TICK != 0 && !(9_000..=11_000).contains(&tick) {
        return Err(-22);
    }
    Ok(modes)
}

/// Persistent NTP kernel state (Linux `struct ntp_data`).
/// adjtimex/clock_adjtime read/write these fields.
static TIMEX_STATE: crate::sync::SpinLock<TimexState> = crate::sync::SpinLock::new(TimexState::new());

struct TimexState {
    offset: i64,
    freq: i64,
    maxerror: i64,
    esterror: i64,
    status: i32,
    constant: i64,
    tick: i64,
    tai: i32,
}

impl TimexState {
    const fn new() -> Self {
        Self {
            offset: 0,
            freq: 0,
            maxerror: 16_000,
            esterror: 0,
            status: 0,
            constant: 2,
            tick: 10_000,
            tai: 0,
        }
    }
}

pub(in crate::syscall) fn build_timex_status(raw: &[u8], modes: u32) -> [u8; TIMEX_SIZE] {
    const ADJ_OFFSET: u32 = 0x0001;
    const ADJ_FREQUENCY: u32 = 0x0002;
    const ADJ_MAXERROR: u32 = 0x0004;
    const ADJ_ESTERROR: u32 = 0x0008;
    const ADJ_STATUS: u32 = 0x0010;
    const ADJ_TIMECONST: u32 = 0x0020;
    const ADJ_TAI: u32 = 0x0080;
    const ADJ_TICK: u32 = 0x4000;

    let mut state = TIMEX_STATE.lock();
    // Apply SET operations
    if modes & ADJ_OFFSET != 0 {
        state.offset = i64::from_ne_bytes(raw[8..16].try_into().unwrap());
    }
    if modes & ADJ_FREQUENCY != 0 {
        state.freq = i64::from_ne_bytes(raw[16..24].try_into().unwrap());
    }
    if modes & ADJ_MAXERROR != 0 {
        state.maxerror = i64::from_ne_bytes(raw[24..32].try_into().unwrap());
    }
    if modes & ADJ_ESTERROR != 0 {
        state.esterror = i64::from_ne_bytes(raw[32..40].try_into().unwrap());
    }
    if modes & ADJ_STATUS != 0 {
        state.status = i32::from_ne_bytes(raw[40..44].try_into().unwrap());
    }
    if modes & ADJ_TIMECONST != 0 {
        state.constant = i64::from_ne_bytes(raw[48..56].try_into().unwrap());
    }
    if modes & ADJ_TAI != 0 {
        state.tai = i32::from_ne_bytes(raw[160..164].try_into().unwrap());
    }
    if modes & ADJ_TICK != 0 {
        state.tick = i64::from_ne_bytes(raw[88..96].try_into().unwrap());
    }

    // Build response with current persisted state
    let mut out = [0u8; TIMEX_SIZE];
    out[0..4].copy_from_slice(&modes.to_ne_bytes());
    out[8..16].copy_from_slice(&state.offset.to_ne_bytes());
    out[16..24].copy_from_slice(&state.freq.to_ne_bytes());
    out[24..32].copy_from_slice(&state.maxerror.to_ne_bytes());
    out[32..40].copy_from_slice(&state.esterror.to_ne_bytes());
    out[40..44].copy_from_slice(&state.status.to_ne_bytes());
    out[48..56].copy_from_slice(&state.constant.to_ne_bytes());
    out[88..96].copy_from_slice(&state.tick.to_ne_bytes());
    out[160..164].copy_from_slice(&state.tai.to_ne_bytes());
    out
}

pub(in crate::syscall) fn sys_adjtimex(buf: *mut u8) -> isize {
    if buf.is_null() {
        return -14;
    }

    let raw = match UserSlice::<u8>::new(buf as *const u8, TIMEX_SIZE).read_bytes() {
        Ok(v) => v,
        Err(_) => return -14,
    };
    let modes = match validate_timex_adjustment(&raw) {
        Ok(modes) => modes,
        Err(errno) => return errno,
    };

    let out = build_timex_status(&raw, modes);
    if UserSlice::<u8>::new_mut(buf, out.len()).write_bytes(&out).is_err() {
        return -14;
    }
    0
}
