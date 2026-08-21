//! 标准 I/O 设备 (Stdin / Stdout)
//!
//! 提供 fd 0/1/2 的真实 File + Inode 实现，
//! 使得 dup / fcntl / close 等 fd 操作统一走 fd_table 路径。

use super::{File, FileStat, FileType, FileMode, FsError, Inode, Directory};

// -------- Stdin --------

pub struct Stdin;

unsafe impl Send for Stdin {}
unsafe impl Sync for Stdin {}

impl File for Stdin {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        // 通过控制台 UART 非阻塞读取（有多少读多少）。使用 `console::getc`，其
        // UART 基址按架构正确配置；切勿在此硬编码 RISC-V 的 0x1000_0000 MMIO
        // —— 在 LoongArch 上该地址无 TLB/DMW 映射，读它会触发内核 TLB refill
        // panic（crash02 模糊 pread64(fd=0,…) 命中本路径即整核 halt）。
        let mut count = 0;
        for b in buf.iter_mut() {
            match crate::console::getc() {
                Some(byte) => {
                    *b = byte;
                    count += 1;
                    if byte == b'\n' { break; }
                }
                None => break,
            }
        }
        if count > 0 {
            Ok(count)
        } else {
            Err(FsError::WouldBlock)
        }
    }
    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat { file_type: FileType::CharDevice, size: 0, mode: FileMode(0o020444),
            nlink: 1, uid: 0, gid: 0, atime: 0, mtime: 0, ctime: 0 })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
}

impl Inode for Stdin {
    fn ino(&self) -> usize { 0x1000_0000 }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}

// -------- Stdout --------

pub struct Stdout;

unsafe impl Send for Stdout {}
unsafe impl Sync for Stdout {}

impl File for Stdout {
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }
    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        // 通过 UART MMIO 输出（与 crate::print! 一致）
        if let Ok(s) = core::str::from_utf8(buf) {
            crate::print!("{}", s);
        } else {
            for &b in buf {
                crate::print!("{}", b as char);
            }
        }
        Ok(buf.len())
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat { file_type: FileType::CharDevice, size: 0, mode: FileMode(0o020222),
            nlink: 1, uid: 0, gid: 0, atime: 0, mtime: 0, ctime: 0 })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
}

impl Inode for Stdout {
    fn ino(&self) -> usize { 0x1000_0001 }
    fn file_type(&self) -> FileType { FileType::CharDevice }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}
