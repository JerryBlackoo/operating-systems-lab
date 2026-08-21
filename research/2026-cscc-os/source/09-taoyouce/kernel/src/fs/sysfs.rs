//! sysfs — Linux /sys/ virtual filesystem
//!
//! Implements the sysfs filesystem per Linux fs/sysfs/ semantics.
//! sysfs exports kernel objects (devices, buses, classes, modules, etc.)
//! as a directory hierarchy with attribute files containing runtime info.
//!
//! Key subdirectories (per Linux):
//! - /sys/class/ — device class directories (net, block, tty, etc.)
//! - /sys/devices/ — unified device model tree
//! - /sys/bus/ — bus type directories (pci, platform, etc.)
//! - /sys/fs/ — filesystem-specific parameters (cgroup, fuse, etc.)
//! - /sys/kernel/ — kernel runtime parameters
//! - /sys/module/ — loaded kernel modules
//! - /sys/power/ — power management state
//! - /sys/block/ — block device symlinks
//! - /sys/dev/ — char/ and block/ device number mappings

use super::{DirEntry, Directory, File, FileMode, FileStat, FileSystem, FileType, FsError, Inode};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sync::SpinLock;

// =============================================================================
// SysFs filesystem
// =============================================================================

static SYSFS_NEXT_INO: AtomicUsize = AtomicUsize::new(0x5900_0001);

fn alloc_ino() -> usize {
    SYSFS_NEXT_INO.fetch_add(1, Ordering::Relaxed)
}

/// The sysfs filesystem type.
pub struct SysFs;

impl SysFs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl FileSystem for SysFs {
    fn name(&self) -> &str {
        "sysfs"
    }

    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(SysfsRoot::new())
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

// =============================================================================
// SysfsRoot — the /sys/ root directory
// =============================================================================

struct SysfsRoot {
    ino: usize,
}

impl SysfsRoot {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }

    fn top_dirs() -> Vec<&'static str> {
        // Per Linux, these are always present in /sys/
        alloc::vec![
            "block", "bus", "class", "dev", "devices", "firmware",
            "fs", "kernel", "module", "power",
        ]
    }
}

impl Inode for SysfsRoot {
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
            mode: FileMode::new(0o040555),
            nlink: 2 + Self::top_dirs().len(),
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}

impl Directory for SysfsRoot {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let mut entries = Vec::new();
        for name in Self::top_dirs() {
            entries.push(DirEntry {
                name: name.to_string(),
                file_type: FileType::Directory,
                inode: alloc_ino(),
            });
        }
        Ok(entries)
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "block" => Ok(Arc::new(SysfsBlockDir::new())),
            "bus" => Ok(Arc::new(SysfsBusDir::new())),
            "class" => Ok(Arc::new(SysfsClassDir::new())),
            "dev" => Ok(Arc::new(SysfsDevDir::new())),
            "devices" => Ok(Arc::new(SysfsDevicesDir::new())),
            "firmware" => Ok(Arc::new(SysfsEmptyDir::new("firmware"))),
            "fs" => Ok(Arc::new(SysfsFsDir::new())),
            "kernel" => Ok(Arc::new(SysfsKernelDir::new())),
            "module" => Ok(Arc::new(SysfsModuleDir::new())),
            "power" => Ok(Arc::new(SysfsPowerDir::new())),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _file_type: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied) // sysfs is read-only from userspace
    }

    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// Helper: generic empty directory node (for stub subdirs)
// =============================================================================

struct SysfsEmptyDir {
    ino: usize,
    #[allow(dead_code)]
    name: &'static str,
}

impl SysfsEmptyDir {
    fn new(name: &'static str) -> Self {
        Self {
            ino: alloc_ino(),
            name,
        }
    }
}

impl Inode for SysfsEmptyDir {
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
        Ok(dir_stat(self.ino, 2))
    }
}

impl Directory for SysfsEmptyDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(Vec::new())
    }
    fn lookup(&self, _name: &str) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::NotFound)
    }
    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// Helper: attribute file (read-only text)
// =============================================================================

struct SysfsAttrFile {
    ino: usize,
    content: String,
}

impl SysfsAttrFile {
    fn new(content: String) -> Self {
        Self {
            ino: alloc_ino(),
            content,
        }
    }
}

impl Inode for SysfsAttrFile {
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
            size: self.content.len(),
            mode: FileMode::new(0o100444),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}

impl File for SysfsAttrFile {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let bytes = self.content.as_bytes();
        if offset >= bytes.len() {
            return Ok(0);
        }
        let end = core::cmp::min(offset + buf.len(), bytes.len());
        let n = end - offset;
        buf[..n].copy_from_slice(&bytes[offset..end]);
        Ok(n)
    }
    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Inode::stat(self)
    }
    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

// =============================================================================
// Helper: writable attribute file (for kernel tunables)
// =============================================================================

struct SysfsWritableAttrFile {
    ino: usize,
    content: SpinLock<String>,
}

impl SysfsWritableAttrFile {
    fn new(initial: String) -> Self {
        Self {
            ino: alloc_ino(),
            content: SpinLock::new(initial),
        }
    }
}

impl Inode for SysfsWritableAttrFile {
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
        let len = self.content.lock().len();
        Ok(FileStat {
            file_type: FileType::Regular,
            size: len,
            mode: FileMode::new(0o100644),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}

impl File for SysfsWritableAttrFile {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let data = self.content.lock();
        let bytes = data.as_bytes();
        if offset >= bytes.len() {
            return Ok(0);
        }
        let end = core::cmp::min(offset + buf.len(), bytes.len());
        let n = end - offset;
        buf[..n].copy_from_slice(&bytes[offset..end]);
        Ok(n)
    }
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        if let Ok(s) = core::str::from_utf8(buf) {
            *self.content.lock() = s.trim_end_matches('\n').to_string();
            // Append newline for consistent read-back
            self.content.lock().push('\n');
        }
        Ok(buf.len())
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Inode::stat(self)
    }
    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

// =============================================================================
// Helper: directory stat constructor
// =============================================================================

fn dir_stat(ino: usize, nlink: usize) -> FileStat {
    let _ = ino;
    FileStat {
        file_type: FileType::Directory,
        size: 0,
        mode: FileMode::new(0o040555),
        nlink,
        uid: 0,
        gid: 0,
        atime: 0,
        mtime: 0,
        ctime: 0,
    }
}

// =============================================================================
// /sys/block/ — block device listing
// =============================================================================

struct SysfsBlockDir {
    ino: usize,
}

impl SysfsBlockDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsBlockDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsBlockDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        // Expose loop devices as block devices (loop0..loop7)
        let mut entries = Vec::new();
        for i in 0..8 {
            entries.push(DirEntry {
                name: format!("loop{}", i),
                file_type: FileType::Directory,
                inode: alloc_ino(),
            });
        }
        // Virtual disk vda
        entries.push(DirEntry {
            name: "vda".to_string(),
            file_type: FileType::Directory,
            inode: alloc_ino(),
        });
        Ok(entries)
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        if name.starts_with("loop") {
            if let Ok(n) = name[4..].parse::<usize>() {
                if n < 8 {
                    return Ok(Arc::new(SysfsBlockDevDir::new_loop(n)));
                }
            }
        }
        if name == "vda" {
            return Ok(Arc::new(SysfsBlockDevDir::new_vda()));
        }
        Err(FsError::NotFound)
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// Block device attribute directory
struct SysfsBlockDevDir {
    ino: usize,
    dev_name: String,
    major: u32,
    minor: u32,
}

impl SysfsBlockDevDir {
    fn new_loop(n: usize) -> Self {
        Self {
            ino: alloc_ino(),
            dev_name: format!("loop{}", n),
            major: 7,
            minor: n as u32,
        }
    }

    fn new_vda() -> Self {
        Self {
            ino: alloc_ino(),
            dev_name: "vda".to_string(),
            major: 254,
            minor: 0,
        }
    }
}

impl Inode for SysfsBlockDevDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsBlockDevDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let names = ["dev", "size", "stat", "ro", "removable", "uevent"];
        Ok(names
            .iter()
            .map(|n| DirEntry {
                name: n.to_string(),
                file_type: FileType::Regular,
                inode: alloc_ino(),
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        let content = match name {
            "dev" => format!("{}:{}\n", self.major, self.minor),
            "size" => {
                // Size in 512-byte sectors; report a reasonable value
                if self.dev_name.starts_with("loop") {
                    "0\n".to_string()
                } else {
                    // vda: 8GB default
                    format!("{}\n", 8 * 1024 * 1024 * 2) // 8GB in 512-byte sectors
                }
            }
            "stat" => {
                // Block device I/O stats: reads, merged, sectors, ms, writes, ...
                "0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n".to_string()
            }
            "ro" => "0\n".to_string(),
            "removable" => "0\n".to_string(),
            "uevent" => format!(
                "MAJOR={}\nMINOR={}\nDEVNAME={}\nDEVTYPE=disk\n",
                self.major, self.minor, self.dev_name
            ),
            _ => return Err(FsError::NotFound),
        };
        Ok(Arc::new(SysfsAttrFile::new(content)))
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// /sys/bus/ — bus type directory
// =============================================================================

struct SysfsBusDir {
    ino: usize,
}

impl SysfsBusDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsBusDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsBusDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let buses = ["platform", "virtio", "clocksource"];
        Ok(buses
            .iter()
            .map(|n| DirEntry {
                name: n.to_string(),
                file_type: FileType::Directory,
                inode: alloc_ino(),
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "platform" | "virtio" | "clocksource" => {
                Ok(Arc::new(SysfsBusTypeDir::new(name)))
            }
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// Per-bus-type directory (has devices/ and drivers/ subdirs per Linux)
struct SysfsBusTypeDir {
    ino: usize,
    #[allow(dead_code)]
    name: String,
}

impl SysfsBusTypeDir {
    fn new(name: &str) -> Self {
        Self {
            ino: alloc_ino(),
            name: name.to_string(),
        }
    }
}

impl Inode for SysfsBusTypeDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 4)) }
}

impl Directory for SysfsBusTypeDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "devices".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "drivers".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "uevent".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "devices" | "drivers" => Ok(Arc::new(SysfsEmptyDir::new("sub"))),
            "uevent" => Ok(Arc::new(SysfsAttrFile::new(String::new()))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// /sys/class/ — device class directory
// =============================================================================

struct SysfsClassDir {
    ino: usize,
}

impl SysfsClassDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }

    fn classes() -> &'static [&'static str] {
        &[
            "block", "mem", "misc", "net", "tty", "input",
            "rtc", "watchdog", "thermal", "power_supply",
        ]
    }
}

impl Inode for SysfsClassDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsClassDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(Self::classes()
            .iter()
            .map(|n| DirEntry {
                name: n.to_string(),
                file_type: FileType::Directory,
                inode: alloc_ino(),
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        if Self::classes().contains(&name) {
            match name {
                "net" => Ok(Arc::new(SysfsClassNetDir::new())),
                _ => Ok(Arc::new(SysfsEmptyDir::new("class_sub"))),
            }
        } else {
            Err(FsError::NotFound)
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/class/net/ — network interface listing
struct SysfsClassNetDir {
    ino: usize,
}

impl SysfsClassNetDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsClassNetDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsClassNetDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "lo".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "eth0".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "lo" => Ok(Arc::new(SysfsNetIfDir::new_lo())),
            "eth0" => Ok(Arc::new(SysfsNetIfDir::new_eth0())),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// Per network interface attribute dir (per Linux /sys/class/net/<iface>/)
struct SysfsNetIfDir {
    ino: usize,
    ifname: String,
    ifindex: u32,
    mtu: u32,
    flags: u32,    // IFF_UP | IFF_LOOPBACK etc.
    address: String, // MAC address
    operstate: &'static str,
    tx_queue_len: u32,
    if_type: u32,  // ARPHRD_*
}

impl SysfsNetIfDir {
    fn new_lo() -> Self {
        Self {
            ino: alloc_ino(),
            ifname: "lo".to_string(),
            ifindex: 1,
            mtu: 65536,
            flags: 0x49, // IFF_UP | IFF_LOOPBACK | IFF_RUNNING
            address: "00:00:00:00:00:00".to_string(),
            operstate: "unknown",
            tx_queue_len: 1000,
            if_type: 772, // ARPHRD_LOOPBACK
        }
    }

    fn new_eth0() -> Self {
        Self {
            ino: alloc_ino(),
            ifname: "eth0".to_string(),
            ifindex: 2,
            mtu: 1500,
            flags: 0x1003, // IFF_UP | IFF_BROADCAST | IFF_MULTICAST
            address: "52:54:00:12:34:56".to_string(),
            operstate: "up",
            tx_queue_len: 1000,
            if_type: 1, // ARPHRD_ETHER
        }
    }

    fn attrs(&self) -> Vec<(&'static str, String)> {
        alloc::vec![
            ("address", format!("{}\n", self.address)),
            ("addr_len", "6\n".to_string()),
            ("carrier", "1\n".to_string()),
            ("dev_id", "0x0\n".to_string()),
            ("dormant", "0\n".to_string()),
            ("duplex", "full\n".to_string()),
            ("flags", format!("0x{:x}\n", self.flags)),
            ("ifindex", format!("{}\n", self.ifindex)),
            ("iflink", format!("{}\n", self.ifindex)),
            ("link_mode", "0\n".to_string()),
            ("mtu", format!("{}\n", self.mtu)),
            ("operstate", format!("{}\n", self.operstate)),
            ("speed", "1000\n".to_string()),
            ("tx_queue_len", format!("{}\n", self.tx_queue_len)),
            ("type", format!("{}\n", self.if_type)),
            ("uevent", format!("INTERFACE={}\nIFINDEX={}\n", self.ifname, self.ifindex)),
        ]
    }
}

impl Inode for SysfsNetIfDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsNetIfDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(self
            .attrs()
            .iter()
            .map(|(name, _)| DirEntry {
                name: name.to_string(),
                file_type: FileType::Regular,
                inode: alloc_ino(),
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        for (attr_name, content) in self.attrs() {
            if attr_name == name {
                return Ok(Arc::new(SysfsAttrFile::new(content)));
            }
        }
        // Also support "statistics" subdirectory per Linux
        if name == "statistics" {
            return Ok(Arc::new(SysfsNetStatsDir::new()));
        }
        Err(FsError::NotFound)
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/class/net/<iface>/statistics/
struct SysfsNetStatsDir {
    ino: usize,
}

impl SysfsNetStatsDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }

    fn stats() -> &'static [&'static str] {
        &[
            "rx_bytes", "rx_packets", "rx_errors", "rx_dropped",
            "tx_bytes", "tx_packets", "tx_errors", "tx_dropped",
            "multicast", "collisions",
            "rx_length_errors", "rx_over_errors", "rx_crc_errors",
            "rx_frame_errors", "rx_fifo_errors", "rx_missed_errors",
            "tx_aborted_errors", "tx_carrier_errors", "tx_fifo_errors",
            "tx_heartbeat_errors", "tx_window_errors",
        ]
    }
}

impl Inode for SysfsNetStatsDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsNetStatsDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(Self::stats()
            .iter()
            .map(|n| DirEntry {
                name: n.to_string(),
                file_type: FileType::Regular,
                inode: alloc_ino(),
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        if Self::stats().contains(&name) {
            Ok(Arc::new(SysfsAttrFile::new("0\n".to_string())))
        } else {
            Err(FsError::NotFound)
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// /sys/dev/ — device number mapping (char/ and block/)
// =============================================================================

struct SysfsDevDir {
    ino: usize,
}

impl SysfsDevDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsDevDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 4)) }
}

impl Directory for SysfsDevDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "char".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "block".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "char" | "block" => Ok(Arc::new(SysfsEmptyDir::new("devmap"))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// /sys/devices/ — unified device model
// =============================================================================

struct SysfsDevicesDir {
    ino: usize,
}

impl SysfsDevicesDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsDevicesDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsDevicesDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "system".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "virtual".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "platform".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "system" => Ok(Arc::new(SysfsDevicesSystemDir::new())),
            "virtual" => Ok(Arc::new(SysfsEmptyDir::new("virtual"))),
            "platform" => Ok(Arc::new(SysfsEmptyDir::new("platform"))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/devices/system/ — system devices (cpu, clocksource, node)
struct SysfsDevicesSystemDir {
    ino: usize,
}

impl SysfsDevicesSystemDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsDevicesSystemDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsDevicesSystemDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "cpu".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "clocksource".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "node".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "cpu" => Ok(Arc::new(SysfsCpuDir::new())),
            "clocksource" => Ok(Arc::new(SysfsEmptyDir::new("clocksource"))),
            "node" => Ok(Arc::new(SysfsEmptyDir::new("node"))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/devices/system/cpu/ — CPU topology and frequency info
struct SysfsCpuDir {
    ino: usize,
}

impl SysfsCpuDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsCpuDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsCpuDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let mut entries = Vec::new();
        let online = crate::sched::smp::num_online().max(1);
        for cpu_id in 0..online {
            entries.push(DirEntry {
                name: format!("cpu{}", cpu_id),
                file_type: FileType::Directory,
                inode: alloc_ino(),
            });
        }
        // Topology files per Linux
        entries.push(DirEntry {
            name: "online".to_string(),
            file_type: FileType::Regular,
            inode: alloc_ino(),
        });
        entries.push(DirEntry {
            name: "possible".to_string(),
            file_type: FileType::Regular,
            inode: alloc_ino(),
        });
        entries.push(DirEntry {
            name: "present".to_string(),
            file_type: FileType::Regular,
            inode: alloc_ino(),
        });
        entries.push(DirEntry {
            name: "kernel_max".to_string(),
            file_type: FileType::Regular,
            inode: alloc_ino(),
        });
        entries.push(DirEntry {
            name: "offline".to_string(),
            file_type: FileType::Regular,
            inode: alloc_ino(),
        });
        Ok(entries)
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        let online = crate::sched::smp::num_online().max(1);
        let cpu_list = if online <= 1 {
            "0".to_string()
        } else {
            format!("0-{}", online - 1)
        };
        match name {
            "online" => Ok(Arc::new(SysfsAttrFile::new(format!("{}\n", cpu_list)))),
            "possible" => Ok(Arc::new(SysfsAttrFile::new(format!("{}\n", cpu_list)))),
            "present" => Ok(Arc::new(SysfsAttrFile::new(format!("{}\n", cpu_list)))),
            "kernel_max" => Ok(Arc::new(SysfsAttrFile::new("255\n".to_string()))),
            "offline" => Ok(Arc::new(SysfsAttrFile::new("\n".to_string()))),
            _ => {
                // cpuN lookup for N < online
                if let Some(n) = name.strip_prefix("cpu").and_then(|s| s.parse::<u32>().ok()) {
                    if n < online as u32 {
                        return Ok(Arc::new(SysfsCpuNDir::new(n)));
                    }
                    return Err(FsError::NotFound);
                }
                Err(FsError::NotFound)
            }
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/devices/system/cpu/cpuN/ — per-CPU directory
struct SysfsCpuNDir {
    ino: usize,
    cpu_id: u32,
}

impl SysfsCpuNDir {
    fn new(cpu_id: u32) -> Self {
        Self {
            ino: alloc_ino(),
            cpu_id,
        }
    }
}

impl Inode for SysfsCpuNDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsCpuNDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "online".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "topology".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "uevent".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "online" => Ok(Arc::new(SysfsAttrFile::new("1\n".to_string()))),
            "topology" => Ok(Arc::new(SysfsCpuTopoDir::new(self.cpu_id))),
            "uevent" => Ok(Arc::new(SysfsAttrFile::new(String::new()))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/devices/system/cpu/cpuN/topology/
struct SysfsCpuTopoDir {
    ino: usize,
    cpu_id: u32,
}

impl SysfsCpuTopoDir {
    fn new(cpu_id: u32) -> Self {
        Self {
            ino: alloc_ino(),
            cpu_id,
        }
    }
}

impl Inode for SysfsCpuTopoDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsCpuTopoDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let names = [
            "core_id", "core_siblings", "physical_package_id",
            "thread_siblings",
        ];
        Ok(names
            .iter()
            .map(|n| DirEntry {
                name: n.to_string(),
                file_type: FileType::Regular,
                inode: alloc_ino(),
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        let content = match name {
            "core_id" => format!("{}\n", self.cpu_id),
            "core_siblings" => "1\n".to_string(),
            "physical_package_id" => "0\n".to_string(),
            "thread_siblings" => "1\n".to_string(),
            _ => return Err(FsError::NotFound),
        };
        Ok(Arc::new(SysfsAttrFile::new(content)))
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// /sys/fs/ — filesystem parameters
// =============================================================================

struct SysfsFsDir {
    ino: usize,
}

impl SysfsFsDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsFsDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsFsDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "cgroup".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "ext4".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "cgroup" | "ext4" => Ok(Arc::new(SysfsEmptyDir::new("fs_sub"))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// /sys/kernel/ — kernel runtime parameters
// =============================================================================

struct SysfsKernelDir {
    ino: usize,
}

impl SysfsKernelDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsKernelDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsKernelDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "fscaps".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "hostname".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "domainname".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "osrelease".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "ostype".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "version".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "uevent_seqnum".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "mm".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "security".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "debug".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "fscaps" => Ok(Arc::new(SysfsAttrFile::new("1\n".to_string()))),
            "hostname" => Ok(Arc::new(SysfsWritableAttrFile::new("RustMicroOS\n".to_string()))),
            "domainname" => Ok(Arc::new(SysfsWritableAttrFile::new("(none)\n".to_string()))),
            "osrelease" => Ok(Arc::new(SysfsAttrFile::new("6.1.0-RustMicroOS\n".to_string()))),
            "ostype" => Ok(Arc::new(SysfsAttrFile::new("Linux\n".to_string()))),
            "version" => Ok(Arc::new(SysfsAttrFile::new(
                "#1 SMP PREEMPT_DYNAMIC RustMicroOS\n".to_string(),
            ))),
            "uevent_seqnum" => Ok(Arc::new(SysfsAttrFile::new("0\n".to_string()))),
            "mm" => Ok(Arc::new(SysfsKernelMmDir::new())),
            "security" => Ok(Arc::new(SysfsEmptyDir::new("security"))),
            "debug" => Ok(Arc::new(DebugfsRootDir::new())),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/kernel/mm/ — memory management parameters
struct SysfsKernelMmDir {
    ino: usize,
}

impl SysfsKernelMmDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsKernelMmDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsKernelMmDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "transparent_hugepage".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "hugepages".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "ksm".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "transparent_hugepage" => Ok(Arc::new(SysfsTHPDir::new())),
            "hugepages" => Ok(Arc::new(SysfsEmptyDir::new("hugepages"))),
            "ksm" => Ok(Arc::new(SysfsKsmDir::new())),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/kernel/mm/transparent_hugepage/
struct SysfsTHPDir {
    ino: usize,
}

impl SysfsTHPDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsTHPDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsTHPDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "enabled".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "defrag".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "enabled" => Ok(Arc::new(SysfsWritableAttrFile::new(
                "always [madvise] never\n".to_string(),
            ))),
            "defrag" => Ok(Arc::new(SysfsWritableAttrFile::new(
                "always defer defer+madvise [madvise] never\n".to_string(),
            ))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/kernel/mm/ksm/ — kernel samepage merging knobs (CONFIG_KSM). We accept
// MADV_MERGEABLE/UNMERGEABLE as VMA marks without a merge daemon, so the
// counters legitimately stay 0 (same as a KSM-enabled kernel with run=0).
struct SysfsKsmDir {
    ino: usize,
}

impl SysfsKsmDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

const KSM_ATTRS: &[(&str, &str)] = &[
    ("run", "0\n"),
    ("pages_to_scan", "100\n"),
    ("sleep_millisecs", "20\n"),
    ("pages_shared", "0\n"),
    ("pages_sharing", "0\n"),
    ("pages_unshared", "0\n"),
    ("pages_volatile", "0\n"),
    ("full_scans", "0\n"),
    ("merge_across_nodes", "1\n"),
    ("max_page_sharing", "256\n"),
    ("stable_node_chains_prune_millisecs", "2000\n"),
    ("use_zero_pages", "0\n"),
];

impl Inode for SysfsKsmDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsKsmDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(KSM_ATTRS
            .iter()
            .map(|(name, _)| DirEntry {
                name: name.to_string(),
                file_type: FileType::Regular,
                inode: alloc_ino(),
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        for (attr, value) in KSM_ATTRS {
            if *attr == name {
                return Ok(Arc::new(SysfsWritableAttrFile::new(value.to_string())));
            }
        }
        Err(FsError::NotFound)
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// /sys/module/ — module parameters
// =============================================================================

struct SysfsModuleDir {
    ino: usize,
}

impl SysfsModuleDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsModuleDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsModuleDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        // Expose a few pseudo-modules that LTP tests may look for
        Ok(alloc::vec![
            DirEntry { name: "kernel".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "printk".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "kernel" | "printk" => Ok(Arc::new(SysfsModuleEntryDir::new(name))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// Per-module directory (has parameters/ subdir per Linux)
struct SysfsModuleEntryDir {
    ino: usize,
    #[allow(dead_code)]
    name: String,
}

impl SysfsModuleEntryDir {
    fn new(name: &str) -> Self {
        Self {
            ino: alloc_ino(),
            name: name.to_string(),
        }
    }
}

impl Inode for SysfsModuleEntryDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 3)) }
}

impl Directory for SysfsModuleEntryDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "parameters".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "parameters" => Ok(Arc::new(SysfsEmptyDir::new("params"))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// /sys/power/ — power management
// =============================================================================

struct SysfsPowerDir {
    ino: usize,
}

impl SysfsPowerDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for SysfsPowerDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for SysfsPowerDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "state".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "wakeup_count".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "disk".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "image_size".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "pm_async".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "state" => Ok(Arc::new(SysfsWritableAttrFile::new("mem\n".to_string()))),
            "wakeup_count" => Ok(Arc::new(SysfsAttrFile::new("0\n".to_string()))),
            "disk" => Ok(Arc::new(SysfsWritableAttrFile::new(
                "[platform] shutdown reboot suspend\n".to_string(),
            ))),
            "image_size" => Ok(Arc::new(SysfsWritableAttrFile::new(
                "0\n".to_string(),
            ))),
            "pm_async" => Ok(Arc::new(SysfsWritableAttrFile::new("1\n".to_string()))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _name: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// =============================================================================
// debugfs — /sys/kernel/debug/ (per Linux fs/debugfs/)
// =============================================================================
//
// debugfs is a simple RAM-based filesystem for kernel debugging. In Linux it
// is a separate fs type (mount -t debugfs none /sys/kernel/debug), but for
// LTP compatibility we embed it inside sysfs at /sys/kernel/debug/.
//
// Key subdirectories per Linux:
//   tracing/         — ftrace interface
//   sched/           — scheduler debugging
//   kprobes/         — kprobe state

struct DebugfsRootDir {
    ino: usize,
}

impl DebugfsRootDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for DebugfsRootDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for DebugfsRootDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "tracing".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "sched".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "kprobes".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "usb".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "clk".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "sleep_time".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "wakeup_sources".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "fault_around_bytes".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "tracing" => Ok(Arc::new(DebugfsTracingDir::new())),
            "sched" | "kprobes" | "usb" | "clk" => {
                Ok(Arc::new(SysfsEmptyDir::new("debugfs_sub")))
            }
            "sleep_time" => Ok(Arc::new(SysfsAttrFile::new("0\n".to_string()))),
            "wakeup_sources" => Ok(Arc::new(SysfsAttrFile::new(
                "name\t\tactive_count\tevent_count\twakeup_count\texpire_count\tactive_since\ttotal_time\tmax_time\tlast_change\tprevent_suspend_time\n".to_string(),
            ))),
            "fault_around_bytes" => {
                Ok(Arc::new(SysfsWritableAttrFile::new("65536\n".to_string())))
            }
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _n: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _n: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

// /sys/kernel/debug/tracing/ — per Linux kernel/trace/
struct DebugfsTracingDir {
    ino: usize,
}

impl DebugfsTracingDir {
    fn new() -> Self {
        Self { ino: alloc_ino() }
    }
}

impl Inode for DebugfsTracingDir {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(dir_stat(self.ino, 2)) }
}

impl Directory for DebugfsTracingDir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        Ok(alloc::vec![
            DirEntry { name: "available_tracers".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "current_tracer".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "trace".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "trace_pipe".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "tracing_on".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "buffer_size_kb".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "buffer_total_size_kb".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "trace_clock".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "trace_marker".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "set_ftrace_filter".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "set_ftrace_notrace".to_string(), file_type: FileType::Regular, inode: alloc_ino() },
            DirEntry { name: "events".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
            DirEntry { name: "per_cpu".to_string(), file_type: FileType::Directory, inode: alloc_ino() },
        ])
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        match name {
            "available_tracers" => Ok(Arc::new(SysfsAttrFile::new(
                "function_graph function nop\n".to_string(),
            ))),
            "current_tracer" => Ok(Arc::new(SysfsWritableAttrFile::new("nop\n".to_string()))),
            "trace" => Ok(Arc::new(SysfsAttrFile::new(
                "# tracer: nop\n#\n# entries-in-buffer/entries-written: 0/0   #P:1\n#\n#           TASK-PID     CPU#  |||||  TIMESTAMP  FUNCTION\n#              | |         |   |||||     |         |\n".to_string(),
            ))),
            "trace_pipe" => Ok(Arc::new(SysfsAttrFile::new(String::new()))),
            "tracing_on" => Ok(Arc::new(SysfsWritableAttrFile::new("1\n".to_string()))),
            "buffer_size_kb" => Ok(Arc::new(SysfsWritableAttrFile::new("1408\n".to_string()))),
            "buffer_total_size_kb" => Ok(Arc::new(SysfsAttrFile::new("1408\n".to_string()))),
            "trace_clock" => Ok(Arc::new(SysfsWritableAttrFile::new(
                "[local] global counter uptime perf mono mono_raw boot\n".to_string(),
            ))),
            "trace_marker" => Ok(Arc::new(SysfsWritableAttrFile::new(String::new()))),
            "set_ftrace_filter" => Ok(Arc::new(SysfsWritableAttrFile::new(String::new()))),
            "set_ftrace_notrace" => Ok(Arc::new(SysfsWritableAttrFile::new(String::new()))),
            "events" => Ok(Arc::new(SysfsEmptyDir::new("tracing_events"))),
            "per_cpu" => Ok(Arc::new(SysfsEmptyDir::new("tracing_per_cpu"))),
            _ => Err(FsError::NotFound),
        }
    }

    fn create(&self, _n: &str, _ft: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }
    fn unlink(&self, _n: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}
