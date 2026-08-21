//! 文件系统相关类型和函数
//!
//! 提供文件操作的用户态封装

use crate::syscall::*;

/// 文件打开标志
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenFlags(u32);

impl OpenFlags {
    pub const RDONLY: Self = Self(0);
    pub const WRONLY: Self = Self(1);
    pub const RDWR: Self = Self(2);
    pub const CREATE: Self = Self(0x40);
    pub const TRUNC: Self = Self(0x200);
    pub const APPEND: Self = Self(0x400);
}

impl core::ops::BitOr for OpenFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// 目录创建标志
#[derive(Debug, Clone, Copy, Default)]
pub struct MkdirFlags(u32);

impl MkdirFlags {
    pub const NONE: Self = Self(0);
    pub const PARENTS: Self = Self(1); // 创建父目录
}

/// 文件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    File,
    Directory,
    Device,
    Symlink,
    Unknown,
}

/// 目录项
#[derive(Debug, Clone, Copy)]
pub struct DirEntry {
    pub name: [u8; 64],
    pub name_len: usize,
    pub file_type: FileType,
    pub size: u64,
    pub inode: u64,
}

impl DirEntry {
    pub fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("<invalid>")
    }
}

/// 文件状态信息
#[repr(C)]
#[derive(Debug, Clone, Default)]
pub struct FileStat {
    pub dev: u64,
    pub inode: u64,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
}

/// 文件系统错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    NotDirectory,
    IsDirectory,
    NotEmpty,
    InvalidPath,
    IoError,
    NoSpace,
    TooManyOpenFiles,
    Unknown(i32),
}

impl From<isize> for FsError {
    fn from(code: isize) -> Self {
        match code {
            -2 => FsError::NotFound,
            -13 => FsError::PermissionDenied,
            -17 => FsError::AlreadyExists,
            -20 => FsError::NotDirectory,
            -21 => FsError::IsDirectory,
            -39 => FsError::NotEmpty,
            -22 => FsError::InvalidPath,
            -5 => FsError::IoError,
            -28 => FsError::NoSpace,
            -24 => FsError::TooManyOpenFiles,
            code => FsError::Unknown(code as i32),
        }
    }
}

/// 打开文件
pub fn open(path: &str, flags: OpenFlags) -> Result<usize, FsError> {
    let ret = sys_open(path, flags.0);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(ret as usize)
    }
}

/// 关闭文件
pub fn close(fd: usize) -> Result<(), FsError> {
    let ret = sys_close(fd);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(())
    }
}

/// 读取文件
pub fn read(fd: usize, buf: &mut [u8]) -> Result<usize, FsError> {
    let ret = sys_read(fd, buf);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(ret as usize)
    }
}

/// 写入文件
pub fn write(fd: usize, buf: &[u8]) -> Result<usize, FsError> {
    let ret = sys_write(fd, buf);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(ret as usize)
    }
}

/// 创建目录
pub fn mkdir(path: &str, _flags: MkdirFlags) -> Result<(), FsError> {
    let ret = sys_mkdir(path);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(())
    }
}

/// 删除文件
pub fn unlink(path: &str) -> Result<(), FsError> {
    let ret = sys_unlink(path);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(())
    }
}

/// 删除目录
pub fn rmdir(path: &str) -> Result<(), FsError> {
    let ret = sys_rmdir(path);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(())
    }
}

/// 获取文件状态
pub fn stat(path: &str) -> Result<FileStat, FsError> {
    let mut stat = FileStat::default();
    let ret = sys_stat(path, &mut stat);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(stat)
    }
}

/// 获取当前工作目录
pub fn getcwd() -> Result<[u8; 256], FsError> {
    let mut buf = [0u8; 256];
    let ret = sys_getcwd(&mut buf);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(buf)
    }
}

/// 改变当前工作目录
pub fn chdir(path: &str) -> Result<(), FsError> {
    let ret = sys_chdir(path);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(())
    }
}

/// 读取目录项
pub fn readdir(fd: usize, entries: &mut [DirEntry]) -> Result<usize, FsError> {
    let ret = sys_getdents(fd, entries);
    if ret < 0 {
        Err(FsError::from(ret))
    } else {
        Ok(ret as usize)
    }
}

/// 复制文件
pub fn copy_file(src: &str, dst: &str) -> Result<u64, FsError> {
    let src_fd = open(src, OpenFlags::RDONLY)?;
    let dst_fd = open(dst, OpenFlags::WRONLY | OpenFlags::CREATE | OpenFlags::TRUNC)?;
    
    let mut buf = [0u8; 4096];
    let mut total = 0u64;
    
    loop {
        let n = read(src_fd, &mut buf)?;
        if n == 0 {
            break;
        }
        write(dst_fd, &buf[..n])?;
        total += n as u64;
    }
    
    close(src_fd)?;
    close(dst_fd)?;
    
    Ok(total)
}
