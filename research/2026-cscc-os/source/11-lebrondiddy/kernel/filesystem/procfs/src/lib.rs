#![no_std]

extern crate alloc;

mod interrupts;
mod meminfo;
mod mounts;

use alloc::{collections::BTreeMap, string::ToString, sync::Arc, vec::Vec};
use core::cmp;
use interrupts::Interrupts;
use libc_types::types::{Stat, StatFS, StatMode};
use meminfo::MemInfo;
use mounts::Mounts;
use syscalls::Errno;
use vfscore::{DirEntry, FileSystem, FileType, INodeInterface, VfsResult};

const PROC_CONFIG_GZ: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x73, 0xf6, 0xf7, 0x73, 0xf3, 0x74,
    0x8f, 0x77, 0x0d, 0x73, 0xf5, 0x0b, 0x71, 0x73, 0xb1, 0xad, 0xe4, 0x02, 0x00, 0x29, 0x2e, 0x22,
    0x06, 0x11, 0x00, 0x00, 0x00,
];
const PROC_CONFIG: &[u8] = b"CONFIG_EVENTFD=y\n";

pub struct ProcFS {
    root: Arc<ProcDir>,
}

impl ProcFS {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            root: ProcDir::new(),
        })
    }
}

impl FileSystem for ProcFS {
    fn root_dir(&self) -> Arc<dyn INodeInterface> {
        Arc::new(DevDirContainer {
            inner: self.root.clone(),
        })
    }

    fn name(&self) -> &str {
        "procfs"
    }
}

pub struct ProcDir {
    map: BTreeMap<&'static str, Arc<dyn INodeInterface>>,
}

impl ProcDir {
    pub fn new() -> Arc<ProcDir> {
        let mut map: BTreeMap<&str, Arc<dyn INodeInterface>> = BTreeMap::new();
        map.insert("mounts", Arc::new(Mounts::new()));
        map.insert("meminfo", Arc::new(MemInfo::new()));
        map.insert("interrupts", Arc::new(Interrupts::new()));
        map.insert("uptime", Arc::new(StaticFile::new(b"1.00 0.00\n")));
        map.insert("sched_debug", Arc::new(StaticFile::new(b"")));
        map.insert(
            "cpuinfo",
            Arc::new(StaticFile::new(
                b"processor\t: 0\nhart\t\t: 0\nisa\t\t: rv64imafdch\n",
            )),
        );
        map.insert(
            "stat",
            Arc::new(StaticFile::new(
                b"cpu  1 0 1 100 0 0 0 0 0 0\n\
ctxt 1\n\
btime 0\n\
processes 1\n\
procs_running 1\n\
procs_blocked 0\n",
            )),
        );
        map.insert("cmdline", Arc::new(StaticFile::new(b"ByteOS\n")));
        map.insert(
            "version",
            Arc::new(StaticFile::new(
                b"Linux version 5.10.0 (ByteOS) #1 SMP PREEMPT\n",
            )),
        );
        map.insert("config", Arc::new(StaticFile::new(PROC_CONFIG)));
        map.insert("config.gz", Arc::new(StaticFile::new(PROC_CONFIG_GZ)));
        map.insert("sys", StaticDir::sys());
        map.insert("self", ProcPidDir::new("self", "busybox"));
        map.insert("1", ProcPidDir::new("1", "initproc"));
        Arc::new(ProcDir { map })
    }
}

pub struct StaticFile {
    content: &'static [u8],
}

impl StaticFile {
    pub const fn new(content: &'static [u8]) -> Self {
        Self { content }
    }
}

impl INodeInterface for StaticFile {
    fn readat(&self, offset: usize, buffer: &mut [u8]) -> VfsResult<usize> {
        if offset >= self.content.len() {
            return Ok(0);
        }
        let rsize = cmp::min(self.content.len() - offset, buffer.len());
        buffer[..rsize].copy_from_slice(&self.content[offset..offset + rsize]);
        Ok(rsize)
    }

    fn stat(&self, stat: &mut Stat) -> VfsResult<()> {
        stat.dev = 0;
        stat.ino = 1;
        stat.mode = StatMode::FILE;
        stat.nlink = 1;
        stat.uid = 0;
        stat.gid = 0;
        stat.size = self.content.len() as _;
        stat.blksize = 512;
        stat.blocks = self.content.len().div_ceil(512) as _;
        stat.rdev = 0;
        Ok(())
    }
}

pub struct StaticDir {
    ino: u64,
    map: BTreeMap<&'static str, Arc<dyn INodeInterface>>,
}

impl StaticDir {
    fn sys() -> Arc<Self> {
        let mut map: BTreeMap<&'static str, Arc<dyn INodeInterface>> = BTreeMap::new();
        map.insert("kernel", Self::kernel_sysctl());
        Arc::new(Self { ino: 2, map })
    }

    fn kernel_sysctl() -> Arc<Self> {
        let mut map: BTreeMap<&'static str, Arc<dyn INodeInterface>> = BTreeMap::new();
        map.insert("tainted", Arc::new(StaticFile::new(b"0\n")));
        map.insert("pid_max", Arc::new(StaticFile::new(b"4194304\n")));
        map.insert("threads-max", Arc::new(StaticFile::new(b"4096\n")));
        Arc::new(Self { ino: 3, map })
    }
}

impl INodeInterface for StaticDir {
    fn lookup(&self, name: &str) -> VfsResult<Arc<dyn INodeInterface>> {
        self.map.get(name).cloned().ok_or(Errno::ENOENT)
    }

    fn read_dir(&self) -> VfsResult<Vec<DirEntry>> {
        Ok(self
            .map
            .keys()
            .map(|name| DirEntry {
                filename: name.to_string(),
                len: 0,
                file_type: FileType::File,
            })
            .collect())
    }

    fn stat(&self, stat: &mut Stat) -> VfsResult<()> {
        stat.dev = 0;
        stat.ino = self.ino;
        stat.mode = StatMode::DIR;
        stat.nlink = 1;
        stat.uid = 0;
        stat.gid = 0;
        stat.size = 0;
        stat.blksize = 512;
        stat.blocks = 0;
        stat.rdev = 0;
        Ok(())
    }
}

pub struct ProcPidDir {
    name: &'static str,
    map: BTreeMap<&'static str, Arc<dyn INodeInterface>>,
}

impl ProcPidDir {
    fn new(name: &'static str, comm: &'static str) -> Arc<Self> {
        let mut map: BTreeMap<&'static str, Arc<dyn INodeInterface>> = BTreeMap::new();
        map.insert("cmdline", Arc::new(StaticFile::new(comm.as_bytes())));
        map.insert("comm", Arc::new(StaticFile::new(comm.as_bytes())));
        map.insert("mounts", Arc::new(Mounts::new()));
        map.insert(
            "stat",
            Arc::new(StaticFile::new(
                b"1 (initproc) S 0 1 1 0 -1 0 0 0 0 0 0 0 0 20 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n",
            )),
        );
        map.insert(
            "statm",
            Arc::new(StaticFile::new(b"1024 256 128 64 0 128 0\n")),
        );
        map.insert(
            "maps",
            Arc::new(StaticFile::new(
                b"10000-20000 r-xp 00000000 00:00 0 /bin/busybox\n",
            )),
        );
        map.insert("exe", Arc::new(ProcSelfExe));
        Arc::new(Self { name, map })
    }
}

struct ProcSelfExe;

impl INodeInterface for ProcSelfExe {
    fn resolve_link(&self) -> VfsResult<alloc::string::String> {
        Ok("/bin/busybox".to_string())
    }

    fn stat(&self, stat: &mut Stat) -> VfsResult<()> {
        stat.dev = 0;
        stat.ino = 1;
        stat.mode = StatMode::LINK;
        stat.nlink = 1;
        stat.uid = 0;
        stat.gid = 0;
        stat.size = 14;
        stat.blksize = 512;
        stat.blocks = 0;
        stat.rdev = 0;
        Ok(())
    }
}

impl INodeInterface for ProcPidDir {
    fn lookup(&self, name: &str) -> VfsResult<Arc<dyn INodeInterface>> {
        self.map.get(name).cloned().ok_or(Errno::ENOENT)
    }

    fn read_dir(&self) -> VfsResult<Vec<DirEntry>> {
        Ok(self
            .map
            .keys()
            .map(|name| DirEntry {
                filename: name.to_string(),
                len: 0,
                file_type: FileType::File,
            })
            .collect())
    }

    fn stat(&self, stat: &mut Stat) -> VfsResult<()> {
        stat.dev = 0;
        stat.ino = self.name.as_bytes()[0] as _;
        stat.mode = StatMode::DIR;
        stat.nlink = 1;
        stat.uid = 0;
        stat.gid = 0;
        stat.size = 0;
        stat.blksize = 512;
        stat.blocks = 0;
        stat.rdev = 0;
        Ok(())
    }
}

pub struct DevDirContainer {
    inner: Arc<ProcDir>,
}

impl INodeInterface for DevDirContainer {
    fn lookup(&self, name: &str) -> VfsResult<Arc<dyn INodeInterface>> {
        self.inner.map.get(name).cloned().ok_or(Errno::ENOENT)
    }

    fn read_dir(&self) -> VfsResult<Vec<DirEntry>> {
        Ok(self
            .inner
            .map
            .keys()
            .map(|name| DirEntry {
                filename: name.to_string(),
                len: 0,
                file_type: FileType::Device,
            })
            .collect())
    }

    fn stat(&self, stat: &mut Stat) -> VfsResult<()> {
        stat.dev = 0;
        stat.ino = 1; // TODO: convert path to number(ino)
        stat.mode = StatMode::DIR; // TODO: add access mode
        stat.nlink = 1;
        stat.uid = 0;
        stat.gid = 0;
        stat.size = 0;
        stat.blksize = 512;
        stat.blocks = 0;
        stat.rdev = 0; // TODO: add device id
        Ok(())
    }

    fn statfs(&self, statfs: &mut StatFS) -> VfsResult<()> {
        statfs.ftype = 0x9fa0;
        statfs.bsize = 4096;
        statfs.blocks = 0;
        statfs.bfree = 0;
        statfs.bavail = 0;
        statfs.files = 32;
        statfs.ffree = 0;
        statfs.fsid = 0x9fa0;
        statfs.namelen = 255;
        statfs.frsize = 4096;
        statfs.flags = 0;
        statfs.spare = [0; 4];
        Ok(())
    }
}
