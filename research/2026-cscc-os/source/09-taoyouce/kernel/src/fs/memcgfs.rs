//! memcgfs — the cgroup **v1** memory controller filesystem
//! (`mount -t cgroup -o memory ... /sys/fs/cgroup/memory`).
//!
//! Per Linux `mm/memcontrol.c` + `kernel/cgroup/cgroup-v1.c`. Unlike the v2
//! unified hierarchy (`cgroupfs.rs`), the v1 memory controller is its own
//! hierarchy mounted at `/sys/fs/cgroup/memory`. Each directory is a memory
//! cgroup; `mkdir`/`rmdir` create/remove sub-cgroups; every directory exposes
//! the v1 interface files (`tasks`, `cgroup.procs`, `memory.limit_in_bytes`,
//! `memory.usage_in_bytes`). All tree mutations and file I/O delegate to
//! `crate::mm::memcg`, which owns the real controller state (limits, members)
//! and drives reclaim — so this is a live view of the one global memory
//! hierarchy, not a disconnected copy.
//!
//! This is the dependency LTP `madvise09` mounts to validate `MADV_FREE` lazy
//! reclaim + anonymous swap-out under a per-cgroup `memory.limit_in_bytes`.

use super::{DirEntry, Directory, File, FileMode, FileStat, FileSystem, FileType, FsError, Inode};
use crate::mm::memcg;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

static MEMCGFS_NEXT_INO: AtomicUsize = AtomicUsize::new(0xC600_0001);

fn alloc_ino() -> usize {
    MEMCGFS_NEXT_INO.fetch_add(1, Ordering::Relaxed)
}

fn join_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", parent, name)
    }
}

/// v1 memory controller interface files present in every memcg directory.
/// Intentionally excludes `memory.memsw.limit_in_bytes`: its absence makes LTP
/// treat swap accounting as disabled (CONFIG_MEMCG_SWAP off), matching this
/// kernel — `madvise09` then validates the no-memsw path.
const FILES: &[&str] = &[
    "tasks",
    "cgroup.procs",
    "memory.limit_in_bytes",
    "memory.usage_in_bytes",
];

fn is_readonly(name: &str) -> bool {
    matches!(name, "memory.usage_in_bytes")
}

fn file_present(name: &str) -> bool {
    FILES.contains(&name)
}

/// Render an interface file's contents.
fn read_file(cg_path: &str, name: &str) -> String {
    match name {
        "tasks" | "cgroup.procs" => memcg::read_tasks(cg_path),
        "memory.limit_in_bytes" => alloc::format!("{}\n", memcg::get_limit(cg_path)),
        "memory.usage_in_bytes" => alloc::format!("{}\n", memcg::usage_bytes(cg_path)),
        _ => String::new(),
    }
}

/// Apply a write to an interface file. Returns 0 on success, negative errno.
fn write_file(cg_path: &str, name: &str, data: &str) -> isize {
    match name {
        "tasks" | "cgroup.procs" => {
            let pid = match data.trim().parse::<usize>() {
                Ok(p) => p,
                Err(_) => return -22,
            };
            if memcg::add_task(cg_path, pid) { 0 } else { -2 }
        }
        "memory.limit_in_bytes" => {
            let v = match data.trim().parse::<i64>() {
                Ok(v) => v,
                Err(_) => return -22,
            };
            // Linux rounds down to a page boundary; -1 means "unlimited".
            let bytes = if v < 0 {
                memcg::UNLIMITED
            } else {
                (v as usize) & !(crate::mm::PAGE_SIZE - 1)
            };
            if memcg::set_limit(cg_path, bytes) { 0 } else { -2 }
        }
        _ => -22,
    }
}

// =============================================================================
// MemcgFs filesystem
// =============================================================================

pub struct MemcgFs;

impl MemcgFs {
    pub fn new() -> Arc<Self> {
        // Ensure the root memory cgroup exists.
        memcg::create("/");
        Arc::new(Self)
    }
}

impl FileSystem for MemcgFs {
    fn name(&self) -> &str {
        "cgroup"
    }
    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(MemcgDir::new(String::from("/")))
    }
    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

// =============================================================================
// MemcgDir — a memory cgroup directory node
// =============================================================================

struct MemcgDir {
    ino: usize,
    cg_path: String,
}

impl MemcgDir {
    fn new(cg_path: String) -> Self {
        Self { ino: alloc_ino(), cg_path }
    }
}

impl Inode for MemcgDir {
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
        Ok(FileStat {
            file_type: FileType::Directory,
            size: 0,
            mode: FileMode::new(0o040755),
            nlink: 2,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}

impl Directory for MemcgDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        // List child memcg directories AND the interface files, so a directory
        // listing of the mount shows the v1 control files (LTP enumerates them).
        let mut entries = Vec::new();
        for name in FILES {
            entries.push(DirEntry {
                name: name.to_string(),
                file_type: FileType::Regular,
                inode: alloc_ino(),
            });
        }
        for child in memcg::children(&self.cg_path) {
            entries.push(DirEntry {
                name: child,
                file_type: FileType::Directory,
                inode: alloc_ino(),
            });
        }
        Ok(entries)
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        if file_present(name) {
            return Ok(Arc::new(MemcgFile::new(self.cg_path.clone(), name.to_string())));
        }
        let child = join_path(&self.cg_path, name);
        if memcg::exists(&child) {
            return Ok(Arc::new(MemcgDir::new(child)));
        }
        Err(FsError::NotFound)
    }

    fn create(&self, name: &str, file_type: FileType) -> Result<Arc<dyn Inode>, FsError> {
        if file_type != FileType::Directory {
            return Err(FsError::PermissionDenied);
        }
        let child = join_path(&self.cg_path, name);
        if memcg::create(&child) {
            Ok(Arc::new(MemcgDir::new(child)))
        } else {
            Err(FsError::AlreadyExists)
        }
    }

    fn unlink(&self, name: &str) -> Result<(), FsError> {
        let child = join_path(&self.cg_path, name);
        if memcg::exists(&child) {
            if memcg::remove(&child) {
                Ok(())
            } else {
                Err(FsError::Busy)
            }
        } else if file_present(name) {
            Err(FsError::PermissionDenied)
        } else {
            Err(FsError::NotFound)
        }
    }
}

// =============================================================================
// MemcgFile — a memory cgroup interface file
// =============================================================================

struct MemcgFile {
    ino: usize,
    cg_path: String,
    name: String,
}

impl MemcgFile {
    fn new(cg_path: String, name: String) -> Self {
        Self { ino: alloc_ino(), cg_path, name }
    }
    fn mode(&self) -> u32 {
        if is_readonly(&self.name) { 0o100444 } else { 0o100644 }
    }
}

impl Inode for MemcgFile {
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
        Ok(FileStat {
            file_type: FileType::Regular,
            size: read_file(&self.cg_path, &self.name).len(),
            mode: FileMode::new(self.mode()),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}

impl File for MemcgFile {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let content = read_file(&self.cg_path, &self.name);
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
        match write_file(&self.cg_path, &self.name, s) {
            0 => Ok(buf.len()),
            -2 => Err(FsError::NotFound),
            _ => Err(FsError::InvalidArgument),
        }
    }

    // cgroup interface files have no backing length to truncate; an O_TRUNC
    // open (e.g. glibc `fopen(path, "w")`, which LTP's SAFE_FILE_PRINTF uses)
    // is a no-op. Without this the default impl returns EACCES and the write
    // open fails — matching neither Linux nor the test's expectation.
    fn truncate(&self, _length: usize) -> Result<(), FsError> {
        Ok(())
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Inode::stat(self)
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}
