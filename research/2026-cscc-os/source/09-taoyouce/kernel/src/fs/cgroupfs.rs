//! cgroupfs — the cgroup v2 unified-hierarchy filesystem (`mount -t cgroup2`).
//!
//! Per Linux `kernel/cgroup/cgroup.c` (kernfs-backed). Each directory is a
//! cgroup node; `mkdir`/`rmdir` create/remove child cgroups; every directory
//! exposes the core `cgroup.*` interface files plus the controller interface
//! files (`memory.*`, `cpu.*`, `pids.*`, `io.*`, `cpuset.*`) for whichever
//! controllers are enabled on that node. All file reads/writes and tree
//! mutations delegate to `crate::kernel::cgroup`, which owns the real
//! controller state (`CGROUP_TREE`). This is the same hierarchy the scheduler
//! and mm subsystem charge against, so a freshly mounted instance is not a
//! disconnected copy — it is a view of the one global v2 hierarchy.

use super::{DirEntry, Directory, File, FileMode, FileStat, FileSystem, FileType, FsError, Inode};
use crate::kernel::cgroup as cg;
use crate::sync::SpinLock;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

static CGROUPFS_NEXT_INO: AtomicUsize = AtomicUsize::new(0xC700_0001);

fn alloc_ino() -> usize {
    CGROUPFS_NEXT_INO.fetch_add(1, Ordering::Relaxed)
}

// =============================================================================
// Per-node ownership / mode overrides
//
// cgroupfs nodes are synthesised on demand from the global cgroup tree, so they
// carry no persistent inode metadata of their own. Linux's kernfs, however,
// keeps per-file owner/mode that `chown`/`chmod` mutate and that drive
// permission checks (e.g. cgroup delegation: an unprivileged user is given
// ownership of a sub-tree's `cgroup.procs`). We mirror that with a side table
// keyed by (cgroup-path, interface-file-name); the directory node itself uses
// the empty file name. Defaults match kernfs: root:root, dirs 0755, rw files
// 0644, ro files 0444.
// =============================================================================

#[derive(Clone, Copy)]
struct AttrOverride {
    uid: u32,
    gid: u32,
    /// Full mode (type bits included) when overridden by chmod, else `None`.
    mode: Option<u32>,
}

static CG_ATTRS: SpinLock<BTreeMap<String, AttrOverride>> = SpinLock::new(BTreeMap::new());

fn attr_key(cg_path: &str, name: &str) -> String {
    // \u{1} cannot appear in a cgroup path or interface-file name.
    alloc::format!("{}\u{1}{}", cg_path, name)
}

fn attr_lookup(cg_path: &str, name: &str) -> Option<AttrOverride> {
    CG_ATTRS.lock().get(&attr_key(cg_path, name)).copied()
}

fn attr_set_owner(cg_path: &str, name: &str, uid: Option<u32>, gid: Option<u32>) {
    let mut t = CG_ATTRS.lock();
    let e = t
        .entry(attr_key(cg_path, name))
        .or_insert(AttrOverride { uid: 0, gid: 0, mode: None });
    if let Some(u) = uid {
        e.uid = u;
    }
    if let Some(g) = gid {
        e.gid = g;
    }
}

fn attr_set_mode(cg_path: &str, name: &str, mode: u32, type_bits: u32) {
    let mut t = CG_ATTRS.lock();
    let e = t
        .entry(attr_key(cg_path, name))
        .or_insert(AttrOverride { uid: 0, gid: 0, mode: None });
    e.mode = Some((mode & 0o7777) | type_bits);
}

/// Drop every override belonging to a cgroup and its descendants (rmdir).
fn attr_clear_subtree(cg_path: &str) {
    let dir_prefix = alloc::format!("{}/", cg_path);
    CG_ATTRS.lock().retain(|k, _| {
        let path = match k.find('\u{1}') {
            Some(pos) => &k[..pos],
            None => k.as_str(),
        };
        !(path == cg_path || path.starts_with(&dir_prefix))
    });
}

/// Deepest cgroup that is an ancestor of (or equal to) both `a` and `b`.
/// Mirrors Linux `cgroup_procs_write_permission()`'s common-ancestor walk.
fn common_ancestor(a: &str, b: &str) -> String {
    let ca: Vec<&str> = a.split('/').filter(|s| !s.is_empty()).collect();
    let cb: Vec<&str> = b.split('/').filter(|s| !s.is_empty()).collect();
    let mut common: Vec<&str> = Vec::new();
    for (x, y) in ca.iter().zip(cb.iter()) {
        if x == y {
            common.push(*x);
        } else {
            break;
        }
    }
    if common.is_empty() {
        String::from("/")
    } else {
        alloc::format!("/{}", common.join("/"))
    }
}

/// Effective (euid, egid) of the running task.
fn current_euid_egid() -> (u32, u32) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)))
        .map(|p| (p.euid, p.egid))
        .unwrap_or((0, 0))
}

/// Unix permission check for write (`MAY_WRITE`) against an owner/mode triple.
/// The caller guarantees `euid != 0` (root bypass handled separately).
fn may_write(euid: u32, egid: u32, fuid: u32, fgid: u32, mode: u32) -> bool {
    let bits = if euid == fuid {
        (mode >> 6) & 0o7
    } else if egid == fgid {
        (mode >> 3) & 0o7
    } else {
        mode & 0o7
    };
    bits & 0o2 != 0
}

/// Join a cgroup tree path with a child name (mirrors `mkdir_cgroup`).
fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", parent, name)
    }
}

/// Core cgroup.* files present in every cgroup directory.
const CORE_FILES: &[&str] = &[
    "cgroup.controllers",
    "cgroup.subtree_control",
    "cgroup.procs",
    "cgroup.threads",
    "cgroup.type",
    "cgroup.events",
    "cgroup.freeze",
    "cgroup.stat",
    "cgroup.max.descendants",
    "cgroup.max.depth",
];

/// Controller interface files, gated by the node's enabled controllers.
fn controller_files(controllers: u32) -> Vec<&'static str> {
    let mut v = Vec::new();
    if controllers & cg::CTRL_MEMORY != 0 {
        v.extend_from_slice(&[
            "memory.max", "memory.current", "memory.high", "memory.low",
            "memory.min", "memory.stat", "memory.swap.max", "memory.swap.current",
        ]);
    }
    if controllers & cg::CTRL_CPU != 0 {
        v.extend_from_slice(&["cpu.stat", "cpu.max", "cpu.weight"]);
    }
    if controllers & cg::CTRL_PIDS != 0 {
        v.extend_from_slice(&["pids.max", "pids.current"]);
    }
    if controllers & cg::CTRL_IO != 0 {
        v.extend_from_slice(&["io.max", "io.stat"]);
    }
    if controllers & cg::CTRL_CPUSET != 0 {
        v.extend_from_slice(&[
            "cpuset.cpus", "cpuset.mems", "cpuset.cpus.effective",
            "cpuset.mems.effective", "cpuset.cpus.partition",
        ]);
    }
    v
}

/// Read-only interface files (everything else is writable, mode 0644).
fn is_readonly(name: &str) -> bool {
    matches!(
        name,
        "cgroup.controllers" | "cgroup.events" | "cgroup.stat" | "cgroup.type"
            | "cgroup.max.descendants" | "cgroup.max.depth"
            | "memory.current" | "memory.stat" | "memory.swap.current"
            | "cpu.stat" | "pids.current" | "io.stat"
            | "cpuset.cpus.effective" | "cpuset.mems.effective"
    )
}

/// Whether `name` is an interface file present in the node at `cg_path`.
fn file_present(cg_path: &str, name: &str) -> bool {
    if CORE_FILES.contains(&name) {
        return true;
    }
    match cg::cgroup_node_controllers(cg_path) {
        Some(ctrls) => controller_files(ctrls).contains(&name),
        None => false,
    }
}

// =============================================================================
// CgroupFs filesystem
// =============================================================================

pub struct CgroupFs;

impl CgroupFs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl FileSystem for CgroupFs {
    fn name(&self) -> &str {
        "cgroup2"
    }

    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(CgroupDir::new(String::from("/")))
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

// =============================================================================
// CgroupDir — a cgroup directory node
// =============================================================================

struct CgroupDir {
    ino: usize,
    cg_path: String,
}

impl CgroupDir {
    fn new(cg_path: String) -> Self {
        Self { ino: alloc_ino(), cg_path }
    }
}

impl Inode for CgroupDir {
    fn ino(&self) -> usize {
        self.ino
    }
    fn file_type(&self) -> FileType {
        FileType::Directory
    }
    fn as_file(&self) -> Option<&dyn File> {
        None
    }
    fn as_dir(&self) -> Option<&dyn Directory> {
        Some(self)
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        let (uid, gid, mode) = match attr_lookup(&self.cg_path, "") {
            Some(o) => (o.uid, o.gid, o.mode.unwrap_or(0o040755)),
            None => (0, 0, 0o040755),
        };
        Ok(FileStat {
            file_type: FileType::Directory,
            size: 0,
            mode: FileMode::new(mode),
            nlink: 2,
            uid,
            gid,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
    fn set_owner(&self, uid: Option<u32>, gid: Option<u32>) -> Result<(), FsError> {
        attr_set_owner(&self.cg_path, "", uid, gid);
        Ok(())
    }
    fn set_mode(&self, mode: u32) -> Result<(), FsError> {
        attr_set_mode(&self.cg_path, "", mode, 0o040000);
        Ok(())
    }
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }
}

/// Recover the cgroup-tree path (e.g. `/cg_test_simple`) behind a directory
/// inode, if it is a cgroup2 directory. Used by clone3 CLONE_INTO_CGROUP to
/// turn the user-supplied cgroup dir fd into a tree path.
pub fn cgroup_dir_path(inode: &dyn Inode) -> Option<String> {
    inode
        .as_any()
        .downcast_ref::<CgroupDir>()
        .map(|d| d.cg_path.clone())
}

impl Directory for CgroupDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        // Only child cgroup directories are listed. Interface files are
        // synthetic and accessible by name via lookup(); excluding them keeps
        // an otherwise-empty cgroup reported as empty so rmdir() (whose VFS
        // emptiness check counts readdir entries) succeeds — matching Linux,
        // where only child cgroups/processes block cgroup removal.
        let mut entries = Vec::new();
        for child in cg::list_cgroup_children(&self.cg_path) {
            entries.push(DirEntry {
                name: child,
                file_type: FileType::Directory,
                inode: alloc_ino(),
            });
        }
        Ok(entries)
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        if file_present(&self.cg_path, name) {
            return Ok(Arc::new(CgroupFile::new(self.cg_path.clone(), name.to_string())));
        }
        let child = join_path(&self.cg_path, name);
        if cg::cgroup_node_exists(&child) {
            return Ok(Arc::new(CgroupDir::new(child)));
        }
        Err(FsError::NotFound)
    }

    fn create(&self, name: &str, file_type: FileType) -> Result<Arc<dyn Inode>, FsError> {
        // Only mkdir (sub-cgroup creation) is supported; interface files are
        // synthetic and cannot be created by userspace.
        if file_type != FileType::Directory {
            return Err(FsError::PermissionDenied);
        }
        match cg::mkdir_cgroup(&self.cg_path, name) {
            Ok(()) => Ok(Arc::new(CgroupDir::new(join_path(&self.cg_path, name)))),
            Err(-17) => Err(FsError::AlreadyExists),
            Err(-2) => Err(FsError::NotFound),
            Err(_) => Err(FsError::InvalidArgument),
        }
    }

    fn unlink(&self, name: &str) -> Result<(), FsError> {
        // rmdir of a child cgroup. Interface files cannot be removed.
        let child = join_path(&self.cg_path, name);
        if !cg::cgroup_node_exists(&child) {
            if file_present(&self.cg_path, name) {
                return Err(FsError::PermissionDenied);
            }
            return Err(FsError::NotFound);
        }
        match cg::rmdir_cgroup(&child) {
            Ok(()) => {
                attr_clear_subtree(&child);
                Ok(())
            }
            Err(-16) => Err(FsError::Busy),
            Err(-2) => Err(FsError::NotFound),
            Err(_) => Err(FsError::InvalidArgument),
        }
    }
}

// =============================================================================
// CgroupFile — a cgroup interface file
// =============================================================================

struct CgroupFile {
    ino: usize,
    cg_path: String,
    name: String,
    /// euid of the task that opened this description, captured at open time
    /// (`u32::MAX` until set). cgroup.procs migration uses the *opener's*
    /// credentials, not the writer's — Linux `of->file->f_cred`, CVE-2021-4197.
    open_euid: AtomicU32,
    /// egid captured alongside `open_euid`.
    open_egid: AtomicU32,
}

impl CgroupFile {
    fn new(cg_path: String, name: String) -> Self {
        Self {
            ino: alloc_ino(),
            cg_path,
            name,
            open_euid: AtomicU32::new(u32::MAX),
            open_egid: AtomicU32::new(u32::MAX),
        }
    }

    fn mode(&self) -> u32 {
        if is_readonly(&self.name) { 0o100444 } else { 0o100644 }
    }

    /// Enforce open-time-credential migration permission for cgroup.procs /
    /// cgroup.threads writes (CVE-2021-4197). Returns `Ok(())` if the migration
    /// is permitted, `Err(AccessDenied)` (→ EACCES) otherwise.
    fn check_migration_perm(&self, target_pid: usize) -> Result<(), FsError> {
        let open_euid = self.open_euid.load(Ordering::Relaxed);
        let (cur_euid, cur_egid) = current_euid_egid();
        let euid = if open_euid == u32::MAX { cur_euid } else { open_euid };
        // Privileged opener migrates freely (CAP_SYS_ADMIN / root).
        if euid == 0 {
            return Ok(());
        }
        let open_egid = self.open_egid.load(Ordering::Relaxed);
        let egid = if open_egid == u32::MAX { cur_egid } else { open_egid };
        // The migration must be authorised against the common ancestor of the
        // task's source cgroup and this (destination) cgroup — Linux checks
        // write permission on that ancestor's cgroup.procs with the opener's
        // credentials.
        let src = cg::cgroup_of_pid(target_pid).unwrap_or_else(|| String::from("/"));
        let anc = common_ancestor(&src, &self.cg_path);
        let (auid, agid, amode) = match attr_lookup(&anc, "cgroup.procs") {
            Some(o) => (o.uid, o.gid, o.mode.unwrap_or(0o100644) & 0o7777),
            None => (0, 0, 0o644),
        };
        if may_write(euid, egid, auid, agid, amode) {
            Ok(())
        } else {
            Err(FsError::AccessDenied)
        }
    }
}

impl Inode for CgroupFile {
    fn ino(&self) -> usize {
        self.ino
    }
    fn file_type(&self) -> FileType {
        FileType::Regular
    }
    fn as_file(&self) -> Option<&dyn File> {
        Some(self)
    }
    fn as_dir(&self) -> Option<&dyn Directory> {
        None
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        let size = cg::read_cgroup_file(&self.cg_path, &self.name)
            .map(|s| s.len())
            .unwrap_or(0);
        let (uid, gid, mode) = match attr_lookup(&self.cg_path, &self.name) {
            Some(o) => (o.uid, o.gid, o.mode.unwrap_or_else(|| self.mode())),
            None => (0, 0, self.mode()),
        };
        Ok(FileStat {
            file_type: FileType::Regular,
            size,
            mode: FileMode::new(mode),
            nlink: 1,
            uid,
            gid,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
    fn set_owner(&self, uid: Option<u32>, gid: Option<u32>) -> Result<(), FsError> {
        attr_set_owner(&self.cg_path, &self.name, uid, gid);
        Ok(())
    }
    fn set_mode(&self, mode: u32) -> Result<(), FsError> {
        attr_set_mode(&self.cg_path, &self.name, mode, 0o100000);
        Ok(())
    }
    fn note_open_cred(&self, euid: u32, egid: u32) {
        self.open_euid.store(euid, Ordering::Relaxed);
        self.open_egid.store(egid, Ordering::Relaxed);
    }
}

impl File for CgroupFile {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let content = cg::read_cgroup_file(&self.cg_path, &self.name).unwrap_or_default();
        let bytes = content.as_bytes();
        if offset >= bytes.len() {
            return Ok(0);
        }
        let end = core::cmp::min(offset + buf.len(), bytes.len());
        let n = end - offset;
        buf[..n].copy_from_slice(&bytes[offset..end]);
        Ok(n)
    }

    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        let s = core::str::from_utf8(buf).map_err(|_| FsError::InvalidArgument)?;

        // cgroup.procs / cgroup.threads migrate a task into this cgroup.
        if self.name == "cgroup.procs" || self.name == "cgroup.threads" {
            // Linux: a written pid of "0" means the calling task. Resolve it so
            // the membership set records the real pid — otherwise the stale 0
            // never leaves the cgroup on exit and rmdir() wrongly returns EBUSY.
            let target_pid = match s.trim().parse::<usize>() {
                Ok(0) => crate::kernel::security::current_pid(),
                Ok(p) => p,
                Err(_) => return Err(FsError::InvalidArgument),
            };
            self.check_migration_perm(target_pid)?;
            let resolved = alloc::format!("{}", target_pid);
            return match cg::write_cgroup_file(&self.cg_path, &self.name, &resolved) {
                Ok(()) => Ok(buf.len()),
                Err(-2) => Err(FsError::NotFound),
                Err(-22) => Err(FsError::InvalidArgument),
                Err(_) => Err(FsError::PermissionDenied),
            };
        }

        match cg::write_cgroup_file(&self.cg_path, &self.name, s) {
            Ok(()) => Ok(buf.len()),
            Err(-2) => Err(FsError::NotFound),
            Err(-22) => Err(FsError::InvalidArgument),
            Err(_) => Err(FsError::PermissionDenied),
        }
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Inode::stat(self)
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}
