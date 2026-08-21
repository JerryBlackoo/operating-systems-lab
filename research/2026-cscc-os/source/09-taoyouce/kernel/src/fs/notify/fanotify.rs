//! fanotify 真 watch + event queue — Slice 2.x T1-3
//!
//! 与 `crate::fs::inotify` 类似的设计，但 fanotify 关注 file 访问 /
//! 权限事件（FAN_ACCESS / FAN_MODIFY / FAN_OPEN_PERM 等）。
//!
//! Linux `struct fanotify_event_metadata`：
//!   u32 event_len;
//!   u8  vers;
//!   u8  reserved;
//!   u16 metadata_len;
//!   u64 mask;
//!   i32 fd;
//!   i32 pid;
//! 共 24 字节。
//!
//! 真化〔去 "不携带 fd, 简化" 注释〕：
//!   - `FanotifyInstance` 持 watch 列表 + event queue
//!   - `fanotify_init` 创建实例（真 FanotifyFile inode + 进程 fd 表注册）
//!   - `fanotify_mark` 注册路径（FAN_MARK_ADD / REMOVE / FLUSH 真做）
//!   - VFS hook 真投递 FAN_ACCESS / FAN_MODIFY / FAN_OPEN / FAN_CREATE /
//!     FAN_DELETE / FAN_MOVED_FROM / FAN_MOVED_TO / FAN_ATTRIB / FAN_OPEN_PERM
//!   - read(fd) 排出 24-byte metadata，**fd 由 `allocate_event_fd` 真在
//!     reader 进程 fd 表里分配**（指向触发事件的 inode），与 Linux
//!     `fanotify_get_response` + `create_fd` 等价；FAN_Q_OVERFLOW 时 fd = -1
//!   - write(fd) 真按 8B `struct fanotify_response { fd, response }` 处理
//!     FAN_ALLOW / FAN_DENY perm-event 回执，支持批量
//!
//! LTP `fanotify_init0[1-9]` + `fanotify_mark0[1-9]` 主路径覆盖完整。

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sync::IrqSpinLock;

pub const FAN_ACCESS:      u64 = 0x0000_0001;
pub const FAN_MODIFY:      u64 = 0x0000_0002;
pub const FAN_ATTRIB:      u64 = 0x0000_0004;
pub const FAN_CLOSE_WRITE: u64 = 0x0000_0008;
pub const FAN_CLOSE_NOWRITE: u64 = 0x0000_0010;
pub const FAN_OPEN:        u64 = 0x0000_0020;
pub const FAN_MOVED_FROM:  u64 = 0x0000_0040;
pub const FAN_MOVED_TO:    u64 = 0x0000_0080;
pub const FAN_CREATE:      u64 = 0x0000_0100;
pub const FAN_DELETE:      u64 = 0x0000_0200;
pub const FAN_DELETE_SELF: u64 = 0x0000_0400;
pub const FAN_MOVE_SELF:   u64 = 0x0000_0800;
pub const FAN_OPEN_EXEC:   u64 = 0x0000_1000;
pub const FAN_Q_OVERFLOW:  u64 = 0x0000_4000;
pub const FAN_OPEN_PERM:   u64 = 0x0001_0000;
pub const FAN_ACCESS_PERM: u64 = 0x0002_0000;
pub const FAN_OPEN_EXEC_PERM: u64 = 0x0004_0000;
pub const FAN_RENAME:      u64 = 0x1000_0000;
pub const FAN_EVENT_ON_CHILD: u64 = 0x0800_0000;
pub const FAN_ONDIR:       u64 = 0x4000_0000;
/// Dirent-class events: reported against the parent directory (Linux
/// FANOTIFY_DIRENT_EVENTS).
pub const FAN_DIRENT_EVENTS: u64 = FAN_MOVED_FROM | FAN_MOVED_TO | FAN_CREATE | FAN_DELETE;
/// Self-class events: reported against the object itself with no parent
/// directory info (Linux fsnotify hooks pass dir=NULL for these).
pub const FAN_SELF_EVENTS: u64 = FAN_DELETE_SELF | FAN_MOVE_SELF;

/// Merge identity of a queued event (Linux `fanotify_should_merge`): pid,
/// FAN_ONDIR/FAN_RENAME split, and the exact info records the event will
/// serialize (or the path for legacy fd-mode groups). Events whose records
/// differ never merge; equal-record events OR their masks.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MergeKey {
    pid: i32,
    ondir: bool,
    rename: bool,
    dir_fh: Option<(u64, Option<String>)>,
    dir2_fh: Option<(u64, String)>,
    obj_fh: Option<u64>,
    path: Option<String>,
}

/// Reconstruct the merge key of a queued event from its stored record plan,
/// so read() can drop/restore the coalescing index.
fn event_merge_key(ev: &FanotifyEvent, fid_mode: bool) -> MergeKey {
    MergeKey {
        pid: ev.pid,
        ondir: ev.mask & FAN_ONDIR != 0,
        rename: ev.mask & FAN_RENAME != 0,
        dir_fh: ev.dir_fh.clone(),
        dir2_fh: ev.dir2_fh.clone(),
        obj_fh: ev.obj_fh,
        path: if fid_mode { None } else { ev.path.clone() },
    }
}

/// fanotify_init flag: queue without the per-group `max_queued_events` cap.
pub const FAN_UNLIMITED_QUEUE: u32 = 0x0000_0010;
/// Default per-group event-queue cap for groups without FAN_UNLIMITED_QUEUE.
/// Matches Linux `/proc/sys/fs/fanotify/max_queued_events` default (16384).
/// When the queue is full a single FAN_Q_OVERFLOW sentinel is delivered and
/// further events are dropped until it is read.
pub const FAN_MAX_QUEUED_EVENTS: usize = 16384;

/// fanotify_init flags that request file-handle (FID) reporting instead of an
/// open fd in the event. When either bit is set the group reports events in
/// the `FAN_REPORT_FID` ABI: metadata.fd == FAN_NOFD followed by a
/// `fanotify_event_info_fid` record (fsid + file_handle).
pub const FAN_REPORT_FID:     u32 = 0x0000_0200;
pub const FAN_REPORT_DIR_FID: u32 = 0x0000_0400;
/// When set, FID-mode events append the triggering object's basename after the
/// directory file handle. Combined with `FAN_REPORT_DIR_FID` this is the
/// `FAN_REPORT_DFID_NAME` ABI: the info record carries the parent directory's
/// handle plus the child name (fanotify09 test #5).
pub const FAN_REPORT_NAME:    u32 = 0x0000_0800;
/// When set in a group's init flags, events carry the triggering thread's tid
/// in `metadata.pid`; otherwise the thread-group leader pid (tgid) is reported.
pub const FAN_REPORT_TID:     u32 = 0x0000_0100;
/// When set, each read event is followed by a `fanotify_event_info_pidfd`
/// record (info_type=4) carrying a pidfd for the triggering process, or one of
/// the FAN_NOPIDFD/FAN_EPIDFD sentinels when no pidfd could be created
/// (fanotify21).
pub const FAN_REPORT_PIDFD:   u32 = 0x0000_0080;
/// When set (requires FAN_REPORT_DFID_NAME_FID), dirent events additionally
/// carry the created/deleted/moved child's own fid record (Linux 5.17+).
pub const FAN_REPORT_TARGET_FID: u32 = 0x0000_1000;
/// `metadata.fd` value used in FID-mode events (no open descriptor).
pub const FAN_NOFD: i32 = -1;
/// pidfd info record sentinel: the triggering process was already gone when the
/// event was read, so no pidfd could be created (Linux `FAN_NOPIDFD`).
pub const FAN_NOPIDFD: i32 = -1;
/// pidfd info record sentinel: pidfd creation failed for some other reason
/// (Linux `FAN_EPIDFD`).
pub const FAN_EPIDFD: i32 = -2;
/// `fanotify_event_info_header.info_type` for a target-object fid record.
pub const FAN_EVENT_INFO_TYPE_FID: u8 = 1;
/// `fanotify_event_info_header.info_type` for a parent-directory fid record
/// carrying the child name appended after the file handle.
pub const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
/// `fanotify_event_info_header.info_type` for a bare directory fid record
/// (FAN_REPORT_DIR_FID without a recorded name).
pub const FAN_EVENT_INFO_TYPE_DFID: u8 = 3;
/// `fanotify_event_info_header.info_type` for a pidfd record (FAN_REPORT_PIDFD).
pub const FAN_EVENT_INFO_TYPE_PIDFD: u8 = 4;
/// FAN_RENAME info records: old parent dir fid + old name, new parent dir fid
/// + new name.
pub const FAN_EVENT_INFO_TYPE_OLD_DFID_NAME: u8 = 10;
pub const FAN_EVENT_INFO_TYPE_NEW_DFID_NAME: u8 = 12;
/// File-handle payload size produced by `name_to_handle_at` (8-byte inode).
const FHANDLE_BYTES: u32 = 8;
const FHANDLE_TYPE:  i32 = 1;

pub const FAN_MARK_ADD:    u32 = 0x0000_0001;
pub const FAN_MARK_REMOVE: u32 = 0x0000_0002;
pub const FAN_MARK_FLUSH:  u32 = 0x0000_0080;
pub const FAN_MARK_FILESYSTEM: u32 = 0x0000_0100;
pub const FAN_MARK_DONT_FOLLOW: u32 = 0x0000_0004;
pub const FAN_MARK_ONLYDIR:     u32 = 0x0000_0008;
pub const FAN_MARK_MOUNT:       u32 = 0x0000_0010;
pub const FAN_MARK_INODE:       u32 = 0x0000_0000;
/// Add `mask` to the mark's *ignore* set instead of its watch set: matching
/// event types are suppressed for this object.
pub const FAN_MARK_IGNORED_MASK:        u32 = 0x0000_0020;
/// Keep the ignore set across FAN_MODIFY events. Without it, a modify event
/// clears the mark's ignore set (Linux fsnotify drops non-surviving ignores).
pub const FAN_MARK_IGNORED_SURV_MODIFY: u32 = 0x0000_0040;
/// Mark may be evicted from the inode cache under memory pressure (we never
/// evict, so this behaves like a normal ignore mark for our purposes).
pub const FAN_MARK_EVICTABLE:   u32 = 0x0000_0200;
/// Linux 5.19+ ignore mark with stricter semantics than FAN_MARK_IGNORED_MASK:
/// the ignore set honours FAN_EVENT_ON_CHILD in the *ignore* mark itself (the
/// legacy flag instead keys child-application off the watch mark). Both flags
/// add `mask` to the mark's ignore set.
pub const FAN_MARK_IGNORE:      u32 = 0x0000_0400;
/// Any flag that means "add `mask` to the ignore set" rather than the watch set.
pub const FAN_MARK_IGNORE_ANY:  u32 = FAN_MARK_IGNORED_MASK | FAN_MARK_IGNORE;

#[derive(Debug, Clone)]
pub struct FanotifyEvent {
    pub mask: u64,
    pub fd: i32,
    pub pid: i32,
    pub path: Option<String>,
    /// FID 模式记录计划（投递时按 Linux `fanotify_alloc_event` 语义固化，
    /// 避免 read 时对已删除对象 lookup 失败——fanotify15）：
    /// dir_fh = 目录 fid 记录（ino + 记录的 name，None = 无名 DFID 记录）；
    /// dir2_fh = FAN_RENAME 的新父目录 fid + 新名；
    /// obj_fh = 对象自身 fid 记录。序列化时的 info_type 按
    /// `copy_info_records_to_user` 规则在 read 时决定。
    pub dir_fh: Option<(u64, Option<String>)>,
    pub dir2_fh: Option<(u64, String)>,
    pub obj_fh: Option<u64>,
}

impl FanotifyEvent {
    /// 序列化为 24 字节 `struct fanotify_event_metadata`。
    pub fn to_bytes(&self) -> [u8; 24] {
        let mut out = [0u8; 24];
        let event_len: u32 = 24;
        out[0..4].copy_from_slice(&event_len.to_le_bytes());
        out[4] = 3; // FANOTIFY_METADATA_VERSION
        out[5] = 0; // reserved
        let metadata_len: u16 = 24;
        out[6..8].copy_from_slice(&metadata_len.to_le_bytes());
        out[8..16].copy_from_slice(&self.mask.to_le_bytes());
        out[16..20].copy_from_slice(&self.fd.to_le_bytes());
        out[20..24].copy_from_slice(&self.pid.to_le_bytes());
        out
    }
}

#[derive(Debug, Clone)]
pub struct FanotifyMark {
    pub path: String,
    pub mask: u64,
    pub flags: u32,
    /// Event types to suppress (FAN_MARK_IGNORED_MASK / FAN_MARK_IGNORE).
    pub ignored_mask: u64,
    /// Whether `ignored_mask` survives FAN_MODIFY (FAN_MARK_IGNORED_SURV_MODIFY).
    pub ignored_surv_modify: bool,
    /// True when the ignore set was installed via the newer FAN_MARK_IGNORE
    /// (Linux 5.19+) rather than legacy FAN_MARK_IGNORED_MASK. The two differ
    /// in how the ignore applies to events on children (see `ignored_onchild`).
    pub ignore_is_new: bool,
    /// Whether the ignore set applies to events on children. For the new
    /// FAN_MARK_IGNORE this tracks FAN_EVENT_ON_CHILD on the *ignore* mark
    /// itself; for legacy FAN_MARK_IGNORED_MASK child-application is keyed off
    /// the watch mark's FAN_EVENT_ON_CHILD instead and this stays false.
    pub ignored_onchild: bool,
    /// Mark scope: 0 = inode (default), 1 = mount, 2 = filesystem. A group may
    /// hold marks of different types on the same path (e.g. an inode watch and
    /// a mount ignore); they are stored separately and merged at delivery.
    pub mark_type: u8,
    /// Inode number of the marked object (inode/parent marks). Bind-mount
    /// aliases of the same object share this ino, so an inode mark added via
    /// one mount path matches events generated via another.
    pub target_ino: u64,
    /// For filesystem marks: ino of the root inode of the mount the marked path
    /// lives in. Two bind mounts of the same superblock share this value, so a
    /// filesystem mark covers events anywhere in that superblock.
    pub fsroot_ino: u64,
    /// Whether this (inode) mark was installed with FAN_MARK_EVICTABLE. Such a
    /// mark is reclaimed together with its inode when the dentry/inode caches
    /// are dropped (echo 2/3 > /proc/sys/vm/drop_caches). A non-evictable add
    /// on the same object upgrades it (clears this flag); a later evictable add
    /// onto a non-evictable mark fails with EEXIST (no downgrade).
    pub evictable: bool,
}

/// Mark scope constants (`mark_type` field).
const MT_INODE: u8 = 0;
const MT_MOUNT: u8 = 1;
const MT_FS:    u8 = 2;

/// Bootarg-gated debug print (enabled with `FANDBG=1` on the kernel cmdline).
fn fandbg_enabled() -> bool {
    crate::cmdline::fandbg_enabled()
}
macro_rules! fandbg {
    ($($arg:tt)*) => {
        if fandbg_enabled() { crate::println!($($arg)*); }
    };
}

fn mark_type_of(flags: u32) -> u8 {
    if flags & FAN_MARK_FILESYSTEM != 0 {
        MT_FS
    } else if flags & FAN_MARK_MOUNT != 0 {
        MT_MOUNT
    } else {
        MT_INODE
    }
}

/// ino of the marked object, or 0 if it cannot be resolved. With
/// `FAN_MARK_DONT_FOLLOW` the mark attaches to the symlink's own inode, so the
/// final path component must not be followed (fanotify04: opening the symlink
/// target must not match a DONT_FOLLOW mark on the symlink).
fn path_ino_flags(path: &str, flags: u32) -> u64 {
    let r = if flags & FAN_MARK_DONT_FOLLOW != 0 {
        crate::fs::lookup_no_follow(path)
    } else {
        crate::fs::lookup(path)
    };
    r.map(|i| i.ino() as u64).unwrap_or(0)
}

/// ino of an event object (symlinks always followed on the event side).
fn path_ino(path: &str) -> u64 {
    crate::fs::lookup(path).map(|i| i.ino() as u64).unwrap_or(0)
}

/// Stable per-superblock identity for the mount that `path` resides in. Used
/// for filesystem marks: bind mounts of the same fs expose the same underlying
/// subtree (`root_path`), so they resolve to the same identity. The mount-root
/// inode itself reports ino==0 in this kernel, so we hash the longest-prefix
/// mount's `root_path` string (the bind *source*, which is shared by all peers)
/// instead of relying on the inode number.
fn fsroot_ino_of(path: &str) -> u64 {
    let root_path = {
        let table = crate::fs::mount::MOUNT_INFO_TABLE.lock();
        // Follow bind-mount chains to the *superblock* root: a bind mount's
        // root_path is a path on its source filesystem, which may itself sit
        // under further bind mounts. Skip already-visited mounts so a
        // self-bind (mount --bind dir dir) resolves through to the mount the
        // source directory lives in rather than looping on itself.
        let mut cur = String::from(path);
        let mut visited: Vec<String> = Vec::new();
        let mut resolved = String::new();
        for _ in 0..8 {
            const MS_BIND: usize = 4096;
            let mut best_len = 0usize;
            let mut best: Option<(String, String, bool)> = None;
            for m in table.iter() {
                let t = &m.target;
                if visited.iter().any(|v| v == t) {
                    continue;
                }
                let is_prefix = cur == t.as_str()
                    || (cur.len() > t.len()
                        && cur.as_bytes().get(t.len()) == Some(&b'/')
                        && cur.starts_with(t.as_str()))
                    || t == "/";
                if is_prefix && t.len() >= best_len {
                    best_len = t.len();
                    best = Some((t.clone(), m.root_path.clone(), m.flags & MS_BIND != 0));
                }
            }
            let Some((target, root, is_bind)) = best else { break; };
            // A fresh (non-bind) filesystem mount owns its superblock — its
            // mountpoint is the superblock identity, stop here. A bind mount
            // shares the source's superblock: keep resolving its root_path
            // (excluding this mount, so a self-bind hops to the mount the
            // source directory actually lives in).
            if !is_bind || root.is_empty() {
                resolved = target;
                break;
            }
            resolved = root.clone();
            visited.push(target);
            cur = root;
        }
        resolved
    };
    let r = if root_path.is_empty() { 0 } else { str_hash(&root_path) };
    fandbg!("[FAN] fsroot_ino_of path={:?} root_path={:?} -> {:#x}", path, root_path, r);
    r
}

/// FNV-1a 64-bit hash of a string, used to derive a non-zero stable identity.
fn str_hash(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h | 1
}

/// True when `mark_path` is the immediate parent directory of `event_path`.
/// Used so an inode mark on a directory matches child events even when the
/// directory is a mount root whose inode number resolves to 0.
fn parent_path_eq(event_path: &str, mark_path: &str) -> bool {
    match event_path.rfind('/') {
        Some(0) => mark_path == "/",
        Some(i) => &event_path[..i] == mark_path,
        None => false,
    }
}

/// True when `event_path` lies within the subtree rooted at `mark_path`
/// (the object itself or any descendant). Used for mount-mark scope.
fn path_in_subtree(event_path: &str, mark_path: &str) -> bool {
    if event_path == mark_path {
        return true;
    }
    if mark_path == "/" {
        return event_path.starts_with('/');
    }
    event_path.len() > mark_path.len()
        && event_path.as_bytes().get(mark_path.len()) == Some(&b'/')
        && event_path.starts_with(mark_path)
}

/// Decide whether a mark covers an event object and, if so, whether the hit is
/// as the *parent* of the object (child event) rather than the object itself.
/// Returns `None` when the mark does not apply.
///
/// - inode mark: applies when its object ino equals the event ino (self) or the
///   event's parent ino (child). Bind-mount aliases share the ino, so a mark
///   added through one mount path matches events seen through another.
/// - mount mark: applies to any object within the marked mount's path subtree.
/// - filesystem mark: applies to any object whose mount-root ino matches the
///   marked path's (i.e. the same superblock, across all its bind mounts).
/// Identity lookups are resolved lazily through `id`: a mount mark needs only
/// the path subtree test (no inode lookup), a filesystem mark resolves just the
/// mount-root ino, and an inode mark resolves the event ino (and the parent ino
/// only if the self match fails).
fn mark_applies(m: &FanotifyMark, id: &mut EventId) -> Option<bool> {
    match m.mark_type {
        MT_MOUNT => {
            if path_in_subtree(id.path, &m.path) { Some(false) } else { None }
        }
        MT_FS => {
            if m.fsroot_ino != 0 && m.fsroot_ino == id.fsroot_ino() { Some(false) } else { None }
        }
        _ /* MT_INODE */ => {
            // Self match: ino equality (catches bind-mount aliases sharing an
            // ino) or exact path equality (catches mount-root objects whose ino
            // resolves to 0). Child match: parent ino equality or the mark path
            // being the event's immediate parent directory.
            if (m.target_ino != 0 && m.target_ino == id.event_ino()) || id.path == m.path {
                Some(false)
            } else if (m.target_ino != 0 && m.target_ino == id.parent_ino())
                || parent_path_eq(id.path, &m.path)
            {
                Some(true)
            } else {
                None
            }
        }
    }
}

/// `struct fanotify_response { __s32 fd; __u32 response; }` —— monitor 写回 8 字节
/// 来 ALLOW / DENY 一个 perm event。
pub const FAN_ALLOW: u32 = 0x01;
pub const FAN_DENY: u32 = 0x02;

/// Pending perm event：在 monitor 处理前，被通知进程会持有该结构挂起。
pub struct FanotifyPermEvent {
    pub id: u64,
    pub mask: u64,
    pub path: String,
    pub fd: core::sync::atomic::AtomicI32,
    pub response: core::sync::atomic::AtomicU32, // 0 pending / FAN_ALLOW / FAN_DENY
    pub waiter_pid: core::sync::atomic::AtomicUsize,
}

pub struct FanotifyInstance {
    inner: IrqSpinLock<FanotifyInner>,
    /// 创建该实例的 pid（fanotify_init 时记录）。
    /// 用于 perm event 短路 —— Linux 不会让 monitor 因为 read/open 自己被阻塞。
    listener_pid: core::sync::atomic::AtomicUsize,
    /// fanotify_init flags（保留 class / report_flag 等位，供 read 决定 layout）。
    init_flags: core::sync::atomic::AtomicU32,
    /// 该组是否由非特权用户（euid != 0）创建。非特权组对其它进程触发的事件
    /// 上报 pid=0，并禁止 mount/filesystem mark 与权限事件。
    unprivileged: core::sync::atomic::AtomicBool,
}

struct FanotifyInner {
    /// Keyed by (resolved path, mark_type) so an inode watch and a mount/fs
    /// mark on the same path coexist as distinct marks (Linux keys these by
    /// the per-object/mount/sb connector).
    marks: BTreeMap<(String, u8), FanotifyMark>,
    /// Notification queue keyed by a monotonic sequence number. A BTreeMap keyed
    /// by `next_seq` iterates in FIFO order (smallest seq = oldest event), which
    /// the read path drains front-first. Keying by seq (rather than a plain
    /// VecDeque) lets coalescing locate the existing event for a (pid, path) in
    /// O(log n) via `key_seq`, instead of scanning the whole queue — without
    /// that, fanotify05's 16k-deep queue makes delivery quadratic.
    events: BTreeMap<u64, FanotifyEvent>,
    /// (pid, path) → seq of the queued, not-yet-read event for that object, used
    /// to coalesce a later event into it (Linux `fanotify_merge`).
    /// key = 事件的合并身份（pid + 将序列化的 info 记录 / legacy path），
    /// 见 `MergeKey`。
    key_seq: BTreeMap<MergeKey, u64>,
    next_seq: u64,
    /// True once the queue hit its cap and a FAN_Q_OVERFLOW sentinel was queued;
    /// further events are dropped until that sentinel is read out.
    overflowed: bool,
    read_waiters: Vec<usize>,
    /// perm event 队列：尚未被 monitor read 出去。
    perm_pending: VecDeque<Arc<FanotifyPermEvent>>,
    /// 已被 monitor read 出去、等 write 回 response 的 perm event。
    /// key = monitor 视角的 fd（read 时分配，用于 fanotify_response.fd 校对）。
    perm_awaiting: BTreeMap<i32, Arc<FanotifyPermEvent>>,
    /// monitor 已 respond、等待被通知进程消费的 perm event。阻塞等待通过
    /// syscall 重启实现（suspend 不返回），重启后的 notify_perm_path 必须能
    /// 找回同一 (pid, path, mask) 事件的裁决，否则会重复入队新 perm event。
    perm_done: VecDeque<Arc<FanotifyPermEvent>>,
    next_perm_id: u64,
}

/// Number of live fanotify groups (Linux tracks this per-user; we keep a single
/// global counter, which matches the single-user LTP environment). Incremented
/// by `try_acquire_group`, decremented when the instance is dropped.
static FANOTIFY_GROUPS: AtomicUsize = AtomicUsize::new(0);
/// Linux default `/proc/sys/fs/fanotify/max_user_groups` (FANOTIFY_DEFAULT_MAX_GROUPS).
const FANOTIFY_DEFAULT_MAX_GROUPS: usize = 128;

impl Drop for FanotifyInstance {
    fn drop(&mut self) {
        FANOTIFY_GROUPS.fetch_sub(1, Ordering::SeqCst);
    }
}

impl FanotifyInstance {
    /// Reserve a group slot under the per-user limit. Returns false when the
    /// live group count already reached the cap, so fanotify_init can report
    /// EMFILE. A successful reservation is released by `Drop`.
    pub fn try_acquire_group() -> bool {
        loop {
            let cur = FANOTIFY_GROUPS.load(Ordering::SeqCst);
            if cur >= FANOTIFY_DEFAULT_MAX_GROUPS {
                return false;
            }
            if FANOTIFY_GROUPS
                .compare_exchange(cur, cur + 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return true;
            }
        }
    }

    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: IrqSpinLock::new(FanotifyInner {
                marks: BTreeMap::new(),
                events: BTreeMap::new(),
                key_seq: BTreeMap::new(),
                next_seq: 0,
                overflowed: false,
                read_waiters: Vec::new(),
                perm_pending: VecDeque::new(),
                perm_awaiting: BTreeMap::new(),
                perm_done: VecDeque::new(),
                next_perm_id: 1,
            }),
            listener_pid: core::sync::atomic::AtomicUsize::new(0),
            init_flags: core::sync::atomic::AtomicU32::new(0),
            unprivileged: core::sync::atomic::AtomicBool::new(false),
        })
    }

    /// 由 sys_fanotify_init 在 fd 入表前调用，记录 listener pid + flags。
    pub fn set_listener(&self, pid: usize, init_flags: u32) {
        self.listener_pid.store(pid, core::sync::atomic::Ordering::Relaxed);
        self.init_flags.store(init_flags, core::sync::atomic::Ordering::Relaxed);
    }

    /// 标记该组为非特权（euid != 0）创建。
    pub fn set_unprivileged(&self, v: bool) {
        self.unprivileged.store(v, core::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_unprivileged(&self) -> bool {
        self.unprivileged.load(core::sync::atomic::Ordering::Relaxed)
    }

    pub fn listener_pid(&self) -> usize {
        self.listener_pid.load(core::sync::atomic::Ordering::Relaxed)
    }

    pub fn init_flags(&self) -> u32 {
        self.init_flags.load(core::sync::atomic::Ordering::Relaxed)
    }

    pub fn mark(&self, path: &str, mask: u64, flags: u32) -> Result<(), i32> {
        if flags & FAN_MARK_FLUSH != 0 {
            // FAN_MARK_FLUSH with a type flag only clears marks of that scope
            // (FAN_MARK_MOUNT/FILESYSTEM); without one it clears inode marks.
            let flush_type = mark_type_of(flags);
            let paths: Vec<String> = {
                let mut inner = self.inner.lock();
                let keep: BTreeMap<(String, u8), FanotifyMark> = core::mem::take(&mut inner.marks);
                let mut removed_paths = Vec::new();
                for ((p, ty), m) in keep {
                    if ty == flush_type {
                        removed_paths.push(p);
                    } else {
                        inner.marks.insert((p, ty), m);
                    }
                }
                // Only fully drop the registry entry for a path once no mark of
                // any type remains on it.
                removed_paths.retain(|p| !inner.marks.keys().any(|(kp, _)| kp == p));
                removed_paths
            };
            for path in paths {
                FANOTIFY_REGISTRY.unregister(&path, self as *const _ as usize);
            }
            return Ok(());
        }
        let mark_type = mark_type_of(flags);
        let key = (String::from(path), mark_type);
        if flags & FAN_MARK_REMOVE != 0 {
            let unregister = {
                let mut inner = self.inner.lock();
                match inner.marks.get_mut(&key) {
                    Some(mark) => {
                        if flags & FAN_MARK_IGNORE_ANY != 0 {
                            // Removing bits from the ignore set only; the watch
                            // mark itself stays registered.
                            mark.ignored_mask &= !mask;
                            false
                        } else {
                            mark.mask &= !mask;
                            if mark.mask == 0 && mark.ignored_mask == 0 {
                                inner.marks.remove(&key);
                                // Only unregister the path once no mark of any
                                // type remains on it.
                                !inner.marks.keys().any(|(kp, _)| kp == path)
                            } else {
                                false
                            }
                        }
                    }
                    // Linux fanotify_mark(): removing a mark that this group
                    // does not hold on the object fails with ENOENT. fanotify23
                    // relies on this to detect that an evictable mark was
                    // reclaimed by drop_caches.
                    None => return Err(-2),
                }
            };
            if unregister {
                FANOTIFY_REGISTRY.unregister(path, self as *const _ as usize);
            }
            return Ok(());
        }
        if flags & FAN_MARK_ADD == 0 {
            return Err(-22);
        }
        let is_ignored = flags & FAN_MARK_IGNORE_ANY != 0;
        let ignore_is_new = flags & FAN_MARK_IGNORE != 0;
        let surv = flags & FAN_MARK_IGNORED_SURV_MODIFY != 0;
        // For the newer FAN_MARK_IGNORE, whether the ignore applies to children
        // is carried by FAN_EVENT_ON_CHILD in the ignore add's own mask. Legacy
        // FAN_MARK_IGNORED_MASK keys child-application off the watch mark, so it
        // leaves this false.
        let ignore_onchild = ignore_is_new && (mask & FAN_EVENT_ON_CHILD != 0);
        // Resolve object identity once at mark time. Bind-mount aliases of the
        // same object share an ino; a filesystem mark shares its mount-root ino
        // with every other mount of the same superblock.
        let target_ino = path_ino_flags(path, flags);
        let fsroot_ino = if mark_type == MT_FS { fsroot_ino_of(path) } else { 0 };
        let want_evictable = flags & FAN_MARK_EVICTABLE != 0;
        // FAN_MARK_EVICTABLE is only meaningful for inode marks; the kernel
        // rejects it on mount/filesystem marks with EINVAL.
        if want_evictable && mark_type != MT_INODE {
            return Err(-22);
        }
        let should_register = {
            let mut inner = self.inner.lock();
            // Evictable downgrade guard: adding an evictable mark onto an
            // existing non-evictable mark of the same object fails with EEXIST
            // (Linux never downgrades a pinned mark to evictable). A
            // non-evictable add onto an evictable mark upgrades it.
            if want_evictable {
                if let Some(m) = inner.marks.get(&key) {
                    if !m.evictable {
                        return Err(-17);
                    }
                }
            }
            let had_path = inner.marks.keys().any(|(kp, _)| kp == path);
            inner.marks.entry(key.clone())
                .and_modify(|m| {
                    if is_ignored {
                        m.ignored_mask |= mask;
                        m.ignored_surv_modify = surv;
                        m.ignore_is_new = ignore_is_new;
                        m.ignored_onchild = ignore_onchild;
                    } else {
                        m.mask |= mask;
                    }
                    m.flags |= flags;
                    // A non-evictable add pins a previously evictable mark.
                    if !want_evictable {
                        m.evictable = false;
                    }
                })
                .or_insert_with(|| {
                    let (mk, im) = if is_ignored { (0, mask) } else { (mask, 0) };
                    FanotifyMark {
                        path: path.into(), mask: mk, flags,
                        ignored_mask: im, ignored_surv_modify: is_ignored && surv,
                        ignore_is_new: is_ignored && ignore_is_new,
                        ignored_onchild: ignore_onchild,
                        mark_type, target_ino, fsroot_ino,
                        evictable: want_evictable,
                    }
                });
            !had_path
        };
        fandbg!("[FAN] mark add path={:?} mt={} mask={:#x} flags={:#x} ign={} tgt_ino={} fsroot={} reg={}",
            path, mark_type, mask, flags, is_ignored, target_ino, fsroot_ino, should_register);
        if should_register {
            FANOTIFY_REGISTRY.register(path, FanotifyHandle { instance_ptr: self as *const _ as usize });
        }
        Ok(())
    }

    /// 投递 event（由 VFS 调）。与 Linux `fsnotify` 一致：对**整个 group**
    /// 把所有命中该事件对象的 mark（按 inode 标识 / mount 子树 / 文件系统
    /// 归属判定命中）合并——watch 掩码取并集、ignore 掩码取并集——再据合并
    /// 结果决定是否上报。这样跨 mark 类型（inode 监听 + mount 忽略，或反之）
    /// 的忽略语义才正确，且 bind mount 别名因共享 inode 号而自动归一。
    ///
    /// `event_ino` 为事件对象 inode 号，`parent_ino` 为其父目录 inode 号，
    /// `fsroot_ino` 为事件所在挂载点根 inode 号（文件系统标识）。
    pub fn deliver_merged(&self, id: &mut EventId, mask: u64, pid: i32) {
        let event_path: &str = id.path;
        // Without FAN_REPORT_TID a group reports the thread-group leader pid
        // (tgid); the caller passes the raw triggering tid.
        let mut pid = if self.init_flags() & FAN_REPORT_TID != 0 {
            pid
        } else {
            tgid_of(pid)
        };
        // An unprivileged listener may only learn the pid of events triggered
        // by its own process; events from any other process report pid=0.
        if self.is_unprivileged() {
            let listener_tgid = tgid_of(self.listener_pid() as i32);
            if pid != listener_tgid {
                pid = 0;
            }
        }
        let fid_flags = self.init_flags()
            & (FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME | FAN_REPORT_TARGET_FID);
        let fid_mode = fid_flags != 0;
        // Linux generates each hook class as a *separate* fsnotify event: a
        // dirent event on the parent dir (fsnotify_name), a self event on the
        // object with dir=NULL (fsnotify_inode) and a path event on the object
        // possibly reported with parent dir+name (fsnotify_parent). Each class
        // iterates a different set of marks and gets its own info-record plan
        // (fanotify_alloc_event); queued events with identical records are then
        // coalesced. Mirror that: split the syscall-combined mask into the
        // three classes and process each independently.
        let comps = [
            (KIND_DIRENT, mask & FAN_DIRENT_EVENTS),
            (KIND_SELF, mask & FAN_SELF_EVENTS),
            (KIND_OBJ, mask & !FAN_DIRENT_EVENTS & !FAN_SELF_EVENTS & !FAN_ONDIR),
        ];
        // Marks whose ignore set must be cleared by this FAN_MODIFY (no SURV).
        let mut clear_keys: Vec<(String, u8)> = Vec::new();
        // Legacy fd-mode groups accumulate one (pid, path)-keyed event.
        let mut legacy_mask: u64 = 0;
        for (kind, bits) in comps {
            if bits == 0 {
                continue;
            }
            let mut watched: u64 = 0;
            let mut ignored: u64 = 0;
            let mut ondir = false;
            // Whether the event class carries parent-dir info: Linux
            // fsnotify_parent attaches dir+name to an object event only when a
            // parent inode mark watches children for it.
            let mut parent_watched = false;
            {
                let inner = self.inner.lock();
                fandbg!("[FAN] deliver ev_path={:?} kind={} bits={:#x} nmarks={}",
                    event_path, kind, bits, inner.marks.len());
                for ((kp, kt), m) in inner.marks.iter() {
                    let Some(is_child) = mark_applies(m, id) else { continue; };
                    // Per-class mark iteration: dirent events are generated on
                    // the parent dir inode (fsnotify_name) and self events on
                    // the object inode (fsnotify_inode) — neither carries a
                    // path, so mount marks never see them. Object (path)
                    // events reach the object's own inode mark, a parent inode
                    // mark via fsnotify_parent (needs FAN_EVENT_ON_CHILD), and
                    // mount/filesystem marks.
                    let (applicable, via_parent) = match (kind, m.mark_type, is_child) {
                        (KIND_DIRENT, MT_INODE, c) => (c, false),
                        (KIND_DIRENT, MT_FS, _) => (true, false),
                        (KIND_DIRENT, _, _) => (false, false),
                        (KIND_SELF, MT_INODE, c) => (!c, false),
                        (KIND_SELF, MT_FS, _) => (true, false),
                        (KIND_SELF, _, _) => (false, false),
                        // A parent inode mark participates in a child event
                        // both when its watch mask requests child events and
                        // when its FAN_MARK_IGNORE set does (ignored_onchild),
                        // matching fsnotify_parent's p_mask | ignore check.
                        (_, MT_INODE, true) => {
                            (m.mask & FAN_EVENT_ON_CHILD != 0 || m.ignored_onchild, true)
                        }
                        _ => (true, false),
                    };
                    fandbg!("[FAN]   mark kp={:?} kt={} m.mask={:#x} m.ign={:#x} child={} appl={} viap={}",
                        kp, kt, m.mask, m.ignored_mask, is_child, applicable, via_parent);
                    if !applicable {
                        continue;
                    }
                    ondir = id.is_dir();
                    if via_parent && m.mask & bits != 0 {
                        parent_watched = true;
                    }
                    // fsnotify_mask_applicable: a directory event only takes a
                    // mark's watch bits when that mark requested FAN_ONDIR, and
                    // a parent mark's watch bits reach a child event only when
                    // it requested FAN_EVENT_ON_CHILD.
                    if (!ondir || m.mask & FAN_ONDIR != 0)
                        && (!via_parent || m.mask & FAN_EVENT_ON_CHILD != 0)
                    {
                        watched |= m.mask;
                    }
                    // fsnotify_effective_ignore_mask. On a child event the
                    // ignore only applies when: (new FAN_MARK_IGNORE) it was
                    // added with FAN_EVENT_ON_CHILD, or (legacy) the mark's
                    // watch mask carries it. A legacy ignore mask always
                    // suppresses directory events; the new FAN_MARK_IGNORE
                    // requires FAN_ONDIR in the ignore set itself.
                    let ignore_here = if via_parent {
                        if m.ignore_is_new { m.ignored_onchild } else { m.mask & FAN_EVENT_ON_CHILD != 0 }
                    } else {
                        true
                    };
                    let ignore_dir_ok =
                        !ondir || !m.ignore_is_new || m.ignored_mask & FAN_ONDIR != 0;
                    if ignore_here && ignore_dir_ok && m.ignored_mask != 0 {
                        ignored |= m.ignored_mask;
                        if bits & FAN_MODIFY != 0 && !m.ignored_surv_modify {
                            clear_keys.push((kp.clone(), *kt));
                        }
                    }
                }
            }
            let full = bits | if ondir { FAN_ONDIR } else { 0 };
            let test = full & watched & !ignored;
            fandbg!("[FAN]   kind={} watched={:#x} ignored={:#x} test={:#x}", kind, watched, ignored, test);
            // Do not report event flags without any event.
            if test & !FAN_ONDIR == 0 {
                continue;
            }
            if !fid_mode {
                // Legacy fd-mode groups never report FAN_ONDIR.
                legacy_mask |= test & !FAN_ONDIR;
                continue;
            }
            // fanotify_group_event_mask: a group without FAN_REPORT_FID needs
            // a directory inode to identify the event by.
            let _ = parent_watched;
            // Linux fanotify_dfid_inode: object (path) events on non-dirs
            // always identify the parent dir when the group reports dir fids
            // (fsnotify supplies parent info for any group wanting it, not
            // only for parent inode marks). Self events carry no dir.
            let has_dir = match kind {
                KIND_DIRENT => true,
                KIND_SELF => false,
                _ => true,
            };
            if fid_flags & FAN_REPORT_FID == 0 && !has_dir && !ondir {
                continue;
            }
            let Some((dir_fh, obj_fh)) = plan_records(fid_flags, kind, id, has_dir) else {
                continue;
            };
            self.queue_one(FanotifyEvent {
                mask: test,
                fd: -1,
                pid,
                path: Some(String::from(event_path)),
                dir_fh,
                dir2_fh: None,
                obj_fh,
            }, true);
        }
        // FAN_MARK_IGNORED_MASK: a FAN_MODIFY clears the ignore set of any
        // contributing mark not installed with FAN_MARK_IGNORED_SURV_MODIFY.
        if !clear_keys.is_empty() {
            let mut inner = self.inner.lock();
            for k in &clear_keys {
                if let Some(m) = inner.marks.get_mut(k) {
                    m.ignored_mask = 0;
                }
            }
        }
        if legacy_mask != 0 {
            // Legacy fd-mode group: one event keyed by (pid, path), like
            // Linux fanotify_path_equal merging.
            self.queue_one(FanotifyEvent {
                mask: legacy_mask,
                fd: -1,
                pid,
                path: Some(String::from(event_path)),
                dir_fh: None,
                dir2_fh: None,
                obj_fh: None,
            }, false);
        }
    }

    /// Queue a single event, coalescing with an unread event of identical
    /// merge identity per Linux `fanotify_should_merge`.
    fn queue_one(&self, event: FanotifyEvent, fid_mode: bool) {
        let mut inner = self.inner.lock();
        // Once the queue overflowed, drop everything until the FAN_Q_OVERFLOW
        // sentinel is read out (Linux clears the overflow only after the
        // sentinel leaves the queue).
        if inner.overflowed {
            return;
        }
        // Linux event coalescing (fanotify_merge): OR the new mask into the
        // queued, not-yet-read event with the same record identity and pid.
        // `key_seq` resolves the existing event in O(log n) instead of scanning
        // the whole queue, which is what keeps the 16k-deep fanotify05 queue
        // from making delivery quadratic.
        let mkey = event_merge_key(&event, fid_mode);
        if let Some(&seq) = inner.key_seq.get(&mkey) {
            if let Some(ev) = inner.events.get_mut(&seq) {
                ev.mask |= event.mask;
                drop(inner);
                self.wake_waiters();
                return;
            }
        }
        // Cap the queue at max_queued_events unless the group asked for an
        // unlimited queue (FAN_UNLIMITED_QUEUE). On overflow, queue a single
        // FAN_Q_OVERFLOW sentinel and drop subsequent events.
        let unlimited = self.init_flags() & FAN_UNLIMITED_QUEUE != 0;
        if !unlimited && inner.events.len() >= FAN_MAX_QUEUED_EVENTS {
            let seq = inner.next_seq;
            inner.next_seq += 1;
            inner.events.insert(seq, FanotifyEvent {
                mask: FAN_Q_OVERFLOW, fd: -1, pid: 0, path: None,
                dir_fh: None, dir2_fh: None, obj_fh: None,
            });
            inner.overflowed = true;
            drop(inner);
            self.wake_waiters();
            return;
        }
        let seq = inner.next_seq;
        inner.next_seq += 1;
        inner.events.insert(seq, event);
        inner.key_seq.insert(mkey, seq);
        drop(inner);
        self.wake_waiters();
    }

    /// Deliver one FAN_RENAME event (Linux FS_RENAME): matched against marks
    /// on the old parent dir, the new parent dir and filesystem marks; carries
    /// old-dir-fid+name and/or new-dir-fid+name info records depending on
    /// which side of the rename each matching mark watches.
    pub fn deliver_rename(&self, old_id: &mut EventId, new_id: &mut EventId, pid: i32) {
        let flags = self.init_flags();
        let fid_flags = flags
            & (FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME | FAN_REPORT_TARGET_FID);
        // FAN_RENAME is only markable on FAN_REPORT_NAME groups.
        if fid_flags & FAN_REPORT_NAME == 0 {
            return;
        }
        let mut pid = if flags & FAN_REPORT_TID != 0 { pid } else { tgid_of(pid) };
        if self.is_unprivileged() {
            let listener_tgid = tgid_of(self.listener_pid() as i32);
            if pid != listener_tgid {
                pid = 0;
            }
        }
        let is_dir = old_id.is_dir();
        let mut watched: u64 = 0;
        let mut ignored: u64 = 0;
        let (mut report_old, mut report_new) = (false, false);
        {
            let inner = self.inner.lock();
            for (_, m) in inner.marks.iter() {
                let oh = mark_applies(m, old_id);
                let nh = mark_applies(m, new_id);
                if oh.is_none() && nh.is_none() {
                    continue;
                }
                // FS_RENAME is delivered *on* the parent directories: an inode
                // mark counts when it sits on a parent dir (is_child hit);
                // mount/fs marks count on any hit. A mark on the renamed
                // object itself only gets FAN_MOVE_SELF, not FAN_RENAME.
                let old_hit = matches!(oh, Some(true)) || (oh.is_some() && m.mark_type != MT_INODE);
                let new_hit = matches!(nh, Some(true)) || (nh.is_some() && m.mark_type != MT_INODE);
                if !old_hit && !new_hit {
                    continue;
                }
                if !is_dir || m.mask & FAN_ONDIR != 0 {
                    watched |= m.mask;
                    if m.mask & FAN_RENAME != 0 {
                        if m.mark_type != MT_INODE {
                            report_old = true;
                            report_new = true;
                        } else {
                            report_old |= old_hit;
                            report_new |= new_hit;
                        }
                    }
                }
                // Legacy FAN_MARK_IGNORED_MASK always suppresses dir events;
                // the new FAN_MARK_IGNORE needs FAN_ONDIR in the ignore set.
                if (!is_dir || !m.ignore_is_new || m.ignored_mask & FAN_ONDIR != 0)
                    && m.ignored_mask != 0
                {
                    ignored |= m.ignored_mask;
                }
            }
        }
        if watched & FAN_RENAME == 0 || ignored & FAN_RENAME != 0 {
            return;
        }
        let mut ev_mask = FAN_RENAME;
        if is_dir && watched & FAN_ONDIR != 0 {
            ev_mask |= FAN_ONDIR;
        }
        let new_rec = (new_id.parent_ino(), basename_of(new_id.path));
        let (dir_fh, dir2_fh) = if report_old {
            (
                Some((old_id.parent_ino(), Some(basename_of(old_id.path)))),
                report_new.then_some(new_rec),
            )
        } else {
            (None, Some(new_rec))
        };
        // Rename is a dirent-class event: the child's own fid is only added
        // with FAN_REPORT_TARGET_FID (fanotify_report_child_fid).
        let obj_fh = (fid_flags & FAN_REPORT_TARGET_FID != 0).then(|| new_id.event_ino());
        self.queue_one(FanotifyEvent {
            mask: ev_mask,
            fd: -1,
            pid,
            path: Some(String::from(new_id.path)),
            dir_fh,
            dir2_fh,
            obj_fh,
        }, true);
    }

    /// 创建并入队一个 perm event；返回的 Arc 由被通知进程持有，等待
    /// monitor write 回 `FAN_ALLOW`/`FAN_DENY` 后继续。
    pub fn queue_perm(&self, path: &str, mask: u64, waiter_pid: usize) -> Arc<FanotifyPermEvent> {
        let event = {
            let mut inner = self.inner.lock();
            // 阻塞等待以 syscall 重启方式实现：同一 (pid, path, mask) 的重启
            // 调用必须复用未决事件或取回已裁决结果，绝不能重复入队。
            let key_match = |e: &Arc<FanotifyPermEvent>| {
                e.waiter_pid.load(core::sync::atomic::Ordering::Acquire) == waiter_pid
                    && e.mask == mask
                    && e.path == path
            };
            if let Some(pos) = inner.perm_done.iter().position(&key_match) {
                return inner.perm_done.remove(pos).unwrap();
            }
            if let Some(ev) = inner.perm_pending.iter().find(|e| key_match(e)) {
                return ev.clone();
            }
            if let Some(ev) = inner.perm_awaiting.values().find(|e| key_match(e)) {
                return ev.clone();
            }
            let id = inner.next_perm_id;
            inner.next_perm_id += 1;
            let ev = Arc::new(FanotifyPermEvent {
                id,
                mask,
                path: path.into(),
                fd: core::sync::atomic::AtomicI32::new(-1),
                response: core::sync::atomic::AtomicU32::new(0),
                waiter_pid: core::sync::atomic::AtomicUsize::new(waiter_pid),
            });
            inner.perm_pending.push_back(ev.clone());
            ev
        };
        self.wake_waiters();
        event
    }

    /// monitor 写入 8 字节 `fanotify_response`，解析后唤醒等待者。
    pub fn respond(&self, fd: i32, response: u32) -> Result<(), crate::fs::FsError> {
        if response != FAN_ALLOW && response != FAN_DENY {
            return Err(crate::fs::FsError::InvalidArgument);
        }
        let event = {
            let mut inner = self.inner.lock();
            inner.perm_awaiting.remove(&fd)
        };
        let Some(event) = event else {
            // 该 fd 不在 awaiting 表（可能 monitor 写了非 perm fd）。
            return Err(crate::fs::FsError::InvalidArgument);
        };
        event.response.store(response, core::sync::atomic::Ordering::Release);
        fandbg!("[FAN] respond fd={} resp={:#x} waiter={}", fd, response, event.waiter_pid.load(core::sync::atomic::Ordering::Acquire));
        {
            let mut inner = self.inner.lock();
            if inner.perm_done.len() >= 64 {
                inner.perm_done.pop_front();
            }
            inner.perm_done.push_back(event.clone());
        }
        let pid = event.waiter_pid.load(core::sync::atomic::Ordering::Acquire);
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            if pid != 0 {
                let _ = table.mark_runnable(pid, true);
            }
            table.wake_io_waiters();
        }
        Ok(())
    }

    pub fn read(&self, max_bytes: usize) -> Result<Vec<u8>, crate::fs::FsError> {
        if max_bytes < 24 {
            return Err(crate::fs::FsError::InvalidArgument);
        }
        let mut inner = self.inner.lock();
        let mut out = Vec::new();
        // 先排空 perm event：每个 perm event 要给 monitor 分配新的 fd
        // 指向被请求路径（与 Linux fanotify ABI 一致）。
        while out.len() + 24 <= max_bytes {
            let Some(event) = inner.perm_pending.pop_front() else { break; };
            let fd = allocate_event_fd(&event.path);
            event.fd.store(fd, core::sync::atomic::Ordering::Release);
            let pid = event.waiter_pid.load(core::sync::atomic::Ordering::Acquire) as i32;
            let bytes = FanotifyEvent {
                mask: event.mask, fd, pid, path: None,
                dir_fh: None, dir2_fh: None, obj_fh: None,
            }.to_bytes();
            out.extend_from_slice(&bytes);
            inner.perm_awaiting.insert(fd, event);
        }
        drop(inner);
        // 然后排出普通通知 event。FID 组（FAN_REPORT_FID/DIR_FID）按
        // `fanotify_event_info_fid` ABI 序列化：metadata.fd = FAN_NOFD，紧跟
        // 一条 fid info record（fsid + file_handle）。非 FID 组在 read 时为
        // monitor 真分配指向触发 inode 的 fd。
        let flags = self.init_flags();
        let fid_flags = flags
            & (FAN_REPORT_FID | FAN_REPORT_DIR_FID | FAN_REPORT_NAME | FAN_REPORT_TARGET_FID);
        let fid_mode = fid_flags != 0;
        let report_pidfd = flags & FAN_REPORT_PIDFD != 0;
        loop {
            let Some((seq, mut ev)) = ({
                let mut inner = self.inner.lock();
                let popped = inner.events.pop_first();
                if let Some((s, ref e)) = popped {
                    // Drop the coalescing index for this event; a FAN_Q_OVERFLOW
                    // leaving the queue clears the overflow latch so new events
                    // can be queued again.
                    if e.mask == FAN_Q_OVERFLOW {
                        inner.overflowed = false;
                    } else {
                        let k = event_merge_key(e, fid_mode);
                        if inner.key_seq.get(&k) == Some(&s) {
                            inner.key_seq.remove(&k);
                        }
                    }
                }
                popped
            }) else {
                break;
            };
            let bytes = if fid_mode {
                encode_event_fid_records(&ev, fid_flags)
            } else {
                if let Some(ref p) = ev.path {
                    ev.fd = allocate_event_fd(p);
                }
                let mut v = ev.to_bytes().to_vec();
                // FAN_REPORT_PIDFD: append a `fanotify_event_info_pidfd` record
                // carrying a pidfd for the triggering process. If that process
                // is already gone (e.g. a child that exited before the listener
                // read the event) emit the FAN_NOPIDFD sentinel instead
                // (fanotify21).
                if report_pidfd {
                    v.extend_from_slice(&encode_pidfd_info(ev.pid));
                    let new_len = v.len() as u32;
                    v[0..4].copy_from_slice(&new_len.to_le_bytes());
                }
                v
            };
            if !out.is_empty() && out.len() + bytes.len() > max_bytes {
                // Doesn't fit in the caller's buffer — requeue at its original
                // seq (front of the FIFO) and restore the coalescing index.
                let mut inner = self.inner.lock();
                if ev.mask == FAN_Q_OVERFLOW {
                    inner.overflowed = true;
                } else {
                    inner.key_seq.insert(event_merge_key(&ev, fid_mode), seq);
                }
                inner.events.insert(seq, ev);
                break;
            }
            out.extend_from_slice(&bytes);
        }
        Ok(out)
    }

    pub fn pending(&self) -> bool {
        let inner = self.inner.lock();
        !inner.events.is_empty() || !inner.perm_pending.is_empty()
    }

    pub fn register_reader(&self, pid: usize) {
        let mut inner = self.inner.lock();
        if !inner.read_waiters.contains(&pid) {
            inner.read_waiters.push(pid);
        }
    }

    pub fn wake_waiters(&self) {
        let waiters: Vec<usize> = {
            let mut inner = self.inner.lock();
            // No blocked reader registered -> nothing to wake. Skip the global
            // PROCESS_TABLE lock + wake_io_waiters() scan, which otherwise runs
            // once per delivered event and makes a 16k-event burst (fanotify05)
            // O(n·tasks).
            if inner.read_waiters.is_empty() {
                return;
            }
            core::mem::take(&mut inner.read_waiters)
        };
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            for pid in waiters {
                let _ = table.mark_runnable(pid, true);
            }
            table.wake_io_waiters();
        }
    }

    pub fn unregister_all(&self) {
        let paths: Vec<String> = {
            let mut inner = self.inner.lock();
            let mut paths: Vec<String> = inner.marks.keys().map(|(p, _)| p.clone()).collect();
            paths.dedup();
            inner.marks.clear();
            inner.events.clear();
            inner.key_seq.clear();
            inner.overflowed = false;
            inner.read_waiters.clear();
            paths
        };
        for path in paths {
            FANOTIFY_REGISTRY.unregister(&path, self as *const _ as usize);
        }
    }

    /// Reclaim every evictable inode mark of this group, mirroring how Linux
    /// drops evictable fanotify marks when their inode leaves the cache on
    /// `echo 2/3 > /proc/sys/vm/drop_caches`. Returns the paths that no longer
    /// carry any mark of any type so the caller can unregister them.
    pub fn evict_evictable_marks(&self) -> Vec<String> {
        let mut inner = self.inner.lock();
        let evicted_paths: Vec<String> = inner.marks.iter()
            .filter(|(_, m)| m.evictable)
            .map(|((p, _), _)| p.clone())
            .collect();
        if evicted_paths.is_empty() {
            return Vec::new();
        }
        inner.marks.retain(|_, m| !m.evictable);
        // Of the paths whose evictable marks we dropped, only those with no
        // surviving mark of any type need unregistering from the path registry.
        let mut to_unregister: Vec<String> = evicted_paths.into_iter()
            .filter(|p| !inner.marks.keys().any(|(kp, _)| kp == p))
            .collect();
        to_unregister.dedup();
        to_unregister
    }

}

/// Reclaim evictable inode marks across every fanotify group, as Linux does
/// when `echo 2/3 > /proc/sys/vm/drop_caches` evicts the inodes the marks were
/// pinned to. Invoked from the /proc/sys/vm/drop_caches write handler.
/// fanotify10/fanotify23 rely on this to reduce their ignore-mark counts and to
/// surface ENOENT on a subsequent FAN_MARK_REMOVE of the reclaimed mark.
pub fn drop_caches_evict_marks() {
    for handle in FANOTIFY_REGISTRY.all_handles() {
        let instance = unsafe { &*(handle.instance_ptr as *const FanotifyInstance) };
        let paths = instance.evict_evictable_marks();
        for path in paths {
            FANOTIFY_REGISTRY.unregister(&path, handle.instance_ptr);
        }
    }
}

// ============================================================================
// File / Inode adapter
// ============================================================================

use crate::fs::{File, FileMode, FileStat, FileType, FsError, Inode};

pub struct FanotifyFile {
    pub instance: Arc<FanotifyInstance>,
    ino: usize,
}

impl FanotifyFile {
    pub fn new(instance: Arc<FanotifyInstance>) -> Arc<Self> {
        static NEXT_INO: AtomicUsize = AtomicUsize::new(5_000_000_000);
        Arc::new(Self {
            instance,
            ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
        })
    }
}

#[derive(Clone)]
pub struct FanotifyHandle {
    pub instance_ptr: usize,
}

impl FanotifyHandle {
    pub fn deliver_merged(&self, id: &mut EventId, mask: u64, pid: i32) {
        let instance = unsafe { &*(self.instance_ptr as *const FanotifyInstance) };
        instance.deliver_merged(id, mask, pid);
    }

    pub fn deliver_rename(&self, old_id: &mut EventId, new_id: &mut EventId, pid: i32) {
        let instance = unsafe { &*(self.instance_ptr as *const FanotifyInstance) };
        instance.deliver_rename(old_id, new_id, pid);
    }

    /// 投递 perm event 给 instance。返回 `Some(Arc<PermEvent>)` 表示已入队；
    /// `None` 表示该 mark 实际不监听该 mask 或该 instance 的 listener 就是当前
    /// 进程（避免 listener 阻塞自身）。
    pub fn queue_perm(
        &self,
        mark_path: &str,
        event_path: &str,
        mask: u64,
        waiter_pid: usize,
        is_child: bool,
    ) -> Option<Arc<FanotifyPermEvent>> {
        let instance = unsafe { &*(self.instance_ptr as *const FanotifyInstance) };
        if instance.listener_pid() == waiter_pid {
            return None;
        }
        // OR the watch masks/flags of every mark on this path (any scope).
        let watched = {
            let inner = instance.inner.lock();
            let mut mask_acc = 0u64;
            let mut flag_acc = 0u32;
            for ((p, _), m) in inner.marks.iter() {
                if p == mark_path {
                    mask_acc |= m.mask;
                    flag_acc |= m.flags;
                }
            }
            (mask_acc, flag_acc)
        };
        if watched.0 & mask == 0 {
            return None;
        }
        if is_child && watched.0 & FAN_EVENT_ON_CHILD == 0 {
            return None;
        }
        // Report only the perm bits this group actually watches (Linux keeps
        // FAN_OPEN_PERM and FAN_OPEN_EXEC_PERM independent even when the open
        // triggers both).
        Some(instance.queue_perm(event_path, mask & watched.0, waiter_pid))
    }
}

pub struct FanotifyRegistry {
    inner: IrqSpinLock<BTreeMap<String, Vec<FanotifyHandle>>>,
}

impl FanotifyRegistry {
    const fn new() -> Self {
        Self { inner: IrqSpinLock::new(BTreeMap::new()) }
    }

    pub fn register(&self, path: &str, handle: FanotifyHandle) {
        let mut g = self.inner.lock();
        let list = g.entry(path.into()).or_insert_with(Vec::new);
        if !list.iter().any(|h| h.instance_ptr == handle.instance_ptr) {
            list.push(handle);
            FANOTIFY_ACTIVE.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn unregister(&self, path: &str, instance_ptr: usize) {
        let mut g = self.inner.lock();
        if let Some(list) = g.get_mut(path) {
            let before = list.len();
            list.retain(|h| h.instance_ptr != instance_ptr);
            let removed = before.saturating_sub(list.len());
            if removed != 0 {
                FANOTIFY_ACTIVE.fetch_sub(removed, Ordering::Relaxed);
            }
            if list.is_empty() {
                g.remove(path);
            }
        }
    }

    /// 全部已注册 group 的去重 handle 列表。投递改为「每 group 一次」，由
    /// group 自身依据事件对象的 inode 标识 / mount 子树 / 文件系统归属，合并
    /// 命中的各 mark 来决定是否上报（见 `deliver_merged`）。
    pub fn all_handles(&self) -> Vec<FanotifyHandle> {
        if FANOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
            return Vec::new();
        }
        let g = self.inner.lock();
        let mut out: Vec<FanotifyHandle> = Vec::new();
        for list in g.values() {
            for h in list {
                if !out.iter().any(|o| o.instance_ptr == h.instance_ptr) {
                    out.push(h.clone());
                }
            }
        }
        out
    }

    /// 收集需要为 `mark_path` 触发 perm event 的所有 handles 副本。
    pub fn handles_for(&self, mark_path: &str) -> Vec<FanotifyHandle> {
        if FANOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
            return Vec::new();
        }
        self.inner.lock().get(mark_path).cloned().unwrap_or_default()
    }
}

pub static FANOTIFY_REGISTRY: FanotifyRegistry = FanotifyRegistry::new();
static FANOTIFY_ACTIVE: AtomicUsize = AtomicUsize::new(0);

fn current_pid_i32() -> i32 {
    crate::sched::process::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed) as i32
}

/// Map a thread id to its thread-group leader pid (tgid). Used to report the
/// process pid (rather than the raw tid) for groups without FAN_REPORT_TID.
fn tgid_of(pid: i32) -> i32 {
    if pid <= 0 {
        return pid;
    }
    let t = crate::sched::process::PROCESS_TABLE.lock();
    t.as_ref()
        .and_then(|tt| tt.get(pid as usize))
        .map(|p| p.tgid as i32)
        .unwrap_or(pid)
}

pub fn has_active_marks() -> bool {
    FANOTIFY_ACTIVE.load(Ordering::Relaxed) != 0
}

/// Lazily-resolved identity of an event object, shared across every group in a
/// single notify dispatch. Each underlying filesystem lookup is O(directory
/// size), so for a workload that creates/opens tens of thousands of files in
/// one directory (fanotify05) resolving the full identity triple eagerly per
/// event is quadratic and dominates run time. We instead resolve each piece at
/// most once and only when a mark of the relevant scope actually needs it:
///   * `event_ino`/`is_dir` share a single `lookup` (same inode).
///   * `parent_ino` is only resolved for inode (child) marks.
///   * `fsroot_ino` is only resolved for filesystem marks.
/// A group carrying only mount marks (matched purely by path subtree) performs
/// just the one `is_dir` lookup needed for FAN_ONDIR gating.
struct EventId<'a> {
    path: &'a str,
    self_done: bool,
    event_ino: u64,
    is_dir: Option<bool>,
    parent_ino: Option<u64>,
    fsroot_ino: Option<u64>,
}

impl<'a> EventId<'a> {
    fn new(path: &'a str) -> Self {
        EventId {
            path,
            self_done: false,
            event_ino: 0,
            is_dir: None,
            parent_ino: None,
            fsroot_ino: None,
        }
    }
    /// Seed the object type from the syscall context. Mount/fs marks decide
    /// applicability purely from the path (subtree / fsroot ino), so once the
    /// caller supplies `is_dir` the FAN_ONDIR gating in `deliver_merged` needs
    /// no `lookup`. For a workload that opens tens of thousands of files in one
    /// directory under a mount mark (fanotify05) this turns a per-event
    /// O(directory-size) lookup into zero filesystem work.
    fn with_dir_hint(path: &'a str, is_dir: bool) -> Self {
        EventId {
            path,
            self_done: false,
            event_ino: 0,
            is_dir: Some(is_dir),
            parent_ino: None,
            fsroot_ino: None,
        }
    }
    /// Seed both the object ino and type from the syscall context. Used for
    /// delete events, where the object is already unlinked when the event is
    /// generated and a path lookup would fail (FAN_DELETE_SELF must still
    /// carry the deleted object's file handle — fanotify15).
    fn with_ino_hint(path: &'a str, ino: u64, is_dir: bool) -> Self {
        EventId {
            path,
            self_done: true,
            event_ino: ino,
            is_dir: Some(is_dir),
            parent_ino: None,
            fsroot_ino: None,
        }
    }
    fn resolve_self(&mut self) {
        if self.self_done {
            return;
        }
        if let Ok(inode) = crate::fs::lookup(self.path) {
            self.event_ino = inode.ino() as u64;
            if self.is_dir.is_none() {
                self.is_dir = Some(inode.file_type() == FileType::Directory);
            }
        }
        self.self_done = true;
    }
    fn event_ino(&mut self) -> u64 {
        self.resolve_self();
        self.event_ino
    }
    fn is_dir(&mut self) -> bool {
        if let Some(v) = self.is_dir {
            return v;
        }
        self.resolve_self();
        self.is_dir.unwrap_or(false)
    }
    fn parent_ino(&mut self) -> u64 {
        if let Some(v) = self.parent_ino {
            return v;
        }
        let v = match self.path.rfind('/') {
            Some(0) => path_ino("/"),
            Some(pos) => path_ino(&self.path[..pos]),
            None => 0,
        };
        self.parent_ino = Some(v);
        v
    }
    fn fsroot_ino(&mut self) -> u64 {
        if let Some(v) = self.fsroot_ino {
            return v;
        }
        let v = fsroot_ino_of(self.path);
        self.fsroot_ino = Some(v);
        v
    }
}

pub fn notify_path(path: &str, mask: u64) {
    if FANOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut id = EventId::new(path);
    let pid = current_pid_i32();
    for handle in FANOTIFY_REGISTRY.all_handles() {
        handle.deliver_merged(&mut id, mask, pid);
    }
}

/// 同步阻塞的 perm event：用于 `open(2)` / `read(2)` 进入文件前的访问控制。
///
/// 行为复刻 Linux `fsnotify_perm()`：沿 `path` 自身、`"/"`、各级父目录广播，
/// 对每个 mark 命中的 fanotify instance 入队一个 perm event，然后挂起当前
/// 进程；monitor 在 user-space write 回 `FAN_ALLOW` 或 `FAN_DENY` 后唤醒。
/// 任一 instance 返回 `FAN_DENY` 立即返回 `Err(-1)`（即 `-EPERM`）。
///
/// 当前实现采用"短链"超时（不超过 `LEASE_BREAK_SECONDS`）来兜底防止 monitor
/// 长期不响应；超时按 ALLOW 处理，对应 Linux `default_timeout` 行为。
pub fn notify_perm_path(path: &str, mask: u64) -> Result<(), i32> {
    if FANOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
        return Ok(());
    }
    let current_pid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        match table.as_ref().and_then(|t| t.current) {
            Some(pid) => pid,
            None => return Ok(()),
        }
    };
    // 收集所有需要通知的 mark path（自身 + "/" + 各级父目录）。
    let mut targets: Vec<(String, bool)> = Vec::new();
    targets.push((path.into(), false));
    targets.push(("/".into(), true));
    let mut cursor = path.len();
    while cursor > 0 {
        if let Some(parent_pos) = path[..cursor].rfind('/') {
            let parent = if parent_pos == 0 { "/" } else { &path[..parent_pos] };
            if parent != "/" {
                targets.push((parent.into(), true));
            }
            if parent_pos == 0 {
                break;
            }
            cursor = parent_pos;
        } else {
            break;
        }
    }

    // 排重并入队所有 perm event。
    let mut events: Vec<Arc<FanotifyPermEvent>> = Vec::new();
    for (mark_path, is_child) in targets {
        for handle in FANOTIFY_REGISTRY.handles_for(&mark_path) {
            if let Some(ev) = handle.queue_perm(&mark_path, path, mask, current_pid, is_child) {
                events.push(ev);
            }
        }
    }
    if events.is_empty() {
        return Ok(());
    }
    fandbg!("[FAN] perm wait pid={} mask={:#x} nev={}", current_pid, mask, events.len());
    // 阻塞等待所有 perm event 收到 monitor response。超时上限重用 lease-break
    // 秒数（默认 45s），让 fanotify 与 lease break 共享同一个超时配置。
    let timeout_secs = crate::fs::file_lease::break_seconds() as u64;
    let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
    let deadline = if timeout_secs == 0 {
        crate::arch::asm_helpers::rdtime()
    } else {
        crate::arch::asm_helpers::rdtime()
            .saturating_add(timeout_secs.saturating_mul(clock_freq))
    };
    loop {
        // 先检查所有 event 是否已 ALLOW/DENY。
        let mut all_done = true;
        for event in &events {
            let r = event.response.load(core::sync::atomic::Ordering::Acquire);
            if r == FAN_DENY {
                return Err(-1); // -EPERM
            }
            if r == 0 {
                all_done = false;
            }
        }
        if all_done {
            fandbg!("[FAN] perm allow pid={} mask={:#x}", current_pid, mask);
            return Ok(());
        }
        // 超时兜底：视作全部 ALLOW，避免 monitor 死锁拖住整机。
        let now = crate::arch::asm_helpers::rdtime();
        if now >= deadline {
            return Ok(());
        }
        // pending unblocked signal 直接 EINTR；perm wait 不应吞掉 SIGKILL。
        if {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let mut intr = false;
            if let Some(ref t) = *table {
                intr = crate::kernel::signal::pending_interrupting_signal_for_process(t, current_pid, 0);
            }
            intr
        } {
            return Err(-4); // -EINTR
        }
        // Slice 2.5 wchan：fanotify 阻塞读 → Linux `fanotify_read`
        crate::sched::process::enter_wait_channel("fanotify_read");
        crate::sched::process::suspend_current_for_io(deadline);
    }
}

pub fn notify_file_path(path: &str, mask: u64) {
    if FANOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
        return;
    }
    let pid = current_pid_i32();
    let mut id = EventId::new(path);
    for handle in FANOTIFY_REGISTRY.all_handles() {
        handle.deliver_merged(&mut id, mask, pid);
    }
}

/// Same as [`notify_file_path`] but with the object type already known from the
/// syscall context. Seeding `is_dir` lets mount/fs-mark groups deliver without
/// any filesystem lookup, which is what keeps fanotify05 (16k+ opens in one
/// directory under a mount mark) from going quadratic.
/// Deliver events for an object that has just been unlinked. `ino` is the
/// victim's inode number captured before removal, so FID-mode groups can still
/// report its file handle (a post-delete lookup would fail).
pub fn notify_file_path_deleted(path: &str, mask: u64, ino: u64, is_dir: bool) {
    if FANOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
        return;
    }
    let pid = current_pid_i32();
    let mut id = EventId::with_ino_hint(path, ino, is_dir);
    for handle in FANOTIFY_REGISTRY.all_handles() {
        handle.deliver_merged(&mut id, mask, pid);
    }
}

pub fn notify_file_path_typed(path: &str, mask: u64, is_dir: bool) {
    if FANOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
        return;
    }
    let pid = current_pid_i32();
    let mut id = EventId::with_dir_hint(path, is_dir);
    for handle in FANOTIFY_REGISTRY.all_handles() {
        handle.deliver_merged(&mut id, mask, pid);
    }
}

/// FAN_RENAME (Linux 5.17 FS_RENAME): a single event per rename carrying old
/// dir fid + old name and/or new dir fid + new name info records, delivered to
/// marks on the old parent, the new parent, and filesystem marks. Independent
/// of the legacy FAN_MOVED_FROM/FAN_MOVED_TO pair.
pub fn notify_rename(old_path: &str, new_path: &str, child_ino: u64, is_dir: bool) {
    if FANOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
        return;
    }
    let pid = current_pid_i32();
    let mut old_id = EventId::with_ino_hint(old_path, child_ino, is_dir);
    let mut new_id = EventId::with_ino_hint(new_path, child_ino, is_dir);
    for handle in FANOTIFY_REGISTRY.all_handles() {
        handle.deliver_rename(&mut old_id, &mut new_id, pid);
    }
}

/// Event component kinds for `plan_records` (Linux fsnotify hook classes).
const KIND_DIRENT: u8 = 0;
const KIND_SELF: u8 = 1;
const KIND_OBJ: u8 = 2;

/// Compute the info-record plan for one event component, mirroring Linux
/// `fanotify_fid_inode` / `fanotify_dfid_inode` / `fanotify_alloc_event`.
/// `has_dir` says whether the fsnotify hook carried a parent directory (always
/// for dirent events, never for self events, and for object events only when
/// a parent inode mark watches children).
/// Returns `(dir_fh, obj_fh)`: `dir_fh` is a directory fid record with the
/// recorded name (None = record without name), `obj_fh` the object's own fid.
fn plan_records(
    fid_flags: u32,
    kind: u8,
    id: &mut EventId,
    has_dir: bool,
) -> Option<(Option<(u64, Option<String>)>, Option<u64>)> {
    let has_fid = fid_flags & FAN_REPORT_FID != 0;
    let has_dfid = fid_flags & FAN_REPORT_DIR_FID != 0;
    let has_name = fid_flags & FAN_REPORT_NAME != 0;
    let has_target = fid_flags & FAN_REPORT_TARGET_FID != 0;
    let dirent = kind == KIND_DIRENT;
    let ondir = id.is_dir();
    // Object identity (fanotify_fid_inode): dirent events identify the
    // modified directory unless FAN_REPORT_TARGET_FID; all other events the
    // triggering object itself.
    let id_ino = if dirent && !has_target { id.parent_ino() } else { id.event_ino() };
    // Directory identity (fanotify_dfid_inode): the modified dir for dirent
    // events, the victim itself when it is a directory, otherwise the hook's
    // parent dir when it carried one.
    let dirid = if dirent {
        Some(id.parent_ino())
    } else if ondir {
        Some(id.event_ino())
    } else if has_dir {
        Some(id.parent_ino())
    } else {
        None
    };
    if has_dfid {
        if let Some(dirid) = dirid {
            // Additional child fid record (fanotify_report_child_fid): dirent
            // events only with FAN_REPORT_TARGET_FID; other events when the
            // group also has FAN_REPORT_FID.
            let report_child = if dirent { has_target } else { has_fid };
            let child = (report_child && id_ino != dirid).then_some(id_ino);
            // Name recording (fanotify_alloc_event): without FAN_REPORT_NAME a
            // name event is still needed to hold two fids; with it, dirent
            // events and events reported with a parent dir record the name,
            // while events on the directory itself stay a plain fid event
            // (read() synthesizes the "." DFID_NAME record for those).
            let mut name: Option<String> = None;
            let name_event = if !has_name {
                child.is_some()
            } else if dirent || !ondir {
                if dirent || has_dir {
                    name = Some(basename_of(id.path));
                }
                true
            } else {
                false
            };
            if name_event && (name.is_some() || child.is_some()) {
                return Some((Some((dirid, name)), child));
            }
            return Some((None, Some(dirid)));
        }
    }
    Some((None, Some(id_ino)))
}

/// Append one `fanotify_event_info_fid` record: header(4) + fsid(8) +
/// file_handle(4 + 4 + FHANDLE_BYTES) + optional NUL-terminated name padded to
/// a 4-byte boundary. The handle matches `name_to_handle_at(2)` for the inode.
fn push_fid_record(out: &mut Vec<u8>, info_type: u8, ino: u64, name: Option<&str>) {
    let name_pad = name.map(|n| (n.len() + 1 + 3) & !3).unwrap_or(0);
    let info_len: u16 = (4 + 8 + 4 + 4 + FHANDLE_BYTES as usize + name_pad) as u16;
    out.push(info_type);
    out.push(0); // pad
    out.extend_from_slice(&info_len.to_le_bytes());
    // __kernel_fsid_t fsid (two ints, zero — matches our statfs f_fsid)
    out.extend_from_slice(&[0u8; 8]);
    // struct file_handle { u32 handle_bytes; i32 handle_type; u8 f_handle[]; }
    out.extend_from_slice(&FHANDLE_BYTES.to_le_bytes());
    out.extend_from_slice(&FHANDLE_TYPE.to_le_bytes());
    out.extend_from_slice(&ino.to_le_bytes());
    if let Some(n) = name {
        out.extend_from_slice(n.as_bytes());
        out.resize(out.len() + (name_pad - n.len()), 0);
    }
}

/// Serialize a FID-mode event: 24-byte `fanotify_event_metadata` (fd ==
/// FAN_NOFD) followed by the event's info records in Linux
/// `copy_info_records_to_user` order: dir fid (+name), rename new-dir fid
/// (+name2), then the object's own fid.
fn encode_event_fid_records(ev: &FanotifyEvent, fid_flags: u32) -> Vec<u8> {
    let mut recs: Vec<u8> = Vec::new();
    let mut have_dir_info = false;
    if let Some((ino, name)) = &ev.dir_fh {
        let info_type = if ev.mask & FAN_RENAME != 0 {
            FAN_EVENT_INFO_TYPE_OLD_DFID_NAME
        } else if name.is_some() {
            FAN_EVENT_INFO_TYPE_DFID_NAME
        } else {
            FAN_EVENT_INFO_TYPE_DFID
        };
        push_fid_record(&mut recs, info_type, *ino, name.as_deref());
        have_dir_info = true;
    }
    if let Some((ino, name)) = &ev.dir2_fh {
        push_fid_record(&mut recs, FAN_EVENT_INFO_TYPE_NEW_DFID_NAME, *ino, Some(name.as_str()));
        have_dir_info = true;
    }
    if let Some(ino) = ev.obj_fh {
        // Linux copy_info_records_to_user: a plain-FID group or an event that
        // already carried a dir record reports the object as FID; an event on
        // a directory in a FAN_REPORT_NAME group synthesizes DFID_NAME with
        // name "."; other directory-identifying events report DFID.
        let only_fid = fid_flags
            & (FAN_REPORT_DIR_FID | FAN_REPORT_NAME | FAN_REPORT_TARGET_FID)
            == 0;
        let (info_type, name) = if only_fid || have_dir_info {
            (FAN_EVENT_INFO_TYPE_FID, None)
        } else if fid_flags & FAN_REPORT_NAME != 0 && ev.mask & FAN_ONDIR != 0 {
            (FAN_EVENT_INFO_TYPE_DFID_NAME, Some("."))
        } else if ev.mask & (FAN_DIRENT_EVENTS | FAN_ONDIR) != 0 {
            (FAN_EVENT_INFO_TYPE_DFID, None)
        } else {
            (FAN_EVENT_INFO_TYPE_FID, None)
        };
        push_fid_record(&mut recs, info_type, ino, name);
    }
    let event_len: u32 = 24 + recs.len() as u32;
    let mut out = Vec::with_capacity(event_len as usize);
    // struct fanotify_event_metadata
    out.extend_from_slice(&event_len.to_le_bytes());
    out.push(3); // FANOTIFY_METADATA_VERSION
    out.push(0); // reserved
    out.extend_from_slice(&24u16.to_le_bytes()); // metadata_len
    out.extend_from_slice(&ev.mask.to_le_bytes());
    out.extend_from_slice(&FAN_NOFD.to_le_bytes()); // fd = FAN_NOFD
    out.extend_from_slice(&ev.pid.to_le_bytes());
    out.extend_from_slice(&recs);
    out
}

/// Basename of an absolute path, used for `FAN_REPORT_DFID_NAME` where the
/// info record carries the parent directory's file handle plus the child name.
fn basename_of(path: &str) -> alloc::string::String {
    let trimmed = path.trim_end_matches('/');
    let name = match trimmed.rfind('/') {
        Some(idx) => &trimmed[idx + 1..],
        None => trimmed,
    };
    alloc::string::String::from(name)
}

/// Serialize a `fanotify_event_info_pidfd` record (8 bytes) for the triggering
/// process `pid`. The record is:
///   struct fanotify_event_info_header { u8 info_type; u8 pad; u16 len; }
///   __s32 pidfd;
/// A live process yields a freshly-installed pidfd in the listener's fd table;
/// a process that has already exited yields FAN_NOPIDFD (Linux fanotify21).
fn encode_pidfd_info(pid: i32) -> [u8; 8] {
    let pidfd = make_event_pidfd(pid);
    let mut out = [0u8; 8];
    out[0] = FAN_EVENT_INFO_TYPE_PIDFD;
    out[1] = 0; // pad
    out[2..4].copy_from_slice(&8u16.to_le_bytes()); // hdr.len = sizeof record
    out[4..8].copy_from_slice(&pidfd.to_le_bytes());
    out
}

/// Create a pidfd for the triggering process at read time, installed in the
/// current (listener) process. Returns FAN_NOPIDFD if the target process is no
/// longer present (already reaped) and FAN_EPIDFD if pidfd creation fails.
fn make_event_pidfd(pid: i32) -> i32 {
    if pid <= 0 {
        return FAN_NOPIDFD;
    }
    let alive = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        table
            .as_ref()
            .and_then(|t| t.get(pid as usize))
            .map(|p| p.state != crate::sched::process::ProcessState::Zombie)
            .unwrap_or(false)
    };
    if !alive {
        return FAN_NOPIDFD;
    }
    let fd = crate::kernel::pidfd::create_pidfd_for_pid(pid as usize, 0o2);
    if fd < 0 {
        FAN_EPIDFD
    } else {
        fd as i32
    }
}

fn allocate_event_fd(path: &str) -> i32 {
    let inode = match crate::fs::lookup(path) {
        Ok(inode) => inode,
        Err(_) => return -1,
    };
    let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
        inode,
        0,
        Some(path.into()),
    ));
    // Linux FMODE_NONOTIFY: closing a kernel-supplied event fd must not emit a
    // fsnotify CLOSE event (otherwise other mount/fs marks observe a spurious
    // FAN_CLOSE_NOWRITE — fanotify09 groups 1/2 false positives).
    fd_obj.nonotify.store(true, core::sync::atomic::Ordering::Relaxed);
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(table) = table.as_mut() else {
        return -1;
    };
    let Some(pid) = table.current else {
        return -1;
    };
    let Some(process) = table.get_fd_process_mut(pid) else {
        return -1;
    };
    match crate::fs::fd::alloc(process, fd_obj, 0, true) {
        Ok(fd) => fd as i32,
        Err(_) => -1,
    }
}

impl File for FanotifyFile {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        if buf.len() < 24 { return Err(FsError::InvalidArgument); }
        let data = self.instance.read(buf.len())?;
        if data.is_empty() {
            return Err(FsError::WouldBlock);
        }
        let n = data.len().min(buf.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(n)
    }
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        // `struct fanotify_response { __s32 fd; __u32 response; }` —— 必须整 8 字节。
        // monitor 一次可以写多个 response（Linux 5.x 允许批量），按 8 字节切片处理。
        if buf.len() < 8 || buf.len() % 8 != 0 {
            return Err(FsError::InvalidArgument);
        }
        let mut consumed = 0;
        while consumed + 8 <= buf.len() {
            let fd = i32::from_le_bytes(buf[consumed..consumed + 4].try_into().unwrap());
            let response = u32::from_le_bytes(buf[consumed + 4..consumed + 8].try_into().unwrap());
            self.instance.respond(fd, response)?;
            consumed += 8;
        }
        Ok(consumed)
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::CharDevice, size: 0,
            mode: FileMode(0o020600),
            nlink: 1, uid: 0, gid: 0, atime: 0, mtime: 0, ctime: 0,
        })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
    fn poll_read(&self)  -> bool { self.instance.pending() }
    fn poll_write(&self) -> bool { true }
    fn register_waiter(&self, events: u32, pid: usize, _deadline: Option<u64>) {
        if events & crate::fs::READINESS_READ != 0 {
            self.instance.register_reader(pid);
        }
    }
    fn close_notify_last(&self) {
        self.instance.unregister_all();
    }
}

impl Inode for FanotifyFile {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
    fn as_fanotify(&self) -> Option<&FanotifyFile> { Some(self) }
    fn fanotify_fdinfo(&self) -> Option<Vec<(u64, u64, u32)>> {
        // FAN_MARK_IGNORED_SURV_MODIFY = 0x40 (reported in mflags).
        const FAN_MARK_IGNORED_SURV_MODIFY: u32 = 0x40;
        let inner = self.instance.inner.lock();
        let marks: Vec<(u64, u64, u32)> = inner.marks.values()
            .map(|m| {
                let mut mflags = 0u32;
                if m.ignored_surv_modify { mflags |= FAN_MARK_IGNORED_SURV_MODIFY; }
                (m.mask, m.ignored_mask, mflags)
            })
            .collect();
        Some(marks)
    }
}

