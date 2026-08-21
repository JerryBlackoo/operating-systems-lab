//! This module provides the `libc` types for Types.
//!
//! MUSL: <https://github.com/bminor/musl/blob/c47ad25ea3b484e10326f933e927c0bc8cded3da/include/alltypes.h.in>

pub use crate::arch::{Stat, StatMode};
use crate::signal::SignalNum;
use core::{cmp::Ordering, ops::Add, time::Duration};
use num_enum::TryFromPrimitive;

/// IoVec structure
///
/// MUSL: <https://github.com/bminor/musl/blob/c47ad25ea3b484e10326f933e927c0bc8cded3da/include/alltypes.h.in#L78>
#[repr(C)]
#[derive(Clone)]
pub struct IoVec {
    /// Base address of the buffer
    pub base: usize,
    /// Length of the buffer
    pub len: usize,
}

/// TimeVal structure
///
/// MUSL: <https://github.com/bminor/musl/blob/c47ad25ea3b484e10326f933e927c0bc8cded3da/include/alltypes.h.in#L43>
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TimeVal {
    /// seconds, range in 0~999999999
    pub sec: usize,
    /// microseconds, range in 0~999999
    pub usec: usize,
}

impl Add for TimeVal {
    type Output = TimeVal;

    fn add(self, rhs: Self) -> Self::Output {
        let target = self.usec + rhs.usec;
        Self {
            sec: self.sec + rhs.sec + target / 1_000_000_000,
            usec: target % 1_000_000_000,
        }
    }
}

impl PartialOrd for TimeVal {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        let sec_res = self.sec.cmp(&other.sec);
        match sec_res {
            Ordering::Equal => Some(self.usec.cmp(&other.usec)),
            _ => Some(sec_res),
        }
    }
}

impl From<Duration> for TimeVal {
    fn from(value: Duration) -> Self {
        Self {
            sec: value.as_secs() as _,
            usec: value.subsec_micros() as _,
        }
    }
}

impl From<TimeVal> for Duration {
    fn from(value: TimeVal) -> Self {
        Duration::new(value.sec as _, value.usec as u32 * 1000)
    }
}

#[repr(C)]
/// 表示目录项（dirent）的结构体，用于读取目录内容（如 getdents64）
///
/// MUSL: <https://github.com/bminor/musl/blob/c47ad25ea3b484e10326f933e927c0bc8cded3da/arch/generic/bits/dirent.h#L5C1-L11C3>
pub struct Dirent64 {
    /// inode 号（索引节点号），唯一标识文件
    pub ino: u64,
    /// 偏移量，指向下一个目录项在目录流中的位置（用于遍历）
    pub off: i64,
    /// 当前目录项结构体的长度（包括文件名），单位是字节
    pub reclen: u16,
    /// 文件类型（如常规文件、目录、符号链接等）
    pub ftype: u8,
    /// 文件名（不定长，以空字符结尾，声明为 0 长度数组便于动态追加）
    pub name: [u8; 0],
}

#[repr(C)]
/// 文件系统状态结构体，用于 statfs 系统调用返回信息
///
/// MUSL: <https://github.com/bminor/musl/blob/c47ad25ea3b484e10326f933e927c0bc8cded3da/arch/generic/bits/statfs.h#L4>
pub struct StatFS {
    /// 文件系统的类型（magic number，用于标识如 ext4, tmpfs 等）
    pub ftype: u64,
    /// 最优传输块大小（用于文件系统的 I/O 操作优化）
    pub bsize: u64,
    /// 文件系统中数据块的总数量
    pub blocks: u64,
    /// 当前空闲的数据块数量（包括超级用户可用）
    pub bfree: u64,
    /// 普通用户可用的数据块数量（不包括超级用户保留）
    pub bavail: u64,
    /// 文件结点（i-node）总数，表示最多可创建的文件数量
    pub files: u64,
    /// 可用的文件结点数
    pub ffree: u64,
    /// 文件系统标识符（通常是一个唯一的 ID，用于区分不同的文件系统挂载点）
    pub fsid: u64,
    /// 支持的最大文件名长度（单位：字节）
    pub namelen: u64,
    /// Fragment size.
    pub frsize: u64,
    /// Mount flags.
    pub flags: u64,
    /// Reserved padding matching Linux statfs64 layout.
    pub spare: [u64; 4],
}

#[repr(C)]
#[derive(Default, Clone, Copy, Debug)]
/// 表示高精度时间的结构体（通常用于系统调用中的时间表示）
///
/// MUSL: <https://github.com/bminor/musl/blob/c47ad25ea3b484e10326f933e927c0bc8cded3da/include/alltypes.h.in#L44>
/// TODO: 对比 MUSL 中的定义
pub struct TimeSpec {
    /// 秒（Seconds）
    pub sec: usize,
    /// 纳秒（Nanoseconds），有效范围为 0~999_999_999
    pub nsec: usize,
}

impl TimeSpec {
    /// 将 TimeSpec 转换为纳秒（nanoseconds）
    pub const fn to_nsec(&self) -> usize {
        self.sec * 1_000_000_000 + self.nsec
    }
}

impl From<Duration> for TimeSpec {
    fn from(value: Duration) -> Self {
        Self {
            sec: value.as_secs() as _,
            nsec: value.subsec_nanos() as _,
        }
    }
}

impl From<TimeSpec> for Duration {
    fn from(value: TimeSpec) -> Self {
        Duration::new(value.sec as _, value.nsec as _)
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
/// Linux statx 时间戳结构体。
pub struct StatxTimestamp {
    /// 秒（Seconds）
    pub sec: i64,
    /// 纳秒（Nanoseconds）
    pub nsec: u32,
    /// 保留字段。
    pub __reserved: i32,
}

bitflags::bitflags! {
    /// statx 查询/返回掩码。
    #[derive(Debug, Clone, Copy)]
    pub struct StatxMask: u32 {
        /// 文件类型。
        const TYPE        = 0x0000_0001;
        /// 文件权限。
        const MODE        = 0x0000_0002;
        /// 硬链接数。
        const NLINK       = 0x0000_0004;
        /// 所有者 UID。
        const UID         = 0x0000_0008;
        /// 所有者 GID。
        const GID         = 0x0000_0010;
        /// 访问时间。
        const ATIME       = 0x0000_0020;
        /// 修改时间。
        const MTIME       = 0x0000_0040;
        /// 状态变更时间。
        const CTIME       = 0x0000_0080;
        /// inode 号。
        const INO         = 0x0000_0100;
        /// 文件大小。
        const SIZE        = 0x0000_0200;
        /// 块数。
        const BLOCKS      = 0x0000_0400;
        /// 基础 stat 字段。
        const BASIC_STATS = 0x0000_07ff;
        /// 创建时间。
        const BTIME       = 0x0000_0800;
        /// 挂载 ID。
        const MNT_ID      = 0x0000_1000;
        /// Direct I/O 对齐。
        const DIOALIGN    = 0x0000_2000;
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
/// Linux statx 系统调用返回结构体。
pub struct Statx {
    /// 返回字段掩码。
    pub mask: u32,
    /// 首选块大小。
    pub blksize: u32,
    /// 文件属性。
    pub attributes: u64,
    /// 硬链接数。
    pub nlink: u32,
    /// 所有者 UID。
    pub uid: u32,
    /// 所有者 GID。
    pub gid: u32,
    /// 文件模式。
    pub mode: u16,
    /// 保留字段。
    pub __spare0: [u16; 1],
    /// inode 号。
    pub ino: u64,
    /// 文件大小。
    pub size: u64,
    /// 512 字节块数。
    pub blocks: u64,
    /// 支持的属性掩码。
    pub attributes_mask: u64,
    /// 访问时间。
    pub atime: StatxTimestamp,
    /// 创建时间。
    pub btime: StatxTimestamp,
    /// 状态变更时间。
    pub ctime: StatxTimestamp,
    /// 修改时间。
    pub mtime: StatxTimestamp,
    /// 特殊文件主设备号。
    pub rdev_major: u32,
    /// 特殊文件次设备号。
    pub rdev_minor: u32,
    /// 文件所在设备主设备号。
    pub dev_major: u32,
    /// 文件所在设备次设备号。
    pub dev_minor: u32,
    /// 挂载 ID。
    pub mnt_id: u64,
    /// Direct I/O 内存对齐。
    pub dio_mem_align: u32,
    /// Direct I/O 偏移对齐。
    pub dio_offset_align: u32,
    /// 保留字段。
    pub __spare3: [u64; 12],
}

#[repr(C)]
#[derive(Clone, Copy)]
/// 终端窗口大小结构体
///
/// MUSL: <https://github.com/bminor/musl/blob/c47ad25ea3b484e10326f933e927c0bc8cded3da/include/alltypes.h.in#L80>
pub struct WinSize {
    /// 窗口的行数（以字符为单位）
    pub row: u16,
    /// 窗口的列数（以字符为单位）
    pub col: u16,
    /// 窗口的宽度（以像素为单位）
    pub xpixel: u16,
    /// 窗口的高度（以像素为单位）
    pub ypixel: u16,
}

/// 信号屏蔽操作方式（用于 `sigprocmask` 或类似接口）
///
/// MUSL: <https://github.com/bminor/musl/blob/c47ad25ea3b484e10326f933e927c0bc8cded3da/include/signal.h#L30>
#[repr(u8)]
#[derive(Debug, Clone, Copy, TryFromPrimitive)]
pub enum SigMaskHow {
    /// 阻塞指定信号（将信号加入进程的阻塞信号集）。
    Block = 0,
    /// 解除阻塞指定信号（将信号从阻塞信号集中移除）。
    Unblock = 1,
    /// 设置阻塞信号集为指定的信号集（替换整个信号集）。
    SetMask = 2,
}

/// 信号处理掩码（sigset_t）
#[repr(C)]
#[derive(Debug, Clone)]
pub struct SigSetExtended {
    /// 信号集，包含两个 64 位整数（128 位），用于存储信号的位掩码
    pub sigset: SigSet,
    /// 备用字段，通常用于对齐或扩展结构体大小
    pub __pad: [u64; (128 - size_of::<SigSet>()) / size_of::<u64>()],
}

impl SigSetExtended {
    /// 创建一个新的空信号集（无信号被阻塞）。
    pub const fn empty() -> Self {
        Self {
            sigset: SigSet::empty(),
            __pad: [0; (128 - size_of::<SigSet>()) / size_of::<u64>()],
        }
    }
}

/// 信号处理掩码结构体
///
/// MUSL: <https://github.com/bminor/musl/blob/c47ad25ea3b484e10326f933e927c0bc8cded3da/include/alltypes.h.in>
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SigSet(u64);

impl SigSet {
    /// 创建一个新的空信号集（无信号被阻塞）。
    pub const fn empty() -> Self {
        Self(0)
    }

    /// 修改当前信号集的行为。
    ///
    /// - `how` 指定操作方式（阻塞、解除阻塞、设为新掩码）。
    /// - `mask` 是将要应用的信号集。
    pub const fn handle(&mut self, how: SigMaskHow, mask: &Self) {
        self.0 = match how {
            SigMaskHow::Block => self.0 | mask.0, // 阻塞指定信号（按位或）
            SigMaskHow::Unblock => self.0 & (!mask.0), // 解除阻塞（按位与非）
            SigMaskHow::SetMask => mask.0,        // 设置为指定掩码
        }
    }

    /// 将指定信号添加到信号集中。
    ///
    /// # 参数
    ///
    /// - `signum`: [SignalNum] 信号
    pub const fn insert(&mut self, signum: SignalNum) {
        self.0 |= signum.mask();
    }

    /// 从信号集中移除指定信号。
    ///
    /// # 参数
    ///
    /// - `signum`: [SignalNum] 信号
    pub const fn remove(&mut self, signum: SignalNum) {
        self.0 &= !(signum.mask());
    }

    /// 判断指定信号是否在信号集中。
    ///
    /// # 参数
    ///
    /// - `signum`: [SignalNum] 信号
    ///
    /// # 返回
    ///
    /// - `true`： 该信号在信号集中
    /// - `false`：该信号不在信号集中
    pub const fn has(&self, signum: SignalNum) -> bool {
        self.0 & signum.mask() != 0
    }

    /// 判断信号集是否为空。
    ///
    /// # 返回
    ///
    /// - `true`：信号集为空
    /// - `false`：信号集非空
    pub const fn is_empty(&self, masked: Option<Self>) -> bool {
        match masked {
            Some(masked) => self.0 & !masked.0 == 0,
            None => self.0 == 0,
        }
    }

    /// 从信号集中弹出一个信号
    ///
    /// # 参数
    ///
    /// - `masked`: 可选的信号集，用于屏蔽信号
    ///
    /// # 返回
    ///
    /// - `Some(SignalNum)`：如果信号集非空，返回一个信号
    /// - `None`: 如果已经没有信号可以弹出，返回 [None]
    #[inline]
    pub fn pop_one(&mut self, masked: Option<Self>) -> Option<SignalNum> {
        let set = self.0 & !masked.map_or(0, |m| m.0);
        let sig_bit_idx = set.trailing_zeros();
        if sig_bit_idx == 64 {
            return None;
        }
        self.0 &= !bit!(sig_bit_idx);
        SignalNum::try_from(sig_bit_idx as u8 + 1).ok()
    }
}
