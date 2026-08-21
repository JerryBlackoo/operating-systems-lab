//! 虚拟文件系统 (VFS)
//!
//! 提供统一的文件系统接口

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use crate::sync::{SpinLock, TicketSpinLock};

/// 文件类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// 普通文件
    Regular,
    /// 目录
    Directory,
    /// 符号链接
    Symlink,
    /// 字符设备
    CharDevice,
    /// 块设备
    BlockDevice,
    /// 管道
    Pipe,
    /// 套接字
    Socket,
}

/// 文件权限
#[derive(Debug, Clone, Copy)]
pub struct FileMode(pub u32);

impl FileMode {
    pub const S_IRUSR: u32 = 0o400;
    pub const S_IWUSR: u32 = 0o200;
    pub const S_IXUSR: u32 = 0o100;
    pub const S_IRGRP: u32 = 0o040;
    pub const S_IWGRP: u32 = 0o020;
    pub const S_IXGRP: u32 = 0o010;
    pub const S_IROTH: u32 = 0o004;
    pub const S_IWOTH: u32 = 0o002;
    pub const S_IXOTH: u32 = 0o001;

    pub fn new(mode: u32) -> Self {
        Self(mode)
    }

    pub fn default_file() -> Self {
        Self(0o100644) // S_IFREG | 0644
    }

    pub fn default_dir() -> Self {
        Self(0o040755) // S_IFDIR | 0755
    }
}

/// 文件元数据
#[derive(Debug, Clone)]
pub struct FileStat {
    /// 文件类型
    pub file_type: FileType,
    /// 文件大小
    pub size: usize,
    /// 权限
    pub mode: FileMode,
    /// 硬链接数
    pub nlink: usize,
    /// 用户 ID
    pub uid: u32,
    /// 组 ID
    pub gid: u32,
    /// 访问时间
    pub atime: u64,
    /// 修改时间
    pub mtime: u64,
    /// 创建时间
    pub ctime: u64,
}

/// 目录项
#[derive(Debug, Clone)]
pub struct DirEntry {
    /// 文件名
    pub name: String,
    /// 文件类型
    pub file_type: FileType,
    /// inode 号
    pub inode: usize,
}

pub const READINESS_READ: u32 = 0x0001;
pub const READINESS_WRITE: u32 = 0x0004;
pub const READINESS_ERROR: u32 = 0x0008;
pub const READINESS_HUP: u32 = 0x0010;
/// Read half hung up by peer / local shutdown(SHUT_RD) — maps to EPOLLRDHUP.
pub const READINESS_RDHUP: u32 = 0x0020;

/// 文件操作接口
pub trait File: Send + Sync {
    /// 读取数据
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError>;

    /// 写入数据
    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError>;

    /// Direct read into user physical pages (zero-copy fast path).
    /// Returns Ok(n) if supported, Err(FsError::NotSupported) to fall back.
    fn read_to_user(&self, _offset: usize, _user_ptr: *mut u8, _len: usize) -> Result<usize, FsError> {
        Err(FsError::NotSupported)
    }

    fn truncate(&self, _length: usize) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    /// fallocate(2) preallocation: reserve backing storage for [offset,
    /// offset+len) so later writes into the range cannot fail with ENOSPC.
    /// Default no-op for filesystems without block accounting.
    fn allocate(&self, _offset: usize, _len: usize) -> Result<(), FsError> {
        Ok(())
    }

    /// 获取文件状态
    fn stat(&self) -> Result<FileStat, FsError>;

    /// 同步数据到存储
    fn sync(&self) -> Result<(), FsError>;

    /// 是否有数据可读（用于 select/poll），默认 true（普通文件总是可读）
    fn poll_read(&self) -> bool { true }

    /// 是否可写（用于 select/poll），默认 true
    fn poll_write(&self) -> bool { true }

    fn readiness(&self) -> u32 {
        let mut mask = 0;
        if self.poll_read() {
            mask |= READINESS_READ;
        }
        if self.poll_write() {
            mask |= READINESS_WRITE;
        }
        mask
    }

    fn register_waiter(&self, _events: u32, _pid: usize, _deadline: Option<u64>) {}

    /// Linux `poll_wait` / `ep_ptable_queue_proc`: attach a persistent poll
    /// hook to this file's waitqueue(s). Default no-op for non-pollable files.
    fn attach_poll_hook(&self, _hook: alloc::sync::Arc<dyn crate::sched::wait_queue::PollWakeHook>) {}

    /// Remove a previously attached poll hook (`epoll_ctl(DEL)` / release).
    fn detach_poll_hook(&self, _hook_id: usize) {}

    /// True for a timerfd. A blocking `read()` on a timerfd that has no other
    /// runnable task must keep spinning (the deadline is driven by wall time,
    /// advanced by `inline_wfi_tick`) instead of bailing out with 0 like an
    /// idle pipe — otherwise `timerfd04`'s lone child returns EOF before the
    /// timer ever expires.
    fn is_timerfd(&self) -> bool { false }

    fn close_notify_last(&self) {}

    fn set_async_owner(&self, _owner_type: i32, _owner_pid: i32, _signal: i32, _enabled: bool) {}

    fn tee_to(&self, _dst: &dyn File, _len: usize) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }

    fn splice_to(&self, _dst: &dyn File, _len: usize) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }

    fn pipe_buffer_id(&self) -> Option<usize> {
        None
    }

    fn pipe_capacity(&self) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }

    fn set_pipe_capacity(&self, _requested: usize) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }

    /// Bytes currently buffered and immediately readable from a pipe/FIFO,
    /// independent of which end this fd refers to. Linux `FIONREAD` on either
    /// pipe end reports the unread byte count (pipe12); the read/write ends
    /// both report `st_size == 0` so FIONREAD must query the ring directly.
    /// Default `None` (not a pipe).
    fn pipe_bytes_queued(&self) -> Option<usize> {
        None
    }

    /// Device-specific ioctl hook. Regular files leave this unimplemented and
    /// syscall/fd.rs falls back to generic Linux ioctls (tty, BLKGETSIZE, ...).
    /// Character/block devices override it to expose their real driver
    /// controls without syscall code having to inspect path strings.
    fn ioctl(&self, _request: usize, _arg: usize) -> Option<isize> {
        None
    }

    /// 写端直递登记（pipe/FIFO 专用，见 fs/pipe.rs try_pipe_write_handoff）：
    /// 环满且已有部分进度时，sys_write/sys_writev 把剩余数据（内核 staging
    /// 缓冲，所有权移交）与已写字节数登记；读者消费后替其续写完成（Linux
    /// pipe_write 阻塞语义）。默认 false = 不支持。
    fn register_write_handoff(&self, _data: alloc::vec::Vec<u8>, _a0_base: usize) -> bool {
        false
    }

    /// 写端直递重放入口消费：返回阻塞前已写入字节数并移除登记；None = 无登记。
    fn take_write_handoff(&self, _pid: usize) -> Option<usize> {
        None
    }
}

/// 目录操作接口
pub trait Directory: Send + Sync {
    /// 读取目录项
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError>;

    /// 查找文件
    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError>;

    /// 创建文件
    fn create(&self, name: &str, file_type: FileType) -> Result<Arc<dyn Inode>, FsError>;

    /// 删除文件
    fn unlink(&self, name: &str) -> Result<(), FsError>;

    /// unlink 热路径：单次目录项探测内完成「查 victim + iflags 拒删(EPERM) +
    /// 目录拒删(EISDIR) + 摘除」，返回被摘除的 inode 及其类型（供 notify 用）。
    /// 校验顺序对标 Linux may_delete：immutable/append 的 EPERM 先于 EISDIR。
    /// 默认实现退化为 lookup + unlink 两次探测；哈希表目录（ramfs）应
    /// override 为单探测。
    fn unlink_take(
        &self,
        name: &str,
        deny_dir: bool,
        deny_iflags: u32,
    ) -> Result<(Arc<dyn Inode>, FileType), FsError> {
        let victim = self.lookup(name)?;
        if deny_iflags != 0 && victim.get_inode_flags() & deny_iflags != 0 {
            return Err(FsError::PermissionDenied);
        }
        let ft = victim.file_type();
        if deny_dir && ft == FileType::Directory {
            return Err(FsError::IsDirectory);
        }
        self.unlink(name)?;
        Ok((victim, ft))
    }

    /// 同目录内重命名（POSIX rename within same directory）。
    ///
    /// 默认实现返回 PermissionDenied，只读/特殊文件系统（如 procfs/devfs）
    /// 走默认即可；可写文件系统（如 ramfs，及通过 overlay 挂在 ext4 之上）
    /// 应当 override 为真实的目录项替换语义（保留 inode，原子地移动 dirent）。
    ///
    /// 跨目录的 rename 由 syscall 层用 `rename_to` 协调（对源目录调用，
    /// 携带目标目录的 trait 引用）。
    fn rename(&self, _old_name: &str, _new_name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    /// 跨目录 rename：从 self 中名为 `old_name` 的子项移动到 `new_dir` 中
    /// 名为 `new_name` 的位置。默认返回 PermissionDenied；可写文件系统应
    /// override 实现真实的跨目录移动。当 self 与 new_dir 是同一目录时，
    /// 实现可以转发到 `Self::rename`。
    fn rename_to(
        &self,
        _old_name: &str,
        _new_dir: &dyn Directory,
        _new_name: &str,
    ) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    /// 返回该目录所属的 inode 号（用于 syscall 层识别"同 inode 不同路径"
    /// 的同目录场景，并为后续避免 ABBA 死锁的锁顺序提供依据）。默认 None
    /// 表示不可用，跨目录 rename 时按"非同目录"处理。
    fn dir_ino_opt(&self) -> Option<usize> {
        None
    }

    /// 在该目录下新建一个名为 `name` 的 dirent，指向已经存在的 `existing` inode。
    /// 这是硬链接 (`linkat(2)`) 的底层 VFS 原语。默认返回 `PermissionDenied`，
    /// 仅可写 + 支持 ref-counted dirent 的文件系统（当前为 ramfs）应当 override。
    ///
    /// 实现需保证：
    ///   - 若 `name` 已存在则返回 `AlreadyExists`；
    ///   - 把同一个 `Arc<dyn Inode>` 挂到目录的 children 表中（不复制内容、不分配新 ino）；
    ///   - 在 `existing.stat().nlink` 上 +1（实现者负责通过 `inc_nlink` 或类似机制）。
    fn link(&self, _name: &str, _existing: &Arc<dyn Inode>) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    /// 同目录原子交换两个 dirent (`renameat2 RENAME_EXCHANGE` 的底层原语)。
    ///
    /// 默认实现返回 `PermissionDenied`；可写文件系统应当 override 为：
    /// 在锁内同时校验两个 name 都存在，然后交换 children 表中两条 entry 指向的 inode。
    fn rename_exchange(&self, _name_a: &str, _name_b: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    /// 跨目录交换（`renameat2 RENAME_EXCHANGE` 的跨目录原语）。
    /// 默认 PermissionDenied；Linux 在跨 mount 时返回 EXDEV，syscall 层
    /// 把 PermissionDenied 转译为 -EXDEV。ramfs 已 override 真做（按 ino
    /// 升序双锁、单 Arc 互换 dirent，inode/nlink 不动）。
    fn rename_exchange_to(
        &self,
        _name_a: &str,
        _other: &dyn Directory,
        _name_b: &str,
    ) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    /// 在 `name` 位置安置一个 whiteout char-dev(0,0)，用于 `RENAME_WHITEOUT`
    /// 的后半步。前半步（搬走原 inode）由 syscall 层用普通 rename 完成。
    fn whiteout(&self, _name: &str) -> Result<(), FsError> {
        Err(FsError::PermissionDenied)
    }

    /// 安全 downcast：返回 `&dyn Any`，让 `rename_to / link / rename_exchange_to`
    /// 在同 fs 内识别对端目录的真身。默认 sentinel → downcast 失败 → 跨 fs
    /// 操作返 EXDEV。具体 fs（ramfs）应 override 返回 `self`。
    fn dir_as_any(&self) -> &dyn core::any::Any {
        static SENTINEL: () = ();
        &SENTINEL
    }
}

/// Inode 接口
pub trait Inode: Send + Sync {
    /// 获取 inode 号
    fn ino(&self) -> usize;

    /// 获取文件类型
    fn file_type(&self) -> FileType;

    /// 作为文件
    fn as_file(&self) -> Option<&dyn File>;

    /// Whether select/poll readiness for this inode is a constant IN|OUT.
    /// Regular files always are (Linux vfs_poll semantics); devices like
    /// /dev/zero and /dev/null override this. Lets the poll hot path skip
    /// the dyn-dispatch readiness chain entirely.
    fn poll_always_ready(&self) -> bool {
        self.file_type() == FileType::Regular
    }

    /// 作为目录
    fn as_dir(&self) -> Option<&dyn Directory>;

    /// 获取状态
    fn stat(&self) -> Result<FileStat, FsError>;

    fn is_immutable(&self) -> bool {
        self.get_inode_flags() & 0x10 != 0 // FS_IMMUTABLE_FL
    }

    /// Get Linux inode flags (FS_IOC_GETFLAGS). Default: 0.
    fn get_inode_flags(&self) -> u32 { 0 }
    /// Set Linux inode flags (FS_IOC_SETFLAGS). Default: ENOTTY.
    fn set_inode_flags(&self, _flags: u32) -> Result<(), FsError> { Err(FsError::InvalidArgument) }
    /// Whether this inode is an unnamed AF_UNIX socketpair endpoint. Such fds
    /// report `FileType::Socket` but are pipe-backed and not registered in the
    /// socket-file table, so `getpeername`/`getsockname` must treat them as
    /// connected unnamed AF_UNIX sockets (getpeername01). Default: false.
    fn is_unix_socketpair(&self) -> bool { false }

    /// Return inotify watch info for /proc/[pid]/fdinfo/[fd]. Default: None.
    fn inotify_fdinfo(&self) -> Option<Vec<(i32, u32)>> { None }

    /// Return fanotify mark info for /proc/[pid]/fdinfo/[fd]. Default: None.
    /// Each entry: (mask, ignored_mask, mflags).
    fn fanotify_fdinfo(&self) -> Option<Vec<(u64, u64, u32)>> { None }

    /// 设备节点的真实 rdev（major/minor）。普通文件系统 inode 返回 None；
    /// devfs 中的字符/块设备 override 后，stat(2).st_rdev 与 statx(2)
    /// 的 stx_rdev_major/minor 才能按 Linux 语义区分 /dev/vda、/dev/loop0
    /// 等不同设备。
    fn device_id(&self) -> Option<(u32, u32)> {
        None
    }

    /// 设置设备节点 rdev（mknod(2) 创建 S_IFCHR/S_IFBLK 后调用）。默认不支持
    /// （非设备 fs / 普通文件）静默返回 Ok 以保持兼容；ramfs override 真写入。
    fn set_device_id(&self, _major: u32, _minor: u32) -> Result<(), FsError> {
        Ok(())
    }

    /// 更新 atime/mtime（默认不支持）
    fn set_times(&self, _atime: Option<u64>, _mtime: Option<u64>) -> Result<(), FsError> {
        Ok(()) // 默认静默成功
    }

    /// 更新文件权限位（fchmod / fchmodat）。默认静默成功——只读 fs（procfs）
    /// 不会真实落盘，但 chmod(2) 在 Linux 上对只读 fs 也是返回 EROFS 或允许
    /// 静默通过取决于实现，这里采用"允许"以匹配 ramfs/tmpfs 行为。
    fn set_mode(&self, _mode: u32) -> Result<(), FsError> {
        Ok(())
    }

    /// Mark a successfully-removed directory as dead (Linux `S_DEAD`, set in
    /// `vfs_rmdir`). A subsequent `getdents` on an fd still open to the removed
    /// directory must fail with ENOENT (getdents02). Default no-op; ramfs sets
    /// the inode link count to 0. 
    fn mark_dead(&self) {}

    /// 更新 owner uid/gid（fchown / fchownat）。语义同 `set_mode`：默认静默成功，
    /// 真实可写 fs（ramfs/ext4）应当 override 写入 inode 元数据。
    fn set_owner(&self, _uid: Option<u32>, _gid: Option<u32>) -> Result<(), FsError> {
        Ok(())
    }

    /// Record the effective credentials of the task that opened this file
    /// description. Most inodes ignore it; cgroupfs uses it to enforce
    /// open-time-credential migration permission on `cgroup.procs`
    /// (Linux `of->file->f_cred`, CVE-2021-4197).
    fn note_open_cred(&self, _euid: u32, _egid: u32) {}

    /// 共享 seals（F_GET_SEALS/F_ADD_SEALS）。Linux 把 seal 状态挂在 inode 上，
    /// 因此 dup / mmap / open 出来的不同 fd 必须看到同一份。
    /// 普通 fs inode 返回 `None`，syscall 层回退到 fd 私有 seals；
    /// memfd 的 ramfs inode 应当 override 返回 `Some(&AtomicU32)`。
    fn shared_seals(&self) -> Option<&core::sync::atomic::AtomicU32> {
        None
    }

    /// MFD_HUGETLB memfd 的大页尺寸（字节）；0 = 非 hugetlb memfd。
    /// 非 0 时 write(2) 返 EINVAL（hugetlbfs 无 write_iter），mmap 按大页处理。
    fn memfd_hugetlb_size(&self) -> usize { 0 }

    /// Increment shared writable mmap count (Linux i_mmap_writable).
    fn inc_writable_maps(&self) {}
    /// Decrement shared writable mmap count.
    fn dec_writable_maps(&self) {}
    /// Get the count of active shared writable mappings.
    fn writable_maps_count(&self) -> usize { 0 }

    /// 下钻到 InotifyFile（Slice 1.9b 真 inotify 实现）。
    /// 仅 `crate::fs::inotify::InotifyFile` 应当 override 返回 Some(self)，
    /// 其它实现保持默认 None。这样 sys_inotify_add_watch / rm_watch 可以
    /// 安全地从 `Arc<dyn Inode>` 拿到具体实例，无需 unsafe downcast。
    fn as_inotify(&self) -> Option<&crate::fs::inotify::InotifyFile> {
        None
    }

    /// 下钻到 `EpollInstance`：仅 `crate::fs::epoll::EpollInstance` 应当
    /// override，其余保持默认 None。syscall 层用这个安全地从 `Arc<dyn Inode>`
    /// 取真身，不需要任何 unsafe downcast。
    fn as_epoll(&self) -> Option<&crate::fs::epoll::EpollInstance> {
        None
    }

    /// 同 `as_inotify`，用于 TimerFd 真实现下钻。
    fn as_timerfd(&self) -> Option<&crate::fs::timerfd::TimerFdFile> {
        None
    }

    /// 同 `as_inotify`，用于 SignalFd 真实现下钻。
    fn as_signalfd(&self) -> Option<&crate::fs::signalfd::SignalFdFile> {
        None
    }

    /// 同 `as_inotify`，用于 FanotifyFile 真实现下钻。
    fn as_fanotify(&self) -> Option<&crate::fs::fanotify::FanotifyFile> {
        None
    }

    /// 下钻到 POSIX mq inode：只有 POSIX mq inode
    /// override 返 Some(self)。让 `sys_mq_timedsend / timedreceive /
    /// notify / getsetattr` 等可以从主 fd_table 拿到的 `Arc<dyn Inode>`
    /// 安全取到真身（无 unsafe downcast），mq fd 自此走主 fd_table，与
    /// `close(2)` / `dup(2)` / `execve` / `exit` 路径自然统一。
    fn as_posix_mq(&self) -> Option<&crate::syscall::mqueue::PosixMqInode> {
        None
    }

    /// 读取符号链接 target（仅 FileType::Symlink 应当 override）。
    /// 默认返回 -EINVAL；普通文件 / 目录调 readlink 应得到该错误。
    fn read_link(&self) -> Result<alloc::string::String, FsError> {
        Err(FsError::InvalidArgument)
    }

    /// 安全 downcast：返回 `&dyn Any`，让具体 fs（如 ramfs）能识别自己的
    /// inode 类型。用于真硬链接（`Directory::link` 需要把 `Arc<dyn Inode>`
    /// 在同 fs 内复用）。默认返回一个 unit ZST 引用，保证非具体 fs 的
    /// inode 下 cast 失败而非 panic。具体 fs 应 override 返回 `self`。
    /// Recover the `Arc<SocketFile>` behind this inode, if it is a socket.
    /// Lets the syscall layer resolve fd→socket without a second registry
    /// lock + BTreeMap lookup (Linux keeps `struct socket *` right on the
    /// `struct file`). Non-socket inodes return `None`.
    fn as_socket(&self) -> Option<alloc::sync::Arc<crate::net::socket::SocketFile>> { None }

    fn as_any(&self) -> &dyn core::any::Any {
        // 静态 unit 引用——不可能匹配任何具体 fs 类型，所有 downcast 会返
        // None。Linux 跨 fs 硬链接也是 EXDEV，行为一致。
        static SENTINEL: () = ();
        &SENTINEL
    }
}

/// 文件系统接口
pub trait FileSystem: Send + Sync {
    /// 获取文件系统名称
    fn name(&self) -> &str;

    /// 获取根目录
    fn root(&self) -> Arc<dyn Inode>;

    /// 同步文件系统
    fn sync(&self) -> Result<(), FsError>;
}

/// 文件系统错误
#[derive(Debug, Clone, Copy)]
pub enum FsError {
    /// 文件不存在
    NotFound,
    /// 文件已存在
    AlreadyExists,
    /// 不是目录
    NotDirectory,
    /// 是目录
    IsDirectory,
    /// 权限不足
    PermissionDenied,
    /// 访问被拒绝（EACCES）。语义上区别于 `PermissionDenied`：在 write_io 等
    /// 路径上 `PermissionDenied` 映射为 EPERM，而本变体始终映射为 EACCES。
    AccessDenied,
    /// 操作不允许
    OperationNotPermitted,
    /// 无效参数
    InvalidArgument,
    /// I/O 错误
    IoError,
    /// 文件系统已满
    NoSpace,
    /// 目录非空
    NotEmpty,
    /// 操作会阻塞（管道无数据可读但写端未关闭）
    WouldBlock,
    /// 资源正忙
    Busy,
    /// 管道破裂（读端已关闭）
    BrokenPipe,
    /// 符号链接层数过多（-ELOOP）
    TooManyLinks,
    /// 单个路径分量超过 NAME_MAX(255)（-ENAMETOOLONG）
    NameTooLong,
    /// 内存不足（-ENOMEM），如 pipe per-user 页硬上限耗尽
    OutOfMemory,
    /// 结果超出范围（-ERANGE），如 timens_offsets 写入溢出 64 位 ns 计时器
    OutOfRange,
    /// 操作不支持 — used as sentinel for optional fast-path methods
    NotSupported,
}

/// 文件描述符
pub struct FileDescriptor {
    /// 文件
    pub file: Arc<dyn Inode>,
    /// 打开时对应的路径（目录 fd 的 openat 解析依赖它）
    pub path: Option<String>,
    /// 当前偏移
    pub offset: AtomicUsize,
    /// 目录读取缓存
    pub dir_entries: SpinLock<Option<Vec<DirEntry>>>,
    /// 打开标志
    pub flags: u32,
    pub status_flags: AtomicU32,
    pub f_owner_type: AtomicI32,
    pub f_owner_pid: AtomicI32,
    pub f_owner_signal: AtomicI32,
    pub f_async_enabled: AtomicBool,
    pub lease_type: AtomicI32,
    pub seals: AtomicU32,
    /// Number of installed fd-table entries that point at this open file
    /// description.
    ///
    /// This intentionally does **not** count transient `Arc<FileDescriptor>`
    /// clones held by syscall local variables.  Linux final-close semantics are
    /// driven by the last descriptor table entry disappearing; temporary kernel
    /// references merely keep the object memory alive while in-flight operations
    /// unwind.  Keeping this count separate from `Arc::strong_count()` is what
    /// prevents blocked read/splice waiters from missing pipe EOF/HUP when the
    /// last userspace-visible write descriptor is closed.
    fd_refcnt: AtomicUsize,
    /// Idempotent open-file-description final-close guard.
    ///
    /// The fd-table close path consumes `fd_refcnt`; the normal `Drop` path is
    /// only a safety net for objects that were never installed or for abnormal
    /// construction failures.  The guard makes both paths idempotent.
    close_notified: AtomicBool,
    /// Process-instance binding for pidfd-capable descriptors (anonymous
    /// pidfd from pidfd_open, or a `/proc/<pid>` directory fd). 0 means
    /// "not bound". When non-zero it pins the *instance* of the process that
    /// occupied the target PID at open time, so pidfd_send_signal can return
    /// ESRCH once that PID is reused by a different process instance
    /// (Linux `struct pid` semantics; LTP pidfd_send_signal03).
    pub pidfd_instance: AtomicU64,
    /// Slice 2.x T3-2 真化：per-fd terminal attributes.
    ///
    /// Generic Linux UAPI has two related layouts:
    /// * `TCGETS/TCSETS`:  `struct termios`,  36 bytes (NCCS=19, no speeds)
    /// * `TCGETS2/TCSETS2`: `struct termios2`, 44 bytes (adds ispeed/ospeed)
    ///
    /// Store the superset (`termios2`) internally.  Old TCGETS only copies the
    /// first 36 bytes, exactly like Linux tty ioctl does, so libc stack objects
    /// are never over-written.
    pub termios: SpinLock<Option<[u8; 44]>>,
    /// Linux `FMODE_NONOTIFY`: descriptors the kernel opens to hand an event
    /// object to a fanotify listener must not themselves generate fsnotify
    /// events. When set, the final-close path skips inotify/fanotify CLOSE
    /// notification (fanotify09: closing a delivered event fd must not emit a
    /// spurious FAN_CLOSE_NOWRITE that other mount/fs marks would observe).
    pub nonotify: AtomicBool,
    /// Current path of the open file description after a rename moved it (the
    /// dentry-move analog: Linux close events report the file's *current*
    /// name, not the name it was opened with). None = `path` is still current.
    pub moved_path: SpinLock<Option<String>>,
    /// Regular files are always ready for I/O (Linux select/poll semantics:
    /// vfs_poll on a regular file reports IN|OUT unconditionally). Computed
    /// once at open so the select/poll hot loop can skip the dyn-dispatch
    /// readiness chain (as_file → readiness → poll_read/poll_write) per fd.
    pub always_ready: bool,
    /// Inode file type, cached at open (Linux caches the equivalent via
    /// `f_inode->i_mode`). An inode's type never changes, so read/write hot
    /// paths can branch on this field instead of a dyn `file_type()` call.
    pub ftype: FileType,
}

/// Rename moved `old_path` to `new_path`: retarget every open file
/// description whose current path is `old_path` or lies underneath it (the
/// Linux dentry-move analog), so later fsnotify events (close) report the
/// object's current name.
pub fn relocate_open_fd_paths(old_path: &str, new_path: &str) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(t) = table.as_ref() else { return };
    for proc in t.iter() {
        for slot in proc.fd_table.iter() {
            let Some(fd_obj) = slot.as_ref() else { continue };
            let mut moved = fd_obj.moved_path.lock();
            let cur: String = match moved.as_deref().or(fd_obj.path.as_deref()) {
                Some(c) => String::from(c),
                None => continue,
            };
            if cur == old_path {
                *moved = Some(String::from(new_path));
            } else if cur.len() > old_path.len()
                && cur.starts_with(old_path)
                && cur.as_bytes().get(old_path.len()) == Some(&b'/')
            {
                let mut np = String::from(new_path);
                np.push_str(&cur[old_path.len()..]);
                *moved = Some(np);
            }
        }
    }
}

impl FileDescriptor {
    pub fn new(file: Arc<dyn Inode>, flags: u32) -> Self {
        Self::new_with_path(file, flags, None)
    }

    pub fn new_with_path(file: Arc<dyn Inode>, flags: u32, path: Option<String>) -> Self {
        let always_ready = file.poll_always_ready();
        let ftype = file.file_type();
        Self {
            file,
            path,
            ftype,
            offset: AtomicUsize::new(0),
            dir_entries: SpinLock::new(None),
            flags,
            status_flags: AtomicU32::new(flags),
            f_owner_type: AtomicI32::new(1),
            f_owner_pid: AtomicI32::new(0),
            f_owner_signal: AtomicI32::new(0),
            f_async_enabled: AtomicBool::new(false),
            lease_type: AtomicI32::new(2),
            seals: AtomicU32::new(0),
            fd_refcnt: AtomicUsize::new(0),
            close_notified: AtomicBool::new(false),
            pidfd_instance: AtomicU64::new(0),
            termios: SpinLock::new(None),
            nonotify: AtomicBool::new(false),
            moved_path: SpinLock::new(None),
            always_ready,
        }
    }

    /// 创建一个用于测试的空文件描述符
    #[allow(dead_code)]
    pub fn dummy() -> Self {
        struct NullInode;
        impl Inode for NullInode {
            fn ino(&self) -> usize { 0 }
            fn file_type(&self) -> FileType { FileType::Regular }
            fn as_file(&self) -> Option<&dyn File> { None }
            fn as_dir(&self) -> Option<&dyn Directory> { None }
            fn stat(&self) -> Result<FileStat, FsError> { Err(FsError::IoError) }
        }
        // SAFETY: NullInode is Send + Sync (has no fields)
        unsafe impl Send for NullInode {}
        unsafe impl Sync for NullInode {}
        Self {
            file: Arc::new(NullInode),
            path: None,
            offset: AtomicUsize::new(0),
            dir_entries: SpinLock::new(None),
            flags: 0,
            status_flags: AtomicU32::new(0),
            f_owner_type: AtomicI32::new(1),
            f_owner_pid: AtomicI32::new(0),
            f_owner_signal: AtomicI32::new(0),
            f_async_enabled: AtomicBool::new(false),
            lease_type: AtomicI32::new(2),
            seals: AtomicU32::new(0),
            fd_refcnt: AtomicUsize::new(0),
            close_notified: AtomicBool::new(false),
            pidfd_instance: AtomicU64::new(0),
            termios: SpinLock::new(None),
            nonotify: AtomicBool::new(false),
            moved_path: SpinLock::new(None),
            always_ready: false,
            ftype: FileType::Regular,
        }
    }

    /// Mark that one fd-table slot now refers to this open file description.
    ///
    /// Must be called exactly once for every successful installation into a
    /// process fd table (open/pipe/socket/dup/fork inheritance) while the
    /// fd-table mutation is protected by `PROCESS_TABLE`.
    pub fn inc_fd_ref(&self) {
        self.fd_refcnt.fetch_add(1, Ordering::AcqRel);
    }

    /// Drop one fd-table reference and run final-close cleanup when it was the
    /// last userspace-visible descriptor.
    ///
    /// Returns true only for the transition 1 → 0.  Transient syscall `Arc`
    /// clones do not participate in this count, so final-close notification is
    /// not delayed by blocked/non-returning kernel stack frames.
    pub fn dec_fd_ref_and_notify_if_last(&self) -> bool {
        loop {
            let old = self.fd_refcnt.load(Ordering::Acquire);
            if old == 0 {
                debug_assert!(false, "FileDescriptor fd_refcnt underflow");
                return false;
            }
            if self
                .fd_refcnt
                .compare_exchange(old, old - 1, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                if old == 1 {
                    self.notify_last_close_once();
                    return true;
                }
                return false;
            }
        }
    }

    pub fn readable(&self) -> bool {
        const O_ACCMODE: u32 = 0o3;
        const O_WRONLY: u32 = 0o1;
        const O_PATH: u32 = 0o10000000;
        self.flags & O_PATH == 0 && (self.flags & O_ACCMODE) != O_WRONLY
    }

    pub fn writable(&self) -> bool {
        const O_ACCMODE: u32 = 0o3;
        const O_RDONLY: u32 = 0o0;
        const O_PATH: u32 = 0o10000000;
        self.flags & O_PATH == 0 && (self.flags & O_ACCMODE) != O_RDONLY
    }

    /// Run final open-file-description cleanup exactly once.
    ///
    /// The primary trigger is `fd_refcnt` reaching zero.  The `Drop` fallback
    /// below consumes the same idempotent guard so OFD/flock/lease cleanup,
    /// close notifications, pipe EOF/HUP and waiter wakeups are never lost for
    /// never-installed descriptors or abnormal construction paths.
    pub fn notify_last_close_once(&self) {
        if !self.close_notified.swap(true, Ordering::AcqRel) {
            // Last open-file-description cleanup.  The raw FileDescriptor
            // address is the OFD owner id used by file_lock/file_lease users.
            let ofd_id = self as *const FileDescriptor as usize;
            crate::fs::file_lock::release_all_locks_for_ofd(ofd_id);
            crate::fs::file_lock::release_all_flock_for_fd_obj(ofd_id);
            crate::fs::file_lease::release_all_leases_for_ofd(ofd_id);
            crate::fs::dnotify::unregister_ofd(ofd_id);

            let moved = self.moved_path.lock().clone();
            let cur_path = moved.as_ref().or(self.path.as_ref());
            // O_PATH 描述符（FMODE_PATH）关闭不产生 fsnotify close 事件（Linux
            // fput 对 FMODE_PATH 文件跳过 fsnotify_close）。
            const O_PATH: u32 = 0o10000000;
            if let (Some(path), false, true) = (
                cur_path,
                self.nonotify.load(Ordering::Relaxed),
                self.flags & O_PATH == 0,
            ) {
                const O_ACCMODE: u32 = 0o3;
                let dir_bit = if matches!(self.file.file_type(), crate::fs::FileType::Directory) {
                    crate::fs::inotify::IN_ISDIR
                } else {
                    0
                };
                let close_mask = if self.flags & O_ACCMODE != 0 {
                    crate::fs::inotify::IN_CLOSE_WRITE
                } else {
                    crate::fs::inotify::IN_CLOSE_NOWRITE
                };
                crate::fs::inotify::notify_file_path(path, close_mask | dir_bit);
                let fan_close_mask = if self.flags & O_ACCMODE != 0 {
                    crate::fs::fanotify::FAN_CLOSE_WRITE
                } else {
                    crate::fs::fanotify::FAN_CLOSE_NOWRITE
                };
                crate::fs::fanotify::notify_file_path(path, fan_close_mask);
            }

            if let Some(file) = self.file.as_file() {
                file.close_notify_last();
            }
        }
    }

    #[allow(dead_code)]
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, FsError> {
        if !self.readable() {
            return Err(FsError::PermissionDenied);
        }
        if let Some(file) = self.file.as_file() {
            let offset = self.offset.load(Ordering::Relaxed);
            let n = file.read(offset, buf)?;
            self.offset.fetch_add(n, Ordering::Relaxed);
            Ok(n)
        } else {
            Err(FsError::IsDirectory)
        }
    }

    #[allow(dead_code)]
    pub fn write(&self, buf: &[u8]) -> Result<usize, FsError> {
        if !self.writable() {
            return Err(FsError::PermissionDenied);
        }
        if let Some(file) = self.file.as_file() {
            // POSIX O_APPEND: every write must atomically seek to end-of-file
            // before performing the write. Linux: open(2) "if O_APPEND is set,
            // the file offset is set to the end of the file prior to each write(2)."
            const O_APPEND: u32 = 0x400;
            let offset = if self.status_flags.load(Ordering::Relaxed) & O_APPEND != 0 {
                let end = self.file.stat().map(|s| s.size).unwrap_or_else(|_| {
                    self.offset.load(Ordering::Relaxed)
                });
                self.offset.store(end, Ordering::Relaxed);
                end
            } else {
                self.offset.load(Ordering::Relaxed)
            };
            let n = file.write(offset, buf)?;
            self.offset.fetch_add(n, Ordering::Relaxed);
            Ok(n)
        } else {
            Err(FsError::IsDirectory)
        }
    }
}

impl Drop for FileDescriptor {
    fn drop(&mut self) {
        self.notify_last_close_once();
    }
}

/// 文件描述符表
#[cfg(test)]
pub struct FdTable {
    /// 文件描述符映射
    fds: BTreeMap<usize, Arc<FileDescriptor>>,
    /// 下一个可用 fd
    next_fd: usize,
}

#[cfg(test)]
impl FdTable {
    pub fn new() -> Self {
        Self {
            fds: BTreeMap::new(),
            next_fd: 3, // 0, 1, 2 保留给 stdin, stdout, stderr
        }
    }

    /// 分配文件描述符
    pub fn alloc(&mut self, fd: Arc<FileDescriptor>) -> usize {
        let n = self.next_fd;
        self.next_fd += 1;
        self.fds.insert(n, fd);
        n
    }

    /// 获取文件描述符
    pub fn get(&self, fd: usize) -> Option<Arc<FileDescriptor>> {
        self.fds.get(&fd).cloned()
    }

    /// 关闭文件描述符
    pub fn close(&mut self, fd: usize) -> bool {
        self.fds.remove(&fd).is_some()
    }
}

/// 全局 VFS
static VFS: TicketSpinLock<Option<Vfs>> = TicketSpinLock::new(None);
/// dentry 缓存用哈希表（FNV-1a）：String-BTreeMap 的 O(log n) 全路径字符串比较
/// 在高频 create/unlink 下是热点（memcmp 主调用者=BTree 删除再平衡）；
/// 哈希表 O(1) 且无节点 merge。remove_tree 仅 rename/rmdir 低频路径走 retain。
pub(crate) type PathMap<V> = hashbrown::HashMap<String, V, core::hash::BuildHasherDefault<FnvHasher>>;

/// 目录项短名映射：键用 SSO 内联短名（对标 Linux dentry 的 d_iname 内联
/// 存储）。目录项名几乎总 ≤23B，内联后 create/unlink 每 op 免一次键
/// String 堆分配+释放（SLUB lock/unlock 对 = TCG 串行化点）。
pub(crate) type NameMap<V> =
    hashbrown::HashMap<SsoKey, V, core::hash::BuildHasherDefault<FnvHasher>>;

/// 小字符串优化键：≤23 字节内联存储，超长退堆 String。
/// Hash/Eq/Borrow 与 str 完全一致，可直接以 &str 查询/删除。
pub(crate) enum SsoKey {
    Inline { len: u8, buf: [u8; 23] },
    Heap(String),
}

impl SsoKey {
    #[inline]
    pub(crate) fn as_str(&self) -> &str {
        match self {
            // 构造时来自合法 &str 整体拷贝，UTF-8 有效性保持。
            SsoKey::Inline { len, buf } => unsafe {
                core::str::from_utf8_unchecked(&buf[..*len as usize])
            },
            SsoKey::Heap(s) => s.as_str(),
        }
    }
}

impl From<&str> for SsoKey {
    #[inline]
    fn from(s: &str) -> Self {
        if s.len() <= 23 {
            let mut buf = [0u8; 23];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            SsoKey::Inline { len: s.len() as u8, buf }
        } else {
            SsoKey::Heap(String::from(s))
        }
    }
}

impl core::borrow::Borrow<str> for SsoKey {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for SsoKey {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}
impl Eq for SsoKey {}

impl core::hash::Hash for SsoKey {
    #[inline]
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state)
    }
}

/// FxHash（rustc-hash 算法）：8 字节/迭代 word-at-a-time，替代原 FNV-1a
/// 逐字节（~5 insn/字节）。路径 key 平均 ~17B：FNV ~90 insn → Fx ~25 insn。
/// dentry 缓存 + ramfs children + xattr 等全部 PathMap 共同受益。
#[derive(Default)]
pub(crate) struct FnvHasher(u64);

impl FnvHasher {
    #[inline]
    fn add_word(&mut self, w: u64) {
        const K: u64 = 0x517cc1b727220a95;
        self.0 = (self.0.rotate_left(5) ^ w).wrapping_mul(K);
    }
}

impl core::hash::Hasher for FnvHasher {
    fn finish(&self) -> u64 {
        // murmur3 式终混：rotl^mul 链的低位只依赖输入低位（w*K mod 2^k 只由
        // w mod 2^k 决定），而 hashbrown 用 hash 低位做桶下标——lat_fs 的
        // "lfsNNN" 同长键低位全同 → 900 键同桶、探测链数百组。终混让高位
        // 雪崩进低位，恢复 O(1) 探测。
        let mut h = self.0;
        h ^= h >> 32;
        h = h.wrapping_mul(0xd6e8feb86659fd93);
        h ^ (h >> 32)
    }
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add_word(u64::from_le_bytes(c.try_into().unwrap()));
        }
        let rem = chunks.remainder();
        if !rem.is_empty() {
            // 尾块（1..7B）就地按字节拼 word：避免栈临时缓冲的
            // copy_from_slice 退化成变长 memcpy 调用（每次 hash 都付）。
            let mut w = 0u64;
            for (j, &b) in rem.iter().enumerate() {
                w |= (b as u64) << (8 * j);
            }
            // 尾块混入长度，防止 "a\0" 与 "a" 同 hash
            self.add_word(w ^ (rem.len() as u64));
        }
    }
}

// Dentry cache entry: `followed` is the inode with a terminal symlink
// followed; `nofollow` is the inode WITHOUT following a terminal symlink.
// The two disagree only for symlink terminals. Keeping both in one map means
// every get/insert/remove pays a single path hash + lock instead of two
// (unlink/create of small files hits this on the hot path).
#[derive(Default)]
struct DentryEntry {
    followed: Option<Arc<dyn Inode>>,
    /// `followed` 是否 symlink（插入时预算，get 热路径免每次虚调用 file_type）。
    followed_is_symlink: bool,
    nofollow: Option<Arc<dyn Inode>>,
    /// 以本 key 为目录前缀时的规范（symlink 全展开）形式，供
    /// resolve_symlinks_for_path 免逐分量走查；失效与整条 entry 同命。
    canonical_dir: Option<String>,
    /// 负向 dentry：VFS lookup 返回 NotFound 后记录。cargo/rustc 依赖探测
    /// 会对同一批不存在路径反复 statx（BuildStorm RV 实测数分钟内数千次
    /// 重复 ENOENT）；create/unlink/rename 走 insert/remove/remove_tree 替换
    /// 或清除本条目，与 Linux 负向 dentry 失效模型一致。
    negative: bool,
}

/// dentry 分片数（2 的幂）。路径缓存 hash 到 shard，避免所有 CPU 的
/// lookup/create/unlink 在同一把 dentry 锁上排队。16 分片在 RV=8/LA=12
/// 下保证常见并发度内无分片级争用。
const DENTRY_SHARDS: usize = 16;

struct DentryShard {
    map: PathMap<DentryEntry>,
    negative_count: usize,
}

impl DentryShard {
    const fn new() -> Self {
        Self {
            map: PathMap::with_hasher(core::hash::BuildHasherDefault::new()),
            negative_count: 0,
        }
    }

    fn recount_negatives(&mut self) {
        self.negative_count = self.map.values().filter(|e| e.negative).count();
    }
}

static DENTRY_CACHE: [TicketSpinLock<DentryShard>; DENTRY_SHARDS] =
    [const { TicketSpinLock::new(DentryShard::new()) }; DENTRY_SHARDS];

/// 最近命中的父目录全局单槽 MRU。dentry 树式解析把父路径查询独立于
/// 全路径 shard；MRU 保持全局单槽，dentry_parent_mru_match 才能对任意
/// 子路径做一次前缀比较，不必知道父路径属于哪个 shard。
static DENTRY_PARENT_MRU: TicketSpinLock<Option<(String, Arc<dyn Inode>)>> =
    TicketSpinLock::new(None);

#[inline]
fn dentry_shard(path: &str) -> usize {
    let mut h = FnvHasher::default();
    core::hash::Hasher::write(&mut h, path.as_bytes());
    (core::hash::Hasher::finish(&h) as usize) & (DENTRY_SHARDS - 1)
}

#[inline]
fn dentry_mru_invalidate(path: &str) {
    let mut mru = DENTRY_PARENT_MRU.lock();
    if let Some((k, _)) = mru.as_ref() {
        if k == path {
            *mru = None;
        }
    }
}

/// 负向 dentry 上限：命中集中、生命周期由文件系统变更失效；到上限后
/// 直接放弃新负向项（只损失缓存，不影响正确性）。
const NEGATIVE_DENTRY_LIMIT: usize = 2048;
/// 分片后每 shard 的负向 dentry 上限（总上限保持 2048，语义不变）。
const NEGATIVE_DENTRY_LIMIT_PER_SHARD: usize = NEGATIVE_DENTRY_LIMIT / DENTRY_SHARDS;

fn dentry_cache_clear_all() {
    for shard in &DENTRY_CACHE {
        let mut c = shard.lock();
        c.map.clear();
        c.negative_count = 0;
    }
    *DENTRY_PARENT_MRU.lock() = None;
}

// Mount tracking and flags — moved to fs/mount.rs
pub use self::mount::{
    mark_mount_expired, clear_mount_expired, remove_expired_mark,
    deny_write_access, allow_write_access, is_write_denied,
    get_write_access, put_write_access, has_write_access,
    has_open_writers_under,
    MountInfo, MountPropagation, MOUNT_INFO_TABLE,
    mount_info_init, mount_info_add, mount_info_add_ex, mount_info_remove, mount_info_move,
    mount_move_check,
    generate_proc_mounts, generate_proc_mountinfo,
    set_mount_propagation,
    set_readonly_mount, is_readonly_path,
    set_noexec_mount, is_noexec_path,
    set_nosuid_mount, is_nosuid_path,
    set_nodev_mount, is_nodev_path,
    set_noatime_mount, is_noatime_path,
    set_nodiratime_mount, is_nodiratime_path,
    set_nosymfollow_mount, is_nosymfollow_path,
    touch_atime, ramfs_current_time_sec,
};


fn dentry_cache_enabled(path: &str) -> bool {
    // The dentry cache is keyed by path only, so it cannot represent a path
    // that resolves to different inodes in different mount namespaces. Bypass
    // it entirely outside the init namespace (the cache is cleared on every
    // mount/unmount regardless, so correctness in init is unaffected).
    if crate::fs::mount::current_mnt_ns() != crate::fs::mount::INIT_MNT_NS {
        return false;
    }
    !(path == "/proc" || path.starts_with("/proc/") || path == "/dev" || path.starts_with("/dev/"))
}

fn dentry_cache_get(path: &str) -> Option<Arc<dyn Inode>> {
    if !dentry_cache_enabled(path) {
        return None;
    }
    DENTRY_CACHE[dentry_shard(path)]
        .lock()
        .map
        .get(path)
        .and_then(|e| {
            if e.followed_is_symlink {
                None
            } else {
                e.followed.clone()
            }
        })
}

/// 带负向 dentry 判定的缓存查询结果。
enum DentryLookup {
    Hit(Arc<dyn Inode>),
    Negative,
    Miss,
}

/// lookup()（follow 终端 symlink）用的负向感知查询。
fn dentry_cache_lookup_followed(path: &str) -> DentryLookup {
    if !dentry_cache_enabled(path) {
        return DentryLookup::Miss;
    }
    let cache = DENTRY_CACHE[dentry_shard(path)].lock();
    match cache.map.get(path) {
        Some(e) if e.negative => DentryLookup::Negative,
        Some(e) if !e.followed_is_symlink => match e.followed.clone() {
            Some(inode) => DentryLookup::Hit(inode),
            None => DentryLookup::Miss,
        },
        _ => DentryLookup::Miss,
    }
}

/// lookup_no_follow() 用的负向感知查询。
fn dentry_cache_lookup_nofollow(path: &str) -> DentryLookup {
    if !dentry_cache_enabled(path) {
        return DentryLookup::Miss;
    }
    let cache = DENTRY_CACHE[dentry_shard(path)].lock();
    match cache.map.get(path) {
        Some(e) if e.negative => DentryLookup::Negative,
        Some(e) => match e.nofollow.clone() {
            Some(inode) => DentryLookup::Hit(inode),
            None => DentryLookup::Miss,
        },
        None => DentryLookup::Miss,
    }
}

/// 记录 NotFound（负向 dentry）。路径上后续 create/unlink/rename 会替换或
/// 清除该条目；到上限后放弃新记录，只损失缓存。
fn dentry_cache_insert_negative(path: &str) {
    if !dentry_cache_enabled(path) {
        return;
    }
    dentry_mru_invalidate(path);
    let mut cache = DENTRY_CACHE[dentry_shard(path)].lock();
    if let Some(e) = cache.map.get_mut(path) {
        if e.negative {
            return;
        }
        e.followed = None;
        e.followed_is_symlink = false;
        e.nofollow = None;
        e.canonical_dir = None;
        e.negative = true;
        cache.negative_count += 1;
        return;
    }
    if cache.negative_count >= NEGATIVE_DENTRY_LIMIT_PER_SHARD {
        return;
    }
    cache.map.insert(
        String::from(path),
        DentryEntry {
            negative: true,
            ..DentryEntry::default()
        },
    );
    cache.negative_count += 1;
}

pub fn dentry_cache_insert(path: &str, inode: Arc<dyn Inode>) {
    if dentry_cache_enabled(path) {
        dentry_mru_invalidate(path);
        let mut cache = DENTRY_CACHE[dentry_shard(path)].lock();
        // dentry 树式缓存策略：只缓存目录与 symlink。普通文件/设备/
        // FIFO/socket 末级经父目录缓存 + 单级名字哈希解析（见
        // lookup 快路径），不入全局表——unlink/create 零全路径键
        // 插入/删除/String 分配，且表规模限于目录数。
        // 但成功 lookup 必须先清掉可能存在的负向条目，否则新建普通文件后
        // 仍会命中旧 ENOENT。
        let ft = inode.file_type();
        if ft != FileType::Directory && ft != FileType::Symlink {
            if let Some(e) = cache.map.get_mut(path) {
                if e.negative {
                    e.negative = false;
                    cache.negative_count = cache.negative_count.saturating_sub(1);
                }
            }
            return;
        }
        let is_symlink = ft == FileType::Symlink;
        let had_negative = cache.map.get(path).map_or(false, |e| e.negative);
        if had_negative {
            cache.negative_count = cache.negative_count.saturating_sub(1);
        }
        let e = cache.map.entry_ref(path).or_default();
        if had_negative {
            e.negative = false;
        }
        e.followed_is_symlink = is_symlink;
        e.followed = Some(inode);
    }
}

pub fn dentry_cache_remove(path: &str) {
    if dentry_cache_enabled(path) {
        dentry_mru_invalidate(path);
        let mut c = DENTRY_CACHE[dentry_shard(path)].lock();
        if let Some(entry) = c.map.remove(path) {
            if entry.negative {
                c.negative_count = c.negative_count.saturating_sub(1);
            }
        }
    }
}

/// 删除缓存条目并返回末级不跟随 symlink 的 inode（若有）。unlink 热路径用：
/// 一次哈希操作同时完成“失效缓存 + 取 victim”，命中时省掉父目录的
/// dirent 查找。followed 字段对 symlink 终端存的是 target，不能当 victim，
/// 故仅返回 nofollow。
pub(crate) fn dentry_cache_remove_take(path: &str) -> Option<Arc<dyn Inode>> {
    if !dentry_cache_enabled(path) {
        return None;
    }
    dentry_mru_invalidate(path);
    let mut c = DENTRY_CACHE[dentry_shard(path)].lock();
    c.map.remove(path).and_then(|e| {
        if e.negative {
            c.negative_count = c.negative_count.saturating_sub(1);
        }
        e.nofollow
    })
}

/// 取父目录缓存（MRU 单槽优先，未命中再查哈希表并回填 MRU）。
/// dentry 树式解析的基础操作：末级非目录条目不入全局表，
/// 由「父目录 + 单级名字」解析，unlink/lookup 热路径共用。
pub(crate) fn dentry_cache_parent_get(parent_path: &str) -> Option<Arc<dyn Inode>> {
    if !dentry_cache_enabled(parent_path) {
        return None;
    }
    {
        let mru = DENTRY_PARENT_MRU.lock();
        if let Some((k, inode)) = mru.as_ref() {
            if k == parent_path {
                return Some(inode.clone());
            }
        }
    }
    let mut cache = DENTRY_CACHE[dentry_shard(parent_path)].lock();
    let p = cache.map.get(parent_path).and_then(|e| {
        if e.followed_is_symlink {
            None
        } else {
            e.followed.clone()
        }
    });
    if let Some(ref inode) = p {
        *DENTRY_PARENT_MRU.lock() = Some((String::from(parent_path), inode.clone()));
    }
    p
}

/// 父目录 MRU 前缀直配：整条路径与 MRU 父键做一次前缀比较，末分量
/// 就地校验（非空、无 '/'、非 "."/".."、≤NAME_MAX），命中即同时免去
/// 路径规范性扫描、rfind 父子切分与父目录哈希查找——同目录连续
/// unlink/lookup（对标 Linux dcache 的 parent+name 解析）的单比较快路径。
/// MRU 失效规则与 `dentry_cache_parent_get` 完全共用（dentry_mru_invalidate/clear）。
/// 仅对 plain-root 的绝对路径调用方有效（调用方负责该前提）。
pub(crate) fn dentry_parent_mru_match(path: &str) -> Option<(usize, Arc<dyn Inode>)> {
    const NAME_MAX: usize = 255;
    let mru = DENTRY_PARENT_MRU.lock();
    let (k, inode) = mru.as_ref()?;
    let kb = k.as_bytes();
    let pb = path.as_bytes();
    if kb.len() < 2 || pb.len() <= kb.len() + 1 || &pb[..kb.len()] != kb || pb[kb.len()] != b'/' {
        return None;
    }
    let name = &pb[kb.len() + 1..];
    if name.len() > NAME_MAX
        || name == b"."
        || name == b".."
        || name.iter().any(|&b| b == b'/')
    {
        return None;
    }
    Some((kb.len(), inode.clone()))
}

/// 延迟 inode 析构队列（对标 Linux 的 deferred iput/RCU 延迟释放）：
/// unlink 摘除后的 victim inode 析构（页帧归还 buddy、PageStore/字符串
/// 释放）不在 unlink 系统调用临界路径上做，推入队列由退出/空闲点批量
/// 回收。语义不变：Arc 只延迟 drop，打开中的 fd 引用计数照常。
static INODE_REAP: TicketSpinLock<Vec<Arc<dyn Inode>>> = TicketSpinLock::new(Vec::new());

/// 队列上限：超限即就地回收（逐个），限制延迟占用的内存上界。
/// lmbench lat_fs 动态选 n（实测可到 ~8000/轮），上限须覆盖单轮
/// 全部 victim，否则超出部分的析构落回 unlink 临界路径。
const INODE_REAP_LIMIT: usize = 16384;

pub fn inode_reap_defer(inode: Arc<dyn Inode>) {
    let mut q = INODE_REAP.lock();
    if q.len() >= INODE_REAP_LIMIT {
        // 队列满：仅就地回收本个 victim（一次析构/次），不整批排空——
        // 整批 1024 个析构落在单次 unlink 上是延迟尖刺。
        drop(q);
        drop(inode);
        return;
    }
    q.push(inode);
}

/// 增量排空：最多回收 n 个延迟 victim。挂在 create 等分配路径上
/// （新分配即将发生 = 归还旧页帧的天然时机，对标 Linux 内存压力驱动
/// 的 inode 回收），把析构成本从 unlink 临界路径挪到分配路径摊销。
pub fn inode_reap_drain_some(n: usize) {
    let batch: Vec<Arc<dyn Inode>> = {
        let mut q = INODE_REAP.lock();
        if q.is_empty() {
            return;
        }
        let len = q.len();
        let k = len.min(n);
        q.drain(len - k..).collect()
    };
    drop(batch);
}

/// 排空延迟析构队列。调用点：进程退出、调度器睡眠/空闲循环。
pub fn inode_reap_drain() {
    loop {
        let batch = {
            let mut q = INODE_REAP.lock();
            if q.is_empty() {
                return;
            }
            core::mem::take(&mut *q)
        };
        drop(batch);
    }
}

pub fn dentry_cache_remove_tree(path: &str) {
    if !dentry_cache_enabled(path) {
        return;
    }
    let prefix = if path == "/" {
        String::from("/")
    } else {
        alloc::format!("{}/", path.trim_end_matches('/'))
    };
    // 仅 rename/rmdir 低频路径；哈希表下用 retain 全表扫。
    // canonical_dir 的值（symlink 展开结果）也可能落在被移走的子树下，
    // 键不在前缀内时只清 canonical 字段、保留 inode 缓存。
    *DENTRY_PARENT_MRU.lock() = None;
    for shard in &DENTRY_CACHE {
        let mut c = shard.lock();
        c.map.retain(|cached_path, entry| {
            if cached_path == path || cached_path.starts_with(&prefix) {
                return false;
            }
            if let Some(c) = entry.canonical_dir.as_ref() {
                if c == path || c.starts_with(&prefix) {
                    entry.canonical_dir = None;
                }
            }
            true
        });
        c.recount_negatives();
    }
}

fn nofollow_cache_get(path: &str) -> Option<Arc<dyn Inode>> {
    if !dentry_cache_enabled(path) {
        return None;
    }
    DENTRY_CACHE[dentry_shard(path)]
        .lock()
        .map
        .get(path)
        .and_then(|e| e.nofollow.clone())
}

fn nofollow_cache_insert(path: &str, inode: Arc<dyn Inode>) {
    if dentry_cache_enabled(path) {
        dentry_mru_invalidate(path);
        let mut cache = DENTRY_CACHE[dentry_shard(path)].lock();
        // 同 dentry_cache_insert：只缓存目录与 symlink；普通文件也要先清负向。
        let ft = inode.file_type();
        if ft != FileType::Directory && ft != FileType::Symlink {
            if let Some(e) = cache.map.get_mut(path) {
                if e.negative {
                    e.negative = false;
                    cache.negative_count = cache.negative_count.saturating_sub(1);
                }
            }
            return;
        }
        let had_negative = cache.map.get(path).map_or(false, |e| e.negative);
        if had_negative {
            cache.negative_count = cache.negative_count.saturating_sub(1);
        }
        let e = cache.map.entry_ref(path).or_default();
        if had_negative {
            e.negative = false;
        }
        e.nofollow = Some(inode);
    }
}

pub(crate) fn canonical_dir_get(dir: &str) -> Option<String> {
    if !dentry_cache_enabled(dir) {
        return None;
    }
    DENTRY_CACHE[dentry_shard(dir)]
        .lock()
        .map
        .get(dir)
        .and_then(|e| e.canonical_dir.clone())
}

pub(crate) fn canonical_dir_insert(dir: &str, canonical: &str) {
    if dentry_cache_enabled(dir) {
        DENTRY_CACHE[dentry_shard(dir)]
            .lock()
            .map
            .entry_ref(dir)
            .or_default()
            .canonical_dir = Some(String::from(canonical));
    }
}

/// VFS 管理器
pub struct Vfs {
    /// 挂载点：同一路径上可堆叠多个挂载（对标 Linux mount stacking，
    /// umount 弹出最上层、露出被覆盖的下层挂载/原 dentry）。Vec 末尾为最上层。
    /// 这是 *init* mount namespace 的挂载表。
    mounts: BTreeMap<String, Vec<Arc<dyn FileSystem>>>,
    /// 非 init mount namespace 的挂载表，按 ns_id 索引（对标 Linux 每个
    /// mount namespace 各有独立的挂载视图）。unshare(CLONE_NEWNS) 时从当前
    /// 命名空间快照一份；路径解析按当前进程的命名空间选表。
    ns_mounts: BTreeMap<usize, BTreeMap<String, Vec<Arc<dyn FileSystem>>>>,
    /// 根文件系统
    root: Option<Arc<dyn FileSystem>>,
}

impl Vfs {
    fn new() -> Self {
        Self {
            mounts: BTreeMap::new(),
            ns_mounts: BTreeMap::new(),
            root: None,
        }
    }

    /// Immutable mount map for namespace `ns` (init uses the default map).
    fn map_for(&self, ns: usize) -> Option<&BTreeMap<String, Vec<Arc<dyn FileSystem>>>> {
        if ns == crate::fs::mount::INIT_MNT_NS {
            Some(&self.mounts)
        } else {
            self.ns_mounts.get(&ns)
        }
    }

    /// Mutable mount map for namespace `ns`, creating it on demand.
    fn map_for_mut(&mut self, ns: usize) -> &mut BTreeMap<String, Vec<Arc<dyn FileSystem>>> {
        if ns == crate::fs::mount::INIT_MNT_NS {
            &mut self.mounts
        } else {
            self.ns_mounts.entry(ns).or_insert_with(BTreeMap::new)
        }
    }

    /// Snapshot namespace `from`'s mount table into namespace `to` (对标
    /// unshare(CLONE_NEWNS)/copy_mnt_ns: the new namespace starts as a copy of
    /// the caller's mounts; later mounts on either side stay private unless
    /// propagation applies). Arc layers are shared so cloned mounts mirror
    /// content. Never overwrites an existing `to` table.
    pub fn snapshot_ns(&mut self, from: usize, to: usize) {
        if to == crate::fs::mount::INIT_MNT_NS || self.ns_mounts.contains_key(&to) {
            return;
        }
        let src = self.map_for(from).cloned().unwrap_or_default();
        self.ns_mounts.insert(to, src);
    }

    /// 挂载文件系统（当前命名空间）
    pub fn mount(&mut self, path: &str, fs: Arc<dyn FileSystem>) {
        self.mount_in_ns(crate::fs::mount::current_mnt_ns(), path, fs);
    }

    /// 在指定命名空间挂载文件系统（用于跨命名空间传播副本）。
    pub fn mount_in_ns(&mut self, ns: usize, path: &str, fs: Arc<dyn FileSystem>) {
        if path == "/" && ns == crate::fs::mount::INIT_MNT_NS {
            self.root = Some(fs.clone());
        }
        self.map_for_mut(ns).entry(path.to_string()).or_insert_with(Vec::new).push(fs);
    }

    /// 卸载文件系统。
    ///
    /// Linux mount namespace semantics require a successful non-root umount to
    /// reveal the covered mount-point dentry again.  Keeping a stale entry in
    /// `mounts` makes data created during one loop-backed LTP filesystem round
    /// leak into the next round (ext2 -> vfat -> tmpfs), which is observably
    /// wrong (`mkdir("subdir")` returns EEXIST after a fresh mkfs+mount).
    pub fn unmount(&mut self, path: &str) -> Result<(), FsError> {
        self.unmount_in_ns(crate::fs::mount::current_mnt_ns(), path)
    }

    /// Unmount in a specific namespace's map (peers in other namespaces are
    /// torn down by umount propagation, which crosses namespace boundaries).
    pub fn unmount_in_ns(&mut self, ns: usize, path: &str) -> Result<(), FsError> {
        if path == "/" {
            return Err(FsError::Busy);
        }
        let map = self.map_for_mut(ns);
        match map.get_mut(path) {
            Some(stack) if !stack.is_empty() => {
                stack.pop();
                if stack.is_empty() {
                    map.remove(path);
                }
                Ok(())
            }
            _ => Err(FsError::InvalidArgument),
        }
    }

    /// Replicate the full mount stack at `source` onto `dest`, appending the
    /// same `Arc<dyn FileSystem>` layers in order (对标 Linux copy_tree: each
    /// cloned layer keeps its *own* root; binding every layer to the topmost
    /// mount would make lower stacked layers wrongly show the top mount's
    /// content). Layers are shared by Arc — bind clones must mirror content.
    pub fn clone_mount_stack(&mut self, source: &str, dest: &str) {
        let ns = crate::fs::mount::current_mnt_ns();
        let map = self.map_for_mut(ns);
        if let Some(src_stack) = map.get(source).cloned() {
            let entry = map.entry(dest.to_string()).or_insert_with(Vec::new);
            for fs in src_stack {
                entry.push(fs);
            }
        }
    }

    /// Relocate a whole mount subtree for `mount --move`: every mount stack
    /// whose key is `source` or lives under `source/` is re-keyed under
    /// `target`, preserving stack order (对标 Linux do_move_mount moving the
    /// mount and all its children together). Stacks moved onto an existing
    /// target key are appended on top.
    pub fn move_subtree(&mut self, source: &str, target: &str) {
        let ns = crate::fs::mount::current_mnt_ns();
        let src_prefix = alloc::format!("{}/", source);
        let map = self.map_for_mut(ns);
        let keys: Vec<String> = map.keys()
            .filter(|k| k.as_str() == source || k.starts_with(&src_prefix))
            .cloned()
            .collect();
        for k in keys {
            if let Some(stack) = map.remove(&k) {
                let newk = if k == source {
                    target.to_string()
                } else {
                    alloc::format!("{}{}", target, &k[source.len()..])
                };
                map.entry(newk).or_insert_with(Vec::new).extend(stack);
            }
        }
    }

    /// 解析路径（Slice 2.x T6：支持中间 component 是 symlink 自动 follow）
    pub fn lookup(&self, path: &str) -> Result<Arc<dyn Inode>, FsError> {
        // ELOOP 防护：Linux 默认 40 层 symlink 链
        const MAX_FOLLOW: usize = 40;
        self.lookup_inner(path, 0, MAX_FOLLOW)
    }

    pub fn lookup_no_follow(&self, path: &str) -> Result<Arc<dyn Inode>, FsError> {
        const MAX_FOLLOW: usize = 40;
        self.lookup_no_follow_inner(path, 0, MAX_FOLLOW)
    }

    fn lookup_no_follow_inner(&self, path: &str, depth: usize, max_depth: usize) -> Result<Arc<dyn Inode>, FsError> {
        self.lookup_walk(path, depth, max_depth, false)
    }

    fn lookup_inner(&self, path: &str, depth: usize, max_depth: usize) -> Result<Arc<dyn Inode>, FsError> {
        self.lookup_walk(path, depth, max_depth, true)
    }

    fn lookup_walk(
        &self,
        path: &str,
        depth: usize,
        max_depth: usize,
        follow_final: bool,
    ) -> Result<Arc<dyn Inode>, FsError> {
        if depth > max_depth {
            return Err(FsError::TooManyLinks); // -ELOOP
        }
        // 绝大多数 lookup 的 path 已是规范形（syscall 入口 resolve_at_path 早已
        // 归一化），normalize_lookup_path 会再分配 Vec+String 重走一遍纯属浪费。
        // 本内核全局加锁分配器很贵，故先 O(n) 扫描判定"已规范"→直接借用、零分配；
        // 仅在确有 ""/"."/".." 或非绝对/尾斜杠时才走 normalize（§1.13① 减分配）。
        let normalized_owned;
        let normalized: &str = if path_is_lookup_normalized(path) {
            path
        } else {
            normalized_owned = normalize_lookup_path(path);
            normalized_owned.as_str()
        };
        let root = self.root.as_ref().ok_or(FsError::NotFound)?;

        let mut mount_path = "/";
        let mut mounted_fs = root.clone();
        // Resolve mounts in the *current process's* mount namespace only (对标
        // Linux: path resolution walks the task's own mount tree).
        if let Some(active) = self.map_for(crate::fs::mount::current_mnt_ns()) {
            for (candidate, stack) in active {
                if candidate == "/" {
                    continue;
                }
                let fs = match stack.last() { Some(f) => f, None => continue };
                let exact = normalized == candidate;
                let child = normalized.starts_with(candidate)
                    && normalized.as_bytes().get(candidate.len()) == Some(&b'/');
                if (exact || child) && candidate.len() > mount_path.len() {
                    mount_path = candidate;
                    mounted_fs = fs.clone();
                }
            }
        }

        let mut current = mounted_fs.root();
        let remainder = if mount_path == "/" {
            normalized
        } else if normalized == mount_path {
            "/"
        } else {
            &normalized[mount_path.len()..]
        };

        // 常规走查不物化 components（每次 lookup 省一个 Vec 分配）：用 peekable
        // 探测末项即可；仅在罕见的"中间分量是符号链接"分支才 re-split 取前后缀。
        let mut iter = remainder.split('/').filter(|s| !s.is_empty()).peekable();
        if iter.peek().is_none() {
            return Ok(current);
        }
        let mut idx = 0usize;
        while let Some(component) = iter.next() {
            // Linux namei: a single path component longer than NAME_MAX(255)
            // is rejected with ENAMETOOLONG before any lookup is attempted
            // (statx03/mkdir03/rename/link overlong-name subcases). The whole
            // path may still be under PATH_MAX, so this is distinct from the
            // getname()-level PATH_MAX check done at the syscall boundary.
            if component.len() > 255 {
                return Err(FsError::NameTooLong);
            }
            let dir = current.as_dir().ok_or(FsError::NotDirectory)?;
            let next = dir.lookup(component)?;
            let is_last = iter.peek().is_none();
            let should_follow = next.file_type() == FileType::Symlink && (follow_final || !is_last);

            if should_follow {
                if depth >= max_depth {
                    return Err(FsError::TooManyLinks); // -ELOOP
                }
                // `current_path`（被符号链接解析的所在目录）与前后缀分量仅在此罕见
                // 分支用到，故此处才 re-split 物化 components、懒构造 current_path
                // （= mount_path 拼接此前已走过的 components[..idx]）。常规路径零此类分配。
                let components: Vec<&str> =
                    remainder.split('/').filter(|s| !s.is_empty()).collect();
                let mut current_path: String = mount_path.into();
                for done in &components[..idx] {
                    current_path = join_lookup_path(&current_path, done);
                }
                let target = next.read_link()?;
                let mut resolved = if target.starts_with('/') {
                    normalize_lookup_path(&target)
                } else {
                    join_lookup_path(&current_path, &target)
                };
                for rest in &components[idx + 1..] {
                    resolved = join_lookup_path(&resolved, rest);
                }
                return self.lookup_walk(&resolved, depth + 1, max_depth, follow_final);
            }

            current = next;
            idx += 1;
        }

        Ok(current)
    }
}

/// VFS-internal syntactic path normalization used during path walking.
///
/// Syscall entry points normalize user-provided paths before reaching VFS, but
/// symlink expansion happens inside VFS and can introduce `.`/`..` components
/// (e.g. `dir/link -> ../dir`).  Linux namei handles these while preserving
/// symlink-follow accounting; without this step such paths incorrectly fail as
/// ENOENT instead of eventually returning ELOOP for recursive links.
/// 快速判定 `path` 是否已是 `normalize_lookup_path` 的输出形（绝对、无尾斜杠、
/// 无空/"."/".." 分量），从而可被 lookup_walk 直接借用、跳过重归一化分配。
/// 与 normalize_lookup_path 的规则严格一致：split('/') 后丢弃 ""/"."、遇 ".." 弹栈。
fn path_is_lookup_normalized(path: &str) -> bool {
    let bytes = path.as_bytes();
    if path == "/" {
        return true;
    }
    // 必须绝对路径且无尾斜杠（"/a/" 归一化后变 "/a"）。
    if bytes.first() != Some(&b'/') || bytes.last() == Some(&b'/') {
        return false;
    }
    // 逐字节单遍扫描检查无 "" / "." / ".." 分量（不用 split 的通用
    // CharSearcher，它在每 syscall 路径解析热路上每次 next 都是一次非
    // 内联调用）。
    let mut comp_start = 1;
    let mut i = 1;
    while i <= bytes.len() {
        if i == bytes.len() || bytes[i] == b'/' {
            let comp = &bytes[comp_start..i];
            if comp.is_empty() || comp == b"." || comp == b".." {
                return false;
            }
            comp_start = i + 1;
        }
        i += 1;
    }
    true
}

fn normalize_lookup_path(path: &str) -> String {
    let mut components: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                components.pop();
            }
            other => components.push(other),
        }
    }

    if components.is_empty() {
        String::from("/")
    } else {
        let mut out = String::new();
        for component in components {
            out.push('/');
            out.push_str(component);
        }
        out
    }
}

fn join_lookup_path(base: &str, child: &str) -> String {
    if child.starts_with('/') {
        return normalize_lookup_path(child);
    }
    if base == "/" {
        normalize_lookup_path(&alloc::format!("/{}", child))
    } else {
        normalize_lookup_path(&alloc::format!("{}/{}", base.trim_end_matches('/'), child))
    }
}

/// 初始化 VFS
pub fn init() {
    let mut vfs = VFS.lock();
    *vfs = Some(Vfs::new());
    crate::println!("[fs] VFS initialized");
    // Initialise the mount info table with built-in mounts.
    drop(vfs);
    mount_info_init();
}

/// 尝试挂载 FAT32 文件系统（从 VirtIO 块设备）
pub fn mount_fat32() -> Result<(), &'static str> {
    match fat32::Fat32FsWrapper::mount_virtio() {
        Ok(fs) => {
            mount("/", Arc::new(fs));
            crate::println!("[fs] FAT32 mounted at /");
            Ok(())
        }
        Err(e) => {
            crate::println!("[fs] Failed to mount FAT32: {}", e);
            Err(e)
        }
    }
}

/// 挂载文件系统
pub fn mount(path: &str, fs: Arc<dyn FileSystem>) {
    if let Some(vfs) = VFS.lock().as_mut() {
        vfs.mount(path, fs);
        dentry_cache_clear_all();
    }
}

/// Mount into a specific mount namespace (cross-namespace propagation copies).
pub fn mount_in_ns(ns: usize, path: &str, fs: Arc<dyn FileSystem>) {
    if let Some(vfs) = VFS.lock().as_mut() {
        vfs.mount_in_ns(ns, path, fs);
        dentry_cache_clear_all();
    }
}

/// Unmount in a specific mount namespace (umount propagation crosses ns).
pub fn unmount_in_ns(path: &str, ns: usize) -> Result<(), FsError> {
    let result = if let Some(vfs) = VFS.lock().as_mut() {
        vfs.unmount_in_ns(ns, path)
    } else {
        Err(FsError::NotFound)
    };
    if result.is_ok() {
        dentry_cache_clear_all();
    }
    result
}

/// Snapshot namespace `from`'s VFS mount table into a freshly created namespace
/// `to` (unshare(CLONE_NEWNS) / setns into a not-yet-materialised ns).
pub fn snapshot_mount_ns(from: usize, to: usize) {
    if let Some(vfs) = VFS.lock().as_mut() {
        vfs.snapshot_ns(from, to);
        dentry_cache_clear_all();
    }
}

/// Replicate the full VFS mount stack at `source` onto `dest` (copy_tree:
/// each cloned layer keeps its own root). See `Vfs::clone_mount_stack`.
pub fn clone_mount_stack(source: &str, dest: &str) {
    if let Some(vfs) = VFS.lock().as_mut() {
        vfs.clone_mount_stack(source, dest);
        dentry_cache_clear_all();
    }
}

/// Returns true if `path` is exactly a registered mount point (the root dentry
/// of a mounted filesystem). Linux rmdir(2)/rename(2) on such a dentry fails
/// with EBUSY because it is in use as a mount point (rmdir02 mntpoint subcase).
pub fn is_mount_point(path: &str) -> bool {
    let normalized = if path.is_empty() { "/" } else { path.trim_end_matches('/') };
    let normalized = if normalized.is_empty() { "/" } else { normalized };
    if let Some(vfs) = VFS.lock().as_ref() {
        vfs.map_for(crate::fs::mount::current_mnt_ns())
            .map(|m| m.contains_key(normalized))
            .unwrap_or(false)
    } else {
        false
    }
}

/// Return the mount_id for the mount that owns `path`.
/// Picks the longest-prefix match among registered mount targets.
pub fn mount_id_for_path(path: &str) -> u32 {
    let cur_ns = crate::fs::mount::current_mnt_ns();
    let table = MOUNT_INFO_TABLE.lock();
    let normalized = if path.is_empty() { "/" } else { path };
    table.iter()
        .filter(|m| {
            if m.ns_id != cur_ns { return false; }
            if m.target == "/" { return true; }
            if normalized == m.target { return true; }
            // Check if normalized starts with m.target + "/"
            normalized.len() > m.target.len()
                && normalized.as_bytes()[m.target.len()] == b'/'
                && normalized.starts_with(&m.target)
        })
        .max_by_key(|m| m.target.len())
        .map(|m| m.mount_id)
        .unwrap_or(0)
}

/// Return the filesystem Arc for a given mount point (for bind/move mounts).
pub fn get_mount_fs(path: &str) -> Option<Arc<dyn FileSystem>> {
    let normalized = if path.is_empty() { "/" } else { path.trim_end_matches('/') };
    let normalized = if normalized.is_empty() { "/" } else { normalized };
    if let Some(vfs) = VFS.lock().as_ref() {
        vfs.map_for(crate::fs::mount::current_mnt_ns())
            .and_then(|m| m.get(normalized))
            .and_then(|s| s.last().cloned())
    } else {
        None
    }
}

/// Relocate a whole mount subtree at the VFS layer for `mount --move`.
pub fn move_mount_subtree(source: &str, target: &str) {
    if let Some(vfs) = VFS.lock().as_mut() {
        vfs.move_subtree(source, target);
        dentry_cache_clear_all();
    }
}

pub fn unmount(path: &str) -> Result<(), FsError> {
    let result = if let Some(vfs) = VFS.lock().as_mut() {
        vfs.unmount(path)
    } else {
        Err(FsError::NotFound)
    };
    if result.is_ok() {
        dentry_cache_clear_all();
        set_readonly_mount(path, false);
        set_noexec_mount(path, false);
        set_nosuid_mount(path, false);
        set_nodev_mount(path, false);
        set_noatime_mount(path, false);
        set_nodiratime_mount(path, false);
        set_nosymfollow_mount(path, false);
    }
    result
}

/// VFS 路径解析的无锁 walk：每次进入只短暂锁 VFS 取「根 fs + 当前
/// mount namespace 最长前缀挂载」的快照，随后释放 VFS 锁，在文件系统层
/// （目录内 hash/串查找、ext4 块读取）完成全部组件走查。并发 mount/unmount
/// 仅影响下一次快照；已取得的 `Arc<dyn FileSystem>` 由引用计数保活。
fn lookup_path_impl(
    path: &str,
    depth: usize,
    max_depth: usize,
    follow_final: bool,
) -> Result<Arc<dyn Inode>, FsError> {
    if depth > max_depth {
        return Err(FsError::TooManyLinks);
    }
    let normalized_owned;
    let normalized: &str = if path_is_lookup_normalized(path) {
        path
    } else {
        normalized_owned = normalize_lookup_path(path);
        normalized_owned.as_str()
    };

    // Mount snapshot（对标 Linux RCU path walk：只在 mount 表上短暂取快照）。
    let (mount_path, mounted_fs) = {
        let vfs_guard = VFS.lock();
        let vfs = vfs_guard.as_ref().ok_or(FsError::NotFound)?;
        let root = vfs.root.clone().ok_or(FsError::NotFound)?;
        let mut mount_path = String::from("/");
        let mut mounted_fs = root;
        if let Some(active) = vfs.map_for(crate::fs::mount::current_mnt_ns()) {
            for (candidate, stack) in active {
                if candidate == "/" {
                    continue;
                }
                let fs = match stack.last() {
                    Some(f) => f,
                    None => continue,
                };
                let exact = normalized == candidate;
                let child = normalized.starts_with(candidate)
                    && normalized.as_bytes().get(candidate.len()) == Some(&b'/');
                if (exact || child) && candidate.len() > mount_path.len() {
                    mount_path = candidate.clone();
                    mounted_fs = fs.clone();
                }
            }
        }
        (mount_path, mounted_fs)
    };

    let mut current = mounted_fs.root();
    let remainder = if mount_path == "/" {
        normalized
    } else if normalized == mount_path.as_str() {
        "/"
    } else {
        &normalized[mount_path.len()..]
    };

    let mut iter = remainder.split('/').filter(|s| !s.is_empty()).peekable();
    if iter.peek().is_none() {
        return Ok(current);
    }
    let mut idx = 0usize;
    while let Some(component) = iter.next() {
        if component.len() > 255 {
            return Err(FsError::NameTooLong);
        }
        let dir = current.as_dir().ok_or(FsError::NotDirectory)?;
        let next = dir.lookup(component)?;
        let is_last = iter.peek().is_none();
        let should_follow =
            next.file_type() == FileType::Symlink && (follow_final || !is_last);
        if should_follow {
            if depth >= max_depth {
                return Err(FsError::TooManyLinks);
            }
            let components: Vec<&str> = remainder
                .split('/')
                .filter(|s| !s.is_empty())
                .collect();
            let mut current_path: String = mount_path.clone();
            for done in &components[..idx] {
                current_path = join_lookup_path(&current_path, done);
            }
            let target = next.read_link()?;
            let mut resolved = if target.starts_with('/') {
                normalize_lookup_path(&target)
            } else {
                join_lookup_path(&current_path, &target)
            };
            for rest in &components[idx + 1..] {
                resolved = join_lookup_path(&resolved, rest);
            }
            return lookup_path_impl(&resolved, depth + 1, max_depth, follow_final);
        }
        current = next;
        idx += 1;
    }
    Ok(current)
}

/// 查找文件
pub fn lookup(path: &str) -> Result<Arc<dyn Inode>, FsError> {
    if path == "/dev" || path.starts_with("/dev/") {
        if let Ok(inode) = devfs::lookup_path(path) {
            return Ok(inode);
        }
    }
// 末级单级快路径优先（dentry 树式解析）：父目录 MRU/缓存命中时
    // 直接在父目录上做一级名字查找（非目录末级不入全局表，同目录
    // 连续操作 = 串比较 + 单次短名哈希）。目录/symlink 结果不走快路径
    //（目录可能是挂载点、终端 symlink 需 follow），回退全 walk 保正确。
    if let Some(inode) = single_level_lookup_fast(path, true) {
        return Ok(inode);
    }
    match dentry_cache_lookup_followed(path) {
        DentryLookup::Hit(inode) => return Ok(inode),
        DentryLookup::Negative => return Err(FsError::NotFound),
        DentryLookup::Miss => {}
    }
    let result = lookup_path_impl(path, 0, 40, true);
    let inode = match result {
        Ok(inode) => inode,
        Err(e) => {
            if matches!(e, FsError::NotFound) {
                dentry_cache_insert_negative(path);
            }
            // FSTRACE：把「内核对哪条路径报了不存在」直接落到串口，用来对上
            // 用户态的 `couldn't find source for <crate>`，避免从现象反推。
            if crate::cmdline::fstrace_enabled() {
                let pid = crate::sched::process_sched::HART_CURRENT_PID
                    .load(core::sync::atomic::Ordering::Relaxed);
                crate::println!("[FSMISS] pid={} err={:?} path={}", pid, e, path);
            }
            return Err(e);
        }
    };
    dentry_cache_insert(path, inode.clone());
    Ok(inode)
}

/// 父目录 dentry 缓存命中时的单级查找快路径（MRU 单槽优先）。
/// 仅当结果是「非目录且（follow 模式下）非 symlink」才命中：目录可能被挂载
/// 覆盖、终端 symlink 需要 follow 解析，那些情形交回全 walk。
/// 路径须已是规范形（绝对、无 "."/".."），否则父目录切分不可靠。
fn single_level_lookup_fast(path: &str, follow: bool) -> Option<Arc<dyn Inode>> {
    if !path_is_lookup_normalized(path) {
        return None;
    }
    let pos = path.rfind('/')?;
    if pos == 0 {
        return None; // 根目录直子项可能是挂载点，走全 walk
    }
    let (parent_path, name) = (&path[..pos], &path[pos + 1..]);
    let parent = dentry_cache_parent_get(parent_path)?;
    let inode = parent.as_dir()?.lookup(name).ok()?;
    let ft = inode.file_type();
    if ft == FileType::Directory || (follow && ft == FileType::Symlink) {
        return None;
    }
    Some(inode)
}

/// Resolve the parent directory of `path` and return (parent_inode, child_name).
/// Single VFS traversal — avoids the double-lookup in O_CREAT (first full path
/// fails NotFound, then re-lookup parent).
pub fn lookup_parent(path: &str) -> Result<(Arc<dyn Inode>, &str), FsError> {
    let (parent_path, file_name) = match path.rfind('/') {
        Some(pos) if pos > 0 => (&path[..pos], &path[pos + 1..]),
        Some(_) => ("/", &path[1..]),
        None => return Err(FsError::NotFound),
    };
    if file_name.is_empty() {
        return Err(FsError::InvalidArgument);
    }
    // 父目录 MRU 单槽快路径（dentry 树式解析）：同目录海量 create
    // （lmbench lat_fs）时字符串相等比较即命中，免全路径哈希+探测。
    if let Some(parent) = dentry_cache_parent_get(parent_path) {
        return Ok((parent, file_name));
    }
    let parent = lookup(parent_path)?;
    Ok((parent, file_name))
}

/// Resolve every path component, following intermediate and final symlinks, and
/// return the canonical absolute path (Linux `__d_path` style). Used by chdir(2)
/// so that the stored cwd — and thus getcwd(2) — reports the physical directory
/// reached through a symlink rather than the symlink's own name (getcwd03).
pub fn canonicalize(path: &str) -> Result<alloc::string::String, FsError> {
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;
    let mut result: Vec<String> = Vec::new();
    let mut pending: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .rev()
        .collect();
    let mut hops = 0usize;
    while let Some(comp) = pending.pop() {
        match comp.as_str() {
            "." => continue,
            ".." => {
                result.pop();
                continue;
            }
            _ => {}
        }
        let cand = if result.is_empty() {
            alloc::format!("/{}", comp)
        } else {
            alloc::format!("/{}/{}", result.join("/"), comp)
        };
        let inode = lookup_no_follow(&cand)?;
        if inode.file_type() == FileType::Symlink {
            hops += 1;
            if hops > 40 {
                return Err(FsError::TooManyLinks);
            }
            let target = inode.read_link()?;
            if target.starts_with('/') {
                result.clear();
            }
            let mut tcomps: Vec<String> = target
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            while let Some(tc) = tcomps.pop() {
                pending.push(tc);
            }
        } else {
            result.push(comp);
        }
    }
    if result.is_empty() {
        Ok("/".to_string())
    } else {
        Ok(alloc::format!("/{}", result.join("/")))
    }
}

pub fn lookup_no_follow(path: &str) -> Result<Arc<dyn Inode>, FsError> {
    if path == "/dev" || path.starts_with("/dev/") {
        if let Ok(inode) = devfs::lookup_path(path) {
            return Ok(inode);
        }
    }
    if let Some(inode) = single_level_lookup_fast(path, false) {
        return Ok(inode);
    }
    match dentry_cache_lookup_nofollow(path) {
        DentryLookup::Hit(inode) => return Ok(inode),
        DentryLookup::Negative => return Err(FsError::NotFound),
        DentryLookup::Miss => {}
    }
    let result = lookup_path_impl(path, 0, 40, false);
    let inode = match result {
        Ok(inode) => inode,
        Err(e) => {
            if matches!(e, FsError::NotFound) {
                dentry_cache_insert_negative(path);
            }
            return Err(e);
        }
    };
    nofollow_cache_insert(path, inode.clone());
    Ok(inode)
}

// --- File handle cache for name_to_handle_at / open_by_handle_at ---
static FILE_HANDLE_CACHE: TicketSpinLock<Option<BTreeMap<usize, String>>> = TicketSpinLock::new(None);

/// Register an inode→path mapping for open_by_handle_at
pub fn fhandle_register(ino: usize, path: &str) {
    let mut cache = FILE_HANDLE_CACHE.lock();
    let map = cache.get_or_insert_with(BTreeMap::new);
    map.insert(ino, String::from(path));
}

/// Lookup an inode by its number via the file handle cache
pub fn lookup_by_ino(ino: usize) -> Option<Arc<dyn Inode>> {
    let path = {
        let cache = FILE_HANDLE_CACHE.lock();
        cache.as_ref()?.get(&ino)?.clone()
    };
    // Resolve without following a final symlink so a handle registered for a
    // symlink itself returns the symlink inode (open_by_handle_at02 symlink).
    lookup_no_follow(&path).ok()
}


/// 确保目录存在，缺失时逐级创建。
pub fn create_dir_all(path: &str) -> Result<(), FsError> {
    if path.is_empty() || path == "/" {
        return Ok(());
    }

    let mut current = String::from("/");
    for component in path.split('/').filter(|s| !s.is_empty()) {
        let next = if current == "/" {
            alloc::format!("/{}", component)
        } else {
            alloc::format!("{}/{}", current, component)
        };

        if lookup(&next).is_err() {
            let parent = lookup(&current)?;
            let dir = parent.as_dir().ok_or(FsError::NotDirectory)?;
            let inode = dir.create(component, FileType::Directory)?;
            dentry_cache_insert(&next, inode);
        }

        current = next;
    }

    Ok(())
}

/// 确保普通文件存在；若已存在则不做任何操作。
pub fn create_file_if_missing(path: &str, file_type: FileType) -> Result<(), FsError> {
    if lookup(path).is_ok() {
        return Ok(());
    }

    let (parent_path, file_name) = match path.rfind('/') {
        Some(pos) if pos > 0 => (&path[..pos], &path[pos + 1..]),
        Some(_) => ("/", &path[1..]),
        None => return Err(FsError::InvalidArgument),
    };
    if file_name.is_empty() {
        return Err(FsError::InvalidArgument);
    }

    create_dir_all(parent_path)?;
    let parent = lookup(parent_path)?;
    let dir = parent.as_dir().ok_or(FsError::NotDirectory)?;
    match dir.create(file_name, file_type) {
        Ok(inode) => {
            dentry_cache_insert(path, inode);
            Ok(())
        }
        Err(FsError::AlreadyExists) => Ok(()),
        Err(err) => Err(err),
    }
}

pub mod mount;
pub mod ramfs;
pub mod bindfs;
pub mod overlayfs;
pub mod devfs;
pub mod notify;
pub use notify::dnotify;
pub mod epoll;
pub mod eventfd;
pub use notify::fanotify;
pub mod fat32;
pub mod ext4;
pub mod fd;
pub mod file_copy;
pub mod file_handle;
pub mod file_limits;
pub mod file_ops;
pub mod file_position;
pub mod positional_io;
pub mod file_cap;
pub mod file_lease;
pub mod file_lock;
pub use notify::inotify;
pub mod proc;
pub mod procfs;
pub mod read_io;
pub mod write_io;
pub mod attr;
pub mod pipe;
pub mod splice;
pub mod signalfd;
pub mod stdio;
pub mod timerfd;
pub mod sysfs;
pub mod cgroupfs;
pub mod memcgfs;

#[cfg(test)]
mod procfs_tests;

// 重新导出常用类型
#[allow(unused_imports)]
pub use ramfs::RamFs;

/// Inode 类型（用于 DirEntry，兼容旧代码）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InodeType {
    File,
    Directory,
    Symlink,
    Device,
}

/// 文件操作 trait（简化版，兼容旧代码）
pub trait FileOps: Send + Sync {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str>;
    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, &'static str>;
}


/// 文件系统自测试
pub fn self_test() {
    crate::println!("[fs] Running self-test...");

    // 测试 FileType
    let _types = [
        FileType::Regular,
        FileType::Directory,
        FileType::Symlink,
        FileType::CharDevice,
        FileType::BlockDevice,
        FileType::Pipe,
        FileType::Socket,
    ];

    // 测试 ProcFS
    procfs::init();

    // 测试 ProcFS 读取
    if let Ok(meminfo) = procfs::read("meminfo") {
        crate::println!("[fs] ProcFS meminfo: {} bytes", meminfo.len());
    }

    // 测试 ProcFS 目录列表
    if let Ok(entries) = procfs::readdir("") {
        crate::println!("[fs] ProcFS root has {} entries", entries.len());
    }

    // 测试 ProcFS VFS 适配器
    let procfs_adapter = procfs::ProcFsAdapter::new();
    let _root = procfs_adapter.root();
    crate::println!("[fs] ProcFS VFS adapter created");

    // 挂载 ProcFS
    procfs::mount_procfs();

    // 运行 ProcFS 自测试
    procfs::self_test();

    // 运行 FAT32 自测试
    fat32::self_test();

    // 测试 FileMode
    let mode = FileMode::default_file();
    assert_eq!(mode.0, 0o644);

    let dir_mode = FileMode::default_dir();
    assert_eq!(dir_mode.0, 0o755);

    let custom_mode = FileMode::new(0o777);
    assert_eq!(custom_mode.0, 0o777);

    // 测试 FileMode 常量
    let _perms = [
        FileMode::S_IRUSR,
        FileMode::S_IWUSR,
        FileMode::S_IXUSR,
        FileMode::S_IRGRP,
        FileMode::S_IWGRP,
        FileMode::S_IXGRP,
        FileMode::S_IROTH,
        FileMode::S_IWOTH,
        FileMode::S_IXOTH,
    ];
    crate::println!("[fs] FileMode tests passed");

    // 测试 FileStat
    let stat = FileStat {
        file_type: FileType::Regular,
        size: 1024,
        mode: FileMode::default_file(),
        nlink: 1,
        uid: 0,
        gid: 0,
        atime: 0,
        mtime: 0,
        ctime: 0,
    };
    assert_eq!(stat.size, 1024);
    assert_eq!(stat.file_type, FileType::Regular);

    // 测试 DirEntry
    let entry = DirEntry {
        name: alloc::string::String::from("test.txt"),
        file_type: FileType::Regular,
        inode: 1,
    };
    assert_eq!(entry.name, "test.txt");
    assert_eq!(entry.inode, 1);
    crate::println!("[fs] DirEntry tests passed");

    // 测试 FsError 变体
    let _errors = [
        FsError::NotFound,
        FsError::AlreadyExists,
        FsError::NotDirectory,
        FsError::IsDirectory,
        FsError::PermissionDenied,
        FsError::OperationNotPermitted,
        FsError::InvalidArgument,
        FsError::IoError,
        FsError::NoSpace,
        FsError::NotEmpty,
        FsError::WouldBlock,
        FsError::Busy,
        FsError::BrokenPipe,
        FsError::OutOfMemory,
    ];

    #[cfg(test)]
    {
        // 测试 FdTable
        let mut fd_table = FdTable::new();

        // 创建一个简单的 RamFs 来测试
        let ramfs = ramfs::RamFs::new();
        let root = ramfs.root();

        // 测试 FileDescriptor
        let fd = FileDescriptor::new(root.clone(), 0);
        let _stat = fd.file.stat();

        // 分配文件描述符
        let fd_num = fd_table.alloc(Arc::new(fd));
        assert!(fd_num >= 3); // 0, 1, 2 保留

        // 获取文件描述符
        let retrieved = fd_table.get(fd_num);
        assert!(retrieved.is_some());

        // 关闭文件描述符
        let closed = fd_table.close(fd_num);
        assert!(closed);

        let not_found = fd_table.get(fd_num);
        assert!(not_found.is_none());
        crate::println!("[fs] FdTable tests passed");
    }

    // 测试 VFS lookup
    let result = lookup("/");
    if result.is_ok() {
        crate::println!("[fs] Root lookup succeeded");
    }

    // 测试 InodeType
    let _inode_types = [
        InodeType::File,
        InodeType::Directory,
        InodeType::Symlink,
        InodeType::Device,
    ];

    crate::println!("[fs] Self-test passed!");
}
