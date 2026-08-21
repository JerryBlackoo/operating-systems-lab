//! io_uring 用户态接口
//!
//! 提供异步 I/O 操作的用户态封装

use crate::syscall::*;

/// io_uring 参数
#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct IoUringParams {
    /// SQ 大小
    pub sq_entries: u32,
    /// CQ 大小
    pub cq_entries: u32,
    /// 标志
    pub flags: u32,
    /// SQ 线程 CPU
    pub sq_thread_cpu: u32,
    /// SQ 线程空闲超时（毫秒）
    pub sq_thread_idle: u32,
    /// 保留字段
    pub _reserved: [u32; 3],
}

impl IoUringParams {
    pub fn new(entries: u32) -> Self {
        Self {
            sq_entries: entries,
            cq_entries: entries * 2,
            ..Default::default()
        }
    }
    
    pub fn with_sqpoll(mut self, idle_ms: u32) -> Self {
        self.flags |= IORING_SETUP_SQPOLL;
        self.sq_thread_idle = idle_ms;
        self
    }
}

/// io_uring 设置标志
pub const IORING_SETUP_SQPOLL: u32 = 1 << 1;
pub const IORING_SETUP_IOPOLL: u32 = 1 << 0;

/// io_uring 操作码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IoUringOp {
    Nop = 0,
    Readv = 1,
    Writev = 2,
    Fsync = 3,
    ReadFixed = 4,
    WriteFixed = 5,
    PollAdd = 6,
    PollRemove = 7,
    Timeout = 11,
    TimeoutRemove = 12,
    Accept = 13,
    Connect = 14,
    Close = 19,
    Read = 22,
    Write = 23,
}

/// 提交队列条目 (SQE)
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct IoUringSqe {
    pub opcode: u8,
    pub flags: u8,
    pub ioprio: u16,
    pub fd: i32,
    pub off: u64,
    pub addr: u64,
    pub len: u32,
    pub op_flags: u32,
    pub user_data: u64,
    pub buf_index: u16,
    pub personality: u16,
    pub splice_fd_in: i32,
    pub _pad: [u64; 2],
}

impl IoUringSqe {
    pub fn new() -> Self {
        Self::default()
    }
    
    pub fn prep_nop(&mut self, user_data: u64) {
        self.opcode = IoUringOp::Nop as u8;
        self.user_data = user_data;
    }
    
    pub fn prep_read(&mut self, fd: i32, buf: *mut u8, len: u32, offset: u64, user_data: u64) {
        self.opcode = IoUringOp::Read as u8;
        self.fd = fd;
        self.addr = buf as u64;
        self.len = len;
        self.off = offset;
        self.user_data = user_data;
    }
    
    pub fn prep_write(&mut self, fd: i32, buf: *const u8, len: u32, offset: u64, user_data: u64) {
        self.opcode = IoUringOp::Write as u8;
        self.fd = fd;
        self.addr = buf as u64;
        self.len = len;
        self.off = offset;
        self.user_data = user_data;
    }
    
    pub fn prep_close(&mut self, fd: i32, user_data: u64) {
        self.opcode = IoUringOp::Close as u8;
        self.fd = fd;
        self.user_data = user_data;
    }
    
    pub fn prep_fsync(&mut self, fd: i32, user_data: u64) {
        self.opcode = IoUringOp::Fsync as u8;
        self.fd = fd;
        self.user_data = user_data;
    }
}

/// 完成队列条目 (CQE)
#[derive(Debug, Clone, Copy, Default)]
#[repr(C)]
pub struct IoUringCqe {
    pub user_data: u64,
    pub res: i32,
    pub flags: u32,
}

/// io_uring 实例
pub struct IoUring {
    fd: i32,
    sq_entries: u32,
    cq_entries: u32,
}

/// io_uring 错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoUringError {
    InvalidParams,
    OutOfMemory,
    TooManyRings,
    SubmissionFailed,
    CompletionFailed,
    Unknown(i32),
}

impl From<isize> for IoUringError {
    fn from(code: isize) -> Self {
        match code {
            -22 => IoUringError::InvalidParams,
            -12 => IoUringError::OutOfMemory,
            -24 => IoUringError::TooManyRings,
            code => IoUringError::Unknown(code as i32),
        }
    }
}

impl IoUring {
    /// 创建 io_uring 实例
    pub fn new(params: &mut IoUringParams) -> Result<Self, IoUringError> {
        let ret = sys_io_uring_setup(params.sq_entries, params);
        if ret < 0 {
            Err(IoUringError::from(ret))
        } else {
            Ok(Self {
                fd: ret as i32,
                sq_entries: params.sq_entries,
                cq_entries: params.cq_entries,
            })
        }
    }
    
    /// 获取 io_uring 文件描述符
    pub fn fd(&self) -> i32 {
        self.fd
    }
    
    /// 获取 SQ 大小
    pub fn sq_entries(&self) -> u32 {
        self.sq_entries
    }
    
    /// 获取 CQ 大小
    pub fn cq_entries(&self) -> u32 {
        self.cq_entries
    }
    
    /// 提交请求并等待完成
    pub fn submit_and_wait(&self, sqes: &[IoUringSqe], cqes: &mut [IoUringCqe], min_complete: u32) -> Result<usize, IoUringError> {
        let ret = sys_io_uring_enter(
            self.fd,
            sqes.as_ptr(),
            sqes.len() as u32,
            cqes.as_mut_ptr(),
            cqes.len() as u32,
            min_complete,
        );
        if ret < 0 {
            Err(IoUringError::from(ret))
        } else {
            Ok(ret as usize)
        }
    }
    
    /// 仅提交请求（不等待）
    pub fn submit(&self, sqes: &[IoUringSqe]) -> Result<usize, IoUringError> {
        let mut cqes = [];
        self.submit_and_wait(sqes, &mut cqes, 0)
    }
    
    /// 等待完成事件
    pub fn wait(&self, cqes: &mut [IoUringCqe], min_complete: u32) -> Result<usize, IoUringError> {
        let sqes = [];
        self.submit_and_wait(&sqes, cqes, min_complete)
    }
}

impl Drop for IoUring {
    fn drop(&mut self) {
        let _ = crate::close(self.fd as usize);
    }
}
