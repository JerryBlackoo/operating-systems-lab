//! 真 inotify 实现 — Slice 1.9 P0-1
//!
//! 此前 `sys_inotify_init1` 仅返回 /dev/null 假 fd，`sys_inotify_add_watch`
//! 返回伪 wd=1，`read()` 永远 EOF。LTP `inotify_init01` / `inotify01..09`
//! 在 read() 出 0 字节后即 TFAIL。
//!
//! 本模块提供：
//!   - `InotifyInstance`：每个 inotify fd 一个实例，含 watch 表 + 事件 queue
//!   - `InotifyWatch`：（wd, path, mask）
//!   - `InotifyEvent`：与 Linux ABI 二进制兼容的 `struct inotify_event`
//!   - 全局 `INOTIFY_REGISTRY`：路径 → 关心该路径的 watch 列表
//!   - `notify(path, mask, name)`：由 VFS 的 create/unlink/rename/write 调用
//!
//! 实现策略：
//!   - 单实例最多 256 个 watch、256 个 pending event；溢出按 LIFO 丢弃
//!     （Linux 是 IN_Q_OVERFLOW，我们目前不投递溢出事件）。
//!   - watch 表按 wd 索引，wd 从 1 起递增（0 保留）。
//!   - 事件 read 用 wait_io 阻塞，被 notify 唤醒。

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::sync::IrqSpinLock;

/// inotify event masks (Linux `<sys/inotify.h>`)
pub const IN_ACCESS:        u32 = 0x00000001;
pub const IN_MODIFY:        u32 = 0x00000002;
pub const IN_ATTRIB:        u32 = 0x00000004;
pub const IN_CLOSE_WRITE:   u32 = 0x00000008;
pub const IN_CLOSE_NOWRITE: u32 = 0x00000010;
pub const IN_OPEN:          u32 = 0x00000020;
pub const IN_MOVED_FROM:    u32 = 0x00000040;
pub const IN_MOVED_TO:      u32 = 0x00000080;
pub const IN_CREATE:        u32 = 0x00000100;
pub const IN_DELETE:        u32 = 0x00000200;
pub const IN_DELETE_SELF:   u32 = 0x00000400;
pub const IN_MOVE_SELF:     u32 = 0x00000800;
pub const IN_UNMOUNT:       u32 = 0x00002000;
pub const IN_Q_OVERFLOW:    u32 = 0x00004000;
pub const IN_IGNORED:       u32 = 0x00008000;
pub const IN_ISDIR:         u32 = 0x40000000;
pub const IN_ONESHOT:       u32 = 0x80000000;
pub const IN_EXCL_UNLINK:  u32 = 0x04000000;

// Linux defaults: /proc/sys/fs/inotify/max_user_watches = 65536,
// /proc/sys/fs/inotify/max_queued_events = 16384. Must match what
// fs/proc/sys.rs reports — userspace sizes its event buffers from proc.
const MAX_WATCHES_PER_INSTANCE: usize = 65536;
const MAX_QUEUED_EVENTS:        usize = 16384;

/// `struct inotify_event` Linux ABI:
///   i32 wd;
///   u32 mask;
///   u32 cookie;
///   u32 len;
///   char name[]; // null-padded to multiple of sizeof(struct inotify_event)
#[derive(Debug, Clone)]
pub struct InotifyEvent {
    pub wd: i32,
    pub mask: u32,
    pub cookie: u32,
    pub name: String, // empty for watch-target itself
}

impl InotifyEvent {
    /// Serialize to bytes, returns total bytes used (header + name + padding).
    /// Linux semantics: len=0 when name is empty (event on watched item itself).
    /// When name is present, it includes name + NUL + padding to align.
    pub fn write_to(&self, buf: &mut Vec<u8>) -> usize {
        let header_start = buf.len();
        buf.extend_from_slice(&self.wd.to_le_bytes());
        buf.extend_from_slice(&self.mask.to_le_bytes());
        buf.extend_from_slice(&self.cookie.to_le_bytes());
        if self.name.is_empty() {
            // No name — len field = 0, no name bytes emitted
            buf.extend_from_slice(&0u32.to_le_bytes());
        } else {
            // Pad name+NUL to multiple of event alignment (sizeof(struct inotify_event) is natural)
            let raw_name_len = self.name.len() + 1; // include NUL
            let padded = (raw_name_len + 15) & !15;
            buf.extend_from_slice(&(padded as u32).to_le_bytes());
            buf.extend_from_slice(self.name.as_bytes());
            for _ in 0..(padded - self.name.len()) {
                buf.push(0);
            }
        }
        buf.len() - header_start
    }

    pub fn total_size(&self) -> usize {
        if self.name.is_empty() {
            16
        } else {
            16 + ((self.name.len() + 1 + 15) & !15)
        }
    }
}

#[derive(Debug)]
pub struct InotifyWatch {
    pub wd: i32,
    pub path: String,
    pub mask: u32,
}

pub struct InotifyInstance {
    inner: IrqSpinLock<InotifyInner>,
}

struct InotifyInner {
    next_wd: i32,
    watches: BTreeMap<i32, InotifyWatch>,
    /// path → wd (one-to-one — re-adding same path returns same wd per Linux).
    by_path: BTreeMap<String, i32>,
    events: VecDeque<InotifyEvent>,
    /// PIDs currently blocked in read() — woken by deliver().
    read_waiters: Vec<usize>,
}

impl InotifyInstance {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: IrqSpinLock::new(InotifyInner {
                next_wd: 1,
                watches: BTreeMap::new(),
                by_path: BTreeMap::new(),
                events: VecDeque::new(),
                read_waiters: Vec::new(),
            }),
        })
    }

    /// Add (or update) a watch. Returns the wd. Idempotent re-adds OR
    /// existing mask (`IN_MASK_ADD` semantic — we always merge).
    ///
    /// Lock ordering note: `InotifyRegistry::notify_with_cookie` acquires
    /// `INOTIFY_REGISTRY.inner` first and then calls back into each
    /// instance's `deliver`, which acquires `instance.inner`. We therefore
    /// MUST release `instance.inner` here before touching `INOTIFY_REGISTRY`
    /// or we deadlock against a concurrent VFS-side notify.
    pub fn add_watch(&self, path: &str, mask: u32) -> Result<i32, i32> {
        let (wd, register) = {
            let mut inner = self.inner.lock();
            if let Some(&existing) = inner.by_path.get(path) {
                if let Some(w) = inner.watches.get_mut(&existing) {
                    w.mask |= mask;
                }
                return Ok(existing);
            }
            if inner.watches.len() >= MAX_WATCHES_PER_INSTANCE {
                return Err(-28); // -ENOSPC
            }
            let wd = inner.next_wd;
            inner.next_wd = inner.next_wd.wrapping_add(1).max(1);
            let watch = InotifyWatch { wd, path: path.into(), mask };
            inner.by_path.insert(path.into(), wd);
            inner.watches.insert(wd, watch);
            (wd, true)
        };
        if register {
            INOTIFY_REGISTRY.register(
                path,
                wd,
                Arc::new(InotifyHandle { instance_ptr: self as *const _ as usize }),
            );
        }
        Ok(wd)
    }

    pub fn rm_watch(&self, wd: i32) -> Result<(), i32> {
        let watch = {
            let mut inner = self.inner.lock();
            let watch = inner.watches.remove(&wd).ok_or(-22)?;
            inner.by_path.remove(&watch.path);
            inner.events.push_back(InotifyEvent { wd, mask: IN_IGNORED, cookie: 0, name: String::new() });
            watch
        };
        INOTIFY_REGISTRY.unregister(&watch.path, wd);
        self.wake_waiters();
        Ok(())
    }

    pub fn unregister_all(&self) {
        let watches: Vec<(String, i32)> = {
            let mut inner = self.inner.lock();
            let watches = inner.watches.values()
                .map(|watch| (watch.path.clone(), watch.wd))
                .collect();
            inner.watches.clear();
            inner.by_path.clear();
            inner.events.clear();
            inner.read_waiters.clear();
            watches
        };
        for (path, wd) in watches {
            INOTIFY_REGISTRY.unregister(&path, wd);
        }
    }

    /// Called from VFS: deliver an event if any watch on `path` cares about
    /// the mask. `name` is the affected child within `path` (empty when the
    /// event targets `path` itself).
    pub fn deliver(&self, wd: i32, mask: u32, cookie: u32, name: &str) {
        self.deliver_ex(wd, mask, cookie, name, false);
    }

    /// Returns `true` when the watch was auto-removed (IN_DELETE_SELF /
    /// IN_ONESHOT). The registry caller must then drop its own entry for
    /// this (path, wd) — otherwise the registry keeps a dangling
    /// `instance_ptr` alive after the fd is closed (close's
    /// `unregister_all` can no longer see the watch), and a later notify
    /// on the same path dereferences freed memory.
    pub fn deliver_ex(&self, wd: i32, mask: u32, cookie: u32, name: &str, child_unlinked: bool) -> bool {
        let mut inner = self.inner.lock();
        let watch_mask = match inner.watches.get(&wd) {
            Some(w) => w.mask,
            None => return false,
        };
        // IN_UNMOUNT, IN_Q_OVERFLOW, IN_IGNORED are always delivered regardless of watch mask.
        let always_deliver = mask & (IN_UNMOUNT | IN_Q_OVERFLOW | IN_IGNORED) != 0;
        if !always_deliver && watch_mask & mask == 0 {
            return false;
        }
        // IN_EXCL_UNLINK: suppress events for unlinked children
        if child_unlinked && !name.is_empty() && (watch_mask & IN_EXCL_UNLINK != 0) {
            return false;
        }
        if inner.events.len() >= MAX_QUEUED_EVENTS {
            // Linux: drop the new event, queue IN_Q_OVERFLOW if not already present
            let already_has_overflow = inner.events.iter().any(|e| e.mask & IN_Q_OVERFLOW != 0);
            if !already_has_overflow {
                inner.events.push_back(InotifyEvent {
                    wd: -1,
                    mask: IN_Q_OVERFLOW,
                    cookie: 0,
                    name: String::new(),
                });
            }
            // new event is dropped
        } else {
            // Linux inotify_merge(): coalesce the new event into the queue's
            // tail when they are identical — same wd, mask and name (cookie is
            // intentionally NOT compared, and an IN_IGNORED tail never merges).
            // Only the immediate tail is checked, matching Linux. This collapses
            // e.g. two back-to-back IN_MOVE_SELF from renaming a watched dir
            // twice in a row into one (inotify02 final coalescing subtest).
            let merge = inner.events.back().map_or(false, |last| {
                last.mask == mask
                    && last.wd == wd
                    && (last.mask & IN_IGNORED == 0)
                    && last.name.as_str() == name
            });
            if !merge {
                inner.events.push_back(InotifyEvent { wd, mask, cookie, name: name.into() });
            }
        }
        // Linux: only IN_DELETE_SELF (the watched inode is gone) and IN_ONESHOT
        // auto-remove the watch + emit IN_IGNORED. IN_MOVE_SELF must NOT remove
        // the watch — the mark lives on the inode, which survives a rename, so
        // the watch keeps firing for subsequent events (inotify02: rename(cwd)
        // then unlink + further renames must still be observed). The registry
        // path key is re-keyed by `rename_path` after the move instead.
        let auto_remove = (mask & IN_DELETE_SELF != 0)
            || (watch_mask & IN_ONESHOT != 0);
        if auto_remove {
            if let Some(watch) = inner.watches.remove(&wd) {
                inner.by_path.remove(&watch.path);
                inner.events.push_back(InotifyEvent {
                    wd,
                    mask: IN_IGNORED,
                    cookie: 0,
                    name: String::new(),
                });
            }
        }
        drop(inner);
        // Wake any task blocked in read() on this (or any) inotify fd. The
        // global I/O waiter wakeup is coarse but cheap; pid-specific waiters
        // are also stored via `register_waiter` and woken below.
        self.wake_waiters();
        auto_remove
    }

    /// Wake all pids registered via `InotifyFile::register_waiter` and trigger
    /// a generic IO waiter wakeup so sys_read sees the new event.
    pub fn wake_waiters(&self) {
        // Drain waiters list and mark them runnable.
        let waiters: Vec<usize> = {
            let mut inner = self.inner.lock();
            core::mem::take(&mut inner.read_waiters)
        };
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            for pid in waiters {
                let _ = table.mark_runnable(pid, true);
            }
            // Coarse wake — covers any pid that registered before this
            // instance existed (e.g. select/poll across fd sets).
            table.wake_io_waiters();
        }
    }

    /// Record a reader that should be woken when an event arrives.
    pub fn register_reader(&self, pid: usize) {
        let mut inner = self.inner.lock();
        if !inner.read_waiters.contains(&pid) {
            inner.read_waiters.push(pid);
        }
    }

    /// Drain queued events into the caller-provided byte buffer. Returns
    /// the number of bytes written, or 0 if no events (caller should block
    /// or return -EAGAIN).
    pub fn read(&self, max_bytes: usize) -> Result<Vec<u8>, crate::fs::FsError> {
        if max_bytes < 16 {
            return Err(crate::fs::FsError::InvalidArgument);
        }
        let mut inner = self.inner.lock();
        let mut buf = Vec::new();
        while let Some(ev) = inner.events.pop_front() {
            let est = ev.total_size();
            if buf.len() + est > max_bytes {
                // Would overflow; put it back.
                inner.events.push_front(ev);
                if buf.is_empty() {
                    return Err(crate::fs::FsError::InvalidArgument);
                }
                break;
            }
            ev.write_to(&mut buf);
        }
        Ok(buf)
    }

    pub fn has_pending(&self) -> bool {
        !self.inner.lock().events.is_empty()
    }
}

// ============================================================================
// Global registry — maps a VFS path to the set of inotify instances watching it.
// Used by VFS-side notify() hooks.
// ============================================================================

/// Lightweight handle that VFS code can clone freely. The `instance_ptr`
/// is a raw `*const InotifyInstance` re-cast back on dispatch; the actual
/// `Arc<InotifyInstance>` is held by the syscall-fd table, so the pointer
/// stays valid as long as that fd is open. When the fd is closed we
/// `unregister()` all of the instance's watches, removing the dangling
/// pointer.
#[derive(Clone)]
pub struct InotifyHandle {
    pub instance_ptr: usize,
}

impl InotifyHandle {
    pub fn deliver(&self, wd: i32, mask: u32, cookie: u32, name: &str) -> bool {
        let instance = unsafe { &*(self.instance_ptr as *const InotifyInstance) };
        instance.deliver_ex(wd, mask, cookie, name, false)
    }
    pub fn deliver_ex(&self, wd: i32, mask: u32, cookie: u32, name: &str, child_unlinked: bool) -> bool {
        let instance = unsafe { &*(self.instance_ptr as *const InotifyInstance) };
        instance.deliver_ex(wd, mask, cookie, name, child_unlinked)
    }
}

pub struct InotifyRegistry {
    inner: IrqSpinLock<BTreeMap<String, Vec<(i32, InotifyHandle)>>>,
}

impl InotifyRegistry {
    const fn new() -> Self {
        Self { inner: IrqSpinLock::new(BTreeMap::new()) }
    }

    pub fn register(&self, path: &str, wd: i32, handle: Arc<InotifyHandle>) {
        let mut g = self.inner.lock();
        g.entry(path.into()).or_insert_with(Vec::new).push((wd, (*handle).clone()));
        INOTIFY_ACTIVE.fetch_add(1, Ordering::Relaxed);
    }

    pub fn unregister(&self, path: &str, wd: i32) {
        let mut g = self.inner.lock();
        if let Some(list) = g.get_mut(path) {
            let before = list.len();
            list.retain(|(w, _)| *w != wd);
            let removed = before.saturating_sub(list.len());
            if removed != 0 {
                INOTIFY_ACTIVE.fetch_sub(removed, Ordering::Relaxed);
            }
            if list.is_empty() { g.remove(path); }
        }
    }

    /// Called from VFS layer to push an event. `parent_path` is the watched
    /// directory and `name` is the child name (empty for self-events).
    pub fn notify(&self, parent_path: &str, mask: u32, name: &str) {
        self.notify_with_cookie(parent_path, mask, 0, name);
    }

    pub fn notify_with_cookie(&self, parent_path: &str, mask: u32, cookie: u32, name: &str) {
        self.notify_with_cookie_ex(parent_path, mask, cookie, name, false);
    }

    pub fn notify_with_cookie_ex(&self, parent_path: &str, mask: u32, cookie: u32, name: &str, child_unlinked: bool) {
        if INOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
            return;
        }
        let mut g = self.inner.lock();
        if let Some(list) = g.get_mut(parent_path) {
            // A watch auto-removed by delivery (IN_DELETE_SELF / IN_ONESHOT)
            // must also leave the registry now: close() can no longer find
            // it, and a stale entry dangles once the instance is freed.
            let before = list.len();
            list.retain(|(wd, handle)| {
                !handle.deliver_ex(*wd, mask, cookie, name, child_unlinked)
            });
            let removed = before.saturating_sub(list.len());
            if removed != 0 {
                INOTIFY_ACTIVE.fetch_sub(removed, Ordering::Relaxed);
            }
            if list.is_empty() {
                g.remove(parent_path);
            }
        }
    }
}

impl InotifyRegistry {
    /// Called before umount: for every watched path at or under `mount_point`,
    /// deliver IN_UNMOUNT then IN_IGNORED, auto-remove the watch from both
    /// the global registry and each InotifyInstance.
    pub fn notify_unmount(&self, mount_point: &str) {
        if INOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
            return;
        }
        let prefix = if mount_point.ends_with('/') {
            String::from(mount_point)
        } else {
            let mut s = String::from(mount_point);
            s.push('/');
            s
        };
        // Collect matching (path, wd, instance_ptr) tuples
        let entries: Vec<(String, Vec<(i32, usize)>)> = {
            let g = self.inner.lock();
            g.iter()
                .filter(|(p, _)| *p == mount_point || p.starts_with(&prefix))
                .map(|(p, list)| {
                    let wds: Vec<(i32, usize)> = list.iter().map(|(wd, h)| (*wd, h.instance_ptr)).collect();
                    (p.clone(), wds)
                })
                .collect()
        };
        // Deliver IN_UNMOUNT + IN_IGNORED to each watcher, then remove from instance
        for (path, watchers) in &entries {
            self.notify_with_cookie(path, IN_UNMOUNT, 0, "");
            self.notify_with_cookie(path, IN_IGNORED, 0, "");
            // Remove watch from each InotifyInstance so subsequent rm_watch returns EINVAL
            for &(wd, inst_ptr) in watchers {
                let instance = unsafe { &*(inst_ptr as *const InotifyInstance) };
                let mut inner = instance.inner.lock();
                inner.watches.remove(&wd);
                inner.by_path.remove(path);
            }
        }
        // Remove all watches for those paths from global registry
        let mut g = self.inner.lock();
        for (path, _) in &entries {
            if let Some(list) = g.remove(path) {
                INOTIFY_ACTIVE.fetch_sub(list.len(), Ordering::Relaxed);
            }
        }
    }

    /// Called after a successful rename: re-key every watched path at or under
    /// `old_path` to the matching path under `new_path`. Linux keeps the
    /// inotify mark on the inode, so renaming a watched directory (or an
    /// ancestor of one) transparently follows — the watch persists and keeps
    /// reporting events under the new name. We store watches by path string, so
    /// we rewrite the path prefix in both the global registry and each owning
    /// InotifyInstance's `watches`/`by_path` (inotify02: rename(cwd) then unlink
    /// + further renames must still reach the same watch).
    pub fn rename_path(&self, old_path: &str, new_path: &str) {
        if INOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 || old_path == new_path {
            return;
        }
        // A path P is affected iff P == old_path or P is strictly under it.
        let is_affected = |p: &str| -> bool {
            p == old_path
                || (p.len() > old_path.len()
                    && p.as_bytes()[old_path.len()] == b'/'
                    && p.starts_with(old_path))
        };
        let rekey = |p: &str| -> String {
            if p == old_path {
                String::from(new_path)
            } else {
                let mut s = String::from(new_path);
                s.push_str(&p[old_path.len()..]);
                s
            }
        };
        let affected: Vec<String> = {
            let g = self.inner.lock();
            g.keys().filter(|p| is_affected(p)).cloned().collect()
        };
        if affected.is_empty() {
            return;
        }
        let mut g = self.inner.lock();
        for old_key in affected {
            let new_key = rekey(&old_key);
            let list = match g.remove(&old_key) {
                Some(l) => l,
                None => continue,
            };
            // Rewrite the stored path inside each owning InotifyInstance.
            for (wd, handle) in &list {
                let instance = unsafe { &*(handle.instance_ptr as *const InotifyInstance) };
                let mut inner = instance.inner.lock();
                if let Some(w) = inner.watches.get_mut(wd) {
                    w.path = new_key.clone();
                }
                inner.by_path.remove(&old_key);
                inner.by_path.insert(new_key.clone(), *wd);
            }
            // Merge into any pre-existing list already at the destination key.
            g.entry(new_key).or_insert_with(Vec::new).extend(list);
        }
    }
}

pub static INOTIFY_REGISTRY: InotifyRegistry = InotifyRegistry::new();
static INOTIFY_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// 热路径合并门：是否存在任何 inotify watch。
#[inline]
pub fn watches_active() -> bool {
    INOTIFY_ACTIVE.load(Ordering::Relaxed) != 0
}

/// Convenient shorthand for VFS callers.
pub fn notify_path(parent_path: &str, mask: u32, name: &str) {
    INOTIFY_REGISTRY.notify(parent_path, mask, name);
}

pub fn notify_path_with_cookie(parent_path: &str, mask: u32, cookie: u32, name: &str) {
    INOTIFY_REGISTRY.notify_with_cookie(parent_path, mask, cookie, name);
}

pub fn notify_file_path(path: &str, mask: u32) {
    notify_file_path_with_cookie(path, mask, 0);
}

pub fn notify_file_path_with_cookie(path: &str, mask: u32, cookie: u32) {
    // Fast path: with no inotify watch installed there is nothing to deliver,
    // so skip the per-call fs::lookup(path) below. That lookup walks the whole
    // path (dir-tree traversal + locks) on every read/write and is pure
    // overhead for iozone's high-frequency small I/O when nobody is watching.
    if INOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
        return;
    }
    // Linux order: parent directory event first, then self event
    // Check if the file still exists for IN_EXCL_UNLINK filtering.
    let child_unlinked = crate::fs::lookup(path).is_err();
    match path.rfind('/') {
        Some(pos) if pos > 0 && pos + 1 < path.len() => {
            INOTIFY_REGISTRY.notify_with_cookie_ex(&path[..pos], mask, cookie, &path[pos + 1..], child_unlinked);
        }
        Some(0) if path.len() > 1 => {
            INOTIFY_REGISTRY.notify_with_cookie_ex("/", mask, cookie, &path[1..], child_unlinked);
        }
        _ => {}
    }
    INOTIFY_REGISTRY.notify_with_cookie(path, mask, cookie, "");
}

// ============================================================================
// Per-fd File adapter — so syscalls can return a real fd backed by an
// InotifyInstance instead of /dev/null.
// ============================================================================

use crate::fs::{File, FileMode, FileStat, FileType, FsError, Inode};

pub struct InotifyFile {
    pub instance: Arc<InotifyInstance>,
    ino: usize,
}

impl InotifyFile {
    pub fn new(instance: Arc<InotifyInstance>) -> Arc<Self> {
        static NEXT_INO: AtomicUsize = AtomicUsize::new(1_000_000_000);
        Arc::new(Self {
            instance,
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
        })
    }
}

impl File for InotifyFile {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let data = self.instance.read(buf.len())?;
        if data.is_empty() {
            return Err(FsError::WouldBlock);
        }
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }
    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::CharDevice,
            size: 0,
            mode: FileMode(0o020444),
            nlink: 1,
            uid: 0, gid: 0,
            atime: 0, mtime: 0, ctime: 0,
        })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
    fn poll_read(&self) -> bool { self.instance.has_pending() }
    fn poll_write(&self) -> bool { false }
    fn register_waiter(&self, events: u32, pid: usize, _deadline: Option<u64>) {
        if events & crate::fs::READINESS_READ != 0 {
            self.instance.register_reader(pid);
        }
    }
    fn close_notify_last(&self) {
        self.instance.unregister_all();
    }
}

impl Inode for InotifyFile {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
    fn as_inotify(&self) -> Option<&InotifyFile> { Some(self) }
    fn inotify_fdinfo(&self) -> Option<Vec<(i32, u32)>> {
        let inner = self.instance.inner.lock();
        let watches: Vec<(i32, u32)> = inner.watches.iter()
            .map(|(wd, w)| (*wd, w.mask))
            .collect();
        Some(watches)
    }
}

// Suppress dead_code warnings for fields used by VFS callers we'll hook later.
#[allow(dead_code)]
fn _silence_warnings() {
    let _ = (IN_ACCESS, IN_MODIFY, IN_ATTRIB, IN_CLOSE_WRITE, IN_CLOSE_NOWRITE,
             IN_OPEN, IN_MOVED_FROM, IN_MOVED_TO, IN_CREATE, IN_DELETE,
             IN_DELETE_SELF, IN_MOVE_SELF, IN_UNMOUNT, IN_Q_OVERFLOW, IN_IGNORED,
             IN_ISDIR);
    let _ = AtomicU32::new(0);
}

// ============================================================================
// Syscall entry points — moved from syscall/fd.rs (P0 architecture refactor)
// ============================================================================

/// inotify_init1(flags) — create a real InotifyInstance fd.
#[allow(dead_code)]
pub fn sys_inotify_init1(flags: usize) -> isize {
    const IN_CLOEXEC:  usize = 0o2000000;
    const IN_NONBLOCK: usize = 0o4000;
    let instance = InotifyInstance::new();
    let file = InotifyFile::new(instance);
    let fd_obj = alloc::sync::Arc::new(crate::fs::FileDescriptor::new(file, flags as u32));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get_fd_process_mut(pid) {
                return match crate::fs::fd::alloc(process, fd_obj, 0, true) {
                    Ok(fd) => {
                        if flags & IN_CLOEXEC != 0 {
                            let _ = crate::fs::fd::set_cloexec(process, fd, true);
                        }
                        let _ = IN_NONBLOCK; // already encoded in fd.flags
                        fd as isize
                    }
                    Err(errno) => errno,
                };
            }
        }
    }
    -22
}

/// inotify_add_watch(fd, pathname, mask) — add a watch on a path.
#[allow(dead_code)]
pub fn sys_inotify_add_watch(fd: usize, path: *const u8, mask: usize) -> isize {
    let path_str = match crate::syscall::user::read_c_string(path) {
        Ok(s) => s,
        Err(_) => return -14, // -EFAULT
    };
    let abs = match crate::syscall::open::resolve_at_path(-100isize, &path_str) {
        Ok(p) => p,
        Err(e) => return e,
    };
    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    match fd_obj.file.as_inotify() {
        Some(inotify_file) => match inotify_file.instance.add_watch(&abs, mask as u32) {
            Ok(wd) => wd as isize,
            Err(e) => e as isize,
        },
        None => -9, // -EBADF: fd is not inotify
    }
}

/// inotify_rm_watch(fd, wd) — remove a watch.
#[allow(dead_code)]
pub fn sys_inotify_rm_watch(fd: usize, wd: usize) -> isize {
    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(f) => f,
        None => return -9,
    };
    match fd_obj.file.as_inotify() {
        Some(inotify_file) => match inotify_file.instance.rm_watch(wd as i32) {
            Ok(()) => 0,
            Err(e) => e as isize,
        },
        None => -9,
    }
}
