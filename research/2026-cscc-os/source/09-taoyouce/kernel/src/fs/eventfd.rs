//! eventfd 真 counter — Slice 2.x T4-eventfd
//!
//! 此前 `sys_eventfd2` 返 /dev/null fd，read 永远 EOF。LTP
//! `eventfd[2-9]` 读 8 字节预期拿到 counter 值，全部 FAIL。
//!
//! 本模块实现 Linux `eventfd2(2)` 语义：
//!
//!   - `EventFd { counter: u64, sem: bool, nonblock: bool, cloexec: bool }`
//!   - read(8 字节)：counter==0 时阻塞或 -EAGAIN；否则
//!       - sem 模式：返 1，counter -= 1
//!       - 非 sem 模式：返 counter 全值，counter = 0
//!   - write(8 字节)：把传入的 u64 累加进 counter；
//!     若 counter + add > 0xFFFFFFFFFFFFFFFE → -EAGAIN（或阻塞）

extern crate alloc;

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::sched::wait_queue::WaitQueue;
use crate::sync::SpinLock;

/// eventfd2 flags
pub const EFD_SEMAPHORE: u32 = 0o0001;
pub const EFD_CLOEXEC:   u32 = 0o2000000;
pub const EFD_NONBLOCK:  u32 = 0o4000;

pub struct EventFdInstance {
    pub(crate) counter: AtomicU64,
    sem:     bool,
    nonblock: bool,
    /// PIDs blocked in read() — woken when write() bumps counter > 0.
    read_waiters: SpinLock<Vec<usize>>,
    /// PIDs blocked in write() — woken when read() drains counter.
    write_waiters: SpinLock<Vec<usize>>,
    /// Persistent epoll hooks attached by `epoll_ctl(ADD)` (Linux
    /// `eventpoll.c` eppoll_entry on `eventfd` wqh). epoll waiters park on
    /// ep->wq, so counter transitions must fire these hooks explicitly.
    poll_hooks: WaitQueue,
}

impl EventFdInstance {
    pub fn new(initval: u64, flags: u32) -> Arc<Self> {
        Arc::new(Self {
            counter:  AtomicU64::new(initval),
            sem:      flags & EFD_SEMAPHORE != 0,
            nonblock: flags & EFD_NONBLOCK != 0,
            read_waiters:  SpinLock::new(Vec::new()),
            write_waiters: SpinLock::new(Vec::new()),
            poll_hooks:    WaitQueue::new(),
        })
    }

    pub fn register_reader(&self, pid: usize) {
        let mut w = self.read_waiters.lock();
        if !w.contains(&pid) { w.push(pid); }
    }
    pub fn register_writer(&self, pid: usize) {
        let mut w = self.write_waiters.lock();
        if !w.contains(&pid) { w.push(pid); }
    }

    fn wake_list(list: Vec<usize>) {
        if list.is_empty() { return; }
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            for pid in list {
                let _ = table.mark_runnable(pid, true);
            }
            table.wake_io_waiters();
        }
    }

    /// 8-byte read: drain counter to 0 (or by 1 in sem mode).
    /// Returns Err(-EAGAIN=-11) when counter==0.
    pub fn read(&self) -> Result<u64, isize> {
        let cur = self.counter.load(Ordering::Acquire);
        if cur == 0 {
            return Err(-11);
        }
        let got = if self.sem {
            self.counter.fetch_sub(1, Ordering::AcqRel);
            1
        } else {
            // CAS to 0 returning the previous value
            loop {
                let v = self.counter.load(Ordering::Acquire);
                if v == 0 { return Err(-11); }
                if self.counter.compare_exchange(v, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    break v;
                }
            }
        };
        // Drain made room for writers blocked on overflow.
        let waiters = core::mem::take(&mut *self.write_waiters.lock());
        Self::wake_list(waiters);
        // Counter went back to 0 (or dropped) — EPOLLOUT becomes ready.
        self.poll_hooks.notify_poll_hooks(crate::fs::epoll::EPOLLOUT);
        Ok(got)
    }

    /// 8-byte write: add `add` to counter. Err(-EAGAIN) if overflow.
    /// Linux refuses 0xFFFFFFFFFFFFFFFF (reserved sentinel).
    pub fn write(&self, add: u64) -> Result<(), isize> {
        if add == u64::MAX { return Err(-22); }
        loop {
            let cur = self.counter.load(Ordering::Acquire);
            let new = cur.saturating_add(add);
            if new > u64::MAX - 1 {
                return Err(-11); // -EAGAIN
            }
            if self.counter.compare_exchange(cur, new, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                break;
            }
        }
        // Counter went positive — wake any blocked readers and epollers.
        let waiters = core::mem::take(&mut *self.read_waiters.lock());
        Self::wake_list(waiters);
        self.poll_hooks.notify_poll_hooks(crate::fs::epoll::EPOLLIN);
        Ok(())
    }

    pub fn pending(&self) -> bool {
        self.counter.load(Ordering::Acquire) > 0
    }

    pub fn is_nonblock(&self) -> bool { self.nonblock }
}

// ============================================================================
// File / Inode adapter
// ============================================================================

use crate::fs::{File, FileMode, FileStat, FileType, FsError, Inode};

pub struct EventFdFile {
    pub instance: Arc<EventFdInstance>,
    ino: usize,
}

impl EventFdFile {
    pub fn new(instance: Arc<EventFdInstance>) -> Arc<Self> {
        static NEXT_INO: AtomicUsize = AtomicUsize::new(2_000_000_000);
        Arc::new(Self {
            instance,
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
        })
    }
}

impl File for EventFdFile {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        if buf.len() < 8 { return Err(FsError::InvalidArgument); }
        match self.instance.read() {
            Ok(v) => {
                buf[..8].copy_from_slice(&v.to_le_bytes());
                Ok(8)
            }
            Err(-11) => Err(FsError::WouldBlock),
            Err(_) => Err(FsError::InvalidArgument),
        }
    }
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        if buf.len() < 8 { return Err(FsError::InvalidArgument); }
        let v = u64::from_le_bytes(buf[..8].try_into().unwrap());
        match self.instance.write(v) {
            Ok(()) => Ok(8),
            Err(-11) => Err(FsError::WouldBlock),
            Err(_) => Err(FsError::InvalidArgument),
        }
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::CharDevice,
            size: 0,
            mode: FileMode(0o020600),
            nlink: 1, uid: 0, gid: 0, atime: 0, mtime: 0, ctime: 0,
        })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
    fn poll_read(&self)  -> bool { self.instance.pending() }
    fn poll_write(&self) -> bool {
        // Writable while counter has headroom for at least one max-size add.
        self.instance.counter.load(Ordering::Acquire) < u64::MAX - 1
    }
    fn register_waiter(&self, events: u32, pid: usize, _deadline: Option<u64>) {
        if events & crate::fs::READINESS_READ != 0 {
            self.instance.register_reader(pid);
        }
        if events & crate::fs::READINESS_WRITE != 0 {
            self.instance.register_writer(pid);
        }
    }
    fn attach_poll_hook(&self, hook: alloc::sync::Arc<dyn crate::sched::wait_queue::PollWakeHook>) {
        self.instance.poll_hooks.add_poll_hook(hook);
    }
    fn detach_poll_hook(&self, hook_id: usize) {
        self.instance.poll_hooks.remove_poll_hook(hook_id);
    }
}

impl Inode for EventFdFile {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}


// ============================================================================
// Syscall entry point — moved from syscall/fd.rs (P0 architecture refactor)
// ============================================================================

/// eventfd2(initval, flags) — create a real eventfd counter.
#[allow(dead_code)]
pub fn sys_eventfd2(initval: usize, flags: usize) -> isize {
    let flags_u32 = flags as u32;
    if flags_u32 & !(EFD_SEMAPHORE | EFD_CLOEXEC | EFD_NONBLOCK) != 0 {
        return -22;
    }
    let instance = EventFdInstance::new(initval as u32 as u64, flags_u32);
    let file = EventFdFile::new(instance);
    let fd_obj = Arc::new(crate::fs::FileDescriptor::new(file, (flags_u32 & !0o3) | 0o2));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                return match crate::fs::fd::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        if flags_u32 & EFD_CLOEXEC != 0 {
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
