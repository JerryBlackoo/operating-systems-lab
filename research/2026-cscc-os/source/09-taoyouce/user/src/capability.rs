//! 能力系统用户态接口
//!
//! 提供能力操作的用户态封装

use crate::syscall::*;

/// 能力类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CapType {
    /// 空能力
    Null = 0,
    /// 内存能力
    Memory = 1,
    /// 端点能力（IPC）
    Endpoint = 2,
    /// 通知能力
    Notification = 3,
    /// 线程控制块能力
    TCB = 4,
    /// 地址空间能力
    CNode = 5,
    /// 页表能力
    PageTable = 6,
    /// 中断能力
    IRQ = 7,
    /// 设备能力
    Device = 8,
    /// 文件能力
    File = 9,
}

/// 能力权限
#[derive(Debug, Clone, Copy, Default)]
pub struct CapRights(u32);

impl CapRights {
    pub const NONE: Self = Self(0);
    pub const READ: Self = Self(1 << 0);
    pub const WRITE: Self = Self(1 << 1);
    pub const EXECUTE: Self = Self(1 << 2);
    pub const GRANT: Self = Self(1 << 3);
    pub const REVOKE: Self = Self(1 << 4);
    
    pub const fn new(bits: u32) -> Self {
        Self(bits)
    }
    
    pub fn contains(&self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
    
    pub fn bits(&self) -> u32 {
        self.0
    }
}

impl core::ops::BitOr for CapRights {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitAnd for CapRights {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

/// 能力句柄
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CapHandle(pub u64);

impl CapHandle {
    pub const NULL: Self = Self(0);
    
    pub fn is_null(&self) -> bool {
        self.0 == 0
    }
}

/// 能力错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapError {
    InvalidHandle,
    PermissionDenied,
    TypeMismatch,
    AlreadyRevoked,
    OutOfMemory,
    BoundsViolation,
    Unknown(i32),
}

impl From<isize> for CapError {
    fn from(code: isize) -> Self {
        match code {
            -1 => CapError::InvalidHandle,
            -2 => CapError::PermissionDenied,
            -3 => CapError::TypeMismatch,
            -4 => CapError::AlreadyRevoked,
            -5 => CapError::OutOfMemory,
            -6 => CapError::BoundsViolation,
            code => CapError::Unknown(code as i32),
        }
    }
}

/// 创建内存能力
pub fn cap_create_memory(addr: usize, size: usize, rights: CapRights) -> Result<CapHandle, CapError> {
    let ret = sys_cap_create(CapType::Memory as u32, addr, size, rights.bits());
    if ret < 0 {
        Err(CapError::from(ret))
    } else {
        Ok(CapHandle(ret as u64))
    }
}

/// 创建端点能力
pub fn cap_create_endpoint(rights: CapRights) -> Result<CapHandle, CapError> {
    let ret = sys_cap_create(CapType::Endpoint as u32, 0, 0, rights.bits());
    if ret < 0 {
        Err(CapError::from(ret))
    } else {
        Ok(CapHandle(ret as u64))
    }
}

/// 派生能力（权限衰减）
pub fn cap_derive(parent: CapHandle, new_rights: CapRights) -> Result<CapHandle, CapError> {
    let ret = sys_cap_derive(parent.0, new_rights.bits());
    if ret < 0 {
        Err(CapError::from(ret))
    } else {
        Ok(CapHandle(ret as u64))
    }
}

/// 撤销能力（级联撤销所有派生能力）
pub fn cap_revoke(handle: CapHandle) -> Result<usize, CapError> {
    let ret = sys_cap_revoke(handle.0);
    if ret < 0 {
        Err(CapError::from(ret))
    } else {
        Ok(ret as usize)
    }
}

/// 检查能力权限
pub fn cap_check(handle: CapHandle, required: CapRights) -> Result<bool, CapError> {
    let ret = sys_cap_check(handle.0, required.bits());
    if ret < 0 {
        Err(CapError::from(ret))
    } else {
        Ok(ret != 0)
    }
}

/// 获取能力信息
pub fn cap_info(handle: CapHandle) -> Result<CapInfo, CapError> {
    let mut info = CapInfo::default();
    let ret = sys_cap_info(handle.0, &mut info);
    if ret < 0 {
        Err(CapError::from(ret))
    } else {
        Ok(info)
    }
}

/// 能力信息
#[derive(Debug, Clone, Default)]
#[repr(C)]
pub struct CapInfo {
    pub cap_type: u32,
    pub rights: u32,
    pub base: u64,
    pub length: u64,
    pub parent_id: u64,
    pub child_count: u32,
    pub _reserved: u32,
}

/// 通过能力读取内存
pub fn cap_read(handle: CapHandle, offset: usize, buf: &mut [u8]) -> Result<usize, CapError> {
    let ret = sys_cap_read(handle.0, offset, buf.as_mut_ptr(), buf.len());
    if ret < 0 {
        Err(CapError::from(ret))
    } else {
        Ok(ret as usize)
    }
}

/// 通过能力写入内存
pub fn cap_write(handle: CapHandle, offset: usize, buf: &[u8]) -> Result<usize, CapError> {
    let ret = sys_cap_write(handle.0, offset, buf.as_ptr(), buf.len());
    if ret < 0 {
        Err(CapError::from(ret))
    } else {
        Ok(ret as usize)
    }
}
