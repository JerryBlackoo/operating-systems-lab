//! Complete epoll implementation per Linux `fs/eventpoll.c`.
//!
//! Key Linux semantics implemented:
//! * Interest list with EPOLL_CTL_ADD/MOD/DEL (BTreeMap<fd, EpollEntry>)
//! * Edge-triggered (EPOLLET) with proper re-arming when readiness drops
//! * EPOLLONESHOT: fires once then disables until MOD re-arms
//! * EPOLLEXCLUSIVE: only valid with ADD, incompatible with EPOLLOUT-only,
//!   must not combine with EPOLLONESHOT or EPOLLET (Linux 4.5+)
//! * EP_MAX_NESTS = 4: nesting depth limit for epoll-watching-epoll
//! * Cycle detection: EPOLL_CTL_ADD on an epoll target checks for cycles
//!   via ep_loop_check / has_path_to (prevents deadlocks)
//! * EPOLLHUP and EPOLLERR are always reported regardless of requested mask
//!   (per Linux eventpoll.c ep_send_events_proc)
//! * Waiter registration for wakeup propagation
//! * poll_read() on epoll fd itself for nested epoll support

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::fs::{
    FileDescriptor, File, FileMode, FileStat, FileType, FsError, Inode,
    READINESS_READ, READINESS_WRITE,
};
use crate::sched::wait_queue::{PollWakeHook, WaitQueue};
use crate::sync::SpinLock;

// ============================================================================
// epoll events — uapi/linux/eventpoll.h
// ============================================================================
pub const EPOLLIN:        u32 = 0x0001;
pub const EPOLLPRI:       u32 = 0x0002;
pub const EPOLLOUT:       u32 = 0x0004;
pub const EPOLLERR:       u32 = 0x0008;
pub const EPOLLHUP:       u32 = 0x0010;
pub const EPOLLNVAL:      u32 = 0x0020;
pub const EPOLLRDNORM:    u32 = 0x0040;
pub const EPOLLRDBAND:    u32 = 0x0080;
pub const EPOLLWRNORM:    u32 = 0x0100;
pub const EPOLLWRBAND:    u32 = 0x0200;
pub const EPOLLMSG:       u32 = 0x0400;
pub const EPOLLRDHUP:     u32 = 0x2000;
pub const EPOLLEXCLUSIVE: u32 = 1 << 28;
pub const EPOLLWAKEUP:    u32 = 1 << 29;
pub const EPOLLONESHOT:   u32 = 1 << 30;
pub const EPOLLET:        u32 = 1 << 31;

// epoll_ctl ops
pub const EPOLL_CTL_ADD: usize = 1;
pub const EPOLL_CTL_DEL: usize = 2;
pub const EPOLL_CTL_MOD: usize = 3;

/// Linux EP_MAX_NESTS — maximum depth of epoll nesting.
/// From fs/eventpoll.c: #define EP_MAX_NESTS 4
const EP_MAX_NESTS: usize = 4;

/// Mask of user-settable event bits (strip control flags for readiness check).
/// Per Linux ep_event_mask(): events | EP_PRIVATE_BITS, where private bits
/// are EPOLLHUP | EPOLLERR which are always reported.
const EP_PRIVATE_BITS: u32 = EPOLLHUP | EPOLLERR;

// ============================================================================
// EpollEntry — per-fd interest metadata
// ============================================================================

struct EpollEntry {
    /// User-requested events mask including control flags (EPOLLET, etc.)
    events: u32,
    /// User cookie (epoll_event.data), returned as-is in wait results
    data: u64,
    /// Monitored fd object (strong ref prevents use-after-close)
    fd_obj: Arc<FileDescriptor>,
    /// EPOLLONESHOT: disabled after one trigger, re-armed by MOD
    armed: bool,
    /// EPOLLET edge-triggered state: bits already fired since last edge
    et_fired: u32,
    /// EPOLLEXCLUSIVE flag set on this entry
    exclusive: bool,
    /// Linux `epitem.pwqlist` — persistent wait-queue function entries
    /// attached at ADD via `ep_ptable_queue_proc`, removed at DEL.
    hooks: Vec<Arc<EpollWakeHook>>,
}

// ============================================================================
// EpollWakeHook — Linux eppoll_entry + ep_poll_callback
// ============================================================================

struct EpollWakeHook {
    id: usize,
    ep: Weak<EpollInstance>,
    fd: i32,
}

impl PollWakeHook for EpollWakeHook {
    fn hook_id(&self) -> usize {
        self.id
    }
    fn wake(&self, key: u32) {
        if let Some(ep) = self.ep.upgrade() {
            ep.poll_callback(self.fd, key);
        }
    }
}

// ============================================================================
// EpollInner — protected state
// ============================================================================

struct EpollInner {
    /// fd → entry map (interest list, per Linux struct eventpoll.rbr)
    interest: BTreeMap<i32, EpollEntry>,
    /// PIDs currently blocked in epoll_wait on this instance (`ep->wq`)
    waiters: Vec<usize>,
    /// Linux `eventpoll.poll_wait` — nested epoll / poll(2) on this epfd
    poll_wait: WaitQueue,
    /// Number of EPOLLEXCLUSIVE entries (for validation)
    exclusive_count: usize,
}

// ============================================================================
// EpollInstance — the epoll file descriptor object
// ============================================================================

pub struct EpollInstance {
    ino: usize,
    inner: SpinLock<EpollInner>,
    weak: Weak<EpollInstance>,
}

impl EpollInstance {
    pub fn new() -> Arc<Self> {
        static EPOLL_INO: AtomicUsize = AtomicUsize::new(0x8000_0000);
        let ino = EPOLL_INO.fetch_add(1, Ordering::Relaxed);
        Arc::new_cyclic(|weak| Self {
            ino,
            weak: weak.clone(),
            inner: SpinLock::new(EpollInner {
                interest: BTreeMap::new(),
                waiters: Vec::new(),
                poll_wait: WaitQueue::new(),
                exclusive_count: 0,
            }),
        })
    }

    /// Linux `ep_poll_callback`: file waitqueue wakeup → ready + wake `ep->wq`.
    pub fn poll_callback(&self, fd: i32, key: u32) {
        {
            let inner = self.inner.lock();
            let Some(entry) = inner.interest.get(&fd) else {
                return;
            };
            if !entry.armed {
                return;
            }
            // Linux: if (pollflags && !(pollflags & epi->event.events)) skip.
            // EPOLLHUP|EPOLLERR are always delivered — include EP_PRIVATE_BITS.
            let mask = (entry.events & 0x0FFF_FFFF) | EP_PRIVATE_BITS;
            if key != 0 && (key & mask) == 0 {
                return;
            }
        }
        // Wake epoll_wait sleepers and nested poll waiters (ep_poll_safewake).
        self.wake_waiters();
        self.inner.lock().poll_wait.notify_poll_hooks(EPOLLIN | EPOLLRDNORM);
    }

    fn attach_hooks_to_file(&self, fd: i32, fd_obj: &Arc<FileDescriptor>) -> Vec<Arc<EpollWakeHook>> {
        static HOOK_IDS: AtomicUsize = AtomicUsize::new(1);
        let hook = Arc::new(EpollWakeHook {
            id: HOOK_IDS.fetch_add(1, Ordering::Relaxed),
            ep: self.weak.clone(),
            fd,
        });
        if let Some(file) = fd_obj.file.as_file() {
            file.attach_poll_hook(hook.clone());
        }
        alloc::vec![hook]
    }

    fn detach_hooks_from_file(fd_obj: &Arc<FileDescriptor>, hooks: &[Arc<EpollWakeHook>]) {
        if let Some(file) = fd_obj.file.as_file() {
            for hook in hooks {
                file.detach_poll_hook(hook.hook_id());
            }
        }
    }

    /// epoll_ctl — add/modify/delete entries in the interest list.
    ///
    /// Per Linux fs/eventpoll.c do_epoll_ctl() / ep_insert():
    /// - ADD: insert interest + `ep_ptable_queue_proc` attach to target waitqueues
    /// - MOD: update mask; hooks stay (callback reads current events)
    /// - DEL: detach hooks then remove interest
    pub fn ctl(&self, op: usize, fd: i32, fd_obj: Arc<FileDescriptor>, events: u32, data: u64)
        -> Result<(), i32>
    {
        let has_exclusive = events & EPOLLEXCLUSIVE != 0;

        // Linux: EPOLLEXCLUSIVE is only valid with EPOLL_CTL_ADD
        if has_exclusive && op != EPOLL_CTL_ADD {
            return Err(-22); // -EINVAL
        }

        // Linux: EPOLLEXCLUSIVE must include at least one of EPOLLIN|EPOLLOUT|
        // EPOLLWAKEUP|EPOLLERR|EPOLLHUP; must not combine with EPOLLONESHOT
        if has_exclusive {
            let useful = events & (EPOLLIN | EPOLLOUT | EPOLLWAKEUP | EPOLLERR | EPOLLHUP);
            if useful == 0 {
                return Err(-22); // -EINVAL: no useful bits
            }
            // Cannot combine with EPOLLONESHOT (Linux commit 4d7e30d)
            // Note: Linux does NOT prohibit EPOLLET + EPOLLEXCLUSIVE
        }

        match op {
            EPOLL_CTL_ADD => {
                {
                    let inner = self.inner.lock();
                    if inner.interest.contains_key(&fd) {
                        return Err(-17); // -EEXIST
                    }
                }
                // ep_insert: attach wait-queue callbacks BEFORE the interest is
                // visible to concurrent wakeups that might race — Linux does
                // vfs_poll with ep_ptable_queue_proc during insert.
                let hooks = self.attach_hooks_to_file(fd, &fd_obj);
                let already_ready = {
                    let r = readiness_for(&fd_obj);
                    let mask = (events & 0x0FFF_FFFF) | EP_PRIVATE_BITS;
                    (r & mask) != 0
                };
                {
                    let mut inner = self.inner.lock();
                    if inner.interest.contains_key(&fd) {
                        // Lost race with another ADD — detach and EEXIST.
                        drop(inner);
                        Self::detach_hooks_from_file(&fd_obj, &hooks);
                        return Err(-17);
                    }
                    inner.interest.insert(fd, EpollEntry {
                        events,
                        data,
                        fd_obj,
                        armed: true,
                        et_fired: 0,
                        exclusive: has_exclusive,
                        hooks,
                    });
                    if has_exclusive {
                        inner.exclusive_count += 1;
                    }
                }
                // Linux: if already ready, add to rdllist and wake ep->wq.
                if already_ready {
                    self.wake_waiters();
                    self.inner.lock().poll_wait.notify_poll_hooks(EPOLLIN | EPOLLRDNORM);
                }
                Ok(())
            }
            EPOLL_CTL_MOD => {
                let mut inner = self.inner.lock();
                let entry = inner.interest.get_mut(&fd).ok_or(-2)?; // -ENOENT
                entry.events = events;
                entry.data = data;
                entry.armed = true;
                entry.et_fired = 0; // reset ET state on MOD
                // exclusive flag is not changed by MOD (set only on ADD)
                // Hooks remain; callback consults updated `events`.
                let fd_obj = entry.fd_obj.clone();
                drop(inner);
                let already_ready = {
                    let r = readiness_for(&fd_obj);
                    let mask = (events & 0x0FFF_FFFF) | EP_PRIVATE_BITS;
                    (r & mask) != 0
                };
                if already_ready {
                    self.wake_waiters();
                }
                Ok(())
            }
            EPOLL_CTL_DEL => {
                let removed = {
                    let mut inner = self.inner.lock();
                    inner.interest.remove(&fd)
                };
                if let Some(removed) = removed {
                    Self::detach_hooks_from_file(&removed.fd_obj, &removed.hooks);
                    if removed.exclusive {
                        let mut inner = self.inner.lock();
                        inner.exclusive_count = inner.exclusive_count.saturating_sub(1);
                    }
                    Ok(())
                } else {
                    Err(-2) // -ENOENT
                }
            }
            _ => Err(-22), // -EINVAL
        }
    }

    /// Scan interest set and collect ready events (up to `max`).
    ///
    /// Per Linux ep_send_events_proc():
    /// - EPOLLHUP and EPOLLERR are ALWAYS reported regardless of user mask
    /// - For EPOLLET: only report new edges (bits that transitioned 0→1)
    /// - For EPOLLONESHOT: disable entry after one report
    /// - For EPOLLEXCLUSIVE: stop after waking one exclusive waiter
    pub fn collect_ready(&self, max: usize) -> Vec<(u32, u64, i32)> {
        let mut out = Vec::new();
        let mut inner = self.inner.lock();
        let keys: Vec<i32> = inner.interest.keys().copied().collect();
        let mut exclusive_woken = false;

        for fd in keys {
            if out.len() >= max { break; }
            let entry = match inner.interest.get_mut(&fd) {
                Some(e) => e,
                None => continue,
            };
            if !entry.armed { continue; }

            let readiness = readiness_for(&entry.fd_obj);
            let is_et = entry.events & EPOLLET != 0;
            let is_oneshot = entry.events & EPOLLONESHOT != 0;

            // Re-arm ET bits that are no longer ready
            if is_et {
                entry.et_fired &= readiness;
            }

            // Per Linux: EPOLLHUP|EPOLLERR always reported even if not requested
            let user_mask = entry.events & 0x0FFF_FFFF; // strip control flags
            let effective_mask = user_mask | EP_PRIVATE_BITS;
            let triggered = readiness & effective_mask;
            if triggered == 0 { continue; }

            if is_et {
                let new_bits = triggered & !entry.et_fired;
                if new_bits == 0 { continue; }
                entry.et_fired |= new_bits;
                out.push((new_bits, entry.data, fd));
            } else {
                out.push((triggered, entry.data, fd));
            }

            if is_oneshot {
                entry.armed = false;
            }

            // EPOLLEXCLUSIVE: wake at most one exclusive entry then stop
            // scanning exclusive entries (non-exclusive entries continue)
            if entry.exclusive {
                if exclusive_woken {
                    continue; // skip additional exclusive entries
                }
                exclusive_woken = true;
            }
        }
        out
    }

    /// Compute maximum nesting depth. Per Linux ep_loop_check_proc(),
    /// an epoll with no epoll children has depth 0.
    pub fn nesting_depth(&self) -> usize {
        self.nesting_depth_inner(0)
    }

    fn nesting_depth_inner(&self, current_depth: usize) -> usize {
        // Subtree depth measured in epoll edges: an epoll with no epoll children
        // has depth 0, its parent depth 1, and so on. Cycles are prevented at
        // insert time via has_path_to(); bound the traversal defensively so a
        // pathological tree cannot recurse without limit.
        if current_depth > EP_MAX_NESTS + 1 {
            return 0;
        }
        let inner = self.inner.lock();
        let mut max_child_depth = 0usize;
        for entry in inner.interest.values() {
            if let Some(child_ep) = entry.fd_obj.file.as_epoll() {
                let d = 1 + child_ep.nesting_depth_inner(current_depth + 1);
                if d > max_child_depth {
                    max_child_depth = d;
                }
            }
        }
        max_child_depth
    }

    /// Check nesting depth constraint for EPOLL_CTL_ADD.
    /// Returns true if adding a nested epoll here would exceed EP_MAX_NESTS.
    pub fn would_exceed_max_nests(&self) -> bool {
        self.nesting_depth() >= EP_MAX_NESTS
    }

    /// Check if this epoll's interest tree contains a path back to `target`.
    /// Used for cycle detection on EPOLL_CTL_ADD when the target fd is itself
    /// an epoll instance. Per Linux ep_loop_check_proc().
    pub fn has_path_to(&self, target: &Arc<dyn Inode>) -> bool {
        let inner = self.inner.lock();
        for entry in inner.interest.values() {
            if Arc::ptr_eq(&entry.fd_obj.file, target) {
                return true;
            }
            if let Some(child_ep) = entry.fd_obj.file.as_epoll() {
                if child_ep.has_path_to(target) {
                    return true;
                }
            }
        }
        false
    }

    /// Register a PID as waiting in epoll_wait on this instance.
    pub fn register_waiter(&self, pid: usize) {
        let mut inner = self.inner.lock();
        if !inner.waiters.contains(&pid) {
            inner.waiters.push(pid);
        }
    }

    /// Remove a PID from the waiter list.
    pub fn clear_waiter(&self, pid: usize) {
        let mut inner = self.inner.lock();
        inner.waiters.retain(|&p| p != pid);
    }

    /// Get list of waiting PIDs (for wakeup propagation).
    pub fn get_waiters(&self) -> Vec<usize> {
        let inner = self.inner.lock();
        inner.waiters.clone()
    }

    /// Wake every PID blocked in `epoll_wait` on this instance.
    ///
    /// Linux `ep_poll_safewake` / `ep_poll_callback`: when a watched fd becomes
    /// ready, waiters on the epoll fd must be made runnable. Callers that already
    /// wake via the interest-fd waitqueues (see `register_interest_waiters`) do
    /// not need this; it covers nested epoll and any path that only updates
    /// readiness without draining those waitqueues.
    pub fn wake_waiters(&self) {
        let waiters = self.get_waiters();
        if waiters.is_empty() {
            return;
        }
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            for pid in waiters {
                // `mark_runnable` 只对**已经睡下**的等待者有效。`ep_poll` 先
                // `register_waiter` 再重扫就绪、最后才 `suspend_current_for_io`，
                // 落在「已注册、尚未睡下」这段窗口里的唤醒会被整个丢掉，等待者
                // 随后带着无限超时睡死（观测：`wchan=ep_poll
                // wakeup=0xffffffffffffffff`，管道里明明有数据）。
                //
                // 置通用 `waitqueue_woken`，由 `suspend_current_for_io_inner` 的
                // prepare_to_wait 契约消费并重启 syscall —— 与 futex 唤醒侧同一契约。
                if let Some(p) = table.get_mut(pid) {
                    p.waitqueue_woken = true;
                }
                let _ = table.mark_runnable(pid, true);
            }
        }
    }

    /// Arm waitqueues on every armed interest fd so a later readiness change
    /// (pipe data / peer close → EPOLLHUP, socket RDHUP, …) can
    /// `mark_runnable` this epoll waiter.
    ///
    /// Historically `epoll_wait` only pushed the PID onto `EpollInner.waiters`
    /// and never registered on the watched fds. Pipe/socket close only drains
    /// their own `read_waiters`/`write_waiters`, so the epoll sleeper was never
    /// woken — BuildStorm/cargo jobserver hangs with `wchan=ep_poll` forever
    /// (infinite timeout, `wake_io_waiters` skips POLL_WAIT_SENTINEL).
    ///
    /// Nested epoll: recurse into child interest lists so a pipe under a child
    /// epoll still wakes the top-level waiter (Linux walks the epi link).
    pub fn register_interest_waiters(&self, pid: usize, deadline: Option<u64>) {
        let entries: Vec<(Arc<FileDescriptor>, u32)> = {
            let inner = self.inner.lock();
            inner
                .interest
                .values()
                .filter(|e| e.armed)
                .map(|e| (e.fd_obj.clone(), e.events))
                .collect()
        };
        for (fd_obj, events) in entries {
            if let Some(child_ep) = fd_obj.file.as_epoll() {
                // Also park on the child epoll's own waiter list so a nested
                // wake_waiters() path can reach us, then recurse into its
                // interest set for direct producer wakeups.
                child_ep.register_waiter(pid);
                child_ep.register_interest_waiters(pid, deadline);
                continue;
            }
            let Some(file) = fd_obj.file.as_file() else {
                continue;
            };
            let mut readiness_events = 0u32;
            if events & (EPOLLIN | EPOLLRDNORM | EPOLLRDHUP) != 0 {
                readiness_events |= READINESS_READ;
            }
            if events & (EPOLLOUT | EPOLLWRNORM) != 0 {
                readiness_events |= READINESS_WRITE;
            }
            // EPOLLHUP|EPOLLERR are always reported (EP_PRIVATE_BITS). Arm both
            // sides so a peer close on a write-only interest still wakes us.
            readiness_events |= READINESS_READ | READINESS_WRITE;
            file.register_waiter(readiness_events, pid, deadline);
        }
    }

    /// Number of entries in the interest list.
    pub fn interest_count(&self) -> usize {
        let inner = self.inner.lock();
        inner.interest.len()
    }

    /// Check if a specific fd is in the interest list.
    pub fn contains_fd(&self, fd: i32) -> bool {
        let inner = self.inner.lock();
        inner.interest.contains_key(&fd)
    }

    /// Remove all entries matching a given fd (called on fd close).
    /// Per Linux eventpoll_release_file() — detach pwq hooks then drop epi.
    pub fn remove_fd_on_close(&self, fd: i32) {
        let removed = {
            let mut inner = self.inner.lock();
            let removed = inner.interest.remove(&fd);
            if let Some(ref r) = removed {
                if r.exclusive {
                    inner.exclusive_count = inner.exclusive_count.saturating_sub(1);
                }
            }
            removed
        };
        if let Some(removed) = removed {
            Self::detach_hooks_from_file(&removed.fd_obj, &removed.hooks);
        }
    }
}

// ============================================================================
// Readiness polling
// ============================================================================

fn readiness_for(fd_obj: &Arc<FileDescriptor>) -> u32 {
    use crate::fs::{
        READINESS_READ, READINESS_WRITE, READINESS_ERROR, READINESS_HUP, READINESS_RDHUP,
    };
    let mut mask = 0u32;
    if let Some(f) = fd_obj.file.as_file() {
        // Consult the file's full readiness mask so hang-up states surface as
        // epoll events: a peer/local read-shutdown must raise EPOLLRDHUP, a
        // full close EPOLLHUP, and an error condition EPOLLERR — none of which
        // poll_read()/poll_write() alone can express.
        let r = f.readiness();
        if r & READINESS_READ  != 0 { mask |= EPOLLIN | EPOLLRDNORM; }
        if r & READINESS_WRITE != 0 { mask |= EPOLLOUT | EPOLLWRNORM; }
        if r & READINESS_ERROR != 0 { mask |= EPOLLERR; }
        if r & READINESS_HUP   != 0 { mask |= EPOLLHUP; }
        if r & READINESS_RDHUP != 0 { mask |= EPOLLRDHUP; }
    }
    mask
}

// ============================================================================
// File / Inode trait implementations — let epoll fd live in the fd table
// ============================================================================

impl File for EpollInstance {
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument) // epoll fds are not readable
    }
    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument) // epoll fds are not writable
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
    fn poll_read(&self) -> bool {
        // An epoll fd is itself readable when any of its monitored fds have events.
        // This enables nested epoll (epoll watching another epoll).
        let inner = self.inner.lock();
        inner.interest.values().any(|e| {
            e.armed && {
                let r = readiness_for(&e.fd_obj);
                let m = (e.events & 0x0FFF_FFFF) | EP_PRIVATE_BITS;
                (r & m) != 0
            }
        })
    }
    fn poll_write(&self) -> bool { false }

    fn register_waiter(&self, events: u32, pid: usize, deadline: Option<u64>) {
        // poll/select on an epoll fd — park on ep->wq until nested ready.
        if events & READINESS_READ == 0 {
            return;
        }
        if self.poll_read() {
            return;
        }
        EpollInstance::register_waiter(self, pid);
        let _ = deadline;
    }

    fn attach_poll_hook(&self, hook: alloc::sync::Arc<dyn PollWakeHook>) {
        // Linux ep_item_poll on nested epoll: poll_wait(&ep->poll_wait).
        self.inner.lock().poll_wait.add_poll_hook(hook);
    }

    fn detach_poll_hook(&self, hook_id: usize) {
        self.inner.lock().poll_wait.remove_poll_hook(hook_id);
    }
}

impl Inode for EpollInstance {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
    fn as_epoll(&self) -> Option<&EpollInstance> { Some(self) }
}

// ============================================================================
// Syscall entry points — moved from syscall/fd.rs (P0 architecture refactor)
// ============================================================================

use crate::syscall::user::{read_user_buffer, write_user_buffer, UserSlice};

/// `struct epoll_event` on RISC-V/LA: `__u32 events + __u64 data`
/// naturally aligned = 16 bytes (x86 is packed=12, not supported here).
const EPOLL_EVENT_SIZE: usize = 16;

/// epoll_create1(flags) — create a real EpollInstance fd.
#[allow(dead_code)]
pub fn sys_epoll_create1(flags: usize) -> isize {
    const EPOLL_CLOEXEC: usize = 0o2000000; // O_CLOEXEC
    if flags & !EPOLL_CLOEXEC != 0 { return -22; }
    let instance = EpollInstance::new();
    let fd_obj = Arc::new(FileDescriptor::new(instance, 0o2));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                return match crate::fs::fd::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        if flags & EPOLL_CLOEXEC != 0 {
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

/// Retrieve the `Arc<dyn Inode>` backing `epfd` and verify it is an
/// EpollInstance via `Inode::as_epoll`. The returned Arc keeps the inode
/// alive while the caller uses the `&EpollInstance` reference.
#[allow(dead_code)]
fn epoll_lookup(epfd: usize) -> Option<Arc<dyn Inode>> {
    let fd_obj = crate::syscall::fd::get_fd_obj(epfd)?;
    let inode_arc: Arc<dyn Inode> = fd_obj.file.clone();
    if (&*inode_arc as &dyn Inode).as_epoll().is_none() {
        return None;
    }
    Some(inode_arc)
}

/// Linux `file_can_poll`: a file is epoll-able only if its f_op defines a
/// poll handler. Regular files, directories and block devices do not, so
/// epoll_ctl(EPOLL_CTL_ADD) on them returns EPERM (epoll_ctl02).
fn file_can_poll(ft: FileType) -> bool {
    matches!(
        ft,
        FileType::CharDevice
            | FileType::Pipe
            | FileType::Socket
    )
}

/// epoll_ctl(epfd, op, fd, event) — maintain the interest table.
#[allow(dead_code)]
pub fn sys_epoll_ctl(epfd: usize, op: usize, fd: usize, event: *const u8) -> isize {
    // Linux do_epoll_ctl ordering: a bad epfd or target fd is EBADF (not
    // EINVAL); a target that does not support poll is EPERM; only then is a
    // non-epoll epfd / self-loop reported as EINVAL (epoll_ctl02).
    let ep_fd_obj = match crate::syscall::fd::get_fd_obj(epfd) { Some(f) => f, None => return -9 };
    let target_fd = match crate::syscall::fd::get_fd_obj(fd) { Some(f) => f, None => return -9 };
    if !file_can_poll(target_fd.file.file_type()) {
        return -1; // -EPERM: fd does not support epoll
    }
    let ep_inode: Arc<dyn Inode> = ep_fd_obj.file.clone();
    let ep = match ep_inode.as_epoll() { Some(e) => e, None => return -22 };
    if Arc::ptr_eq(&target_fd.file, &ep_inode) {
        return -22;
    }
    // Linux ep_loop_check: if target is also an epoll, check for cycles and
    // enforce max nesting depth (EP_MAX_NESTS = 4 in Linux).
    // Linux detects cycles and depth in the same traversal; ELOOP has priority.
    if op == EPOLL_CTL_ADD {
        if let Some(target_ep) = target_fd.file.as_epoll() {
            // Cycle detection first (ELOOP takes priority over EINVAL in Linux).
            if target_ep.has_path_to(&ep_inode) {
                return -40; // -ELOOP
            }
            let target_depth = target_ep.nesting_depth();
            if target_depth + 1 > 4 {
                return -22; // -EINVAL: too deep
            }
        }
    }
    let (events, data) = if op == EPOLL_CTL_DEL {
        (0u32, 0u64)
    } else {
        if event.is_null() { return -14; }
        let buf = match UserSlice::<u8>::new(event, EPOLL_EVENT_SIZE).read_bytes() {
            Ok(v) => v,
            Err(_) => return -14,
        };
        let events = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        // data starts at offset 8 (naturally aligned to 8)
        let data = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        (events, data)
    };
    match ep.ctl(op, fd as i32, target_fd, events, data) {
        Ok(()) => 0,
        Err(e) => e as isize,
    }
}

/// epoll_pwait(epfd, events, maxevents, timeout_ms, sigmask, sigsetsize)
/// Scans readiness; blocks when no events and timeout != 0.
#[allow(dead_code)]
pub fn sys_epoll_pwait(
    epfd: usize, events: *mut u8, maxevents: usize, timeout: isize,
    sigmask: *const u8, sigsetsize: usize,
) -> isize {
    if events.is_null() || maxevents == 0 || maxevents > 1024 {
        return -22;
    }
    // Linux set_user_sigmask: a non-NULL sigmask must have the kernel
    // sigset_t size (8) and point to readable memory; a bad pointer is EFAULT
    // (epoll_pwait04), a wrong size is EINVAL.
    let new_mask: Option<u64> = if !sigmask.is_null() {
        if sigsetsize != 8 {
            return -22; // -EINVAL
        }
        match read_user_buffer(sigmask, sigsetsize) {
            Ok(buf) => Some(u64::from_le_bytes(buf[..8].try_into().unwrap())),
            Err(_) => return -14, // -EFAULT
        }
    } else {
        None
    };
    // A bad epfd is EBADF; a valid fd that is not an epoll instance is EINVAL
    // (epoll_wait03).
    let ep_fd_obj = match crate::syscall::fd::get_fd_obj(epfd) { Some(f) => f, None => return -9 };
    let ep_inode: Arc<dyn Inode> = ep_fd_obj.file.clone();
    let ep = match ep_inode.as_epoll() { Some(e) => e, None => return -22 };
    let current_pid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        match table.as_ref().and_then(|t| t.current) { Some(p) => p, None => return -3 }
    };
    // io_wait wakeups replay the whole syscall (sepc rewound to ecall), so the
    // wait deadline must be remembered in the PCB across re-entries — otherwise
    // each replay would push the deadline forward and the timeout never expires
    // (same contract as poll/select's POLL_WAIT_SENTINEL re-entry deadline).
    let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
    let deadline = match take_saved_epoll_deadline(current_pid) {
        Some(saved) => saved,
        None => {
            if timeout < 0 {
                u64::MAX
            } else if timeout == 0 {
                0
            } else {
                crate::arch::asm_helpers::rdtime()
                    .saturating_add((timeout as u64).saturating_mul(clock_freq / 1000))
            }
        }
    };

    // Linux set_user_sigmask: atomically swap signal mask for the duration of
    // the wait, then restore.
    let has_sigmask = new_mask.is_some();
    if let Some(mask) = new_mask {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(p) = t.get_mut(current_pid) {
                if p.sigsuspend_restore_mask.is_none() {
                    p.sigsuspend_restore_mask = Some(p.signal_mask);
                }
                p.signal_mask = mask;
            }
        }
    }

    let result = epoll_pwait_inner(ep, current_pid, events, maxevents, timeout, deadline);

    // Restore original signal mask on normal return
    if has_sigmask {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(p) = t.get_mut(current_pid) {
                if let Some(restore) = p.sigsuspend_restore_mask.take() {
                    p.signal_mask = restore;
                }
            }
        }
    }

    result
}

/// On ecall re-entry after an io_wait wakeup, recover (and clear) the wait
/// deadline stashed in the PCB by `remember_epoll_deadline`.
fn take_saved_epoll_deadline(current_pid: usize) -> Option<u64> {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_mut()?;
    let p = t.get_mut(current_pid)?;
    if p.waiting_for == Some(crate::sched::process::POLL_WAIT_SENTINEL) && p.wakeup_channel > 1 {
        let saved = p.wakeup_channel as u64;
        p.clear_wait_state();
        Some(saved)
    } else {
        None
    }
}

/// Stash the wait deadline in the PCB so a syscall replay can recover it.
/// POLL_WAIT_SENTINEL keeps `wakeup_channel` intact across the deadline wake
/// in `check_sleep_deadlines`.
fn remember_epoll_deadline(current_pid: usize, deadline: u64) {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(t) = table.as_mut() {
        if let Some(p) = t.get_mut(current_pid) {
            p.waiting_for = Some(crate::sched::process::POLL_WAIT_SENTINEL);
            p.wakeup_channel = deadline as usize;
        }
    }
}

fn deliver_epoll_events(
    ep: &EpollInstance,
    current_pid: usize,
    events: *mut u8,
    ready: &[(u32, u64, i32)],
) -> isize {
    ep.clear_waiter(current_pid);
    let mut buf = alloc::vec![0u8; ready.len() * EPOLL_EVENT_SIZE];
    for (i, (mask, data, _fd)) in ready.iter().enumerate() {
        let off = i * EPOLL_EVENT_SIZE;
        buf[off..off + 4].copy_from_slice(&mask.to_le_bytes());
        buf[off + 8..off + 16].copy_from_slice(&data.to_le_bytes());
    }
    if write_user_buffer(events, &buf).is_err() {
        return -14;
    }
    ready.len() as isize
}

fn epoll_pwait_inner(
    ep: &EpollInstance, current_pid: usize,
    events: *mut u8, maxevents: usize, timeout: isize, deadline: u64,
) -> isize {
    loop {
        let ready = ep.collect_ready(maxevents);
        if !ready.is_empty() {
            return deliver_epoll_events(ep, current_pid, events, &ready);
        }
        if timeout == 0 {
            return 0; // non-blocking
        }
        // Signal check: only interrupts the wait, not event delivery
        let intr = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(ref t) = *table {
                crate::kernel::signal::pending_interrupting_signal_for_process(t, current_pid, 0)
            } else { false }
        };
        if intr {
            ep.clear_waiter(current_pid);
            return -4;
        }
        let now = crate::arch::asm_helpers::rdtime();
        if now >= deadline {
            ep.clear_waiter(current_pid);
            return 0;
        }
        // Linux ep_poll: park ONLY on ep->wq. Target fds already have
        // ep_poll_callback attached from epoll_ctl(ADD) / ep_insert.
        ep.register_waiter(current_pid);
        // Lost-wakeup contract: readiness may have arrived between the empty
        // scan and waitqueue arming (Linux prepare_to_wait / schedule window).
        let ready = ep.collect_ready(maxevents);
        if !ready.is_empty() {
            return deliver_epoll_events(ep, current_pid, events, &ready);
        }
        remember_epoll_deadline(current_pid, deadline);
        crate::sched::process::enter_wait_channel("ep_poll");
        crate::sched::process::suspend_current_for_io(deadline);
    }
}
