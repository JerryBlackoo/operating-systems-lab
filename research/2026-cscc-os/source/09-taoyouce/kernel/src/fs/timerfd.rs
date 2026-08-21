//! timerfd 真到期计数 — Slice 2.x T4-timerfd
//!
//! 此前 `sys_timerfd_create` 返 /dev/null fd，settime 静默 0。LTP
//! `timerfd_create0[1-9]` / `timerfd_settime0[1-3]` 测：
//!   - settime(interval + value) → 启动定时器
//!   - read(8) → 取出过期次数（u64）
//!   - 全 FAIL。
//!
//! 本模块用纯软定时器（基于 clock_gettime 的 monotonic 时间）：
//!   - `TimerFdInstance { clockid, period_ns, expire_at_ns }`
//!   - read(8): 计算 elapsed/period 得过期次数 N，更新 expire_at_ns += N*period
//!   - poll_read: 当前时间 >= expire_at_ns
//!
//! 局限：单线程定时器，不真投递 SIGALRM。但 LTP `timerfd_*` 不依赖信号。

extern crate alloc;

use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::sched::wait_queue::WaitQueue;
use crate::sync::SpinLock;

/// Global list of all live TimerFdInstance — walked by the scheduler tick.
/// Stored as Weak so dropping the last fd cleans up automatically.
static TIMERFD_REGISTRY: SpinLock<Vec<Weak<TimerFdInstance>>> =
    SpinLock::new(Vec::new());

/// 登记表条目数快照（仅增不减也只会偏大）：每次定时器中断都要进
/// poll_and_wake_expired，为 0 时直接短路，免去每中断一次的锁+扫描。
static TIMERFD_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Walked from the periodic scheduler tick. Any instance whose
/// `expire_at_ns` is now in the past gets its registered readers woken so
/// they can drain the expiration counter via `read()`.
pub fn poll_and_wake_expired() {
    if TIMERFD_COUNT.load(Ordering::Relaxed) == 0 {
        return;
    }
    let snapshot: Vec<Arc<TimerFdInstance>> = {
        let mut reg = TIMERFD_REGISTRY.lock();
        reg.retain(|w| w.strong_count() > 0);
        TIMERFD_COUNT.store(reg.len(), Ordering::Relaxed);
        reg.iter().filter_map(|w| w.upgrade()).collect()
    };
    for inst in snapshot {
        if inst.pending() {
            inst.wake_waiters();
        }
    }
}

/// timerfd_create flags
pub const TFD_CLOEXEC:  u32 = 0o2000000;
pub const TFD_NONBLOCK: u32 = 0o4000;

/// timerfd_settime flags
pub const TFD_TIMER_ABSTIME: i32 = 1;
pub const TFD_TIMER_CANCEL_ON_SET: i32 = 2;

pub struct TimerFdInstance {
    _clockid: usize,
    /// initial expiration absolute time, ns since boot
    expire_at_ns: AtomicU64,
    /// repeat interval, 0 = one-shot
    period_ns: AtomicU64,
    /// PIDs blocked in read() — woken by `poll_and_wake_expired()`.
    read_waiters: SpinLock<Vec<usize>>,
    /// Persistent epoll hooks attached by `epoll_ctl(ADD)`.
    poll_hooks: WaitQueue,
}

impl TimerFdInstance {
    pub fn new(clockid: usize) -> Arc<Self> {
        let inst = Arc::new(Self {
            _clockid: clockid,
            expire_at_ns: AtomicU64::new(0),
            period_ns:    AtomicU64::new(0),
            read_waiters: SpinLock::new(Vec::new()),
            poll_hooks:   WaitQueue::new(),
        });
        TIMERFD_REGISTRY.lock().push(Arc::downgrade(&inst));
        TIMERFD_COUNT.fetch_add(1, Ordering::Relaxed);
        inst
    }

    pub fn register_reader(&self, pid: usize) {
        let mut w = self.read_waiters.lock();
        if !w.contains(&pid) {
            w.push(pid);
        }
    }

    pub fn wake_waiters(&self) {
        let waiters: Vec<usize> = core::mem::take(&mut *self.read_waiters.lock());
        if !waiters.is_empty() {
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(table) = guard.as_mut() {
                for pid in waiters {
                    let _ = table.mark_runnable(pid, true);
                }
                table.wake_io_waiters();
            }
        }
        // epoll waiters do not sit in read_waiters; they are woken through the
        // persistent ep_poll_callback hook installed by epoll_ctl(ADD).
        self.poll_hooks.notify_poll_hooks(crate::fs::epoll::EPOLLIN);
    }

    fn now_ns() -> u64 {
        let cycles = crate::arch::asm_helpers::rdtime() as u64;
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        // 先除后乘，避免 cycles*1e9 在 u64 溢出（LA 100MHz 下 ~184s 就会回绕）
        (cycles / freq) * 1_000_000_000 + (cycles % freq) * 1_000_000_000 / freq
    }

    /// settime: `it_value` 是首次到期（如果 abstime 则为绝对，否则为相对），
    /// `it_interval` 是周期。两个都用 (sec, nsec) ns 表示。
    pub fn settime(&self, value_sec: i64, value_nsec: u64, interval_sec: u64, interval_nsec: u64, abstime: bool) {
        let now = Self::now_ns();
        // ABSTIME deadlines may be negative when the caller is in a time
        // namespace with a negative CLONE_NEWTIME offset (the absolute time in
        // the namespace clock can be < 0 on a freshly-booted host), so compute
        // in signed i128 nanoseconds. `value_nsec` is always normalised to
        // [0, 1e9) by the syscall layer.
        let value_ns: i128 = (value_sec as i128) * 1_000_000_000 + value_nsec as i128;
        let interval_ns = interval_sec.saturating_mul(1_000_000_000).saturating_add(interval_nsec);
        if value_ns == 0 {
            // disarm
            self.expire_at_ns.store(0, Ordering::Release);
            self.period_ns.store(0, Ordering::Release);
            return;
        }
        let expire = if abstime {
            // ABSTIME deadlines are expressed in the caller's time-namespace
            // clock. Translate to host monotonic by subtracting the namespace
            // offset (CLONE_NEWTIME). Host (root) namespace has offset 0.
            let (mono_off, boot_off) =
                crate::kernel::namespace::current_time_ns_offsets();
            let clock_off = match self._clockid {
                7 => boot_off,  // CLOCK_BOOTTIME
                _ => mono_off,  // CLOCK_MONOTONIC (and others treated as monotonic)
            };
            let host = value_ns - clock_off as i128;
            // A deadline already in the past fires immediately (count >= 1).
            if host <= 0 { 1 } else { host as u64 }
        } else {
            // Relative deadlines are non-negative durations.
            now.saturating_add(value_ns.max(0) as u64)
        };
        self.expire_at_ns.store(expire, Ordering::Release);
        self.period_ns.store(interval_ns, Ordering::Release);
    }

    /// gettime: return remaining time + interval as two (sec, nsec) pairs.
    /// Returns 16 bytes: tv_sec(8) + tv_nsec(8) of it_value, then interval.
    pub fn gettime(&self) -> [u8; 32] {
        let now = Self::now_ns();
        let expire = self.expire_at_ns.load(Ordering::Acquire);
        let period = self.period_ns.load(Ordering::Acquire);
        let remain = if expire > now { expire - now } else { 0 };
        let mut out = [0u8; 32];
        let v_sec  = (remain / 1_000_000_000) as i64;
        let v_nsec = (remain % 1_000_000_000) as i64;
        let p_sec  = (period / 1_000_000_000) as i64;
        let p_nsec = (period % 1_000_000_000) as i64;
        out[0..8].copy_from_slice(&v_sec.to_le_bytes());
        out[8..16].copy_from_slice(&v_nsec.to_le_bytes());
        out[16..24].copy_from_slice(&p_sec.to_le_bytes());
        out[24..32].copy_from_slice(&p_nsec.to_le_bytes());
        out
    }

    /// read: drain expiration count. Returns the count as u64 (8 bytes).
    /// Err(-EAGAIN) if not yet expired.
    pub fn read(&self) -> Result<u64, isize> {
        let now = Self::now_ns();
        let expire = self.expire_at_ns.load(Ordering::Acquire);
        let period = self.period_ns.load(Ordering::Acquire);
        if expire == 0 || now < expire {
            return Err(-11); // -EAGAIN
        }
        let mut count: u64 = 1;
        if period > 0 {
            let elapsed = now - expire;
            count = 1 + (elapsed / period);
            // advance expire to next future expiration
            self.expire_at_ns.store(expire + count * period, Ordering::Release);
        } else {
            // one-shot: disarm
            self.expire_at_ns.store(0, Ordering::Release);
        }
        Ok(count)
    }

    pub fn pending(&self) -> bool {
        let expire = self.expire_at_ns.load(Ordering::Acquire);
        expire != 0 && Self::now_ns() >= expire
    }
}

// ============================================================================
// File / Inode adapter
// ============================================================================

use crate::fs::{File, FileMode, FileStat, FileType, FsError, Inode};

pub struct TimerFdFile {
    pub instance: Arc<TimerFdInstance>,
    ino: usize,
}

impl TimerFdFile {
    pub fn new(instance: Arc<TimerFdInstance>) -> Arc<Self> {
        static NEXT_INO: AtomicUsize = AtomicUsize::new(3_000_000_000);
        Arc::new(Self {
            instance,
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
        })
    }
}

impl File for TimerFdFile {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        if buf.len() < 8 { return Err(FsError::InvalidArgument); }
        match self.instance.read() {
            Ok(n) => {
                buf[..8].copy_from_slice(&n.to_le_bytes());
                Ok(8)
            }
            Err(-11) => Err(FsError::WouldBlock),
            Err(_) => Err(FsError::InvalidArgument),
        }
    }
    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::CharDevice, size: 0,
            mode: FileMode(0o020600),
            nlink: 1, uid: 0, gid: 0, atime: 0, mtime: 0, ctime: 0,
        })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
    fn is_timerfd(&self) -> bool { true }
    fn poll_read(&self)  -> bool { self.instance.pending() }
    fn poll_write(&self) -> bool { false }
    fn register_waiter(&self, events: u32, pid: usize, _deadline: Option<u64>) {
        if events & crate::fs::READINESS_READ != 0 {
            self.instance.register_reader(pid);
        }
    }
    fn attach_poll_hook(&self, hook: alloc::sync::Arc<dyn crate::sched::wait_queue::PollWakeHook>) {
        self.instance.poll_hooks.add_poll_hook(hook);
    }
    fn detach_poll_hook(&self, hook_id: usize) {
        self.instance.poll_hooks.remove_poll_hook(hook_id);
    }
}

impl Inode for TimerFdFile {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
    fn as_timerfd(&self) -> Option<&TimerFdFile> { Some(self) }
}

// ============================================================================
// Syscall entry points — moved from syscall/fd.rs (P0 architecture refactor)
// ============================================================================

/// timerfd_create(clockid, flags) — create a real TimerFdInstance fd.
#[allow(dead_code)]
pub fn sys_timerfd_create(clockid: usize, flags: usize) -> isize {
    // Linux `timerfd_create`: clockid must be supported, flags only
    // TFD_NONBLOCK|TFD_CLOEXEC, otherwise EINVAL.
    match clockid {
        // CLOCK_REALTIME=0, MONOTONIC=1, BOOTTIME=7,
        // REALTIME_ALARM=8, BOOTTIME_ALARM=9
        0 | 1 | 7 | 8 | 9 => {}
        _ => return -22,
    }
    let valid_flags = (TFD_CLOEXEC | TFD_NONBLOCK) as usize;
    if flags & !valid_flags != 0 {
        return -22;
    }
    let instance = TimerFdInstance::new(clockid);
    let file = TimerFdFile::new(instance);
    let fd_obj = alloc::sync::Arc::new(crate::fs::FileDescriptor::new(file, flags as u32));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                return match crate::fs::fd::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        if (flags as u32) & TFD_CLOEXEC != 0 {
                            let _ = crate::fs::fd::set_cloexec(process, fd, true);
                        }
                        fd as isize
                    }
                    Err(e) => e,
                };
            }
        }
    }
    -22
}

/// timerfd_settime(fd, flags, new_value, old_value)
#[allow(dead_code)]
pub fn sys_timerfd_settime(fd: usize, flags: usize, new: *const u8, old: *mut u8) -> isize {
    if new.is_null() { return -14; }
    let new_buf = match crate::syscall::user::read_user_buffer(new, 32) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let int_sec  = u64::from_le_bytes(new_buf[0..8].try_into().unwrap());
    let int_nsec = u64::from_le_bytes(new_buf[8..16].try_into().unwrap());
    let val_sec  = i64::from_le_bytes(new_buf[16..24].try_into().unwrap());
    let val_nsec = u64::from_le_bytes(new_buf[24..32].try_into().unwrap());
    let valid_flags = TFD_TIMER_ABSTIME | TFD_TIMER_CANCEL_ON_SET;
    if (flags as i32) & !valid_flags != 0 { return -22; }
    if val_nsec >= 1_000_000_000 || int_nsec >= 1_000_000_000 { return -22; }
    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    let timerfd = match fd_obj.file.as_timerfd() {
        Some(t) => t,
        None => return -22,
    };
    let abstime = (flags as i32) & TFD_TIMER_ABSTIME != 0;
    let old_buf = timerfd.instance.gettime();
    timerfd.instance.settime(val_sec, val_nsec, int_sec, int_nsec, abstime);
    if !old.is_null() && crate::syscall::user::write_user_buffer(old, &old_buf).is_err() {
        return -14;
    }
    0
}

/// timerfd_gettime(fd, curr_value)
#[allow(dead_code)]
pub fn sys_timerfd_gettime(fd: usize, curr: *mut u8) -> isize {
    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    let timerfd = match fd_obj.file.as_timerfd() {
        Some(t) => t,
        None => return -22,
    };
    let buf = timerfd.instance.gettime();
    if curr.is_null() { return -14; }
    if crate::syscall::user::write_user_buffer(curr, &buf).is_err() { return -14; }
    0
}
