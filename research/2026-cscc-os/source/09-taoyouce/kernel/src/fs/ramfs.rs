//! RAM 文件系统
//!
//! 简单的内存文件系统实现

use super::*;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use alloc::sync::Arc;
use crate::sync::SpinLock;

/// RAM 文件系统
pub struct RamFs {
    root: Arc<RamInode>,
}

/// ramfs 合成 inode 号分配器。基址与 ext4 覆盖层（0x1000_0001 起）及
/// 磁盘镜像上的真实 ext4 inode（百万量级以内）错开：本内核所有文件系统
/// 共用 st_dev，(dev,ino) 的唯一性只能靠 ino 空间不相交保证——glibc
/// getcwd/realpath 的 ".." 回溯按 (dev,ino) 匹配目录项，ino 撞号会导致
/// 匹配到错误目录而死循环。ino 0 在 Linux 上是非法值，一律不用。
static RAMFS_NEXT_INO: AtomicUsize = AtomicUsize::new(0x2000_0001);

pub(crate) fn ramfs_alloc_ino() -> usize {
    RAMFS_NEXT_INO.fetch_add(1, Ordering::Relaxed)
}

impl RamFs {
    pub fn new() -> Arc<Self> {
        let root = Arc::new(RamInode::new_dir(ramfs_alloc_ino()));
        Arc::new(Self { root })
    }
}

impl FileSystem for RamFs {
    fn name(&self) -> &str {
        "ramfs"
    }

    fn root(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

/// Word-wide all-zero test for a byte slice. Scans 8 bytes at a time over the
/// naturally-aligned middle (the compiler vectorizes the `u64` loop far better
/// than a byte-at-a-time `iter().all()`), which the file-write hole detector
/// runs on every chunk (lmbench lat_fs / iozone hot path).
#[inline]
fn slice_is_all_zero(s: &[u8]) -> bool {
    // SAFETY: `align_to` only reinterprets the already-valid `&[u8]` bytes as
    // `u64`s over the aligned interior; the unaligned head/tail stay `u8`.
    let (head, mid, tail) = unsafe { s.align_to::<u64>() };
    head.iter().all(|&b| b == 0)
        && mid.iter().all(|&w| w == 0)
        && tail.iter().all(|&b| b == 0)
}

/// Safety margin of free physical memory below which RAM-backed filesystems
/// stop allocating new backing pages and report ENOSPC, so an unbounded write
/// can never OOM-panic the kernel. Sized to leave the kernel enough headroom
/// to keep running (and to tear the offending process down) after the limit
/// is hit; comfortably below the smallest target's frame pool (RV: ~150 MiB).
const RAMFS_OOM_MARGIN: usize = 64 * 1024 * 1024;

/// RAM Inode
pub struct RamInode {
    ino: usize,
    file_type: FileType,
    inner: SpinLock<RamInodeInner>,
    /// inode-level seals（F_GET_SEALS / F_ADD_SEALS）。普通 ramfs 文件初始为
    /// `F_SEAL_SEAL`（禁止加新 seal，等同未来不可用 memfd 语义）；memfd 走
    /// `new_memfd` 时清零或仅置 `F_SEAL_SEAL`（取决于 MFD_ALLOW_SEALING）。
    seals: core::sync::atomic::AtomicU32,
    /// inode 是否允许进一步 F_ADD_SEALS。non-memfd 默认 false。
    allow_sealing: core::sync::atomic::AtomicBool,
    /// 设备节点 rdev：高 32 位 major，低 32 位 minor。0 = 非设备节点。
    /// mknod(2) 创建 S_IFCHR/S_IFBLK 时写入，stat(2).st_rdev 与
    /// statx(2).stx_rdev_major/minor 由 `device_id()` 读取（statx01）。
    rdev: core::sync::atomic::AtomicU64,
    /// Count of active MAP_SHARED writable mappings (Linux i_mmap_writable).
    /// F_SEAL_WRITE cannot be added while this is > 0.
    shared_writable_maps: core::sync::atomic::AtomicUsize,
    /// Linux FS_IOC_GETFLAGS / FS_IOC_SETFLAGS inode flags (FS_IMMUTABLE_FL, FS_APPEND_FL, etc.)
    inode_flags: core::sync::atomic::AtomicU32,
    /// 硬链接计数（Linux `inode->i_nlink`）。原子字段而非 inner.stat 内字段：
    /// unlink/link 只需一次原子增减，免在热路径上取子 inode 的 inner 锁。
    /// stat() 返回时从这里回填（inner.stat.nlink 仅作初始值，不再更新）。
    nlink: core::sync::atomic::AtomicUsize,
    /// MFD_HUGETLB memfd 的大页尺寸（字节）；0 = 非 hugetlb memfd。
    /// Linux hugetlbfs 的 f_op 无 write_iter → write(2)/pwrite(2) 返 EINVAL；
    /// mmap 必须按大页处理（memfd_create03）。
    memfd_hugetlb: usize,
}

/// 页对齐、可 Arc 共享的 4 KiB 文件数据页。read_to_user 零拷贝别名映射
///（把文件页帧直接挂进用户页表）要求真物理帧 + 引用计数（防
/// unlink/truncate 后 use-after-free），故用 `SharedFrame`（clone=MetaSlot+1、
/// drop=MetaSlot-1 的平衡句柄，2026-08-14 P0 修复）而非裸 Arc<FrameTracker>。
/// 写时若帧被别名引用（meta_ref>1）先复制新帧，保证 read(2) 的
/// 快照语义（已读入用户缓冲区的内容不随后续文件写入变化）。
pub(crate) struct PageBuf(crate::mm::SharedFrame);

impl PageBuf {
    const PAGE: usize = 4096;

    fn new_zeroed() -> Option<Self> {
        crate::mm::frame_alloc().map(|f| PageBuf(crate::mm::SharedFrame::from_alloc(f)))
    }

    /// 从 src 构造：前 chunk 字节拷源，尾部清零（只碰尾部，不双写数据区）。
    fn from_slice(src: &[u8]) -> Option<Self> {
        let f = crate::mm::frame_alloc_uninit()?;
        let base = f.ppn.kernel_addr() as *mut u8;
        unsafe {
            core::ptr::copy_nonoverlapping(src.as_ptr(), base, src.len());
            if src.len() < Self::PAGE {
                core::ptr::write_bytes(base.add(src.len()), 0, Self::PAGE - src.len());
            }
        }
        Some(PageBuf(crate::mm::SharedFrame::from_alloc(f)))
    }

    #[inline]
    fn as_slice(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.0.ppn.kernel_addr() as *const u8, Self::PAGE) }
    }

    /// 可变访问；帧被别名共享时先 COW 复制。
    fn make_mut(&mut self) -> &mut [u8] {
        if self.0.meta_ref() > 1 {
            if let Some(copy) = Self::from_slice(self.as_slice()) {
                self.0 = copy.0;
            }
        }
        unsafe { core::slice::from_raw_parts_mut(self.0.ppn.kernel_addr() as *mut u8, Self::PAGE) }
    }

    pub(crate) fn frame(&self) -> crate::mm::SharedFrame {
        self.0.clone()
    }

    #[inline]
    pub(crate) fn frame_ptr(&self) -> *const crate::mm::FrameTracker {
        self.0.arc_ptr()
    }

    #[inline]
    pub(crate) fn frame_ref(&self) -> &crate::mm::SharedFrame {
        &self.0
    }
}

impl core::ops::Deref for PageBuf {
    type Target = [u8];
    #[inline]
    fn deref(&self) -> &[u8] { self.as_slice() }
}

/// 全局共享零页（Linux ZERO_PAGE 语义）：洞页读取的零拷贝别名目标。
/// 只读映射进用户页表，用户写时 handle_cow 见 meta_ref>1 会复制，永不被改。
static ZERO_PAGE: SpinLock<Option<crate::mm::SharedFrame>> =
    SpinLock::new(None);

fn zero_page_frame() -> Option<crate::mm::SharedFrame> {
    let mut g = ZERO_PAGE.lock();
    if g.is_none() {
        *g = crate::mm::frame_alloc().map(crate::mm::SharedFrame::from_alloc);
    }
    g.clone()
}

/// 零页裸指针（未分配时 None）——稳态零分配比对用，不触发分配。
fn zero_page_ptr() -> Option<*const crate::mm::FrameTracker> {
    ZERO_PAGE.lock().as_ref().map(|z| z.arc_ptr())
}

/// 页粒度文件页存储，对标 Linux 页缓存 xarray：按 512 页块分块的
/// 基数结构——块内 O(1) 直接下标，区间迭代沿块内切片顺扫（免逐页
/// BTreeMap 下降/节点追逐）；稀疏洞保持（缺页 = 洞），大稀疏 loop
/// 镜像只为存在页的块付 4 KiB 槽位开销，块清空即释放。
const PS_CHUNK: usize = 512;
const PS_WORDS: usize = PS_CHUNK / 64;

/// 位图 + `MaybeUninit` 槽：占用状态由 64 字节位图承载，槽区不初始化
/// （新块免 4 KiB memset），Drop/迭代只沿位图 set 位访问（免 512 槽顺扫）。
struct PageChunk {
    used: u16,
    bitmap: [u64; PS_WORDS],
    slots: core::mem::ManuallyDrop<PageSlots>,
}

type PageSlots = alloc::boxed::Box<[core::mem::MaybeUninit<PageBuf>; PS_CHUNK]>;

/// 槽数组回收池（对标 PCB/内核栈池先例）：小文件 create/unlink
/// 频繁构造/析构单块 PageChunk，免每次 4 KiB 堆 alloc/dealloc。
static PS_SLAB: SpinLock<alloc::vec::Vec<PageSlots>> = SpinLock::new(alloc::vec::Vec::new());
const PS_SLAB_CAP: usize = 1024;

impl PageChunk {
    fn new() -> Self {
        let slots = PS_SLAB
            .lock()
            .pop()
            .unwrap_or_else(|| unsafe { alloc::boxed::Box::new_uninit().assume_init() });
        PageChunk { used: 0, bitmap: [0; PS_WORDS], slots: core::mem::ManuallyDrop::new(slots) }
    }

    #[inline]
    fn is_set(&self, i: usize) -> bool {
        self.bitmap[i / 64] & (1u64 << (i % 64)) != 0
    }

    #[inline]
    fn get(&self, i: usize) -> Option<&PageBuf> {
        if self.is_set(i) {
            Some(unsafe { self.slots[i].assume_init_ref() })
        } else {
            None
        }
    }

    #[inline]
    fn get_mut(&mut self, i: usize) -> Option<&mut PageBuf> {
        if self.is_set(i) {
            Some(unsafe { self.slots[i].assume_init_mut() })
        } else {
            None
        }
    }

    /// 置入槽 `i`；已占用则原地替换（旧值正确析构）。
    fn set(&mut self, i: usize, page: PageBuf) {
        if self.is_set(i) {
            unsafe { self.slots[i].assume_init_drop() };
        } else {
            self.bitmap[i / 64] |= 1u64 << (i % 64);
            self.used += 1;
        }
        self.slots[i].write(page);
    }

    /// 取出槽 `i` 的值并清占用位。
    fn take(&mut self, i: usize) -> Option<PageBuf> {
        if !self.is_set(i) {
            return None;
        }
        self.bitmap[i / 64] &= !(1u64 << (i % 64));
        self.used -= 1;
        Some(unsafe { self.slots[i].assume_init_read() })
    }
}

impl Drop for PageChunk {
    fn drop(&mut self) {
        if self.used != 0 {
            for w in 0..PS_WORDS {
                let mut bits = self.bitmap[w];
                while bits != 0 {
                    let b = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    unsafe { self.slots[w * 64 + b].assume_init_drop() };
                }
            }
        }
        let slots = unsafe { core::mem::ManuallyDrop::take(&mut self.slots) };
        let mut pool = PS_SLAB.lock();
        if pool.len() < PS_SLAB_CAP {
            pool.push(slots);
        }
    }
}

/// 块 0（页 0..512 = 文件首 2 MiB）内联存放：绝大多数文件单块即容纳，
/// 全程零 BTreeMap 节点分配/析构/下降；仅 >2 MiB 的溢出块进 BTreeMap。
#[derive(Default)]
struct PageStore {
    chunk0: Option<PageChunk>,
    chunks: BTreeMap<usize, PageChunk>,
}

impl PageStore {
    fn new() -> Self {
        PageStore { chunk0: None, chunks: BTreeMap::new() }
    }

    #[inline]
    fn chunk(&self, c: usize) -> Option<&PageChunk> {
        if c == 0 { self.chunk0.as_ref() } else { self.chunks.get(&c) }
    }

    #[inline]
    fn get(&self, idx: usize) -> Option<&PageBuf> {
        self.chunk(idx / PS_CHUNK)?.get(idx % PS_CHUNK)
    }

    #[inline]
    fn get_mut(&mut self, idx: usize) -> Option<&mut PageBuf> {
        let c = idx / PS_CHUNK;
        let chunk = if c == 0 { self.chunk0.as_mut() } else { self.chunks.get_mut(&c) };
        chunk?.get_mut(idx % PS_CHUNK)
    }

    #[inline]
    fn contains_key(&self, idx: usize) -> bool {
        self.get(idx).is_some()
    }

    fn insert(&mut self, idx: usize, page: PageBuf) {
        let c = idx / PS_CHUNK;
        let chunk = if c == 0 {
            self.chunk0.get_or_insert_with(PageChunk::new)
        } else {
            self.chunks.entry(c).or_insert_with(PageChunk::new)
        };
        chunk.set(idx % PS_CHUNK, page);
    }

    fn remove(&mut self, idx: usize) -> Option<PageBuf> {
        let c = idx / PS_CHUNK;
        if c == 0 {
            let chunk = self.chunk0.as_mut()?;
            let old = chunk.take(idx % PS_CHUNK);
            if old.is_some() && chunk.used == 0 {
                self.chunk0 = None;
            }
            return old;
        }
        let chunk = self.chunks.get_mut(&c)?;
        let old = chunk.take(idx % PS_CHUNK);
        if old.is_some() && chunk.used == 0 {
            self.chunks.remove(&c);
        }
        old
    }

    /// 升序迭代 `[r.start, r.end)` 内已物化的页（洞页跳过）。
    fn range(
        &self,
        r: core::ops::Range<usize>,
    ) -> impl Iterator<Item = (usize, &PageBuf)> {
        let (start, end) = (r.start, r.end.max(r.start));
        let hi_c = end.div_ceil(PS_CHUNK);
        let c0 = if start < PS_CHUNK { self.chunk0.as_ref() } else { None };
        let head = c0.into_iter().flat_map(move |ch| {
            let hi_i = end.min(PS_CHUNK);
            (start..hi_i).filter_map(move |i| ch.get(i).map(|p| (i, p)))
        });
        let lo_c = (start / PS_CHUNK).max(1);
        let tail = self.chunks.range(lo_c..hi_c.max(1)).flat_map(move |(&c, ch)| {
            let base = c * PS_CHUNK;
            let lo_i = start.saturating_sub(base).min(PS_CHUNK);
            let hi_i = (end - base).min(PS_CHUNK);
            (lo_i..hi_i).filter_map(move |i| ch.get(i).map(|p| (base + i, p)))
        });
        head.chain(tail)
    }

    /// 块缓存游标：升序逐页查询时每 512 页只做一次块定位，
    /// 块内 O(1) 直接下标（热读路径用，免迭代器适配器堆栈）。
    #[inline]
    fn cursor(&self) -> PageCursor<'_> {
        PageCursor { store: self, chunk_idx: usize::MAX, chunk: None }
    }

    /// 移除一切 `idx >= first` 的页（truncate 缩小路径）。
    fn remove_from(&mut self, first: usize) {
        let lo_c = first / PS_CHUNK;
        if lo_c == 0 {
            if first == 0 {
                self.chunk0 = None;
            } else if let Some(ch) = self.chunk0.as_mut() {
                for i in first..PS_CHUNK {
                    let _ = ch.take(i);
                }
                if ch.used == 0 {
                    self.chunk0 = None;
                }
            }
            self.chunks.clear();
            return;
        }
        let split = self.chunks.split_off(&lo_c);
        for (c, mut ch) in split {
            if c == lo_c && first % PS_CHUNK != 0 {
                for i in first % PS_CHUNK..PS_CHUNK {
                    let _ = ch.take(i);
                }
                if ch.used > 0 {
                    self.chunks.insert(c, ch);
                }
            }
        }
    }
}

struct PageCursor<'a> {
    store: &'a PageStore,
    chunk_idx: usize,
    chunk: Option<&'a PageChunk>,
}

impl<'a> PageCursor<'a> {
    #[inline]
    fn get(&mut self, idx: usize) -> Option<&'a PageBuf> {
        let c = idx / PS_CHUNK;
        if self.chunk_idx != c {
            self.chunk = self.store.chunk(c);
            self.chunk_idx = c;
        }
        self.chunk?.get(idx % PS_CHUNK)
    }
}

struct RamInodeInner {
    /// Symlink target / legacy small metadata buffer. Regular-file payload is
    /// stored in `sparse_pages` so large fallocate/truncate backed loop images
    /// do not require hundreds of MiB of contiguous RAM.
    data: Vec<u8>,
    /// Sparse regular-file data pages. Missing pages are holes and read back
    /// as zero until `stat.size`, matching Linux tmpfs/ramfs sparse semantics.
    sparse_pages: PageStore,
    /// 子目录项。Slice 2.x 真硬链接重构：这里从 `Arc<RamInode>`
    /// 改为 `Arc<dyn Inode>`，让一个 inode 可被多个目录项指向（真 Linux 的
    /// `inode->i_nlink` 语义）。`link(2)` 只需 `Arc::clone` 后插入另一个
    /// dirent。`unlink(2)` 移除 dirent后若 strong_count==0 刚Arc 自动释放数据。
    /// FNV 哈希表（非 BTreeMap）：目录项查找/插入/删除 O(1)，避免海量
    /// create/unlink 场景下 B 树逐层 String 比较（memcmp）成为热点；
    /// read_dir 输出时排序，保证 getdents 偏移语义稳定。
    children: crate::fs::NameMap<Arc<dyn Inode>>,
    /// 元数据
    stat: FileStat,
}

/// memfd / shmem seal bits —— 与 `syscall/fd.rs` 内的常量同步。
const F_SEAL_SEAL: u32         = 0x0001;
const F_SEAL_SHRINK: u32       = 0x0002;
const F_SEAL_GROW: u32         = 0x0004;
const F_SEAL_WRITE: u32        = 0x0008;
const F_SEAL_FUTURE_WRITE: u32 = 0x0010;

fn current_create_ids() -> (u32, u32) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)))
        .map(|p| (p.euid, p.egid))
        .unwrap_or((0, 0))
}

fn current_time_sec() -> u64 {
    // 文件时间戳取 CLOCK_REALTIME 整秒（与 clock_gettime 同源）。
    crate::syscall::time::realtime_sec().max(0) as u64
}

impl RamInode {
    /// Recover `Arc<RamInode>` from an `Arc<dyn Inode>` that was constructed as
    /// `Arc::new(RamInode { .. })`. Used by ext4's overlay layer to re-wrap
    /// mkdir'd directories as `Ext4DirInode` without losing the same backing node.
    pub(crate) fn try_arc_from_dyn(inode: Arc<dyn Inode>) -> Option<Arc<RamInode>> {
        if !inode.as_ref().as_any().is::<RamInode>() {
            return None;
        }
        // SAFETY: concrete type checked above; Arc vtable/data match RamInode.
        Some(unsafe { Arc::from_raw(Arc::into_raw(inode) as *const RamInode) })
    }

    pub(crate) fn new_file(ino: usize) -> Self {
        let (uid, gid) = current_create_ids();
        let now = current_time_sec();
        Self {
            ino,
            file_type: FileType::Regular,
            inner: SpinLock::new(RamInodeInner {
                data: Vec::new(),
                sparse_pages: PageStore::new(),
                children: crate::fs::NameMap::default(),
                stat: FileStat {
                    file_type: FileType::Regular,
                    size: 0,
                    mode: FileMode::default_file(),
                    nlink: 1,
                    uid,
                    gid,
                    atime: now,
                    mtime: now,
                    ctime: now,
                },
            }),
            seals: core::sync::atomic::AtomicU32::new(F_SEAL_SEAL),
            allow_sealing: core::sync::atomic::AtomicBool::new(false),
            rdev: core::sync::atomic::AtomicU64::new(0),
            shared_writable_maps: core::sync::atomic::AtomicUsize::new(0),
            inode_flags: core::sync::atomic::AtomicU32::new(0),
            nlink: core::sync::atomic::AtomicUsize::new(1),
            memfd_hugetlb: 0,
        }
    }

    pub(crate) fn new_dir(ino: usize) -> Self {
        let (uid, gid) = current_create_ids();
        let now = current_time_sec();
        Self {
            ino,
            file_type: FileType::Directory,
            inner: SpinLock::new(RamInodeInner {
                data: Vec::new(),
                sparse_pages: PageStore::new(),
                children: crate::fs::NameMap::default(),
                stat: FileStat {
                    file_type: FileType::Directory,
                    size: 0,
                    mode: FileMode::default_dir(),
                    nlink: 2,
                    uid,
                    gid,
                    atime: now,
                    mtime: now,
                    ctime: now,
                },
            }),
            seals: core::sync::atomic::AtomicU32::new(F_SEAL_SEAL),
            allow_sealing: core::sync::atomic::AtomicBool::new(false),
            rdev: core::sync::atomic::AtomicU64::new(0),
            shared_writable_maps: core::sync::atomic::AtomicUsize::new(0),
            inode_flags: core::sync::atomic::AtomicU32::new(0),
            nlink: core::sync::atomic::AtomicUsize::new(2),
            memfd_hugetlb: 0,
        }
    }

    pub(crate) fn new_chardev(ino: usize) -> Self {
        let (uid, gid) = current_create_ids();
        let now = current_time_sec();
        Self {
            ino,
            file_type: FileType::CharDevice,
            inner: SpinLock::new(RamInodeInner {
                data: Vec::new(),
                sparse_pages: PageStore::new(),
                children: crate::fs::NameMap::default(),
                stat: FileStat {
                    file_type: FileType::CharDevice,
                    size: 0,
                    mode: FileMode(0o020666), // S_IFCHR | rw-rw-rw-
                    nlink: 1,
                    uid,
                    gid,
                    atime: now,
                    mtime: now,
                    ctime: now,
                },
            }),
            seals: core::sync::atomic::AtomicU32::new(F_SEAL_SEAL),
            allow_sealing: core::sync::atomic::AtomicBool::new(false),
            rdev: core::sync::atomic::AtomicU64::new(0),
            shared_writable_maps: core::sync::atomic::AtomicUsize::new(0),
            inode_flags: core::sync::atomic::AtomicU32::new(0),
            nlink: core::sync::atomic::AtomicUsize::new(1),
            memfd_hugetlb: 0,
        }
    }
    pub(crate) fn new_symlink(ino: usize) -> Self {
        let (uid, gid) = current_create_ids();
        let now = current_time_sec();
        Self {
            ino,
            file_type: FileType::Symlink,
            inner: SpinLock::new(RamInodeInner {
                data: Vec::new(),
                sparse_pages: PageStore::new(),
                children: crate::fs::NameMap::default(),
                stat: FileStat {
                    file_type: FileType::Symlink,
                    size: 0,
                    mode: FileMode(0o120777),
                    nlink: 1,
                    uid,
                    gid,
                    atime: now,
                    mtime: now,
                    ctime: now,
                },
            }),
            seals: core::sync::atomic::AtomicU32::new(F_SEAL_SEAL),
            allow_sealing: core::sync::atomic::AtomicBool::new(false),
            rdev: core::sync::atomic::AtomicU64::new(0),
            shared_writable_maps: core::sync::atomic::AtomicUsize::new(0),
            inode_flags: core::sync::atomic::AtomicU32::new(0),
            nlink: core::sync::atomic::AtomicUsize::new(1),
            memfd_hugetlb: 0,
        }
    }

    /// Slice 1.9 — Generic node constructor for mknodat(2). Synthesizes a
    /// minimal RamInode with the requested type and a sensible default mode.
    /// FIFO/Socket/BlockDevice are zero-content placeholders; read/write
    /// returns empty / accepts data per `Regular` File impl below.
    pub(crate) fn new_typed(ino: usize, file_type: FileType) -> Self {
        let mode = match file_type {
            FileType::Regular     => FileMode::default_file(),
            FileType::Directory   => FileMode::default_dir(),
            FileType::Symlink     => FileMode(0o120777),
            FileType::CharDevice  => FileMode(0o020666),
            FileType::BlockDevice => FileMode(0o060666),
            FileType::Pipe        => FileMode(0o010666), // S_IFIFO | rw-rw-rw-
            FileType::Socket      => FileMode(0o140777), // S_IFSOCK | rwxrwxrwx
        };
        let (uid, gid) = current_create_ids();
        let now = current_time_sec();
        Self {
            ino,
            file_type,
            inner: SpinLock::new(RamInodeInner {
                data: Vec::new(),
                sparse_pages: PageStore::new(),
                children: crate::fs::NameMap::default(),
                stat: FileStat {
                    file_type,
                    size: 0,
                    mode,
                    nlink: if matches!(file_type, FileType::Directory) { 2 } else { 1 },
                    uid,
                    gid,
                    atime: now,
                    mtime: now,
                    ctime: now,
                },
            }),
            seals: core::sync::atomic::AtomicU32::new(F_SEAL_SEAL),
            allow_sealing: core::sync::atomic::AtomicBool::new(false),
            rdev: core::sync::atomic::AtomicU64::new(0),
            shared_writable_maps: core::sync::atomic::AtomicUsize::new(0),
            inode_flags: core::sync::atomic::AtomicU32::new(0),
            nlink: core::sync::atomic::AtomicUsize::new(
                if matches!(file_type, FileType::Directory) { 2 } else { 1 },
            ),
            memfd_hugetlb: 0,
        }
    }

    /// 创建一个匿名 memfd inode（与 Linux `memfd_create` 行为一致）：
    /// - `allow_sealing == true` → seals 初始 = 0，允许后续 F_ADD_SEALS。
    /// - `allow_sealing == false` → seals 初始 = F_SEAL_SEAL（任何 F_ADD_SEALS
    ///   都返回 EPERM）。
    pub fn new_memfd(ino: usize, allow_sealing: bool) -> Self {
        Self::new_memfd_sized(ino, allow_sealing, 0)
    }

    /// MFD_HUGETLB 变体：`huge_page_size` 非 0 时标记为 hugetlb memfd，
    /// write(2) 拒 EINVAL（hugetlbfs 无 write_iter），mmap 按大页处理。
    pub fn new_memfd_sized(ino: usize, allow_sealing: bool, huge_page_size: usize) -> Self {
        let initial_seals = if allow_sealing { 0 } else { F_SEAL_SEAL };
        let (uid, gid) = current_create_ids();
        let now = current_time_sec();
        Self {
            ino,
            file_type: FileType::Regular,
            inner: SpinLock::new(RamInodeInner {
                data: Vec::new(),
                sparse_pages: PageStore::new(),
                children: crate::fs::NameMap::default(),
                stat: FileStat {
                    file_type: FileType::Regular,
                    size: 0,
                    mode: FileMode::default_file(),
                    nlink: 1,
                    uid,
                    gid,
                    atime: now,
                    mtime: now,
                    ctime: now,
                },
            }),
            seals: core::sync::atomic::AtomicU32::new(initial_seals),
            allow_sealing: core::sync::atomic::AtomicBool::new(allow_sealing),
            rdev: core::sync::atomic::AtomicU64::new(0),
            shared_writable_maps: core::sync::atomic::AtomicUsize::new(0),
            inode_flags: core::sync::atomic::AtomicU32::new(0),
            nlink: core::sync::atomic::AtomicUsize::new(1),
            memfd_hugetlb: huge_page_size,
        }
    }

    /// unlink 热路径：原子递减 nlink（不取 inner 锁），地板为 0。
    #[inline]
    fn dec_nlink(&self) {
        use core::sync::atomic::Ordering::Relaxed;
        let _ = self.nlink.fetch_update(Relaxed, Relaxed, |n| n.checked_sub(1));
    }

    /// memfd-style seal 容许位是否允许 F_ADD_SEALS。F_SEAL_SEAL 一旦置位
    /// 也会让 syscall 层拒绝任何后续 add。
    pub fn allow_sealing(&self) -> bool {
        self.allow_sealing.load(core::sync::atomic::Ordering::Acquire)
    }

    /// 取得当前 inode-level seals。
    pub fn current_seals(&self) -> u32 {
        self.seals.load(core::sync::atomic::Ordering::Acquire)
    }

    /// 添加 seal 位。返回 `Err(-EPERM)` 表示已被 F_SEAL_SEAL 锁死，
    /// `Err(-EBUSY)` 表示有未关闭的写映射（这里暂未追踪，TODO future）。
    pub fn add_seals(&self, mask: u32) -> Result<(), i32> {
        if !self.allow_sealing() {
            return Err(-1); // -EPERM
        }
        loop {
            let cur = self.seals.load(core::sync::atomic::Ordering::Acquire);
            if cur & F_SEAL_SEAL != 0 {
                return Err(-1); // -EPERM
            }
            let new = cur | mask;
            if self.seals
                .compare_exchange(cur, new, core::sync::atomic::Ordering::AcqRel, core::sync::atomic::Ordering::Acquire)
                .is_ok()
            {
                return Ok(());
            }
        }
    }
}

impl Drop for RamInode {
    /// Return any huge pages this inode still holds to the global pool when the
    /// last reference is dropped. The key matches the inode data-pointer address
    /// used by `sys_fallocate` on hugetlbfs files; for a plain ramfs inode (no
    /// hugetlb reservations) this is a cheap map-miss no-op.
    fn drop(&mut self) {
        let key = self as *const RamInode as *const () as usize;
        crate::mm::hugetlb_file_drop(key);
        // Release any inode-owned MAP_SHARED hugetlbfs reservations/pages still
        // charged to this inode (Linux frees them when the inode is evicted, e.g.
        // the file is unlinked and the last fd closed). munmap deliberately keeps
        // them charged, so this is the final backstop (hugemmap10).
        crate::mm::address_space::shared_huge_cache_evict_ino(self.ino);
        crate::mm::hugetlb_shared_drop(self.ino);
    }
}

impl Inode for RamInode {
    fn ino(&self) -> usize {
        self.ino
    }

    fn file_type(&self) -> FileType {
        self.file_type
    }

    /// 设备节点 rdev（statx01）。仅字符/块设备返回写入的 major/minor；
    /// 普通文件/目录或未设置 rdev 的节点返回 None（st_rdev=0）。
    fn device_id(&self) -> Option<(u32, u32)> {
        if !matches!(self.file_type, FileType::CharDevice | FileType::BlockDevice) {
            return None;
        }
        let dev = self.rdev.load(core::sync::atomic::Ordering::Acquire);
        if dev == 0 {
            None
        } else {
            Some(((dev >> 32) as u32, dev as u32))
        }
    }

    /// rmdir(2) marks the removed directory dead (Linux `S_DEAD`): drop its link
    /// count to 0 so getdents on a still-open fd returns ENOENT (getdents02).
    fn mark_dead(&self) {
        self.nlink.store(0, core::sync::atomic::Ordering::Relaxed);
    }

    /// mknod(2) 创建字符/块设备节点后写入 rdev（major<<32 | minor）。
    fn set_device_id(&self, major: u32, minor: u32) -> Result<(), FsError> {
        self.rdev.store(
            ((major as u64) << 32) | (minor as u64),
            core::sync::atomic::Ordering::Release,
        );
        Ok(())
    }

    /// 真硬链接需要 ramfs 在 `Directory::link` 中 downcast 任意
    /// `Arc<dyn Inode>` 到 `&RamInode`。其他 fs（procfs/devfs/ext4 shim）
    /// 走默认 sentinel 实现 → downcast 失败 → 跨 fs link 返 EXDEV/PermissionDenied。
    fn get_inode_flags(&self) -> u32 {
        self.inode_flags.load(core::sync::atomic::Ordering::Acquire)
    }

    fn set_inode_flags(&self, flags: u32) -> Result<(), FsError> {
        self.inode_flags.store(flags, core::sync::atomic::Ordering::Release);
        Ok(())
    }

    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_file(&self) -> Option<&dyn File> {
        if self.file_type == FileType::Regular || self.file_type == FileType::CharDevice || self.file_type == FileType::Symlink {
            Some(self)
        } else {
            None
        }
    }

    fn as_dir(&self) -> Option<&dyn Directory> {
        if self.file_type == FileType::Directory {
            Some(self)
        } else {
            None
        }
    }

    /// Slice 2.x T6 — 真 read_link：当此 inode 是 Symlink 时返回 inner.data
    /// 解析为 UTF-8 字符串作为目标路径。
    fn read_link(&self) -> Result<alloc::string::String, FsError> {
        if self.file_type != FileType::Symlink {
            return Err(FsError::InvalidArgument);
        }
        let inner = self.inner.lock();
        match core::str::from_utf8(&inner.data) {
            Ok(s) => Ok(s.into()),
            Err(_) => Err(FsError::InvalidArgument),
        }
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        let mut s = self.inner.lock().stat.clone();
        s.nlink = self.nlink.load(core::sync::atomic::Ordering::Relaxed);
        Ok(s)
    }

    fn set_times(&self, atime: Option<u64>, mtime: Option<u64>) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        let changed = atime.is_some() || mtime.is_some();
        if let Some(a) = atime { inner.stat.atime = a; }
        if let Some(m) = mtime { inner.stat.mtime = m; }
        if changed { inner.stat.ctime = current_time_sec(); }
        Ok(())
    }

    fn set_mode(&self, mode: u32) -> Result<(), FsError> {
        // Preserve high S_IFMT bits (file-type) and only replace the lower
        // 12 permission bits (rwx + setuid/setgid/sticky), mirroring Linux
        // chmod(2) semantics.
        const S_IFMT: u32 = 0o170000;
        const PERM_MASK: u32 = 0o7777;
        let mut inner = self.inner.lock();
        let old = inner.stat.mode.0;
        inner.stat.mode = FileMode((old & S_IFMT) | (mode & PERM_MASK));
        inner.stat.ctime = current_time_sec();
        Ok(())
    }

    fn set_owner(&self, uid: Option<u32>, gid: Option<u32>) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        if let Some(uid) = uid { inner.stat.uid = uid; }
        if let Some(gid) = gid { inner.stat.gid = gid; }
        inner.stat.ctime = current_time_sec();
        // Linux `chown_common()` / `setattr_copy()` semantics for regular
        // files: successful chown clears S_ISUID. It clears S_ISGID only when
        // the file is group-executable; non-group-executable S_ISGID is the
        // historical mandatory-locking marker and must be preserved.  LTP
        // chown02 checks exactly this split.
        if matches!(inner.stat.file_type, FileType::Regular) {
            const S_ISUID: u32 = 0o4000;
            const S_ISGID: u32 = 0o2000;
            const S_IXGRP: u32 = 0o0010;
            let mut mode = inner.stat.mode.0 & !S_ISUID;
            if mode & S_IXGRP != 0 {
                mode &= !S_ISGID;
            }
            inner.stat.mode = FileMode(mode);
        }
        Ok(())
    }

    fn memfd_hugetlb_size(&self) -> usize {
        self.memfd_hugetlb
    }

    fn shared_seals(&self) -> Option<&core::sync::atomic::AtomicU32> {
        Some(&self.seals)
    }

    fn inc_writable_maps(&self) {
        self.shared_writable_maps.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
    }
    fn dec_writable_maps(&self) {
        self.shared_writable_maps.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
    }
    fn writable_maps_count(&self) -> usize {
        self.shared_writable_maps.load(core::sync::atomic::Ordering::Acquire)
    }
}

impl File for RamInode {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        if self.file_type == FileType::CharDevice {
            return Err(FsError::InvalidArgument);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let mut inner = self.inner.lock();

        if self.file_type == FileType::Symlink {
            let data = &inner.data;
            if offset >= data.len() {
                return Ok(0);
            }
            let end = offset.saturating_add(buf.len()).min(data.len());
            let len = end - offset;
            buf[..len].copy_from_slice(&data[offset..end]);
            return Ok(len);
        }

        if offset >= inner.stat.size {
            return Ok(0);
        }

        const PAGE: usize = 4096;
        let end = offset.saturating_add(buf.len()).min(inner.stat.size);
        let len = end - offset;
        // Hot path (iozone 1k records on a fully-written file): copy each page
        // directly into buf, zero-fill only the holes. Avoids the previous
        // unconditional `buf[..len].fill(0)` that doubled byte traffic on every
        // read of dense files.
        let mut cur = offset;
        while cur < end {
            let page_idx = cur / PAGE;
            let page_off = cur % PAGE;
            let chunk = (PAGE - page_off).min(end - cur);
            let dst_off = cur - offset;
            match inner.sparse_pages.get(page_idx) {
                Some(page) => buf[dst_off..dst_off + chunk]
                    .copy_from_slice(&page[page_off..page_off + chunk]),
                None => buf[dst_off..dst_off + chunk].fill(0),
            }
            cur += chunk;
        }
        Ok(len)
    }

    fn read_to_user(
        &self,
        offset: usize,
        user_ptr: *mut u8,
        len: usize,
    ) -> Result<usize, FsError> {
        use crate::mm::{PTEFlags, VirtAddr, PAGE_SIZE};
        use crate::sched::process::PROCESS_TABLE;

        if self.file_type != FileType::Regular || len == 0 {
            return Err(FsError::InvalidArgument);
        }

        let inner = self.inner.lock();
        if offset >= inner.stat.size {
            return Ok(0);
        }
        let end = offset.saturating_add(len).min(inner.stat.size);
        let total = end - offset;

        let mut table_guard = PROCESS_TABLE.lock();
        let table = table_guard.as_mut().ok_or(FsError::NotFound)?;
        let current_pid = table.current.ok_or(FsError::NotFound)?;
        let as_pid = {
            let p = table.get(current_pid).ok_or(FsError::NotFound)?;
            if p.mm.is_some() { current_pid } else { p.tgid }
        };
        let process = table.get(as_pid).ok_or(FsError::NotFound)?;
        let mm = process.mm.as_ref().ok_or(FsError::NotFound)?;
        let mut address_space = mm.lock();

        let user_addr = user_ptr as usize;
        let mut file_cur = offset;

        // 批量零拷贝快路径：文件偏移与用户地址都页对齐时，把 range 内的
        // 文件页帧（洞页 → 共享零页）一次性别名映射进用户页表，一次 VMA
        // 查找摊薄全部页；稳态重复 read 走 Arc::ptr_eq 短路近乎零开销。
        if user_addr % PAGE_SIZE == 0 && offset % PAGE_SIZE == 0 && total >= PAGE_SIZE {
            let full_pages = total / PAGE_SIZE;
            let base_idx = offset / PAGE_SIZE;
            // 稳态零分配短路：重读同一段时帧已别名在位，裸指针逐页
            // 比对即可确认，不构造 Vec、不克隆 Arc、不碰页表/TLB。
            let zp_ptr = zero_page_ptr();
            let steady = {
                let mut cur = inner.sparse_pages.cursor();
                let ptrs = (base_idx..base_idx + full_pages).map_while(|idx| {
                    match cur.get(idx) {
                        Some(page) => Some(page.frame_ptr()),
                        None => zp_ptr,
                    }
                });
                address_space.alias_steady_prefix(VirtAddr(user_addr), ptrs)
            };
            file_cur += steady * PAGE_SIZE;
            if steady < full_pages {
                // 尾部未在位段：区间迭代一次定位（免逐页 BTreeMap 查找），
                // 洞页补共享零页，`&Arc` 迭代器直接喏给别名映射（零 Vec
                // 分配/零 Arc 克隆；滑窗重读每 read 整段换帧都走此路）。
                let sub_base = base_idx + steady;
                let zp = zero_page_frame();
                let mut cur = inner.sparse_pages.cursor();
                let frames = (sub_base..base_idx + full_pages).map_while(|idx| {
                    match cur.get(idx) {
                        Some(page) => Some(page.frame_ref()),
                        None => zp.as_ref(),
                    }
                });
                let n = address_space.alias_map_user_iter(
                    VirtAddr(user_addr + steady * PAGE_SIZE),
                    frames,
                );
                file_cur += n * PAGE_SIZE;
            }
        }
        while file_cur < end {
            let ramfs_page_idx = file_cur / PAGE_SIZE;
            let ramfs_page_off = file_cur % PAGE_SIZE;
            let chunk = (PAGE_SIZE - ramfs_page_off).min(end - file_cur);

            let user_off = file_cur - offset;
            let va = VirtAddr(user_addr + user_off);
            let vpn = va.floor();
            let page_off_in_user = va.page_offset();

            let user_chunk = chunk.min(PAGE_SIZE - page_off_in_user);

            // 零拷贝快路径：文件偏移与用户地址都页对齐、整页读、且该文件页
            // 已物化 —— 把文件页帧直接别名映射进用户页表（只读+COW），
            // 免掉 4 KiB memcpy（bw_file_rd io_only 热路径）。
            if page_off_in_user == 0
                && ramfs_page_off == 0
                && user_chunk == PAGE_SIZE
            {
                let frame = match inner.sparse_pages.get(ramfs_page_idx) {
                    Some(page) => Some(page.frame()),
                    // 洞页 → 共享零页（免掉 4 KiB memset）。
                    None => zero_page_frame(),
                };
                if let Some(frame) = frame {
                    if address_space.alias_map_user_page(va, frame) {
                        file_cur += PAGE_SIZE;
                        continue;
                    }
                }
            }

            let flags = address_space.page_table().get_pte_flags(vpn);
            match flags {
                Some(f) if !f.contains(PTEFlags::W) => {
                    if !address_space.handle_page_fault(va, true) {
                        return Err(FsError::InvalidArgument);
                    }
                }
                None => {
                    if !address_space.handle_page_fault(va, true) {
                        return Err(FsError::InvalidArgument);
                    }
                }
                _ => {}
            }
            let ppn = address_space.page_table().translate(vpn)
                .ok_or(FsError::InvalidArgument)?;
            let dst = crate::syscall::ppn_offset_to_kernel_ptr(ppn, page_off_in_user) as *mut u8;

            match inner.sparse_pages.get(ramfs_page_idx) {
                Some(page) => unsafe {
                    core::ptr::copy_nonoverlapping(
                        page[ramfs_page_off..].as_ptr(),
                        dst,
                        user_chunk,
                    );
                },
                None => unsafe {
                    core::ptr::write_bytes(dst, 0, user_chunk);
                },
            }
            file_cur += user_chunk;
        }
        Ok(total)
    }

    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        if self.file_type == FileType::CharDevice {
            return Err(FsError::InvalidArgument);
        }
        // hugetlb memfd：Linux hugetlbfs f_op 无 write_iter，write(2)/pwrite(2)
        // 一律 EINVAL（memfd_create03）。
        if self.memfd_hugetlb != 0 {
            return Err(FsError::InvalidArgument);
        }
        if buf.is_empty() {
            return Ok(0);
        }
        // F_SEAL_WRITE：完全禁写。F_SEAL_FUTURE_WRITE：等同 SEAL_WRITE 对后续
        // 任何 fd（我们没区分 future vs all，按全禁处理，保证 LTP memfd seals
        // 一致性）。返回 EPERM 与 Linux shmem_write_iter 一致。
        let seals = self.seals.load(core::sync::atomic::Ordering::Acquire);
        if seals & (F_SEAL_WRITE | F_SEAL_FUTURE_WRITE) != 0 {
            return Err(FsError::PermissionDenied);
        }
        let mut inner = self.inner.lock();

        // 若写入会超过当前 stat.size，等价于 grow；F_SEAL_GROW 禁止。
        let end = offset.checked_add(buf.len()).ok_or(FsError::NoSpace)?;
        if end > inner.stat.size && seals & F_SEAL_GROW != 0 {
            return Err(FsError::PermissionDenied);
        }

        if self.file_type == FileType::Symlink {
            if end > inner.data.len() {
                inner.data.resize(end, 0);
            }
            inner.data[offset..end].copy_from_slice(buf);
            if end > inner.stat.size {
                inner.stat.size = end;
            }
            let now = current_time_sec();
            inner.stat.mtime = now;
            inner.stat.ctime = now;
            return Ok(buf.len());
        }

        const PAGE: usize = 4096;
        let mut src_off = 0usize;
        let mut cur = offset;
        while src_off < buf.len() {
            let page_idx = cur / PAGE;
            let page_off = cur % PAGE;
            let chunk = (PAGE - page_off).min(buf.len() - src_off);
            let src = &buf[src_off..src_off + chunk];
            // The all-zero scan only matters for hole preservation on pages
            // that are not yet materialized; overwrites of existing pages
            // (stdio/iozone rewrite hot path) skip the O(chunk) scan and, like
            // Linux tmpfs, never collapse a materialized page back to a hole.
            let needs_new_page = !inner.sparse_pages.contains_key(page_idx);
            let all_zero = needs_new_page && slice_is_all_zero(src);

            if all_zero {
                // Hole write into an unallocated page remains a hole.
            } else {
                // Memory-pressure guard: a RAM-backed filesystem (tmpfs, and
                // ext4 COW overlay) has finite capacity bounded by physical
                // RAM. Allocating a fresh backing page once free memory has
                // dropped below a safety margin would eventually OOM-panic the
                // whole kernel (e.g. LTP dma_thread_diotest writing a ~1 GiB
                // file). Instead behave like a full disk: stop at the page
                // boundary and report a short write, or ENOSPC if nothing has
                // been written yet. Existing pages are always rewritable.
                if needs_new_page
                    && crate::mm::get_available_memory() < RAMFS_OOM_MARGIN
                {
                    if src_off == 0 {
                        return Err(FsError::NoSpace);
                    }
                    break;
                }
                if needs_new_page && page_off == 0 {
                    // Write that starts at a page boundary into a brand-new
                    // page: materialize it directly from the source and zero
                    // only the tail [chunk, PAGE) instead of allocating a
                    // zero-filled page (vec![0; 4096]) and overwriting the first
                    // `chunk` bytes — that double-writes the data region and
                    // memsets the whole page. Here we memset only PAGE-chunk
                    // bytes and never touch the data region twice. Eliminates
                    // the redundant 4 KiB memset on the sequential write hot
                    // path including the common non-page-aligned tail writes
                    // (lat_fs 1k/4k/10k, iozone). Reachable only when
                    // `all_zero` is false (an all-zero new page is handled as a
                    // hole above), so the hole-collapse check is unnecessary.
                    let page = match PageBuf::from_slice(src) {
                        Some(p) => p,
                        None => {
                            if src_off == 0 { return Err(FsError::NoSpace); }
                            break;
                        }
                    };
                    inner.sparse_pages.insert(page_idx, page);
                } else {
                    if !inner.sparse_pages.contains_key(page_idx) {
                        match PageBuf::new_zeroed() {
                            Some(p) => { inner.sparse_pages.insert(page_idx, p); }
                            None => {
                                if src_off == 0 { return Err(FsError::NoSpace); }
                                break;
                            }
                        }
                    }
                    let page = inner.sparse_pages.get_mut(page_idx).unwrap();
                    page.make_mut()[page_off..page_off + chunk].copy_from_slice(src);
                }
            }

            cur += chunk;
            src_off += chunk;
        }

        let written_end = offset + src_off;
        if written_end > inner.stat.size {
            inner.stat.size = written_end;
        }
        let now = current_time_sec();
        inner.stat.mtime = now;
        inner.stat.ctime = now;
        drop(inner);
        // inotify IN_MODIFY is emitted at the syscall layer (sys_write /
        // sys_writev / sys_pwrite64 / sys_pwritev) where the absolute fd path
        // is known. Ramfs cannot infer that here so it must stay silent —
        // otherwise wildcard double-deliveries leak into watch queues.
        Ok(src_off)
    }

    /// fallocate(2) preallocation: materialize backing pages for the range so
    /// later writes cannot fail with ENOSPC even after the fs fills up
    /// (fallocate06). Hole writes normally stay holes, so zero-filling via
    /// write() would not reserve anything — pages must be inserted directly.
    fn allocate(&self, offset: usize, len: usize) -> Result<(), FsError> {
        if self.file_type != FileType::Regular {
            return Ok(());
        }
        const PAGE: usize = 4096;
        // RAM-backed fs: a huge preallocation (e.g. the 1 GiB loop-device
        // backing image LTP creates) cannot be materialized without OOMing
        // the kernel. Keep those sparse (old behavior) and only truly
        // reserve modest ranges, which is what write-after-full-fs probes
        // like fallocate06 exercise.
        const MATERIALIZE_CAP: usize = 64 * 1024 * 1024;
        let end = offset.checked_add(len).ok_or(FsError::NoSpace)?;
        if len > MATERIALIZE_CAP {
            return Ok(());
        }
        let start_idx = offset / PAGE;
        let end_idx = (end + PAGE - 1) / PAGE;
        let mut inner = self.inner.lock();
        let missing = (start_idx..end_idx)
            .filter(|&idx| !inner.sparse_pages.contains_key(idx))
            .count();
        if crate::mm::get_available_memory() < RAMFS_OOM_MARGIN + missing * PAGE {
            // Not enough headroom to reserve for real — succeed sparsely
            // rather than fail an allocation Linux would satisfy on disk.
            return Ok(());
        }
        for idx in start_idx..end_idx {
            if inner.sparse_pages.contains_key(idx) {
                continue;
            }
            match PageBuf::new_zeroed() {
                Some(p) => {
                    inner.sparse_pages.insert(idx, p);
                }
                None => return Ok(()),
            }
        }
        Ok(())
    }

    fn truncate(&self, length: usize) -> Result<(), FsError> {
        if self.file_type == FileType::CharDevice {
            return Err(FsError::InvalidArgument);
        }
        let mut inner = self.inner.lock();
        let seals = self.seals.load(core::sync::atomic::Ordering::Acquire);
        // F_SEAL_SHRINK：禁止缩小。F_SEAL_GROW：禁止增大。
        if length < inner.stat.size && seals & F_SEAL_SHRINK != 0 {
            return Err(FsError::PermissionDenied);
        }
        if length > inner.stat.size && seals & F_SEAL_GROW != 0 {
            return Err(FsError::PermissionDenied);
        }

        if self.file_type == FileType::Symlink {
            inner.data.resize(length, 0);
            inner.stat.size = length;
            let now = current_time_sec();
            inner.stat.mtime = now;
            inner.stat.ctime = now;
            return Ok(());
        }

        const PAGE: usize = 4096;
        if length < inner.stat.size {
            let keep_pages = (length + PAGE - 1) / PAGE;
            inner.sparse_pages.remove_from(keep_pages);
            if length % PAGE != 0 {
                let tail_idx = length / PAGE;
                let tail_off = length % PAGE;
                if let Some(page) = inner.sparse_pages.get_mut(tail_idx) {
                    page.make_mut()[tail_off..].fill(0);
                    if page.as_slice().iter().all(|&b| b == 0) {
                        inner.sparse_pages.remove(tail_idx);
                    }
                }
            }
        }
        // grow is sparse: no page allocation, holes read as zero.
        inner.data.clear();
        inner.stat.size = length;
        let now = current_time_sec();
        inner.stat.mtime = now;
        inner.stat.ctime = now;
        Ok(())
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        let mut s = self.inner.lock().stat.clone();
        s.nlink = self.nlink.load(core::sync::atomic::Ordering::Relaxed);
        Ok(s)
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

impl Directory for RamInode {
    /// 真化跨 fs / 跨目录 rename / link 用：在 Directory trait object 上
    /// 也支持 downcast 到 RamInode，让 ramfs 内的另一 dir 能被识别。
    fn dir_as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let inner = self.inner.lock();
        let mut entries: Vec<DirEntry> = inner.children.iter().map(|(name, inode)| {
            DirEntry {
                name: String::from(name.as_str()),
                file_type: inode.file_type(),
                inode: inode.ino(),
            }
        }).collect();
        entries.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        let inner = self.inner.lock();
        inner.children.get(name)
            .cloned()
            .ok_or(FsError::NotFound)
    }

    fn create(&self, name: &str, file_type: FileType) -> Result<Arc<dyn Inode>, FsError> {
        let mut inner = self.inner.lock();

        if inner.children.contains_key(name) {
            return Err(FsError::AlreadyExists);
        }

        let ino = ramfs_alloc_ino();

        // Linux inode creation semantics:
        //   * normally new inode gid is current egid;
        //   * inside a directory with S_ISGID set, new inode gid inherits the
        //     parent directory gid (`inode_init_owner()` / `bsdgroups` rule).
        //
        // LTP mknod03 depends on this for FIFO nodes created after dropping to
        // an unprivileged egid inside an SGID directory.  Implement it at the
        // filesystem create layer, not in mknodat, so open(O_CREAT), mkdir,
        // symlink and socket-bind nodes all share the same VFS semantics.
        const S_ISGID: u32 = 0o2000;
        let inherit_parent_gid = inner.stat.mode.0 & S_ISGID != 0;
        let parent_gid = inner.stat.gid;

        // Slice 1.9 — use new_typed so mknodat(2) can create Pipe/Socket/
        // BlockDevice in addition to the legacy types. The legacy constructors
        // (new_file/new_dir/new_chardev/new_symlink) are kept for binary
        // compatibility but are equivalent to new_typed(...).
        let node = RamInode::new_typed(ino, file_type);
        if inherit_parent_gid {
            let mut child = node.inner.lock();
            child.stat.gid = parent_gid;
            if matches!(file_type, FileType::Directory) {
                child.stat.mode.0 |= S_ISGID;
            }
        }
        let inode: Arc<dyn Inode> = Arc::new(node);

        inner.children.insert(crate::fs::SsoKey::from(name), inode.clone());

        // inotify IN_CREATE is emitted by the syscall layer (sys_openat /
        // sys_mknodat / sys_mkdirat / sys_linkat / sys_symlinkat) where the
        // absolute parent path is available. Ramfs intentionally does not
        // notify here to avoid double events on the unsubscribable empty path.
        drop(inner);
        let _ = file_type;
        Ok(inode)
    }

    fn unlink(&self, name: &str) -> Result<(), FsError> {
        let mut inner = self.inner.lock();
        let removed = inner.children.remove(name).ok_or(FsError::NotFound)?;
        drop(inner);
        // 真 Linux `vfs_unlink`：移 dirent 后 nlink-=1； nlink 走到 0 后
        // Arc 计数为 0 时自然释放数据。
        if let Some(ram) = removed.as_any().downcast_ref::<RamInode>() {
            ram.dec_nlink();
        }
        // inotify IN_DELETE / IN_DELETE_SELF events are routed by the
        // syscall layer (sys_unlink / sys_unlinkat) with absolute paths.
        Ok(())
    }

    fn unlink_take(
        &self,
        name: &str,
        deny_dir: bool,
        deny_iflags: u32,
    ) -> Result<(Arc<dyn Inode>, FileType), FsError> {
        let mut inner = self.inner.lock();
        let o = match inner.children.entry_ref(name) {
            hashbrown::hash_map::EntryRef::Occupied(o) => o,
            hashbrown::hash_map::EntryRef::Vacant(_) => return Err(FsError::NotFound),
        };
        // 校验顺序对标 Linux may_delete：EPERM(iflags) 先于 EISDIR。
        // get_inode_flags 是原子读；file_type 持父 inner 锁内取子
        //（父→子锁序与 create 一致）。
        if deny_iflags != 0 && o.get().get_inode_flags() & deny_iflags != 0 {
            return Err(FsError::PermissionDenied);
        }
        let ft = o.get().file_type();
        if deny_dir && ft == FileType::Directory {
            return Err(FsError::IsDirectory);
        }
        let removed = o.remove();
        drop(inner);
        if let Some(ram) = removed.as_any().downcast_ref::<RamInode>() {
            ram.dec_nlink();
        }
        Ok((removed, ft))
    }

    /// 真 Linux `vfs_link`：给同一个 inode 多插一个 dirent，nlink+=1。
    /// `existing` 必须是 RamInode 同 fs。跨 fs 碍链路径 syscall 层返 -EXDEV。
    fn link(&self, name: &str, existing: &Arc<dyn Inode>) -> Result<(), FsError> {
        // Linux: hardlink 不能指向 directory，不能跨 fs。
        if existing.file_type() == FileType::Directory {
            return Err(FsError::PermissionDenied);
        }
        // 限 ramfs 同 fs（跨 fs 由 syscall 层检 EXDEV）。
        let ram = existing.as_any().downcast_ref::<RamInode>()
            .ok_or(FsError::PermissionDenied)?;
        let _ = ram; // 仅用于验证 same fs。
        let mut inner = self.inner.lock();
        if inner.children.contains_key(name) {
            return Err(FsError::AlreadyExists);
        }
        // 复用同一 Arc —— 这正是真 Linux i_nlink 语义。
        inner.children.insert(crate::fs::SsoKey::from(name), existing.clone());
        drop(inner);
        // nlink+=1：走真 RamInode 接口
        if let Some(ram) = existing.as_any().downcast_ref::<RamInode>() {
            ram.nlink.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }
        Ok(())
    }

    /// 同目录 rename：在 BTreeMap 中以原子方式将 dirent old_name 重新映射
    /// 为 new_name，保留同一个 RamInode（数据/inode 号不变）。覆盖 new_name
    /// 处既存目录项时遵循 POSIX rename(2) 语义：
    /// - 普通文件可被覆盖；
    /// - 目录只能被空目录覆盖（否则 ENOTEMPTY）；
    /// - 不允许将文件覆盖目录或反之（这里返回 EINVAL）。
    fn rename(&self, old_name: &str, new_name: &str) -> Result<(), FsError> {
        if old_name.is_empty() || new_name.is_empty()
            || old_name == "." || old_name == ".."
            || new_name == "." || new_name == ".."
        {
            return Err(FsError::InvalidArgument);
        }
        // POSIX：rename(a, a) 静默成功（若 a 存在）。
        if old_name == new_name {
            let inner = self.inner.lock();
            return if inner.children.contains_key(old_name) {
                Ok(())
            } else {
                Err(FsError::NotFound)
            };
        }
        let mut inner = self.inner.lock();
        // 1) old 必须存在
        let src = inner.children.get(old_name).cloned().ok_or(FsError::NotFound)?;
        // 2) 若 new 存在，按 POSIX 规则裁决覆盖合法性
        if let Some(dst) = inner.children.get(new_name).cloned() {
            // 同一 inode（硬链接/同对象）：noop 重命名
            if dst.ino() == src.ino() {
                inner.children.remove(old_name);
                return Ok(());
            }
            let src_is_dir = src.file_type() == FileType::Directory;
            let dst_is_dir = dst.file_type() == FileType::Directory;
            if src_is_dir != dst_is_dir {
                return Err(FsError::InvalidArgument);
            }
            if dst_is_dir {
                // 仅允许覆盖空目录——通过 Directory::read_dir
                if let Some(d) = dst.as_dir() {
                    match d.read_dir() {
                        Ok(v) if !v.is_empty() => return Err(FsError::NotEmpty),
                        Ok(_) => {}
                        Err(_) => return Err(FsError::IoError),
                    }
                }
            }
            // 覆盖：先把 dst 移除（旧 inode 释放），再做 dirent 重命名。
            inner.children.remove(new_name);
        }
        // 3) 原子地把 src 从 old_name 搬到 new_name
        let src_is_dir = src.file_type() == FileType::Directory;
        inner.children.remove(old_name);
        inner.children.insert(crate::fs::SsoKey::from(new_name), src);
        drop(inner);
        // inotify IN_MOVED_FROM / IN_MOVED_TO (with a shared cookie) are
        // emitted by the syscall layer (sys_renameat2) once the rename
        // succeeds; ramfs has no access to the absolute parent path and
        // therefore must not double-fire.
        let _ = src_is_dir;
        Ok(())
    }

    /// 跨目录 rename：当 new_dir 与 self 是同一个 RamInode（按 ino 判定）
    /// 时直接转发同目录路径；当 new_dir 是另一个 RamInode 时，按 ino 顺序
    /// 加锁两个 inner 以避免 ABBA 死锁；当 new_dir 不是 RamInode 时返回
    /// EXDEV（POSIX 跨文件系统 rename 不支持）。
    ///
    /// Slice 2.x 真化（去 `PermissionDenied` 简化）：通过 `dir_as_any` 把
    /// `&dyn Directory` 安全 downcast 到 `&RamInode`；按 (self.ino, other.ino)
    /// 升序双锁；按 POSIX `renameat(2)` 完整覆盖语义校验（src/dst type 一致、
    /// 目录覆盖须空、同 inode 时 noop）；最后把同一个 `Arc<dyn Inode>` 从
    /// self.children 移到 other.children，nlink 不变（dirent 重定向，与
    /// Linux `vfs_rename` 一致）。
    fn rename_to(
        &self,
        old_name: &str,
        new_dir: &dyn Directory,
        new_name: &str,
    ) -> Result<(), FsError> {
        // 同 inode：转发
        if let Some(other_ino) = new_dir.dir_ino_opt() {
            if other_ino == self.ino {
                return self.rename(old_name, new_name);
            }
        }
        // Same-mount peer may be bare RamInode *or* Ext4DirInode (overlay wrap).
        // Hold Ext4 overlay Arc for the duration of the double-lock critical section.
        let ext4_peer = new_dir
            .dir_as_any()
            .downcast_ref::<crate::fs::ext4::Ext4DirInode>()
            .map(|e| e.overlay_ram());
        let other: &RamInode = if let Some(r) = new_dir.dir_as_any().downcast_ref::<RamInode>() {
            r
        } else if let Some(ref arc) = ext4_peer {
            arc.as_ref()
        } else {
            return Err(FsError::PermissionDenied); // 真跨 fs → EXDEV
        };
        if old_name.is_empty() || new_name.is_empty()
            || old_name == "." || old_name == ".."
            || new_name == "." || new_name == ".."
        {
            return Err(FsError::InvalidArgument);
        }
        // ABBA 防护：按 ino 升序锁两边 inner。
        let (first, second) = if self.ino <= other.ino { (self, other) } else { (other, self) };
        let mut a = first.inner.lock();
        let mut b = second.inner.lock();
        // 让 `src_inner`/`dst_inner` 永远指向逻辑上的 self/other，简化下面逻辑。
        let (src_inner, dst_inner): (&mut RamInodeInner, &mut RamInodeInner) =
            if core::ptr::eq(first, self) { (&mut *a, &mut *b) } else { (&mut *b, &mut *a) };

        // 1) old 必须存在于 self
        let src = src_inner.children.get(old_name).cloned().ok_or(FsError::NotFound)?;
        // 2) 若 dst.new_name 已存在，按 POSIX 裁决
        if let Some(dst) = dst_inner.children.get(new_name).cloned() {
            if dst.ino() == src.ino() {
                // 同 inode → 直接移除 old_name 即可
                src_inner.children.remove(old_name);
                return Ok(());
            }
            let src_is_dir = src.file_type() == FileType::Directory;
            let dst_is_dir = dst.file_type() == FileType::Directory;
            if src_is_dir != dst_is_dir {
                return Err(FsError::InvalidArgument);
            }
            if dst_is_dir {
                if let Some(d) = dst.as_dir() {
                    match d.read_dir() {
                        Ok(v) if !v.is_empty() => return Err(FsError::NotEmpty),
                        Ok(_) => {}
                        Err(_) => return Err(FsError::IoError),
                    }
                }
            }
            dst_inner.children.remove(new_name);
        }
        // 3) 原子移动 dirent —— 同一 Arc，nlink 不变
        src_inner.children.remove(old_name);
        dst_inner.children.insert(crate::fs::SsoKey::from(new_name), src);
        Ok(())
    }

    fn dir_ino_opt(&self) -> Option<usize> {
        Some(self.ino)
    }

    /// RENAME_EXCHANGE — atomically swap two dirents inside the same directory.
    /// Both names must exist. Returns NotFound if either is missing.
    fn rename_exchange(&self, name_a: &str, name_b: &str) -> Result<(), FsError> {
        if name_a.is_empty() || name_b.is_empty()
            || name_a == "." || name_a == ".."
            || name_b == "." || name_b == ".."
        {
            return Err(FsError::InvalidArgument);
        }
        if name_a == name_b {
            // POSIX: exchange(x, x) is a no-op if x exists.
            let inner = self.inner.lock();
            return if inner.children.contains_key(name_a) { Ok(()) } else { Err(FsError::NotFound) };
        }
        let mut inner = self.inner.lock();
        let a = inner.children.get(name_a).cloned().ok_or(FsError::NotFound)?;
        let b = inner.children.get(name_b).cloned().ok_or(FsError::NotFound)?;
        // Atomic swap: replace both entries with each other.
        inner.children.insert(crate::fs::SsoKey::from(name_a), b);
        inner.children.insert(crate::fs::SsoKey::from(name_b), a);
        drop(inner);
        // inotify swap pair (IN_MOVED_FROM/IN_MOVED_TO x2) is emitted by the
        // syscall layer once `renameat2(... RENAME_EXCHANGE)` succeeds, so
        // this method stays silent (no path context here).
        Ok(())
    }

    /// Cross-dir RENAME_EXCHANGE：同 dir → 转发就地交换；同 fs 不同 dir →
    /// 真化做（之前是 PermissionDenied 简化）：按 ino 升序锁两边 inner，
    /// 双方 children\[name\] 都必须存在，然后用单一 Arc 互换 dirent；跨 fs
    /// 返 EXDEV 由 syscall 层映射。语义与 Linux `vfs_rename` 在
    /// `RENAME_EXCHANGE` 路径一致（dirent 重指，inode 不动，nlink 不变）。
    fn rename_exchange_to(
        &self,
        name_a: &str,
        other: &dyn Directory,
        name_b: &str,
    ) -> Result<(), FsError> {
        if let Some(o) = other.dir_ino_opt() {
            if o == self.ino {
                return self.rename_exchange(name_a, name_b);
            }
        }
        let other_ram = other.dir_as_any().downcast_ref::<RamInode>()
            .ok_or(FsError::PermissionDenied)?; // 跨 fs → EXDEV
        if name_a.is_empty() || name_b.is_empty()
            || name_a == "." || name_a == ".."
            || name_b == "." || name_b == ".."
        {
            return Err(FsError::InvalidArgument);
        }
        let (first, second) = if self.ino <= other_ram.ino {
            (self, other_ram)
        } else {
            (other_ram, self)
        };
        let mut a = first.inner.lock();
        let mut b = second.inner.lock();
        let (self_inner, other_inner): (&mut RamInodeInner, &mut RamInodeInner) =
            if core::ptr::eq(first, self) { (&mut *a, &mut *b) } else { (&mut *b, &mut *a) };
        // 双方都必须有对应 entry，否则 -ENOENT
        let arc_a = self_inner.children.get(name_a).cloned().ok_or(FsError::NotFound)?;
        let arc_b = other_inner.children.get(name_b).cloned().ok_or(FsError::NotFound)?;
        // 原子互换 dirent —— 同一 Arc，inode/nlink 不动
        self_inner.children.insert(crate::fs::SsoKey::from(name_a), arc_b);
        other_inner.children.insert(crate::fs::SsoKey::from(name_b), arc_a);
        Ok(())
    }

    /// Place a whiteout — char device (0,0) — at the given name. Used by
    /// `renameat2(... RENAME_WHITEOUT)` after the source has been moved out.
    fn whiteout(&self, name: &str) -> Result<(), FsError> {
        if name.is_empty() || name == "." || name == ".." {
            return Err(FsError::InvalidArgument);
        }
        let mut inner = self.inner.lock();
        if inner.children.contains_key(name) {
            return Err(FsError::AlreadyExists);
        }
        static NEXT_INO: AtomicUsize = AtomicUsize::new(0x4000_0000);
        let ino = NEXT_INO.fetch_add(1, Ordering::Relaxed);
        let inode: Arc<dyn Inode> = Arc::new(RamInode::new_typed(ino, FileType::CharDevice));
        inner.children.insert(crate::fs::SsoKey::from(name), inode);
        Ok(())
    }
}
