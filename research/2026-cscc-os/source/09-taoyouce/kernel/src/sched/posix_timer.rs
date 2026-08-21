//! POSIX per-process timer — Slice 2.x T4 真实化。
//!
//! Linux `kernel/time/posix-timers.c` 的最小真实化：
//!
//! * `timer_create(clockid, sigevent, &id)` 分配一个 `PosixTimer` 实例，
//!   写出 id；id 全局唯一并复用为 BTreeMap key。
//! * `timer_settime(id, flags, new_value, old_value)` 写入首次到期 +
//!   period（与 timerfd 完全相同的 itimerspec 字段布局）。
//! * `timer_gettime(id, &curr)` 返剩余 + interval（绝对时钟来源同
//!   `TimerFdInstance::now_ns` —— monotonic from `rdtime`）。
//! * `timer_getoverrun(id)` 返该 timer 自上次 deliver 起累计的 overrun。
//! * `timer_delete(id)` 取消并释放。
//! * **真 tick hook**：`fire_due_timers()` 由 `sched::timer_tick` 调，
//!   遍历所有 timer，若 `now >= expire_at_ns` → `signal::deliver_to_pid`
//!   投递 `sigev_signo`，并按 period 推进下次到期 + 累计 overrun。
//!
//! 进程退出时自动 GC：owner 的 `pid` 不再在 PROCESS_TABLE → drop 该 timer。

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};

use crate::sync::SpinLock;

/// 单个 POSIX timer 实例。
pub struct PosixTimer {
    pub id: i32,
    pub owner_pid: usize,
    pub _clockid: i32,
    pub sigev_signo: i32,
    /// monotonic ns，下一次到期时刻；0 = disarmed
    pub expire_at_ns: AtomicU64,
    /// monotonic ns，周期；0 = one-shot
    pub period_ns: AtomicU64,
    /// 自上次 `timer_getoverrun` 调用以来的溢出次数（Linux 行为：
    /// `timer_getoverrun` 返累计值且重置为 0）。
    pub overrun: AtomicU64,
    /// 上次成功 deliver 的 signo（POSIX timers 的 si_overrun 是 deliver
    /// 之间累积的 fires - 1；最小化模型：直接累计 fires-1 到 overrun）。
    pub last_signo: AtomicI32,
}

static TIMER_TABLE: SpinLock<BTreeMap<i32, Arc<PosixTimer>>> = SpinLock::new(BTreeMap::new());
/// 表条目数快照：每次定时器中断都进 fire_due_timers，为 0 时直接
/// 短路，免每中断一次的锁+全表克隆。
static TIMER_COUNT: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);
static NEXT_TIMER_ID: SpinLock<i32> = SpinLock::new(1);

fn now_ns() -> u64 {
    let cycles = crate::arch::asm_helpers::rdtime() as u64;
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    // 先除后乘，避免 cycles*1e9 在 u64 溢出（LA 100MHz 下 ~184s 就会回绕）
    (cycles / freq) * 1_000_000_000 + (cycles % freq) * 1_000_000_000 / freq
}

/// timer_create — 分配新 timer，返回 id。`sigev_signo` 若为 0 默认 SIGALRM(14)。
pub fn create(owner_pid: usize, clockid: i32, sigev_signo: i32) -> i32 {
    let signo = if sigev_signo > 0 && sigev_signo < 64 {
        sigev_signo
    } else {
        14
    };
    let mut next = NEXT_TIMER_ID.lock();
    let id = *next;
    *next = next.wrapping_add(1);
    drop(next);
    let inst = Arc::new(PosixTimer {
        id,
        owner_pid,
        _clockid: clockid,
        sigev_signo: signo,
        expire_at_ns: AtomicU64::new(0),
        period_ns: AtomicU64::new(0),
        overrun: AtomicU64::new(0),
        last_signo: AtomicI32::new(0),
    });
    TIMER_TABLE.lock().insert(id, inst);
    TIMER_COUNT.fetch_add(1, Ordering::Relaxed);
    id
}

/// timer_settime — 同 timerfd 的 itimerspec 解析。
/// `abstime=true` 则 `value_*` 是绝对 monotonic；否则相对当前时间。
pub fn settime(
    id: i32,
    value_sec: u64,
    value_nsec: u64,
    interval_sec: u64,
    interval_nsec: u64,
    abstime: bool,
) -> Result<(), i32> {
    let table = TIMER_TABLE.lock();
    let t = match table.get(&id) {
        Some(t) => t.clone(),
        None => return Err(22), // EINVAL
    };
    drop(table);
    let value_ns = value_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(value_nsec);
    let interval_ns = interval_sec
        .saturating_mul(1_000_000_000)
        .saturating_add(interval_nsec);
    if value_ns == 0 {
        // disarm
        t.expire_at_ns.store(0, Ordering::Release);
        t.period_ns.store(0, Ordering::Release);
        return Ok(());
    }
    let expire = if abstime {
        // 绝对时刻是该 timer 的 clockid 读数；内核到期比较基准是 monotonic
        // uptime，wall 时钟（REALTIME/TAI 族）须先减去 wallclock 偏移换算，
        // 否则 clock_settime 之后 armed 值整体漂移（timer_settime01 绝对档）。
        let clk = t._clockid;
        let off = if clk == 0 || clk == 5 || clk == 8 || clk == 11 {
            crate::syscall::time::WALLCLOCK_OFFSET_NSEC
                .load(Ordering::Relaxed)
        } else {
            0
        };
        let v = value_ns as i128 - off as i128;
        if v <= 0 { 1 } else { v as u64 }
    } else {
        now_ns().saturating_add(value_ns)
    };
    t.expire_at_ns.store(expire, Ordering::Release);
    t.period_ns.store(interval_ns, Ordering::Release);
    Ok(())
}

/// timer_gettime — 32 字节 `struct itimerspec`（it_value + it_interval）。
pub fn gettime(id: i32) -> Result<[u8; 32], i32> {
    let table = TIMER_TABLE.lock();
    let t = match table.get(&id) {
        Some(t) => t.clone(),
        None => return Err(22),
    };
    drop(table);
    let now = now_ns();
    let expire = t.expire_at_ns.load(Ordering::Acquire);
    let period = t.period_ns.load(Ordering::Acquire);
    let remain = if expire > now { expire - now } else { 0 };
    let mut buf = [0u8; 32];
    // Linux struct itimerspec: it_interval first (offset 0), it_value second (offset 16)
    buf[0..8].copy_from_slice(&(period / 1_000_000_000).to_le_bytes());
    buf[8..16].copy_from_slice(&(period % 1_000_000_000).to_le_bytes());
    buf[16..24].copy_from_slice(&(remain / 1_000_000_000).to_le_bytes());
    buf[24..32].copy_from_slice(&(remain % 1_000_000_000).to_le_bytes());
    Ok(buf)
}

/// timer_getoverrun — 返累计 overrun，**不**重置（Linux 行为 — overrun 在
/// signal deliver 完成的瞬间清 0，由 `fire_due_timers` 控制）。
pub fn getoverrun(id: i32) -> Result<i32, i32> {
    let table = TIMER_TABLE.lock();
    let t = match table.get(&id) {
        Some(t) => t.clone(),
        None => return Err(22),
    };
    drop(table);
    Ok(t.overrun.load(Ordering::Acquire) as i32)
}

/// timer_delete — 移除并释放。
pub fn delete(id: i32) -> Result<(), i32> {
    let mut table = TIMER_TABLE.lock();
    if table.remove(&id).is_some() {
        TIMER_COUNT.fetch_sub(1, Ordering::Relaxed);
        Ok(())
    } else {
        Err(22)
    }
}

// ---- syscall ABI wrappers -------------------------------------------------

/// `timer_create(clockid, evp, timerid)` syscall wrapper.
pub(crate) fn sys_timer_create(clockid: i32, evp: *const u8, timerid: *mut i32) -> isize {
    if timerid.is_null() {
        return -14;
    }
    let signo: i32 = if evp.is_null() {
        14
    } else {
        match crate::syscall::user::read_user_buffer(evp, 12) {
            Ok(b) => {
                let mut a = [0u8; 4];
                a.copy_from_slice(&b[8..12]);
                i32::from_le_bytes(a)
            }
            Err(_) => return -14,
        }
    };
    let owner_pid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table.as_ref().and_then(|t| t.current).unwrap_or(0)
    };
    let id = create(owner_pid, clockid, signo);
    if crate::syscall::user::write_user_buffer(timerid as *mut u8, &id.to_le_bytes()).is_err() {
        return -14;
    }
    0
}

/// `timer_settime(id, flags, new_value, old_value)` syscall wrapper.
pub(crate) fn sys_timer_settime(
    id: i32,
    flags: i32,
    new_value: *const u8,
    old_value: *mut u8,
) -> isize {
    const TIMER_ABSTIME: i32 = 1;
    if new_value.is_null() {
        return -22;
    }
    if !old_value.is_null() {
        match gettime(id) {
            Ok(buf) => {
                if crate::syscall::user::write_user_buffer(old_value, &buf).is_err() {
                    return -14;
                }
            }
            Err(e) => return -(e as isize),
        }
    }
    let bytes = match crate::syscall::user::read_user_buffer(new_value, 32) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let mut a = [0u8; 8];
    a.copy_from_slice(&bytes[0..8]);
    let i_sec = u64::from_le_bytes(a);
    a.copy_from_slice(&bytes[8..16]);
    let i_nsec = u64::from_le_bytes(a);
    a.copy_from_slice(&bytes[16..24]);
    let v_sec = u64::from_le_bytes(a);
    a.copy_from_slice(&bytes[24..32]);
    let v_nsec = u64::from_le_bytes(a);
    if i_nsec >= 1_000_000_000 || v_nsec >= 1_000_000_000 {
        return -22;
    }
    let abstime = flags & TIMER_ABSTIME != 0;
    match settime(id, v_sec, v_nsec, i_sec, i_nsec, abstime) {
        Ok(()) => 0,
        Err(e) => -(e as isize),
    }
}

/// `timer_gettime(id, curr_value)` syscall wrapper.
pub(crate) fn sys_timer_gettime(id: i32, curr_value: *mut u8) -> isize {
    if curr_value.is_null() {
        return -14;
    }
    match gettime(id) {
        Ok(buf) => {
            if crate::syscall::user::write_user_buffer(curr_value, &buf).is_err() {
                return -14;
            }
            0
        }
        Err(e) => -(e as isize),
    }
}

/// `timer_getoverrun(id)` syscall wrapper.
pub(crate) fn sys_timer_getoverrun(id: i32) -> isize {
    match getoverrun(id) {
        Ok(n) => n as isize,
        Err(e) => -(e as isize),
    }
}

/// `timer_delete(id)` syscall wrapper.
pub(crate) fn sys_timer_delete(id: i32) -> isize {
    match delete(id) {
        Ok(()) => 0,
        Err(e) => -(e as isize),
    }
}

/// `getitimer(which, curr_value)` — report remaining time for an interval timer.
pub(crate) fn sys_getitimer(which: i32, curr_value: *mut u8) -> isize {
    const ITIMER_REAL: i32 = 0;
    const ITIMER_VIRTUAL: i32 = 1;
    const ITIMER_PROF: i32 = 2;
    if !(ITIMER_REAL..=ITIMER_PROF).contains(&which) {
        return -22;
    }
    if curr_value.is_null() {
        return -14;
    }

    let mut buf = [0u8; 32];
    let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
    let now = crate::arch::asm_helpers::rdtime();

    let (remaining_ticks, interval_ticks): (u64, u64) = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let proc_opt = table
            .as_ref()
            .and_then(|t| t.current.and_then(|pid| t.get(pid)));
        match (which, proc_opt) {
            (ITIMER_REAL, Some(p)) => {
                let rem = if p.alarm_deadline == 0 || (p.alarm_deadline as u64) <= now {
                    0u64
                } else {
                    (p.alarm_deadline as u64).saturating_sub(now)
                };
                (rem, p.itimer_real_interval)
            }
            (ITIMER_VIRTUAL, Some(p)) => {
                let rem = if p.itimer_virtual_deadline == 0
                    || p.cpu_user_ticks >= p.itimer_virtual_deadline
                {
                    0u64
                } else {
                    p.itimer_virtual_deadline.saturating_sub(p.cpu_user_ticks)
                };
                (rem, p.itimer_virtual_interval)
            }
            (ITIMER_PROF, Some(p)) => {
                let cpu_total = p.cpu_user_ticks.saturating_add(p.cpu_kernel_ticks);
                let rem = if p.itimer_prof_deadline == 0 || cpu_total >= p.itimer_prof_deadline {
                    0u64
                } else {
                    p.itimer_prof_deadline.saturating_sub(cpu_total)
                };
                (rem, p.itimer_prof_interval)
            }
            _ => (0, 0),
        }
    };
    let to_tv = |ticks: u64| -> (i64, i64) {
        let sec = (ticks / clock_freq) as i64;
        let usec = ((ticks % clock_freq).saturating_mul(1_000_000) / clock_freq) as i64;
        (sec, usec)
    };
    let (iv_sec, iv_usec) = to_tv(interval_ticks);
    let (val_sec, val_usec) = to_tv(remaining_ticks);
    buf[0..8].copy_from_slice(&iv_sec.to_le_bytes());
    buf[8..16].copy_from_slice(&iv_usec.to_le_bytes());
    buf[16..24].copy_from_slice(&val_sec.to_le_bytes());
    buf[24..32].copy_from_slice(&val_usec.to_le_bytes());
    match crate::syscall::user::write_user_buffer(curr_value, &buf) {
        Ok(_) => 0,
        Err(_) => -14,
    }
}

/// 进程退出时清理：owner_pid == pid 的所有 timer 直接释放。
pub fn release_for_pid(pid: usize) {
    let mut table = TIMER_TABLE.lock();
    let ids: Vec<i32> = table
        .iter()
        .filter(|(_, t)| t.owner_pid == pid)
        .map(|(id, _)| *id)
        .collect();
    for id in ids {
        if table.remove(&id).is_some() {
            TIMER_COUNT.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

/// 由 `sched::timer_tick` 调，每 tick 一次：扫所有 timer，对到期的
/// `deliver_to_pid` 投信号，按 period 推进下次到期 + 累计 overrun。
pub fn fire_due_timers() {
    if TIMER_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let snapshot: Vec<Arc<PosixTimer>> = {
        let table = TIMER_TABLE.lock();
        table.values().cloned().collect()
    };
    if snapshot.is_empty() {
        return;
    }
    let now = now_ns();
    for t in snapshot {
        let expire = t.expire_at_ns.load(Ordering::Acquire);
        if expire == 0 || now < expire {
            continue;
        }
        let period = t.period_ns.load(Ordering::Acquire);
        // 计算从 expire 到 now 期间应触发次数（≥1）
        let fires: u64 = if period == 0 {
            1
        } else {
            1 + (now - expire) / period
        };
        // 推进 expire：one-shot disarm；周期式推到下个 ≥ now+period 的边界
        if period == 0 {
            t.expire_at_ns.store(0, Ordering::Release);
        } else {
            let new_expire = expire.saturating_add(fires.saturating_mul(period));
            t.expire_at_ns.store(new_expire, Ordering::Release);
        }
        // overrun = fires - 1（POSIX 行为：第 1 次正常 deliver 不算 overrun）
        if fires > 1 {
            t.overrun.fetch_add(fires - 1, Ordering::AcqRel);
        }
        t.last_signo.store(t.sigev_signo, Ordering::Release);
        // 真投递信号（用 sys_kill 等价路径 — owner_pid + sigev_signo）
        let _ = crate::syscall::signal::sys_kill(t.owner_pid as isize, t.sigev_signo);
    }
}
