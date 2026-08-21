//! POSIX message queue subsystem — mq_open / mq_unlink / mq_timedsend /
//! mq_timedreceive / mq_notify / mq_getsetattr
//!
//! Extracted from the SysV IPC module for low-coupling architecture.
//! Implements full Linux POSIX MQ semantics: per-name queues with priority
//! ordering, blocking send/receive with absolute CLOCK_REALTIME timeouts,
//! O_NONBLOCK, mq_notify(SIGEV_SIGNAL/SIGEV_NONE), mq_getsetattr, and
//! mq_unlink with "open queues survive unlink" behaviour.

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::fs::{File, FileMode, FileStat, FileType, Inode, FsError};
use crate::sched::process::{MQ_WOKEN_SENTINEL, MQ_SLEEP_SENTINEL};
use crate::sched::wait_queue::{WaitQueue, WaitReason};
use crate::sync::SpinLock;
use crate::syscall::user::{read_user_buffer, write_user_buffer};

// ── types ───────────────────────────────────────────────────────────────────

pub struct PosixMqInner {
    pub name: String,
    pub max_msg: usize,
    pub msgsize: usize,
    pub ino: usize,
    pub creator_uid: u32,
    pub mode: u32,
    pub state: SpinLock<PosixMqState>,
}

/// Registered mq_notify target. Per Linux ipc/mqueue.c, exactly one process may
/// register for empty→non-empty transitions.
pub enum MqNotify {
    /// SIGEV_SIGNAL (signo>0) or SIGEV_NONE (signo==0): deliver a signal to pid.
    Signal { pid: usize, signo: i32 },
    /// SIGEV_THREAD (glibc/musl): the C library registers an AF_NETLINK socket
    /// (`sigev_signo` = fd) plus a 32-byte cookie (`sigev_value.sival_ptr`).
    /// On notification the kernel sets cookie[31]=NOTIFY_WOKENUP(1) and pushes
    /// it to that socket, waking the libc helper thread blocked in recv() which
    /// then calls sigev_notify_function. Per net/netlink/af_netlink.c
    /// netlink_sendskb + ipc/mqueue.c __do_notify.
    Thread { socket: Arc<crate::net::socket::SocketFile>, cookie: [u8; 32] },
}

pub struct PosixMqState {
    pub messages: VecDeque<(u32, Vec<u8>)>,
    pub notify: Option<MqNotify>,
    #[allow(dead_code)]
    pub unlinked: bool,
}

pub struct PosixMqInode {
    pub inner: Arc<PosixMqInner>,
    pub flags: AtomicU32,
}

// ── constants ───────────────────────────────────────────────────────────────

const MQ_O_NONBLOCK: u32 = 0o4000;
#[allow(dead_code)]
const MQ_O_RDONLY: u32 = 0;
const MQ_O_WRONLY: u32 = 1;
const MQ_O_RDWR: u32 = 2;
const MQ_ACCMODE: u32 = 3;

// ── Inode impl ──────────────────────────────────────────────────────────────

impl Inode for PosixMqInode {
    fn ino(&self) -> usize { self.inner.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> {
        let cur = self.inner.state.lock().messages.len();
        Ok(FileStat {
            file_type: FileType::CharDevice,
            size: cur,
            mode: FileMode(0o020600),
            nlink: 1, uid: 0, gid: 0, atime: 0, mtime: 0, ctime: 0,
        })
    }
    fn as_posix_mq(&self) -> Option<&PosixMqInode> { Some(self) }
}

// ── global state ────────────────────────────────────────────────────────────

static MQ_TABLE: SpinLock<BTreeMap<String, Arc<PosixMqInner>>> =
    SpinLock::new(BTreeMap::new());
static MQ_NEXT_INO: AtomicUsize = AtomicUsize::new(0x8000_0000);

/// /proc/sys/fs/mqueue/queues_max — per-namespace upper bound on the number of
/// POSIX message queues an unprivileged process may create (Linux default 256).
/// Privileged callers (euid 0, i.e. CAP_SYS_RESOURCE) bypass it.
pub static MQ_QUEUES_MAX: AtomicUsize = AtomicUsize::new(256);

pub fn mq_queues_max_get() -> usize { MQ_QUEUES_MAX.load(Ordering::Relaxed) }
pub fn mq_queues_max_set(v: usize) { MQ_QUEUES_MAX.store(v, Ordering::Relaxed); }

static MQ_SEND_WAITERS: SpinLock<BTreeMap<usize, WaitQueue>> =
    SpinLock::new(BTreeMap::new());
static MQ_RECV_WAITERS: SpinLock<BTreeMap<usize, WaitQueue>> =
    SpinLock::new(BTreeMap::new());

// ── helpers ─────────────────────────────────────────────────────────────────

fn mq_alloc_ino() -> usize {
    MQ_NEXT_INO.fetch_add(1, Ordering::Relaxed)
}

fn current_pid_locked() -> usize {
    crate::ipc::sysv::current_pid_locked()
}

fn parse_mq_abstime(ts: *const u8) -> Result<Option<u64>, i32> {
    if ts.is_null() { return Ok(None); }
    let buf = read_user_buffer(ts, 16).map_err(|_| -14)?;
    let mut a = [0u8; 8];
    a.copy_from_slice(&buf[0..8]);  let sec  = i64::from_le_bytes(a);
    a.copy_from_slice(&buf[8..16]); let nsec = i64::from_le_bytes(a);
    if nsec < 0 || nsec >= 1_000_000_000 { return Err(-22); }
    if sec < 0 { return Err(-22); }
    let real_ns: i128 = (sec as i128) * 1_000_000_000 + (nsec as i128);
    let off_ns = crate::syscall::time::WALLCLOCK_OFFSET_NSEC
        .load(Ordering::Relaxed) as i128;
    let up_ns = real_ns - off_ns;
    if up_ns <= 0 { return Ok(Some(0)); }
    let freq = crate::arch::consts::CLOCK_FREQ as i128;
    let deadline_ticks = (up_ns * freq) / 1_000_000_000;
    Ok(Some(deadline_ticks as u64))
}

fn mq_wake_one(map: &SpinLock<BTreeMap<usize, WaitQueue>>, ino: usize) {
    let target = {
        let mut tbl = map.lock();
        let entry = tbl.get(&ino).and_then(|q| q.dequeue_one());
        if let Some(q) = tbl.get(&ino) {
            if q.is_empty() { tbl.remove(&ino); }
        }
        entry
    };
    let pid = match target { Some(e) => e.pid, None => return };
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() { Some(t) => t, None => return };
    let was_sleeping = t.get(pid).map_or(false, |p| {
        p.state == crate::sched::process::ProcessState::Sleeping
    });
    if !was_sleeping { return; }
    if let Some(p) = t.get_mut(pid) {
        p.waiting_for = Some(MQ_WOKEN_SENTINEL);
    }
    t.mark_runnable(pid, false);
}

fn mq_enqueue_self(
    map: &SpinLock<BTreeMap<usize, WaitQueue>>,
    ino: usize,
    pid: usize,
    deadline: Option<u64>,
    is_send: bool,
) {
    let mut tbl = map.lock();
    let q = tbl.entry(ino).or_insert_with(WaitQueue::new);
    let reason = WaitReason::Custom(if is_send { 0 } else { 1 });
    q.enqueue_pid(pid, reason, deadline);
}

fn mq_remove_self(map: &SpinLock<BTreeMap<usize, WaitQueue>>, ino: usize, pid: usize) {
    let mut tbl = map.lock();
    if let Some(q) = tbl.get(&ino) {
        q.remove_pid(pid);
        if q.is_empty() { tbl.remove(&ino); }
    }
}

fn mq_fd_obj(mqdes: i32) -> Result<Arc<crate::fs::FileDescriptor>, isize> {
    if mqdes < 0 { return Err(-9); }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_ref().ok_or(-9isize)?;
    let pid = t.current.ok_or(-9isize)?;
    let proc = t.get_fd_process(pid).ok_or(-9isize)?;
    let fd_obj = crate::fs::fd::get(proc, mqdes as usize).ok_or(-9isize)?;
    if fd_obj.file.as_posix_mq().is_none() { return Err(-9); }
    Ok(fd_obj)
}

fn mq_check_reentry(ino: usize, is_send: bool) -> Option<isize> {
    let pid = current_pid_locked();
    if pid == 0 { return Some(-9); }
    let map = if is_send { &MQ_SEND_WAITERS } else { &MQ_RECV_WAITERS };
    mq_remove_self(map, ino, pid);

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_mut()?;
    let pending_sig = crate::kernel::signal::pending_interrupting_signal_for_process(t, pid, 0);
    let p = t.get_mut(pid)?;

    let deadline = if p.wakeup_channel > 1 { Some(p.wakeup_channel as u64) } else { None };
    let woken_by_peer = p.waiting_for == Some(MQ_WOKEN_SENTINEL);
    let sleeping_natural = p.waiting_for == Some(MQ_SLEEP_SENTINEL);

    if sleeping_natural {
        let now = crate::arch::asm_helpers::rdtime();
        if let Some(d) = deadline { if now >= d {
            p.clear_wait_state();
            return Some(-110);
        } }
    }
    if pending_sig {
        p.clear_wait_state();
        return Some(-4);
    }
    if let Some(dl) = deadline {
        let now = crate::arch::asm_helpers::rdtime();
        if now >= dl {
            p.clear_wait_state();
            return Some(-110);
        }
    }
    if woken_by_peer {
        p.waiting_for = None;
        p.wait_channel = None;
    }
    None
}

fn mq_sleep_self(ino: usize, deadline: Option<u64>, is_send: bool) -> ! {
    let pid = current_pid_locked();
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            if let Some(p) = t.get_mut(pid) {
                p.wakeup_channel = deadline.unwrap_or(0) as usize;
            }
        }
    }
    let map = if is_send { &MQ_SEND_WAITERS } else { &MQ_RECV_WAITERS };
    mq_enqueue_self(map, ino, pid, deadline, is_send);

    crate::sched::process::enter_wait_channel(if is_send { "mqueue_wait_send" } else { "mqueue_wait_recv" });
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            let wc = deadline.unwrap_or(0) as usize;
            t.mark_sleeping(pid, Some(MQ_SLEEP_SENTINEL), wc);
        }
    }
    crate::sched::process::suspend_current_and_run_next();
    loop { core::hint::spin_loop(); }
}

// ── syscall implementations ─────────────────────────────────────────────────

pub(in crate::syscall) fn sys_mq_open(name: *const u8, oflag: i32, mode: u32, attr: *const u8) -> isize {
    let name_str = match crate::syscall::user::read_c_string(name) {
        Ok(s) => s,
        Err(_) => return -14,
    };
    const O_CREAT:    i32 = 0o100;
    const O_EXCL:     i32 = 0o200;
    const O_CLOEXEC:  i32 = 0o2000000;
    const O_NONBLOCK: i32 = 0o4000;

    if name_str.is_empty() { return -22; }
    let name_part = name_str.strip_prefix('/').unwrap_or(&name_str);
    if name_part.is_empty() { return -22; }
    if name_part.contains('/') { return -22; }
    if name_part.len() > 255 { return -36; }
    let norm_name = if name_str.starts_with('/') {
        name_str.clone()
    } else {
        let mut s = String::from("/");
        s.push_str(&name_str);
        s
    };

    let access = (oflag as u32) & MQ_ACCMODE;
    if access > MQ_O_RDWR { return -22; }

    let euid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table.as_ref().and_then(|t| t.current.and_then(|pid| t.get(pid).map(|p| p.euid))).unwrap_or(u32::MAX)
    };

    let (inner, created) = {
        let mut tbl = MQ_TABLE.lock();
        if let Some(existing) = tbl.get(&norm_name) {
            if oflag & O_CREAT != 0 && oflag & O_EXCL != 0 { return -17; }
            // POSIX: opening an existing queue is an access check against the
            // queue's mode/owner for the requested access mode (EACCES on deny).
            let granted = if euid == 0 {
                0o7
            } else if euid == existing.creator_uid {
                (existing.mode >> 6) & 0o7
            } else {
                existing.mode & 0o7
            };
            let need = match access {
                MQ_O_RDONLY => 0o4,
                MQ_O_WRONLY => 0o2,
                _ => 0o6,
            };
            if granted & need != need { return -13; }
            (existing.clone(), false)
        } else if oflag & O_CREAT != 0 {
            // queues_max: unprivileged callers cannot exceed the per-namespace
            // cap (Linux ENOSPC); euid 0 (CAP_SYS_RESOURCE) bypasses it.
            if euid != 0 && tbl.len() >= mq_queues_max_get() { return -28; }
            let (max_msg, msgsize) = if attr.is_null() {
                (10usize, 8192usize)
            } else {
                match read_user_buffer(attr, 32) {
                    Ok(b) => {
                        let mut a = [0u8; 8];
                        a.copy_from_slice(&b[8..16]);  let mm = i64::from_le_bytes(a);
                        a.copy_from_slice(&b[16..24]); let ms = i64::from_le_bytes(a);
                        if mm <= 0 || ms <= 0 { return -22; }
                        let mm = mm as usize;
                        let ms = ms as usize;
                        if euid != 0 {
                            if mm > 10 || ms > 8192 { return -22; }
                        } else {
                            if mm > 65536 || ms > 16777216 { return -22; }
                        }
                        (mm, ms)
                    }
                    Err(_) => return -14,
                }
            };
            let creator_uid = if euid == u32::MAX { 0 } else { euid };
            let inner = Arc::new(PosixMqInner {
                name: norm_name.clone(),
                max_msg,
                msgsize,
                ino: mq_alloc_ino(),
                creator_uid,
                mode,
                state: SpinLock::new(PosixMqState {
                    messages: VecDeque::new(),
                    notify: None,
                    unlinked: false,
                }),
            });
            tbl.insert(norm_name.clone(), inner.clone());
            (inner, true)
        } else {
            return -2;
        }
    };

    let mut flags = access;
    if oflag & O_NONBLOCK != 0 { flags |= MQ_O_NONBLOCK; }
    let inode = Arc::new(PosixMqInode {
        inner,
        flags: AtomicU32::new(flags),
    });
    let path = {
        let mut s = String::from("mqueue:");
        s.push_str(&norm_name);
        Some(s)
    };
    let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
        inode,
        access,
        path,
    ));
    let cloexec = oflag & O_CLOEXEC != 0;
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() { Some(t) => t, None => return -23 };
    let pid = match t.current { Some(p) => p, None => return -23 };
    let process = match t.get_fd_process_mut(pid) { Some(p) => p, None => return -23 };
    match crate::fs::fd::alloc(process, fd_obj, 0, true) {
        Ok(fd) => {
            if cloexec {
                let _ = crate::fs::fd::set_cloexec(process, fd, true);
            }
            fd as isize
        }
        Err(e) => {
            // The fd table was full: a queue we just created must not leak into
            // the namespace (Linux unwinds the new node on open failure).
            if created {
                drop(table);
                MQ_TABLE.lock().remove(&norm_name);
            }
            e
        }
    }
}

pub(in crate::syscall) fn sys_mq_unlink(name: *const u8) -> isize {
    let s = match crate::syscall::user::read_c_string(name) {
        Ok(s) => s,
        Err(_) => return -14,
    };
    let s = if s.starts_with('/') { s } else { let mut n = String::from("/"); n.push_str(&s); n };
    let name_part = &s[1..];
    if name_part.len() > 255 { return -36; }
    let euid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -3 };
        let pid = match t.current { Some(p) => p, None => return -3 };
        match t.get(pid) { Some(p) => p.euid, None => return -3 }
    };
    let mut tbl = MQ_TABLE.lock();
    if let Some(inner) = tbl.get(&s) {
        let granted = if euid == 0 {
            0o7
        } else if euid == inner.creator_uid {
            (inner.mode >> 6) & 0o7
        } else {
            inner.mode & 0o7
        };
        if granted & 0o2 == 0 { return -13; }
    }
    match tbl.remove(&s) {
        Some(inner) => {
            inner.state.lock().unlinked = true;
            0
        }
        None => -2,
    }
}

pub(in crate::syscall) fn sys_mq_timedsend(mqdes: i32, msg: *const u8, len: usize, prio: u32, ts: *const u8) -> isize {
    let fd_obj = match mq_fd_obj(mqdes) { Ok(f) => f, Err(e) => return e };
    let inode = fd_obj.file.as_posix_mq().expect("checked");
    let flags = inode.flags.load(Ordering::Acquire);
    let access = flags & MQ_ACCMODE;
    if access != MQ_O_WRONLY && access != MQ_O_RDWR { return -9; }
    let nonblock = flags & MQ_O_NONBLOCK != 0;
    let inner = inode.inner.clone();
    let ino = inner.ino;

    if prio >= 32768 { return -22; }

    let deadline = match parse_mq_abstime(ts) {
        Ok(Some(0)) => Some(0u64),
        Ok(Some(d)) => Some(d),
        Ok(None)    => None,
        Err(e)      => return e as isize,
    };

    if let Some(err) = mq_check_reentry(ino, true) { return err; }

    let buf = match read_user_buffer(msg, len) { Ok(b) => b, Err(_) => return -14 };
    if len > inner.msgsize { return -90; }

    let push_ok: bool;
    let notify_target: Option<MqNotify>;
    {
        let mut state = inner.state.lock();
        if state.messages.len() < inner.max_msg {
            let was_empty = state.messages.is_empty();
            let pos = state.messages.iter().position(|(p, _)| *p < prio);
            let idx = pos.unwrap_or(state.messages.len());
            let mut new_q = VecDeque::with_capacity(state.messages.len() + 1);
            for (i, item) in state.messages.drain(..).enumerate() {
                if i == idx { new_q.push_back((prio, buf.clone())); }
                new_q.push_back(item);
            }
            if idx == new_q.len() { new_q.push_back((prio, buf)); }
            state.messages = new_q;
            notify_target = if was_empty { state.notify.take() } else { None };
            push_ok = true;
        } else {
            push_ok = false;
            notify_target = None;
        }
    }
    if push_ok {
        mq_wake_one(&MQ_RECV_WAITERS, ino);
        match notify_target {
            Some(MqNotify::Signal { pid, signo }) if signo > 0 => {
                // Linux ipc/mqueue.c __do_notify: si_code=SI_MESGQ, si_pid 为
                // 触发通知的发送者(current) 翻译到接收者 PID ns 的可见值。
                // 不能用 sys_kill(si_code=SI_USER, si_pid 未按接收者 ns 翻译)。
                crate::syscall::signal::deliver_mq_notify_signal(pid, signo);
            }
            Some(MqNotify::Thread { socket, mut cookie }) => {
                cookie[31] = 1; // NOTIFY_WOKENUP — libc helper checks cookie[31]==1
                crate::net::socket::mq_notify_push_cookie(&socket, cookie);
            }
            _ => {}
        }
        let cur = current_pid_locked();
        if cur != 0 {
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_mut() {
                if let Some(p) = t.get_mut(cur) { p.wakeup_channel = 0; }
            }
        }
        return 0;
    }

    if nonblock { return -11; }

    if deadline == Some(0) { return -110; }
    mq_sleep_self(ino, deadline, true);
}

pub(in crate::syscall) fn sys_mq_timedreceive(mqdes: i32, msg: *mut u8, len: usize, prio: *mut u32, ts: *const u8) -> isize {
    let fd_obj = match mq_fd_obj(mqdes) { Ok(f) => f, Err(e) => return e };
    let inode = fd_obj.file.as_posix_mq().expect("checked");
    let flags = inode.flags.load(Ordering::Acquire);
    let access = flags & MQ_ACCMODE;
    if access != MQ_O_RDONLY && access != MQ_O_RDWR { return -9; }
    let nonblock = flags & MQ_O_NONBLOCK != 0;
    let inner = inode.inner.clone();
    let ino = inner.ino;

    if len < inner.msgsize { return -90; }

    let deadline = match parse_mq_abstime(ts) {
        Ok(Some(0)) => Some(0u64),
        Ok(Some(d)) => Some(d),
        Ok(None)    => None,
        Err(e)      => return e as isize,
    };

    if let Some(err) = mq_check_reentry(ino, false) { return err; }

    let popped: Option<(u32, Vec<u8>)> = {
        let mut state = inner.state.lock();
        state.messages.pop_front()
    };
    if let Some((p, data)) = popped {
        mq_wake_one(&MQ_SEND_WAITERS, ino);
        let cur = current_pid_locked();
        if cur != 0 {
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_mut() {
                if let Some(pcb) = t.get_mut(cur) { pcb.wakeup_channel = 0; }
            }
        }
        let copy_len = data.len().min(len);
        if write_user_buffer(msg, &data[..copy_len]).is_err() { return -14; }
        if !prio.is_null() {
            let _ = write_user_buffer(prio as *mut u8, &p.to_le_bytes());
        }
        return copy_len as isize;
    }

    if nonblock { return -11; }

    if deadline == Some(0) { return -110; }
    mq_sleep_self(ino, deadline, false);
}

pub(in crate::syscall) fn sys_mq_notify(mqdes: i32, notification: *const u8) -> isize {
    const SIGEV_SIGNAL: i32 = 0;
    const SIGEV_NONE:   i32 = 1;
    const SIGEV_THREAD: i32 = 2;

    if notification.is_null() {
        let fd_obj = match mq_fd_obj(mqdes) { Ok(f) => f, Err(e) => return e };
        let inode = fd_obj.file.as_posix_mq().expect("checked");
        inode.inner.clone().state.lock().notify = None;
        return 0;
    }

    let buf = match read_user_buffer(notification, 16) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let mut a8 = [0u8; 8];
    a8.copy_from_slice(&buf[0..8]); let sival_ptr = u64::from_le_bytes(a8);
    let mut a4 = [0u8; 4];
    a4.copy_from_slice(&buf[8..12]);  let signo  = i32::from_le_bytes(a4);
    a4.copy_from_slice(&buf[12..16]); let notify = i32::from_le_bytes(a4);

    // Linux mq_notify() validates the sigevent (sigev_notify/sigev_signo) before
    // fdget(mqdes), so an invalid notification is EINVAL even for a bad fd.
    // SIGEV_THREAD_ID is not a valid mq_notify mode (kernel `default: EINVAL`).
    match notify {
        SIGEV_NONE | SIGEV_SIGNAL | SIGEV_THREAD => {}
        _ => return -22,
    }
    if notify == SIGEV_SIGNAL && (signo <= 0 || signo >= 64) { return -22; }

    // SIGEV_THREAD: glibc/musl register an AF_NETLINK socket (sigev_signo=fd) and
    // a 32-byte cookie (sigev_value.sival_ptr). Resolve both before locking the
    // queue so a bad netlink fd yields EBADF.
    let thread_target = if notify == SIGEV_THREAD {
        let socket = match crate::net::socket::socket_from_fd(signo) {
            Ok(s) => s,
            Err(_) => return -9, // EBADF
        };
        let cookie_vec = match read_user_buffer(sival_ptr as *const u8, 32) {
            Ok(b) => b,
            Err(_) => return -14,
        };
        let mut cookie = [0u8; 32];
        cookie.copy_from_slice(&cookie_vec[..32]);
        Some((socket, cookie))
    } else {
        None
    };

    let fd_obj = match mq_fd_obj(mqdes) { Ok(f) => f, Err(e) => return e };
    let inode = fd_obj.file.as_posix_mq().expect("checked");
    let inner = inode.inner.clone();

    match notify {
        SIGEV_NONE => {
            let mut state = inner.state.lock();
            if state.notify.is_some() { return -16; }
            state.notify = Some(MqNotify::Signal { pid: current_pid_locked(), signo: 0 });
            0
        }
        SIGEV_SIGNAL => {
            let mut state = inner.state.lock();
            if state.notify.is_some() { return -16; }
            state.notify = Some(MqNotify::Signal { pid: current_pid_locked(), signo });
            0
        }
        SIGEV_THREAD => {
            let (socket, cookie) = thread_target.expect("set above");
            let mut state = inner.state.lock();
            if state.notify.is_some() { return -16; }
            state.notify = Some(MqNotify::Thread { socket, cookie });
            0
        }
        _ => -22,
    }
}

pub(in crate::syscall) fn sys_mq_getsetattr(mqdes: i32, new: *const u8, old: *mut u8) -> isize {
    const O_NONBLOCK: i64 = 0o4000;
    let fd_obj = match mq_fd_obj(mqdes) { Ok(f) => f, Err(e) => return e };
    let inode = fd_obj.file.as_posix_mq().expect("checked");
    let inner = inode.inner.clone();

    let (cur_flags, cur_msgs): (i64, i64) = {
        let f = inode.flags.load(Ordering::Acquire);
        let cm = inner.state.lock().messages.len() as i64;
        let mqfl = if f & MQ_O_NONBLOCK != 0 { O_NONBLOCK } else { 0 };
        (mqfl, cm)
    };

    if !old.is_null() {
        let mut buf = [0u8; 32];
        buf[0..8].copy_from_slice(&cur_flags.to_le_bytes());
        buf[8..16].copy_from_slice(&(inner.max_msg as i64).to_le_bytes());
        buf[16..24].copy_from_slice(&(inner.msgsize as i64).to_le_bytes());
        buf[24..32].copy_from_slice(&cur_msgs.to_le_bytes());
        if write_user_buffer(old, &buf).is_err() { return -14; }
    }

    if !new.is_null() {
        let nb = match read_user_buffer(new, 8) {
            Ok(b) => {
                let mut a = [0u8; 8]; a.copy_from_slice(&b[0..8]);
                i64::from_le_bytes(a)
            }
            Err(_) => return -14,
        };
        let mut f = inode.flags.load(Ordering::Acquire);
        if nb & O_NONBLOCK != 0 { f |= MQ_O_NONBLOCK; } else { f &= !MQ_O_NONBLOCK; }
        inode.flags.store(f, Ordering::Release);
    }
    0
}
