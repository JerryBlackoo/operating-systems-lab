//! io_uring 操作码定义
//!
//! 定义所有支持的 I/O 操作类型

/// io_uring 操作码
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoUringOp {
    /// 空操作（用于测试）
    Nop = 0,
    /// 读取文件
    Read = 1,
    /// 写入文件
    Write = 2,
    /// 同步文件数据
    Fsync = 3,
    /// 轮询文件描述符
    Poll = 4,
    /// 发送消息
    SendMsg = 5,
    /// 接收消息
    RecvMsg = 6,
    /// 超时
    Timeout = 7,
    /// 接受连接
    Accept = 8,
    /// 建立连接
    Connect = 9,
    /// 取消超时
    TimeoutRemove = 10,
    /// 取消操作
    Cancel = 11,
    /// 链接超时
    LinkTimeout = 12,
    /// 关闭文件
    Close = 13,
    /// 同步文件范围
    SyncFileRange = 14,
    /// 发送数据
    Send = 15,
    /// 接收数据
    Recv = 16,
    /// 打开文件
    Openat = 17,
    /// 关闭文件（带路径）
    Closeat = 18,
    /// 获取文件状态
    Statx = 19,
    /// 向量读
    Readv = 20,
    /// 向量写
    Writev = 21,
    /// 预读
    FAdvise = 22,
    /// 内存建议
    MAdvise = 23,
    /// 打开文件（直接）
    Openat2 = 24,
    /// 文件分配
    Fallocate = 25,
    /// 删除文件
    Unlinkat = 26,
    /// 重命名文件
    Renameat = 27,
    /// 创建目录
    Mkdirat = 28,
    /// 创建符号链接
    Symlinkat = 29,
    /// 创建硬链接
    Linkat = 30,
    /// 提供缓冲区
    ProvideBuffers = 31,
    /// 移除缓冲区
    RemoveBuffers = 32,
    /// 拼接
    Splice = 33,
    /// Tee
    Tee = 34,
    /// 关闭
    Shutdown = 35,
}

impl Default for IoUringOp {
    fn default() -> Self {
        Self::Nop
    }
}

impl IoUringOp {
    /// 从 u8 值创建操作码
    pub fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Nop,
            1 => Self::Read,
            2 => Self::Write,
            3 => Self::Fsync,
            4 => Self::Poll,
            5 => Self::SendMsg,
            6 => Self::RecvMsg,
            7 => Self::Timeout,
            8 => Self::Accept,
            9 => Self::Connect,
            10 => Self::TimeoutRemove,
            11 => Self::Cancel,
            12 => Self::LinkTimeout,
            13 => Self::Close,
            14 => Self::SyncFileRange,
            15 => Self::Send,
            16 => Self::Recv,
            17 => Self::Openat,
            18 => Self::Closeat,
            19 => Self::Statx,
            20 => Self::Readv,
            21 => Self::Writev,
            22 => Self::FAdvise,
            23 => Self::MAdvise,
            24 => Self::Openat2,
            25 => Self::Fallocate,
            26 => Self::Unlinkat,
            27 => Self::Renameat,
            28 => Self::Mkdirat,
            29 => Self::Symlinkat,
            30 => Self::Linkat,
            31 => Self::ProvideBuffers,
            32 => Self::RemoveBuffers,
            33 => Self::Splice,
            34 => Self::Tee,
            35 => Self::Shutdown,
            _ => Self::Nop, // 未知操作码默认为 Nop
        }
    }
    
    /// 检查操作是否需要文件描述符
    pub fn needs_fd(&self) -> bool {
        match self {
            Self::Nop | Self::Timeout | Self::TimeoutRemove | 
            Self::Cancel | Self::LinkTimeout | Self::ProvideBuffers |
            Self::RemoveBuffers => false,
            _ => true,
        }
    }

    /// 检查操作是否是读操作
    pub fn is_read(&self) -> bool {
        matches!(self, Self::Read | Self::Readv | Self::RecvMsg | Self::Recv)
    }

    /// 检查操作是否是写操作
    pub fn is_write(&self) -> bool {
        matches!(self, Self::Write | Self::Writev | Self::SendMsg | Self::Send)
    }

    /// 检查操作是否可以异步执行
    pub fn is_async(&self) -> bool {
        match self {
            Self::Nop | Self::Timeout | Self::TimeoutRemove |
            Self::Cancel | Self::LinkTimeout => false,
            _ => true,
        }
    }

    /// 获取操作名称
    pub fn name(&self) -> &'static str {
        match self {
            Self::Nop => "NOP",
            Self::Read => "READ",
            Self::Write => "WRITE",
            Self::Fsync => "FSYNC",
            Self::Poll => "POLL",
            Self::SendMsg => "SENDMSG",
            Self::RecvMsg => "RECVMSG",
            Self::Timeout => "TIMEOUT",
            Self::Accept => "ACCEPT",
            Self::Connect => "CONNECT",
            Self::TimeoutRemove => "TIMEOUT_REMOVE",
            Self::Cancel => "CANCEL",
            Self::LinkTimeout => "LINK_TIMEOUT",
            Self::Close => "CLOSE",
            Self::SyncFileRange => "SYNC_FILE_RANGE",
            Self::Send => "SEND",
            Self::Recv => "RECV",
            Self::Openat => "OPENAT",
            Self::Closeat => "CLOSEAT",
            Self::Statx => "STATX",
            Self::Readv => "READV",
            Self::Writev => "WRITEV",
            Self::FAdvise => "FADVISE",
            Self::MAdvise => "MADVISE",
            Self::Openat2 => "OPENAT2",
            Self::Fallocate => "FALLOCATE",
            Self::Unlinkat => "UNLINKAT",
            Self::Renameat => "RENAMEAT",
            Self::Mkdirat => "MKDIRAT",
            Self::Symlinkat => "SYMLINKAT",
            Self::Linkat => "LINKAT",
            Self::ProvideBuffers => "PROVIDE_BUFFERS",
            Self::RemoveBuffers => "REMOVE_BUFFERS",
            Self::Splice => "SPLICE",
            Self::Tee => "TEE",
            Self::Shutdown => "SHUTDOWN",
        }
    }
}

/// Poll 事件掩码
pub mod poll_events {
    /// 可读
    pub const POLLIN: u32 = 0x0001;
    /// 可写
    pub const POLLOUT: u32 = 0x0004;
    /// 错误
    pub const POLLERR: u32 = 0x0008;
    /// 挂起
    pub const POLLHUP: u32 = 0x0010;
    /// 无效
    pub const POLLNVAL: u32 = 0x0020;
    /// 优先数据可读
    pub const POLLPRI: u32 = 0x0002;
    /// 读挂起
    pub const POLLRDHUP: u32 = 0x2000;
}

/// Fsync 标志
pub mod fsync_flags {
    /// 只同步数据
    pub const DATASYNC: u32 = 1 << 0;
}

/// 超时标志
pub mod timeout_flags {
    /// 绝对时间
    pub const ABS: u32 = 1 << 0;
    /// 更新超时
    pub const UPDATE: u32 = 1 << 1;
    /// 引导时间
    pub const BOOTTIME: u32 = 1 << 2;
    /// 实时时间
    pub const REALTIME: u32 = 1 << 3;
    /// 链接超时更新
    pub const LINK_UPDATE: u32 = 1 << 4;
    /// 精确时间
    pub const ETIME_SUCCESS: u32 = 1 << 5;
}

/// 取消标志
pub mod cancel_flags {
    /// 取消所有
    pub const ALL: u32 = 1 << 0;
    /// 按文件描述符取消
    pub const FD: u32 = 1 << 1;
    /// 任意匹配
    pub const ANY: u32 = 1 << 2;
}

/// 打开文件标志
pub mod open_flags {
    /// 只读
    pub const O_RDONLY: u32 = 0;
    /// 只写
    pub const O_WRONLY: u32 = 1;
    /// 读写
    pub const O_RDWR: u32 = 2;
    /// 创建
    pub const O_CREAT: u32 = 0o100;
    /// 排他
    pub const O_EXCL: u32 = 0o200;
    /// 截断
    pub const O_TRUNC: u32 = 0o1000;
    /// 追加
    pub const O_APPEND: u32 = 0o2000;
    /// 非阻塞
    pub const O_NONBLOCK: u32 = 0o4000;
    /// 目录
    pub const O_DIRECTORY: u32 = 0o200000;
}
