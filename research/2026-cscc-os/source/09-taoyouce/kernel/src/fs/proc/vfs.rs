//! ProcFS VFS adapter — bridges ProcFs to the virtual filesystem layer.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::sync::SpinLock;
use super::super::{FileType, FileStat, FileMode, FsError, DirEntry};
use super::super::Inode as VfsInode;
use super::super::File as VfsFile;
use super::super::Directory as VfsDirectory;
use super::super::FileSystem as VfsFileSystem;
use crate::fs::procfs::{ProcFs, ProcFsAdapter, ProcInodeType};

/// ProcFS VFS Inode 适配器
pub struct ProcVfsInode {
    pub(crate) procfs: Arc<SpinLock<ProcFs>>,
    pub(crate) path: String,
    pub(crate) inode_type: ProcInodeType,
    /// Per-open seq_file 快照缓冲。procfs 文件内容由一次生成器遍历整体产生，
    /// Linux `seq_read` 把它缓冲在 `file->private_data` 里跨多次 `read()` 复用，
    /// 不会每个 read 系统调用重新生成。本字段等价实现：首个 read（或回绕到
    /// offset 0）时整体快照一次，后续顺序 read 直接从快照切片返回。避免对 ~MB
    /// 级 `/proc/[pid]/smaps`（2500+ VMA）在 musl ~1KB 分块读取下，把整文件生成
    /// 重复执行上千次而退化成 O(N²)。每次 `open()` 经 `lookup_walk` 新建独立
    /// inode（procfs 无 inode 缓存），故此缓冲天然 per-open。
    content_cache: SpinLock<Option<Vec<u8>>>,
}

impl ProcVfsInode {
    pub(crate) fn new(
        procfs: Arc<SpinLock<ProcFs>>,
        path: String,
        inode_type: ProcInodeType,
    ) -> Self {
        Self {
            procfs,
            path,
            inode_type,
            content_cache: SpinLock::new(None),
        }
    }
}

impl VfsInode for ProcVfsInode {
    fn ino(&self) -> usize {
        let mut hash: usize = 0;
        for byte in self.path.bytes() {
            hash = hash.wrapping_mul(31).wrapping_add(byte as usize);
        }
        hash
    }

    fn file_type(&self) -> FileType {
        match self.inode_type {
            ProcInodeType::Directory => FileType::Directory,
            ProcInodeType::File => FileType::Regular,
            ProcInodeType::Symlink => FileType::Symlink,
        }
    }

    fn as_file(&self) -> Option<&dyn VfsFile> {
        if self.inode_type == ProcInodeType::File {
            Some(self)
        } else {
            None
        }
    }

    fn as_dir(&self) -> Option<&dyn VfsDirectory> {
        if self.inode_type == ProcInodeType::Directory {
            Some(self)
        } else {
            None
        }
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        // Linux 语义：几乎所有 /proc 文件 st_size=0（seq_file 内容按 read 动态
        // 生成，i_size 不预计算）。旧实现为算 size 全量物化文件内容，stat 一个
        // /proc/[pid]/smaps 就要 O(VMA×页) 全扫（ls -l /proc/[pid] 一次触发
        // 全部文件生成），既慢又偏离 Linux（Linux 这里就是 0）。
        let size = 0;

        let writable_file = self.inode_type == ProcInodeType::File
            && self.procfs.lock().writable(&self.path);
        let mode = match self.inode_type {
            ProcInodeType::Directory => FileMode::default_dir(),
            ProcInodeType::File if writable_file => FileMode::new(0o100644),
            ProcInodeType::File => FileMode::new(0o100444),
            ProcInodeType::Symlink => FileMode::new(0o120777),
        };

        Ok(FileStat {
            file_type: self.file_type(),
            size,
            mode,
            nlink: if self.inode_type == ProcInodeType::Directory { 2 } else { 1 },
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
}

impl VfsFile for ProcVfsInode {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        // Fast path: a forward (non-rewind) read of an already-materialized
        // snapshot is served straight from the cached buffer, WITHOUT
        // re-resolving the procfs path. musl reads a multi-MB /proc/[pid]/maps
        // in 1KiB chunks, so a 65536-VMA maps file is ~2600 read() calls; doing
        // the global-lock + tree path-lookup (read_at) on each of them turned the
        // read into O(reads × lookup) ≈ O(N^1.5) and timed out max_map_count.
        // Only sparse offset-addressed files (pagemap) leave the cache empty and
        // need read_at; once a snapshot exists, every forward read is O(chunk).
        if offset != 0 {
            let cache = self.content_cache.lock();
            if let Some(bytes) = cache.as_ref() {
                if offset >= bytes.len() {
                    return Ok(0);
                }
                let len = buf.len().min(bytes.len() - offset);
                buf[..len].copy_from_slice(&bytes[offset..offset + len]);
                return Ok(len);
            }
        }
        // 偏移寻址文件（pagemap 等近乎无限稀疏、无法整体物化为 Vec）按需逐项
        // 计算，不进入快照缓冲。
        if let Some(n) = self.procfs.lock().read_at(&self.path, offset, buf) {
            return Ok(n);
        }
        // seq_file 语义：整文件由一次生成器遍历产生，跨多次 read() 复用同一份
        // 快照。回绕到 offset 0（seq_lseek rewind / 重新 pread 轮询）时重新快照，
        // 保证每次 open 的首个 read 观察到当前状态（如 /proc/self 解析到当前
        // 进程）。
        let mut cache = self.content_cache.lock();
        if offset == 0 || cache.is_none() {
            let fresh = self.procfs.lock().read_bytes(&self.path)
                .map_err(|_| FsError::NotFound)?;
            *cache = Some(fresh);
        }
        let bytes = match cache.as_ref() {
            Some(bytes) => bytes,
            None => return Ok(0),
        };
        if offset >= bytes.len() {
            return Ok(0);
        }
        let len = buf.len().min(bytes.len() - offset);
        buf[..len].copy_from_slice(&bytes[offset..offset + len]);
        Ok(len)
    }

    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        // 写入使本 fd 的读快照失效：下一次 read 会重新快照，观察到写后的内容
        // （seq_file 写后重读语义）。在锁 procfs 之前短暂取/放 content_cache 锁，
        // 与 read() 的 content_cache→procfs 取锁顺序不冲突。
        *self.content_cache.lock() = None;
        let procfs = self.procfs.lock();
        if !procfs.writable(&self.path) {
            return Err(FsError::PermissionDenied);
        }
        let text = core::str::from_utf8(buf).map_err(|_| FsError::InvalidArgument)?;
        match procfs.write(&self.path, text) {
            Ok(()) => Ok(buf.len()),
            Err(crate::fs::procfs::ProcError::ReadOnly)
            | Err(crate::fs::procfs::ProcError::PermissionDenied) => {
                Err(FsError::PermissionDenied)
            }
            Err(crate::fs::procfs::ProcError::OperationNotPermitted) => {
                Err(FsError::OperationNotPermitted)
            }
            Err(crate::fs::procfs::ProcError::InvalidArgument)
            | Err(crate::fs::procfs::ProcError::InvalidData) => {
                Err(FsError::InvalidArgument)
            }
            Err(crate::fs::procfs::ProcError::NotFound) => Err(FsError::NotFound),
            Err(crate::fs::procfs::ProcError::OutOfRange) => Err(FsError::OutOfRange),
        }
    }

    fn truncate(&self, length: usize) -> Result<(), FsError> {
        if length != 0 {
            return Err(FsError::InvalidArgument);
        }
        let procfs = self.procfs.lock();
        if procfs.writable(&self.path) {
            Ok(())
        } else {
            Err(FsError::PermissionDenied)
        }
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        VfsInode::stat(self)
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

impl VfsDirectory for ProcVfsInode {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let entries = self.procfs.lock().readdir(&self.path)
            .map_err(|_| FsError::NotFound)?;

        let is_fd_dir = self.path.ends_with("/fd") || self.path == "fd";
        Ok(entries.into_iter().enumerate().map(|(i, name)| {
            let ft = if is_fd_dir {
                FileType::Symlink
            } else if name == "self" || name == "task" || name.parse::<usize>().is_ok() {
                FileType::Directory
            } else {
                FileType::Regular
            };
            DirEntry {
                name: name.clone(),
                file_type: ft,
                inode: i + 1,
            }
        }).collect())
    }

    fn lookup(&self, name: &str) -> Result<Arc<dyn VfsInode>, FsError> {
        let child_path = if self.path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", self.path, name)
        };

        let procfs = self.procfs.lock();
        if let Some(inode) = procfs.lookup(&child_path) {
            Ok(Arc::new(ProcVfsInode::new(
                self.procfs.clone(),
                child_path,
                inode.inode_type,
            )))
        } else {
            if self.path.is_empty() && (name == "self" || name.parse::<usize>().is_ok()) {
                return Ok(Arc::new(ProcVfsInode::new(
                    self.procfs.clone(),
                    child_path,
                    ProcInodeType::Directory,
                )));
            }
            Err(FsError::NotFound)
        }
    }

    fn create(&self, _name: &str, _file_type: FileType) -> Result<Arc<dyn VfsInode>, FsError> {
        Err(FsError::PermissionDenied)
    }

    fn unlink(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }
}

impl VfsFileSystem for ProcFsAdapter {
    fn name(&self) -> &str {
        "procfs"
    }

    fn root(&self) -> Arc<dyn VfsInode> {
        Arc::new(ProcVfsInode::new(
            self.procfs.clone(),
            String::new(),
            ProcInodeType::Directory,
        ))
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

/// 挂载 ProcFS 到 VFS
pub fn mount_procfs() {
    let procfs_adapter = ProcFsAdapter::new();
    crate::fs::mount("/proc", procfs_adapter);
    crate::println!("[procfs] Mounted at /proc");
}
