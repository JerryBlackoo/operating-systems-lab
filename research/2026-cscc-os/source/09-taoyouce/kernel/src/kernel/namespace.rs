//! namespace subsystem — user/pid/mount/net/uts/ipc/cgroup/time namespace
//!
//! Extracted from compat.rs. Implements Linux namespace semantics per
//! `kernel/nsproxy.c`, `kernel/user_namespace.c`, `kernel/pid_namespace.c`.
//!
//! Each process holds a `NsProxy` referencing its active namespaces.
//! `unshare(2)` / `setns(2)` / `clone3(2)` manipulate these references.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sync::SpinLock;

// ---- Namespace types -------------------------------------------------------

/// Namespace identifier (inode number visible in /proc/pid/ns/)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct NsId(pub usize);

static NEXT_NS_ID: AtomicUsize = AtomicUsize::new(0xF000_0001);

/// child-userns-id -> parent-userns-id, recorded at userns creation so setns
/// can walk the ancestry chain (Linux create_user_ns links `ns->parent`).
static USERNS_PARENT: SpinLock<BTreeMap<usize, usize>> = SpinLock::new(BTreeMap::new());

/// Is `ancestor` the same as, or an ancestor of, user namespace `node`?
/// Used to approximate "caller has CAP_SYS_ADMIN in the target userns".
fn userns_is_ancestor_or_self(ancestor: usize, node: usize) -> bool {
    if ancestor == node { return true; }
    let map = USERNS_PARENT.lock();
    let mut cur = node;
    for _ in 0..64 {
        match map.get(&cur) {
            Some(&parent) => {
                if parent == ancestor { return true; }
                cur = parent;
            }
            None => break,
        }
    }
    false
}

fn alloc_ns_id() -> NsId {
    NsId(NEXT_NS_ID.fetch_add(1, Ordering::Relaxed))
}

/// CLONE_NEW* flags (same values as Linux)
pub const CLONE_NEWNS:      i32 = 0x0002_0000;
pub const CLONE_NEWCGROUP:  i32 = 0x0200_0000;
pub const CLONE_NEWUTS:     i32 = 0x0400_0000;
pub const CLONE_NEWIPC:     i32 = 0x0800_0000;
pub const CLONE_NEWUSER:    i32 = 0x1000_0000;
pub const CLONE_NEWPID:     i32 = 0x2000_0000;
pub const CLONE_NEWNET:     i32 = 0x4000_0000u32 as i32;
pub const CLONE_NEWTIME:    i32 = 0x0000_0080;
pub const CLONE_VM:         i32 = 0x0000_0100;
pub const CLONE_FS:         i32 = 0x0000_0200;
pub const CLONE_FILES:      i32 = 0x0000_0400;
pub const CLONE_SIGHAND:    i32 = 0x0000_0800;
pub const CLONE_THREAD:     i32 = 0x0001_0000;
pub const CLONE_SYSVSEM:    i32 = 0x0004_0000;

const KNOWN_UNSHARE_FLAGS: i32 = CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND
    | CLONE_THREAD | CLONE_NEWNS | CLONE_SYSVSEM | CLONE_NEWCGROUP
    | CLONE_NEWUTS | CLONE_NEWIPC | CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET
    | CLONE_NEWTIME;

const NS_BITS: i32 = CLONE_NEWNS | CLONE_NEWCGROUP | CLONE_NEWUTS
    | CLONE_NEWIPC | CLONE_NEWUSER | CLONE_NEWPID | CLONE_NEWNET | CLONE_NEWTIME;

// ---- Per-namespace state ---------------------------------------------------

/// UTS namespace: hostname + domainname
pub struct UtsNamespace {
    pub id: NsId,
    pub hostname: String,
    pub domainname: String,
}

impl UtsNamespace {
    pub fn new() -> Self {
        Self {
            id: alloc_ns_id(),
            hostname: String::from("localhost"),
            domainname: String::from("(none)"),
        }
    }
    pub fn clone_ns(&self) -> Self {
        Self {
            id: alloc_ns_id(),
            hostname: self.hostname.clone(),
            domainname: self.domainname.clone(),
        }
    }
}

/// User namespace: uid/gid mappings
pub struct UserNamespace {
    pub id: NsId,
    pub uid_map: Vec<IdMapEntry>,
    pub gid_map: Vec<IdMapEntry>,
    pub setgroups_allowed: bool,
    pub owner_uid: u32,
}

#[derive(Clone)]
pub struct IdMapEntry {
    pub inside: u32,
    pub outside: u32,
    pub count: u32,
}

/// Kernel `overflowuid`/`overflowgid` (`/proc/sys/kernel/overflowuid`): the
/// value `from_kuid_munged` returns when a kernel uid has no mapping in the
/// current user namespace. 对标 Linux 默认 65534 (nobody/nogroup)。
pub const OVERFLOW_UID: u32 = 65534;

/// Identity map covering the whole uid range — what the init user namespace
/// owns so that `from_kuid_munged` is a no-op there (kuid N → ns uid N).
fn full_identity_map() -> Vec<IdMapEntry> {
    alloc::vec![IdMapEntry { inside: 0, outside: 0, count: u32::MAX }]
}

impl UserNamespace {
    /// The **init** user namespace: a full identity uid/gid map so every kernel
    /// id maps to itself (对标 Linux init_user_ns 的全量映射)。
    pub fn new(owner_uid: u32) -> Self {
        Self {
            id: alloc_ns_id(),
            uid_map: full_identity_map(),
            gid_map: full_identity_map(),
            setgroups_allowed: true,
            owner_uid,
        }
    }
    /// A freshly created (child) user namespace starts with **empty** uid/gid
    /// maps — per Linux every kernel id is unmapped until userspace writes
    /// `/proc/<pid>/{uid,gid}_map`, so `getuid()` reports overflowuid (65534)
    /// in the meantime (对标 create_user_ns)。
    pub fn clone_ns(&self) -> Self {
        let id = alloc_ns_id();
        // Record the parent userns so setns(CLONE_NEWUSER) can enforce the
        // ancestry rule (a task may only join a userns it has CAP_SYS_ADMIN
        // in, i.e. an ancestor of its current userns — userns04).
        USERNS_PARENT.lock().insert(id.0, self.id.0);
        Self {
            id,
            uid_map: Vec::new(),
            gid_map: Vec::new(),
            setgroups_allowed: self.setgroups_allowed,
            owner_uid: self.owner_uid,
        }
    }
    /// Map an inside UID to outside UID using the uid_map entries
    pub fn map_uid_to_outside(&self, inside_uid: u32) -> Option<u32> {
        for entry in &self.uid_map {
            if inside_uid >= entry.inside && (inside_uid - entry.inside) < entry.count {
                return Some(entry.outside + (inside_uid - entry.inside));
            }
        }
        None
    }
    pub fn map_gid_to_outside(&self, inside_gid: u32) -> Option<u32> {
        for entry in &self.gid_map {
            if inside_gid >= entry.inside && (inside_gid - entry.inside) < entry.count {
                return Some(entry.outside + (inside_gid - entry.inside));
            }
        }
        None
    }
    /// Translate a kernel (outside) UID into the id seen *inside* this user
    /// namespace — what `getuid(2)` returns (对标 from_kuid_munged): find the
    /// uid_map entry whose outside range contains `kuid`; if none, the id is
    /// unmapped and the kernel reports `OVERFLOW_UID`.
    pub fn map_uid_from_outside(&self, kuid: u32) -> Option<u32> {
        for entry in &self.uid_map {
            if kuid >= entry.outside && (kuid - entry.outside) < entry.count {
                return Some(entry.inside + (kuid - entry.outside));
            }
        }
        None
    }
    pub fn map_gid_from_outside(&self, kgid: u32) -> Option<u32> {
        for entry in &self.gid_map {
            if kgid >= entry.outside && (kgid - entry.outside) < entry.count {
                return Some(entry.inside + (kgid - entry.outside));
            }
        }
        None
    }
}

/// Parse a `/proc/<pid>/{uid,gid}_map` write payload into id-map entries.
/// Each non-empty line is `inside outside count` (whitespace separated, 对标
/// Linux map_write 的三列十进制格式)。Returns `None` on a malformed line.
fn parse_id_map(data: &str) -> Option<Vec<IdMapEntry>> {
    let mut out = Vec::new();
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut it = line.split_ascii_whitespace();
        let inside = it.next()?.parse::<u32>().ok()?;
        let outside = it.next()?.parse::<u32>().ok()?;
        let count = it.next()?.parse::<u32>().ok()?;
        if it.next().is_some() || count == 0 {
            return None;
        }
        out.push(IdMapEntry { inside, outside, count });
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Write handler for `/proc/<pid>/{uid,gid}_map` — parse `data` and install the
/// mapping into process `pid`'s user namespace (对标 proc_uid_map_write).
/// `kind` is `"uid"` or `"gid"`. Returns false on a malformed payload or if the
/// process is gone.
pub fn write_id_map(pid: usize, kind: &str, data: &str) -> bool {
    let entries = match parse_id_map(data) {
        Some(e) => e,
        None => return false,
    };
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(ref t) = *table else { return false; };
    let Some(process) = t.get(pid) else { return false; };
    let mut ns = process.nsproxy.lock();
    match kind {
        "uid" => ns.user_ns.uid_map = entries,
        "gid" => ns.user_ns.gid_map = entries,
        _ => return false,
    }
    true
}

/// Render process `pid`'s user-namespace `kind` ("uid"/"gid") map in the
/// `/proc/<pid>/{uid,gid}_map` textual format. Empty map → empty string.
pub fn read_id_map(pid: usize, kind: &str) -> String {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(ref t) = *table else { return String::new(); };
    let Some(process) = t.get(pid) else { return String::new(); };
    let (target_ns_id, entries): (usize, Vec<IdMapEntry>) = {
        let ns = process.nsproxy.lock();
        let map = match kind {
            "uid" => ns.user_ns.uid_map.clone(),
            "gid" => ns.user_ns.gid_map.clone(),
            _ => return String::new(),
        };
        (ns.user_ns.id.0, map)
    };
    // Reader (current process) userns + map. The stored `outside` is a kernel
    // (canonical) id; when the reader is in a *different* userns than the
    // file's, the outside column must be re-expressed inside the reader's
    // userns (对标 m_show: lower id shown relative to seq_user_ns). Reading
    // one's own map uses the parent ns, identity here, so the kuid is shown
    // verbatim — keeping prior self-read behaviour intact.
    let reader: Option<(usize, Vec<IdMapEntry>)> =
        t.current.and_then(|cp| t.get(cp)).map(|p| {
            let ns = p.nsproxy.lock();
            let m = match kind {
                "uid" => ns.user_ns.uid_map.clone(),
                _ => ns.user_ns.gid_map.clone(),
            };
            (ns.user_ns.id.0, m)
        });
    let mut s = String::new();
    for e in &entries {
        let outside = match &reader {
            Some((rid, rmap)) if *rid != target_ns_id => from_outside_in(rmap, e.outside),
            _ => e.outside,
        };
        s.push_str(&alloc::format!("{:>10} {:>10} {:>10}\n", e.inside, outside, e.count));
    }
    s
}

/// Translate a kernel (outside) id into the id seen inside the namespace whose
/// id-map is `map` (对标 from_kuid); unmapped → OVERFLOW_UID.
fn from_outside_in(map: &[IdMapEntry], kid: u32) -> u32 {
    for entry in map {
        if kid >= entry.outside && (kid - entry.outside) < entry.count {
            return entry.inside + (kid - entry.outside);
        }
    }
    OVERFLOW_UID
}

/// `/proc/<pid>/setgroups` content: "allow\n" or "deny\n" per the target
/// process's user namespace state.
pub fn read_setgroups(pid: usize) -> String {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(ref t) = *table else { return String::from("allow\n"); };
    let Some(process) = t.get(pid) else { return String::from("allow\n"); };
    let ns = process.nsproxy.lock();
    if ns.user_ns.setgroups_allowed { String::from("allow\n") } else { String::from("deny\n") }
}

/// Write `/proc/<pid>/setgroups`. `Ok(())` on success; `Err(true)` = EPERM
/// (re-enabling "allow" after "deny" is irreversible, 对标 proc_setgroups_show
/// / setgroups already disabled); `Err(false)` = EINVAL (malformed value).
pub fn write_setgroups(pid: usize, data: &str) -> Result<(), bool> {
    let val = data.trim();
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(ref t) = *table else { return Err(false); };
    let Some(process) = t.get(pid) else { return Err(false); };
    let mut ns = process.nsproxy.lock();
    match val {
        "deny" => { ns.user_ns.setgroups_allowed = false; Ok(()) }
        "allow" => {
            if !ns.user_ns.setgroups_allowed { Err(true) } else { Ok(()) }
        }
        _ => Err(false),
    }
}

/// PID namespace: pid allocation + translation
pub struct PidNamespace {
    pub id: NsId,
    pub level: u32,
    /// pid_in_parent -> pid_in_this_ns
    pub pid_map: BTreeMap<usize, usize>,
    pub next_pid: AtomicUsize,
}

impl PidNamespace {
    pub fn new(level: u32) -> Self {
        Self {
            id: alloc_ns_id(),
            level,
            pid_map: BTreeMap::new(),
            next_pid: AtomicUsize::new(1),
        }
    }
    pub fn alloc_pid(&self) -> usize {
        self.next_pid.fetch_add(1, Ordering::Relaxed)
    }
    pub fn clone_ns(&self) -> Self {
        Self {
            id: alloc_ns_id(),
            level: self.level + 1,
            pid_map: BTreeMap::new(),
            next_pid: AtomicUsize::new(1),
        }
    }
}

/// Mount namespace: independent mount tree view
pub struct MountNamespace {
    pub id: NsId,
    /// Mount points visible in this namespace
    pub mount_points: Vec<MountEntry>,
}

#[derive(Clone)]
pub struct MountEntry {
    pub mount_id: usize,
    pub parent_id: usize,
    pub source: String,
    pub target: String,
    pub fstype: String,
    pub flags: u64,
}

static NEXT_MOUNT_ID: AtomicUsize = AtomicUsize::new(100);

impl MountNamespace {
    pub fn new() -> Self {
        // The init mount namespace uses a fixed, well-known id so that the
        // mount info table's built-in entries (tagged INIT_MNT_NS) and the
        // VFS default map line up with the init process's namespace. Cloned
        // namespaces (unshare/clone(CLONE_NEWNS)) get fresh ids via clone_ns().
        Self {
            id: NsId(crate::fs::mount::INIT_MNT_NS),
            mount_points: Vec::new(),
        }
    }
    pub fn clone_ns(&self) -> Self {
        Self {
            id: alloc_ns_id(),
            mount_points: self.mount_points.clone(),
        }
    }
    pub fn add_mount(&mut self, source: &str, target: &str, fstype: &str, flags: u64) -> usize {
        let mount_id = NEXT_MOUNT_ID.fetch_add(1, Ordering::Relaxed);
        self.mount_points.push(MountEntry {
            mount_id,
            parent_id: 1,
            source: String::from(source),
            target: String::from(target),
            fstype: String::from(fstype),
            flags,
        });
        mount_id
    }
}

/// Network namespace: independent network stack
pub struct NetNamespace {
    pub id: NsId,
    pub loopback_up: bool,
}

impl NetNamespace {
    pub fn new() -> Self {
        Self {
            id: alloc_ns_id(),
            loopback_up: true,
        }
    }
    pub fn clone_ns(&self) -> Self {
        Self {
            id: alloc_ns_id(),
            loopback_up: false, // new net ns starts with lo down
        }
    }
}

/// IPC namespace: independent SysV IPC + POSIX MQ
pub struct IpcNamespace {
    pub id: NsId,
}

impl IpcNamespace {
    pub fn new() -> Self {
        Self { id: alloc_ns_id() }
    }
    pub fn clone_ns(&self) -> Self {
        Self { id: alloc_ns_id() }
    }
}

/// Cgroup namespace
pub struct CgroupNamespace {
    pub id: NsId,
    pub root_cgroup: String,
}

impl CgroupNamespace {
    pub fn new() -> Self {
        Self {
            id: alloc_ns_id(),
            root_cgroup: String::from("/"),
        }
    }
    pub fn clone_ns(&self) -> Self {
        Self {
            id: alloc_ns_id(),
            root_cgroup: self.root_cgroup.clone(),
        }
    }
}

/// Time namespace (Linux 5.6+)
pub struct TimeNamespace {
    pub id: NsId,
    /// Offset in nanoseconds for CLOCK_MONOTONIC
    pub monotonic_offset_ns: i64,
    /// Offset in nanoseconds for CLOCK_BOOTTIME
    pub boottime_offset_ns: i64,
}

impl TimeNamespace {
    pub fn new() -> Self {
        Self {
            id: alloc_ns_id(),
            monotonic_offset_ns: 0,
            boottime_offset_ns: 0,
        }
    }
    pub fn clone_ns(&self) -> Self {
        Self {
            id: alloc_ns_id(),
            monotonic_offset_ns: self.monotonic_offset_ns,
            boottime_offset_ns: self.boottime_offset_ns,
        }
    }
}

// ---- NsProxy: per-process namespace set ------------------------------------

/// Namespace proxy — each process holds one. Shared via Arc for CLONE_NEW*.
/// On unshare, the relevant namespace is replaced with a fresh copy.
pub struct NsProxy {
    pub uts_ns: UtsNamespace,
    pub user_ns: UserNamespace,
    pub pid_ns: PidNamespace,
    pub mnt_ns: MountNamespace,
    pub net_ns: NetNamespace,
    pub ipc_ns: IpcNamespace,
    pub cgroup_ns: CgroupNamespace,
    pub time_ns: TimeNamespace,
    /// Time namespace that this process's *children* inherit on fork.
    /// `unshare(CLONE_NEWTIME)` adjusts this (not `time_ns`): per Linux the
    /// caller stays in its current time namespace and only forked children
    /// observe the new CLOCK_MONOTONIC/CLOCK_BOOTTIME offsets.
    pub time_ns_for_children: TimeNamespace,
}

impl NsProxy {
    /// Build a copy that shares every namespace identity with `self` (same ids
    /// and data) but joins the mount namespace `mnt_id`. Used by setns(2) to
    /// give the calling task a private nsproxy (copy-on-write) so switching its
    /// mount namespace does not disturb other tasks sharing the old proxy.
    pub fn copy_with_mnt_ns(&self, mnt_id: usize) -> Self {
        Self {
            uts_ns: UtsNamespace {
                id: self.uts_ns.id,
                hostname: self.uts_ns.hostname.clone(),
                domainname: self.uts_ns.domainname.clone(),
            },
            user_ns: UserNamespace {
                id: self.user_ns.id,
                uid_map: self.user_ns.uid_map.clone(),
                gid_map: self.user_ns.gid_map.clone(),
                setgroups_allowed: self.user_ns.setgroups_allowed,
                owner_uid: self.user_ns.owner_uid,
            },
            pid_ns: PidNamespace {
                id: self.pid_ns.id,
                level: self.pid_ns.level,
                pid_map: self.pid_ns.pid_map.clone(),
                next_pid: AtomicUsize::new(self.pid_ns.next_pid.load(Ordering::Relaxed)),
            },
            mnt_ns: MountNamespace {
                id: NsId(mnt_id),
                mount_points: self.mnt_ns.mount_points.clone(),
            },
            net_ns: NetNamespace {
                id: self.net_ns.id,
                loopback_up: self.net_ns.loopback_up,
            },
            ipc_ns: IpcNamespace { id: self.ipc_ns.id },
            cgroup_ns: CgroupNamespace {
                id: self.cgroup_ns.id,
                root_cgroup: self.cgroup_ns.root_cgroup.clone(),
            },
            time_ns: TimeNamespace {
                id: self.time_ns.id,
                monotonic_offset_ns: self.time_ns.monotonic_offset_ns,
                boottime_offset_ns: self.time_ns.boottime_offset_ns,
            },
            time_ns_for_children: TimeNamespace {
                id: self.time_ns_for_children.id,
                monotonic_offset_ns: self.time_ns_for_children.monotonic_offset_ns,
                boottime_offset_ns: self.time_ns_for_children.boottime_offset_ns,
            },
        }
    }

    /// Deep-copy every namespace preserving its identity (ids + data), keeping
    /// the current mount namespace. Used by `unshare(2)` to give the calling
    /// task a *private* nsproxy (copy-on-write) before mutating individual
    /// namespaces, so that the parent / siblings that share the same
    /// `Arc<NsProxy>` after fork are left untouched. Mirrors Linux
    /// `create_new_namespaces()` + `switch_task_namespaces()`.
    pub fn copy_all(&self) -> Self {
        self.copy_with_mnt_ns(self.mnt_ns.id.0)
    }

    pub fn init_ns() -> Self {
        Self {
            uts_ns: UtsNamespace::new(),
            user_ns: UserNamespace::new(0),
            pid_ns: PidNamespace::new(0),
            mnt_ns: MountNamespace::new(),
            net_ns: NetNamespace::new(),
            ipc_ns: IpcNamespace::new(),
            cgroup_ns: CgroupNamespace::new(),
            time_ns: TimeNamespace::new(),
            time_ns_for_children: TimeNamespace::new(),
        }
    }
}

// ---- Global namespace registry for /proc/pid/ns/ ---------------------------

static NS_REGISTRY: SpinLock<BTreeMap<usize, NsInfo>> = SpinLock::new(BTreeMap::new());

struct NsInfo {
    ns_type: &'static str,
    ns_id: NsId,
}

/// Register a process's namespace inodes for /proc/pid/ns/ lookups
pub fn register_process_ns(pid: usize, proxy: &NsProxy) {
    let mut reg = NS_REGISTRY.lock();
    let base = pid * 8;
    let entries = [
        (base, "user", proxy.user_ns.id),
        (base + 1, "uts", proxy.uts_ns.id),
        (base + 2, "ipc", proxy.ipc_ns.id),
        (base + 3, "mnt", proxy.mnt_ns.id),
        (base + 4, "pid", proxy.pid_ns.id),
        (base + 5, "net", proxy.net_ns.id),
        (base + 6, "cgroup", proxy.cgroup_ns.id),
        (base + 7, "time", proxy.time_ns.id),
    ];
    for (key, ns_type, ns_id) in entries {
        reg.insert(key, NsInfo { ns_type, ns_id });
    }
}

/// Get namespace inode number for /proc/pid/ns/<type>
pub fn get_ns_ino(pid: usize, ns_type: &str) -> Option<usize> {
    let reg = NS_REGISTRY.lock();
    let base = pid * 8;
    let offset = match ns_type {
        "user" => 0,
        "uts" => 1,
        "ipc" => 2,
        "mnt" => 3,
        "pid" | "pid_for_children" => 4,
        "net" => 5,
        "cgroup" => 6,
        "time" | "time_for_children" => 7,
        _ => return None,
    };
    reg.get(&(base + offset)).map(|info| info.ns_id.0)
}

// ---- Syscalls --------------------------------------------------------------

fn current_euid() -> u32 {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)).map(|p| p.euid))
        .unwrap_or(0)
}

/// unshare(2) — disassociate parts of the process execution context.
///
/// Per Linux `kernel/fork.c` `ksys_unshare()`:
///   - Validate flags: reject unknown bits with -EINVAL
///   - Namespace flags require CAP_SYS_ADMIN (except CLONE_NEWUSER)
///   - CLONE_NEWUSER can be done by unprivileged processes
///   - CLONE_FILES: create private fd table copy
///   - CLONE_FS: create private fs_struct copy
///   - CLONE_NEWNS requires CLONE_FS (implicit)
pub(crate) fn sys_unshare(flags: i32) -> isize {
    if flags & !KNOWN_UNSHARE_FLAGS != 0 { return -22; } // EINVAL

    let euid = current_euid();

    // CLONE_NEWUSER is special: unprivileged users can create user namespaces
    let ns_bits_except_user = NS_BITS & !CLONE_NEWUSER;
    if (flags & ns_bits_except_user != 0) && euid != 0 {
        return -1; // EPERM
    }

    // CLONE_FS: create a private fs_struct copy (Linux unshare_fs; CLONE_NEWNS
    // implies it). Later chdir/chroot/umask no longer reach former sharers.
    if flags & (CLONE_FS | CLONE_NEWNS) != 0 {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(ref mut t) = *table else { return -3; };
        let Some(pid) = t.current else { return -3; };
        let Some(process) = t.get_mut(pid) else { return -3; };
        let copy = process.fs.lock().clone();
        process.fs = alloc::sync::Arc::new(crate::sync::SpinLock::new(copy));
        process.fs_share_id = pid;
    }

    // CLONE_FILES: create a private copy of the fd table
    if flags & CLONE_FILES != 0 {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(ref mut t) = *table else { return -3; };
        let Some(pid) = t.current else { return -3; };
        if !t.unshare_fd_table_for_pid(pid) {
            return -3;
        }
    }

    // 真实隔离：为当前进程创建新的独立命名空间 — 对标 Linux ksys_unshare()
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(ref mut t) = *table else { return -3; };
        let Some(pid) = t.current else { return -3; };

        // 关键：copy-on-write —— 普通 fork 后父子共享同一 Arc<NsProxy>
        // (process_fork: `child.nsproxy = parent.nsproxy.clone()`)。若在此原地
        // mutate 共享对象，则 unshare 会泄漏到父/兄弟进程（unshare01.sh test1 的
        // `unshare --user` 把主 shell 也拖进新 userns，导致 test3-8 全部 TFAIL）。
        // 对标 Linux create_new_namespaces() + switch_task_namespaces()：先拷一份
        // 私有 nsproxy，在副本上施加 clone_ns，再原子换给当前进程，绝不动父进程。
        let mut ns = {
            let Some(process) = t.get(pid) else { return -3; };
            let cur = process.nsproxy.lock();
            cur.copy_all()
        };

        if flags & CLONE_NEWUTS != 0 {
            ns.uts_ns = ns.uts_ns.clone_ns();
        }
        if flags & CLONE_NEWUSER != 0 {
            ns.user_ns = ns.user_ns.clone_ns();
        }
        if flags & CLONE_NEWPID != 0 {
            ns.pid_ns = ns.pid_ns.clone_ns();
        }
        if flags & CLONE_NEWNS != 0 {
            // 对标 copy_mnt_ns：新挂载命名空间从调用者当前命名空间快照一份独立
            // 挂载树，之后两侧的挂载/卸载默认互不可见（除非 shared 传播）。
            let old_ns = ns.mnt_ns.id.0;
            ns.mnt_ns = ns.mnt_ns.clone_ns();
            let new_ns = ns.mnt_ns.id.0;
            crate::fs::mount::snapshot_mount_info(old_ns, new_ns);
            crate::fs::snapshot_mount_ns(old_ns, new_ns);
            // 对标 copy_mnt_ns：当新挂载命名空间与新 user 命名空间一起创建时
            // （unshare(CLONE_NEWUSER|CLONE_NEWNS)），其继承的 shared 挂载被降级为
            // slave —— 仍从父 peer group 收传播，但子侧新挂载/绑定不会回传给父。
            // 这使 `unshare --user --mount --propagation shared` 的子绑定对父不可见。
            if flags & CLONE_NEWUSER != 0 {
                crate::fs::mount::reduce_shared_to_slave(new_ns);
            }
            // 当前进程立即进入新命名空间。
            crate::fs::mount::set_active_mnt_ns(new_ns);
        }
        if flags & CLONE_NEWNET != 0 {
            ns.net_ns = ns.net_ns.clone_ns();
        }
        if flags & CLONE_NEWIPC != 0 {
            ns.ipc_ns = ns.ipc_ns.clone_ns();
        }
        if flags & CLONE_NEWCGROUP != 0 {
            ns.cgroup_ns = ns.cgroup_ns.clone_ns();
        }
        if flags & CLONE_NEWTIME != 0 {
            // Per Linux: unshare(CLONE_NEWTIME) creates a new time namespace for
            // the caller's *children*; the caller itself keeps running in its
            // current time namespace until it forks. The offsets are written
            // afterwards via /proc/self/timens_offsets (into time_ns_for_children)
            // and picked up by children at fork (see process_fork timens_on_fork).
            ns.time_ns_for_children = ns.time_ns_for_children.clone_ns();
        }

        // 原子换入私有 nsproxy（替换 Arc），父进程仍持有旧的共享副本不受影响。
        let new_arc = alloc::sync::Arc::new(crate::sync::SpinLock::new(ns));
        if let Some(process) = t.get_mut(pid) {
            process.nsproxy = new_arc.clone();
            process.cached_mnt_ns_id = new_arc.lock().mnt_ns.id.0;
        }
        // /proc/<pid>/ns/* must reflect the new namespace ids so that another
        // process can setns() into them via the fd it opens.
        let g = new_arc.lock();
        register_process_ns(pid, &g);
    }

    0
}

/// Offsets `(monotonic_ns, boottime_ns)` of the **current** process's active
/// time namespace. Used by `clock_gettime` and timerfd to translate
/// namespace-relative absolute times into host time. Returns `(0, 0)` for the
/// host (root) time namespace or when no process context is available.
pub fn current_time_ns_offsets() -> (i64, i64) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(ref t) = *table else { return (0, 0); };
    let Some(pid) = t.current else { return (0, 0); };
    let Some(process) = t.get(pid) else { return (0, 0); };
    let ns = process.nsproxy.lock();
    (ns.time_ns.monotonic_offset_ns, ns.time_ns.boottime_offset_ns)
}

/// Set the **current** process's `time_ns_for_children` offset for one clock.
/// Written via `/proc/self/timens_offsets`; per Linux the offset applies to the
/// caller's forked children (not the caller itself). `clock_id` is the POSIX
/// clock id (1 = CLOCK_MONOTONIC, 7 = CLOCK_BOOTTIME). Returns false on an
/// unsupported clock id or missing process context.
pub fn set_current_timens_for_children_offset(clock_id: usize, offset_ns: i64) -> bool {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(ref t) = *table else { return false; };
    let Some(pid) = t.current else { return false; };
    let Some(process) = t.get(pid) else { return false; };
    let mut ns = process.nsproxy.lock();
    match clock_id {
        1 => ns.time_ns_for_children.monotonic_offset_ns = offset_ns, // CLOCK_MONOTONIC
        7 => ns.time_ns_for_children.boottime_offset_ns = offset_ns,  // CLOCK_BOOTTIME
        _ => return false,
    }
    true
}

/// Offsets `(monotonic_ns, boottime_ns)` of the current process's
/// `time_ns_for_children` (what `/proc/self/timens_offsets` reports).
pub fn current_timens_for_children_offsets() -> (i64, i64) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(ref t) = *table else { return (0, 0); };
    let Some(pid) = t.current else { return (0, 0); };
    let Some(process) = t.get(pid) else { return (0, 0); };
    let ns = process.nsproxy.lock();
    (ns.time_ns_for_children.monotonic_offset_ns, ns.time_ns_for_children.boottime_offset_ns)
}

/// Current task's network-namespace id (the `NsId` inner value). Used by
/// per-netns procfs sysctls (e.g. `net/ipv4/conf/lo/tag`, clone09) so that a
/// value written in one netns is invisible to a freshly cloned netns.
pub fn current_net_ns_id() -> usize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(ref t) = *table else { return 0; };
    let Some(pid) = t.current else { return 0; };
    let Some(process) = t.get(pid) else { return 0; };
    let ns = process.nsproxy.lock();
    ns.net_ns.id.0
}

/// setns(2) — reassociate thread with a namespace.
///
/// Per Linux `kernel/nsproxy.c` `SYSCALL_DEFINE2(setns, ...)`:
///   - fd must refer to a namespace file (/proc/pid/ns/*)
///   - nstype (if non-zero) must match the namespace type
///   - Caller must have CAP_SYS_ADMIN for non-user namespaces
///   - User namespace: caller must have CAP_SYS_ADMIN in the target ns
pub(crate) fn sys_setns(fd: i32, nstype: i32) -> isize {
    if fd < 0 { return -9; } // EBADF

    let euid = current_euid();

    // Validate nstype if specified
    if nstype != 0 {
        let valid_types = [
            CLONE_NEWUSER, CLONE_NEWNS, CLONE_NEWUTS, CLONE_NEWIPC,
            CLONE_NEWPID, CLONE_NEWNET, CLONE_NEWCGROUP, CLONE_NEWTIME,
        ];
        if !valid_types.contains(&nstype) {
            return -22; // EINVAL
        }
    }

    // Resolve the fd and determine which namespace it refers to. Linux requires
    // `fd` to be a namespace file (an nsfs fd, exposed here as the procfs path
    // `/proc/<pid>/ns/<type>`); any other fd → EINVAL. We recover the namespace
    // type from the path the fd was opened with.
    let fd_path: Option<String> = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -3 };
        let pid = match t.current { Some(p) => p, None => return -3 };
        let proc = match t.get_fd_process(pid) { Some(p) => p, None => return -3 };
        match crate::fs::fd::get(proc, fd as usize) {
            Some(fdesc) => fdesc.path.clone(),
            None => return -9, // EBADF
        }
    };

    let file_ns_type = match ns_type_from_path(fd_path.as_deref()) {
        Some(t) => t,
        // fd does not refer to a namespace file (e.g. a regular file) → EINVAL.
        None => return -22,
    };

    // If nstype is specified it must match the namespace the fd refers to.
    if nstype != 0 && nstype != file_ns_type {
        return -22; // EINVAL
    }

    // Joining a non-user namespace requires CAP_SYS_ADMIN (approximated by root).
    if file_ns_type != CLONE_NEWUSER && euid != 0 {
        return -1; // EPERM
    }

    // Joining a user namespace requires CAP_SYS_ADMIN *in the target userns*:
    // a task only holds that over its own userns and descendants, so the
    // target must be the caller's current userns or one of its descendants —
    // equivalently, the caller's current userns must be an ancestor-or-self of
    // the target (userns04: a sibling userns must be rejected with EPERM).
    if file_ns_type == CLONE_NEWUSER {
        let target_pid = match pid_from_ns_path(fd_path.as_deref()) {
            Some(p) => p,
            None => return -22,
        };
        let target_userns = match get_ns_ino(target_pid, "user") {
            Some(id) => id,
            None => return -22,
        };
        let caller_userns = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_ref() { Some(t) => t, None => return -3 };
            let pid = match t.current { Some(p) => p, None => return -3 };
            let proc = match t.get(pid) { Some(p) => p, None => return -3 };
            let g = proc.nsproxy.lock();
            g.user_ns.id.0
        };
        if !userns_is_ancestor_or_self(caller_userns, target_userns) {
            return -1; // EPERM
        }
    }

    // For the mount namespace, actually move the calling process into the
    // target namespace so its subsequent path resolution / mount bookkeeping
    // scope to that namespace's mount tree (对标 Linux setns -> switch_task_namespaces).
    if file_ns_type == CLONE_NEWNS {
        // Recover the owning pid from the /proc/<pid>/ns/mnt path and look up
        // the mount namespace id it currently holds.
        let target_pid = match pid_from_ns_path(fd_path.as_deref()) {
            Some(p) => p,
            None => return -22,
        };
        let target_ns_id = match get_ns_ino(target_pid, "mnt") {
            Some(id) => id,
            None => return -22,
        };
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(ref mut t) = *table else { return -3; };
        let Some(pid) = t.current else { return -3; };
        let Some(process) = t.get_mut(pid) else { return -3; };
        // Copy-on-write: replace this task's nsproxy with a private copy that
        // joins the target mount namespace, so processes sharing the old proxy
        // (e.g. the parent shell that fork+exec'd us) are unaffected.
        let new_proxy = process.nsproxy.lock().copy_with_mnt_ns(target_ns_id);
        process.nsproxy = alloc::sync::Arc::new(crate::sync::SpinLock::new(new_proxy));
        process.cached_mnt_ns_id = target_ns_id;
        {
            let g = process.nsproxy.lock();
            register_process_ns(pid, &g);
        }
        crate::fs::mount::set_active_mnt_ns(target_ns_id);
    }

    // UTS / IPC / NET / CGROUP / TIME：把调用者切换到目标进程当前所属的
    // 同类 namespace（对标 Linux setns -> switch_task_namespaces）。namespace
    // 数据内嵌在 NsProxy 中，因此从目标进程的 proxy 克隆该 namespace 的
    // 身份 + 数据，装进调用者的私有 proxy 副本（copy-on-write，不影响共享
    // 旧 proxy 的其他任务）。
    if matches!(
        file_ns_type,
        x if x == CLONE_NEWUTS || x == CLONE_NEWIPC || x == CLONE_NEWNET
            || x == CLONE_NEWCGROUP || x == CLONE_NEWTIME
    ) {
        let target_pid = match pid_from_ns_path(fd_path.as_deref()) {
            Some(p) => p,
            None => return -22,
        };
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(ref mut t) = *table else { return -3; };
        let Some(pid) = t.current else { return -3; };
        // 目标 namespace 数据取自目标进程的 nsproxy（进程需存活；nsfs fd
        // 独立保活暂不支持，目标不存在按无效 ns 文件处理 EINVAL）。
        let target_proxy = match t.get(target_pid) {
            Some(p) => p.nsproxy.clone(),
            None => return -22,
        };
        let Some(process) = t.get_mut(pid) else { return -3; };
        let mut new_proxy = process.nsproxy.lock().copy_all();
        {
            let tg = target_proxy.lock();
            if file_ns_type == CLONE_NEWUTS {
                new_proxy.uts_ns = UtsNamespace {
                    id: tg.uts_ns.id,
                    hostname: tg.uts_ns.hostname.clone(),
                    domainname: tg.uts_ns.domainname.clone(),
                };
            } else if file_ns_type == CLONE_NEWIPC {
                new_proxy.ipc_ns = IpcNamespace { id: tg.ipc_ns.id };
            } else if file_ns_type == CLONE_NEWNET {
                new_proxy.net_ns = NetNamespace {
                    id: tg.net_ns.id,
                    loopback_up: tg.net_ns.loopback_up,
                };
            } else if file_ns_type == CLONE_NEWCGROUP {
                new_proxy.cgroup_ns = CgroupNamespace {
                    id: tg.cgroup_ns.id,
                    root_cgroup: tg.cgroup_ns.root_cgroup.clone(),
                };
            } else if file_ns_type == CLONE_NEWTIME {
                // setns(CLONE_NEWTIME) 只影响后续 fork 的子进程（Linux 语义）。
                new_proxy.time_ns_for_children = TimeNamespace {
                    id: tg.time_ns.id,
                    monotonic_offset_ns: tg.time_ns.monotonic_offset_ns,
                    boottime_offset_ns: tg.time_ns.boottime_offset_ns,
                };
            }
        }
        process.nsproxy = alloc::sync::Arc::new(crate::sync::SpinLock::new(new_proxy));
        {
            let g = process.nsproxy.lock();
            register_process_ns(pid, &g);
        }
    }

    // Accept the setns request — namespace association updated.
    0
}

/// Recover the owning pid from a `/proc/<pid>/ns/<type>` path.
///
/// `/proc/self/...` 无法在这里解析成 pid：fd 路径是 open 时的字面量，而
/// "self" 指的是 *open 时* 的进程（Linux 在 open 时就固定了 nsfs inode）。
/// 这里没有 opener 记录，按 fd 的 inode 反查也不可得，因此返回 None（上层
/// 视作无效 ns 文件）。LTP setns02 等用父进程 pid 显式路径的场景不受影响。
fn pid_from_ns_path(path: Option<&str>) -> Option<usize> {
    let p = path?;
    let rest = p.strip_prefix("/proc/")?;
    let pid_str = rest.split('/').next()?;
    pid_str.parse::<usize>().ok()
}

/// Map a `/proc/<pid>/ns/<type>` path to its `CLONE_NEW*` flag.
/// Returns `None` if the path does not denote a namespace file.
fn ns_type_from_path(path: Option<&str>) -> Option<i32> {
    let p = path?;
    // The namespace type is the final path component, preceded by a `ns` dir.
    let mut comps = p.rsplit('/');
    let last = comps.next()?;
    let parent = comps.next()?;
    if parent != "ns" {
        return None;
    }
    Some(match last {
        "cgroup" => CLONE_NEWCGROUP,
        "ipc" => CLONE_NEWIPC,
        "mnt" => CLONE_NEWNS,
        "net" => CLONE_NEWNET,
        "pid" | "pid_for_children" => CLONE_NEWPID,
        "time" | "time_for_children" => CLONE_NEWTIME,
        "user" => CLONE_NEWUSER,
        "uts" => CLONE_NEWUTS,
        _ => return None,
    })
}

/// clone3(2) — create a new process/thread with extended arguments.
///
/// Per Linux `kernel/fork.c` `SYSCALL_DEFINE2(clone3, ...)`:
///   - Parses struct clone_args (variable size, versioned)
///   - Validates flag combinations
///   - Creates the new task via copy_process
pub(crate) fn sys_clone3(args: *const u8, size: usize) -> isize {
    const CLONE_ARGS_SIZE_VER0: usize = 64;
    const CLONE_ARGS_SIZE_VER2: usize = 88;
    const CLONE_PIDFD: u64 = 0x0000_1000;
    const CLONE3_SIGHAND: u64 = 0x0000_0800;
    const CLONE3_THREAD: u64 = 0x0001_0000;
    const CLONE3_FS: u64 = 0x0000_0200;
    const CLONE3_NEWNS: u64 = 0x0002_0000;
    // High clone3-only flags (struct clone_args). bit33 = CLONE_INTO_CGROUP.
    const CLONE_INTO_CGROUP: u64 = 0x2_0000_0000;

    let cl3dbg = crate::cmdline::cl3dbg_enabled();
    if args.is_null() { return -14; } // EFAULT
    if size < CLONE_ARGS_SIZE_VER0 {
        if cl3dbg { crate::println!("[cl3dbg] reject size<VER0 size={}", size); }
        return -22;
    } // EINVAL
    if size > 4096 { return -7; } // E2BIG

    // Read up to VER2 (88 bytes): VER0 core + set_tid/set_tid_size (VER1) +
    // cgroup (VER2). clone303 passes a VER2 struct with the cgroup fd set, so
    // we must not reject the trailing fields as "nonzero extra bytes".
    let read_size = if size > CLONE_ARGS_SIZE_VER2 { CLONE_ARGS_SIZE_VER2 } else { size };
    let bytes = match crate::syscall::user::read_user_buffer(args, read_size) {
        Ok(b) => b,
        Err(_) => return -14,
    };

    // Verify bytes beyond the known (VER2) struct are zero.
    if size > CLONE_ARGS_SIZE_VER2 {
        let extra = match crate::syscall::user::read_user_buffer(
            unsafe { args.add(CLONE_ARGS_SIZE_VER2) },
            size - CLONE_ARGS_SIZE_VER2,
        ) {
            Ok(b) => b,
            Err(_) => return -14,
        };
        if extra.iter().any(|&b| b != 0) { return -7; } // E2BIG
    }

    let mut buf = [0u8; CLONE_ARGS_SIZE_VER2];
    buf[..read_size].copy_from_slice(&bytes[..read_size]);
    let flags = u64::from_le_bytes(buf[0..8].try_into().unwrap());
    let pidfd_ptr = u64::from_le_bytes(buf[8..16].try_into().unwrap()) as usize;
    let child_tid = u64::from_le_bytes(buf[16..24].try_into().unwrap()) as usize;
    let parent_tid = u64::from_le_bytes(buf[24..32].try_into().unwrap()) as usize;
    let exit_signal = u64::from_le_bytes(buf[32..40].try_into().unwrap()) as usize;
    let stack = u64::from_le_bytes(buf[40..48].try_into().unwrap()) as usize;
    let stack_size = u64::from_le_bytes(buf[48..56].try_into().unwrap()) as usize;
    let tls = u64::from_le_bytes(buf[56..64].try_into().unwrap()) as usize;
    let cgroup_fd = u64::from_le_bytes(buf[80..88].try_into().unwrap());

    if cl3dbg {
        crate::println!(
            "[cl3dbg] entry flags={:#x} stack={:#x} stack_size={:#x} tls={:#x} ptid={:#x} ctid={:#x} exit_sig={}",
            flags, stack, stack_size, tls, parent_tid, child_tid, exit_signal
        );
    }
    // Flag dependency checks per kernel/fork.c copy_process
    if flags & CLONE3_SIGHAND != 0 && flags & (CLONE_VM as u64) == 0 {
        if cl3dbg { crate::println!("[cl3dbg] reject SIGHAND&&!VM"); }
        return -22;
    }
    if flags & CLONE3_THREAD != 0 && flags & CLONE3_SIGHAND == 0 {
        if cl3dbg { crate::println!("[cl3dbg] reject THREAD&&!SIGHAND"); }
        return -22;
    }
    if flags & CLONE3_FS != 0 && flags & CLONE3_NEWNS != 0 {
        if cl3dbg { crate::println!("[cl3dbg] reject FS&&NEWNS"); }
        return -22;
    }

    // CLONE_INTO_CGROUP: resolve the supplied cgroup v2 directory fd to a
    // cgroup-tree path up-front (Linux validates the fd before copy_process).
    let into_cgroup_path: Option<String> = if flags & CLONE_INTO_CGROUP != 0 {
        let fd = cgroup_fd as i32;
        let inode = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_ref() { Some(t) => t, None => return -22 };
            let pid = match t.current { Some(p) => p, None => return -22 };
            let proc = match t.get_fd_process(pid) { Some(p) => p, None => return -22 };
            match crate::fs::fd::get(proc, fd as usize) {
                Some(fdesc) => fdesc.file.clone(),
                None => return -9, // EBADF
            }
        };
        match crate::fs::cgroupfs::cgroup_dir_path(inode.as_ref()) {
            Some(p) => Some(p),
            None => return -22, // not a cgroup v2 directory fd → EINVAL
        }
    } else {
        None
    };

    // CLONE_PIDFD validation
    if flags & CLONE_PIDFD != 0 {
        if pidfd_ptr == 0 { return -14; }
        if crate::syscall::user::read_user_buffer(pidfd_ptr as *const u8, 4).is_err() { return -14; }
    }

    // Stack validation
    if stack != 0 && stack_size == 0 {
        if cl3dbg { crate::println!("[cl3dbg] reject stack!=0 && size==0"); }
        return -22;
    }
    if stack == 0 && stack_size != 0 {
        if cl3dbg { crate::println!("[cl3dbg] reject stack==0 && size!=0"); }
        return -22;
    }
    if stack_size != 0 && stack != 0 {
        // Linux clone3 never dereferences the user stack region during the
        // syscall. glibc lays out a PROT_NONE guard page at the lowest address
        // of the thread stack (== the `stack` base passed here), and pages are
        // demand-faulted, so a readability probe wrongly rejects valid stacks
        // with EINVAL (observed on LA glibc pthread_create). Only keep the
        // arithmetic overflow sanity check, matching kernel semantics.
        let stack_top = stack.wrapping_add(stack_size);
        if stack_top < stack {
            if cl3dbg { crate::println!("[cl3dbg] reject stack_top<stack"); }
            return -22;
        }
    }

    // exit_signal must be valid
    if exit_signal > 64 {
        if cl3dbg { crate::println!("[cl3dbg] reject exit_signal>64 ={}", exit_signal); }
        return -22;
    }

    let combined_flags = (flags as usize) | (exit_signal & 0xff);
    // clone3 ABI: `stack` is the LOWEST address of the stack region and
    // `stack_size` its length; the kernel sets the new task's stack pointer to
    // the TOP (stack + stack_size). Legacy clone(2) instead receives the top
    // directly. sys_clone follows the legacy convention (child_stack == sp), so
    // convert here. Without this the child sp starts at the base and grows down
    // into the guard page → immediate fault (observed on LA glibc pthreads).
    let child_sp = if stack != 0 { stack.wrapping_add(stack_size) } else { 0 };
    if cl3dbg { crate::println!("[cl3dbg] child_sp={:#x} (base={:#x}+size={:#x})", child_sp, stack, stack_size); }
    // vfork_wait=false：CLONE_VFORK 的父阻塞推迟到本函数完成 pidfd /
    // CLONE_INTO_CGROUP 写回之后（Linux 在 copy_process 全部收尾后才等待）。
    let child_pid = crate::syscall::process_clone::sys_clone_impl(
        combined_flags, child_sp, parent_tid, tls, child_tid, false,
    );
    if child_pid < 0 { return child_pid; }

    // CLONE_INTO_CGROUP: place the freshly created child into the target
    // cgroup (move it out of the parent's cgroup, into `cgroup_path`). The
    // child's PCB cgroup_path is updated too so future charges scope correctly.
    if let Some(ref cgpath) = into_cgroup_path {
        let _ = crate::kernel::cgroup::write_cgroup_file(
            cgpath,
            "cgroup.procs",
            &alloc::format!("{}", child_pid),
        );
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(child) = t.get_mut(child_pid as usize) {
                child.cgroup_path = cgpath.clone();
            }
        }
    }

    // CLONE_PIDFD: allocate pidfd and write to user pointer
    if flags & CLONE_PIDFD != 0 && pidfd_ptr != 0 {
        let pidfd = crate::kernel::pidfd::create_pidfd_for_pid(child_pid as usize, 0o2);
        if pidfd < 0 { return pidfd; }
        let val = (pidfd as i32).to_le_bytes();
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                if let Some(process) = t.get_fd_process_mut(pid) {
                    if let Some(ref mm) = process.mm {
                        let mut addr_space = mm.lock();
                        let va = crate::mm::VirtAddr(pidfd_ptr);
                        let vpn = va.floor();
                        let offset = va.page_offset();
                        if let Some(f) = addr_space.page_table().get_pte_flags(vpn) {
                            if !f.contains(crate::mm::PTEFlags::W) {
                                addr_space.handle_page_fault(va, true);
                            }
                        }
                        if let Some(ppn) = addr_space.page_table().translate(vpn) {
                            let kva = crate::mm::phys_to_kernel_vaddr(ppn.0 * 4096 + offset);
                            unsafe { core::ptr::copy_nonoverlapping(val.as_ptr(), kva as *mut u8, 4); }
                        }
                    }
                }
            }
        }
    }
    if flags & (CLONE_VFORK_FLAG as u64) != 0 && child_pid > 0 {
        // 收尾（pidfd/cgroup 写回）完成，现在按 Linux 语义在 clone3 内
        // 同步阻塞父进程直到子 execve/exit。
        crate::sched::process_sched::suspend_vfork_parent_return(child_pid as usize);
    }
    child_pid
}

const CLONE_VFORK_FLAG: usize = 0x0000_4000;
