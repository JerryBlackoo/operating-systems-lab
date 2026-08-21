//! `/proc/<pid>/mem` —— 进程地址空间的字节流视图。
//!
//! Linux 把它实现为一个普通文件：读/写偏移 = 目标进程的虚拟地址，read/write
//! 直接访问目标进程地址空间（受 ptrace/权限约束）。本内核当前仅支持目标进程
//! 与调用者**共享地址空间**（self / 同 tgid 线程）的情形——这覆盖绝大多数
//! 自访问型用例（thp04 经 /proc/self/mem 读写堆区触发 THP 拆分）。访问越界/
//! 未映射区域返回 EIO（对标 Linux access_remote_vm 失败语义）。
use crate::fs::{Directory, File, FileMode, FileStat, FileType, FsError, Inode};

pub struct ProcMemInode {
    /// 目标进程全局 pid（self 解析为调用者）。
    target_pid: usize,
}

impl ProcMemInode {
    pub fn new(target_pid: usize) -> Self {
        Self { target_pid }
    }

    /// 仅当目标进程与当前进程共享地址空间（self 或同线程组）时可直接经
    /// 当前页表访问；否则跨地址空间访问尚未实现。
    fn same_address_space(&self) -> bool {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() {
            Some(t) => t,
            None => return false,
        };
        let cur = match t.current {
            Some(c) => c,
            None => return false,
        };
        if cur == self.target_pid {
            return true;
        }
        let cur_tgid = t.get(cur).map(|p| if p.is_thread { p.tgid } else { cur });
        let tgt_tgid = t
            .get(self.target_pid)
            .map(|p| if p.is_thread { p.tgid } else { self.target_pid });
        match (cur_tgid, tgt_tgid) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }
}

impl File for ProcMemInode {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.same_address_space() {
            return Err(FsError::IoError);
        }
        match crate::syscall::user::read_user_buffer(offset as *const u8, buf.len()) {
            Ok(data) => {
                let n = data.len().min(buf.len());
                buf[..n].copy_from_slice(&data[..n]);
                Ok(n)
            }
            Err(_) => Err(FsError::IoError),
        }
    }

    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if !self.same_address_space() {
            return Err(FsError::IoError);
        }
        match crate::syscall::user::force_write_user_buffer(offset as *mut u8, buf) {
            Ok(()) => Ok(buf.len()),
            Err(_) => Err(FsError::IoError),
        }
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Inode::stat(self)
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

impl Inode for ProcMemInode {
    fn ino(&self) -> usize {
        0
    }
    fn file_type(&self) -> FileType {
        FileType::Regular
    }
    fn as_file(&self) -> Option<&dyn File> {
        Some(self)
    }
    fn as_dir(&self) -> Option<&dyn Directory> {
        None
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Regular,
            size: 0,
            mode: FileMode::new(0o100600),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}
