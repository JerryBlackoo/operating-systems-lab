//! 统一错误处理
//!
//! 本模块定义了内核统一的错误类型 `KernelError`，用于提供一致的错误处理机制。
//! 所有内核子系统的错误都可以转换为 `KernelError`，便于错误传播和处理。

use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};
use alloc::vec::Vec;
use alloc::string::String;
use alloc::boxed::Box;

use crate::capability::CapError;
use crate::cheri::CheriViolation;
use crate::driver::DriverError;
use crate::ebpf::{BpfError, maps::MapError};
use crate::fs::FsError;
use crate::fs::procfs::ProcError;
use crate::hypervisor::HypervisorError;
use crate::ipc::IpcError;
use crate::io_uring::IoUringError;
use crate::livepatch::PatchError;
use crate::net::socket::SocketError;
use crate::syscall::SyscallError;
use crate::secure::BootChainError;

// ============================================================================
// 时间戳支持
// ============================================================================

static TIMESTAMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[inline]
pub fn get_timestamp() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        let time: u64;
        time = crate::arch::asm_helpers::rdtime();
        if time > 0 { return time; }
    }
    TIMESTAMP_COUNTER.fetch_add(1, Ordering::SeqCst)
}

#[inline]
pub fn timestamp_to_us(timestamp: u64) -> u64 { timestamp / 10 }

#[inline]
pub fn timestamp_to_ms(timestamp: u64) -> u64 { timestamp_to_us(timestamp) / 1000 }


// ============================================================================
// 错误严重级别
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ErrorSeverity {
    Debug = 0, Info = 1, Warning = 2, Error = 3, Critical = 4, Fatal = 5,
}

impl ErrorSeverity {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Debug => "DEBUG", Self::Info => "INFO", Self::Warning => "WARN",
            Self::Error => "ERROR", Self::Critical => "CRIT", Self::Fatal => "FATAL",
        }
    }
    pub const fn ansi_color(&self) -> &'static str {
        match self {
            Self::Debug => "\x1b[36m", Self::Info => "\x1b[32m", Self::Warning => "\x1b[33m",
            Self::Error => "\x1b[31m", Self::Critical => "\x1b[35m", Self::Fatal => "\x1b[41;37m",
        }
    }
    pub const fn ansi_reset() -> &'static str { "\x1b[0m" }
}

impl fmt::Display for ErrorSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { write!(f, "{}", self.as_str()) }
}

// ============================================================================
// 栈帧信息
// ============================================================================

#[derive(Debug, Clone)]
pub struct StackFrame {
    pub return_address: usize,
    pub frame_pointer: usize,
    pub symbol_name: Option<&'static str>,
    pub symbol_offset: usize,
}

impl StackFrame {
    pub const fn new(return_address: usize, frame_pointer: usize) -> Self {
        Self { return_address, frame_pointer, symbol_name: None, symbol_offset: 0 }
    }
    pub fn with_symbol(mut self, name: &'static str, offset: usize) -> Self {
        self.symbol_name = Some(name);
        self.symbol_offset = offset;
        self
    }
}

impl fmt::Display for StackFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(name) = self.symbol_name {
            write!(f, "{:#x} <{}+{:#x}>", self.return_address, name, self.symbol_offset)
        } else {
            write!(f, "{:#x}", self.return_address)
        }
    }
}

#[derive(Debug, Clone)]
pub struct Backtrace {
    pub frames: Vec<StackFrame>,
    pub truncated: bool,
}

impl Backtrace {
    pub const MAX_FRAMES: usize = 32;
    
    pub fn capture() -> Self {
        let mut frames = Vec::with_capacity(Self::MAX_FRAMES);
        let mut truncated = false;
        let mut fp: usize = 0;
        #[cfg(target_arch = "riscv64")]
        unsafe { core::arch::asm!("mv {}, fp", out(reg) fp); }
        for _ in 0..Self::MAX_FRAMES {
            if fp == 0 || fp < 0x8000_0000 || fp > 0x9000_0000 { break; }
            let ra = unsafe { *((fp - 8) as *const usize) };
            let prev_fp = unsafe { *((fp - 16) as *const usize) };
            if ra == 0 { break; }
            frames.push(StackFrame::new(ra, fp));
            fp = prev_fp;
        }
        if fp != 0 && fp >= 0x8000_0000 { truncated = true; }
        Self { frames, truncated }
    }
    
    pub const fn empty() -> Self { Self { frames: Vec::new(), truncated: false } }
    pub fn is_empty(&self) -> bool { self.frames.is_empty() }
    pub fn len(&self) -> usize { self.frames.len() }
}

impl fmt::Display for Backtrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.frames.is_empty() { return write!(f, "  <no backtrace available>"); }
        for (i, frame) in self.frames.iter().enumerate() { writeln!(f, "  #{:2}: {}", i, frame)?; }
        if self.truncated { writeln!(f, "  ... (truncated)")?; }
        Ok(())
    }
}


// ============================================================================
// 错误上下文
// ============================================================================

static ERROR_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

fn generate_error_id() -> u64 { ERROR_ID_COUNTER.fetch_add(1, Ordering::SeqCst) }

#[derive(Debug)]
pub struct ErrorContext {
    pub error: KernelError,
    pub file: &'static str,
    pub line: u32,
    pub column: u32,
    pub function: &'static str,
    pub timestamp: u64,
    pub error_id: u64,
    pub severity: ErrorSeverity,
    pub backtrace: Backtrace,
    pub cause: Option<Box<ErrorContext>>,
    pub extra_info: Option<String>,
    pub hart_id: usize,
    pub pid: Option<u32>,
}

impl ErrorContext {
    pub fn new(error: KernelError, file: &'static str, line: u32, column: u32, function: &'static str) -> Self {
        let severity = Self::infer_severity(&error);
        let hart_id = crate::arch::hartid();
        Self {
            error, file, line, column, function,
            timestamp: get_timestamp(), error_id: generate_error_id(), severity,
            backtrace: Backtrace::capture(), cause: None, extra_info: None, hart_id, pid: None,
        }
    }
    
    pub fn new_without_backtrace(error: KernelError, file: &'static str, line: u32, column: u32, function: &'static str) -> Self {
        let severity = Self::infer_severity(&error);
        let hart_id = crate::arch::hartid();
        Self {
            error, file, line, column, function,
            timestamp: get_timestamp(), error_id: generate_error_id(), severity,
            backtrace: Backtrace::empty(), cause: None, extra_info: None, hart_id, pid: None,
        }
    }
    
    fn infer_severity(error: &KernelError) -> ErrorSeverity {
        match error {
            KernelError::Cheri(_) => ErrorSeverity::Critical,
            KernelError::SecureBoot(_) => ErrorSeverity::Fatal,
            KernelError::Sync(SyncError::Deadlock) => ErrorSeverity::Fatal,
            KernelError::Memory(MemoryError::OutOfMemory) => ErrorSeverity::Critical,
            KernelError::Ipc(IpcError::WouldBlock) => ErrorSeverity::Warning,
            KernelError::Sync(SyncError::Timeout) => ErrorSeverity::Warning,
            KernelError::Sync(SyncError::Busy) => ErrorSeverity::Warning,
            KernelError::Hypervisor(HypervisorError::OutOfMemory) => ErrorSeverity::Critical,
            KernelError::ProcFs(_) => ErrorSeverity::Warning,
            _ => ErrorSeverity::Error,
        }
    }
    
    pub fn with_severity(mut self, severity: ErrorSeverity) -> Self { self.severity = severity; self }
    pub fn with_cause(mut self, cause: ErrorContext) -> Self { self.cause = Some(Box::new(cause)); self }
    pub fn chain(self, message: &'static str) -> Self {
        let new_error = KernelError::Other(message);
        let mut new_ctx = ErrorContext::new_without_backtrace(new_error, self.file, self.line, self.column, self.function);
        new_ctx.cause = Some(Box::new(self));
        new_ctx
    }
    pub fn with_info(mut self, info: String) -> Self { self.extra_info = Some(info); self }
    pub fn with_pid(mut self, pid: u32) -> Self { self.pid = Some(pid); self }
    
    pub fn chain_depth(&self) -> usize {
        let mut depth = 1;
        let mut current = &self.cause;
        while let Some(cause) = current { depth += 1; current = &cause.cause; }
        depth
    }
    
    pub fn root_cause(&self) -> &ErrorContext {
        let mut current = self;
        while let Some(ref cause) = current.cause { current = cause; }
        current
    }
    
    pub fn is_recoverable(&self) -> bool { self.error.is_recoverable() }
    pub fn is_fatal(&self) -> bool { self.error.is_fatal() || self.severity >= ErrorSeverity::Fatal }
    pub fn to_errno(&self) -> isize { self.error.to_errno() }
    
    pub fn short_description(&self) -> String {
        alloc::format!("[E{:06}] {} at {}:{}", self.error_id, self.error, self.file, self.line)
    }
    
    pub fn log_short(&self) {
        crate::println!("{}{} {}{}", self.severity.ansi_color(), self.severity, self.short_description(), ErrorSeverity::ansi_reset());
    }
}

impl fmt::Display for ErrorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[E{:06}] {} at {}:{}:{} in {}()", self.error_id, self.error, self.file, self.line, self.column, self.function)
    }
}

impl From<KernelError> for ErrorContext {
    fn from(error: KernelError) -> Self { Self::new_without_backtrace(error, "<unknown>", 0, 0, "<unknown>") }
}


// ============================================================================
// kernel_error! 宏
// ============================================================================

#[macro_export]
macro_rules! function_name {
    () => {{ fn f() {} fn type_name_of<T>(_: T) -> &'static str { core::any::type_name::<T>() } let name = type_name_of(f); &name[..name.len() - 3] }};
}

#[macro_export]
macro_rules! kernel_error {
    ($error:expr) => {{ $crate::error::ErrorContext::new($error.into(), file!(), line!(), column!(), $crate::function_name!()) }};
    ($error:expr, $info:expr) => {{ $crate::error::ErrorContext::new($error.into(), file!(), line!(), column!(), $crate::function_name!()).with_info(alloc::string::String::from($info)) }};
    (fast: $error:expr) => {{ $crate::error::ErrorContext::new_without_backtrace($error.into(), file!(), line!(), column!(), $crate::function_name!()) }};
    (fast: $error:expr, $info:expr) => {{ $crate::error::ErrorContext::new_without_backtrace($error.into(), file!(), line!(), column!(), $crate::function_name!()).with_info(alloc::string::String::from($info)) }};
}

pub type ContextResult<T> = Result<T, ErrorContext>;

pub trait ResultExt<T> {
    fn with_context(self, file: &'static str, line: u32, column: u32, function: &'static str) -> ContextResult<T>;
}

impl<T> ResultExt<T> for KernelResult<T> {
    fn with_context(self, file: &'static str, line: u32, column: u32, function: &'static str) -> ContextResult<T> {
        self.map_err(|e| ErrorContext::new(e, file, line, column, function))
    }
}

#[macro_export]
macro_rules! context {
    ($result:expr) => {{ use $crate::error::ResultExt; $result.with_context(file!(), line!(), column!(), $crate::function_name!()) }};
}


// ============================================================================
// KernelError 枚举
// ============================================================================

#[derive(Debug, Clone)]
pub enum KernelError {
    Memory(MemoryError),
    Process(ProcessError),
    FileSystem(FsError),
    Driver(DriverError),
    Capability(CapError),
    Ipc(IpcError),
    Cheri(CheriViolation),
    IoUring(IoUringError),
    Syscall(SyscallError),
    Network(SocketError),
    Livepatch(PatchError),
    SecureBoot(BootChainError),
    Scheduler(SchedulerError),
    Sync(SyncError),
    Hypervisor(HypervisorError),
    ProcFs(ProcError),
    Bpf(BpfError),
    BpfMap(MapError),
    Other(&'static str),
}

impl fmt::Display for KernelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(e) => write!(f, "Memory error: {}", e),
            Self::Process(e) => write!(f, "Process error: {}", e),
            Self::FileSystem(e) => write!(f, "Filesystem error: {:?}", e),
            Self::Driver(e) => write!(f, "Driver error: {:?}", e),
            Self::Capability(e) => write!(f, "Capability error: {:?}", e),
            Self::Ipc(e) => write!(f, "IPC error: {:?}", e),
            Self::Cheri(e) => write!(f, "CHERI violation: {}", e),
            Self::IoUring(e) => write!(f, "io_uring error: {:?}", e),
            Self::Syscall(e) => write!(f, "Syscall error: {:?}", e),
            Self::Network(e) => write!(f, "Network error: {:?}", e),
            Self::Livepatch(e) => write!(f, "Livepatch error: {:?}", e),
            Self::SecureBoot(e) => write!(f, "Secure boot error: {:?}", e),
            Self::Scheduler(e) => write!(f, "Scheduler error: {}", e),
            Self::Sync(e) => write!(f, "Sync error: {}", e),
            Self::Hypervisor(e) => write!(f, "Hypervisor error: {:?}", e),
            Self::ProcFs(e) => write!(f, "ProcFS error: {:?}", e),
            Self::Bpf(e) => write!(f, "eBPF error: {:?}", e),
            Self::BpfMap(e) => write!(f, "eBPF Map error: {:?}", e),
            Self::Other(msg) => write!(f, "{}", msg),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    OutOfMemory, InvalidAddress, MisalignedAddress, PageFault, PermissionDenied,
    RegionOverlap, RegionNotFound, MappingFailed, UnmappingFailed,
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfMemory => write!(f, "Out of memory"),
            Self::InvalidAddress => write!(f, "Invalid address"),
            Self::MisalignedAddress => write!(f, "Misaligned address"),
            Self::PageFault => write!(f, "Page fault"),
            Self::PermissionDenied => write!(f, "Permission denied"),
            Self::RegionOverlap => write!(f, "Memory region overlap"),
            Self::RegionNotFound => write!(f, "Memory region not found"),
            Self::MappingFailed => write!(f, "Memory mapping failed"),
            Self::UnmappingFailed => write!(f, "Memory unmapping failed"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessError {
    NotFound, AlreadyExists, InvalidPid, InvalidTid, AlreadyExited,
    CreationFailed, TerminationFailed, ResourceLimit, InvalidState, PermissionDenied,
}

impl fmt::Display for ProcessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => write!(f, "Process not found"),
            Self::AlreadyExists => write!(f, "Process already exists"),
            Self::InvalidPid => write!(f, "Invalid process ID"),
            Self::InvalidTid => write!(f, "Invalid thread ID"),
            Self::AlreadyExited => write!(f, "Process already exited"),
            Self::CreationFailed => write!(f, "Process creation failed"),
            Self::TerminationFailed => write!(f, "Process termination failed"),
            Self::ResourceLimit => write!(f, "Resource limit exceeded"),
            Self::InvalidState => write!(f, "Invalid process state"),
            Self::PermissionDenied => write!(f, "Permission denied"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulerError {
    NoRunnableTask, QueueFull, InvalidPriority, InvalidCpuId, MigrationFailed, LoadBalanceFailed,
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoRunnableTask => write!(f, "No runnable task"),
            Self::QueueFull => write!(f, "Task queue full"),
            Self::InvalidPriority => write!(f, "Invalid priority"),
            Self::InvalidCpuId => write!(f, "Invalid CPU ID"),
            Self::MigrationFailed => write!(f, "Task migration failed"),
            Self::LoadBalanceFailed => write!(f, "Load balance failed"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncError { Deadlock, LockHeld, LockNotHeld, Timeout, Interrupted, Busy }

impl fmt::Display for SyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deadlock => write!(f, "Deadlock detected"),
            Self::LockHeld => write!(f, "Lock already held"),
            Self::LockNotHeld => write!(f, "Lock not held"),
            Self::Timeout => write!(f, "Operation timed out"),
            Self::Interrupted => write!(f, "Operation interrupted"),
            Self::Busy => write!(f, "Resource busy"),
        }
    }
}


// ============================================================================
// From trait 实现
// ============================================================================

impl From<MemoryError> for KernelError { fn from(e: MemoryError) -> Self { KernelError::Memory(e) } }
impl From<ProcessError> for KernelError { fn from(e: ProcessError) -> Self { KernelError::Process(e) } }
impl From<FsError> for KernelError { fn from(e: FsError) -> Self { KernelError::FileSystem(e) } }
impl From<DriverError> for KernelError { fn from(e: DriverError) -> Self { KernelError::Driver(e) } }
impl From<CapError> for KernelError { fn from(e: CapError) -> Self { KernelError::Capability(e) } }
impl From<IpcError> for KernelError { fn from(e: IpcError) -> Self { KernelError::Ipc(e) } }
impl From<CheriViolation> for KernelError { fn from(e: CheriViolation) -> Self { KernelError::Cheri(e) } }
impl From<IoUringError> for KernelError { fn from(e: IoUringError) -> Self { KernelError::IoUring(e) } }
impl From<SyscallError> for KernelError { fn from(e: SyscallError) -> Self { KernelError::Syscall(e) } }
impl From<SocketError> for KernelError { fn from(e: SocketError) -> Self { KernelError::Network(e) } }
impl From<PatchError> for KernelError { fn from(e: PatchError) -> Self { KernelError::Livepatch(e) } }
impl From<BootChainError> for KernelError { fn from(e: BootChainError) -> Self { KernelError::SecureBoot(e) } }
impl From<SchedulerError> for KernelError { fn from(e: SchedulerError) -> Self { KernelError::Scheduler(e) } }
impl From<SyncError> for KernelError { fn from(e: SyncError) -> Self { KernelError::Sync(e) } }
impl From<HypervisorError> for KernelError { fn from(e: HypervisorError) -> Self { KernelError::Hypervisor(e) } }
impl From<ProcError> for KernelError { fn from(e: ProcError) -> Self { KernelError::ProcFs(e) } }
impl From<BpfError> for KernelError { fn from(e: BpfError) -> Self { KernelError::Bpf(e) } }
impl From<MapError> for KernelError { fn from(e: MapError) -> Self { KernelError::BpfMap(e) } }
impl From<&'static str> for KernelError { fn from(msg: &'static str) -> Self { KernelError::Other(msg) } }

pub type KernelResult<T> = Result<T, KernelError>;

impl KernelError {
    pub fn is_recoverable(&self) -> bool {
        matches!(self,
            Self::Memory(MemoryError::OutOfMemory) |
            Self::Ipc(IpcError::WouldBlock) |
            Self::Sync(SyncError::Timeout) |
            Self::Sync(SyncError::Busy)
        )
    }
    
    pub fn is_fatal(&self) -> bool {
        matches!(self, Self::Cheri(_) | Self::SecureBoot(_) | Self::Sync(SyncError::Deadlock))
    }
    
    pub fn to_errno(&self) -> isize {
        match self {
            Self::Memory(MemoryError::OutOfMemory) => -12,
            Self::Memory(MemoryError::InvalidAddress) => -14,
            Self::Memory(MemoryError::PermissionDenied) => -13,
            Self::Process(ProcessError::NotFound) => -3,
            Self::Process(ProcessError::PermissionDenied) => -1,
            Self::FileSystem(FsError::NotFound) => -2,
            Self::FileSystem(FsError::PermissionDenied) => -13,
            Self::FileSystem(FsError::AlreadyExists) => -17,
            Self::FileSystem(FsError::IoError) => -5,
            Self::Ipc(IpcError::WouldBlock) => -11,
            Self::Ipc(IpcError::InvalidEndpoint) => -22,
            Self::Sync(SyncError::Timeout) => -110,
            Self::Sync(SyncError::Deadlock) => -35,
            Self::Hypervisor(HypervisorError::OutOfMemory) => -12,
            Self::ProcFs(ProcError::NotFound) => -2,
            Self::Bpf(BpfError::OutOfMemory) => -12,
            Self::BpfMap(MapError::KeyNotFound) => -2,
            _ => -22,
        }
    }
}


// ============================================================================
// 自测试
// ============================================================================

pub fn self_test() {
    crate::println!("[error] Running self-test...");
    
    // 基本错误转换测试
    let mem_err: KernelError = MemoryError::OutOfMemory.into();
    assert!(matches!(mem_err, KernelError::Memory(MemoryError::OutOfMemory)));
    crate::println!("[error] MemoryError conversion: {}", mem_err);
    
    let proc_err: KernelError = ProcessError::NotFound.into();
    assert!(matches!(proc_err, KernelError::Process(ProcessError::NotFound)));
    
    // 测试 is_recoverable 和 is_fatal
    assert!(KernelError::Memory(MemoryError::OutOfMemory).is_recoverable());
    assert!(KernelError::Sync(SyncError::Deadlock).is_fatal());
    
    // 测试 to_errno
    assert_eq!(KernelError::Memory(MemoryError::OutOfMemory).to_errno(), -12);
    
    // 测试 ErrorContext (使用 fast 模式避免内存耗尽)
    {
        let ctx = crate::kernel_error!(fast: KernelError::Memory(MemoryError::OutOfMemory));
        assert!(!ctx.file.is_empty());
        assert!(ctx.line > 0);
        assert!(ctx.timestamp > 0);
        assert!(ctx.error_id > 0);
        crate::println!("[error] ErrorContext test passed");
    }
    
    // 测试错误链 (使用 fast 模式)
    {
        let inner = crate::kernel_error!(fast: KernelError::Memory(MemoryError::OutOfMemory));
        let outer = crate::kernel_error!(fast: KernelError::Process(ProcessError::CreationFailed)).with_cause(inner);
        assert_eq!(outer.chain_depth(), 2);
        crate::println!("[error] Error chain test passed");
    }
    
    crate::println!("[error] Self-test passed!");
    
    // 运行属性测试 (使用减少的迭代次数)
    property_tests::run_property_tests();
}

// ============================================================================
// 属性测试模块
// ============================================================================

pub mod property_tests {
    use super::*;
    
    struct SimpleRng { state: u64 }
    impl SimpleRng {
        fn new(seed: u64) -> Self { Self { state: seed } }
        fn next(&mut self) -> u64 {
            self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            self.state
        }
    }
    
    fn property_14_error_context_completeness() {
        let mut rng = SimpleRng::new(42);
        let error_generators: [fn() -> KernelError; 8] = [
            || KernelError::Memory(MemoryError::OutOfMemory),
            || KernelError::Memory(MemoryError::InvalidAddress),
            || KernelError::Process(ProcessError::NotFound),
            || KernelError::Process(ProcessError::CreationFailed),
            || KernelError::Scheduler(SchedulerError::NoRunnableTask),
            || KernelError::Sync(SyncError::Deadlock),
            || KernelError::Sync(SyncError::Timeout),
            || KernelError::Other("test error"),
        ];
        for _ in 0..10 {
            let idx = (rng.next() % error_generators.len() as u64) as usize;
            let ctx = crate::kernel_error!(fast: error_generators[idx]());
            assert!(!ctx.file.is_empty());
            assert!(ctx.line > 0);
            assert!(ctx.timestamp > 0);
            assert!(ctx.error_id > 0);
        }
    }
    
    fn property_14_error_id_uniqueness() {
        let mut prev_id: Option<u64> = None;
        for _ in 0..10 {
            let ctx = crate::kernel_error!(fast: KernelError::Other("test"));
            if let Some(pid) = prev_id { assert!(ctx.error_id > pid); }
            prev_id = Some(ctx.error_id);
        }
    }
    
    fn property_14_timestamp_monotonicity() {
        let mut prev_ts: Option<u64> = None;
        for _ in 0..10 {
            let ctx = crate::kernel_error!(fast: KernelError::Other("test"));
            if let Some(pts) = prev_ts { assert!(ctx.timestamp >= pts); }
            prev_ts = Some(ctx.timestamp);
        }
    }
    
    fn property_14_severity_inference() {
        let pairs: [(KernelError, ErrorSeverity); 5] = [
            (KernelError::Memory(MemoryError::OutOfMemory), ErrorSeverity::Critical),
            (KernelError::Sync(SyncError::Deadlock), ErrorSeverity::Fatal),
            (KernelError::Ipc(IpcError::WouldBlock), ErrorSeverity::Warning),
            (KernelError::Process(ProcessError::NotFound), ErrorSeverity::Error),
            (KernelError::Other("test"), ErrorSeverity::Error),
        ];
        for (error, expected) in pairs.iter() {
            let ctx = crate::kernel_error!(fast: error.clone());
            assert_eq!(ctx.severity, *expected);
        }
    }
    
    fn property_14_error_chain_integrity() {
        let mut rng = SimpleRng::new(67890);
        for _ in 0..10 {
            let depth = ((rng.next() % 3) + 1) as usize;
            let mut ctx = crate::kernel_error!(fast: KernelError::Memory(MemoryError::OutOfMemory));
            for _ in 1..depth {
                ctx = crate::kernel_error!(fast: KernelError::Other("chain")).with_cause(ctx);
            }
            assert_eq!(ctx.chain_depth(), depth);
            assert!(matches!(ctx.root_cause().error, KernelError::Memory(MemoryError::OutOfMemory)));
        }
    }
    
    fn property_14_fast_mode_no_backtrace() {
        for _ in 0..10 {
            let ctx = crate::kernel_error!(fast: KernelError::Other("test"));
            assert!(ctx.backtrace.is_empty());
        }
    }
    
    pub fn run_property_tests() {
        crate::println!("[error_property_tests] Running Property 14 tests (10 iterations each)...");
        property_14_error_context_completeness();
        crate::println!("[error_property_tests] property_14_error_context_completeness passed");
        property_14_error_id_uniqueness();
        crate::println!("[error_property_tests] property_14_error_id_uniqueness passed");
        property_14_timestamp_monotonicity();
        crate::println!("[error_property_tests] property_14_timestamp_monotonicity passed");
        property_14_severity_inference();
        crate::println!("[error_property_tests] property_14_severity_inference passed");
        property_14_error_chain_integrity();
        crate::println!("[error_property_tests] property_14_error_chain_integrity passed");
        property_14_fast_mode_no_backtrace();
        crate::println!("[error_property_tests] property_14_fast_mode_no_backtrace passed");
        crate::println!("[error_property_tests] All Property 14 tests passed!");
    }
}
