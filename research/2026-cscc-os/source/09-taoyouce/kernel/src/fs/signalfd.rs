//! signalfd 真 signal queue — Slice 2.x T4-signalfd
//!
//! 此前 `sys_signalfd4` 返 /dev/null fd，read 永远 EOF。LTP
//! `signalfd0[1-9]` 测从 fd 读出 `struct signalfd_siginfo` 验证 si_signo。
//!
//! 本模块实现：
//!   - `SignalFdInstance { mask: u64, queue: VecDeque<SignalEntry> }`
//!   - 收到 sigprocmask 阻塞且在 fd mask 内的信号时，push 到 queue
//!     而不是真投递（kernel/src/syscall/signal.rs deliver 路径要识别这个）
//!   - read 一个 128 字节 `struct signalfd_siginfo` 拿出 head
//!
//! Linux `struct signalfd_siginfo` 关键字段（按 RV64 layout，128 字节）：
//!   u32 ssi_signo;    // signal number
//!   i32 ssi_errno;    // 0
//!   i32 ssi_code;     // SI_USER / SI_QUEUE / ...
//!   u32 ssi_pid;
//!   u32 ssi_uid;
//!   i32 ssi_fd;
//!   u32 ssi_tid;
//!   u32 ssi_band;
//!   u32 ssi_overrun;
//!   u32 ssi_trapno;
//!   i32 ssi_status;
//!   i32 ssi_int;
//!   u64 ssi_ptr;
//!   u64 ssi_utime;
//!   u64 ssi_stime;
//!   u64 ssi_addr;
//!   u16 ssi_addr_lsb;
//!   u8  pad[46];      // 总长 128 字节

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::sched::wait_queue::WaitQueue;
use crate::sync::SpinLock;

/// signalfd4 flags
pub const SFD_CLOEXEC:  u32 = 0o2000000;
pub const SFD_NONBLOCK: u32 = 0o4000;

#[derive(Debug, Clone)]
pub struct SignalEntry {
    pub signo: u32,
    pub code: i32,
    pub pid: u32,
    pub uid: u32,
}

pub struct SignalFdInstance {
    /// sigset_t（仅取第一个 u64）
    mask: AtomicU64,
    queue: SpinLock<VecDeque<SignalEntry>>,
    /// Persistent epoll hooks attached by `epoll_ctl(ADD)`.
    poll_hooks: WaitQueue,
}

impl SignalFdInstance {
    pub fn new(mask: u64) -> Arc<Self> {
        Arc::new(Self {
            mask: AtomicU64::new(mask),
            queue: SpinLock::new(VecDeque::new()),
            poll_hooks: WaitQueue::new(),
        })
    }

    pub fn set_mask(&self, mask: u64) { self.mask.store(mask, Ordering::Release); }
    pub fn get_mask(&self) -> u64 { self.mask.load(Ordering::Acquire) }

    /// Called from sys_kill / sigaction default path when a signal in our
    /// mask is targeted at this fd's owning process. Push to queue instead
    /// of delivering as a normal signal.
    pub fn deliver(&self, entry: SignalEntry) {
        let mask = self.mask.load(Ordering::Acquire);
        if (mask >> (entry.signo as u64 - 1)) & 1 == 0 {
            return; // signal not in our mask, ignore
        }
        let mut q = self.queue.lock();
        if q.len() < 64 {
            q.push_back(entry);
        }
        drop(q);
        // 0→1 queue transition: wake epoll waiters through ep_poll_callback.
        self.poll_hooks.notify_poll_hooks(crate::fs::epoll::EPOLLIN);
    }

    /// Pull one entry out and serialize to 128-byte siginfo. Returns the bytes
    /// or Err(-EAGAIN) if queue empty.
    pub fn read_one(&self) -> Result<[u8; 128], isize> {
        let mut q = self.queue.lock();
        let entry = q.pop_front().ok_or(-11isize)?;
        drop(q);
        let mut out = [0u8; 128];
        out[0..4].copy_from_slice(&entry.signo.to_le_bytes());
        out[4..8].copy_from_slice(&0i32.to_le_bytes());      // ssi_errno
        out[8..12].copy_from_slice(&entry.code.to_le_bytes());
        out[12..16].copy_from_slice(&entry.pid.to_le_bytes());
        out[16..20].copy_from_slice(&entry.uid.to_le_bytes());
        // remaining fields = 0
        Ok(out)
    }

    pub fn pending(&self) -> bool {
        !self.queue.lock().is_empty()
    }
}

// ============================================================================
// File / Inode adapter
// ============================================================================

use crate::fs::{File, FileMode, FileStat, FileType, FsError, Inode};

pub struct SignalFdFile {
    pub instance: Arc<SignalFdInstance>,
    ino: usize,
}

impl SignalFdFile {
    pub fn new(instance: Arc<SignalFdInstance>) -> Arc<Self> {
        static NEXT_INO: AtomicUsize = AtomicUsize::new(4_000_000_000);
        Arc::new(Self {
            instance,
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
        })
    }
}

impl File for SignalFdFile {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        if buf.len() < 128 { return Err(FsError::InvalidArgument); }
        if let Ok(siginfo) = self.instance.read_one() {
            buf[..128].copy_from_slice(&siginfo);
            return Ok(128);
        }
        // Linux 语义：signalfd 消费的是调用进程被阻塞的 pending 信号。
        let mask = self.instance.get_mask();
        if let Some((signo, code, pid, uid, status)) =
            crate::kernel::signal::signalfd_dequeue(mask)
        {
            let mut out = [0u8; 128];
            out[0..4].copy_from_slice(&signo.to_le_bytes());
            out[8..12].copy_from_slice(&code.to_le_bytes());
            out[12..16].copy_from_slice(&pid.to_le_bytes());
            out[16..20].copy_from_slice(&uid.to_le_bytes());
            out[40..44].copy_from_slice(&status.to_le_bytes());
            buf[..128].copy_from_slice(&out);
            return Ok(128);
        }
        Err(FsError::WouldBlock)
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
    fn poll_read(&self)  -> bool {
        self.instance.pending()
            || crate::kernel::signal::signalfd_pending(self.instance.get_mask())
    }
    fn poll_write(&self) -> bool { false }
    fn attach_poll_hook(&self, hook: alloc::sync::Arc<dyn crate::sched::wait_queue::PollWakeHook>) {
        self.instance.poll_hooks.add_poll_hook(hook);
    }
    fn detach_poll_hook(&self, hook_id: usize) {
        self.instance.poll_hooks.remove_poll_hook(hook_id);
    }
}

impl Inode for SignalFdFile {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
    fn as_signalfd(&self) -> Option<&SignalFdFile> { Some(self) }
}

// ============================================================================
// Syscall entry point — moved from syscall/fd.rs (P0 architecture refactor)
// ============================================================================

/// signalfd4(fd, mask, sizemask, flags) — create or update a signal fd queue.
#[allow(dead_code)]
pub fn sys_signalfd4(fd: isize, mask: *const u8, sizemask: usize, flags: usize) -> isize {
    let mut mask_val: u64 = 0;
    if !mask.is_null() && sizemask >= 8 {
        if let Ok(b) = crate::syscall::user::read_user_buffer(mask, 8) {
            mask_val = u64::from_le_bytes(b[..8].try_into().unwrap());
        }
    }
    if fd >= 0 {
        if let Some(fd_obj) = crate::syscall::fd::get_fd_obj(fd as usize) {
            if let Some(sfd) = fd_obj.file.as_signalfd() {
                sfd.instance.set_mask(mask_val);
                return fd;
            }
        }
        return -22;
    }
    let instance = SignalFdInstance::new(mask_val);
    let file = SignalFdFile::new(instance);
    let fd_obj = alloc::sync::Arc::new(crate::fs::FileDescriptor::new(file, flags as u32));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                return match crate::fs::fd::alloc(process, fd_obj, 0, true) {
                    Ok(new_fd) => {
                        if (flags as u32) & SFD_CLOEXEC != 0 {
                            let _ = crate::fs::fd::set_cloexec(process, new_fd, true);
                        }
                        new_fd as isize
                    }
                    Err(e) => e,
                };
            }
        }
    }
    -22
}
