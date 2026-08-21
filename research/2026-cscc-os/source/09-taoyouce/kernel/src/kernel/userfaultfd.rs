//! userfaultfd — per Linux `fs/userfaultfd.c`
//!
//! Provides user-space page fault handling:
//! 1. `userfaultfd(flags)` creates a special fd
//! 2. `ioctl(UFFDIO_API)` handshake negotiates features
//! 3. `ioctl(UFFDIO_REGISTER)` registers address ranges for fault monitoring
//! 4. `read(fd)` delivers page fault events to the handler
//! 5. `ioctl(UFFDIO_COPY/UFFDIO_ZEROPAGE)` resolves pending faults

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::fs::{File, FileStat, FileType, FsError, Inode};
use crate::sync::SpinLock;

// UFFDIO ioctl command numbers (per Linux include/uapi/linux/userfaultfd.h)
const UFFDIO_API: usize = 0xc018_aa3f;
const UFFDIO_REGISTER: usize = 0xc020_aa00;
const UFFDIO_UNREGISTER: usize = 0x8010_aa01;
const UFFDIO_COPY: usize = 0xc028_aa03;
const UFFDIO_ZEROPAGE: usize = 0xc020_aa04;
const UFFDIO_WAKE: usize = 0x8010_aa02;
const UFFDIO_WRITEPROTECT: usize = 0xc018_aa06;

// Feature flags
const UFFD_FEATURE_PAGEFAULT_FLAG_WP: u64 = 1 << 0;
const UFFD_FEATURE_EVENT_FORK: u64 = 1 << 1;
const UFFD_FEATURE_EVENT_REMAP: u64 = 1 << 2;
const UFFD_FEATURE_EVENT_REMOVE: u64 = 1 << 3;
const UFFD_FEATURE_EVENT_UNMAP: u64 = 1 << 4;
const UFFD_FEATURE_MISSING_HUGETLBFS: u64 = 1 << 5;
const UFFD_FEATURE_MISSING_SHMEM: u64 = 1 << 6;
const UFFD_FEATURE_SIGBUS: u64 = 1 << 7;
const UFFD_FEATURE_THREAD_ID: u64 = 1 << 8;
const UFFD_FEATURE_MINOR_HUGETLBFS: u64 = 1 << 9;
const UFFD_FEATURE_MINOR_SHMEM: u64 = 1 << 10;

// All features we support
const SUPPORTED_FEATURES: u64 = UFFD_FEATURE_THREAD_ID
    | UFFD_FEATURE_PAGEFAULT_FLAG_WP
    | UFFD_FEATURE_MISSING_SHMEM
    | UFFD_FEATURE_MISSING_HUGETLBFS
    | UFFD_FEATURE_SIGBUS;

// UFFDIO_REGISTER mode bits
const UFFDIO_REGISTER_MODE_MISSING: u64 = 1 << 0;
const UFFDIO_REGISTER_MODE_WP: u64 = 1 << 1;

// uffd_msg types
const UFFD_EVENT_PAGEFAULT: u8 = 0x12;

// uffd_msg size
const UFFD_MSG_SIZE: usize = 32;

// UFFD API version
const UFFD_API: u64 = 0xaa;

/// Registered address range for userfaultfd monitoring.
#[derive(Clone)]
struct UffdRange {
    start: u64,
    len: u64,
    mode: u64,
}

/// Pending page fault event waiting for resolution.
struct PendingFault {
    address: u64,
    flags: u64,
    tid: u32,
    /// The pid of the faulting thread, used to wake it after resolution.
    pid: usize,
}

/// Core userfaultfd state.
struct UserfaultfdState {
    api_done: bool,
    features: u64,
    ranges: Vec<UffdRange>,
    pending_faults: VecDeque<PendingFault>,
    /// PIDs blocked waiting for fault resolution.
    blocked_pids: Vec<usize>,
}

/// Userfaultfd file object — implements the File + Inode traits.
pub struct UserfaultfdFile {
    ino: usize,
    state: SpinLock<UserfaultfdState>,
}

impl UserfaultfdFile {
    fn new(ino: usize) -> Self {
        Self {
            ino,
            state: SpinLock::new(UserfaultfdState {
                api_done: false,
                features: 0,
                ranges: Vec::new(),
                pending_faults: VecDeque::new(),
                blocked_pids: Vec::new(),
            }),
        }
    }

    /// Check if address is in a registered range.
    pub fn covers(&self, addr: u64) -> bool {
        let state = self.state.lock();
        state.ranges.iter().any(|r| addr >= r.start && addr < r.start + r.len)
    }

    /// Enqueue a page fault event. Returns true if the fault was accepted.
    pub fn enqueue_fault(&self, address: u64, is_write: bool, tid: usize, pid: usize) -> bool {
        let mut state = self.state.lock();
        if !state.api_done { return false; }
        let covers = state.ranges.iter().any(|r| address >= r.start && address < r.start + r.len);
        if !covers { return false; }

        let mut flags: u64 = 0;
        if is_write { flags |= 1 << 0; } // UFFD_PAGEFAULT_FLAG_WRITE

        state.pending_faults.push_back(PendingFault {
            address: address & !0xfff,
            flags,
            tid: tid as u32,
            pid,
        });
        state.blocked_pids.push(pid);
        true
    }

    /// Wake all blocked faulting threads.
    fn wake_faulters(&self) {
        let pids: Vec<usize> = {
            let mut state = self.state.lock();
            state.blocked_pids.drain(..).collect()
        };
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            for pid in pids {
                if let Some(proc) = t.get_mut(pid) {
                    if proc.state == crate::sched::process_state::ProcessState::Sleeping {
                        proc.state = crate::sched::process_state::ProcessState::Runnable;
                        t.mark_runnable(pid, false);
                    }
                }
            }
        }
    }

    /// Handle UFFDIO_* ioctls.
    fn handle_ioctl(&self, request: usize, arg: usize) -> isize {
        match request {
            UFFDIO_API => self.ioctl_api(arg),
            UFFDIO_REGISTER => self.ioctl_register(arg),
            UFFDIO_UNREGISTER => self.ioctl_unregister(arg),
            UFFDIO_COPY => self.ioctl_copy(arg),
            UFFDIO_ZEROPAGE => self.ioctl_zeropage(arg),
            UFFDIO_WAKE => self.ioctl_wake(arg),
            UFFDIO_WRITEPROTECT => 0, // stub: success
            _ => -25, // ENOTTY
        }
    }

    /// UFFDIO_API handshake.
    /// struct uffdio_api { u64 api; u64 features; u64 ioctls; }
    fn ioctl_api(&self, arg: usize) -> isize {
        if arg == 0 { return -14; }
        let user = crate::syscall::user::UserSlice::<u8>::new(arg as *const u8, 24);
        let buf = match user.read_bytes() {
            Ok(b) => b,
            Err(_) => return -14,
        };
        let api = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let features = u64::from_le_bytes(buf[8..16].try_into().unwrap());

        if api != UFFD_API {
            return -22; // EINVAL
        }

        let granted = features & SUPPORTED_FEATURES;
        let mut state = self.state.lock();
        state.api_done = true;
        state.features = granted;
        drop(state);

        // Write back: api, features (granted), ioctls (bitmask of supported ioctls)
        let ioctls: u64 = (1 << 0) | (1 << 1) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6);
        let mut out = [0u8; 24];
        out[0..8].copy_from_slice(&UFFD_API.to_le_bytes());
        out[8..16].copy_from_slice(&granted.to_le_bytes());
        out[16..24].copy_from_slice(&ioctls.to_le_bytes());

        let user_mut = crate::syscall::user::UserSlice::<u8>::new_mut(arg as *mut u8, 24);
        if user_mut.write_bytes(&out).is_err() { return -14; }
        0
    }

    /// UFFDIO_REGISTER — register address range for fault monitoring.
    /// struct uffdio_register { struct uffdio_range { u64 start; u64 len; }; u64 mode; u64 ioctls; }
    fn ioctl_register(&self, arg: usize) -> isize {
        if arg == 0 { return -14; }
        let user = crate::syscall::user::UserSlice::<u8>::new(arg as *const u8, 32);
        let buf = match user.read_bytes() {
            Ok(b) => b,
            Err(_) => return -14,
        };
        let start = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let len = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let mode = u64::from_le_bytes(buf[16..24].try_into().unwrap());

        if start & 0xfff != 0 || len & 0xfff != 0 || len == 0 {
            return -22; // EINVAL: not page aligned
        }

        let mut state = self.state.lock();
        if !state.api_done { return -22; }

        state.ranges.push(UffdRange { start, len, mode });
        drop(state);

        // Write back ioctls field: UFFDIO_COPY | UFFDIO_ZEROPAGE | UFFDIO_WAKE
        let ioctls: u64 = (1 << 3) | (1 << 4) | (1 << 5);
        let mut out = [0u8; 8];
        out.copy_from_slice(&ioctls.to_le_bytes());
        let user_mut = crate::syscall::user::UserSlice::<u8>::new_mut(
            (arg + 24) as *mut u8, 8,
        );
        if user_mut.write_bytes(&out).is_err() { return -14; }
        0
    }

    /// UFFDIO_UNREGISTER — unregister address range.
    /// struct uffdio_range { u64 start; u64 len; }
    fn ioctl_unregister(&self, arg: usize) -> isize {
        if arg == 0 { return -14; }
        let user = crate::syscall::user::UserSlice::<u8>::new(arg as *const u8, 16);
        let buf = match user.read_bytes() {
            Ok(b) => b,
            Err(_) => return -14,
        };
        let start = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let len = u64::from_le_bytes(buf[8..16].try_into().unwrap());

        let mut state = self.state.lock();
        state.ranges.retain(|r| !(r.start == start && r.len == len));
        drop(state);
        self.wake_faulters();
        0
    }

    /// UFFDIO_COPY — copy page data to resolve a fault.
    /// struct uffdio_copy { u64 dst; u64 src; u64 len; u64 mode; i64 copy; }
    fn ioctl_copy(&self, arg: usize) -> isize {
        if arg == 0 { return -14; }
        let user = crate::syscall::user::UserSlice::<u8>::new(arg as *const u8, 40);
        let buf = match user.read_bytes() {
            Ok(b) => b,
            Err(_) => return -14,
        };
        let dst = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let src = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let len = u64::from_le_bytes(buf[16..24].try_into().unwrap());

        if dst & 0xfff != 0 || len & 0xfff != 0 || len == 0 {
            return -22;
        }

        // Copy data from src to dst in process address space
        // This resolves the page fault by providing the data
        let page_size = 0x1000usize;
        let mut copied = 0u64;
        while copied < len {
            let src_addr = (src + copied) as *const u8;
            let dst_addr = (dst + copied) as *mut u8;
            let chunk = page_size.min((len - copied) as usize);

            // Read src data
            let src_data = match crate::syscall::user::UserSlice::<u8>::new(src_addr, chunk).read_bytes() {
                Ok(d) => d,
                Err(_) => break,
            };
            // Write to dst
            if crate::syscall::user::UserSlice::<u8>::new_mut(dst_addr, chunk)
                .write_bytes(&src_data).is_err()
            {
                // dst page doesn't exist yet — allocate via page fault handler
                // then retry the write
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                if let Some(ref mut t) = *table {
                    if let Some(pid) = t.current {
                        let as_pid = t.get(pid)
                            .map(|p| if p.mm.is_some() { pid } else { p.tgid })
                            .unwrap_or(pid);
                        if let Some(process) = t.get(as_pid) {
                            if let Some(ref mm) = process.mm {
                                let mut addr_space = mm.lock();
                                let va = crate::mm::VirtAddr(dst_addr as usize + copied as usize);
                                addr_space.handle_page_fault(va, true);
                            }
                        }
                    }
                }
                drop(table);
                // Retry write after fault handled
                let _ = crate::syscall::user::UserSlice::<u8>::new_mut(dst_addr, chunk)
                    .write_bytes(&src_data);
            }
            copied += chunk as u64;
        }

        // Write back copy field (bytes copied)
        let out = (copied as i64).to_le_bytes();
        let user_mut = crate::syscall::user::UserSlice::<u8>::new_mut(
            (arg + 32) as *mut u8, 8,
        );
        let _ = user_mut.write_bytes(&out);

        // Wake faulting threads
        self.wake_faulters();
        0
    }

    /// UFFDIO_ZEROPAGE — provide a zero page to resolve a fault.
    /// struct uffdio_zeropage { struct uffdio_range { u64 start; u64 len; }; u64 mode; i64 zeropage; }
    fn ioctl_zeropage(&self, arg: usize) -> isize {
        if arg == 0 { return -14; }
        let user = crate::syscall::user::UserSlice::<u8>::new(arg as *const u8, 32);
        let buf = match user.read_bytes() {
            Ok(b) => b,
            Err(_) => return -14,
        };
        let start = u64::from_le_bytes(buf[0..8].try_into().unwrap());
        let len = u64::from_le_bytes(buf[8..16].try_into().unwrap());

        if start & 0xfff != 0 || len & 0xfff != 0 || len == 0 {
            return -22;
        }

        // Zero the pages in the destination range
        let page_size = 0x1000usize;
        let zeros = alloc::vec![0u8; page_size];
        let mut done = 0u64;
        while done < len {
            let addr = (start + done) as *mut u8;
            // Allocate via page fault handler if needed
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref mut t) = *table {
                if let Some(pid) = t.current {
                    let as_pid = t.get(pid)
                        .map(|p| if p.mm.is_some() { pid } else { p.tgid })
                        .unwrap_or(pid);
                    if let Some(process) = t.get(as_pid) {
                        if let Some(ref mm) = process.mm {
                            let mut addr_space = mm.lock();
                            let va = crate::mm::VirtAddr(addr as usize);
                            addr_space.handle_page_fault(va, true);
                        }
                    }
                }
            }
            drop(table);
            let _ = crate::syscall::user::UserSlice::<u8>::new_mut(addr, page_size)
                .write_bytes(&zeros);
            done += page_size as u64;
        }

        // Write back zeropage field
        let out = (done as i64).to_le_bytes();
        let user_mut = crate::syscall::user::UserSlice::<u8>::new_mut(
            (arg + 24) as *mut u8, 8,
        );
        let _ = user_mut.write_bytes(&out);

        self.wake_faulters();
        0
    }

    /// UFFDIO_WAKE — wake blocked threads without providing data.
    fn ioctl_wake(&self, _arg: usize) -> isize {
        self.wake_faulters();
        0
    }
}

impl File for UserfaultfdFile {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        if buf.len() < UFFD_MSG_SIZE {
            return Err(FsError::InvalidArgument);
        }

        let state = self.state.lock();
        if !state.api_done {
            return Err(FsError::InvalidArgument);
        }
        drop(state);

        // Try to dequeue a pending fault event
        let mut state = self.state.lock();
        if let Some(fault) = state.pending_faults.pop_front() {
            drop(state);
            // Build uffd_msg (32 bytes):
            // u8 event | u8[3] reserved1 | u32 reserved2 | union(24 bytes)
            // For UFFD_EVENT_PAGEFAULT:
            //   union.pagefault { u64 flags; u64 address; union { u32 ptid; } feat; }
            let mut msg = [0u8; UFFD_MSG_SIZE];
            msg[0] = UFFD_EVENT_PAGEFAULT;
            // pagefault union starts at offset 8
            msg[8..16].copy_from_slice(&fault.flags.to_le_bytes());
            msg[16..24].copy_from_slice(&fault.address.to_le_bytes());
            msg[24..28].copy_from_slice(&fault.tid.to_le_bytes());
            buf[..UFFD_MSG_SIZE].copy_from_slice(&msg);
            Ok(UFFD_MSG_SIZE)
        } else {
            drop(state);
            // No events — would block. Return EAGAIN for non-blocking, or block.
            Err(FsError::WouldBlock)
        }
    }

    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::CharDevice,
            size: 0,
            mode: crate::fs::FileMode::new(0o020600),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }

    fn sync(&self) -> Result<(), FsError> { Ok(()) }

    fn poll_read(&self) -> bool {
        let state = self.state.lock();
        !state.pending_faults.is_empty()
    }

    fn poll_write(&self) -> bool { false }

    fn ioctl(&self, request: usize, arg: usize) -> Option<isize> {
        Some(self.handle_ioctl(request, arg))
    }
}

impl Inode for UserfaultfdFile {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}

static UFFD_INO: AtomicUsize = AtomicUsize::new(0x7000_0000);

/// Create a userfaultfd file descriptor. Called from sys_userfaultfd().
pub fn create_userfaultfd(flags: u32) -> isize {
    const O_CLOEXEC: u32 = 0o2000000;
    const O_NONBLOCK_FLAG: u32 = 0o4000;

    let ino = UFFD_INO.fetch_add(1, Ordering::Relaxed);
    let uffd = Arc::new(UserfaultfdFile::new(ino));
    let inode: Arc<dyn Inode> = uffd;

    let mut open_flags: u32 = 0o0;
    if flags & O_NONBLOCK_FLAG != 0 {
        open_flags |= O_NONBLOCK_FLAG;
    }
    let fd_obj = Arc::new(crate::fs::FileDescriptor::new(inode, open_flags));

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                let cloexec = flags & O_CLOEXEC != 0;
                return match crate::fs::fd::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        if cloexec {
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
