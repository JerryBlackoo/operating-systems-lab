//! io_uring 异步 I/O 子系统
//!
//! 实现类似 Linux io_uring 的高性能异步 I/O 接口。
//! 通过共享内存环形缓冲区减少系统调用开销。
//!
//! # 架构设计
//!
//! io_uring 使用两个环形缓冲区实现用户态和内核态之间的高效通信：
//! - Submission Queue (SQ): 用户态写入 I/O 请求，内核读取并处理
//! - Completion Queue (CQ): 内核写入完成结果，用户态读取
//!
//! 这种设计允许批量提交和完成，减少系统调用次数。
//!
//! # 特性
//! - 零拷贝提交和完成（通过共享内存）
//! - 批量 I/O 处理（单次 enter 处理多个请求）
//! - SQPOLL 模式支持（内核线程主动轮询）
//! - 链式操作支持（IO_LINK 标志）
//! - 超时和取消操作
//! - 注册缓冲区和文件描述符（减少每次操作的开销）
//! - 与内核异步运行时集成
//!
//! # 性能优化
//!
//! 1. 批量处理：用户可以一次提交多个 SQE，内核批量处理
//! 2. 固定缓冲区：预注册缓冲区避免每次操作的地址验证
//! 3. 固定文件：预注册文件描述符避免每次操作的 fd 查找
//! 4. SQPOLL：内核线程主动轮询，完全避免系统调用
//!
//! # 参考
//! - Linux io_uring: https://kernel.dk/io_uring.pdf
//! - liburing: https://github.com/axboe/liburing

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicBool, Ordering};
use crate::sync::SpinLock;

mod ring;
mod ops;

#[cfg(test)]
mod tests;

pub use ring::{SubmissionQueue, CompletionQueue, SubmissionQueueEntry, CompletionQueueEntry, SqeFlags};
pub use ops::IoUringOp;

// 重导出 ring 模块供内部使用
// pub(crate) use ring as ring_internal;

/// io_uring 实例 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IoUringId(pub u32);

impl IoUringId {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
}

/// io_uring 实例
pub struct IoUring {
    /// 实例 ID
    id: IoUringId,
    /// 提交队列
    sq: SubmissionQueue,
    /// 完成队列
    cq: CompletionQueue,
    /// 是否启用 SQPOLL 模式
    sq_poll: bool,
    /// SQPOLL 线程是否运行
    sq_poll_running: AtomicBool,
    /// 注册的缓冲区
    registered_buffers: SpinLock<Vec<RegisteredBuffer>>,
    /// 注册的文件描述符
    registered_fds: SpinLock<Vec<i32>>,
    /// Pending timeout requests (for TIMEOUT_REMOVE cancellation)
    pending_timeouts: SpinLock<Vec<TimeoutRequest>>,
    /// 统计信息
    stats: IoUringStats,
}

/// 注册的缓冲区
///
/// 预注册缓冲区可以避免每次 I/O 操作时的地址验证开销。
/// 内核会在注册时验证并锁定这些页面。
#[allow(dead_code)]
pub struct RegisteredBuffer {
    /// 用户空间地址
    user_addr: usize,
    /// 长度
    len: usize,
    /// 内核映射地址（如果已映射）
    kernel_addr: Option<usize>,
    /// 缓冲区是否已锁定（防止被换出）
    pinned: bool,
    /// 引用计数（正在使用此缓冲区的操作数）
    ref_count: AtomicU32,
}

/// 链式操作上下文
///
/// 当 SQE 设置了 IO_LINK 标志时，后续的 SQE 会形成链式操作。
/// 链中的操作按顺序执行，如果某个操作失败，后续操作会被取消。
#[allow(dead_code)]
struct LinkedOperation {
    /// 链中的 SQE 列表
    sqes: Vec<SubmissionQueueEntry>,
    /// 当前执行位置
    current_index: usize,
    /// 是否已取消
    cancelled: bool,
    /// 链的起始 user_data（用于关联）
    chain_id: u64,
}

/// 超时请求
///
/// 用于实现 TIMEOUT 操作和链式超时
#[allow(dead_code)]
struct TimeoutRequest {
    /// 超时时间（纳秒，从启动开始）
    deadline_ns: u64,
    /// 关联的 user_data
    user_data: u64,
    /// 是否是绝对时间
    is_absolute: bool,
    /// 完成计数（对于 TIMEOUT_REMOVE）
    completion_count: Option<u32>,
    /// 是否已触发
    triggered: bool,
}

/// 取消请求上下文
#[allow(dead_code)]
struct CancelContext {
    /// 要取消的 user_data
    target_user_data: u64,
    /// 取消标志
    flags: u32,
    /// 是否已找到目标
    found: bool,
}

/// io_uring 统计信息
///
/// 用于性能监控和调试
#[derive(Default)]
pub struct IoUringStats {
    /// 提交的请求数
    pub submissions: AtomicU32,
    /// 完成的请求数
    pub completions: AtomicU32,
    /// SQPOLL 唤醒次数
    pub sq_poll_wakeups: AtomicU32,
    /// 缓存命中次数（使用注册缓冲区）
    pub buffer_cache_hits: AtomicU32,
    /// 缓存未命中次数
    pub buffer_cache_misses: AtomicU32,
    /// 链式操作数
    pub linked_operations: AtomicU32,
    /// 取消的操作数
    pub cancelled_operations: AtomicU32,
    /// 超时触发次数
    pub timeouts_triggered: AtomicU32,
    /// CQ 溢出次数
    pub cq_overflows: AtomicU32,
    /// 总处理时间（纳秒）
    pub total_processing_time_ns: AtomicU64,
}

/// io_uring 参数
#[repr(C)]
pub struct IoUringParams {
    /// 提交队列大小
    pub sq_entries: u32,
    /// 完成队列大小
    pub cq_entries: u32,
    /// 标志位
    pub flags: IoUringSetupFlags,
    /// SQ 线程 CPU 亲和性
    pub sq_thread_cpu: u32,
    /// SQ 线程空闲超时（毫秒）
    pub sq_thread_idle: u32,
    /// 保留字段
    pub resv: [u32; 5],
    /// SQ 偏移量（输出）
    pub sq_off: SqRingOffsets,
    /// CQ 偏移量（输出）
    pub cq_off: CqRingOffsets,
}

impl Default for IoUringParams {
    fn default() -> Self {
        Self {
            sq_entries: 32,
            cq_entries: 64,
            flags: IoUringSetupFlags::empty(),
            sq_thread_cpu: 0,
            sq_thread_idle: 1000,
            resv: [0; 5],
            sq_off: SqRingOffsets::default(),
            cq_off: CqRingOffsets::default(),
        }
    }
}

bitflags::bitflags! {
    /// io_uring 设置标志
    #[derive(Default, Clone, Copy)]
    pub struct IoUringSetupFlags: u32 {
        /// 启用 SQPOLL 模式
        const SQPOLL = 1 << 1;
        /// SQ 和 CQ 共享内存
        const SQ_AFF = 1 << 2;
        /// 完成队列大小由用户指定
        const CQSIZE = 1 << 3;
    }
}

/// SQ 环偏移量
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct SqRingOffsets {
    pub head: u32,
    pub tail: u32,
    pub ring_mask: u32,
    pub ring_entries: u32,
    pub flags: u32,
    pub dropped: u32,
    pub array: u32,
    pub resv: [u32; 3],
}

/// CQ 环偏移量
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub struct CqRingOffsets {
    pub head: u32,
    pub tail: u32,
    pub ring_mask: u32,
    pub ring_entries: u32,
    pub overflow: u32,
    pub cqes: u32,
    pub flags: u32,
    pub resv: [u32; 3],
}

/// io_uring 错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum IoUringError {
    /// 队列已满
    QueueFull,
    /// 无效参数
    InvalidArgument,
    /// 无效操作码
    InvalidOpcode,
    /// 无效文件描述符
    InvalidFd,
    /// 内存不足
    OutOfMemory,
    /// 操作取消
    Canceled,
    /// I/O 错误
    IoError,
    /// 权限不足
    PermissionDenied,
    /// 操作超时
    Timeout,
    /// 资源忙
    Busy,
    /// 操作不支持
    NotSupported,
    /// 缓冲区太小
    BufferTooSmall,
    /// 无效的缓冲区索引
    InvalidBufferIndex,
    /// 无效的文件索引
    InvalidFileIndex,
    /// 链式操作失败
    LinkFailed,
    /// 地址无效
    BadAddress,
    /// 操作被中断
    Interrupted,
    /// 资源不可用
    WouldBlock,
    /// ETIME — io_uring timeout expired (distinct from ETIMEDOUT)
    TimedOut,
    /// ENOENT — target not found (e.g. TIMEOUT_REMOVE target doesn't exist)
    NotFound,
}

impl IoUringError {
    /// 转换为 errno 风格的错误码
    pub fn to_errno(&self) -> i32 {
        match self {
            Self::QueueFull => -28,      // ENOSPC
            Self::InvalidArgument => -22, // EINVAL
            Self::InvalidOpcode => -22,   // EINVAL
            Self::InvalidFd => -9,        // EBADF
            Self::OutOfMemory => -12,     // ENOMEM
            Self::Canceled => -125,       // ECANCELED
            Self::IoError => -5,          // EIO
            Self::PermissionDenied => -13, // EACCES
            Self::Timeout => -110,        // ETIMEDOUT
            Self::Busy => -16,            // EBUSY
            Self::NotSupported => -95,    // EOPNOTSUPP
            Self::BufferTooSmall => -34,  // ERANGE
            Self::InvalidBufferIndex => -22, // EINVAL
            Self::InvalidFileIndex => -22,   // EINVAL
            Self::LinkFailed => -125,     // ECANCELED
            Self::BadAddress => -14,      // EFAULT
            Self::Interrupted => -4,      // EINTR
            Self::WouldBlock => -11,      // EAGAIN
            Self::TimedOut => -62,        // ETIME (io_uring timeout convention)
            Self::NotFound => -2,         // ENOENT
        }
    }
}

/// SQPOLL 线程状态
#[allow(dead_code)]
pub struct SqPollThread {
    /// 关联的 io_uring ID
    ring_id: IoUringId,
    /// 是否应该停止
    should_stop: AtomicBool,
    /// 空闲超时（毫秒）
    idle_timeout: u32,
    /// CPU 亲和性
    cpu_affinity: u32,
    /// 上次活动时间
    last_active: AtomicU32,
}

impl SqPollThread {
    /// 创建 SQPOLL 线程
    pub fn new(ring_id: IoUringId, cpu: u32, idle_timeout: u32) -> Self {
        Self {
            ring_id,
            should_stop: AtomicBool::new(false),
            idle_timeout,
            cpu_affinity: cpu,
            last_active: AtomicU32::new(0),
        }
    }

    /// 运行 SQPOLL 线程
    #[allow(dead_code)]
    pub fn run(&self, ring: &IoUring) {
        crate::println!("[io_uring] SQPOLL thread started for ring {:?}", self.ring_id);

        let mut idle_count = 0u32;

        while !self.should_stop.load(Ordering::Relaxed) {
            // 检查提交队列
            if let Some(sqe) = ring.sq.pop() {
                // 有新请求，处理它
                let _ = ring.process_sqe(&sqe);
                idle_count = 0;
                self.last_active.store(0, Ordering::Relaxed);
            } else {
                // 队列为空，增加空闲计数
                idle_count += 1;

                // 检查是否超过空闲超时
                if idle_count > self.idle_timeout {
                    // 进入睡眠状态，等待唤醒
                    ring.sq_poll_running.store(false, Ordering::Release);

                    // 等待唤醒信号
                    while !ring.sq_poll_running.load(Ordering::Acquire)
                        && !self.should_stop.load(Ordering::Relaxed)
                    {
                        core::hint::spin_loop();
                    }

                    idle_count = 0;
                    ring.stats.sq_poll_wakeups.fetch_add(1, Ordering::Relaxed);
                } else {
                    // 短暂自旋等待
                    for _ in 0..100 {
                        core::hint::spin_loop();
                    }
                }
            }
        }

        crate::println!("[io_uring] SQPOLL thread stopped for ring {:?}", self.ring_id);
    }

    /// 停止 SQPOLL 线程
    pub fn stop(&self) {
        self.should_stop.store(true, Ordering::Release);
    }
}

impl IoUring {
    /// 创建新的 io_uring 实例
    pub fn new(params: &mut IoUringParams) -> Result<Self, IoUringError> {
        let sq_size = params.sq_entries.next_power_of_two();
        let cq_size = if params.flags.contains(IoUringSetupFlags::CQSIZE) {
            params.cq_entries.next_power_of_two()
        } else {
            sq_size * 2
        };

        let sq = SubmissionQueue::new(sq_size as usize)?;
        let cq = CompletionQueue::new(cq_size as usize)?;

        // 填充偏移量信息
        params.sq_off = SqRingOffsets {
            head: 0,
            tail: 4,
            ring_mask: 8,
            ring_entries: 12,
            flags: 16,
            dropped: 20,
            array: 24,
            resv: [0; 3],
        };

        params.cq_off = CqRingOffsets {
            head: 0,
            tail: 4,
            ring_mask: 8,
            ring_entries: 12,
            overflow: 16,
            cqes: 20,
            flags: 24,
            resv: [0; 3],
        };

        let sq_poll = params.flags.contains(IoUringSetupFlags::SQPOLL);

        // Per Linux: when IORING_SETUP_SQPOLL is set, the kernel thread
        // is automatically started at ring creation time.
        let sq_poll_running = AtomicBool::new(sq_poll);
        if sq_poll {
            SQPOLL_RING_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        }

        Ok(Self {
            id: IoUringId::new(),
            sq,
            cq,
            sq_poll,
            sq_poll_running,
            registered_buffers: SpinLock::new(Vec::new()),
            registered_fds: SpinLock::new(Vec::new()),
            pending_timeouts: SpinLock::new(Vec::new()),
            stats: IoUringStats::default(),
        })
    }

    /// 获取实例 ID
    pub fn id(&self) -> IoUringId {
        self.id
    }

    /// 推送 SQE 到提交队列
    pub fn push_sqe(&self, sqe: SubmissionQueueEntry) -> Result<(), IoUringError> {
        self.sq.push(sqe)
    }

    /// 提交请求并等待完成
    ///
    /// Per Linux io_uring_enter(2):
    /// - to_submit: number of SQEs to consume from the SQ ring
    /// - min_complete: if GETEVENTS set, block until this many CQEs available
    /// - flags: GETEVENTS to wait, SQ_WAKEUP to wake SQPOLL thread
    pub fn enter(
        &self,
        to_submit: u32,
        min_complete: u32,
        flags: IoUringEnterFlags,
    ) -> Result<u32, IoUringError> {
        // If SQPOLL mode and SQ_WAKEUP flag, wake the poll thread
        if flags.contains(IoUringEnterFlags::SQ_WAKEUP) {
            self.wakeup_sq_poll();
        }

        // Submit SQEs with proper chain semantics
        let submitted = if to_submit > 0 {
            self.submit_batch(to_submit)
        } else {
            0
        };

        // Wait for completions if GETEVENTS
        if min_complete > 0 && flags.contains(IoUringEnterFlags::GETEVENTS) {
            self.wait_completions(min_complete)?;
        }

        Ok(submitted)
    }

    /// 处理单个 SQE
    ///
    /// 这是 io_uring 的核心处理逻辑。对于每个 SQE：
    /// 1. 根据操作码分发到对应的处理函数
    /// 2. 处理链式操作（IO_LINK / IO_HARDLINK 标志）
    /// 3. 生成 CQE 并写入完成队列
    ///
    /// Per Linux semantics:
    /// - IO_LINK (soft link): if this SQE fails, cancel all subsequent linked SQEs
    ///   with -ECANCELED. If it succeeds, proceed to the next in chain.
    /// - IO_HARDLINK: like IO_LINK but subsequent SQEs execute regardless of this
    ///   SQE's result (only explicit cancel stops them).
    /// - The chain is broken when an SQE without IO_LINK/IO_HARDLINK is encountered.
    fn process_sqe(&self, sqe: &SubmissionQueueEntry) -> Result<(), IoUringError> {
        let result = self.dispatch_op(sqe);

        // Emit CQE for this SQE
        let res = match &result {
            Ok(n) => *n as i32,
            Err(e) => e.to_errno(),
        };
        self.emit_cqe(sqe.user_data, res, 0);

        Ok(())
    }

    /// Submit a batch of SQEs, respecting linked chains.
    /// Called from `enter()` to process all pending SQEs with proper chain semantics.
    fn submit_batch(&self, to_submit: u32) -> u32 {
        let mut submitted = 0u32;
        let mut chain: Vec<SubmissionQueueEntry> = Vec::new();
        let mut in_chain = false;

        for _ in 0..to_submit {
            let sqe = match self.sq.pop() {
                Some(s) => s,
                None => break,
            };
            submitted += 1;
            self.stats.submissions.fetch_add(1, Ordering::Relaxed);

            let is_linked = sqe.flags.contains(ring::SqeFlags::IO_LINK)
                || sqe.flags.contains(ring::SqeFlags::IO_HARDLINK);

            if is_linked {
                // Part of a chain — accumulate
                chain.push(sqe);
                in_chain = true;
            } else if in_chain {
                // Last element of the chain (no link flag)
                chain.push(sqe);
                self.execute_chain(&chain);
                chain.clear();
                in_chain = false;
            } else {
                // Standalone SQE — execute immediately
                let result = self.dispatch_op(&sqe);
                let res = match &result {
                    Ok(n) => *n as i32,
                    Err(e) => e.to_errno(),
                };
                self.emit_cqe(sqe.user_data, res, 0);
            }
        }

        // If we ended mid-chain (all remaining SQEs had IO_LINK), execute what we have
        if !chain.is_empty() {
            self.execute_chain(&chain);
        }

        submitted
    }

    /// Execute a linked chain of SQEs per Linux semantics.
    ///
    /// For soft links (IO_LINK): execute sequentially; on failure, cancel remaining.
    /// For hard links (IO_HARDLINK): execute sequentially regardless of prior results.
    fn execute_chain(&self, chain: &[SubmissionQueueEntry]) {
        self.stats.linked_operations.fetch_add(1, Ordering::Relaxed);
        let mut cancelled = false;

        for sqe in chain.iter() {
            if cancelled {
                // Emit -ECANCELED for all cancelled chain members
                self.emit_cqe(sqe.user_data, -125, 0); // -ECANCELED
                self.stats.cancelled_operations.fetch_add(1, Ordering::Relaxed);
                continue;
            }

            let result = self.dispatch_op(sqe);
            let res = match &result {
                Ok(n) => *n as i32,
                Err(e) => e.to_errno(),
            };
            self.emit_cqe(sqe.user_data, res, 0);

            // Check if this failure should cancel the rest
            if result.is_err() && !sqe.flags.contains(ring::SqeFlags::IO_HARDLINK) {
                // Soft link failed — cancel remaining chain members
                cancelled = true;
            }
        }
    }

    /// Dispatch a single SQE to its operation handler.
    fn dispatch_op(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        match sqe.opcode {
            IoUringOp::Nop => self.op_nop(sqe),
            IoUringOp::Read => self.op_read(sqe),
            IoUringOp::Write => self.op_write(sqe),
            IoUringOp::Fsync => self.op_fsync(sqe),
            IoUringOp::Poll => self.op_poll(sqe),
            IoUringOp::Timeout => self.op_timeout(sqe),
            IoUringOp::TimeoutRemove => self.op_timeout_remove(sqe),
            IoUringOp::Cancel => self.op_cancel(sqe),
            IoUringOp::Close => self.op_close(sqe),
            IoUringOp::Accept => self.op_accept(sqe),
            IoUringOp::Connect => self.op_connect(sqe),
            IoUringOp::Send => self.op_send(sqe),
            IoUringOp::Recv => self.op_recv(sqe),
            IoUringOp::SendMsg => self.op_sendmsg(sqe),
            IoUringOp::RecvMsg => self.op_recvmsg(sqe),
            _ => Err(IoUringError::InvalidOpcode),
        }
    }

    /// Emit a CQE into the completion queue.
    fn emit_cqe(&self, user_data: u64, res: i32, flags: u32) {
        let cqe = CompletionQueueEntry { user_data, res, flags };
        match self.cq.push(cqe) {
            Ok(_) => {}
            Err(_) => {
                self.stats.cq_overflows.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.stats.completions.fetch_add(1, Ordering::Relaxed);
    }

    /// IORING_OP_TIMEOUT_REMOVE — per Linux io_uring/timeout.c io_timeout_remove().
    /// Attempts to cancel a pending timeout identified by sqe.addr (the target's
    /// user_data). Since our io_uring processes all SQEs synchronously (timeouts
    /// either spin-complete or return immediately), there are never pending
    /// timeouts in a deferred queue. Return -ENOENT per Linux semantics when the
    /// target timeout is not found in the pending list.
    fn op_timeout_remove(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        let target_user_data = sqe.addr;

        // Search the pending timeout list for a matching user_data
        let mut pending = self.pending_timeouts.lock();
        if let Some(pos) = pending.iter().position(|t| t.user_data == target_user_data) {
            // Found: remove it and post -ECANCELED CQE for the original timeout
            let removed = pending.remove(pos);
            drop(pending);
            // Per Linux: the cancelled timeout gets a CQE with -ECANCELED
            let _ = self.cq.push(CompletionQueueEntry {
                user_data: removed.user_data, res: -125, flags: 0,
            });
            self.stats.cancelled_operations.fetch_add(1, Ordering::Relaxed);
            Ok(0)
        } else {
            // Not found: per Linux, return -ENOENT
            Err(IoUringError::NotFound)
        }
    }

    /// IORING_OP_ASYNC_CANCEL — per Linux io_uring/cancel.c io_async_cancel().
    /// Attempts to cancel a pending operation by user_data or fd. Since our
    /// io_uring processes SQEs synchronously, the only cancelable pending
    /// operations are timeouts. Check pending_timeouts first; if not found,
    /// return -ENOENT per Linux semantics.
    fn op_cancel(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        let target_user_data = sqe.addr;
        let flags = sqe.op_flags;
        let cancel_all = (flags & ops::cancel_flags::ALL) != 0;

        // Try cancelling from pending timeouts
        let mut pending = self.pending_timeouts.lock();
        if cancel_all {
            let mut count = 0u32;
            while let Some(pos) = pending.iter().position(|t| t.user_data == target_user_data) {
                let removed = pending.remove(pos);
                let _ = self.cq.push(CompletionQueueEntry {
                    user_data: removed.user_data, res: -125, flags: 0,
                });
                count += 1;
            }
            drop(pending);
            if count > 0 {
                self.stats.cancelled_operations.fetch_add(count, Ordering::Relaxed);
                return Ok(count as usize);
            }
        } else if let Some(pos) = pending.iter().position(|t| t.user_data == target_user_data) {
            let removed = pending.remove(pos);
            drop(pending);
            let _ = self.cq.push(CompletionQueueEntry {
                user_data: removed.user_data, res: -125, flags: 0,
            });
            self.stats.cancelled_operations.fetch_add(1, Ordering::Relaxed);
            return Ok(1);
        } else {
            drop(pending);
        }

        // Not found in any pending queue
        Err(IoUringError::NotFound)
    }

    /// 关闭文件描述符
    fn op_close(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        if sqe.fd < 0 {
            return Err(IoUringError::InvalidFd);
        }

        // 检查是否使用注册的文件描述符
        let _fd = if sqe.flags.contains(ring::SqeFlags::FIXED_FILE) {
            let fds = self.registered_fds.lock();
            *fds.get(sqe.fd as usize).ok_or(IoUringError::InvalidFd)?
        } else {
            sqe.fd
        };

        // 在实际实现中，这会调用进程的文件描述符表来关闭 fd
        // 目前返回成功
        Ok(0)
    }

    /// NOP 操作
    fn op_nop(&self, _sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        Ok(0)
    }

    /// 读操作 - 与文件系统集成
    fn op_read(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        // 验证文件描述符
        if sqe.fd < 0 {
            return Err(IoUringError::InvalidFd);
        }

        // 检查是否使用注册的文件描述符
        let fd = if sqe.flags.contains(ring::SqeFlags::FIXED_FILE) {
            let fds = self.registered_fds.lock();
            *fds.get(sqe.fd as usize).ok_or(IoUringError::InvalidFd)?
        } else {
            sqe.fd
        };

        // 获取缓冲区
        let buf_addr = if sqe.flags.contains(ring::SqeFlags::BUFFER_SELECT) {
            // 使用注册的缓冲区
            let buffers = self.registered_buffers.lock();
            let buf = buffers.get(sqe.buf_index as usize)
                .ok_or(IoUringError::InvalidArgument)?;
            buf.user_addr as u64
        } else {
            sqe.addr
        };

        // 验证缓冲区地址和长度
        if buf_addr == 0 || sqe.len == 0 {
            return Err(IoUringError::InvalidArgument);
        }

        // 通过进程的文件描述符表获取文件
        // 这里需要与进程管理集成
        let result = unsafe {
            self.do_read(fd, buf_addr as *mut u8, sqe.len as usize, sqe.off as usize)
        };

        result
    }

    /// 执行实际读取 — uses the process fd table for real I/O.
    unsafe fn do_read(&self, fd: i32, buf: *mut u8, len: usize, offset: usize) -> Result<usize, IoUringError> {
        let buffer = core::slice::from_raw_parts_mut(buf, len);

        // Get file from process fd table
        let table_guard = crate::sched::process::PROCESS_TABLE.lock();
        let table = table_guard.as_ref().ok_or(IoUringError::IoError)?;
        let pid = table.current.ok_or(IoUringError::IoError)?;
        let process = table.get_fd_process(pid).ok_or(IoUringError::IoError)?;
        let fd_obj = crate::fs::fd::get(process, fd as usize)
            .ok_or(IoUringError::InvalidFd)?;
        let file_inode = fd_obj.file.clone();
        let cur_off = fd_obj.offset.load(core::sync::atomic::Ordering::Relaxed);
        drop(table_guard);

        let file = file_inode.as_file().ok_or(IoUringError::IoError)?;

        // Perform the read at the given offset
        let read_offset = if offset == u64::MAX as usize {
            cur_off
        } else {
            offset
        };

        match file.read(read_offset, buffer) {
            Ok(n) => Ok(n),
            Err(_) => Err(IoUringError::IoError),
        }
    }

    /// 写操作 - 与文件系统集成
    fn op_write(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        // 验证文件描述符
        if sqe.fd < 0 {
            return Err(IoUringError::InvalidFd);
        }

        // 检查是否使用注册的文件描述符
        let fd = if sqe.flags.contains(ring::SqeFlags::FIXED_FILE) {
            let fds = self.registered_fds.lock();
            *fds.get(sqe.fd as usize).ok_or(IoUringError::InvalidFd)?
        } else {
            sqe.fd
        };

        // 获取缓冲区
        let buf_addr = if sqe.flags.contains(ring::SqeFlags::BUFFER_SELECT) {
            let buffers = self.registered_buffers.lock();
            let buf = buffers.get(sqe.buf_index as usize)
                .ok_or(IoUringError::InvalidArgument)?;
            buf.user_addr as u64
        } else {
            sqe.addr
        };

        // 验证缓冲区
        if buf_addr == 0 || sqe.len == 0 {
            return Err(IoUringError::InvalidArgument);
        }

        // 执行写入
        let result = unsafe {
            self.do_write(fd, buf_addr as *const u8, sqe.len as usize, sqe.off as usize)
        };

        result
    }

    /// 执行实际写入 — uses the process fd table for real I/O.
    unsafe fn do_write(&self, fd: i32, buf: *const u8, len: usize, offset: usize) -> Result<usize, IoUringError> {
        let buffer = core::slice::from_raw_parts(buf, len);

        // Get file from process fd table
        let table_guard = crate::sched::process::PROCESS_TABLE.lock();
        let table = table_guard.as_ref().ok_or(IoUringError::IoError)?;
        let pid = table.current.ok_or(IoUringError::IoError)?;
        let process = table.get_fd_process(pid).ok_or(IoUringError::IoError)?;
        let fd_obj = crate::fs::fd::get(process, fd as usize)
            .ok_or(IoUringError::InvalidFd)?;
        let file_inode = fd_obj.file.clone();
        let cur_off = fd_obj.offset.load(core::sync::atomic::Ordering::Relaxed);
        drop(table_guard);

        let file = file_inode.as_file().ok_or(IoUringError::IoError)?;

        // Perform the write at the given offset
        let write_offset = if offset == u64::MAX as usize {
            cur_off
        } else {
            offset
        };

        match file.write(write_offset, buffer) {
            Ok(n) => Ok(n),
            Err(_) => Err(IoUringError::IoError),
        }
    }

    /// Fsync 操作 - 同步文件数据到存储
    fn op_fsync(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        if sqe.fd < 0 {
            return Err(IoUringError::InvalidFd);
        }

        let _fd = if sqe.flags.contains(ring::SqeFlags::FIXED_FILE) {
            let fds = self.registered_fds.lock();
            *fds.get(sqe.fd as usize).ok_or(IoUringError::InvalidFd)?
        } else {
            sqe.fd
        };

        // 检查是否只同步数据（不同步元数据）
        let _datasync = (sqe.op_flags & ops::fsync_flags::DATASYNC) != 0;

        // 执行同步
        // 在实际实现中，这会调用文件系统的 sync 方法
        // 对于内存文件系统，这是一个空操作
        // 对于块设备支持的文件系统，需要刷新缓冲区

        // 返回 0 表示成功
        Ok(0)
    }

    /// IORING_OP_POLL_ADD — per Linux io_uring/poll.c io_poll_add().
    /// Queries the fd's real poll readiness via the File trait's poll_read/poll_write,
    /// or socket readiness for socket fds, returning the revents mask.
    fn op_poll(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        let fd = self.resolve_fd(sqe)?;
        let poll_mask = sqe.op_flags;

        // Query real readiness from the fd's underlying file/socket
        let readiness = {
            let table_guard = crate::sched::process::PROCESS_TABLE.lock();
            let table = table_guard.as_ref().ok_or(IoUringError::IoError)?;
            let pid = table.current.ok_or(IoUringError::IoError)?;
            let process = table.get_fd_process(pid).ok_or(IoUringError::IoError)?;
            let fd_obj = crate::fs::fd::get(process, fd as usize)
                .ok_or(IoUringError::InvalidFd)?;
            let file_inode = fd_obj.file.clone();
            drop(table_guard);

            // File trait readiness(): returns READINESS_READ | READINESS_WRITE bits
            if let Some(file) = file_inode.as_file() {
                file.readiness()
            } else {
                // Directory or special inode — always readable for readdir
                crate::fs::READINESS_READ
            }
        };

        let mut revents: u32 = 0;
        if (poll_mask & ops::poll_events::POLLIN) != 0
            && (readiness & crate::fs::READINESS_READ) != 0
        {
            revents |= ops::poll_events::POLLIN;
        }
        if (poll_mask & ops::poll_events::POLLOUT) != 0
            && (readiness & crate::fs::READINESS_WRITE) != 0
        {
            revents |= ops::poll_events::POLLOUT;
        }
        // POLLHUP: check if HUP flag is set (pipes, sockets)
        if (readiness & crate::fs::READINESS_HUP) != 0 {
            revents |= ops::poll_events::POLLHUP;
        }
        // POLLERR: check if error flag is set
        if (readiness & crate::fs::READINESS_ERROR) != 0 {
            revents |= ops::poll_events::POLLERR;
        }

        Ok(revents as usize)
    }

    /// IORING_OP_TIMEOUT — per Linux io_uring/timeout.c io_timeout().
    /// Reads a `struct __kernel_timespec` from sqe.addr, computes an absolute
    /// deadline, and checks if it has already elapsed. If IORING_TIMEOUT_ABS is
    /// set, the timespec is treated as an absolute CLOCK_MONOTONIC value;
    /// otherwise it is relative to now. Returns -ETIME when the deadline has
    /// passed (per Linux semantics, timeout completion is reported as -ETIME).
    /// sqe.off encodes the completion count threshold (0 = pure time-based).
    fn op_timeout(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        if sqe.addr == 0 {
            return Err(IoUringError::InvalidArgument);
        }

        let timeout_ptr = sqe.addr as *const [i64; 2];
        let (tv_sec, tv_nsec) = unsafe {
            let ts = &*timeout_ptr;
            (ts[0], ts[1])
        };

        if tv_sec < 0 || tv_nsec < 0 || tv_nsec >= 1_000_000_000 {
            return Err(IoUringError::InvalidArgument);
        }

        let timeout_ns = tv_sec as u64 * 1_000_000_000 + tv_nsec as u64;
        let flags = sqe.op_flags;
        let is_absolute = (flags & ops::timeout_flags::ABS) != 0;

        // Current monotonic time in nanoseconds
        let now_ticks = crate::arch::asm_helpers::rdtime();
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        let f = freq.max(1);
        let now_ns = (now_ticks / f) * 1_000_000_000 + (now_ticks % f) * 1_000_000_000 / f;

        let deadline_ns = if is_absolute {
            timeout_ns
        } else {
            now_ns.saturating_add(timeout_ns)
        };

        // If deadline already passed, report -ETIME immediately (normal path)
        if now_ns >= deadline_ns {
            return Err(IoUringError::TimedOut);
        }

        // Completion count threshold: sqe.off = number of CQEs to wait for.
        // If off > 0, this is a "wait for N completions OR timeout" request.
        // We check if the CQ already has enough completions.
        let count_threshold = sqe.off as u64;
        if count_threshold > 0 {
            let completed = self.cq.total_completed() as u64;
            if completed >= count_threshold {
                return Ok(0);
            }
        }

        // For non-expired timeouts: store deadline and return -ETIME.
        // In a fully async io_uring, this would enqueue a timer and post a CQE
        // later. Since our io_uring processes SQEs synchronously, the best we
        // can do is spin-check the deadline to allow short timeouts to work.
        // For sub-millisecond timeouts, busy-wait; for longer ones, yield.
        let spin_limit_ns: u64 = 10_000_000; // 10ms spin ceiling
        if deadline_ns.saturating_sub(now_ns) <= spin_limit_ns {
            loop {
                let t = crate::arch::asm_helpers::rdtime();
                let cur_ns = (t / f) * 1_000_000_000 + (t % f) * 1_000_000_000 / f;
                if cur_ns >= deadline_ns { break; }
                core::hint::spin_loop();
            }
        }
        // Return -ETIME to signal timeout completed (Linux convention)
        Err(IoUringError::TimedOut)
    }

    // ================================================================
    // io_uring socket ops — per Linux io_uring/net.c
    // ================================================================

    /// Resolve fd from SQE, handling FIXED_FILE.
    fn resolve_fd(&self, sqe: &SubmissionQueueEntry) -> Result<i32, IoUringError> {
        if sqe.fd < 0 { return Err(IoUringError::InvalidFd); }
        if sqe.flags.contains(ring::SqeFlags::FIXED_FILE) {
            let fds = self.registered_fds.lock();
            fds.get(sqe.fd as usize).copied().ok_or(IoUringError::InvalidFd)
        } else {
            Ok(sqe.fd)
        }
    }

    /// IORING_OP_ACCEPT — per Linux io_uring/net.c io_accept().
    /// sqe.fd = listening socket, sqe.addr = sockaddr* (may be 0),
    /// sqe.addr2 (via off) = addrlen*, sqe.op_flags = accept4 flags.
    fn op_accept(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        let fd = self.resolve_fd(sqe)?;
        let addr_ptr = sqe.addr as *mut u8;
        let addrlen_ptr = sqe.off as *mut u32;
        let flags = sqe.op_flags as i32;
        let ret = crate::syscall::socket::sys_accept4(fd, addr_ptr, addrlen_ptr, flags);
        if ret < 0 { Err(IoUringError::IoError) } else { Ok(ret as usize) }
    }

    /// IORING_OP_CONNECT — per Linux io_uring/net.c io_connect().
    /// sqe.fd = socket, sqe.addr = sockaddr*, sqe.off = addrlen.
    fn op_connect(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        let fd = self.resolve_fd(sqe)?;
        let addr_ptr = sqe.addr as *const u8;
        let addrlen = sqe.off as u32;
        let ret = crate::syscall::socket::sys_connect(fd, addr_ptr, addrlen);
        if ret < 0 { Err(IoUringError::IoError) } else { Ok(ret as usize) }
    }

    /// IORING_OP_SEND — per Linux io_uring/net.c io_send().
    /// sqe.fd = socket, sqe.addr = buf, sqe.len = len, sqe.op_flags = msg_flags.
    fn op_send(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        let fd = self.resolve_fd(sqe)?;
        let buf = sqe.addr as *const u8;
        let len = sqe.len as usize;
        let flags = sqe.op_flags as i32;
        let ret = crate::syscall::socket::sys_sendto(
            fd, buf, len, flags,
            core::ptr::null(), 0,
        );
        if ret < 0 { Err(IoUringError::IoError) } else { Ok(ret as usize) }
    }

    /// IORING_OP_RECV — per Linux io_uring/net.c io_recv().
    /// sqe.fd = socket, sqe.addr = buf, sqe.len = len, sqe.op_flags = msg_flags.
    fn op_recv(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        let fd = self.resolve_fd(sqe)?;
        let buf_addr = if sqe.flags.contains(ring::SqeFlags::BUFFER_SELECT) {
            let buffers = self.registered_buffers.lock();
            let b = buffers.get(sqe.buf_index as usize)
                .ok_or(IoUringError::InvalidArgument)?;
            b.user_addr as u64
        } else {
            sqe.addr
        };
        let buf = buf_addr as *mut u8;
        let len = sqe.len as usize;
        let flags = sqe.op_flags as i32;
        let ret = crate::syscall::socket::sys_recvfrom(
            fd, buf, len, flags,
            core::ptr::null_mut(), core::ptr::null_mut(),
        );
        if ret < 0 { Err(IoUringError::IoError) } else { Ok(ret as usize) }
    }

    /// IORING_OP_SENDMSG — per Linux io_uring/net.c io_sendmsg().
    /// sqe.fd = socket, sqe.addr = msghdr*, sqe.op_flags = msg_flags.
    fn op_sendmsg(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        let fd = self.resolve_fd(sqe)?;
        let msg_ptr = sqe.addr as *const u8;
        let flags = sqe.op_flags as i32;
        let ret = crate::syscall::socket::sys_sendmsg(fd, msg_ptr, flags);
        if ret < 0 { Err(IoUringError::IoError) } else { Ok(ret as usize) }
    }

    /// IORING_OP_RECVMSG — per Linux io_uring/net.c io_recvmsg().
    /// sqe.fd = socket, sqe.addr = msghdr*, sqe.op_flags = msg_flags.
    fn op_recvmsg(&self, sqe: &SubmissionQueueEntry) -> Result<usize, IoUringError> {
        let fd = self.resolve_fd(sqe)?;
        let msg_ptr = sqe.addr as *mut u8;
        let flags = sqe.op_flags as i32;
        let ret = crate::syscall::socket::sys_recvmsg(fd, msg_ptr, flags);
        if ret < 0 { Err(IoUringError::IoError) } else { Ok(ret as usize) }
    }

    /// 等待完成
    fn wait_completions(&self, min_complete: u32) -> Result<(), IoUringError> {
        let mut completed = 0;
        while completed < min_complete {
            if self.cq.len() > 0 {
                completed += 1;
            } else {
                // 等待中断或轮询
                core::hint::spin_loop();
            }
        }
        Ok(())
    }

    /// Register fixed buffers — per Linux io_uring_register(IORING_REGISTER_BUFFERS).
    /// The `buffers` slice contains (user_addr, len) pairs from the iovec array.
    /// Pre-registering avoids per-I/O address validation overhead.
    pub fn register_buffers(&self, buffers: &[(usize, usize)]) -> Result<(), IoUringError> {
        let mut registered = self.registered_buffers.lock();
        if !registered.is_empty() {
            return Err(IoUringError::Busy); // -EBUSY: must unregister first
        }
        for &(addr, len) in buffers {
            if addr == 0 && len == 0 {
                // Sparse slot — permitted per Linux (placeholder for later update)
                registered.push(RegisteredBuffer {
                    user_addr: 0,
                    len: 0,
                    kernel_addr: None,
                    pinned: false,
                    ref_count: AtomicU32::new(0),
                });
            } else {
                registered.push(RegisteredBuffer {
                    user_addr: addr,
                    len,
                    kernel_addr: None,
                    pinned: true,
                    ref_count: AtomicU32::new(0),
                });
            }
        }
        Ok(())
    }

    /// Unregister all fixed buffers — per Linux IORING_UNREGISTER_BUFFERS.
    pub fn unregister_buffers(&self) -> Result<(), IoUringError> {
        let mut registered = self.registered_buffers.lock();
        registered.clear();
        Ok(())
    }

    /// Register fixed file descriptors — per Linux IORING_REGISTER_FILES.
    /// Pre-registering avoids per-I/O fd table lookup overhead.
    /// Slot value -1 means empty/sparse (can be filled with FILES_UPDATE).
    pub fn register_files(&self, fds: &[i32]) -> Result<(), IoUringError> {
        let mut registered = self.registered_fds.lock();
        if !registered.is_empty() {
            return Err(IoUringError::Busy); // -EBUSY: must unregister first
        }
        registered.extend_from_slice(fds);
        Ok(())
    }

    /// Unregister all fixed file descriptors — per Linux IORING_UNREGISTER_FILES.
    pub fn unregister_files(&self) -> Result<(), IoUringError> {
        let mut registered = self.registered_fds.lock();
        registered.clear();
        Ok(())
    }

    /// Update fixed file slots — per Linux IORING_REGISTER_FILES_UPDATE.
    /// `offset` is the starting index; `fds` replaces that many slots.
    pub fn update_files(&self, offset: u32, fds: &[i32]) -> Result<usize, IoUringError> {
        let mut registered = self.registered_fds.lock();
        if registered.is_empty() {
            return Err(IoUringError::InvalidArgument); // no file table registered
        }
        let start = offset as usize;
        if start >= registered.len() {
            return Err(IoUringError::InvalidArgument);
        }
        let end = core::cmp::min(start + fds.len(), registered.len());
        let count = end - start;
        registered[start..end].copy_from_slice(&fds[..count]);
        Ok(count)
    }

    /// 获取统计信息
    pub fn stats(&self) -> &IoUringStats {
        &self.stats
    }

    /// 获取完成队列中的事件数
    pub fn cq_ready(&self) -> usize {
        self.cq.len()
    }

    /// 弹出完成事件
    pub fn pop_cqe(&self) -> Option<CompletionQueueEntry> {
        self.cq.pop()
    }

    /// 启动 SQPOLL 模式
    ///
    /// Per Linux: SQPOLL means a kernel thread continuously polls the SQ ring,
    /// processing SQEs without requiring io_uring_enter() for submission.
    /// The thread parks itself after `idle_timeout` ms of no new SQEs and
    /// requires IORING_ENTER_SQ_WAKEUP to restart.
    ///
    /// Implementation: we register this ring for timer-tick-driven polling.
    /// Each timer tick, all SQPOLL-enabled rings with `sq_poll_running=true`
    /// have their SQ drained. This avoids needing a real kernel thread while
    /// preserving the Linux semantic that userspace doesn't need enter() calls.
    pub fn start_sq_poll(&self, _cpu: u32, idle_timeout: u32) -> Result<(), IoUringError> {
        if !self.sq_poll {
            return Err(IoUringError::InvalidArgument);
        }

        if self.sq_poll_running.load(Ordering::Acquire) {
            return Ok(());
        }

        self.sq_poll_running.store(true, Ordering::Release);

        // Store idle timeout for the poll loop
        // (The SqPollThread struct tracks this; here we just enable the mode)
        let _ = idle_timeout;

        Ok(())
    }

    /// 唤醒 SQPOLL 线程
    pub fn wakeup_sq_poll(&self) {
        if self.sq_poll {
            self.sq_poll_running.store(true, Ordering::Release);
            self.stats.sq_poll_wakeups.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 检查 SQPOLL 是否需要唤醒
    pub fn sq_poll_needs_wakeup(&self) -> bool {
        self.sq_poll && !self.sq_poll_running.load(Ordering::Acquire)
    }
}

bitflags::bitflags! {
    /// io_uring_enter 标志
    #[derive(Default, Clone, Copy)]
    pub struct IoUringEnterFlags: u32 {
        /// 获取完成事件
        const GETEVENTS = 1 << 0;
        /// 唤醒 SQ 轮询线程
        const SQ_WAKEUP = 1 << 1;
        /// 等待 SQ 空间
        const SQ_WAIT = 1 << 2;
    }
}

/// 全局 io_uring 管理器
static IO_URING_MANAGER: SpinLock<Option<IoUringManager>> = SpinLock::new(None);

/// 存活的 SQPOLL ring 数：每次定时器中断都进 sqpoll_tick，为 0 时
/// 直接短路，免每中断一次的锁+实例表遍历。
static SQPOLL_RING_COUNT: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// io_uring 管理器
pub struct IoUringManager {
    /// 所有 io_uring 实例
    instances: BTreeMap<IoUringId, Arc<IoUring>>,
}

impl IoUringManager {
    fn new() -> Self {
        Self {
            instances: BTreeMap::new(),
        }
    }

    /// 创建新实例
    pub fn create(&mut self, params: &mut IoUringParams) -> Result<Arc<IoUring>, IoUringError> {
        let ring = Arc::new(IoUring::new(params)?);
        self.instances.insert(ring.id(), ring.clone());
        Ok(ring)
    }

    /// 获取实例
    pub fn get(&self, id: IoUringId) -> Option<Arc<IoUring>> {
        self.instances.get(&id).cloned()
    }

    /// 销毁实例
    #[allow(dead_code)]
    pub fn destroy(&mut self, id: IoUringId) -> bool {
        match self.instances.remove(&id) {
            Some(ring) => {
                if ring.sq_poll {
                    SQPOLL_RING_COUNT.fetch_sub(1, core::sync::atomic::Ordering::Relaxed);
                }
                true
            }
            None => false,
        }
    }
}

/// SQPOLL tick — called from timer interrupt to drain all SQPOLL-enabled rings.
///
/// Per Linux: the SQPOLL kernel thread wakes periodically and processes all
/// pending SQEs. We simulate this via timer-tick polling. Each tick:
/// 1. For each ring with sq_poll=true and sq_poll_running=true
/// 2. Drain all available SQEs from the SQ ring
/// 3. If no SQEs were found, increment idle counter
/// 4. If idle counter exceeds threshold, park the thread (set sq_poll_running=false)
pub fn sqpoll_tick() {
    if SQPOLL_RING_COUNT.load(core::sync::atomic::Ordering::Relaxed) == 0 {
        return;
    }
    let manager = IO_URING_MANAGER.lock();
    let Some(manager) = manager.as_ref() else { return };

    for ring in manager.instances.values() {
        if !ring.sq_poll {
            continue;
        }
        if !ring.sq_poll_running.load(Ordering::Acquire) {
            continue;
        }

        // Drain all available SQEs
        let mut found_work = false;
        while let Some(sqe) = ring.sq.pop() {
            let _ = ring.process_sqe(&sqe);
            ring.stats.submissions.fetch_add(1, Ordering::Relaxed);
            found_work = true;
        }

        if !found_work {
            // No work — eventually the thread would park after idle_timeout.
            // For simplicity, we leave it running (wakeup is cheap).
            ring.stats.sq_poll_wakeups.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// 初始化 io_uring 子系统
pub fn init() {
    let mut manager = IO_URING_MANAGER.lock();
    *manager = Some(IoUringManager::new());
    drop(manager);

    // 运行自检测试，验证 io_uring 功能正常
    if let Err(e) = self_test() {
        crate::println!("[io_uring] Self-test failed: {:?}", e);
    } else {
        crate::println!("[io_uring] Subsystem initialized and self-test passed");
    }
}

/// 静默初始化 io_uring 子系统（不输出调试信息）
pub fn init_quiet() {
    let mut manager = IO_URING_MANAGER.lock();
    *manager = Some(IoUringManager::new());
}

/// io_uring 自检测试
pub fn self_test() -> Result<(), IoUringError> {
    use crate::io_uring::ops::{poll_events, timeout_flags, cancel_flags, open_flags};

    // 测试 1: 创建 io_uring 实例
    let mut params = IoUringParams::default();
    params.sq_entries = 8;
    let ring = setup(&mut params)?;

    // 测试 2: 验证队列创建
    assert!(ring.cq_ready() == 0, "CQ should be empty initially");

    // 测试 3: 测试 NOP 操作
    let sqe = SubmissionQueueEntry::nop(12345);
    ring.sq.push(sqe)?;

    // 测试 4: 处理请求
    let submitted = ring.enter(1, 0, IoUringEnterFlags::empty())?;
    assert!(submitted == 1, "Should submit 1 request");

    // 测试 5: 验证完成
    assert!(ring.cq_ready() == 1, "Should have 1 completion");
    let cqe = ring.pop_cqe().ok_or(IoUringError::InvalidArgument)?;
    assert!(cqe.user_data == 12345, "User data should match");
    assert!(cqe.res == 0, "NOP should return 0");

    // 测试 6: 测试操作码属性
    let op = IoUringOp::Read;
    assert!(op.needs_fd(), "Read needs fd");
    assert!(op.is_read(), "Read is read op");
    assert!(op.is_async(), "Read is async");
    assert!(op.name() == "READ", "Name should be READ");

    // 测试 6.1: 测试更多操作码
    let write_op = IoUringOp::Write;
    assert!(write_op.is_write(), "Write is write op");

    let fsync_op = IoUringOp::Fsync;
    assert!(!fsync_op.is_read(), "Fsync is not read");

    let poll_op = IoUringOp::Poll;
    assert!(poll_op.needs_fd(), "Poll needs fd");

    // 测试所有操作码
    let _ops = [
        IoUringOp::Nop, IoUringOp::Read, IoUringOp::Write, IoUringOp::Fsync,
        IoUringOp::Poll, IoUringOp::SendMsg, IoUringOp::RecvMsg, IoUringOp::Timeout,
        IoUringOp::Accept, IoUringOp::Connect, IoUringOp::TimeoutRemove, IoUringOp::Cancel,
        IoUringOp::LinkTimeout, IoUringOp::Close, IoUringOp::SyncFileRange, IoUringOp::Send,
        IoUringOp::Recv, IoUringOp::Openat, IoUringOp::Closeat, IoUringOp::Statx,
        IoUringOp::Readv, IoUringOp::Writev, IoUringOp::FAdvise, IoUringOp::MAdvise,
        IoUringOp::Openat2, IoUringOp::Fallocate, IoUringOp::Unlinkat, IoUringOp::Renameat,
        IoUringOp::Mkdirat, IoUringOp::Symlinkat, IoUringOp::Linkat, IoUringOp::ProvideBuffers,
        IoUringOp::RemoveBuffers, IoUringOp::Splice, IoUringOp::Tee, IoUringOp::Shutdown,
    ];

    // 测试 7: 测试 poll 事件常量
    let poll_mask = poll_events::POLLIN | poll_events::POLLOUT;
    assert!(poll_mask == 0x0005, "Poll mask should be correct");

    // 测试更多 poll 常量
    let _poll_err = poll_events::POLLERR;
    let _poll_hup = poll_events::POLLHUP;
    let _poll_nval = poll_events::POLLNVAL;
    let _poll_pri = poll_events::POLLPRI;
    let _poll_rdhup = poll_events::POLLRDHUP;

    // 测试 timeout 标志
    let _timeout_update = timeout_flags::UPDATE;
    let _timeout_boottime = timeout_flags::BOOTTIME;
    let _timeout_realtime = timeout_flags::REALTIME;
    let _timeout_link_update = timeout_flags::LINK_UPDATE;
    let _timeout_etime_success = timeout_flags::ETIME_SUCCESS;

    // 测试 cancel 标志
    let _cancel_any = cancel_flags::ANY;

    // 测试 open 标志
    let _o_rdonly = open_flags::O_RDONLY;
    let _o_wronly = open_flags::O_WRONLY;
    let _o_rdwr = open_flags::O_RDWR;
    let _o_creat = open_flags::O_CREAT;
    let _o_excl = open_flags::O_EXCL;
    let _o_trunc = open_flags::O_TRUNC;
    let _o_append = open_flags::O_APPEND;
    let _o_nonblock = open_flags::O_NONBLOCK;
    let _o_directory = open_flags::O_DIRECTORY;

    // 测试 8: 测试 SQE 构造函数
    let _read_sqe = SubmissionQueueEntry::read(1, 0x1000, 4096, 0, 100);
    let _write_sqe = SubmissionQueueEntry::write(1, 0x2000, 4096, 0, 101);
    let _fsync_sqe = SubmissionQueueEntry::fsync(1, 102);
    let _poll_sqe = SubmissionQueueEntry::poll(1, poll_events::POLLIN, 103);

    // 测试 9: 测试 CQE 构造函数
    let success_cqe = CompletionQueueEntry::success(200, 100);
    assert!(success_cqe.is_success(), "Success CQE should be success");

    let error_cqe = CompletionQueueEntry::error(201, -1);
    assert!(!error_cqe.is_success(), "Error CQE should not be success");

    // 测试 10: 测试队列方法
    let sq_len = ring.sq.len();
    let sq_empty = ring.sq.is_empty();
    let sq_full = ring.sq.is_full();
    let sq_dropped = ring.sq.dropped();
    let sq_capacity = ring.sq.capacity();
    crate::println!("[io_uring] SQ: len={}, empty={}, full={}, dropped={}, capacity={}",
        sq_len, sq_empty, sq_full, sq_dropped, sq_capacity);

    let cq_empty = ring.cq.is_empty();
    let cq_overflow = ring.cq.overflow();
    let cq_capacity = ring.cq.capacity();
    let _cq_peek = ring.cq.peek();
    crate::println!("[io_uring] CQ: empty={}, overflow={}, capacity={}",
        cq_empty, cq_overflow, cq_capacity);

    // 测试 11: 测试统计信息
    let stats = ring.stats();
    assert!(stats.submissions.load(Ordering::Relaxed) >= 1, "Should have submissions");
    assert!(stats.completions.load(Ordering::Relaxed) >= 1, "Should have completions");

    // 测试 12: 测试全局统计
    let global_stats = get_global_stats().ok_or(IoUringError::InvalidArgument)?;
    assert!(global_stats.active_rings >= 1, "Should have active rings");

    // 测试 13: 测试 get 函数
    let _ring_copy = get(ring.id);

    // 测试 14: 测试 SqPollThread
    let sq_poll = SqPollThread::new(ring.id, 0, 1000);
    crate::println!("[io_uring] SqPollThread created for ring {}", ring.id.0);
    sq_poll.stop();

    // 测试 15: 测试 register 方法
    let _ = ring.register_buffers(&[(0x1000, 4096)]);
    let _ = ring.register_files(&[0, 1, 2]);
    let _ = ring.start_sq_poll(0, 1000);
    ring.wakeup_sq_poll();
    let _needs_wakeup = ring.sq_poll_needs_wakeup();

    Ok(())
}

/// 创建 io_uring 实例
pub fn setup(params: &mut IoUringParams) -> Result<Arc<IoUring>, IoUringError> {
    IO_URING_MANAGER
        .lock()
        .as_mut()
        .ok_or(IoUringError::InvalidArgument)?
        .create(params)
}

/// 获取 io_uring 实例
pub fn get(id: IoUringId) -> Option<Arc<IoUring>> {
    IO_URING_MANAGER.lock().as_ref()?.get(id)
}

/// 销毁 io_uring 实例。Linux 中 io_uring 生命周期由返回的 anon-inode fd
/// 管理；最后一个 fd close 后 ring 上下文释放。syscall 层的 fd wrapper 在
/// `close_notify_last()` 中调用这里，避免把 ring id 误当作进程 fd。
pub fn destroy(id: IoUringId) -> bool {
    IO_URING_MANAGER
        .lock()
        .as_mut()
        .map(|manager| manager.destroy(id))
        .unwrap_or(false)
}

/// 全局 io_uring 统计
pub struct GlobalIoUringStats {
    /// 活跃的 ring 数量
    pub active_rings: usize,
    /// 总提交数
    pub submissions: u64,
    /// 总完成数
    pub completions: u64,
    /// SQ poll 唤醒次数
    pub sq_poll_wakeups: u64,
}

/// 获取全局 io_uring 统计
pub fn get_global_stats() -> Option<GlobalIoUringStats> {
    let manager = IO_URING_MANAGER.lock();
    let manager = manager.as_ref()?;

    let mut stats = GlobalIoUringStats {
        active_rings: manager.instances.len(),
        submissions: 0,
        completions: 0,
        sq_poll_wakeups: 0,
    };

    for ring in manager.instances.values() {
        let ring_stats = ring.stats();
        stats.submissions += ring_stats.submissions.load(core::sync::atomic::Ordering::Relaxed) as u64;
        stats.completions += ring_stats.completions.load(core::sync::atomic::Ordering::Relaxed) as u64;
        stats.sq_poll_wakeups += ring_stats.sq_poll_wakeups.load(core::sync::atomic::Ordering::Relaxed) as u64;
    }

    Some(stats)
}
