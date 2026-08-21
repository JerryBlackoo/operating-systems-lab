#[derive(Debug, Clone, Copy)]
#[repr(isize)]
#[allow(dead_code)]
pub enum SyscallError {
    /// 无效参数
    InvalidArgument = -22,
    BadAddress = -14,
    /// 权限不足
    PermissionDenied = -13,
    /// 资源不存在
    NotFound = -2,
    /// 资源已存在
    AlreadyExists = -17,
    /// 内存不足
    OutOfMemory = -12,
    /// 操作会阻塞
    WouldBlock = -11,
    /// 无效系统调用号
    InvalidSyscall = -38,
    /// 无效能力
    InvalidCapability = -9,
    /// I/O 错误
    IoError = -5,
    /// 可执行文件格式错误（非 ELF / 非脚本，Linux execve 返回 ENOEXEC）
    ExecFormat = -8,
}

#[allow(dead_code)]
pub type SyscallResult = Result<usize, SyscallError>;
