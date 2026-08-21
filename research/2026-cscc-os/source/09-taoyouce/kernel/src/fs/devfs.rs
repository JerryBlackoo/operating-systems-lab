use super::{DirEntry, Directory, File, FileMode, FileStat, FileSystem, FileType, FsError, Inode};
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sync::SpinLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DeviceId {
    pub major: u32,
    pub minor: u32,
}

pub trait DeviceOps: Send + Sync {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError>;
    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError>;
    fn stat(&self, id: DeviceId) -> FileStat;
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn poll_read(&self) -> bool { true }
    fn poll_write(&self) -> bool { true }
    /// Constant IN|OUT readiness (/dev/zero, /dev/null, …): lets select/poll
    /// skip the per-fd dyn-dispatch readiness chain.
    fn poll_constant(&self) -> bool { false }
    fn ioctl(&self, _id: DeviceId, _request: usize, _arg: usize) -> Option<isize> { None }
}

struct DeviceEntry {
    id: DeviceId,
    ops: Arc<dyn DeviceOps>,
    inode: Arc<DeviceInode>,
}

static DEVICES: SpinLock<Option<BTreeMap<String, DeviceEntry>>> = SpinLock::new(None);
static NEXT_INO: AtomicUsize = AtomicUsize::new(1);
const LOOP_DEVICE_COUNT: usize = 8;

#[derive(Clone)]
struct LoopState {
    backing: Option<Arc<dyn Inode>>,
    block_size: usize,
    /// Offset within the backing file (LOOP_SET_STATUS64 lo_offset)
    offset: u64,
    /// Size limit (0 = use whole file; LOOP_SET_STATUS64 lo_sizelimit)
    sizelimit: u64,
    /// Flags (LO_FLAGS_AUTOCLEAR, LO_FLAGS_READ_ONLY, etc.)
    flags: u32,
    /// Block-device read-only flag (BLKROSET/BLKROGET, i.e. bdev->bd_read_only).
    /// Distinct from loop LO_FLAGS_READ_ONLY; mount() enforces it as EACCES.
    read_only: bool,
}

static LOOP_STATES: SpinLock<Option<Vec<LoopState>>> = SpinLock::new(None);

pub struct DevFs;

impl DevFs {
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }
}

impl FileSystem for DevFs {
    fn name(&self) -> &str {
        "devfs"
    }

    fn root(&self) -> Arc<dyn Inode> {
        Arc::new(DevRoot)
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

pub fn init() {
    let mut devices = DEVICES.lock();
    if devices.is_none() {
        *devices = Some(BTreeMap::new());
    }
    let mut loops = LOOP_STATES.lock();
    if loops.is_none() {
        let mut states = Vec::new();
        for _ in 0..LOOP_DEVICE_COUNT {
            states.push(LoopState {
                backing: None,
                block_size: crate::driver::virtio::blk::BLOCK_SIZE,
                offset: 0,
                sizelimit: 0,
                flags: 0,
                read_only: false,
            });
        }
        *loops = Some(states);
    }
}

pub fn register_builtin_devices() {
    init();
    pty_init();
    let _ = register("null", DeviceId { major: 1, minor: 3 }, Arc::new(NullDevice));
    let _ = register("zero", DeviceId { major: 1, minor: 5 }, Arc::new(ZeroDevice));
    let random = Arc::new(RandomDevice);
    let _ = register("random", DeviceId { major: 1, minor: 8 }, random.clone());
    let _ = register("urandom", DeviceId { major: 1, minor: 9 }, random);
    let _ = register("ptmx", DeviceId { major: 5, minor: 2 }, Arc::new(PtmxDevice));
    let _ = register("misc/rtc", DeviceId { major: 10, minor: 135 }, Arc::new(NullDevice));
    let _ = register("loop-control", DeviceId { major: 10, minor: 237 }, Arc::new(LoopControlDevice));
    for index in 0..LOOP_DEVICE_COUNT {
        let name = alloc::format!("loop{}", index);
        let _ = register(&name, DeviceId { major: 7, minor: index as u32 }, Arc::new(LoopBlockDevice { index }));
    }
    for index in 0..crate::driver::virtio::blk::device_count() {
        let name = alloc::format!("vd{}", (b'a' + index as u8) as char);
        let _ = register(&name, DeviceId { major: 254, minor: index as u32 }, Arc::new(VirtioBlockDevice { index }));
    }
}

pub fn mount_devfs() {
    init();
    super::mount("/dev", DevFs::new());
}

pub fn lookup_path(path: &str) -> Result<Arc<dyn Inode>, FsError> {
    let trimmed = path.trim_matches('/');
    if trimmed == "dev" {
        return Ok(Arc::new(DevRoot));
    }
    if let Some(name) = trimmed.strip_prefix("dev/") {
        let root = DevRoot;
        return root.lookup(name);
    }
    Err(FsError::NotFound)
}

pub fn register(path: &str, id: DeviceId, ops: Arc<dyn DeviceOps>) -> Result<(), FsError> {
    let normalized = normalize(path)?;
    let mut devices = DEVICES.lock();
    let registry = devices.as_mut().ok_or(FsError::InvalidArgument)?;
    if registry.contains_key(&normalized) {
        return Err(FsError::AlreadyExists);
    }

    let inode = Arc::new(DeviceInode {
        ino: NEXT_INO.fetch_add(1, Ordering::Relaxed),
        name: normalized.clone(),
        id,
        ops: ops.clone(),
    });
    registry.insert(normalized, DeviceEntry { id, ops, inode });
    Ok(())
}

fn normalize(path: &str) -> Result<String, FsError> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() || trimmed.split('/').any(|part| part.is_empty()) {
        return Err(FsError::InvalidArgument);
    }
    Ok(trimmed.to_string())
}

struct DevRoot;

impl Inode for DevRoot {
    fn ino(&self) -> usize { 0 }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Directory,
            size: 0,
            mode: FileMode(0o040755),
            nlink: 2,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}

impl Directory for DevRoot {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let devices = DEVICES.lock();
        let registry = devices.as_ref().ok_or(FsError::NotFound)?;
        let mut entries: BTreeMap<String, DirEntry> = BTreeMap::new();
        for (name, entry) in registry.iter() {
            if let Some((dir, _)) = name.split_once('/') {
                entries.entry(dir.to_string()).or_insert(DirEntry {
                    name: dir.to_string(),
                    file_type: FileType::Directory,
                    inode: 0,
                });
            } else {
                entries.insert(name.clone(), DirEntry {
                    name: name.clone(),
                    file_type: entry.ops.file_type(),
                    inode: entry.inode.ino,
                });
            }
        }
        Ok(entries.into_values().collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        let devices = DEVICES.lock();
        let registry = devices.as_ref().ok_or(FsError::NotFound)?;
        if let Some(entry) = registry.get(name) {
            return Ok(entry.inode.clone());
        }
        if registry.keys().any(|key| key.starts_with(name) && key.as_bytes().get(name.len()) == Some(&b'/')) {
            return Ok(Arc::new(DevSubdir { prefix: name.to_string() }));
        }
        Err(FsError::NotFound)
    }

    fn create(&self, _name: &str, _file_type: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }

    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

struct DevSubdir {
    prefix: String,
}

impl Inode for DevSubdir {
    fn ino(&self) -> usize { 0 }
    fn file_type(&self) -> FileType { FileType::Directory }
    fn as_file(&self) -> Option<&dyn File> { None }
    fn as_dir(&self) -> Option<&dyn Directory> { Some(self) }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Directory,
            size: 0,
            mode: FileMode(0o040755),
            nlink: 2,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}

impl Directory for DevSubdir {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let devices = DEVICES.lock();
        let registry = devices.as_ref().ok_or(FsError::NotFound)?;
        let prefix = alloc::format!("{}/", self.prefix);
        Ok(registry.iter()
            .filter_map(|(name, entry)| {
                name.strip_prefix(&prefix).and_then(|rest| {
                    if rest.contains('/') {
                        None
                    } else {
                        Some(DirEntry {
                            name: rest.to_string(),
                            file_type: entry.ops.file_type(),
                            inode: entry.inode.ino,
                        })
                    }
                })
            })
            .collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        let full = alloc::format!("{}/{}", self.prefix, name);
        let devices = DEVICES.lock();
        let registry = devices.as_ref().ok_or(FsError::NotFound)?;
        if let Some(entry) = registry.get(&full) {
            let inode: Arc<dyn Inode> = entry.inode.clone();
            Ok(inode)
        } else {
            Err(FsError::NotFound)
        }
    }

    fn create(&self, _name: &str, _file_type: FileType) -> Result<Arc<dyn Inode>, FsError> {
        Err(FsError::PermissionDenied)
    }

    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

struct DeviceInode {
    ino: usize,
    name: String,
    id: DeviceId,
    ops: Arc<dyn DeviceOps>,
}

impl Inode for DeviceInode {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { self.ops.file_type() }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { Ok(self.ops.stat(self.id)) }
    fn device_id(&self) -> Option<(u32, u32)> { Some((self.id.major, self.id.minor)) }
    fn poll_always_ready(&self) -> bool { self.ops.poll_constant() }
}

impl File for DeviceInode {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        self.ops.read(offset, buf)
    }

    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        self.ops.write(offset, buf)
    }

    fn truncate(&self, _length: usize) -> Result<(), FsError> {
        Ok(())
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(self.ops.stat(self.id))
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }

    fn poll_read(&self) -> bool {
        self.ops.poll_read()
    }

    fn poll_write(&self) -> bool {
        self.ops.poll_write()
    }

    fn ioctl(&self, request: usize, arg: usize) -> Option<isize> {
        self.ops.ioctl(self.id, request, arg)
    }
}

struct NullDevice;

impl DeviceOps for NullDevice {
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, FsError> { Ok(0) }
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> { Ok(buf.len()) }
    fn stat(&self, _id: DeviceId) -> FileStat { char_stat() }
    fn poll_read(&self) -> bool { true }
    fn poll_write(&self) -> bool { true }
    fn poll_constant(&self) -> bool { true }
}

struct ZeroDevice;

impl DeviceOps for ZeroDevice {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        for byte in buf.iter_mut() {
            *byte = 0;
        }
        Ok(buf.len())
    }

    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> { Ok(buf.len()) }
    fn stat(&self, _id: DeviceId) -> FileStat { char_stat() }
    fn poll_read(&self) -> bool { true }
    fn poll_write(&self) -> bool { true }
    fn poll_constant(&self) -> bool { true }
}

struct RandomDevice;

impl DeviceOps for RandomDevice {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        crate::random::fill_bytes(buf);
        Ok(buf.len())
    }

    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        let mut seed = 0u64;
        for (index, byte) in buf.iter().take(8).enumerate() {
            seed |= (*byte as u64) << (index * 8);
        }
        crate::random::reseed(seed ^ (buf.len() as u64));
        Ok(buf.len())
    }

    fn stat(&self, _id: DeviceId) -> FileStat { char_stat() }
    fn poll_read(&self) -> bool { true }
    fn poll_write(&self) -> bool { true }

    fn ioctl(&self, _id: DeviceId, request: usize, arg: usize) -> Option<isize> {
        // RNDGETENTCNT = _IOR('R', 0, int) — return the available entropy count.
        // LTP ioctl07 compares this against /proc/sys/kernel/random/entropy_avail
        // (both report 256 here) within a small fuzz factor.
        const RNDGETENTCNT: usize = 0x8004_5200;
        match request {
            RNDGETENTCNT => {
                if arg == 0 {
                    return Some(-14);
                }
                let cnt: i32 = 256;
                let _ = crate::syscall::user::UserSlice::<u8>::new_mut(arg as *mut u8, 4)
                    .write_bytes(&cnt.to_le_bytes());
                Some(0)
            }
            _ => None,
        }
    }
}

struct LoopControlDevice;

impl DeviceOps for LoopControlDevice {
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, FsError> { Ok(0) }
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> { Ok(buf.len()) }
    fn stat(&self, _id: DeviceId) -> FileStat { char_stat() }
    fn ioctl(&self, _id: DeviceId, request: usize, _arg: usize) -> Option<isize> {
        const LOOP_CTL_ADD: usize = 0x4c80;
        const LOOP_CTL_REMOVE: usize = 0x4c81;
        const LOOP_CTL_GET_FREE: usize = 0x4c82;
        match request {
            LOOP_CTL_GET_FREE => {
                let states = LOOP_STATES.lock();
                let states = states.as_ref()?;
                for (index, state) in states.iter().enumerate() {
                    if state.backing.is_none() {
                        return Some(index as isize);
                    }
                }
                Some(-28) // -ENOSPC
            }
            LOOP_CTL_ADD => Some(0),
            LOOP_CTL_REMOVE => {
                let mut states = LOOP_STATES.lock();
                let states = states.as_mut()?;
                let index = _arg;
                if index >= states.len() {
                    return Some(-19); // -ENODEV
                }
                states[index].backing = None;
                Some(0)
            }
            _ => None,
        }
    }
}

struct LoopBlockDevice {
    index: usize,
}

impl LoopBlockDevice {
    fn backing(&self) -> Option<Arc<dyn Inode>> {
        let states = LOOP_STATES.lock();
        states
            .as_ref()
            .and_then(|states| states.get(self.index))
            .and_then(|state| state.backing.clone())
    }

    fn attach_backing(&self, backing_fd: usize) -> isize {
        let backing = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let Some(table) = table.as_ref() else { return -9; };
            let Some(pid) = table.current else { return -9; };
            let Some(process) = table.get_fd_process(pid) else { return -9; };
            let Some(fd_obj) = crate::fs::fd::get(process, backing_fd) else { return -9; };
            fd_obj.file.clone()
        };
        if backing.as_file().is_none() {
            return -22;
        }
        let mut states = LOOP_STATES.lock();
        let Some(states) = states.as_mut() else { return -19; };
        let Some(state) = states.get_mut(self.index) else { return -19; };
        if state.backing.is_some() {
            return -16; // -EBUSY
        }
        state.backing = Some(backing);
        0
    }

    fn detach(&self) -> isize {
        let mut states = LOOP_STATES.lock();
        let Some(states) = states.as_mut() else { return -19; };
        let Some(state) = states.get_mut(self.index) else { return -19; };
        if state.backing.is_none() {
            return -6; // -ENXIO
        }
        state.backing = None;
        0
    }

    fn set_block_size(&self, arg: usize) -> isize {
        let valid = matches!(arg, 512 | 1024 | 2048 | 4096);
        if !valid {
            return -22;
        }
        let mut states = LOOP_STATES.lock();
        let Some(states) = states.as_mut() else { return -19; };
        let Some(state) = states.get_mut(self.index) else { return -19; };
        state.block_size = arg;
        0
    }

    fn size(&self) -> usize {
        self.backing()
            .and_then(|inode| inode.stat().ok().map(|stat| stat.size))
            .unwrap_or(0)
    }

    fn status_ioctl(&self, arg: usize, is_64: bool) -> isize {
        if self.backing().is_none() {
            return -6; // -ENXIO means free loop device.
        }
        if arg == 0 {
            return -14; // EFAULT
        }
        let states = LOOP_STATES.lock();
        let Some(states) = states.as_ref() else { return -19; };
        let Some(state) = states.get(self.index) else { return -19; };

        if is_64 {
            // struct loop_info64 (232 bytes):
            //   u64 lo_device;        // offset 0
            //   u64 lo_inode;         // offset 8
            //   u64 lo_rdevice;       // offset 16
            //   u64 lo_offset;        // offset 24
            //   u64 lo_sizelimit;     // offset 32
            //   u32 lo_number;        // offset 40
            //   u32 lo_encrypt_type;  // offset 44
            //   u32 lo_encrypt_key_size; // offset 48
            //   u32 lo_flags;         // offset 52
            //   u8  lo_file_name[64]; // offset 56
            //   u8  lo_crypt_name[64]; // offset 120
            //   u8  lo_encrypt_key[32]; // offset 184
            //   u64 lo_init[2];       // offset 216
            let mut buf = [0u8; 232];
            // lo_device = 7 (major for loop)
            buf[0..8].copy_from_slice(&7u64.to_le_bytes());
            // lo_inode = 0
            // lo_rdevice = makedev(7, index)
            let rdev = ((7u64) << 8) | (self.index as u64);
            buf[16..24].copy_from_slice(&rdev.to_le_bytes());
            // lo_offset
            buf[24..32].copy_from_slice(&state.offset.to_le_bytes());
            // lo_sizelimit
            buf[32..40].copy_from_slice(&state.sizelimit.to_le_bytes());
            // lo_number
            buf[40..44].copy_from_slice(&(self.index as u32).to_le_bytes());
            // lo_flags
            buf[52..56].copy_from_slice(&state.flags.to_le_bytes());

            let _ = crate::syscall::user::UserSlice::<u8>::new_mut(
                arg as *mut u8, 232
            ).write_bytes(&buf);
        } else {
            // struct loop_info (legacy, 140 bytes) — zero-filled + minimal fields
            let mut buf = [0u8; 140];
            // lo_number at offset 0 (i32)
            buf[0..4].copy_from_slice(&(self.index as i32).to_le_bytes());
            // lo_flags at offset 12 (i32)
            buf[12..16].copy_from_slice(&(state.flags as i32).to_le_bytes());

            let _ = crate::syscall::user::UserSlice::<u8>::new_mut(
                arg as *mut u8, 140
            ).write_bytes(&buf);
        }
        0
    }

    fn set_status64(&self, arg: usize) -> isize {
        if arg == 0 {
            return -14;
        }
        if self.backing().is_none() {
            return -6;
        }
        // Read lo_offset (offset 24) and lo_sizelimit (offset 32) and lo_flags (offset 52)
        let data = match crate::syscall::user::UserSlice::<u8>::new(arg as *const u8, 232).read_bytes() {
            Ok(d) => d,
            Err(_) => return -14,
        };
        let lo_offset = u64::from_le_bytes(data[24..32].try_into().unwrap_or([0;8]));
        let lo_sizelimit = u64::from_le_bytes(data[32..40].try_into().unwrap_or([0;8]));
        let lo_flags = u32::from_le_bytes(data[52..56].try_into().unwrap_or([0;4]));

        let mut states = LOOP_STATES.lock();
        let Some(states) = states.as_mut() else { return -19; };
        let Some(state) = states.get_mut(self.index) else { return -19; };
        state.offset = lo_offset;
        state.sizelimit = lo_sizelimit;
        state.flags = lo_flags;
        0
    }
}

impl DeviceOps for LoopBlockDevice {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let Some(backing) = self.backing() else {
            return Err(FsError::IoError);
        };
        let Some(file) = backing.as_file() else {
            return Err(FsError::InvalidArgument);
        };
        // Apply loop device offset (lo_offset from LOOP_SET_STATUS64)
        let lo_offset = {
            let states = LOOP_STATES.lock();
            states.as_ref()
                .and_then(|s| s.get(self.index))
                .map(|s| s.offset as usize)
                .unwrap_or(0)
        };
        file.read(offset + lo_offset, buf)
    }

    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        let Some(backing) = self.backing() else {
            return Err(FsError::IoError);
        };
        let Some(file) = backing.as_file() else {
            return Err(FsError::InvalidArgument);
        };
        let lo_offset = {
            let states = LOOP_STATES.lock();
            states.as_ref()
                .and_then(|s| s.get(self.index))
                .map(|s| s.offset as usize)
                .unwrap_or(0)
        };
        file.write(offset + lo_offset, buf)
    }

    fn stat(&self, _id: DeviceId) -> FileStat {
        FileStat {
            file_type: FileType::BlockDevice,
            size: self.size(),
            mode: FileMode(0o060660),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        }
    }

    fn file_type(&self) -> FileType { FileType::BlockDevice }
    fn poll_read(&self) -> bool { true }
    fn poll_write(&self) -> bool { true }

    fn ioctl(&self, _id: DeviceId, request: usize, arg: usize) -> Option<isize> {
        const LOOP_SET_FD: usize = 0x4c00;
        const LOOP_CLR_FD: usize = 0x4c01;
        const LOOP_SET_STATUS: usize = 0x4c02;
        const LOOP_GET_STATUS: usize = 0x4c03;
        const LOOP_SET_STATUS64: usize = 0x4c04;
        const LOOP_GET_STATUS64: usize = 0x4c05;
        const LOOP_SET_CAPACITY: usize = 0x4c07;
        const LOOP_SET_BLOCK_SIZE: usize = 0x4c09;
        const BLKROSET: usize = 0x125d;
        const BLKROGET: usize = 0x125e;
        match request {
            BLKROSET => {
                if arg == 0 { return Some(-14); }
                let val = match crate::syscall::user::UserSlice::<u8>::new(arg as *const u8, 4).read_bytes() {
                    Ok(d) => i32::from_le_bytes([d[0], d[1], d[2], d[3]]),
                    Err(_) => return Some(-14),
                };
                let mut states = LOOP_STATES.lock();
                if let Some(s) = states.as_mut().and_then(|v| v.get_mut(self.index)) {
                    s.read_only = val != 0;
                }
                Some(0)
            }
            BLKROGET => {
                if arg == 0 { return Some(-14); }
                let ro = {
                    let states = LOOP_STATES.lock();
                    states.as_ref().and_then(|v| v.get(self.index)).map(|s| s.read_only).unwrap_or(false)
                };
                let v: i32 = if ro { 1 } else { 0 };
                let _ = crate::syscall::user::UserSlice::<u8>::new_mut(arg as *mut u8, 4)
                    .write_bytes(&v.to_le_bytes());
                Some(0)
            }
            LOOP_SET_FD => Some(self.attach_backing(arg)),
            LOOP_CLR_FD => Some(self.detach()),
            LOOP_SET_STATUS => {
                if self.backing().is_none() { Some(-6) } else { Some(0) }
            }
            LOOP_SET_STATUS64 => Some(self.set_status64(arg)),
            LOOP_GET_STATUS => Some(self.status_ioctl(arg, false)),
            LOOP_GET_STATUS64 => Some(self.status_ioctl(arg, true)),
            LOOP_SET_CAPACITY => Some(if self.backing().is_some() { 0 } else { -6 }),
            LOOP_SET_BLOCK_SIZE => Some(self.set_block_size(arg)),
            _ => None,
        }
    }
}

/// 查询 loop 设备的块层只读标志(BLKROSET 设置)。mount() 据此对只读设备
/// 的可写挂载返回 EACCES，对标 Linux `sb_rdonly`/`bdev_read_only`。
pub fn loop_device_read_only(index: usize) -> bool {
    let states = LOOP_STATES.lock();
    states.as_ref().and_then(|v| v.get(index)).map(|s| s.read_only).unwrap_or(false)
}

struct VirtioBlockDevice {
    index: usize,
}

impl DeviceOps for VirtioBlockDevice {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let capacity = crate::driver::virtio::blk::capacity_sectors_dev(self.index)
            .ok_or(FsError::NotFound)? as usize * crate::driver::virtio::blk::BLOCK_SIZE;
        if offset >= capacity {
            return Ok(0);
        }
        let len = buf.len().min(capacity - offset);
        if len == 0 {
            return Ok(0);
        }
        let block_size = crate::driver::virtio::blk::BLOCK_SIZE;
        let first_sector = offset / block_size;
        let sector_offset = offset % block_size;
        let sector_count = (sector_offset + len + block_size - 1) / block_size;
        let data = crate::driver::virtio::blk::read_sectors_dev(self.index, first_sector as u64, sector_count)
            .map_err(|_| FsError::IoError)?;
        buf[..len].copy_from_slice(&data[sector_offset..sector_offset + len]);
        Ok(len)
    }

    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        let capacity = crate::driver::virtio::blk::capacity_sectors_dev(self.index)
            .ok_or(FsError::NotFound)? as usize * crate::driver::virtio::blk::BLOCK_SIZE;
        if offset >= capacity {
            return Err(FsError::NoSpace);
        }
        let len = buf.len().min(capacity - offset);
        if len == 0 {
            return Ok(0);
        }
        let block_size = crate::driver::virtio::blk::BLOCK_SIZE;
        let first_sector = offset / block_size;
        let sector_offset = offset % block_size;
        let sector_count = (sector_offset + len + block_size - 1) / block_size;
        let mut data = if sector_offset == 0 && len == sector_count * block_size {
            buf[..len].to_vec()
        } else {
            crate::driver::virtio::blk::read_sectors_dev(self.index, first_sector as u64, sector_count)
                .map_err(|_| FsError::IoError)?
        };
        data[sector_offset..sector_offset + len].copy_from_slice(&buf[..len]);
        crate::driver::virtio::blk::write_sectors_dev(self.index, first_sector as u64, data)
            .map_err(|_| FsError::IoError)?;
        Ok(len)
    }

    fn stat(&self, _id: DeviceId) -> FileStat {
        let size = crate::driver::virtio::blk::capacity_sectors_dev(self.index)
            .unwrap_or(0) as usize * crate::driver::virtio::blk::BLOCK_SIZE;
        FileStat {
            file_type: FileType::BlockDevice,
            size,
            mode: FileMode(0o060660),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        }
    }

    fn file_type(&self) -> FileType { FileType::BlockDevice }
    fn poll_read(&self) -> bool { true }
    fn poll_write(&self) -> bool { true }
}

// =========================================================================
// PTY subsystem — /dev/ptmx (master) + /dev/pts/N (slave)
// =========================================================================
//
// Linux PTY model:
//  - open("/dev/ptmx") → allocate a new PTY pair, return master fd
//  - ioctl(master_fd, TIOCGPTN, &n) → get slave index n
//  - ioctl(master_fd, TIOCSPTLCK, &0) → unlock slave
//  - open("/dev/pts/N") → slave fd
//  - master write → slave read, slave write → master read (bidirectional pipe)

use alloc::collections::VecDeque;

/// Per-PTY pair state shared between master and slave.
struct PtyPairInner {
    /// Buffer for master→slave direction (master writes, slave reads)
    m2s: VecDeque<u8>,
    /// Buffer for slave→master direction (slave writes, master reads)
    s2m: VecDeque<u8>,
    /// Slave locked? (must be unlocked via TIOCSPTLCK before slave can be opened)
    slave_locked: bool,
    /// Is master side still open?
    master_open: bool,
    /// Number of currently-open slave file objects (fork/dup share one).
    slave_count: usize,
    /// Has the slave ever been opened? Master read returns EOF only after
    /// the slave was opened at least once and all opens are closed (Linux
    /// tty hangup semantics); before first open it must block, not EOF.
    slave_ever_opened: bool,
    /// termios settings (simplified)
    termios: PtyTermios,
    /// Window size
    winsize: PtyWinsize,
}

#[derive(Clone, Copy)]
struct PtyTermios {
    c_iflag: u32,
    c_oflag: u32,
    c_cflag: u32,
    c_lflag: u32,
    c_cc: [u8; 19],
    c_ispeed: u32,
    c_ospeed: u32,
}

impl Default for PtyTermios {
    fn default() -> Self {
        let mut c_cc = [0u8; 19];
        // Linux defaults for c_cc
        c_cc[0] = 0x03;  // VINTR = ^C
        c_cc[1] = 0x1c;  // VQUIT = ^\
        c_cc[2] = 0x7f;  // VERASE = DEL
        c_cc[3] = 0x15;  // VKILL = ^U
        c_cc[4] = 0x04;  // VEOF = ^D
        c_cc[5] = 0;     // VTIME
        c_cc[6] = 1;     // VMIN
        c_cc[7] = 0;     // VSWTC
        c_cc[8] = 0x11;  // VSTART = ^Q
        c_cc[9] = 0x13;  // VSTOP = ^S
        c_cc[10] = 0x1a; // VSUSP = ^Z
        c_cc[11] = 0;    // VEOL
        c_cc[12] = 0x12; // VREPRINT = ^R
        c_cc[13] = 0x0f; // VDISCARD = ^O
        c_cc[14] = 0x17; // VWERASE = ^W
        c_cc[15] = 0x16; // VLNEXT = ^V
        c_cc[16] = 0;    // VEOL2
        c_cc[17] = 0;
        c_cc[18] = 0;
        PtyTermios {
            c_iflag: 0o00500, // ICRNL | ...
            c_oflag: 0o00005, // OPOST | ONLCR
            c_cflag: 0o00277, // B38400 | CS8 | CREAD
            c_lflag: 0o105073, // ISIG | ICANON | ECHO | ECHOE | ECHOK | ECHOCTL | ECHOKE | IEXTEN
            c_cc,
            c_ispeed: 38400,
            c_ospeed: 38400,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct PtyWinsize {
    ws_row: u16,
    ws_col: u16,
    ws_xpixel: u16,
    ws_ypixel: u16,
}

const PTY_BUF_SIZE: usize = 4096;
const MAX_PTYS: usize = 256;

/// Global PTY pair table. Index = PTY number.
static PTY_TABLE: SpinLock<Option<Vec<Option<Arc<SpinLock<PtyPairInner>>>>>> = SpinLock::new(None);

fn pty_init() {
    let mut table = PTY_TABLE.lock();
    if table.is_none() {
        let mut v = Vec::with_capacity(MAX_PTYS);
        for _ in 0..MAX_PTYS {
            v.push(None);
        }
        *table = Some(v);
    }
}

/// Allocate a new PTY pair. Returns the PTY index.
fn pty_alloc() -> Result<usize, FsError> {
    pty_init();
    let mut table = PTY_TABLE.lock();
    let slots = table.as_mut().ok_or(FsError::InvalidArgument)?;
    for (i, slot) in slots.iter_mut().enumerate() {
        if slot.is_none() {
            let pair = PtyPairInner {
                m2s: VecDeque::with_capacity(PTY_BUF_SIZE),
                s2m: VecDeque::with_capacity(PTY_BUF_SIZE),
                slave_locked: true, // locked until TIOCSPTLCK(0)
                master_open: true,
                slave_count: 0,
                slave_ever_opened: false,
                termios: PtyTermios::default(),
                winsize: PtyWinsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 },
            };
            *slot = Some(Arc::new(SpinLock::new(pair)));
            // Register the slave device at /dev/pts/N
            let name = alloc::format!("pts/{}", i);
            let slave_ops = Arc::new(PtySlaveOps { index: i });
            let _ = register(
                &name,
                DeviceId { major: 136, minor: i as u32 },
                slave_ops,
            );
            return Ok(i);
        }
    }
    Err(FsError::NoSpace)
}

fn pty_get(index: usize) -> Option<Arc<SpinLock<PtyPairInner>>> {
    let table = PTY_TABLE.lock();
    table.as_ref()?.get(index)?.clone()
}

/// /dev/ptmx — opening allocates a new PTY pair; the returned "file" is the master side.
/// The open() call in devfs doesn't create an fd directly, so we handle this
/// in the syscall layer via a special hook for /dev/ptmx.
pub struct PtmxDevice;

impl DeviceOps for PtmxDevice {
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, FsError> {
        // Reads on ptmx inode itself are not valid; the master fd is separate.
        Err(FsError::InvalidArgument)
    }
    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }
    fn stat(&self, _id: DeviceId) -> FileStat {
        FileStat {
            file_type: FileType::CharDevice,
            size: 0,
            mode: FileMode(0o020666),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        }
    }
}

/// Master-side PTY file object (one per open of /dev/ptmx).
pub struct PtyMasterFile {
    pub index: usize,
}

impl Drop for PtyMasterFile {
    fn drop(&mut self) {
        // Linux：关闭 master 即挂断该 PTY（slave 后续读到 EOF / 写得 EIO）。必须
        // 回收 PTY_TABLE 槽位，否则每 open 一个 pty 泄漏一个槽 —— cve-2014-0196
        // 反复 openpty()/close() 喷射数百个 pty，很快耗尽 256 槽 → openpty 得
        // ENOSPC → TBROK。槽位置空后 index 可被后续 pty_alloc 复用（/dev/pts/N
        // 设备项在复用时由 register 覆盖；未复用前 pty_get 返回 None，slave 操作
        // 自然得 EIO，符合 Linux 主控端挂断语义）。
        let mut table = PTY_TABLE.lock();
        if let Some(slots) = table.as_mut() {
            if let Some(slot) = slots.get_mut(self.index) {
                if let Some(pair) = slot.as_ref() {
                    pair.lock().master_open = false;
                }
                *slot = None;
            }
        }
    }
}

impl PtyMasterFile {
    pub fn read_data(&self, buf: &mut [u8]) -> Result<usize, FsError> {
        let pair = pty_get(self.index).ok_or(FsError::IoError)?;
        let mut inner = pair.lock();
        if inner.s2m.is_empty() {
            if inner.slave_ever_opened && inner.slave_count == 0 {
                return Ok(0); // EOF: all slave opens closed (hangup)
            }
            return Err(FsError::WouldBlock);
        }
        let len = buf.len().min(inner.s2m.len());
        for i in 0..len {
            buf[i] = inner.s2m.pop_front().unwrap();
        }
        Ok(len)
    }

    pub fn write_data(&self, buf: &[u8]) -> Result<usize, FsError> {
        let pair = pty_get(self.index).ok_or(FsError::IoError)?;
        let mut inner = pair.lock();
        // Input processing per Linux n_tty line discipline:
        // ICRNL: map CR → NL on input
        // INLCR: map NL → CR on input
        // IGNCR: ignore CR on input
        // ICANON: canonical mode (line buffering, VERASE/VKILL/VEOF)
        // ISIG: enable signal generation (VINTR/VQUIT/VSUSP)
        // ECHO: echo input characters back to output
        const ICRNL: u32 = 0o000400;
        const INLCR: u32 = 0o000100;
        const IGNCR: u32 = 0o000200;
        const ISIG:  u32 = 0o000001;
        const ECHO:  u32 = 0o000010;
        let icrnl = (inner.termios.c_iflag & ICRNL) != 0;
        let inlcr = (inner.termios.c_iflag & INLCR) != 0;
        let igncr = (inner.termios.c_iflag & IGNCR) != 0;
        let isig  = (inner.termios.c_lflag & ISIG)  != 0;
        let echo  = (inner.termios.c_lflag & ECHO)  != 0;
        let vintr = inner.termios.c_cc[0];  // VINTR
        let vquit = inner.termios.c_cc[1];  // VQUIT
        let vsusp = inner.termios.c_cc[10]; // VSUSP
        for &b in buf {
            if inner.m2s.len() >= PTY_BUF_SIZE { break; }
            // Signal character processing
            if isig && b != 0 {
                let sig = if b == vintr { Some(2) }       // SIGINT
                    else if b == vquit { Some(3) }        // SIGQUIT
                    else if b == vsusp { Some(20) }       // SIGTSTP
                    else { None };
                if let Some(sig) = sig {
                    // Flush input/output per Linux
                    inner.m2s.clear();
                    inner.s2m.clear();
                    // Echo the char if ECHO set
                    if echo {
                        inner.s2m.push_back(b'^');
                        inner.s2m.push_back(b'@' + b);
                    }
                    drop(inner);
                    // Send signal to foreground process group
                    let pgid = {
                        let table = crate::sched::process::PROCESS_TABLE.lock();
                        table.as_ref().and_then(|t| t.current.and_then(|pid|
                            t.get(pid).map(|p| p.pgid)
                        ))
                    };
                    if let Some(pgid) = pgid {
                        let _ = crate::kernel::signal_syscall::sys_kill(
                            -(pgid as isize), sig,
                        );
                    }
                    return Ok(buf.len());
                }
            }
            // Input character mapping
            let ch = match b {
                b'\r' if igncr => continue,
                b'\r' if icrnl => b'\n',
                b'\n' if inlcr => b'\r',
                _ => b,
            };
            inner.m2s.push_back(ch);
            // Echo to master read side
            if echo {
                if inner.s2m.len() < PTY_BUF_SIZE {
                    inner.s2m.push_back(ch);
                }
            }
        }
        Ok(buf.len())
    }

    pub fn poll_read_ready(&self) -> bool {
        if let Some(pair) = pty_get(self.index) {
            let inner = pair.lock();
            !inner.s2m.is_empty() || (inner.slave_ever_opened && inner.slave_count == 0)
        } else {
            true
        }
    }

    pub fn poll_write_ready(&self) -> bool {
        true
    }

    pub fn ioctl_handler(&self, request: usize, arg: usize) -> Option<isize> {
        pty_ioctl(self.index, true, request, arg)
    }
}

/// Slave-side PTY device ops (registered at /dev/pts/N).
struct PtySlaveOps {
    index: usize,
}

impl DeviceOps for PtySlaveOps {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let pair = pty_get(self.index).ok_or(FsError::IoError)?;
        let mut inner = pair.lock();
        if !inner.master_open {
            return Err(FsError::IoError); // EIO when master closed
        }
        if inner.m2s.is_empty() {
            return Err(FsError::WouldBlock);
        }
        let len = buf.len().min(inner.m2s.len());
        for i in 0..len {
            buf[i] = inner.m2s.pop_front().unwrap();
        }
        Ok(len)
    }

    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        let pair = pty_get(self.index).ok_or(FsError::IoError)?;
        let mut inner = pair.lock();
        if !inner.master_open {
            return Err(FsError::IoError); // EIO when master closed
        }
        // Output processing per Linux n_tty line discipline:
        // OPOST: enable output processing
        // ONLCR: map NL → CR+NL on output
        // OCRNL: map CR → NL on output
        // ONOCR: don't output CR at column 0
        // ONLRET: NL performs CR function
        const OPOST: u32 = 0o000001;
        const ONLCR: u32 = 0o000004;
        const OCRNL: u32 = 0o000010;
        let opost = (inner.termios.c_oflag & OPOST) != 0;
        let onlcr = (inner.termios.c_oflag & ONLCR) != 0;
        let ocrnl = (inner.termios.c_oflag & OCRNL) != 0;
        for &b in buf {
            if inner.s2m.len() >= PTY_BUF_SIZE { break; }
            if opost {
                match b {
                    b'\n' if onlcr => {
                        if inner.s2m.len() + 1 < PTY_BUF_SIZE {
                            inner.s2m.push_back(b'\r');
                        }
                        inner.s2m.push_back(b'\n');
                    }
                    b'\r' if ocrnl => {
                        inner.s2m.push_back(b'\n');
                    }
                    _ => {
                        inner.s2m.push_back(b);
                    }
                }
            } else {
                inner.s2m.push_back(b);
            }
        }
        Ok(buf.len())
    }

    fn stat(&self, _id: DeviceId) -> FileStat {
        FileStat {
            file_type: FileType::CharDevice,
            size: 0,
            mode: FileMode(0o020620), // crw--w---- (typical slave perms)
            nlink: 1,
            uid: 0,
            gid: 5, // tty group
            atime: 0,
            mtime: 0,
            ctime: 0,
        }
    }

    fn poll_read(&self) -> bool {
        if let Some(pair) = pty_get(self.index) {
            let inner = pair.lock();
            !inner.m2s.is_empty() || !inner.master_open
        } else {
            true
        }
    }

    fn poll_write(&self) -> bool { true }

    fn ioctl(&self, _id: DeviceId, request: usize, arg: usize) -> Option<isize> {
        pty_ioctl(self.index, false, request, arg)
    }
}

/// Common ioctl handler for both master and slave sides.
///
/// Per Linux drivers/tty/pty.c + tty_ioctl.c: the PTY ioctl dispatch
/// handles termios (TCGETS/TCSETS/TCGETS2/TCSETS2), window size
/// (TIOCGWINSZ/TIOCSWINSZ with SIGWINCH delivery), PTY allocation
/// (TIOCGPTN/TIOCSPTLCK), flow control (TCFLSH/TCSBRK/TCXONC),
/// and session management (TIOCGPGRP/TIOCSPGRP/TIOCGSID/TIOCSCTTY).
fn pty_ioctl(index: usize, _is_master: bool, request: usize, arg: usize) -> Option<isize> {
    const TCGETS: usize = 0x5401;
    const TCSETS: usize = 0x5402;
    const TCSETSW: usize = 0x5403;
    const TCSETSF: usize = 0x5404;
    const TCFLSH: usize = 0x540b;
    const TCSBRK: usize = 0x5409;
    const TCSBRKP: usize = 0x5425;
    const TCXONC: usize = 0x540a;
    const TCGETS2: usize = 0x802c_542a;
    const TCSETS2: usize = 0x402c_542b;
    const TCSETSW2: usize = 0x402c_542c;
    const TCSETSF2: usize = 0x402c_542d;
    const TIOCGPTN: usize = 0x80045430;
    const TIOCSPTLCK: usize = 0x40045431;
    const TIOCGPTLCK: usize = 0x80045439;
    const TIOCSWINSZ: usize = 0x5414;
    const TIOCGWINSZ: usize = 0x5413;
    const TIOCSPGRP: usize = 0x5410;
    const TIOCGPGRP: usize = 0x540f;
    const TIOCGSID: usize = 0x5429;
    const TIOCNOTTY: usize = 0x5422;
    const TIOCSCTTY: usize = 0x540e;
    const FIONREAD: usize = 0x541b;
    const TIOCOUTQ: usize = 0x5411;
    const TIOCGPTPEER: usize = 0x5441;
    const TIOCPKT: usize = 0x5420;

    let pair = pty_get(index)?;

    // 用户指针访问必须经 UserSlice（translate→内核物理拷贝），切勿裸 core::ptr::
    // read/write/copy：内核态未置 sstatus.SUM 位，直接解引用用户虚址会在监督态触发
    // load/store 缺页，而该页本已映射 → 缺页处理无事可做 → 返回原指令重执 → 再次缺
    // 页 → 整核死循环（cve-2014-0196 open(/dev/ptmx)+ioctl(TIOCSPTLCK,&0) 即整核
    // 冻结、串口死寂）。read_bytes/write_bytes 失败按 EFAULT(-14) 处理。
    let rd_user = |n: usize| -> Option<Vec<u8>> {
        crate::syscall::user::UserSlice::<u8>::new(arg as *const u8, n).read_bytes().ok()
    };
    let wr_user = |buf: &[u8]| -> bool {
        crate::syscall::user::UserSlice::<u8>::new_mut(arg as *mut u8, buf.len())
            .write_bytes(buf)
            .is_ok()
    };

    match request {
        TIOCGPTN => {
            if arg == 0 { return Some(-14); }
            if wr_user(&(index as u32).to_ne_bytes()) { Some(0) } else { Some(-14) }
        }
        TIOCSPTLCK => {
            if arg == 0 { return Some(-14); }
            let data = match rd_user(4) { Some(d) => d, None => return Some(-14) };
            let val = i32::from_ne_bytes([data[0], data[1], data[2], data[3]]);
            let mut inner = pair.lock();
            inner.slave_locked = val != 0;
            Some(0)
        }
        TIOCGPTLCK => {
            if arg == 0 { return Some(-14); }
            let v: i32 = { let inner = pair.lock(); if inner.slave_locked { 1 } else { 0 } };
            if wr_user(&v.to_ne_bytes()) { Some(0) } else { Some(-14) }
        }
        // TCGETS: legacy struct termios, exactly 36 bytes.
        // Layout: c_iflag(4) + c_oflag(4) + c_cflag(4) + c_lflag(4) + c_line(1) + c_cc[19]
        // Does NOT include c_ispeed/c_ospeed.
        TCGETS => {
            if arg == 0 { return Some(-14); }
            let mut buf = [0u8; 36];
            {
                let inner = pair.lock();
                buf[0..4].copy_from_slice(&inner.termios.c_iflag.to_ne_bytes());
                buf[4..8].copy_from_slice(&inner.termios.c_oflag.to_ne_bytes());
                buf[8..12].copy_from_slice(&inner.termios.c_cflag.to_ne_bytes());
                buf[12..16].copy_from_slice(&inner.termios.c_lflag.to_ne_bytes());
                buf[16] = 0; // c_line
                buf[17..36].copy_from_slice(&inner.termios.c_cc);
            }
            if wr_user(&buf) { Some(0) } else { Some(-14) }
        }
        // TCSETS/TCSETSW/TCSETSF: legacy struct termios, exactly 36 bytes.
        TCSETS | TCSETSW | TCSETSF => {
            if arg == 0 { return Some(-14); }
            let data = match rd_user(36) { Some(d) => d, None => return Some(-14) };
            let buf: [u8; 36] = data[..36].try_into().unwrap();
            let mut inner = pair.lock();
            inner.termios.c_iflag = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
            inner.termios.c_oflag = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
            inner.termios.c_cflag = u32::from_ne_bytes(buf[8..12].try_into().unwrap());
            inner.termios.c_lflag = u32::from_ne_bytes(buf[12..16].try_into().unwrap());
            // c_line at offset 16 ignored
            inner.termios.c_cc.copy_from_slice(&buf[17..36]);
            if request == TCSETSF {
                inner.m2s.clear();
            }
            Some(0)
        }
        // TCGETS2: struct termios2, 44 bytes (includes c_ispeed/c_ospeed).
        TCGETS2 => {
            if arg == 0 { return Some(-14); }
            let mut buf = [0u8; 44];
            {
                let inner = pair.lock();
                buf[0..4].copy_from_slice(&inner.termios.c_iflag.to_ne_bytes());
                buf[4..8].copy_from_slice(&inner.termios.c_oflag.to_ne_bytes());
                buf[8..12].copy_from_slice(&inner.termios.c_cflag.to_ne_bytes());
                buf[12..16].copy_from_slice(&inner.termios.c_lflag.to_ne_bytes());
                buf[16] = 0;
                buf[17..36].copy_from_slice(&inner.termios.c_cc);
                buf[36..40].copy_from_slice(&inner.termios.c_ispeed.to_ne_bytes());
                buf[40..44].copy_from_slice(&inner.termios.c_ospeed.to_ne_bytes());
            }
            if wr_user(&buf) { Some(0) } else { Some(-14) }
        }
        // TCSETS2/TCSETSW2/TCSETSF2: struct termios2, 44 bytes.
        TCSETS2 | TCSETSW2 | TCSETSF2 => {
            if arg == 0 { return Some(-14); }
            let data = match rd_user(44) { Some(d) => d, None => return Some(-14) };
            let buf: [u8; 44] = data[..44].try_into().unwrap();
            let mut inner = pair.lock();
            inner.termios.c_iflag = u32::from_ne_bytes(buf[0..4].try_into().unwrap());
            inner.termios.c_oflag = u32::from_ne_bytes(buf[4..8].try_into().unwrap());
            inner.termios.c_cflag = u32::from_ne_bytes(buf[8..12].try_into().unwrap());
            inner.termios.c_lflag = u32::from_ne_bytes(buf[12..16].try_into().unwrap());
            inner.termios.c_cc.copy_from_slice(&buf[17..36]);
            inner.termios.c_ispeed = u32::from_ne_bytes(buf[36..40].try_into().unwrap());
            inner.termios.c_ospeed = u32::from_ne_bytes(buf[40..44].try_into().unwrap());
            if request == TCSETSF2 {
                inner.m2s.clear();
            }
            Some(0)
        }
        // TCFLSH: flush I/O queues. arg: TCIFLUSH(0), TCOFLUSH(1), TCIOFLUSH(2).
        TCFLSH => {
            let mut inner = pair.lock();
            match arg {
                0 => { // TCIFLUSH
                    if _is_master { inner.s2m.clear(); } else { inner.m2s.clear(); }
                }
                1 => { // TCOFLUSH
                    if _is_master { inner.m2s.clear(); } else { inner.s2m.clear(); }
                }
                2 => { // TCIOFLUSH
                    inner.m2s.clear();
                    inner.s2m.clear();
                }
                _ => return Some(-22),
            }
            Some(0)
        }
        // TCSBRK/TCSBRKP: send break — on PTY these are no-ops per Linux.
        TCSBRK | TCSBRKP => Some(0),
        // TCXONC: flow control — TCOOFF(0)/TCOON(1)/TCIOFF(2)/TCION(3).
        // PTYs don't have real hardware flow control; accept silently per Linux.
        TCXONC => {
            if arg > 3 { return Some(-22); }
            Some(0)
        }
        TIOCGWINSZ => {
            if arg == 0 { return Some(-14); }
            let mut buf = [0u8; 8];
            {
                let inner = pair.lock();
                buf[0..2].copy_from_slice(&inner.winsize.ws_row.to_ne_bytes());
                buf[2..4].copy_from_slice(&inner.winsize.ws_col.to_ne_bytes());
                buf[4..6].copy_from_slice(&inner.winsize.ws_xpixel.to_ne_bytes());
                buf[6..8].copy_from_slice(&inner.winsize.ws_ypixel.to_ne_bytes());
            }
            if wr_user(&buf) { Some(0) } else { Some(-14) }
        }
        // TIOCSWINSZ: set window size. Per Linux tty_do_resize(): if dimensions
        // actually change, deliver SIGWINCH to the foreground process group.
        TIOCSWINSZ => {
            if arg == 0 { return Some(-14); }
            let buf = match rd_user(8) { Some(d) => d, None => return Some(-14) };
            let new_row = u16::from_ne_bytes(buf[0..2].try_into().unwrap());
            let new_col = u16::from_ne_bytes(buf[2..4].try_into().unwrap());
            let new_xp = u16::from_ne_bytes(buf[4..6].try_into().unwrap());
            let new_yp = u16::from_ne_bytes(buf[6..8].try_into().unwrap());
            let mut inner = pair.lock();
            let changed = inner.winsize.ws_row != new_row
                || inner.winsize.ws_col != new_col;
            inner.winsize.ws_row = new_row;
            inner.winsize.ws_col = new_col;
            inner.winsize.ws_xpixel = new_xp;
            inner.winsize.ws_ypixel = new_yp;
            drop(inner);
            if changed {
                // Deliver SIGWINCH (28) to the foreground process group per Linux
                // tty_do_resize(). kill(-pgid, SIGWINCH).
                let pgid = {
                    let table = crate::sched::process::PROCESS_TABLE.lock();
                    table.as_ref().and_then(|t| t.current.and_then(|pid|
                        t.get(pid).map(|p| p.pgid)
                    ))
                };
                if let Some(pgid) = pgid {
                    let _ = crate::kernel::signal_syscall::sys_kill(-(pgid as isize), 28);
                }
            }
            Some(0)
        }
        TIOCGPGRP => {
            let ptr = arg as *mut i32;
            if ptr.is_null() { return Some(-14); }
            let pgid: i32 = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                table.as_ref().and_then(|t| t.current.and_then(|pid|
                    t.get(pid).map(|p| p.pgid as i32)
                )).unwrap_or(1)
            };
            if wr_user(&pgid.to_ne_bytes()) { Some(0) } else { Some(-14) }
        }
        TIOCSPGRP => {
            // Per Linux: set the foreground process group of the tty.
            // We accept the value; true enforcement requires session tracking.
            if arg == 0 { return Some(-14); }
            if rd_user(4).is_none() { return Some(-14); }
            Some(0)
        }
        TIOCGSID => {
            // Return the session leader PID. Per Linux tty_ioctl.c tiocgsid().
            let ptr = arg as *mut i32;
            if ptr.is_null() { return Some(-14); }
            let sid: i32 = {
                let table = crate::sched::process::PROCESS_TABLE.lock();
                table.as_ref().and_then(|t| t.current.and_then(|pid|
                    t.get(pid).map(|p| p.sid as i32)
                )).unwrap_or(1)
            };
            if wr_user(&sid.to_ne_bytes()) { Some(0) } else { Some(-14) }
        }
        TIOCSCTTY | TIOCNOTTY => Some(0),
        TIOCPKT => {
            // Packet mode toggle — we accept but don't implement packets.
            Some(0)
        }
        FIONREAD => {
            if arg == 0 { return Some(-14); }
            let count: i32 = { let inner = pair.lock(); if _is_master { inner.s2m.len() } else { inner.m2s.len() } } as i32;
            if wr_user(&count.to_ne_bytes()) { Some(0) } else { Some(-14) }
        }
        TIOCOUTQ => {
            if arg == 0 { return Some(-14); }
            let count: i32 = { let inner = pair.lock(); if _is_master { inner.m2s.len() } else { inner.s2m.len() } } as i32;
            if wr_user(&count.to_ne_bytes()) { Some(0) } else { Some(-14) }
        }
        TIOCGPTPEER => Some(-38), // Not yet wired to fd allocation
        _ => Some(-25), // ENOTTY
    }
}

/// PtyMasterInode wraps a PtyMasterFile to implement the Inode+File traits.
pub struct PtyMasterInode {
    master: PtyMasterFile,
    ino: usize,
}

impl Inode for PtyMasterInode {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::CharDevice,
            size: 0,
            mode: FileMode(0o020666),
            nlink: 1,
            uid: 0,
            gid: 5,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
    fn device_id(&self) -> Option<(u32, u32)> {
        Some((5, 2)) // ptmx major=5, minor=2
    }
}

impl File for PtyMasterInode {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        self.master.read_data(buf)
    }
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        self.master.write_data(buf)
    }
    fn truncate(&self, _length: usize) -> Result<(), FsError> { Ok(()) }
    fn stat(&self) -> Result<FileStat, FsError> {
        Inode::stat(self)
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
    fn poll_read(&self) -> bool { self.master.poll_read_ready() }
    fn poll_write(&self) -> bool { self.master.poll_write_ready() }
    fn ioctl(&self, request: usize, arg: usize) -> Option<isize> {
        self.master.ioctl_handler(request, arg)
    }
}

/// Called from open.rs when opening /dev/ptmx — allocates PTY pair and returns master inode.
pub fn open_ptmx() -> Result<Arc<dyn Inode>, FsError> {
    pty_init();
    let index = pty_alloc()?;
    let ino = NEXT_INO.fetch_add(1, Ordering::Relaxed);
    Ok(Arc::new(PtyMasterInode {
        master: PtyMasterFile { index },
        ino,
    }))
}

/// Check if a PTY slave at given index is locked.
pub fn pty_slave_is_locked(index: usize) -> bool {
    if let Some(pair) = pty_get(index) {
        let inner = pair.lock();
        inner.slave_locked
    } else {
        true
    }
}

/// Open the slave side: returns a per-open inode whose Drop decrements the
/// open count, so the master sees EOF/POLLHUP once the last slave fd closes
/// (Linux tty hangup semantics — what `script`/sshd wait on to exit).
pub fn open_pty_slave(index: usize) -> Result<Arc<dyn Inode>, FsError> {
    let pair = pty_get(index).ok_or(FsError::IoError)?;
    {
        let mut inner = pair.lock();
        if inner.slave_locked {
            return Err(FsError::IoError);
        }
        inner.slave_count += 1;
        inner.slave_ever_opened = true;
    }
    let ino = NEXT_INO.fetch_add(1, Ordering::Relaxed);
    Ok(Arc::new(PtySlaveInode { index, ino }))
}

/// Per-open PTY slave file object (one per open of /dev/pts/N).
pub struct PtySlaveInode {
    index: usize,
    ino: usize,
}

impl Drop for PtySlaveInode {
    fn drop(&mut self) {
        if let Some(pair) = pty_get(self.index) {
            let mut inner = pair.lock();
            inner.slave_count = inner.slave_count.saturating_sub(1);
        }
    }
}

impl Inode for PtySlaveInode {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::CharDevice,
            size: 0,
            mode: FileMode(0o020620),
            nlink: 1,
            uid: 0,
            gid: 5,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
    fn device_id(&self) -> Option<(u32, u32)> {
        Some((136, self.index as u32))
    }
}

impl File for PtySlaveInode {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        PtySlaveOps { index: self.index }.read(0, buf)
    }
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        PtySlaveOps { index: self.index }.write(0, buf)
    }
    fn truncate(&self, _length: usize) -> Result<(), FsError> { Ok(()) }
    fn stat(&self) -> Result<FileStat, FsError> { Inode::stat(self) }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
    fn poll_read(&self) -> bool {
        if let Some(pair) = pty_get(self.index) {
            let inner = pair.lock();
            !inner.m2s.is_empty() || !inner.master_open
        } else {
            true
        }
    }
    fn poll_write(&self) -> bool { true }
    fn ioctl(&self, request: usize, arg: usize) -> Option<isize> {
        pty_ioctl(self.index, false, request, arg)
    }
}

fn char_stat() -> FileStat {
    FileStat {
        file_type: FileType::CharDevice,
        size: 0,
        mode: FileMode(0o020666),
        nlink: 1,
        uid: 0,
        gid: 0,
        atime: 0,
        mtime: 0,
        ctime: 0,
    }
}
