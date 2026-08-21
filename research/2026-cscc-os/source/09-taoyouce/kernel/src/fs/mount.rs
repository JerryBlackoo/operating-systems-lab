//! Mount tracking, mount flags, write-access denial, and MountInfo table.
//!
//! Extracted from `fs/mod.rs` to align with Linux `fs/namespace.c`.

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use crate::sync::SpinLock;
use super::{FileType, Inode};

/// The init (root) mount namespace id. Every process that never unshares a
/// mount namespace shares this id; their mounts live in the default VFS map
/// and carry `ns_id == INIT_MNT_NS` in the mount info table.
pub const INIT_MNT_NS: usize = 1;

/// Active mount namespace of the currently-running process. Maintained by the
/// scheduler (`switch_to`) and by `unshare`/`setns`. Read by mount bookkeeping
/// and path resolution to scope mounts to the running process's namespace.
/// A single global is sufficient because the contest runs with smp=1 (one
/// process on-CPU at a time).
pub static ACTIVE_MNT_NS: AtomicUsize = AtomicUsize::new(INIT_MNT_NS);

/// Mount namespace the currently-running process belongs to.
pub fn current_mnt_ns() -> usize { ACTIVE_MNT_NS.load(Ordering::Relaxed) }

/// Switch the active mount namespace (scheduler context switch / setns / unshare).
pub fn set_active_mnt_ns(id: usize) { ACTIVE_MNT_NS.store(id, Ordering::Relaxed); }

static READONLY_MOUNTS: SpinLock<BTreeSet<String>> = SpinLock::new(BTreeSet::new());
/// 只读挂载条数（含 "/"）。为 0 时 `is_readonly_path` 免锁直接放行——
/// unlink/open 写路径每 syscall 都要查一次，评测全程无 RO 挂载。
static READONLY_MOUNT_COUNT: AtomicUsize = AtomicUsize::new(0);
static NOEXEC_MOUNTS: SpinLock<BTreeSet<String>> = SpinLock::new(BTreeSet::new());
static NOSUID_MOUNTS: SpinLock<BTreeSet<String>> = SpinLock::new(BTreeSet::new());
static NODEV_MOUNTS: SpinLock<BTreeSet<String>> = SpinLock::new(BTreeSet::new());
static NOATIME_MOUNTS: SpinLock<BTreeSet<String>> = SpinLock::new(BTreeSet::new());
static NODIRATIME_MOUNTS: SpinLock<BTreeSet<String>> = SpinLock::new(BTreeSet::new());
static NOSYMFOLLOW_MOUNTS: SpinLock<BTreeSet<String>> = SpinLock::new(BTreeSet::new());

/// MNT_EXPIRE tracking: mounts that have been marked for expiry by a first
/// umount2(MNT_EXPIRE) call. A second call will actually unmount if still idle.
static EXPIRED_MOUNTS: SpinLock<BTreeSet<String>> = SpinLock::new(BTreeSet::new());

/// Mark a mount as expired (first MNT_EXPIRE call). Returns true if it was
/// already marked (second call should proceed with unmount).
pub fn mark_mount_expired(path: &str) -> bool {
    let mut set = EXPIRED_MOUNTS.lock();
    !set.insert(String::from(path))
}

/// Clear the expired mark for the mount containing `path`.
/// Any access to a file/directory within a mount resets the expire timer.
pub fn clear_mount_expired(path: &str) {
    let mount_point = {
        let table = MOUNT_INFO_TABLE.lock();
        let normalized = if path.is_empty() { "/" } else { path };
        table.iter()
            .filter(|m| {
                if m.target == "/" { return false; } // skip root
                if normalized == m.target { return true; }
                normalized.len() > m.target.len()
                    && normalized.as_bytes()[m.target.len()] == b'/'
                    && normalized.starts_with(&m.target)
            })
            .max_by_key(|m| m.target.len())
            .map(|m| m.target.clone())
    };
    if let Some(mp) = mount_point {
        let mut set = EXPIRED_MOUNTS.lock();
        set.remove(&mp);
    }
}

/// Remove expired mark when mount is actually unmounted.
pub fn remove_expired_mark(path: &str) {
    let mut set = EXPIRED_MOUNTS.lock();
    set.remove(path);
}

// ---- ETXTBSY tracking (Linux i_writecount equivalent) ----
//
// Linux inode.i_writecount is an atomic that supports both directions:
//   - Positive: number of fds open for writing → execve returns ETXTBSY
//   - Negative: number of processes executing this binary → open(O_WRONLY) returns ETXTBSY
//
// We use two separate maps for clarity:
//   EXEC_PATH_COUNT:  absolute path → count of processes executing this binary (deny_write_access)
//   INODE_WRITE_COUNT: ino → count of fds open for writing (get_write_access)
//
// The exec-deny direction is keyed by the executable's absolute path rather
// than its bare inode number. This kernel fabricates st_dev == 0 for every
// filesystem, so a bare inode number is NOT globally unique: an inode on the
// root fs can share its number with an unrelated inode on a loop-mounted ext2
// image. Keying by inode therefore produced false ETXTBSY collisions (e.g.
// fanotify05's running binary vs. fname_159 created on a loop ext2 mount).
// An absolute VFS path is globally unique, so it disambiguates across mounts
// while still matching the same-path open performed by creat07 / execve04.

static EXEC_PATH_COUNT: SpinLock<BTreeMap<String, usize>> = SpinLock::new(BTreeMap::new());
static INODE_WRITE_COUNT: SpinLock<BTreeMap<usize, usize>> = SpinLock::new(BTreeMap::new());

/// Called by execve after successfully loading a binary. Blocks open-for-write
/// on the executable's absolute path.
pub fn deny_write_access(path: &str) {
    let mut map = EXEC_PATH_COUNT.lock();
    *map.entry(String::from(path)).or_insert(0) += 1;
}

/// Called when a process exits (or re-execs) to release the exec deny on its
/// executable path.
pub fn allow_write_access(path: &str) {
    let mut map = EXEC_PATH_COUNT.lock();
    if let Some(count) = map.get_mut(path) {
        if *count <= 1 {
            map.remove(path);
        } else {
            *count -= 1;
        }
    }
}

/// Returns true if the path is currently being executed (write-denied).
pub fn is_write_denied(path: &str) -> bool {
    let map = EXEC_PATH_COUNT.lock();
    map.contains_key(path)
}

/// Called when a file is opened for writing. Blocks execve on this inode.
pub fn get_write_access(ino: usize) {
    let mut map = INODE_WRITE_COUNT.lock();
    *map.entry(ino).or_insert(0) += 1;
}

/// Called when a writable fd is closed.
pub fn put_write_access(ino: usize) {
    let mut map = INODE_WRITE_COUNT.lock();
    if let Some(count) = map.get_mut(&ino) {
        if *count <= 1 {
            map.remove(&ino);
        } else {
            *count -= 1;
        }
    }
}

/// Returns true if the inode has open-for-write fds (exec-denied).
pub fn has_write_access(ino: usize) -> bool {
    let map = INODE_WRITE_COUNT.lock();
    map.contains_key(&ino)
}

/// Returns true if any process has a writable fd open on a file under `mount_path`.
/// Used by MS_REMOUNT|MS_RDONLY to detect EBUSY.
pub fn has_open_writers_under(mount_path: &str) -> bool {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref t) = *table {
        for proc in t.iter() {
            for slot in proc.fd_table.iter() {
                if let Some(fd_obj) = slot {
                    let acc_mode = fd_obj.flags & 3; // O_ACCMODE
                    if acc_mode == 1 || acc_mode == 2 { // O_WRONLY or O_RDWR
                        if let Some(ref p) = fd_obj.path {
                            if p.starts_with(mount_path) {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }
    false
}

// ---- Dynamic mount info table for /proc/mounts & /proc/self/mountinfo ----

// ---- Mount propagation types (对标 Linux include/linux/mount.h) ----

/// Mount propagation type — determines how mount/unmount events propagate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MountPropagation {
    /// MS_PRIVATE: mount events do not propagate in or out.
    Private,
    /// MS_SHARED: mount events propagate to/from all peers in the peer group.
    Shared,
    /// MS_SLAVE: receives propagation from master but does not propagate back.
    Slave,
    /// MS_UNBINDABLE: like private, but cannot be bind-mounted.
    Unbindable,
}

/// Peer group ID for shared mounts.
static PEER_GROUP_NEXT: AtomicU32 = AtomicU32::new(1);

fn alloc_peer_group() -> u32 {
    PEER_GROUP_NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Public allocator for a fresh peer group id, used by recursive bind when an
/// attached subtree must be made shared on a shared destination parent.
pub fn alloc_peer_group_pub() -> u32 {
    alloc_peer_group()
}

/// Per-mount metadata mirroring Linux `struct mount` fields needed by
/// `/proc/mounts` (= `/proc/self/mounts`) and `/proc/self/mountinfo`.
#[derive(Clone)]
pub struct MountInfo {
    /// Unique mount ID (monotonically increasing).
    pub mount_id: u32,
    /// Parent mount ID (0 for root).
    pub parent_id: u32,
    /// Source device string passed to mount(2), e.g. "/dev/vda" or "none".
    pub source: String,
    /// Absolute mount-point path.
    pub target: String,
    /// Underlying subtree path that this mount's root dentry exposes (对标
    /// Linux mnt->mnt_root 在共享 dentry 树里的位置)。For a bind mount this is
    /// the resolved bind *source* path (which may be a subdirectory inside a
    /// shared mount); for a self-bind or a freshly mounted filesystem it equals
    /// `target`. Mount-event propagation maps a mountpoint between peers in this
    /// "dentry frame" rather than the mountpoint frame, so peers that expose
    /// different subtree roots (subdir bind of a shared mount) still receive the
    /// event at the correct path.
    pub root_path: String,
    /// Filesystem type string, e.g. "ext4", "tmpfs", "proc".
    pub fstype: String,
    /// Mount flags (MS_RDONLY etc.) stored as raw bits.
    pub flags: usize,
    /// Mount propagation type (对标 Linux mnt->mnt_flags & MNT_SHARED etc.)
    pub propagation: MountPropagation,
    /// Peer group ID (non-zero for Shared mounts, also for Slave's master group).
    pub peer_group: u32,
    /// Master peer group ID (non-zero for Slave mounts).
    pub master_group: u32,
    /// Mount namespace this entry belongs to (对标 Linux mnt->mnt_ns).
    pub ns_id: usize,
}

static MOUNT_ID_NEXT: AtomicU32 = AtomicU32::new(100);
pub static MOUNT_INFO_TABLE: SpinLock<Vec<MountInfo>> = SpinLock::new(Vec::new());

/// Initialise the mount info table with the three built-in mounts that are
/// always present when the kernel boots (root, proc, tmp).  Called once from
/// the FS init path.
pub fn mount_info_init() {
    let mut table = MOUNT_INFO_TABLE.lock();
    if !table.is_empty() { return; }
    let root_pg = alloc_peer_group();
    let root_id = MOUNT_ID_NEXT.fetch_add(1, Ordering::Relaxed);
    table.push(MountInfo {
        mount_id: root_id,
        parent_id: 0,
        source: String::from("/dev/vda"),
        target: String::from("/"),
        root_path: String::from("/"),
        fstype: String::from("ext4"),
        flags: 0,
        propagation: MountPropagation::Shared,
        peer_group: root_pg,
        master_group: 0,
        ns_id: INIT_MNT_NS,
    });
    let proc_id = MOUNT_ID_NEXT.fetch_add(1, Ordering::Relaxed);
    table.push(MountInfo {
        mount_id: proc_id,
        parent_id: root_id,
        source: String::from("proc"),
        target: String::from("/proc"),
        root_path: String::from("/proc"),
        fstype: String::from("proc"),
        flags: 0,
        propagation: MountPropagation::Shared,
        peer_group: alloc_peer_group(),
        master_group: 0,
        ns_id: INIT_MNT_NS,
    });
    let tmp_id = MOUNT_ID_NEXT.fetch_add(1, Ordering::Relaxed);
    table.push(MountInfo {
        mount_id: tmp_id,
        parent_id: root_id,
        source: String::from("tmpfs"),
        target: String::from("/tmp"),
        root_path: String::from("/tmp"),
        fstype: String::from("tmpfs"),
        flags: 0,
        propagation: MountPropagation::Shared,
        peer_group: alloc_peer_group(),
        master_group: 0,
        ns_id: INIT_MNT_NS,
    });
    let dev_id = MOUNT_ID_NEXT.fetch_add(1, Ordering::Relaxed);
    table.push(MountInfo {
        mount_id: dev_id,
        parent_id: root_id,
        source: String::from("devtmpfs"),
        target: String::from("/dev"),
        root_path: String::from("/dev"),
        fstype: String::from("devtmpfs"),
        flags: 0,
        propagation: MountPropagation::Shared,
        peer_group: alloc_peer_group(),
        master_group: 0,
        ns_id: INIT_MNT_NS,
    });
    let devpts_id = MOUNT_ID_NEXT.fetch_add(1, Ordering::Relaxed);
    table.push(MountInfo {
        mount_id: devpts_id,
        parent_id: dev_id,
        source: String::from("devpts"),
        target: String::from("/dev/pts"),
        root_path: String::from("/dev/pts"),
        fstype: String::from("devpts"),
        flags: 0,
        propagation: MountPropagation::Shared,
        peer_group: alloc_peer_group(),
        master_group: 0,
        ns_id: INIT_MNT_NS,
    });
}

/// Record a new mount in the info table.  Called from sys_mount after success.
/// New mounts inherit propagation from their parent mount (对标 Linux
/// propagate_mnt: child inherits parent's propagation type).
/// Returns the list of peer-propagated target paths so the caller can create
/// the corresponding real VFS mounts (对标 Linux propagate_mnt 创建真实子挂载，
/// 而不是只记录 mountinfo 条目).
pub fn mount_info_add(source: &str, target: &str, fstype: &str, flags: usize) -> Vec<(String, usize)> {
    mount_info_add_ex(source, target, fstype, flags, true)
}

/// Like `mount_info_add` but `propagate` controls whether the new mount is
/// propagated over its parent's propagation tree. Recursive bind/rbind clones
/// must NOT propagate per-submount: copy_tree replicates the source subtree
/// internally and only the *whole* tree is attached to the destination's
/// peers. Propagating each internal submount would spuriously re-mount it onto
/// the source's own peers (对标 Linux copy_tree：内部子挂载只复制不传播).
pub fn mount_info_add_ex(source: &str, target: &str, fstype: &str, flags: usize, propagate: bool) -> Vec<(String, usize)> {
    const MS_BIND: usize = 4096;
    let cur_ns = current_mnt_ns();
    let mut table = MOUNT_INFO_TABLE.lock();
    // The mount's exposed subtree root (root_path). For a bind it is the bind
    // source resolved into the *underlying* dentry frame: the containing mount's
    // own root_path plus the source's offset under that mount (对标 Linux:
    // clone_mnt 以源所在挂载的 mnt_root 为基准, bind 子目录时 mnt_root =
    // old->mnt_root 下的对应 dentry). Resolving through the container keeps
    // peers that bind the *same* underlying subtree (e.g. a disk stacked on a
    // shared child vs. a bind of that child) on a consistent root_path, while a
    // genuine subdir bind of a shared mount lands on a deeper root than its
    // peers. For a fresh filesystem the root is the mountpoint itself.
    let new_root = if flags & MS_BIND != 0 {
        table.iter().enumerate()
            .filter(|(_, m)| {
                if m.ns_id != cur_ns { return false; }
                if m.target == source { return true; }
                if m.target == "/" { return true; }
                source.starts_with(&m.target)
                    && source.as_bytes().get(m.target.len()) == Some(&b'/')
            })
            .max_by_key(|(i, m)| (m.target.len(), *i))
            .map(|(_, m)| {
                if m.target == source {
                    m.root_path.clone()
                } else if m.target == "/" {
                    alloc::format!("{}{}", m.root_path, &source[1..])
                } else {
                    alloc::format!("{}{}", m.root_path, &source[m.target.len()..])
                }
            })
            .unwrap_or_else(|| String::from(source))
    } else {
        String::from(target)
    };
    // bind 挂载的显示 source/fstype 取底层超级块（拥有 source 路径的栈顶
    // 挂载）的字段，对标 Linux /proc/mounts 对 bind 挂载显示底层设备，
    // 而非 bind 源路径（堆叠的 bind 行与源所在挂载行完全一致）。
    let (source, fstype) = if fstype == "none" && flags & MS_BIND != 0 {
        table.iter().enumerate()
            .filter(|(_, m)| {
                if m.ns_id != cur_ns { return false; }
                if m.target == source { return true; }
                if m.target == "/" { return true; }
                source.starts_with(&m.target)
                    && source.as_bytes().get(m.target.len()) == Some(&b'/')
            })
            .max_by_key(|(i, m)| (m.target.len(), *i))
            .map(|(_, m)| (m.source.clone(), m.fstype.clone()))
            .unwrap_or((String::from(source), String::from(fstype)))
    } else {
        (String::from(source), String::from(fstype))
    };
    let (source, fstype) = (source.as_str(), fstype.as_str());
    // 父挂载选择：最长前缀匹配（含 component 边界检查）；同一 target 上的
    // 堆叠挂载也是合法父（对标 Linux：新挂载落在栈顶挂载之内），
    // 同长度时取最后（栈顶）一条。
    let parent = table.iter().enumerate()
        .filter(|(_, m)| {
            if m.ns_id != cur_ns { return false; }
            if m.target == target { return true; }
            if m.target == "/" { return true; }
            target.starts_with(&m.target)
                && target.as_bytes().get(m.target.len()) == Some(&b'/')
        })
        .max_by_key(|(i, m)| (m.target.len(), *i))
        .map(|(_, m)| (m.mount_id, m.propagation, m.peer_group, m.target.clone(), m.root_path.clone()));
    let parent_id = parent.as_ref().map(|p| p.0).unwrap_or(0);
    let parent_prop = parent.as_ref().map(|p| p.1).unwrap_or(MountPropagation::Private);
    let parent_pg = parent.as_ref().map(|p| p.2).unwrap_or(0);
    let parent_target = parent.as_ref().map(|p| p.3.clone()).unwrap_or_default();
    let parent_root = parent.as_ref().map(|p| p.4.clone()).unwrap_or_default();
    let (propagation, peer_group, master_group) = match parent_prop {
        MountPropagation::Shared => (MountPropagation::Shared, alloc_peer_group(), 0),
        MountPropagation::Slave => (MountPropagation::Slave, 0, parent_pg),
        _ => (MountPropagation::Private, 0, 0),
    };
    let id = MOUNT_ID_NEXT.fetch_add(1, Ordering::Relaxed);
    table.push(MountInfo {
        mount_id: id,
        parent_id,
        source: String::from(source),
        target: String::from(target),
        root_path: new_root.clone(),
        fstype: String::from(fstype),
        flags,
        propagation,
        peer_group,
        master_group,
        ns_id: cur_ns,
    });
    let mut propagated: Vec<(String, usize)> = Vec::new();
    // Propagate the mount event over the parent's whole propagation tree
    // (对标 Linux propagate_mnt / propagation_next). The tree is reached by:
    //   - peers : every mount sharing a reachable peer group;
    //   - slaves: every mount whose master is a reachable peer group; a slave
    //             that is *also* shared makes its own peer group reachable, so
    //             propagation continues transitively down multi-level
    //             slave/shared chains.
    // Each propagated copy mirrors the receiver's own shared/slave structure
    // but with fresh group ids (a per-original-group remap), so subsequent
    // mounts/umounts under a copy propagate exactly like under its origin.
    if propagate && parent_prop == MountPropagation::Shared && parent_pg != 0 {
        let rel_path = target.strip_prefix(parent_target.as_str()).unwrap_or(target);
        // Canonical dentry-frame path of the mountpoint = the parent mount's
        // exposed subtree root + the mountpoint's offset under the parent's
        // mountpoint. Peers are re-anchored from this canonical path onto their
        // own exposed root, so a subdir bind of a shared mount (peer whose
        // root_path differs from the parent's) receives the event at the right
        // place (对标 Linux propagate_one: 在 mnt_root dentry 帧里换算挂载点).
        let canonical = if rel_path.is_empty() {
            parent_root.clone()
        } else {
            alloc::format!("{}{}", parent_root, rel_path)
        };
        // Snapshot (mount_id, target, peer_group, master_group, ns, root_path)
        // so we can read the table while pushing copies into it.
        // Peers/slaves may live in *other* mount namespaces (a shared mount
        // copied into a child ns by unshare stays a peer): propagation crosses
        // namespace boundaries, and each copy is created in its receiver's ns.
        let snap: Vec<(u32, String, u32, u32, usize, String)> = table.iter()
            .map(|m| (m.mount_id, m.target.clone(), m.peer_group, m.master_group, m.ns_id, m.root_path.clone()))
            .collect();
        // Set of (target, ns) currently occupied by an *unbindable* mount. A
        // propagation event does not stack onto an unbindable mountpoint (对标
        // Linux: MS_UNBINDABLE 挂载点不接收传播副本)。e.g. rbind36 中对端
        // parent/child1 是 runbindable 自绑定, DISK4 传播到此不应覆盖上去。
        let unbindable_pts: Vec<(String, usize)> = table.iter()
            .filter(|m| m.propagation == MountPropagation::Unbindable)
            .map(|m| (m.target.clone(), m.ns_id))
            .collect();
        // Closure of reachable original peer groups, starting at parent_pg.
        let mut reachable: Vec<u32> = alloc::vec![parent_pg];
        let mut wi = 0;
        while wi < reachable.len() {
            let g = reachable[wi];
            wi += 1;
            for (_id, _t, pg, master, _ns, _rp) in &snap {
                if *master == g && *pg != 0 && !reachable.contains(pg) {
                    reachable.push(*pg);
                }
            }
        }
        // Group remap: parent_pg -> the origin mount's freshly allocated group.
        let mut group_map: Vec<(u32, u32)> = alloc::vec![(parent_pg, peer_group)];
        // Decide each receiver's copy groups first (immutable borrow of snap),
        // then push (mutable borrow of table).
        struct Copy { ptgt: String, pg: u32, master: u32, ns: usize }
        let mut copies: Vec<Copy> = Vec::new();
        for (id, t, pg, master, ns, rp) in &snap {
            if *id == parent_id {
                continue;
            }
            let is_peer = *pg != 0 && reachable.contains(pg);
            let is_slave = *master != 0 && reachable.contains(master);
            if !is_peer && !is_slave {
                continue;
            }
            // Re-anchor the canonical mountpoint onto this peer's exposed root.
            // A peer only receives the event if the mountpoint dentry falls
            // within the subtree this peer exposes (its root_path); subdir-bind
            // peers expose a deeper root, so the same canonical path maps to a
            // different mountpoint path under each peer.
            let ptgt = if *rp == canonical {
                t.clone()
            } else if canonical.len() > rp.len()
                && canonical.starts_with(rp.as_str())
                && canonical.as_bytes().get(rp.len()) == Some(&b'/')
            {
                alloc::format!("{}{}", t, &canonical[rp.len()..])
            } else {
                continue;
            };
            // A propagated mount STACKS on top of whatever already occupies the
            // peer's corresponding path (对标 Linux propagate_mnt: 传播副本覆盖
            // 在已有挂载之上, 而非跳过). Only dedup within *this* propagation
            // pass so two peers mapping to the same path don't double-stack.
            if copies.iter().any(|c| c.ptgt == ptgt && c.ns == *ns) {
                continue;
            }
            // ...except an unbindable mountpoint does not receive propagation.
            if unbindable_pts.iter().any(|(t2, ns2)| *t2 == ptgt && *ns2 == *ns) {
                continue;
            }
            let copy_pg = if is_peer { remap_group(&mut group_map, *pg) } else { 0 };
            let copy_master = if is_slave { remap_group(&mut group_map, *master) } else { 0 };
            copies.push(Copy { ptgt, pg: copy_pg, master: copy_master, ns: *ns });
        }
        for c in copies {
            let prop_id = MOUNT_ID_NEXT.fetch_add(1, Ordering::Relaxed);
            let prop_parent = table.iter().enumerate()
                .filter(|(_, m)| {
                    if m.ns_id != c.ns { return false; }
                    if m.target == c.ptgt { return true; }
                    if m.target == "/" { return true; }
                    c.ptgt.starts_with(&m.target)
                        && c.ptgt.as_bytes().get(m.target.len()) == Some(&b'/')
                })
                .max_by_key(|(i, m)| (m.target.len(), *i))
                .map(|(_, m)| m.mount_id)
                .unwrap_or(0);
            let copy_prop = if c.pg != 0 {
                MountPropagation::Shared
            } else if c.master != 0 {
                MountPropagation::Slave
            } else {
                MountPropagation::Private
            };
            let copy_ns = c.ns;
            table.push(MountInfo {
                mount_id: prop_id,
                parent_id: prop_parent,
                source: String::from(source),
                target: c.ptgt,
                root_path: new_root.clone(),
                fstype: String::from(fstype),
                flags,
                propagation: copy_prop,
                peer_group: c.pg,
                master_group: c.master,
                ns_id: copy_ns,
            });
            let pt = table.last().map(|m| m.target.clone()).unwrap_or_default();
            propagated.push((pt, copy_ns));
        }
    }
    propagated
}

/// Map an original peer-group id to a fresh id for propagated copies, keeping a
/// stable per-call mapping so copies that mirror the same origin group join the
/// same new group.
fn remap_group(map: &mut Vec<(u32, u32)>, orig: u32) -> u32 {
    if let Some(&(_, n)) = map.iter().find(|(o, _)| *o == orig) {
        return n;
    }
    let n = alloc_peer_group();
    map.push((orig, n));
    n
}

/// Snapshot every mount entry of namespace `from` into a freshly created
/// namespace `to` (对标 Linux `copy_mnt_ns`: the new mount namespace starts as
/// a copy of the caller's mount tree). New mount ids are allocated for the
/// copies and parent links are remapped within the new namespace, but
/// peer_group / master_group are preserved unchanged so that a shared mount and
/// its clone in the child namespace remain peers — this is what lets later
/// mounts propagate across the namespace boundary. No-op if `to` already has
/// entries or is the init namespace.
pub fn snapshot_mount_info(from: usize, to: usize) {
    if to == INIT_MNT_NS {
        return;
    }
    let mut table = MOUNT_INFO_TABLE.lock();
    if table.iter().any(|m| m.ns_id == to) {
        return;
    }
    let src: Vec<MountInfo> = table.iter().filter(|m| m.ns_id == from).cloned().collect();
    // Allocate the new ids first so parent links can be remapped.
    let id_map: Vec<(u32, u32)> = src
        .iter()
        .map(|m| (m.mount_id, MOUNT_ID_NEXT.fetch_add(1, Ordering::Relaxed)))
        .collect();
    for m in &src {
        let new_id = id_map.iter().find(|(o, _)| *o == m.mount_id).map(|(_, n)| *n).unwrap();
        let new_parent = id_map
            .iter()
            .find(|(o, _)| *o == m.parent_id)
            .map(|(_, n)| *n)
            .unwrap_or(m.parent_id);
        table.push(MountInfo {
            mount_id: new_id,
            parent_id: new_parent,
            source: m.source.clone(),
            target: m.target.clone(),
            root_path: m.root_path.clone(),
            fstype: m.fstype.clone(),
            flags: m.flags,
            propagation: m.propagation,
            peer_group: m.peer_group,
            master_group: m.master_group,
            ns_id: to,
        });
    }
}

/// Reduce every *shared* mount in namespace `ns` to a **slave** of its former
/// peer group. Called right after `snapshot_mount_info` when a new mount
/// namespace is created together with a new **user** namespace
/// (`unshare(CLONE_NEWUSER|CLONE_NEWNS)` / `clone(...)`). Per Linux
/// `copy_mnt_ns`: a mount namespace owned by a different user namespace has its
/// shared mounts copied with `CL_SLAVE`, so the child still *receives*
/// propagation from the parent peer group but its own mounts/binds do **not**
/// propagate back up to the parent (mount events only flow to peers and down to
/// slaves, never to a master). This is what makes `unshare --user --mount
/// --propagation shared` keep a child bind invisible to the parent (unshare01
/// test 8).
pub fn reduce_shared_to_slave(ns: usize) {
    let mut table = MOUNT_INFO_TABLE.lock();
    for m in table.iter_mut() {
        if m.ns_id == ns
            && m.propagation == MountPropagation::Shared
            && m.peer_group != 0
        {
            m.master_group = m.peer_group;
            m.peer_group = 0;
            m.propagation = MountPropagation::Slave;
        }
    }
}

/// Bind 挂载的 clone 继承源挂载的传播属性（对标 Linux do_loopback /
/// clone_mnt）：
///   - 源为 shared：clone 与源互为 peer（同一 peer group）；
///   - 源为 slave ：clone 成为同一 master 的 slave（CL_SLAVE）——从 master
///     收传播但不回传，复制 slave 子树时必须保持。
/// `targets` 是本次 bind 创建的全部挂载（目标 + 传播副本）。
pub fn bind_join_source_peer_group(source: &str, targets: &[(String, usize)]) {
    let cur_ns = current_mnt_ns();
    let mut table = MOUNT_INFO_TABLE.lock();
    // Resolve the mount the bind *source* resides in — the longest-prefix
    // (topmost) mount whose subtree contains `source`, not only a mount whose
    // target equals `source`. Binding a subdirectory of a shared mount makes
    // the new mount a peer of that shared mount too (对标 Linux do_loopback:
    // clone_mnt 以源所在挂载 `old` 为基准, IS_MNT_SHARED(old) 即加入其 peer
    // group), even though `source` itself is not a mount point.
    let src_idx = match table.iter().enumerate()
        .filter(|(_, m)| {
            if m.ns_id != cur_ns { return false; }
            if m.target == source { return true; }
            if m.target == "/" { return true; }
            source.starts_with(&m.target)
                && source.as_bytes().get(m.target.len()) == Some(&b'/')
        })
        .max_by_key(|(i, m)| (m.target.len(), *i))
        .map(|(i, _)| i)
    {
        Some(i) => i,
        None => return,
    };
    let src_prop = table[src_idx].propagation;
    let src_pg = table[src_idx].peer_group;
    let src_master = table[src_idx].master_group;
    match src_prop {
        MountPropagation::Shared if src_pg != 0 => {
            // The clone joins the source's peer group. If the source is *also* a
            // slave (shared+slave), the clone inherits that master too (对标
            // Linux clone_mnt: IS_MNT_SHARED(old) 加入 mnt_share, 同时
            // IS_MNT_SLAVE(old) 时 mnt_master = old->mnt_master), so a later
            // make-slave that turns the source into a pure slave still leaves a
            // peer→slave→slave chain through which mount events propagate.
            for (t, ns) in targets {
                if let Some(i) = table.iter().rposition(|m| &m.target == t && m.ns_id == *ns) {
                    table[i].propagation = MountPropagation::Shared;
                    table[i].peer_group = src_pg;
                    table[i].master_group = src_master;
                }
            }
        }
        MountPropagation::Slave if src_master != 0 => {
            // 源是 slave：clone 成为同一 master 的 slave (CL_SLAVE)。但若该
            // clone 是挂到 shared 父上的 (mount_info_add 已给它分配了 peer
            // group 并向父的 peers 传播了副本)，则它同时是 shared——保留这个
            // peer group，附加 master，形成 Linux 的 slave+shared 状态：既从
            // master 收传播，又向自己的 peers 传播。直接清掉 peer group 会
            // 切断与 peer (如 share2/child2) 的传播链。
            for (t, ns) in targets {
                if let Some(i) = table.iter().rposition(|m| &m.target == t && m.ns_id == *ns) {
                    table[i].master_group = src_master;
                    if table[i].peer_group != 0 {
                        table[i].propagation = MountPropagation::Shared;
                    } else {
                        table[i].propagation = MountPropagation::Slave;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Set the propagation of the *topmost* mount at `target` (the most recently
/// stacked entry) to the given values. Used by recursive bind/rbind so that
/// each cloned submount inherits the propagation of its *specific* source
/// submount entry — not the topmost mount at the source path (对标 Linux
/// copy_tree+clone_mnt: clone 与对应源子挂载同 peer group / 同 master，逐条
/// 对应，而不是整棵子树都套用源路径栈顶的传播属性).
pub fn set_topmost_propagation(
    target: &str,
    prop: MountPropagation,
    peer_group: u32,
    master_group: u32,
) {
    let cur_ns = current_mnt_ns();
    let mut table = MOUNT_INFO_TABLE.lock();
    if let Some(i) = table.iter().rposition(|m| m.target == target && m.ns_id == cur_ns) {
        table[i].propagation = prop;
        table[i].peer_group = peer_group;
        table[i].master_group = master_group;
    }
}

/// Set the root_path of the *topmost* mount at `target`. Used by copy_tree
/// during recursive bind so that each cloned submount inherits the root_path
/// of its *specific* source submount entry — not a root_path re-derived from
/// the source's display source string (which may be a fstype like "tmpfs",
/// not a real path, causing bogus root_path values).
pub fn set_topmost_root_path(target: &str, root_path: &str) {
    let cur_ns = current_mnt_ns();
    let mut table = MOUNT_INFO_TABLE.lock();
    if let Some(i) = table.iter().rposition(|m| m.target == target && m.ns_id == cur_ns) {
        table[i].root_path = String::from(root_path);
    }
}

/// Change the propagation type of a mount at the given path.
/// Called by mount(2) when MS_SHARED|MS_PRIVATE|MS_SLAVE|MS_UNBINDABLE is set.
pub fn set_mount_propagation(target: &str, prop: MountPropagation, recursive: bool) {
    let cur_ns = current_mnt_ns();
    let mut table = MOUNT_INFO_TABLE.lock();
    // make-{shared,slave,...} operate on the single mount *resolved* at the
    // path — i.e. the top of the stack at `target` — plus (when recursive) the
    // mounts in its subtree. It must NOT touch the other mounts stacked at the
    // identical path below it (对标 Linux do_change_type: 作用于路径解析到的那
    // 个 mount 对象，而非同路径下方所有堆叠挂载). Converting every co-stacked
    // entry would, e.g., turn a bind-from-shared child's *base* mount into a
    // slave and sever its peer link to the source, breaking propagate_umount.
    let top_idx = match table.iter().rposition(|m| m.target == target && m.ns_id == cur_ns) {
        Some(i) => i,
        None => return,
    };
    let mut targets: Vec<usize> = alloc::vec![top_idx];
    if recursive {
        // Subtree match: every mount strictly *under* `target`. For the root
        // mount ("/") the naive `format!("{}/", target)` yields "//", which no
        // mountpoint starts with, so a recursive make-{private,shared,slave} on
        // "/" would wrongly skip every submount (e.g. /tmp). Normalise the root
        // case to "/" so it matches all absolute mountpoints (对标 Linux: "/"
        // 的子树即整棵挂载树).
        let prefix = if target == "/" {
            String::from("/")
        } else {
            alloc::format!("{}/", target)
        };
        for (i, m) in table.iter().enumerate() {
            if i != top_idx && m.ns_id == cur_ns && m.target.starts_with(&prefix) {
                targets.push(i);
            }
        }
    }
    for idx in targets {
        match prop {
            MountPropagation::Shared => {
                // make-shared: join (or create) a peer group. A mount that is
                // already a slave stays a slave (keep master_group): Linux
                // allows "slave + shared" mounts, which both receive from their
                // master AND propagate to their own peers/slaves.
                table[idx].propagation = MountPropagation::Shared;
                if table[idx].peer_group == 0 {
                    table[idx].peer_group = alloc_peer_group();
                }
            }
            MountPropagation::Private => {
                table[idx].propagation = MountPropagation::Private;
                table[idx].peer_group = 0;
                table[idx].master_group = 0;
            }
            MountPropagation::Slave => {
                // make-slave: leave the peer group; the old peer group becomes
                // this mount's master. If it is already a slave (master set,
                // not shared) keep that master.
                let old_pg = table[idx].peer_group;
                if old_pg != 0 {
                    table[idx].master_group = old_pg;
                }
                table[idx].peer_group = 0;
                table[idx].propagation = if table[idx].master_group != 0 {
                    MountPropagation::Slave
                } else {
                    MountPropagation::Private
                };
            }
            MountPropagation::Unbindable => {
                table[idx].propagation = MountPropagation::Unbindable;
                table[idx].peer_group = 0;
                table[idx].master_group = 0;
            }
        }
    }
}

/// Remove a mount from the info table.  Called from sys_umount2 after success.
/// Also propagates unmount to peer mounts in the same peer group (对标 Linux
/// umount_tree → propagate_umount).
pub fn mount_info_remove(target: &str) {
    let cur_ns = current_mnt_ns();
    let mut table = MOUNT_INFO_TABLE.lock();
    // The mount being removed is the one path-resolution lands on: the *top of
    // the stack* at `target`. With propagation copies pushed in arbitrary
    // order, insertion order (rposition) is NOT the stack order — the real
    // topmost is the entry at `target` that no other entry at `target` uses as
    // its parent (对标 Linux: the leaf of the mountpoint's stack).
    let at_target: Vec<usize> = table.iter().enumerate()
        .filter(|(_, m)| m.target == target && m.ns_id == cur_ns)
        .map(|(i, _)| i)
        .collect();
    if at_target.is_empty() { return; }
    let m_idx = at_target.iter().copied().find(|&i| {
        let mid = table[i].mount_id;
        !at_target.iter().any(|&j| j != i && table[j].parent_id == mid)
    }).unwrap_or_else(|| *at_target.last().unwrap());
    let m_id = table[m_idx].mount_id;
    let m_pid = table[m_idx].parent_id;
    let m_pg = table[m_idx].peer_group;
    let m_master = table[m_idx].master_group;

    // propagate_umount: start with M, then for M's parent P's whole propagation
    // tree, also unmount the child stacked on each peer/slave of P at M's
    // mountpoint. Identity is by mount_id; a peer's "corresponding child" is the
    // mount whose parent_id == peer.mount_id and whose target equals
    // peer.target + rel (rel = M's path under P, "" when stacked on P).
    let mut remove_ids: Vec<u32> = alloc::vec![m_id];
    // Propagation parents to walk. Normally just M's stored parent (m_pid). But
    // when M is a propagation copy *stacked* on a same-path mount, its stored
    // parent_id points at the covered mount (e.g. C1), not at the directory
    // mount that actually owns the mountpoint dentry (对标 Linux: 传播副本与既有
    // 同路径挂载同为该目录挂载在同一 dentry 上的孩子)。在这种情况下还要从最近的
    // 真祖先目录挂载出发走一遍传播, 才能把 M 的对端副本一并卸载。
    let mut parents: Vec<(MountPropagation, u32, String, u32)> = table.iter()
        .find(|m| m.mount_id == m_pid)
        .map(|m| (m.propagation, m.peer_group, m.target.clone(), m.mount_id))
        .into_iter().collect();
    let m_pid_same_path = parents.first().map(|(_, _, t, _)| t == target).unwrap_or(false);
    if m_pid_same_path {
        if let Some(anc) = table.iter()
            .filter(|m| m.target != target
                && target.starts_with(m.target.as_str())
                && target.as_bytes().get(m.target.len()) == Some(&b'/'))
            .max_by_key(|m| m.target.len())
            .map(|m| (m.propagation, m.peer_group, m.target.clone(), m.mount_id))
        {
            parents.push(anc);
        }
    }
    for (p_prop, pg, parent_target, parent_id) in parents.into_iter() {
        if let (MountPropagation::Shared, true) = (p_prop, pg != 0) {
            let rel = target.strip_prefix(parent_target.as_str()).unwrap_or("");
            // Closure of reachable peer groups: peers of P plus, transitively,
            // the peer groups of slaves attached to any reachable group.
            let mut reachable: Vec<u32> = alloc::vec![pg];
            let mut wi = 0;
            while wi < reachable.len() {
                let g = reachable[wi];
                wi += 1;
                for m in table.iter() {
                    if m.master_group == g && m.peer_group != 0
                        && !reachable.contains(&m.peer_group) {
                        reachable.push(m.peer_group);
                    }
                }
            }
            // Each peer/slave mount pp in P's propagation tree. Carry pp's
            // namespace: with per-mount-namespace isolation, peers in different
            // namespaces share the *same* target string and peer_group (the
            // snapshot preserves them), so the corresponding child must be
            // disambiguated by pp 的命名空间, 否则 .find 只会命中第一条同名副本,
            // 漏掉别的命名空间里的同路径对端副本。
            let peers: Vec<(u32, String, usize)> = table.iter()
                .filter(|m| m.mount_id != parent_id
                    && ((m.peer_group != 0 && reachable.contains(&m.peer_group))
                        || (m.master_group != 0 && reachable.contains(&m.master_group))))
                .map(|m| (m.mount_id, m.target.clone(), m.ns_id))
                .collect();
            let _ = parent_id;
            for (_pp_id, pp_target, pp_ns) in &peers {
                let child_target = alloc::format!("{}{}", pp_target, rel);
                // M 的传播副本可能 *堆叠* 在该对端路径既有挂载之上 (propagate_mnt
                // 把副本盖在已有挂载上时, 其 parent_id 指向被盖的那条而非对端目录
                // 挂载)。因此按 *传播组身份* 而非 parent_id 来认定副本: 在
                // child_target 处找与 M 同 peer group / 互为 master-slave 的那条。
                // 预先存在、与 M 无传播关系的挂载不是副本, 不连带卸载。副本必须与
                // pp 处于同一命名空间。
                if let Some((c_id, _c_pg, _c_master)) = table.iter()
                    .filter(|m| m.target == child_target && m.ns_id == *pp_ns)
                    .map(|m| (m.mount_id, m.peer_group, m.master_group))
                    .find(|(_, c_pg, c_master)| {
                        (m_pg != 0 && (*c_pg == m_pg || *c_master == m_pg))
                        || (m_master != 0 && (*c_master == m_master || *c_pg == m_master))
                    })
                {
                    // propagate_umount 只摘掉对端上"没有自己子挂载"的对应子挂载
                    // (对标 Linux __propagate_umount: child && list_empty(
                    // &child->mnt_mounts))。若该子挂载之上仍堆叠/挂着别的挂载,
                    // 它处于 busy 状态,不随传播被卸载,得以保留。
                    let has_submount = table.iter()
                        .any(|m| m.parent_id == c_id && m.mount_id != c_id);
                    if !has_submount && !remove_ids.contains(&c_id) {
                        remove_ids.push(c_id);
                    }
                }
            }
        }
    }

    // Expand to the full subtree of every removed mount: any mount stacked on
    // top of it or mounted under it goes away with it (parent_id descendants).
    let mut i = 0;
    while i < remove_ids.len() {
        let pid = remove_ids[i];
        i += 1;
        for m in table.iter() {
            if m.parent_id == pid && !remove_ids.contains(&m.mount_id) {
                remove_ids.push(m.mount_id);
            }
        }
    }

    // VFS side: pop one stack entry per removed mount path. The caller already
    // popped one entry at `target` (the M leaf), so skip a single `target`.
    // Each removed mount is unmounted in *its own* namespace's VFS map (peers
    // in other namespaces are removed by propagation, cross-ns).
    let mut vfs_paths: Vec<(String, usize)> = table.iter()
        .filter(|m| remove_ids.contains(&m.mount_id))
        .map(|m| (m.target.clone(), m.ns_id))
        .collect();
    if let Some(p) = vfs_paths.iter().position(|(p, ns)| p == target && *ns == cur_ns) {
        vfs_paths.remove(p);
    }
    for (p, ns) in &vfs_paths {
        let _ = super::unmount_in_ns(p, *ns);
    }

    // Reparent survivors whose parent is being removed onto the nearest
    // surviving ancestor (对标 Linux umount_tree splicing the stack), so stored
    // parent_id links stay valid for later umount propagation.
    let pid_of: Vec<(u32, u32)> = table.iter()
        .map(|m| (m.mount_id, m.parent_id))
        .collect();
    for m in table.iter_mut() {
        if remove_ids.contains(&m.mount_id) { continue; }
        let mut pid = m.parent_id;
        while remove_ids.contains(&pid) {
            pid = pid_of.iter().find(|(id, _)| *id == pid).map(|(_, p)| *p).unwrap_or(0);
        }
        m.parent_id = pid;
    }

    table.retain(|m| !remove_ids.contains(&m.mount_id));
}

/// Validate a `mount --move` per Linux do_move_mount restrictions.
/// Returns 0 if allowed, or -EINVAL (-22). Two cases that must fail:
///   - the source's current parent is shared (the mount is reachable via
///     propagation, so moving it is ambiguous — fs_bind_move17);
///   - an unbindable source moved onto a shared destination parent (it cannot
///     be cloned for the parent's peers — fs_bind_move13).
pub fn mount_move_check(source: &str, target: &str) -> i32 {
    let cur_ns = current_mnt_ns();
    let table = MOUNT_INFO_TABLE.lock();
    let src_idx = match table.iter().rposition(|m| m.target == source && m.ns_id == cur_ns) {
        Some(i) => i,
        None => return -22,
    };
    let src_prop = table[src_idx].propagation;
    let parent_shared = |path: &str, exclude_self: bool| -> bool {
        table.iter().enumerate()
            .filter(|(_, m)| {
                if m.ns_id != cur_ns { return false; }
                if exclude_self && m.target == path { return false; }
                m.target == "/"
                    || (path.starts_with(&m.target)
                        && path.as_bytes().get(m.target.len()) == Some(&b'/'))
            })
            .max_by_key(|(i, m)| (m.target.len(), *i))
            .map(|(_, m)| m.propagation == MountPropagation::Shared)
            .unwrap_or(false)
    };
    if parent_shared(source, true) {
        return -22;
    }
    if src_prop == MountPropagation::Unbindable && parent_shared(target, false) {
        return -22;
    }
    0
}

/// Relocate a mount subtree (对标 Linux do_move_mount / attach_recursive_mnt).
/// Detaches the mount at `source` and any submounts under it WITHOUT
/// propagating an unmount (a move is not an unmount of the source's peers),
/// reattaches the tree at `target`, then — if the new parent is shared —
/// propagates the moved tree to the new parent's propagation tree. Each copy
/// keeps the moved mount's own group ids, so it becomes a peer of the moved
/// mount, exactly like clones produced before the move.
///
/// Returns the list of propagated copies created in the new parent's
/// propagation tree as `(full_target_path, rel_under_moved_tree)` pairs, so the
/// caller can install matching real VFS mounts (对标 propagate_mnt 创建真实
/// 挂载，而非仅记 mountinfo). The primary destination is omitted because the
/// VFS subtree was already relocated there by `move_mount_subtree`.
pub fn mount_info_move(source: &str, target: &str) -> Vec<(String, String, usize)> {
    let cur_ns = current_mnt_ns();
    let mut table = MOUNT_INFO_TABLE.lock();
    let src_idx = match table.iter().rposition(|m| m.target == source && m.ns_id == cur_ns) {
        Some(i) => i,
        None => return Vec::new(),
    };
    let moved = table[src_idx].clone();
    let sub_prefix = alloc::format!("{}/", source);
    // The moved tree as (rel_path_under_source, pg, master, source, fstype,
    // flags, orig_id, orig_parent_id). rel "" is the root. orig_id/orig_parent_id
    // preserve the *internal* stacking/parent structure of the moved subtree so
    // it can be reconstructed at the destination — without them, several mounts
    // stacked at the same path (e.g. a child1 bind + a DISK propagated onto it)
    // would all be reparented to the longest strict-ancestor and lose their
    // mutual parent/child links (对标 Linux: 移动整棵子树时保持其内部 mnt_parent).
    let mut tree: Vec<(String, u32, u32, String, String, usize, u32, u32, String)> =
        alloc::vec![(String::new(), moved.peer_group, moved.master_group,
                     moved.source.clone(), moved.fstype.clone(), moved.flags,
                     moved.mount_id, moved.parent_id, moved.root_path.clone())];
    for m in table.iter().filter(|m| m.ns_id == cur_ns && m.target.starts_with(&sub_prefix)) {
        let rel = m.target[source.len()..].to_string();
        tree.push((rel, m.peer_group, m.master_group,
                   m.source.clone(), m.fstype.clone(), m.flags,
                   m.mount_id, m.parent_id, m.root_path.clone()));
    }
    // Detach the whole subtree without propagation (current namespace only).
    table.retain(|m| !(m.ns_id == cur_ns && (m.target == source || m.target.starts_with(&sub_prefix))));

    // Resolve the new parent (longest prefix + topmost stacked mount).
    let parent = table.iter().enumerate()
        .filter(|(_, m)| {
            if m.ns_id != cur_ns { return false; }
            if m.target == "/" { return true; }
            target.starts_with(&m.target)
                && target.as_bytes().get(m.target.len()) == Some(&b'/')
        })
        .max_by_key(|(i, m)| (m.target.len(), *i))
        .map(|(_, m)| (m.propagation, m.peer_group, m.mount_id, m.target.clone()));
    let dest_parent_id = parent.as_ref().map(|p| p.2).unwrap_or(0);

    // Instantiate the moved tree at a base path, resolving each entry's parent
    // by longest-prefix against the live table. Mutates table; returns the
    // (full_target, rel) pairs created.
    let mut instantiate = |table: &mut Vec<MountInfo>, base: &str,
                           tree: &[(String, u32, u32, String, String, usize, u32, u32, String)]|
     -> Vec<(String, String, usize)> {
        let mut created = Vec::new();
        // Map original mount ids of this subtree to their freshly-allocated ids
        // so internal parent/child (and stacking) links survive the move.
        let mut id_map: Vec<(u32, u32)> = Vec::new();
        for (rel, pg, master, src, fst, fl, orig_id, orig_pid, rpath) in tree {
            let tgt = alloc::format!("{}{}", base, rel);
            created.push((tgt.clone(), rel.clone(), cur_ns));
            let id = MOUNT_ID_NEXT.fetch_add(1, Ordering::Relaxed);
            id_map.push((*orig_id, id));
            // If this entry's original parent is part of the moved subtree, keep
            // the (remapped) internal link; otherwise resolve the destination
            // parent by longest strict-ancestor prefix.
            let pid = if let Some((_, mapped)) =
                id_map.iter().find(|(o, _)| o == orig_pid)
            {
                *mapped
            } else {
                table.iter()
                    .filter(|m| {
                        m.ns_id == cur_ns && (m.target == "/"
                            || (tgt.starts_with(&m.target)
                                && tgt.as_bytes().get(m.target.len()) == Some(&b'/')))
                    })
                    .max_by_key(|m| m.target.len())
                    .map(|m| m.mount_id)
                    .unwrap_or(dest_parent_id)
            };
            let prop = if *pg != 0 {
                MountPropagation::Shared
            } else if *master != 0 {
                MountPropagation::Slave
            } else {
                MountPropagation::Private
            };
            table.push(MountInfo {
                mount_id: id,
                parent_id: pid,
                source: src.clone(),
                target: tgt,
                root_path: rpath.clone(),
                fstype: fst.clone(),
                flags: *fl,
                propagation: prop,
                peer_group: *pg,
                master_group: *master,
                ns_id: cur_ns,
            });
        }
        created
    };
    // Moving onto a shared parent makes the moved tree shared too (对标
    // Linux CL_MAKE_SHARED in attach_recursive_mnt): any private mount in the
    // tree joins a fresh peer group, so subsequent mounts under it propagate
    // to the new parent's peers. Already-shared mounts keep their group, so
    // they stay peers with clones made before the move.
    let dest_dpg = match &parent {
        Some((MountPropagation::Shared, dpg, _, _)) if *dpg != 0 => *dpg,
        _ => 0,
    };
    if dest_dpg != 0 {
        for e in tree.iter_mut() {
            if e.1 == 0 {
                e.1 = alloc_peer_group();
            }
        }
    }
    // Reattach the moved tree at the destination.
    instantiate(&mut table, target, &tree);

    let mut propagated: Vec<(String, String, usize)> = Vec::new();
    // Propagate the moved tree to the new parent's propagation tree.
    if let Some((MountPropagation::Shared, dpg, _, dparent_target)) = parent {
        if dpg != 0 {
            let rel_at_parent = target.strip_prefix(dparent_target.as_str()).unwrap_or("");
            let snap: Vec<(u32, String, u32, u32)> = table.iter()
                .filter(|m| m.ns_id == cur_ns)
                .map(|m| (m.mount_id, m.target.clone(), m.peer_group, m.master_group))
                .collect();
            let mut reachable: Vec<u32> = alloc::vec![dpg];
            let mut wi = 0;
            while wi < reachable.len() {
                let g = reachable[wi];
                wi += 1;
                for (_id, _t, pg, master) in &snap {
                    if *master == g && *pg != 0 && !reachable.contains(pg) {
                        reachable.push(*pg);
                    }
                }
            }
            let mut bases: Vec<String> = Vec::new();
            for (id, t, pg, master) in &snap {
                if *id == dest_parent_id {
                    continue;
                }
                let is_peer = *pg != 0 && reachable.contains(pg);
                let is_slave = *master != 0 && reachable.contains(master);
                if !is_peer && !is_slave {
                    continue;
                }
                let base = alloc::format!("{}{}", t, rel_at_parent);
                if table.iter().any(|m| m.target == base && m.ns_id == cur_ns) {
                    continue;
                }
                bases.push(base);
            }
            for base in bases {
                propagated.extend(instantiate(&mut table, &base, &tree));
            }
        }
    }
    propagated
}

/// Generate `/proc/mounts` output from the live mount table.
/// Format: `source target fstype options dump pass`
pub fn generate_proc_mounts() -> String {
    let cur_ns = current_mnt_ns();
    let table = MOUNT_INFO_TABLE.lock();
    let ro_mounts = READONLY_MOUNTS.lock();
    let mut out = String::new();
    for m in table.iter().filter(|m| m.ns_id == cur_ns) {
        let opts = if ro_mounts.contains(&m.target) || m.flags & 1 != 0 {
            "ro,relatime"
        } else {
            "rw,relatime"
        };
        // Special per-fstype options to match Linux defaults
        let extra = match m.fstype.as_str() {
            "proc" => ",nosuid,nodev,noexec",
            "tmpfs" => ",nosuid,nodev",
            "devtmpfs" => ",nosuid",
            "devpts" => ",nosuid,noexec",
            _ => "",
        };
        out.push_str(&alloc::format!(
            "{} {} {} {}{} 0 0\n",
            m.source, m.target, m.fstype, opts, extra
        ));
    }
    out
}

/// Generate `/proc/self/mountinfo` output from the live mount table.
/// Format per line (Linux fs/proc_namespace.c):
///   mount_id parent_id major:minor root mount_point mount_options optional_fields - fstype source super_options
pub fn generate_proc_mountinfo() -> String {
    let cur_ns = current_mnt_ns();
    let table = MOUNT_INFO_TABLE.lock();
    let ro_mounts = READONLY_MOUNTS.lock();
    let mut out = String::new();
    for m in table.iter().filter(|m| m.ns_id == cur_ns) {
        let opts = if ro_mounts.contains(&m.target) || m.flags & 1 != 0 {
            "ro,relatime"
        } else {
            "rw,relatime"
        };
        // Optional fields: shared:X, master:Y (对标 Linux show_mountinfo)
        let mut optional = String::new();
        match m.propagation {
            MountPropagation::Shared => {
                optional.push_str(&alloc::format!("shared:{}", m.peer_group));
            }
            MountPropagation::Slave => {
                optional.push_str(&alloc::format!("master:{}", m.master_group));
                if m.peer_group != 0 {
                    optional.push_str(&alloc::format!(" shared:{}", m.peer_group));
                }
            }
            _ => {}
        }
        if !optional.is_empty() {
            optional.push(' ');
        }
        out.push_str(&alloc::format!(
            "{} {} 0:0 / {} {} {}- {} {} {}\n",
            m.mount_id, m.parent_id, m.target, opts, optional, m.fstype, m.source, opts
        ));
    }
    out
}
pub fn set_readonly_mount(path: &str, readonly: bool) {
    let normalized = if path.is_empty() { "/" } else { path };
    let mut mounts = READONLY_MOUNTS.lock();
    if readonly {
        mounts.insert(normalized.to_string());
    } else {
        mounts.remove(normalized);
    }
    READONLY_MOUNT_COUNT.store(mounts.len(), Ordering::Release);
}

#[inline]
pub fn is_readonly_path(path: &str) -> bool {
    if READONLY_MOUNT_COUNT.load(Ordering::Acquire) == 0 {
        return false;
    }
    is_readonly_path_slow(path)
}

#[cold]
fn is_readonly_path_slow(path: &str) -> bool {
    let mounts = READONLY_MOUNTS.lock();
    mounts.iter().any(|mount| {
        path == mount
            || (mount != "/" && path.starts_with(mount) && path.as_bytes().get(mount.len()) == Some(&b'/'))
            || mount == "/"
    })
}

/// Resolve the filesystem type backing `path` by finding the longest-prefix
/// mount in the active namespace. Returns e.g. "hugetlbfs", "tmpfs", "ext4".
pub fn fstype_for_path(path: &str) -> Option<String> {
    let cur_ns = current_mnt_ns();
    let table = MOUNT_INFO_TABLE.lock();
    let normalized = if path.is_empty() { "/" } else { path };
    table.iter()
        .filter(|m| m.ns_id == cur_ns)
        .filter(|m| {
            m.target == "/"
                || normalized == m.target
                || (normalized.len() > m.target.len()
                    && normalized.as_bytes()[m.target.len()] == b'/'
                    && normalized.starts_with(&m.target))
        })
        .max_by_key(|m| m.target.len())
        .map(|m| m.fstype.clone())
}

pub fn set_noexec_mount(path: &str, noexec: bool) {
    let normalized = if path.is_empty() { "/" } else { path };
    let mut mounts = NOEXEC_MOUNTS.lock();
    if noexec {
        mounts.insert(normalized.to_string());
    } else {
        mounts.remove(normalized);
    }
}

pub fn is_noexec_path(path: &str) -> bool {
    let mounts = NOEXEC_MOUNTS.lock();
    mounts.iter().any(|mount| {
        path == mount
            || (mount != "/" && path.starts_with(mount) && path.as_bytes().get(mount.len()) == Some(&b'/'))
            || mount == "/"
    })
}

pub fn set_nosuid_mount(path: &str, nosuid: bool) {
    let normalized = if path.is_empty() { "/" } else { path };
    let mut mounts = NOSUID_MOUNTS.lock();
    if nosuid {
        mounts.insert(normalized.to_string());
    } else {
        mounts.remove(normalized);
    }
}

pub fn is_nosuid_path(path: &str) -> bool {
    let mounts = NOSUID_MOUNTS.lock();
    mounts.iter().any(|mount| {
        path == mount
            || (mount != "/" && path.starts_with(mount) && path.as_bytes().get(mount.len()) == Some(&b'/'))
            || mount == "/"
    })
}

pub fn set_nodev_mount(path: &str, nodev: bool) {
    let normalized = if path.is_empty() { "/" } else { path };
    let mut mounts = NODEV_MOUNTS.lock();
    if nodev {
        mounts.insert(normalized.to_string());
    } else {
        mounts.remove(normalized);
    }
}

pub fn is_nodev_path(path: &str) -> bool {
    let mounts = NODEV_MOUNTS.lock();
    mounts.iter().any(|mount| {
        path == mount
            || (mount != "/" && path.starts_with(mount) && path.as_bytes().get(mount.len()) == Some(&b'/'))
            || mount == "/"
    })
}

pub fn set_noatime_mount(path: &str, noatime: bool) {
    let normalized = if path.is_empty() { "/" } else { path };
    let mut mounts = NOATIME_MOUNTS.lock();
    if noatime {
        mounts.insert(normalized.to_string());
    } else {
        mounts.remove(normalized);
    }
}

pub fn is_noatime_path(path: &str) -> bool {
    let mounts = NOATIME_MOUNTS.lock();
    mounts.iter().any(|mount| {
        path == mount
            || (mount != "/" && path.starts_with(mount) && path.as_bytes().get(mount.len()) == Some(&b'/'))
            || mount == "/"
    })
}

pub fn set_nodiratime_mount(path: &str, nodiratime: bool) {
    let normalized = if path.is_empty() { "/" } else { path };
    let mut mounts = NODIRATIME_MOUNTS.lock();
    if nodiratime {
        mounts.insert(normalized.to_string());
    } else {
        mounts.remove(normalized);
    }
}

pub fn is_nodiratime_path(path: &str) -> bool {
    let mounts = NODIRATIME_MOUNTS.lock();
    mounts.iter().any(|mount| {
        path == mount
            || (mount != "/" && path.starts_with(mount) && path.as_bytes().get(mount.len()) == Some(&b'/'))
            || mount == "/"
    })
}

pub fn set_nosymfollow_mount(path: &str, nosymfollow: bool) {
    let normalized = if path.is_empty() { "/" } else { path };
    let mut mounts = NOSYMFOLLOW_MOUNTS.lock();
    if nosymfollow {
        mounts.insert(normalized.to_string());
    } else {
        mounts.remove(normalized);
    }
}

pub fn is_nosymfollow_path(path: &str) -> bool {
    let mounts = NOSYMFOLLOW_MOUNTS.lock();
    mounts.iter().any(|mount| {
        path == mount
            || (mount != "/" && path.starts_with(mount) && path.as_bytes().get(mount.len()) == Some(&b'/'))
            || mount == "/"
    })
}

/// Linux `touch_atime()` equivalent: update atime on an inode unless the mount
/// has MS_NOATIME (skips all atime), or MS_NODIRATIME (skips atime for dirs).
/// Called after successful read/readdir operations.
pub fn touch_atime(inode: &Arc<dyn Inode>, path: &str) {
    if is_noatime_path(path) {
        return;
    }
    if inode.file_type() == FileType::Directory && is_nodiratime_path(path) {
        return;
    }
    let now = crate::fs::ramfs_current_time_sec();
    let _ = inode.set_times(Some(now), None);
}

/// Expose current_time_sec for use outside ramfs.
pub fn ramfs_current_time_sec() -> u64 {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    if freq == 0 { 0 } else { crate::arch::asm_helpers::rdtime() as u64 / freq }
}
