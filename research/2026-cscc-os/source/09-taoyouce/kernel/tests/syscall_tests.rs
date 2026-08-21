//! 系统调用集成测试
//!
//! 验证系统调用接口的以下属性：
//! 1. 参数验证：无效参数应返回错误
//! 2. 权限检查：无权限操作应被拒绝
//! 3. 返回值正确性：系统调用返回正确的值
//! 4. 状态一致性：系统调用后系统状态一致
//! 5. 错误处理：错误情况正确处理

use crate::{SimpleRng, println};
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

/// 系统调用号
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum SyscallId {
    Exit = 0,
    Fork = 1,
    Read = 2,
    Write = 3,
    Open = 4,
    Close = 5,
    Mmap = 6,
    Munmap = 7,
    Send = 8,
    Recv = 9,
    CapCreate = 10,
    CapDerive = 11,
}

/// 系统调用错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SyscallError {
    InvalidArg,
    PermissionDenied,
    NotFound,
    NoMemory,
    WouldBlock,
    InvalidFd,
    InvalidCap,
}

/// 系统调用结果
type SyscallResult = Result<usize, SyscallError>;

/// 模拟进程
struct MockProcess {
    pid: usize,
    fds: BTreeMap<usize, MockFd>,
    caps: BTreeMap<usize, MockCap>,
    next_fd: usize,
    next_cap: usize,
    memory_regions: Vec<(usize, usize)>, // (start, size)
}

/// 模拟文件描述符
#[derive(Debug, Clone)]
struct MockFd {
    readable: bool,
    writable: bool,
}

/// 模拟能力
#[derive(Debug, Clone)]
struct MockCap {
    rights: u32,
}

impl MockProcess {
    fn new(pid: usize) -> Self {
        Self {
            pid,
            fds: BTreeMap::new(),
            caps: BTreeMap::new(),
            next_fd: 0,
            next_cap: 0,
            memory_regions: Vec::new(),
        }
    }
    
    fn open(&mut self, readable: bool, writable: bool) -> SyscallResult {
        let fd = self.next_fd;
        self.next_fd += 1;
        self.fds.insert(fd, MockFd { readable, writable });
        Ok(fd)
    }
    
    fn close(&mut self, fd: usize) -> SyscallResult {
        if self.fds.remove(&fd).is_some() {
            Ok(0)
        } else {
            Err(SyscallError::InvalidFd)
        }
    }
    
    fn read(&self, fd: usize, _buf: usize, _len: usize) -> SyscallResult {
        match self.fds.get(&fd) {
            Some(f) if f.readable => Ok(0), // 模拟读取 0 字节
            Some(_) => Err(SyscallError::PermissionDenied),
            None => Err(SyscallError::InvalidFd),
        }
    }
    
    fn write(&self, fd: usize, _buf: usize, len: usize) -> SyscallResult {
        match self.fds.get(&fd) {
            Some(f) if f.writable => Ok(len), // 模拟写入全部
            Some(_) => Err(SyscallError::PermissionDenied),
            None => Err(SyscallError::InvalidFd),
        }
    }
    
    fn mmap(&mut self, addr: usize, size: usize) -> SyscallResult {
        if size == 0 {
            return Err(SyscallError::InvalidArg);
        }
        
        // 检查重叠
        for &(start, len) in &self.memory_regions {
            if addr < start + len && start < addr + size {
                return Err(SyscallError::InvalidArg);
            }
        }
        
        self.memory_regions.push((addr, size));
        Ok(addr)
    }
    
    fn munmap(&mut self, addr: usize, size: usize) -> SyscallResult {
        if let Some(pos) = self.memory_regions.iter().position(|&(a, s)| a == addr && s == size) {
            self.memory_regions.remove(pos);
            Ok(0)
        } else {
            Err(SyscallError::NotFound)
        }
    }
    
    fn cap_create(&mut self, rights: u32) -> SyscallResult {
        let cap = self.next_cap;
        self.next_cap += 1;
        self.caps.insert(cap, MockCap { rights });
        Ok(cap)
    }
    
    fn cap_derive(&mut self, src: usize, new_rights: u32) -> SyscallResult {
        match self.caps.get(&src) {
            Some(cap) => {
                // 检查 GRANT 权限
                if cap.rights & 0x8 == 0 {
                    return Err(SyscallError::PermissionDenied);
                }
                // 新权限必须是子集
                let derived_rights = cap.rights & new_rights;
                let new_cap = self.next_cap;
                self.next_cap += 1;
                self.caps.insert(new_cap, MockCap { rights: derived_rights });
                Ok(new_cap)
            }
            None => Err(SyscallError::InvalidCap),
        }
    }
}

/// 属性1：无效文件描述符
/// 对无效 fd 的操作应返回错误
#[test_case]
fn test_invalid_fd() -> bool {
    let proc = MockProcess::new(1);
    
    // 读取无效 fd
    match proc.read(999, 0, 100) {
        Err(SyscallError::InvalidFd) => {}
        _ => {
            println!("  错误: 无效 fd 读取应返回 InvalidFd");
            return false;
        }
    }
    
    // 写入无效 fd
    match proc.write(999, 0, 100) {
        Err(SyscallError::InvalidFd) => {}
        _ => {
            println!("  错误: 无效 fd 写入应返回 InvalidFd");
            return false;
        }
    }
    
    true
}

/// 属性2：权限检查
/// 无权限操作应被拒绝
#[test_case]
fn test_permission_check() -> bool {
    let mut proc = MockProcess::new(1);
    
    // 打开只读文件
    let fd = proc.open(true, false).unwrap();
    
    // 读取应成功
    if proc.read(fd, 0, 100).is_err() {
        println!("  错误: 只读文件读取应成功");
        return false;
    }
    
    // 写入应失败
    match proc.write(fd, 0, 100) {
        Err(SyscallError::PermissionDenied) => {}
        _ => {
            println!("  错误: 只读文件写入应返回 PermissionDenied");
            return false;
        }
    }
    
    true
}

/// 属性3：文件描述符生命周期
/// 关闭后 fd 应无效
#[test_case]
fn test_fd_lifecycle() -> bool {
    let mut proc = MockProcess::new(1);
    
    // 打开文件
    let fd = proc.open(true, true).unwrap();
    
    // 操作应成功
    if proc.read(fd, 0, 100).is_err() {
        println!("  错误: 打开后读取应成功");
        return false;
    }
    
    // 关闭
    proc.close(fd).unwrap();
    
    // 再次操作应失败
    match proc.read(fd, 0, 100) {
        Err(SyscallError::InvalidFd) => {}
        _ => {
            println!("  错误: 关闭后读取应返回 InvalidFd");
            return false;
        }
    }
    
    true
}

/// 属性4：mmap 参数验证
/// 无效参数应返回错误
#[test_case]
fn test_mmap_validation() -> bool {
    let mut proc = MockProcess::new(1);
    
    // 大小为 0 应失败
    match proc.mmap(0x1000, 0) {
        Err(SyscallError::InvalidArg) => {}
        _ => {
            println!("  错误: 大小为 0 应返回 InvalidArg");
            return false;
        }
    }
    
    // 正常映射应成功
    proc.mmap(0x1000, 0x1000).unwrap();
    
    // 重叠映射应失败
    match proc.mmap(0x1800, 0x1000) {
        Err(SyscallError::InvalidArg) => {}
        _ => {
            println!("  错误: 重叠映射应返回 InvalidArg");
            return false;
        }
    }
    
    true
}

/// 属性5：munmap 正确性
/// 取消映射应正确工作
#[test_case]
fn test_munmap() -> bool {
    let mut proc = MockProcess::new(1);
    
    // 映射
    proc.mmap(0x1000, 0x1000).unwrap();
    
    // 取消映射
    proc.munmap(0x1000, 0x1000).unwrap();
    
    // 再次取消应失败
    match proc.munmap(0x1000, 0x1000) {
        Err(SyscallError::NotFound) => {}
        _ => {
            println!("  错误: 重复取消映射应返回 NotFound");
            return false;
        }
    }
    
    // 可以重新映射
    proc.mmap(0x1000, 0x1000).unwrap();
    
    true
}

/// 属性6：能力创建
/// 创建的能力应有正确的权限
#[test_case]
fn test_cap_create() -> bool {
    let mut proc = MockProcess::new(1);
    
    let rights = 0xF; // 全部权限
    let cap = proc.cap_create(rights).unwrap();
    
    if proc.caps.get(&cap).map(|c| c.rights) != Some(rights) {
        println!("  错误: 能力权限不正确");
        return false;
    }
    
    true
}

/// 属性7：能力派生权限检查
/// 无 GRANT 权限不能派生
#[test_case]
fn test_cap_derive_permission() -> bool {
    let mut proc = MockProcess::new(1);
    
    // 创建无 GRANT 权限的能力
    let cap = proc.cap_create(0x3).unwrap(); // READ | WRITE，无 GRANT
    
    // 派生应失败
    match proc.cap_derive(cap, 0x1) {
        Err(SyscallError::PermissionDenied) => {}
        _ => {
            println!("  错误: 无 GRANT 权限派生应返回 PermissionDenied");
            return false;
        }
    }
    
    true
}

/// 属性8：能力派生权限衰减
/// 派生的能力权限应是原权限的子集
#[test_case]
fn test_cap_derive_attenuation() -> bool {
    let mut proc = MockProcess::new(1);
    
    // 创建有 GRANT 权限的能力
    let cap = proc.cap_create(0xF).unwrap(); // 全部权限
    
    // 派生
    let derived = proc.cap_derive(cap, 0x3).unwrap(); // 只要 READ | WRITE
    
    let derived_rights = proc.caps.get(&derived).map(|c| c.rights).unwrap();
    
    if derived_rights != 0x3 {
        println!("  错误: 派生权限应为 0x3，实际 0x{:x}", derived_rights);
        return false;
    }
    
    true
}

/// 属性9：无效能力
/// 对无效能力的操作应返回错误
#[test_case]
fn test_invalid_cap() -> bool {
    let mut proc = MockProcess::new(1);
    
    match proc.cap_derive(999, 0x1) {
        Err(SyscallError::InvalidCap) => {}
        _ => {
            println!("  错误: 无效能力派生应返回 InvalidCap");
            return false;
        }
    }
    
    true
}

/// 属性10：随机系统调用序列
/// 随机执行系统调用，验证状态一致性
#[test_case]
fn test_random_syscall_sequence() -> bool {
    let mut proc = MockProcess::new(1);
    let mut rng = SimpleRng::new(12345);
    let mut open_fds: Vec<usize> = Vec::new();
    let mut mapped_regions: Vec<(usize, usize)> = Vec::new();
    
    for _ in 0..100 {
        let op = rng.next_range(0, 6);
        
        match op {
            0 => {
                // open
                let readable = rng.next_range(0, 2) == 1;
                let writable = rng.next_range(0, 2) == 1;
                if let Ok(fd) = proc.open(readable, writable) {
                    open_fds.push(fd);
                }
            }
            1 => {
                // close
                if !open_fds.is_empty() {
                    let idx = rng.next_range(0, open_fds.len());
                    let fd = open_fds.swap_remove(idx);
                    proc.close(fd).ok();
                }
            }
            2 => {
                // read
                if !open_fds.is_empty() {
                    let idx = rng.next_range(0, open_fds.len());
                    let fd = open_fds[idx];
                    proc.read(fd, 0, 100).ok();
                }
            }
            3 => {
                // write
                if !open_fds.is_empty() {
                    let idx = rng.next_range(0, open_fds.len());
                    let fd = open_fds[idx];
                    proc.write(fd, 0, 100).ok();
                }
            }
            4 => {
                // mmap
                let addr = rng.next_range(0, 100) * 0x1000;
                let size = (rng.next_range(1, 10)) * 0x1000;
                if proc.mmap(addr, size).is_ok() {
                    mapped_regions.push((addr, size));
                }
            }
            _ => {
                // munmap
                if !mapped_regions.is_empty() {
                    let idx = rng.next_range(0, mapped_regions.len());
                    let (addr, size) = mapped_regions.swap_remove(idx);
                    proc.munmap(addr, size).ok();
                }
            }
        }
    }
    
    // 验证状态一致性
    if proc.fds.len() != open_fds.len() {
        println!("  错误: fd 数量不一致");
        return false;
    }
    
    if proc.memory_regions.len() != mapped_regions.len() {
        println!("  错误: 内存区域数量不一致");
        return false;
    }
    
    true
}
