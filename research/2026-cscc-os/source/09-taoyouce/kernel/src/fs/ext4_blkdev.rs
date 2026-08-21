//! ext4 块设备适配层 — 桥接 lwext4_rust 和 VirtIO 块设备
//!
//! 🟡 微内核适配：实现 lwext4_rust 的 `BlockDevice` trait，
//! 将 lwext4 的块读写请求转发到 `virtio::blk::read_sectors_dev`/`write_sectors_dev`。
//!
//! lwext4_rust 内部使用 512 字节（`EXT4_DEV_BSIZE`）作为物理块大小，
//! 与 VirtIO 块设备的扇区大小一致，因此 block_id 可以直接作为扇区号使用。
//!
//! 🔴 自己写：
//! - 支持裸镜像（无分区表，超级块在偏移 1024 字节处）
//! - 可配置块大小（1024/2048/4096），由 lwext4 内部管理
//!
//! # 架构
//!
//! ```text
//! lwext4_rust (ext4 逻辑)
//!     ↓ BlockDevice trait (read_blocks/write_blocks, 512-byte blocks)
//! Ext4VirtioBlockDev (本文件)
//!     ↓ virtio::blk::read_sectors_dev / write_sectors_dev
//! VirtIO 块设备驱动 (virtio-drivers crate)
//! ```
//!
//! # 使用方式
//!
//! ```rust,no_run
//! use crate::fs::ext4_blkdev::Ext4VirtioBlockDev;
//! let dev = Ext4VirtioBlockDev::new(0); // VirtIO 块设备索引 0
//! // 传给 lwext4_rust::Ext4BlockDevice::new(dev)
//! ```

use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use crate::driver::virtio::blk;
use crate::sync::{SpinLock, TicketSpinLock};

/// VirtIO 块设备的扇区大小（字节），与 lwext4_rust 的 EXT4_DEV_BSIZE 一致
const SECTOR_SIZE: usize = 512;

/// 4KiB 对齐读缓存的块大小（扇区数）：lwext4 逻辑块 = 4KiB = 8 扇区。
const CACHE_CHUNK_SECTORS: u64 = 8;
/// 缓存容量上限（4KiB 条目数）：1024 × 4KiB = 4MiB，够覆盖热点元数据 +
/// 反复 exec 的测试二进制，又不至挤压 1G 内存（LA 曾出 OOM）。
const CACHE_MAX_CHUNKS: usize = 1024;

/// 设备级 4KiB 读缓存：key = (dev_idx, 对齐扇区号)。value 附带插入序号做
/// 近似 FIFO 淘汰（避免每次命中都更新 LRU 链的开销）。
/// 评测负载反复 exec 同一批测试二进制、重扫同一批目录块，lwext4 内部
/// bcache 很小，virtio-blk 同步往返（submit+poll+memcpy）是 libc-bench
/// 期间 ~130M 指令的大头；命中此缓存即免一次完整设备 IO。
static BLK_READ_CACHE: TicketSpinLock<BTreeMap<(usize, u64), (u64, Arc<Vec<u8>>)>> =
    SpinLock::new(BTreeMap::new());
static CACHE_SEQ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn cache_lookup(dev: usize, sector: u64) -> Option<Arc<Vec<u8>>> {
    BLK_READ_CACHE.lock().get(&(dev, sector)).map(|(_, d)| d.clone())
}

fn cache_insert(dev: usize, sector: u64, data: Arc<Vec<u8>>) {
    let seq = CACHE_SEQ.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let mut cache = BLK_READ_CACHE.lock();
    if cache.len() >= CACHE_MAX_CHUNKS {
        // 近似 FIFO：淘汰插入序号最小的条目。
        if let Some((&k, _)) = cache.iter().min_by_key(|(_, (s, _))| *s) {
            cache.remove(&k);
        }
    }
    cache.insert((dev, sector), (seq, data));
}

/// 写路径失效：移除与 [sector, sector+count) 重叠的所有 4KiB 缓存条目。
fn cache_invalidate(dev: usize, sector: u64, count: u64) {
    let first = sector / CACHE_CHUNK_SECTORS * CACHE_CHUNK_SECTORS;
    let last = (sector + count).div_ceil(CACHE_CHUNK_SECTORS) * CACHE_CHUNK_SECTORS;
    let mut cache = BLK_READ_CACHE.lock();
    let mut s = first;
    while s < last {
        cache.remove(&(dev, s));
        s += CACHE_CHUNK_SECTORS;
    }
}

/// ext4 VirtIO 块设备适配器
///
/// 桥接 lwext4_rust 的 `BlockDevice` trait 和 VirtIO 块设备驱动。
///
/// # 字段说明
///
/// - `dev_idx`: VirtIO 块设备索引（0 = 第一个块设备，通常是 sdcard/评测镜像）
/// - `part_offset`: 分区偏移（以扇区为单位）。裸镜像为 0，
///   有分区表时为分区起始扇区号。
///
/// # 裸镜像支持
///
/// 评测环境使用无分区表的 ext4 裸镜像，超级块位于磁盘偏移 1024 字节处
/// （即扇区 2 的偏移 0 处，或扇区 1 的偏移 512 处）。
/// lwext4 内部会自动定位超级块，适配层只需提供正确的块读写即可。
pub struct Ext4VirtioBlockDev {
    /// VirtIO 块设备索引
    dev_idx: usize,
    /// 分区偏移（以 512 字节扇区为单位），裸镜像为 0
    part_offset: u64,
}

impl Ext4VirtioBlockDev {
    /// 创建新的 ext4 块设备适配器（裸镜像，无分区偏移）
    ///
    /// # 参数
    /// - `dev_idx`: VirtIO 块设备索引
    ///
    /// # 示例
    /// ```rust,no_run
    /// let dev = Ext4VirtioBlockDev::new(0);
    /// ```
    pub fn new(dev_idx: usize) -> Self {
        Self {
            dev_idx,
            part_offset: 0,
        }
    }

    /// 创建带分区偏移的 ext4 块设备适配器
    ///
    /// # 参数
    /// - `dev_idx`: VirtIO 块设备索引
    /// - `part_offset_sectors`: 分区起始扇区号
    ///
    /// # 示例
    /// ```rust,no_run
    /// // 分区从扇区 2048 开始（常见的 MBR 分区对齐）
    /// let dev = Ext4VirtioBlockDev::with_offset(0, 2048);
    /// ```
    #[allow(dead_code)]
    pub fn with_offset(dev_idx: usize, part_offset_sectors: u64) -> Self {
        Self {
            dev_idx,
            part_offset: part_offset_sectors,
        }
    }

    /// 获取设备索引
    #[allow(dead_code)]
    pub fn dev_idx(&self) -> usize {
        self.dev_idx
    }

    /// 获取分区偏移（扇区数）
    #[allow(dead_code)]
    pub fn part_offset(&self) -> u64 {
        self.part_offset
    }
}

/// 实现 lwext4_rust 的 BlockDevice trait
///
/// lwext4_rust 使用 512 字节（EXT4_DEV_BSIZE）作为物理块大小。
/// `block_id` 是以 512 字节为单位的块号，直接对应 VirtIO 扇区号。
/// `buf` 的长度是 `block_count * 512` 字节。
///
/// lwext4 内部会根据 ext4 超级块中记录的逻辑块大小（1024/2048/4096）
/// 自动将逻辑块操作转换为多个 512 字节物理块操作。
impl lwext4_rust::BlockDevice for Ext4VirtioBlockDev {
    /// 从块设备读取数据
    ///
    /// # 参数
    /// - `block_id`: 起始块号（512 字节为单位）
    /// - `buf`: 读取缓冲区，长度为 N * 512 字节
    ///
    /// # 返回
    /// 成功时返回读取的字节数
    fn read_blocks(&mut self, block_id: u64, buf: &mut [u8]) -> lwext4_rust::Ext4Result<usize> {
        let actual_sector = block_id + self.part_offset;
        let sector_count = buf.len() / SECTOR_SIZE;

        if buf.len() % SECTOR_SIZE != 0 {
            return Err(lwext4_rust::Ext4Error::new(
                5, // EIO
                Some("read buffer not aligned to sector size"),
            ));
        }

        if sector_count == 0 {
            return Ok(0);
        }

        // 4KiB 对齐读走设备级缓存：命中免一次 virtio 同步往返。
        if actual_sector % CACHE_CHUNK_SECTORS == 0
            && sector_count as u64 == CACHE_CHUNK_SECTORS
        {
            if let Some(data) = cache_lookup(self.dev_idx, actual_sector) {
                let copy_len = core::cmp::min(data.len(), buf.len());
                buf[..copy_len].copy_from_slice(&data[..copy_len]);
                return Ok(buf.len());
            }
        }

        // 一次性批量读取整段连续扇区：lwext4 通常按逻辑块（4KiB = 8 扇区）请求，
        // 旧实现逐扇区发 virtio 请求会把 1 次 IO 放大成 8 次同步往返。
        // read_sectors_dev 支持单请求读取 count 个连续扇区，直接批量读入。
        match blk::read_sectors_dev(self.dev_idx, actual_sector, sector_count) {
            Ok(data) => {
                let copy_len = core::cmp::min(data.len(), buf.len());
                buf[..copy_len].copy_from_slice(&data[..copy_len]);
                if actual_sector % CACHE_CHUNK_SECTORS == 0
                    && sector_count as u64 == CACHE_CHUNK_SECTORS
                    && data.len() == (CACHE_CHUNK_SECTORS as usize) * SECTOR_SIZE
                {
                    cache_insert(self.dev_idx, actual_sector, Arc::new(data));
                }
                Ok(buf.len())
            }
            Err(_e) => Err(lwext4_rust::Ext4Error::new(
                5, // EIO
                Some("virtio-blk read failed"),
            )),
        }
    }

    /// 向块设备写入数据
    ///
    /// # 参数
    /// - `block_id`: 起始块号（512 字节为单位）
    /// - `buf`: 写入数据，长度为 N * 512 字节
    ///
    /// # 返回
    /// 成功时返回写入的字节数
    fn write_blocks(&mut self, block_id: u64, buf: &[u8]) -> lwext4_rust::Ext4Result<usize> {
        let actual_sector = block_id + self.part_offset;

        if buf.len() % SECTOR_SIZE != 0 {
            return Err(lwext4_rust::Ext4Error::new(
                5, // EIO
                Some("write buffer not aligned to sector size"),
            ));
        }

        if buf.is_empty() {
            return Ok(0);
        }

        // 将 buf 数据写入 VirtIO 块设备
        // write_sectors_dev 接受 Vec<u8>，需要从 buf 切片构造
        cache_invalidate(self.dev_idx, actual_sector, (buf.len() / SECTOR_SIZE) as u64);
        let data = Vec::from(buf);
        match blk::write_sectors_dev(self.dev_idx, actual_sector, data) {
            Ok(()) => Ok(buf.len()),
            Err(_e) => {
                Err(lwext4_rust::Ext4Error::new(
                    5, // EIO
                    Some("virtio-blk write failed"),
                ))
            }
        }
    }

    /// 获取设备总块数（512 字节为单位）
    ///
    /// 返回 VirtIO 块设备的总扇区数（减去分区偏移）。
    /// lwext4 内部会根据超级块中的 blocks_count 字段确定文件系统实际大小，
    /// 此处返回的是设备级别的容量上限。
    fn num_blocks(&self) -> lwext4_rust::Ext4Result<u64> {
        match blk::device_capacity(self.dev_idx) {
            Ok(total_sectors) => {
                // 减去分区偏移，返回可用扇区数
                let available = total_sectors.saturating_sub(self.part_offset);
                Ok(available)
            }
            Err(_e) => {
                Err(lwext4_rust::Ext4Error::new(
                    19, // ENODEV
                    Some("failed to get block device capacity"),
                ))
            }
        }
    }
}
