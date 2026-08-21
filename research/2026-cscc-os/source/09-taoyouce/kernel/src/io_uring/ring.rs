//! io_uring 环形缓冲区实现
//!
//! 实现提交队列 (SQ) 和完成队列 (CQ) 的环形缓冲区

use alloc::vec::Vec;
use alloc::vec;
use core::sync::atomic::{AtomicU32, Ordering};
use crate::sync::SpinLock;
use super::{IoUringError, IoUringOp};

/// 提交队列条目 (SQE)
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SubmissionQueueEntry {
    /// 操作码
    pub opcode: IoUringOp,
    /// 标志位
    pub flags: SqeFlags,
    /// I/O 优先级
    pub ioprio: u16,
    /// 文件描述符
    pub fd: i32,
    /// 偏移量
    pub off: u64,
    /// 缓冲区地址
    pub addr: u64,
    /// 长度
    pub len: u32,
    /// 操作特定标志
    pub op_flags: u32,
    /// 用户数据（用于关联请求和响应）
    pub user_data: u64,
    /// 缓冲区索引（用于固定缓冲区）
    pub buf_index: u16,
    /// 个性化 ID
    pub personality: u16,
    /// 文件索引（用于固定文件）
    pub file_index: i32,
    /// 保留字段
    pub __pad: [u64; 2],
}

impl Default for SubmissionQueueEntry {
    fn default() -> Self {
        Self {
            opcode: IoUringOp::Nop,
            flags: SqeFlags::empty(),
            ioprio: 0,
            fd: -1,
            off: 0,
            addr: 0,
            len: 0,
            op_flags: 0,
            user_data: 0,
            buf_index: 0,
            personality: 0,
            file_index: -1,
            __pad: [0; 2],
        }
    }
}

impl SubmissionQueueEntry {
    /// 创建 NOP 请求
    pub fn nop(user_data: u64) -> Self {
        Self {
            opcode: IoUringOp::Nop,
            user_data,
            ..Default::default()
        }
    }

    /// 创建读请求
    pub fn read(fd: i32, buf: u64, len: u32, offset: u64, user_data: u64) -> Self {
        Self {
            opcode: IoUringOp::Read,
            fd,
            addr: buf,
            len,
            off: offset,
            user_data,
            ..Default::default()
        }
    }

    /// 创建写请求
    pub fn write(fd: i32, buf: u64, len: u32, offset: u64, user_data: u64) -> Self {
        Self {
            opcode: IoUringOp::Write,
            fd,
            addr: buf,
            len,
            off: offset,
            user_data,
            ..Default::default()
        }
    }

    /// 创建 fsync 请求
    pub fn fsync(fd: i32, user_data: u64) -> Self {
        Self {
            opcode: IoUringOp::Fsync,
            fd,
            user_data,
            ..Default::default()
        }
    }

    /// 创建 poll 请求
    pub fn poll(fd: i32, poll_mask: u32, user_data: u64) -> Self {
        Self {
            opcode: IoUringOp::Poll,
            fd,
            op_flags: poll_mask,
            user_data,
            ..Default::default()
        }
    }
}

bitflags::bitflags! {
    /// SQE 标志
    #[derive(Default, Clone, Copy, Debug)]
    pub struct SqeFlags: u8 {
        /// 固定文件
        const FIXED_FILE = 1 << 0;
        /// I/O 排空
        const IO_DRAIN = 1 << 1;
        /// I/O 链接
        const IO_LINK = 1 << 2;
        /// 硬链接
        const IO_HARDLINK = 1 << 3;
        /// 异步
        const ASYNC = 1 << 4;
        /// 缓冲区选择
        const BUFFER_SELECT = 1 << 5;
    }
}

/// 完成队列条目 (CQE)
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CompletionQueueEntry {
    /// 用户数据（从 SQE 复制）
    pub user_data: u64,
    /// 操作结果（正数为成功，负数为错误码）
    pub res: i32,
    /// 标志位
    pub flags: u32,
}

impl CompletionQueueEntry {
    /// 创建成功的 CQE
    pub fn success(user_data: u64, result: i32) -> Self {
        Self {
            user_data,
            res: result,
            flags: 0,
        }
    }

    /// 创建失败的 CQE
    pub fn error(user_data: u64, error: i32) -> Self {
        Self {
            user_data,
            res: error,  // error 应该已经是负数
            flags: 0,
        }
    }

    /// 检查是否成功
    pub fn is_success(&self) -> bool {
        self.res >= 0
    }
}

/// 提交队列
pub struct SubmissionQueue {
    /// 头指针（内核读取位置）
    head: AtomicU32,
    /// 尾指针（用户写入位置）
    tail: AtomicU32,
    /// 队列大小掩码
    ring_mask: u32,
    /// 队列条目数
    ring_entries: u32,
    /// SQE 数组
    sqes: SpinLock<Vec<SubmissionQueueEntry>>,
    /// 丢弃的请求数
    dropped: AtomicU32,
}

impl SubmissionQueue {
    /// 创建新的提交队列
    pub fn new(size: usize) -> Result<Self, IoUringError> {
        if size == 0 || !size.is_power_of_two() {
            return Err(IoUringError::InvalidArgument);
        }

        let sqes = vec![SubmissionQueueEntry::default(); size];

        Ok(Self {
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
            ring_mask: (size - 1) as u32,
            ring_entries: size as u32,
            sqes: SpinLock::new(sqes),
            dropped: AtomicU32::new(0),
        })
    }

    /// 推入 SQE
    pub fn push(&self, sqe: SubmissionQueueEntry) -> Result<(), IoUringError> {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);

        // 检查队列是否已满
        if tail.wrapping_sub(head) >= self.ring_entries {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return Err(IoUringError::QueueFull);
        }

        let index = (tail & self.ring_mask) as usize;
        {
            let mut sqes = self.sqes.lock();
            sqes[index] = sqe;
        }

        // 更新尾指针
        self.tail.store(tail.wrapping_add(1), Ordering::Release);

        Ok(())
    }

    /// 弹出 SQE
    pub fn pop(&self) -> Option<SubmissionQueueEntry> {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);

        if head == tail {
            return None;
        }

        let index = (head & self.ring_mask) as usize;
        let sqe = {
            let sqes = self.sqes.lock();
            sqes[index]
        };

        // 更新头指针
        self.head.store(head.wrapping_add(1), Ordering::Release);

        Some(sqe)
    }

    /// 获取队列长度
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head) as usize
    }

    /// 检查队列是否为空
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// 检查队列是否已满
    pub fn is_full(&self) -> bool {
        self.len() >= self.ring_entries as usize
    }

    /// 获取丢弃的请求数
    pub fn dropped(&self) -> u32 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// 获取队列容量
    pub fn capacity(&self) -> usize {
        self.ring_entries as usize
    }
}

/// 完成队列
pub struct CompletionQueue {
    /// 头指针（用户读取位置）
    head: AtomicU32,
    /// 尾指针（内核写入位置）
    tail: AtomicU32,
    /// 队列大小掩码
    ring_mask: u32,
    /// 队列条目数
    ring_entries: u32,
    /// CQE 数组
    cqes: SpinLock<Vec<CompletionQueueEntry>>,
    /// 溢出计数
    overflow: AtomicU32,
}

impl CompletionQueue {
    /// 创建新的完成队列
    pub fn new(size: usize) -> Result<Self, IoUringError> {
        if size == 0 || !size.is_power_of_two() {
            return Err(IoUringError::InvalidArgument);
        }

        let cqes = vec![CompletionQueueEntry { user_data: 0, res: 0, flags: 0 }; size];

        Ok(Self {
            head: AtomicU32::new(0),
            tail: AtomicU32::new(0),
            ring_mask: (size - 1) as u32,
            ring_entries: size as u32,
            cqes: SpinLock::new(cqes),
            overflow: AtomicU32::new(0),
        })
    }

    /// 推入 CQE
    pub fn push(&self, cqe: CompletionQueueEntry) -> Result<(), IoUringError> {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);

        // 检查队列是否已满
        if tail.wrapping_sub(head) >= self.ring_entries {
            self.overflow.fetch_add(1, Ordering::Relaxed);
            return Err(IoUringError::QueueFull);
        }

        let index = (tail & self.ring_mask) as usize;
        {
            let mut cqes = self.cqes.lock();
            cqes[index] = cqe;
        }

        // 更新尾指针
        self.tail.store(tail.wrapping_add(1), Ordering::Release);

        Ok(())
    }

    /// 弹出 CQE
    pub fn pop(&self) -> Option<CompletionQueueEntry> {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);

        if head == tail {
            return None;
        }

        let index = (head & self.ring_mask) as usize;
        let cqe = {
            let cqes = self.cqes.lock();
            cqes[index]
        };

        // 更新头指针
        self.head.store(head.wrapping_add(1), Ordering::Release);

        Some(cqe)
    }

    /// 获取队列长度
    pub fn len(&self) -> usize {
        let tail = self.tail.load(Ordering::Acquire);
        let head = self.head.load(Ordering::Acquire);
        tail.wrapping_sub(head) as usize
    }

    /// 检查队列是否为空
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Total number of CQEs ever produced (monotonic write cursor).
    pub fn total_completed(&self) -> u32 {
        self.tail.load(Ordering::Acquire)
    }

    /// 获取溢出计数
    pub fn overflow(&self) -> u32 {
        self.overflow.load(Ordering::Relaxed)
    }

    /// 获取队列容量
    pub fn capacity(&self) -> usize {
        self.ring_entries as usize
    }

    /// 查看队首 CQE（不移除）
    pub fn peek(&self) -> Option<CompletionQueueEntry> {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);

        if head == tail {
            return None;
        }

        let index = (head & self.ring_mask) as usize;
        let cqes = self.cqes.lock();
        Some(cqes[index])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submission_queue_basic() {
        let sq = SubmissionQueue::new(4).unwrap();
        assert!(sq.is_empty());
        assert_eq!(sq.capacity(), 4);

        // 推入
        let sqe = SubmissionQueueEntry::nop(42);
        sq.push(sqe).unwrap();
        assert_eq!(sq.len(), 1);

        // 弹出
        let sqe = sq.pop().unwrap();
        assert_eq!(sqe.user_data, 42);
        assert!(sq.is_empty());
    }

    #[test]
    fn test_completion_queue_basic() {
        let cq = CompletionQueue::new(4).unwrap();
        assert!(cq.is_empty());

        // 推入
        let cqe = CompletionQueueEntry::success(42, 100);
        cq.push(cqe).unwrap();
        assert_eq!(cq.len(), 1);

        // 弹出
        let cqe = cq.pop().unwrap();
        assert_eq!(cqe.user_data, 42);
        assert_eq!(cqe.res, 100);
        assert!(cq.is_empty());
    }
}
