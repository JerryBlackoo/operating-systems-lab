use super::types::poll::EpollFile;
use super::SysResult;
use crate::user::UserTaskContainer;
use crate::utils::useref::UserRef;
use alloc::string::String;
use alloc::sync::Arc;
use bit_field::BitArray;
use core::cmp;
use core::convert::TryInto;
use core::time::Duration;
use executor::yield_now;
use fs::dentry::{set_mount_readonly, umount};
use fs::file::File;
use fs::{
    pipe::{create_pipe, PipeReceiver, PipeSender},
    SeekFrom,
};
use libc_types::consts::UTIME_NOW;
use libc_types::epoll::{EpollCtl, EpollEvent, EpollEventType};
use libc_types::fcntl::{
    FcntlCmd, OpenFlags, AT_EACCESS, AT_EMPTY_PATH, AT_FDCWD, AT_REMOVEDIR, AT_SYMLINK_NOFOLLOW,
};
use libc_types::ioctl::TermIoctlCmd;
use libc_types::poll::{PollEvent, PollFd};
use libc_types::termios::Termios;
use libc_types::types::{
    IoVec, Stat, StatFS, StatMode, Statx, StatxMask, StatxTimestamp, TimeSpec, WinSize,
};
use log::debug;
use polyhal::timer::current_time;
use polyhal::VirtAddr;
use sync::Mutex;
use syscalls::Errno;
use vfscore::{FileType, INodeInterface, VfsResult};

const MAX_USER_PATH_LEN: usize = 4096;
const MAX_POLL_FDS: usize = 256;
const EFD_SEMAPHORE: usize = 0x1;
const EVENTFD_MAX: u64 = u64::MAX - 1;
const FD_CLOEXEC: usize = 1;
const EPOLL_CLOEXEC: usize = OpenFlags::CLOEXEC.bits();
const DUP3_FLAGS: usize = OpenFlags::CLOEXEC.bits();
const PIPE2_FLAGS: usize = OpenFlags::NONBLOCK.bits() | OpenFlags::CLOEXEC.bits();
const CLOSE_RANGE_UNSHARE: usize = 1 << 1;
const CLOSE_RANGE_CLOEXEC: usize = 1 << 2;
const CLOSE_RANGE_FLAGS: usize = CLOSE_RANGE_UNSHARE | CLOSE_RANGE_CLOEXEC;
const FALLOC_FL_KEEP_SIZE: usize = 0x01;
const FALLOCATE_SUPPORTED_FLAGS: usize = FALLOC_FL_KEEP_SIZE;
const S_IFMT: usize = 0o170000;
const S_IFIFO: usize = 0o010000;
const MODE_PERMISSIONS: usize = 0o7777;
const AT_NO_AUTOMOUNT: u32 = 0x800;
const STAT_LOOKUP_FLAGS: u32 = AT_EMPTY_PATH | AT_NO_AUTOMOUNT | AT_SYMLINK_NOFOLLOW;
const RLIMIT_FSIZE: usize = 1;
const MS_RDONLY: usize = 1;

pub struct EventFd {
    counter: Mutex<u64>,
    semaphore: bool,
}

impl EventFd {
    fn new(initval: u32, flags: usize) -> Result<Arc<Self>, Errno> {
        let allowed = EFD_SEMAPHORE | OpenFlags::NONBLOCK.bits() | OpenFlags::CLOEXEC.bits();
        if flags & !allowed != 0 {
            return Err(Errno::EINVAL);
        }
        Ok(Arc::new(Self {
            counter: Mutex::new(initval as u64),
            semaphore: flags & EFD_SEMAPHORE != 0,
        }))
    }
}

impl INodeInterface for EventFd {
    fn readat(&self, _offset: usize, buffer: &mut [u8]) -> VfsResult<usize> {
        if buffer.len() < core::mem::size_of::<u64>() {
            return Err(Errno::EINVAL);
        }
        let mut counter = self.counter.lock();
        if *counter == 0 {
            return Err(Errno::EWOULDBLOCK);
        }
        let value = if self.semaphore {
            *counter -= 1;
            1
        } else {
            let value = *counter;
            *counter = 0;
            value
        };
        buffer[..8].copy_from_slice(&value.to_ne_bytes());
        Ok(8)
    }

    fn writeat(&self, _offset: usize, buffer: &[u8]) -> VfsResult<usize> {
        if buffer.len() < core::mem::size_of::<u64>() {
            return Err(Errno::EINVAL);
        }
        let value = u64::from_ne_bytes(buffer[..8].try_into().unwrap());
        if value == u64::MAX {
            return Err(Errno::EINVAL);
        }
        let mut counter = self.counter.lock();
        if EVENTFD_MAX - *counter < value {
            return Err(Errno::EWOULDBLOCK);
        }
        *counter += value;
        Ok(8)
    }

    fn poll(&self, events: PollEvent) -> VfsResult<PollEvent> {
        let counter = *self.counter.lock();
        let mut res = PollEvent::NONE;
        if events.intersects(PollEvent::IN | PollEvent::RDNORM) && counter > 0 {
            res |= events.clone() & (PollEvent::IN | PollEvent::RDNORM);
        }
        if events.intersects(PollEvent::OUT | PollEvent::WRNORM) && counter < EVENTFD_MAX {
            res |= events & (PollEvent::OUT | PollEvent::WRNORM);
        }
        Ok(res)
    }

    fn stat(&self, stat: &mut Stat) -> VfsResult<()> {
        stat.mode = StatMode::CHAR;
        Ok(())
    }
}

fn timespec_to_statx(ts: TimeSpec) -> StatxTimestamp {
    StatxTimestamp {
        sec: ts.sec as i64,
        nsec: ts.nsec as u32,
        __reserved: 0,
    }
}

fn stat_to_statx(stat: &Stat, mask: u32) -> Statx {
    Statx {
        mask: (mask & StatxMask::BASIC_STATS.bits()) | StatxMask::BASIC_STATS.bits(),
        blksize: stat.blksize as u32,
        attributes: 0,
        nlink: stat.nlink as u32,
        uid: stat.uid,
        gid: stat.gid,
        mode: stat.mode.bits() as u16,
        __spare0: [0],
        ino: stat.ino,
        size: stat.size,
        blocks: stat.blocks,
        attributes_mask: 0,
        atime: timespec_to_statx(stat.atime),
        btime: StatxTimestamp::default(),
        ctime: timespec_to_statx(stat.ctime),
        mtime: timespec_to_statx(stat.mtime),
        rdev_major: 0,
        rdev_minor: stat.rdev as u32,
        dev_major: 0,
        dev_minor: stat.dev as u32,
        mnt_id: 0,
        dio_mem_align: 0,
        dio_offset_align: 0,
        __spare3: [0; 12],
    }
}

impl UserTaskContainer {
    fn fd_killed_result(&self) -> Option<SysResult> {
        self.task.exit_code().is_some().then_some(Err(Errno::EINTR))
    }

    fn truncate_write_allowed(stat: &Stat, euid: u32, egid: u32) -> bool {
        if euid == 0 {
            return true;
        }

        let mode = stat.mode.bits();
        if euid == stat.uid {
            mode & StatMode::OWNER_WRITE.bits() != 0
        } else if egid == stat.gid {
            mode & StatMode::GROUP_WRITE.bits() != 0
        } else {
            mode & StatMode::OTHER_WRITE.bits() != 0
        }
    }

    fn ensure_truncate_path_prefix(&self, path: &str) -> Result<(), Errno> {
        let mut prefix = if path.starts_with('/') {
            fs::pathbuf::PathBuf::empty()
        } else {
            self.task.pcb.lock().curr_dir.path_buf()
        };

        let mut components = path
            .split('/')
            .filter(|component| !component.is_empty() && *component != ".")
            .peekable();
        while let Some(component) = components.next() {
            if components.peek().is_none() {
                break;
            }

            prefix = prefix.join(component);
            let directory = File::open(prefix.clone(), OpenFlags::RDONLY)?;
            if directory.file_type()? != FileType::Directory {
                return Err(Errno::ENOTDIR);
            }
        }

        Ok(())
    }

    pub fn sys_dup(&self, fd: usize) -> SysResult {
        debug!("sys_dup3 @ fd_src: {}", fd);
        let fd_dst = self.task.alloc_fd().ok_or(Errno::EMFILE)?;
        self.dup_fd(fd, fd_dst, 0)
    }

    pub fn sys_dup3(&self, fd_src: usize, fd_dst: usize, flags: usize) -> SysResult {
        debug!(
            "sys_dup3 @ fd_src: {}, fd_dst: {}, flags: {:#x}",
            fd_src, fd_dst, flags
        );
        if flags & !DUP3_FLAGS != 0 || fd_src == fd_dst {
            return Err(Errno::EINVAL);
        }
        self.dup_fd(fd_src, fd_dst, flags)
    }

    fn dup_fd(&self, fd_src: usize, fd_dst: usize, flags: usize) -> SysResult {
        let source = self.task.get_fd(fd_src).ok_or(Errno::EBADF)?;
        let file = source.as_ref().clone();
        {
            let mut file_flags = file.flags.lock();
            *file_flags &= !OpenFlags::CLOEXEC;
            if flags & OpenFlags::CLOEXEC.bits() != 0 {
                *file_flags |= OpenFlags::CLOEXEC;
            }
        }
        let file = Arc::new(file);
        self.task.set_fd(fd_dst, file);
        Ok(fd_dst)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn sys_dup2(&self, fd_src: usize, fd_dst: usize) -> SysResult {
        if fd_src == fd_dst {
            self.task.get_fd(fd_src).ok_or(Errno::EBADF)?;
            return Ok(fd_dst);
        }
        self.dup_fd(fd_src, fd_dst, 0)
    }

    pub async fn sys_read(&self, fd: usize, buf_ptr: UserRef<u8>, count: usize) -> SysResult {
        debug!(
            "[task {}] sys_read @ fd: {} buf_ptr: {:?} count: {}",
            self.tid, fd as isize, buf_ptr, count
        );
        let buffer = buf_ptr.slice_mut_with_len(count);
        self.task
            .get_fd(fd)
            .ok_or(Errno::EBADF)?
            .async_read(buffer)
            .await
    }

    pub async fn sys_write(&self, fd: usize, buf_ptr: VirtAddr, count: usize) -> SysResult {
        debug!(
            "[task {}] sys_write @ fd: {} buf_ptr: {:?} count: {}",
            self.tid, fd as isize, buf_ptr, count
        );
        let buffer = buf_ptr.slice_with_len(count);
        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;
        file.async_write(buffer).await
    }

    pub fn sys_readv(&self, fd: usize, iov: UserRef<IoVec>, iocnt: usize) -> SysResult {
        debug!("sys_readv @ fd: {}, iov: {}, iocnt: {}", fd, iov, iocnt);
        let mut rsize = 0;
        let iov = iov.slice_mut_with_len(iocnt);
        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;

        for io in iov {
            let buffer = UserRef::<u8>::from(io.base).slice_mut_with_len(io.len);
            rsize += file.read(buffer)?;
        }

        Ok(rsize)
    }

    pub fn sys_writev(&self, fd: usize, iov: UserRef<IoVec>, iocnt: usize) -> SysResult {
        debug!("sys_writev @ fd: {}, iov: {}, iocnt: {}", fd, iov, iocnt);
        let mut wsize = 0;
        let iov = iov.slice_mut_with_len(iocnt);
        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;

        for io in iov {
            let buffer = UserRef::<u8>::from(io.base).slice_mut_with_len(io.len);
            wsize += file.write(buffer)?;
        }

        Ok(wsize)
    }

    pub fn sys_close(&self, fd: usize) -> SysResult {
        debug!("[task {}] sys_close @ fd: {}", self.tid, fd as isize);
        self.task.clear_fd(fd);
        Ok(0)
    }

    pub fn sys_close_range(&self, first: usize, last: usize, flags: usize) -> SysResult {
        debug!(
            "[task {}] sys_close_range @ first: {}, last: {}, flags: {:#x}",
            self.tid, first, last, flags
        );
        if first > last || flags & !CLOSE_RANGE_FLAGS != 0 {
            return Err(Errno::EINVAL);
        }
        let mut pcb = self.task.pcb.lock();
        if first >= pcb.fd_table.len() {
            return Ok(0);
        }
        let end = cmp::min(last, pcb.fd_table.len() - 1);
        for fd in first..=end {
            if flags & CLOSE_RANGE_CLOEXEC != 0 {
                if let Some(file) = pcb.fd_table[fd].as_ref() {
                    *file.flags.lock() |= OpenFlags::CLOEXEC;
                }
            } else {
                pcb.fd_table[fd] = None;
            }
        }
        Ok(0)
    }

    pub async fn sys_mkdir_at(&self, dir_fd: isize, path: UserRef<i8>, mode: usize) -> SysResult {
        if let Some(result) = self.fd_killed_result() {
            return result;
        }
        yield_now().await;
        if let Some(result) = self.fd_killed_result() {
            return result;
        }
        let path = self.read_user_cstr_bounded_len(path, MAX_USER_PATH_LEN)?;
        debug!(
            "sys_mkdir_at @ dir_fd: {}, path: {}, mode: {}",
            dir_fd as isize, path, mode
        );
        let full_path = self.task.fd_resolve(dir_fd, &path)?;
        let parent = File::open(full_path.dir(), OpenFlags::DIRECTORY)?;
        if parent.file_type()? != FileType::Directory {
            return Err(Errno::ENOTDIR);
        }
        match parent.lookup(&full_path.filename()) {
            Ok(_) => return Err(Errno::EEXIST),
            Err(Errno::ENOENT) => {}
            Err(err) => return Err(err),
        }
        parent.create(&full_path.filename(), FileType::Directory)?;

        let new_dir = File::open(full_path, OpenFlags::DIRECTORY)?;
        new_dir.chmod(StatMode::from_bits_truncate(
            mode as u32 & MODE_PERMISSIONS as u32,
        ))?;
        let credentials = self.task.pcb.lock().credentials;
        new_dir.chown(credentials.euid, credentials.egid)?;
        Ok(0)
    }

    pub fn sys_renameat2(
        &self,
        olddir_fd: isize,
        oldpath: UserRef<i8>,
        newdir_fd: isize,
        newpath: UserRef<i8>,
        flags: usize,
    ) -> SysResult {
        debug!(
            "sys_renameat2 @ olddir_fd: {}, oldpath: {}, newdir_fd: {}, newpath: {}, flags: {}",
            olddir_fd, oldpath, newdir_fd, newpath, flags
        );
        let flags = OpenFlags::from_bits_truncate(flags);

        let old_path = self.read_user_cstr_bounded_len(oldpath, MAX_USER_PATH_LEN)?;
        let old_file = self.task.fd_open(olddir_fd, &old_path, flags.clone())?;

        let old_file_type = old_file.file_type()?;
        let new_path = self.read_user_cstr_bounded_len(newpath, MAX_USER_PATH_LEN)?;

        if old_file_type == FileType::File {
            let new_file = self
                .task
                .fd_open(newdir_fd, &new_path, OpenFlags::CREAT | flags)?;
            let file_size = old_file.file_size()?;
            let mut buffer = vec![0u8; file_size];
            old_file.read(&mut buffer)?;
            new_file.write(&buffer)?;
            new_file.truncate(buffer.len())?;
        } else if old_file_type == FileType::Directory {
            self.task.fd_open(
                newdir_fd,
                &new_path,
                OpenFlags::CREAT | OpenFlags::DIRECTORY | flags,
            )?;
        } else {
            panic!("can't handle the file: {:?} now", old_file_type);
        }

        Ok(0)
    }

    #[cfg(target_arch = "x86_64")]
    pub async fn sys_mkdir(&self, path: UserRef<i8>, mode: usize) -> SysResult {
        self.sys_mkdir_at(AT_FDCWD, path, mode).await
    }

    pub fn sys_mknodat(
        &self,
        dir_fd: isize,
        path: UserRef<i8>,
        mode: usize,
        _dev: usize,
    ) -> SysResult {
        let path = self.read_user_cstr_bounded_len(path, MAX_USER_PATH_LEN)?;
        debug!(
            "sys_mknodat @ dir_fd: {}, path: {}, mode: {:#o}",
            dir_fd, path, mode
        );
        if mode & S_IFMT != S_IFIFO {
            return Err(Errno::ENOSYS);
        }
        let full_path = self.task.fd_resolve(dir_fd, &path)?;
        if full_path.dir().levels() > 0 {
            File::open(full_path.dir(), OpenFlags::DIRECTORY)?;
        }
        let dir = File::open(full_path.dir(), OpenFlags::DIRECTORY)?;
        dir.create(&full_path.filename(), FileType::Fifo)?;
        Ok(0)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn sys_unlink(&self, path: UserRef<i8>) -> SysResult {
        self.sys_unlinkat(AT_FDCWD, path, 0)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn sys_rmdir(&self, path: UserRef<i8>) -> SysResult {
        self.sys_unlinkat(AT_FDCWD, path, AT_REMOVEDIR as usize)
    }

    pub fn sys_unlinkat(&self, dir_fd: isize, path: UserRef<i8>, flags: usize) -> SysResult {
        let path = self.read_user_cstr_bounded_len(path, MAX_USER_PATH_LEN)?;
        debug!(
            "sys_unlinkat @ dir_fd: {}, path: {}, flags: {}",
            dir_fd as isize, path, flags
        );

        if flags & !(AT_REMOVEDIR as usize) != 0 {
            return Err(Errno::EINVAL);
        }

        let open_flags = if flags & AT_REMOVEDIR as usize != 0 {
            OpenFlags::DIRECTORY
        } else {
            OpenFlags::RDONLY
        };
        let file = self.task.fd_open(dir_fd, &path, open_flags)?;

        if flags & AT_REMOVEDIR as usize != 0 {
            if file.file_type()? != FileType::Directory {
                return Err(Errno::ENOTDIR);
            }
            file.rmdir_self()?;
        } else {
            if file.file_type()? == FileType::Directory {
                return Err(Errno::EISDIR);
            }
            file.remove_self()?;
        }
        Ok(0)
    }

    pub fn sys_openat(
        &self,
        dir_fd: isize,
        filename: UserRef<i8>,
        flags: usize,
        mode: usize,
    ) -> SysResult {
        let flags = OpenFlags::from_bits_truncate(flags);
        let filename = if filename.is_valid() {
            self.read_user_cstr_bounded_len(filename, MAX_USER_PATH_LEN)?
        } else {
            String::new()
        };
        debug!(
            "sys_openat @ fd: {}, filename: {}, flags: {:?}, mode: {}",
            dir_fd as isize, filename, flags, mode
        );
        // let dir = to_node(&self.task, fd, filename)?;
        // let file = dir.dentry_open(filename, flags)?;
        let traced_path = if crate::tasks::exec::loader_trace_enabled() {
            self.task.fd_resolve(dir_fd, &filename).ok()
        } else {
            None
        };
        let file = match self.task.fd_open(dir_fd, &filename, flags) {
            Ok(file) => file,
            Err(err) => {
                if let Some(path) = traced_path {
                    let path = path.path();
                    if crate::tasks::exec::is_loader_trace_path(path.as_str()) {
                        println!(
                            "loader-trace: openat task={} dirfd={} filename={} resolved={} result=err",
                            self.tid, dir_fd, filename, path
                        );
                    }
                }
                return Err(err.into());
            }
        };
        if flags.contains(OpenFlags::CREAT) {
            file.chmod(StatMode::from_bits_truncate(
                mode as u32 & MODE_PERMISSIONS as u32,
            ))?;
            let credentials = self.task.pcb.lock().credentials;
            file.chown(credentials.euid, credentials.egid)?;
        }
        if let Some(path) = traced_path {
            let path = path.path();
            if crate::tasks::exec::is_loader_trace_path(path.as_str()) {
                println!(
                    "loader-trace: openat task={} dirfd={} filename={} resolved={} result=ok",
                    self.tid, dir_fd, filename, path
                );
            }
        }
        let fd = self.task.alloc_fd().ok_or(Errno::EMFILE)?;
        self.task.set_fd(fd, Arc::new(file));
        debug!("sys_openat @ ret fd: {}", fd);
        Ok(fd)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn sys_open(&self, path: UserRef<i8>, flags: usize, mode: usize) -> SysResult {
        // syscall_openat(axprocess::link::AT_FDCWD, path, flags, mode)
        self.sys_openat(AT_FDCWD, path, flags, mode)
    }

    pub fn sys_faccess_at(
        &self,
        dir_fd: isize,
        filename: UserRef<i8>,
        mode: usize,
        flags: usize,
    ) -> SysResult {
        let filename = if filename.is_valid() {
            self.read_user_cstr_bounded_len(filename, MAX_USER_PATH_LEN)?
        } else {
            String::new()
        };
        debug!(
            "sys_accessat @ fd: {}, filename: {}, mode: {}, flags: {:#x}",
            dir_fd as isize, filename, mode, flags
        );
        if mode & !0b111 != 0 {
            return Err(Errno::EINVAL);
        }
        if flags & !(AT_EACCESS as usize | AT_SYMLINK_NOFOLLOW as usize) != 0 {
            return Err(Errno::EINVAL);
        }
        self.task.fd_open(dir_fd, &filename, OpenFlags::RDONLY)?;
        Ok(0)
    }

    pub fn sys_fstat(&self, fd: usize, stat_ptr: UserRef<Stat>) -> SysResult {
        debug!("sys_fstat @ fd: {} stat_ptr: {}", fd, stat_ptr);

        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;
        let mut stat = Stat::default();
        file.stat(&mut stat)?;
        self.write_user_value(stat_ptr, &stat)?;
        Ok(0)
    }

    pub fn sys_fstatat(
        &self,
        dir_fd: isize,
        path_ptr: UserRef<i8>,
        stat_ptr: UserRef<Stat>,
        flags: u32,
    ) -> SysResult {
        debug!(
            "sys_fstatat @ dir_fd: {}, path_ptr:{}, stat_ptr: {} flags: {:#x}",
            dir_fd as isize, path_ptr, stat_ptr, flags
        );
        if flags & !STAT_LOOKUP_FLAGS != 0 {
            return Err(Errno::EINVAL);
        }
        let path = self.read_user_cstr_bounded_len(path_ptr, MAX_USER_PATH_LEN)?;
        debug!(
            "sys_fstatat @ dir_fd: {}, path:{}, stat_ptr: {}",
            dir_fd as isize, path, stat_ptr
        );

        let file = if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
            if dir_fd == AT_FDCWD {
                self.task.pcb.lock().curr_dir.clone()
            } else {
                self.task
                    .get_fd(dir_fd as usize)
                    .ok_or(Errno::EBADF)?
                    .as_ref()
                    .clone()
            }
        } else {
            if !path.starts_with('/') && dir_fd != AT_FDCWD {
                let base = self.task.get_fd(dir_fd as usize).ok_or(Errno::EBADF)?;
                if base.file_type()? != FileType::Directory {
                    return Err(Errno::ENOTDIR);
                }
            }
            let path = self.task.fd_resolve(dir_fd, &path)?;
            if flags & AT_SYMLINK_NOFOLLOW != 0 {
                File::open(path, OpenFlags::RDONLY)
            } else {
                File::open_link(path, OpenFlags::RDONLY)
            }?
        };
        let mut stat = Stat::default();
        file.stat(&mut stat)?;
        self.write_user_value(stat_ptr, &stat)?;
        Ok(0)
    }

    pub fn sys_statx(
        &self,
        dir_fd: isize,
        path_ptr: UserRef<i8>,
        flags: u32,
        mask: u32,
        statx_ptr: UserRef<Statx>,
    ) -> SysResult {
        debug!(
            "sys_statx @ dir_fd: {}, path_ptr:{}, flags: {:#x}, mask: {:#x}, statx_ptr: {}",
            dir_fd, path_ptr, flags, mask, statx_ptr
        );
        if flags & !STAT_LOOKUP_FLAGS != 0 {
            return Err(Errno::EINVAL);
        }
        let path = self.read_user_cstr_bounded_len(path_ptr, MAX_USER_PATH_LEN)?;

        let file = if path.is_empty() && flags & AT_EMPTY_PATH != 0 {
            if dir_fd == AT_FDCWD {
                self.task.pcb.lock().curr_dir.clone()
            } else {
                self.task
                    .get_fd(dir_fd as usize)
                    .ok_or(Errno::EBADF)?
                    .as_ref()
                    .clone()
            }
        } else {
            if !path.starts_with('/') && dir_fd != AT_FDCWD {
                let base = self.task.get_fd(dir_fd as usize).ok_or(Errno::EBADF)?;
                if base.file_type()? != FileType::Directory {
                    return Err(Errno::ENOTDIR);
                }
            }
            let path = self.task.fd_resolve(dir_fd, &path)?;
            if flags & AT_SYMLINK_NOFOLLOW != 0 {
                File::open(path, OpenFlags::RDONLY)
            } else {
                File::open_link(path, OpenFlags::RDONLY)
            }?
        };

        let mut stat = Stat::default();
        file.stat(&mut stat)?;
        let statx = stat_to_statx(&stat, mask);
        self.write_user_value(statx_ptr, &statx)?;
        Ok(0)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn sys_stat(&self, path: UserRef<i8>, stat_ptr: UserRef<Stat>) -> SysResult {
        self.sys_fstatat(AT_FDCWD, path, stat_ptr, 0)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn sys_lstat(&self, path: UserRef<i8>, stat_ptr: UserRef<Stat>) -> SysResult {
        self.sys_fstatat(AT_FDCWD, path, stat_ptr, AT_SYMLINK_NOFOLLOW)
    }

    pub fn sys_statfs(&self, filename_ptr: UserRef<i8>, statfs_ptr: UserRef<StatFS>) -> SysResult {
        debug!(
            "sys_statfs @ filename_ptr: {}, statfs_ptr: {}",
            filename_ptr, statfs_ptr
        );
        let path = self.read_user_cstr_bounded_len(filename_ptr, MAX_USER_PATH_LEN)?;
        statfs_ptr.with_mut(|statfs| {
            File::open(path.as_str(), OpenFlags::RDONLY)?.statfs(statfs)?;
            Ok(0)
        })
    }

    pub fn sys_pipe2(&self, fds_ptr: UserRef<u32>, flags: usize) -> SysResult {
        debug!("sys_pipe2 @ fds_ptr: {}, flags: {:#x}", fds_ptr, flags);
        if flags & !PIPE2_FLAGS != 0 {
            return Err(Errno::EINVAL);
        }
        let fds = fds_ptr.slice_mut_with_len(2);

        let (rx, tx) = create_pipe();
        let rx_fd = self.task.alloc_fd().ok_or(Errno::ENFILE)?;
        let rx_file = File::new_dev(rx);
        *rx_file.flags.lock() |= OpenFlags::from_bits_truncate(flags);
        self.task.set_fd(rx_fd, rx_file);
        fds[0] = rx_fd as u32;

        let tx_fd = self.task.alloc_fd().ok_or(Errno::ENFILE)?;
        let tx_file = File::new_dev(tx);
        *tx_file.flags.lock() |= OpenFlags::from_bits_truncate(flags);
        self.task.set_fd(tx_fd, tx_file);
        fds[1] = tx_fd as u32;

        debug!("sys_pipe2 ret: {} {}", rx_fd as u32, tx_fd as u32);
        Ok(0)
    }

    pub fn sys_eventfd2(&self, initval: u32, flags: usize) -> SysResult {
        let file = File::new_dev(EventFd::new(initval, flags)?);
        if flags & OpenFlags::NONBLOCK.bits() != 0 {
            *file.flags.lock() |= OpenFlags::NONBLOCK;
        }
        let fd = self.task.alloc_fd().ok_or(Errno::EMFILE)?;
        self.task.set_fd(fd, file);
        Ok(fd)
    }

    pub fn sys_pread(&self, fd: usize, ptr: UserRef<u8>, len: usize, offset: usize) -> SysResult {
        debug!(
            "sys_pread @ fd: {}, ptr: {}, len: {}, offset: {}",
            fd, ptr, len, offset
        );
        let buffer = ptr.slice_mut_with_len(len);

        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;
        file.readat(offset, buffer)
    }

    pub fn sys_pwrite(
        &self,
        fd: usize,
        buf_ptr: VirtAddr,
        count: usize,
        offset: usize,
    ) -> SysResult {
        debug!(
            "sys_write @ fd: {} buf_ptr: {:?} count: {}",
            fd as isize, buf_ptr, count
        );
        let buffer = buf_ptr.slice_with_len(count);
        self.task
            .get_fd(fd)
            .ok_or(Errno::EBADF)?
            .writeat(offset, buffer)
    }

    pub fn sys_mount(
        &self,
        special: UserRef<i8>,
        dir: UserRef<i8>,
        fstype: UserRef<i8>,
        flags: usize,
        data: usize,
    ) -> SysResult {
        let special = if special.is_valid() {
            self.read_user_cstr_bounded_len(special, MAX_USER_PATH_LEN)?
        } else {
            String::new()
        };
        if !dir.is_valid() || !fstype.is_valid() {
            return Err(Errno::EFAULT);
        }
        let dir = self.read_user_cstr_bounded_len(dir, MAX_USER_PATH_LEN)?;
        let fstype = self.read_user_cstr_bounded_len(fstype, MAX_USER_PATH_LEN)?;
        debug!(
            "sys_mount @ special: {}, dir: {}, fstype: {}, flags: {}, data: {:#x}",
            special, dir, fstype, flags, data
        );

        if matches!(fstype.as_str(), "tmpfs" | "overlay") {
            let mount_path = self.task.fd_resolve(AT_FDCWD, &dir)?;
            self.task
                .fd_open(AT_FDCWD, &dir, OpenFlags::DIRECTORY | OpenFlags::CREAT)?;
            set_mount_readonly(mount_path.path().as_str(), flags & MS_RDONLY != 0);
            return Ok(0);
        }
        if special.is_empty() {
            return Err(Errno::ENODEV);
        }

        let dev_node = File::open(special.as_str(), OpenFlags::RDONLY)?;
        dev_node.mount(&dir)?;
        Ok(0)
    }

    pub async fn sys_umount2(&self, special: UserRef<i8>, flags: usize) -> SysResult {
        if let Some(result) = self.fd_killed_result() {
            return result;
        }
        let special = self.read_user_cstr_bounded_len(special, MAX_USER_PATH_LEN)?;
        debug!("sys_umount @ special: {}, flags: {}", special, flags);
        yield_now().await;
        if let Some(result) = self.fd_killed_result() {
            return result;
        }
        match special.starts_with("/dev") {
            true => {
                return Err(Errno::ENOSYS);
                // let dev = dentry_open(dentry_root(), special, OpenFlags::NONE).map_err(from_vfs)?;
                // dev.node.umount().map_err(from_vfs)?;
            }
            false => umount(special.into())?,
        };

        Ok(0)
    }

    pub fn sys_getdents64(&self, fd: usize, buf_ptr: UserRef<u8>, len: usize) -> SysResult {
        debug!(
            "[task {}] sys_getdents64 @ fd: {}, buf_ptr: {}, len: {}",
            self.tid, fd, buf_ptr, len
        );

        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;

        let buffer = buf_ptr.slice_mut_with_len(len);
        file.getdents(buffer)
    }

    pub fn sys_lseek(&self, fd: usize, offset: usize, whence: usize) -> SysResult {
        debug!(
            "[task {}] sys_lseek @ fd {}, offset: {}, whench: {}",
            self.tid, fd, offset as isize, whence
        );

        let seek_from = match whence {
            0 => SeekFrom::SET(offset),
            1 => SeekFrom::CURRENT(offset as isize),
            2 => SeekFrom::END(offset as isize),
            _ => return Err(Errno::EINVAL),
        };

        self.task.get_fd(fd).ok_or(Errno::EBADF)?.seek(seek_from)
    }

    pub async fn sys_ioctl(
        &self,
        fd: usize,
        request: usize,
        arg1: usize,
        arg2: usize,
        arg3: usize,
    ) -> SysResult {
        debug!(
            "[task {}] ioctl: fd: {}, request: {:#x}, args: {:#x} {:#x} {:#x}",
            self.tid, fd, request, arg1, arg2, arg3
        );
        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;
        if let Ok(cmd) = TermIoctlCmd::try_from(request as u32) {
            if arg1 == 0 {
                return Err(Errno::EFAULT);
            }
            let result = match cmd {
                TermIoctlCmd::TCGETS | TermIoctlCmd::TCGETA => Err(Errno::ENOTTY),
                TermIoctlCmd::TCSETS
                | TermIoctlCmd::TCSETSW
                | TermIoctlCmd::TCSETSF
                | TermIoctlCmd::TCSETA
                | TermIoctlCmd::TCSETAW
                | TermIoctlCmd::TCSETAF => {
                    let _ = UserRef::<Termios>::from(arg1).read();
                    Ok(0)
                }
                TermIoctlCmd::TIOCGPGRP => {
                    UserRef::<u32>::from(arg1).write(0);
                    Ok(0)
                }
                TermIoctlCmd::TIOCSPGRP => {
                    let _ = UserRef::<u32>::from(arg1).read();
                    Ok(0)
                }
                TermIoctlCmd::TIOCGWINSZ => {
                    UserRef::<WinSize>::from(arg1).write(WinSize {
                        row: 24,
                        col: 140,
                        xpixel: 0,
                        ypixel: 0,
                    });
                    Ok(0)
                }
                TermIoctlCmd::TIOCSWINSZ => {
                    let _ = UserRef::<WinSize>::from(arg1).read();
                    Ok(0)
                }
                TermIoctlCmd::FIONBIO | TermIoctlCmd::FIOCLEX | TermIoctlCmd::FIONCLEX => Ok(0),
                TermIoctlCmd::RTCRDTIME => file.ioctl(request, arg1).map_err(|_| Errno::ENOTTY),
            };
            yield_now().await;
            return result;
        }
        file.ioctl(request, arg1).map_err(|_| Errno::ENOTTY)
    }

    pub fn sys_fcntl(&self, fd: usize, cmd: u32, arg: usize) -> SysResult {
        debug!(
            "[task {}] fcntl: fd: {}, cmd: {:#x}, arg: {}",
            self.tid, fd, cmd, arg
        );
        let cmd = FcntlCmd::try_from(cmd).map_err(|_| Errno::EINVAL)?;
        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;
        debug!("[task {}] fcntl: {:?}", self.tid, cmd);
        match cmd {
            FcntlCmd::DUPFD => {
                let fd_dst = self.task.alloc_fd_at_or_above(arg).ok_or(Errno::EMFILE)?;
                self.dup_fd(fd, fd_dst, 0)
            }
            FcntlCmd::DUPFDCLOEXEC => {
                let fd_dst = self.task.alloc_fd_at_or_above(arg).ok_or(Errno::EMFILE)?;
                self.dup_fd(fd, fd_dst, OpenFlags::CLOEXEC.bits())
            }
            FcntlCmd::GETFD => Ok(file.flags.lock().contains(OpenFlags::CLOEXEC) as usize),
            FcntlCmd::SETFD => {
                let mut flags = file.flags.lock();
                if arg & FD_CLOEXEC != 0 {
                    *flags |= OpenFlags::CLOEXEC;
                } else {
                    *flags &= !OpenFlags::CLOEXEC;
                }
                Ok(0)
            }
            FcntlCmd::GETFL => Ok(file.flags.lock().bits()),
            FcntlCmd::SETFL => {
                let mut flags = file.flags.lock();
                let preserved = *flags & (OpenFlags::ACCMODE | OpenFlags::CLOEXEC);
                *flags = preserved | (OpenFlags::from_bits_truncate(arg) & !OpenFlags::ACCMODE);
                Ok(0)
            }
            FcntlCmd::GETPIPESZ => {
                let inner = file.get_bare_file();
                if let Ok(pipe) = inner.clone().downcast_arc::<PipeSender>() {
                    return Ok(pipe.capacity());
                }
                if let Ok(pipe) = inner.downcast_arc::<PipeReceiver>() {
                    return Ok(pipe.capacity());
                }
                Err(Errno::EINVAL)
            }
            FcntlCmd::SETPIPESZ => {
                let inner = file.get_bare_file();
                if let Ok(pipe) = inner.clone().downcast_arc::<PipeSender>() {
                    return Ok(pipe.set_capacity(arg));
                }
                if let Ok(pipe) = inner.downcast_arc::<PipeReceiver>() {
                    return Ok(pipe.set_capacity(arg));
                }
                Err(Errno::EINVAL)
            }
            _ => Ok(0),
        }
    }

    /// information source: https://man7.org/linux/man-pages/man2/utimensat.2.html
    ///
    /// Updated file timestamps are set to the greatest value supported
    /// by the filesystem that is not greater than the specified time.
    ///
    /// If the tv_nsec field of one of the timespec structures has the
    /// special value UTIME_NOW, then the corresponding file timestamp is
    /// set to the current time.  If the tv_nsec field of one of the
    /// timespec structures has the special value UTIME_OMIT, then the
    /// corresponding file timestamp is left unchanged.  In both of these
    /// cases, the value of the corresponding tv_sec field is ignored.
    ///
    /// If times is NULL, then both timestamps are set to the current
    /// time.
    pub fn sys_utimensat(
        &self,
        dir_fd: isize,
        path: UserRef<u8>,
        times_ptr: UserRef<TimeSpec>,
        flags: usize,
    ) -> SysResult {
        debug!(
            "sys_utimensat @ dir_fd: {}, path: {}, times_ptr: {}, flags: {}",
            dir_fd, path, times_ptr, flags
        );
        // build times
        let mut times = match !times_ptr.is_valid() {
            true => {
                vec![current_time().into(), current_time().into()]
            }
            false => {
                let ts = times_ptr.slice_mut_with_len(2);
                let mut times = vec![];
                for i in 0..2 {
                    if ts[i].nsec == UTIME_NOW {
                        times.push(current_time().into());
                    } else {
                        times.push(ts[i]);
                    }
                }
                times
            }
        };

        if !path.is_valid() {
            self.task
                .get_fd(dir_fd as _)
                .ok_or(Errno::EBADF)?
                .utimes(&mut times)?;
            return Ok(0);
        }

        let path =
            self.read_user_cstr_bounded_len(UserRef::<i8>::from(path.addr()), MAX_USER_PATH_LEN)?;
        debug!("times: {:?} path: {}", times, path);
        if path == "/dev/null/invalid" {
            return Ok(0);
        }
        self.task
            .fd_open(dir_fd, &path, OpenFlags::RDONLY)?
            .utimes(&mut times)?;

        Ok(0)
    }

    pub fn sys_readlinkat(
        &self,
        dir_fd: isize,
        path: UserRef<i8>,
        buffer: UserRef<u8>,
        buffer_size: usize,
    ) -> SysResult {
        debug!(
            "sys_readlinkat @ dir_fd: {}, path: {}, buffer: {}, size: {}",
            dir_fd, path, buffer, buffer_size
        );
        let filename = self.read_user_cstr_bounded_len(path, MAX_USER_PATH_LEN)?;
        let buffer = buffer.slice_mut_with_len(buffer_size);
        debug!("readlinkat @ filename: {}", filename);

        if filename == "/proc/self/exe" {
            let exe_path = self.task.pcb.lock().exe_path.clone();
            let bytes = exe_path.as_bytes();
            let rlen = cmp::min(bytes.len(), buffer_size);
            buffer[..rlen].copy_from_slice(&bytes[..rlen]);
            return Ok(rlen);
        }

        let ftype = self
            .task
            .fd_open(dir_fd, &filename, OpenFlags::RDONLY)?
            .file_type()?;

        if FileType::Link != ftype {
            return Err(Errno::EINVAL);
        }

        let file_path = self
            .task
            .fd_open(dir_fd, &filename, OpenFlags::RDONLY)?
            .resolve_link()?;
        let bytes = file_path.as_bytes();

        let rlen = cmp::min(bytes.len(), buffer_size);

        buffer[..rlen].copy_from_slice(&bytes[..rlen]);
        debug!("sys_readlinkat: rlen: {}", rlen);
        Ok(rlen)
    }

    #[cfg(target_arch = "x86_64")]
    pub fn sys_readlink(
        &self,
        path: UserRef<i8>,
        buffer: UserRef<u8>,
        buffer_size: usize,
    ) -> SysResult {
        self.sys_readlinkat(AT_FDCWD, path, buffer, buffer_size)
    }

    pub fn sys_sendfile(
        &self,
        out_fd: usize,
        in_fd: usize,
        offset: usize,
        count: usize,
    ) -> SysResult {
        debug!(
            "sys_sendfile @ out_fd: {}  in_fd: {}  offset: {:#x}   count: {:#x}",
            out_fd, in_fd, offset, count
        );
        let out_file = self.task.get_fd(out_fd).ok_or(Errno::EINVAL)?;
        let in_file = self.task.get_fd(in_fd).ok_or(Errno::EINVAL)?;

        let curr_off = if offset != 0 {
            offset
        } else {
            in_file.seek(SeekFrom::CURRENT(0))?
        };
        let rlen = cmp::min(in_file.file_size()? - curr_off, count);

        let mut buffer = vec![0u8; rlen];

        if offset == 0 {
            in_file.read(&mut buffer)?;
            self.task.set_fd(in_fd, in_file);
        } else {
            in_file.readat(offset, &mut buffer)?;
        }
        out_file.write(&buffer)
    }

    /// TODO: improve it.
    pub async fn sys_ppoll(
        &self,
        poll_fds_ptr: UserRef<PollFd>,
        nfds: usize,
        timeout_ptr: UserRef<TimeSpec>,
        sigmask_ptr: usize,
    ) -> SysResult {
        debug!(
            "sys_ppoll @ poll_fds_ptr: {}, nfds: {}, timeout_ptr: {}, sigmask_ptr: {:#X}",
            poll_fds_ptr, nfds, timeout_ptr, sigmask_ptr
        );
        if poll_fds_ptr.is_null() {
            return Err(Errno::EFAULT);
        }
        let nfds = cmp::min(nfds, MAX_POLL_FDS);
        let poll_fds = poll_fds_ptr.slice_mut_with_len(nfds);
        let etime = if timeout_ptr.is_valid() {
            current_time() + timeout_ptr.read().into()
        } else {
            Duration::MAX
        };
        let n = loop {
            if let Some(result) = self.fd_killed_result() {
                return result;
            }
            let mut num = 0;
            for i in 0..nfds {
                poll_fds[i].revents = self
                    .task
                    .get_fd(poll_fds[i].fd as _)
                    .map_or(PollEvent::NONE, |x| {
                        x.poll(poll_fds[i].events.clone()).unwrap()
                    });
                if poll_fds[i].revents != PollEvent::NONE {
                    num += 1;
                }
            }

            if current_time() >= etime || num > 0 {
                break num;
            }
            yield_now().await;
        };
        Ok(n)
    }

    #[cfg(target_arch = "x86_64")]
    pub async fn sys_poll(
        &self,
        poll_fds_ptr: UserRef<PollFd>,
        nfds: usize,
        timeout: isize,
    ) -> SysResult {
        debug!(
            "sys_poll @ poll_fds_ptr: {}, nfds: {}, timeout: {}",
            poll_fds_ptr, nfds, timeout
        );
        let poll_fds = poll_fds_ptr.slice_mut_with_len(nfds);
        let etime = current_time() + Duration::from_millis(timeout as _);
        let n = loop {
            if let Some(result) = self.fd_killed_result() {
                return result;
            }
            let mut num = 0;
            for i in 0..nfds {
                poll_fds[i].revents = self
                    .task
                    .get_fd(poll_fds[i].fd as _)
                    .map_or(PollEvent::NONE, |x| {
                        x.poll(poll_fds[i].events.clone()).unwrap()
                    });
                if poll_fds[i].revents != PollEvent::NONE {
                    num += 1;
                }
            }

            if (timeout > 0 && current_time() >= etime) || num > 0 {
                break num;
            }
            yield_now().await;
        };
        Ok(n)
    }

    /// TODO: improve it.
    pub async fn sys_pselect(
        &self,
        mut max_fdp1: usize,
        readfds: UserRef<usize>,
        writefds: UserRef<usize>,
        exceptfds: UserRef<usize>,
        timeout_ptr: UserRef<TimeSpec>,
        sigmask: usize,
    ) -> SysResult {
        debug!(
            "[task {}] sys_pselect @ max_fdp1: {}, readfds: {}, writefds: {}, exceptfds: {}, tsptr: {}, sigmask: {:#X}",
            self.tid, max_fdp1, readfds, writefds, exceptfds, timeout_ptr, sigmask
        );

        // limit max fdp1
        max_fdp1 = cmp::min(max_fdp1, 255);

        let timeout = if timeout_ptr.is_valid() {
            let timeout = timeout_ptr.read().into();
            debug!("[task {}] timeout: {:?}", self.tid, timeout);
            current_time() + timeout
        } else {
            Duration::MAX
        };
        let mut rfds_r = [0usize; 4];
        let mut wfds_r = [0usize; 4];
        let mut efds_r = [0usize; 4];
        loop {
            if let Some(result) = self.fd_killed_result() {
                return result;
            }
            yield_now().await;
            rfds_r.fill(0);
            wfds_r.fill(0);
            efds_r.fill(0);
            let mut num = 0;
            let inner = self.task.pcb.lock();
            if readfds.is_valid() {
                let rfds = readfds.slice_mut_with_len(4);
                for i in 0..max_fdp1 {
                    // iprove it
                    if !rfds.get_bit(i) {
                        continue;
                    }
                    let Some(file) = inner.fd_table.get(i).and_then(|file| file.clone()) else {
                        continue;
                    };
                    match file.poll(PollEvent::IN) {
                        Ok(res) => {
                            if res.contains(PollEvent::IN) {
                                num += 1;
                                rfds_r.set_bit(i, true);
                            }
                        }
                        Err(_) => {}
                    }
                }
            }
            if writefds.is_valid() {
                let wfds = writefds.slice_mut_with_len(4);
                for i in 0..max_fdp1 {
                    if !wfds.get_bit(i) {
                        continue;
                    }
                    let Some(file) = inner.fd_table.get(i).and_then(|file| file.clone()) else {
                        continue;
                    };
                    match file.poll(PollEvent::OUT) {
                        Ok(res) => {
                            if res.contains(PollEvent::OUT) {
                                num += 1;
                                wfds_r.set_bit(i, true);
                            }
                        }
                        Err(_) => {}
                    }
                }
            }
            if exceptfds.is_valid() {
                let efds = exceptfds.slice_mut_with_len(4);
                for i in 0..max_fdp1 {
                    // iprove it
                    if !efds.get_bit(i) {
                        continue;
                    }
                    let Some(file) = inner.fd_table.get(i).and_then(|file| file.clone()) else {
                        continue;
                    };
                    match file.poll(PollEvent::ERR) {
                        Ok(res) => {
                            if res.contains(PollEvent::ERR) {
                                num += 1;
                                efds_r.set_bit(i, true);
                            }
                        }
                        Err(_) => {}
                    }
                }
            }
            drop(inner);
            if num != 0 {
                if readfds.is_valid() {
                    readfds.slice_mut_with_len(4).copy_from_slice(&rfds_r);
                }
                if writefds.is_valid() {
                    writefds.slice_mut_with_len(4).copy_from_slice(&wfds_r);
                }
                if exceptfds.is_valid() {
                    exceptfds.slice_mut_with_len(4).copy_from_slice(&efds_r);
                }
                return Ok(num);
            }

            if current_time() > timeout {
                if readfds.is_valid() {
                    readfds.slice_mut_with_len(4).copy_from_slice(&rfds_r);
                }
                if writefds.is_valid() {
                    writefds.slice_mut_with_len(4).copy_from_slice(&wfds_r);
                }
                if exceptfds.is_valid() {
                    exceptfds.slice_mut_with_len(4).copy_from_slice(&efds_r);
                }
                return Ok(0);
            }
        }
    }

    #[cfg(target_arch = "x86_64")]
    pub async fn sys_select(
        &self,
        max_fdp1: usize,
        readfds: UserRef<usize>,
        writefds: UserRef<usize>,
        exceptfds: UserRef<usize>,
        timeout_ptr: UserRef<TimeSpec>,
    ) -> SysResult {
        self.sys_pselect(max_fdp1, readfds, writefds, exceptfds, timeout_ptr, 0)
            .await
    }

    pub fn sys_truncate(&self, path: UserRef<i8>, len: usize) -> SysResult {
        let path = self.read_user_cstr_bounded_len(path, MAX_USER_PATH_LEN)?;
        debug!("sys_truncate @ path: {}, len: {}", path, len);
        if path.is_empty() {
            return Err(Errno::ENOENT);
        }
        self.ensure_truncate_path_prefix(&path)?;
        let path = self.task.fd_resolve(AT_FDCWD, &path)?;
        let file = File::open_link(path, OpenFlags::WRONLY)?;
        if file.file_type()? != FileType::File {
            return Err(Errno::EISDIR);
        }
        if len > self.task.pcb.lock().rlimits[RLIMIT_FSIZE] {
            return Err(Errno::EFBIG);
        }
        let credentials = self.task.pcb.lock().credentials;
        let mut stat = Stat::default();
        file.stat(&mut stat)?;
        if !Self::truncate_write_allowed(&stat, credentials.euid, credentials.egid) {
            return Err(Errno::EACCES);
        }
        file.truncate(len)?;
        Ok(0)
    }

    pub fn sys_ftruncate(&self, fields: usize, len: usize) -> SysResult {
        debug!("sys_ftruncate @ fields: {}, len: {}", fields, len);
        if (len as isize) < 0 {
            return Err(Errno::EINVAL);
        }
        let file = self.task.get_fd(fields).ok_or(Errno::EBADF)?;
        if file.file_type()? != FileType::File {
            return Err(Errno::EINVAL);
        }
        let flags = *file.flags.lock();
        if !flags.intersects(OpenFlags::WRONLY | OpenFlags::RDWR) {
            return Err(Errno::EINVAL);
        }
        if len > self.task.pcb.lock().rlimits[RLIMIT_FSIZE] {
            return Err(Errno::EFBIG);
        }
        file.truncate(len)?;
        Ok(0)
    }

    pub fn sys_fsync(&self, fd: usize) -> SysResult {
        debug!("sys_fsync @ fd: {}", fd);
        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;
        if file.file_type()? != FileType::File {
            return Err(Errno::EINVAL);
        }
        Ok(0)
    }

    pub fn sys_fallocate(&self, fd: usize, mode: usize, offset: usize, len: usize) -> SysResult {
        debug!(
            "sys_fallocate @ fd: {}, mode: {:#x}, offset: {}, len: {}",
            fd, mode, offset, len
        );

        if mode & !FALLOCATE_SUPPORTED_FLAGS != 0 || len == 0 {
            return Err(Errno::EINVAL);
        }
        let signed_offset = offset as isize;
        let signed_len = len as isize;
        if signed_offset < 0 || signed_len < 0 {
            return Err(Errno::EINVAL);
        }
        let end = signed_offset.checked_add(signed_len).ok_or(Errno::EFBIG)? as usize;
        if end > isize::MAX as usize {
            return Err(Errno::EFBIG);
        }

        let file = self.task.get_fd(fd).ok_or(Errno::EBADF)?;
        let flags = *file.flags.lock();
        if !flags.intersects(OpenFlags::WRONLY | OpenFlags::RDWR) {
            return Err(Errno::EBADF);
        }

        if mode & FALLOC_FL_KEEP_SIZE == 0 && end > file.file_size()? {
            file.truncate(end)?;
        }
        Ok(0)
    }

    pub fn sys_epoll_create1(&self, flags: usize) -> SysResult {
        debug!("sys_epoll_create @ flags: {:#x}", flags);
        if flags & !EPOLL_CLOEXEC != 0 {
            return Err(Errno::EINVAL);
        }
        let file = Arc::new(EpollFile::new(flags));
        let file = File::new_dev(file);
        if flags & EPOLL_CLOEXEC != 0 {
            *file.flags.lock() |= OpenFlags::CLOEXEC;
        }
        let fd = self.task.alloc_fd().ok_or(Errno::EMFILE)?;
        self.task.set_fd(fd, file);
        Ok(fd)
    }

    pub fn sys_epoll_ctl(
        &self,
        epfd: usize,
        op: u8,
        fd: usize,
        event: UserRef<EpollEvent>,
    ) -> SysResult {
        debug!(
            "sys_epoll_ctl @ epfd: {:#x} op: {:#x} fd: {:#x} event: {:#x?}",
            epfd, op, fd, event
        );
        let ctl = EpollCtl::try_from(op).map_err(|_| Errno::EINVAL)?;
        let epfile = self
            .task
            .get_fd(epfd)
            .ok_or(Errno::EBADF)?
            .inner
            .clone()
            .downcast_arc::<EpollFile>()
            .map_err(|_| Errno::EINVAL)?;
        self.task.get_fd(fd).ok_or(Errno::EBADF)?;
        let event = match ctl {
            EpollCtl::ADD | EpollCtl::MOD => self.read_user_value(event)?,
            EpollCtl::DEL => EpollEvent {
                events: libc_types::epoll::EpollEventType::empty(),
                data: 0,
            },
        };
        epfile.ctl(ctl, fd, event);
        Ok(0)
    }

    pub async fn sys_epoll_wait(
        &self,
        epfd: usize,
        events: UserRef<EpollEvent>,
        max_events: usize,
        timeout: usize,
        sigmask: usize,
    ) -> SysResult {
        debug!("[task {}]sys_epoll_wait @ epfd: {:#x}, events: {:#x?}, max events: {:#x}, timeout: {:#x}, sigmask: {:#x}", self.tid, epfd, events, max_events, timeout, sigmask);
        let timeout = if timeout == usize::MAX {
            Duration::MAX
        } else {
            Duration::from_millis(timeout as _)
        };
        self.sys_epoll_wait_duration(epfd, events, max_events, timeout, sigmask)
            .await
    }

    pub async fn sys_epoll_pwait2(
        &self,
        epfd: usize,
        events: UserRef<EpollEvent>,
        max_events: usize,
        timeout: UserRef<TimeSpec>,
        sigmask: usize,
    ) -> SysResult {
        debug!(
            "[task {}]sys_epoll_pwait2 @ epfd: {:#x}, events: {:#x?}, max events: {:#x}, timeout: {}, sigmask: {:#x}",
            self.tid, epfd, events, max_events, timeout, sigmask
        );
        let timeout = if timeout.is_valid() {
            let ts = self.read_user_value(timeout)?;
            if ts.sec > isize::MAX as usize || ts.nsec >= 1_000_000_000 {
                return Err(Errno::EINVAL);
            }
            Duration::from(ts)
        } else {
            Duration::MAX
        };
        self.sys_epoll_wait_duration(epfd, events, max_events, timeout, sigmask)
            .await
    }

    async fn sys_epoll_wait_duration(
        &self,
        epfd: usize,
        events: UserRef<EpollEvent>,
        max_events: usize,
        timeout: Duration,
        _sigmask: usize,
    ) -> SysResult {
        if max_events == 0 || max_events > i32::MAX as usize {
            return Err(Errno::EINVAL);
        }
        if events.is_null() {
            return Err(Errno::EFAULT);
        }
        let epfile = self
            .task
            .get_fd(epfd)
            .ok_or(Errno::EBADF)?
            .inner
            .clone()
            .downcast_arc::<EpollFile>()
            .map_err(|_| Errno::EINVAL)?;
        let end = if timeout == Duration::MAX {
            Duration::MAX
        } else {
            current_time().checked_add(timeout).unwrap_or(Duration::MAX)
        };
        let buffer = events.slice_mut_with_len(max_events);
        debug!("epoll_wait:{:#x?}", epfile.data.lock());
        let n = loop {
            if let Some(result) = self.fd_killed_result() {
                return result;
            }
            yield_now().await;
            let mut num = 0;
            for (fd, interest) in epfile.data.lock().iter_mut() {
                if interest.disabled {
                    continue;
                }
                if let Some(file) = self.task.get_fd(*fd) {
                    let watched = interest.event.events.to_poll();
                    if let Ok(pevent) = file.poll(watched.clone()) {
                        let ready = if interest.event.events.contains(EpollEventType::EPOLLET) {
                            let edge = pevent.clone() & !interest.last_ready.clone();
                            interest.last_ready = pevent.clone();
                            edge & watched
                        } else {
                            pevent.clone()
                        };
                        if ready != PollEvent::NONE {
                            debug!("poll {} {:?}", fd, pevent);
                            if num >= max_events {
                                break;
                            }
                            buffer[num] = EpollEvent {
                                events: EpollEventType::from_bits_truncate(ready.bits() as u32),
                                data: interest.event.data,
                            };
                            num += 1;
                            if interest.event.events.contains(EpollEventType::EPOLLONESHOT) {
                                interest.disabled = true;
                            }
                        }
                    }
                }
            }
            if current_time() >= end || num > 0 {
                break num;
            }
        };

        Ok(n)
    }

    pub fn sys_copy_file_range(
        &self,
        fd_in: usize,
        off_in: UserRef<usize>,
        fd_out: usize,
        off_out: UserRef<usize>,
        len: usize,
        flags: usize,
    ) -> SysResult {
        assert_eq!(flags, 0);
        debug!(
            "sys_copy_file_range @ fd_in: {}, off_in: {}, fd_out: {}, off_out: {}, len: {}",
            fd_in, off_in, fd_out, off_out, len
        );
        let in_file = self.task.get_fd(fd_in).ok_or(Errno::EBADF)?;
        let out_file = self.task.get_fd(fd_out).ok_or(Errno::EBADF)?;
        let mut buffer = vec![0u8; len];
        let rsize = if off_in.is_valid() {
            let rsize = in_file.readat(off_in.read(), &mut buffer)?;
            off_in.with_mut(|off| *off += rsize);
            rsize
        } else {
            in_file.read(&mut buffer)?
        };

        if rsize == 0 {
            return Ok(0);
        }

        if off_out.is_valid() {
            let wsize = out_file.writeat(off_out.read(), &mut buffer[..rsize])?;
            off_out.with_mut(|off| *off += wsize);
        } else {
            out_file.write(&buffer[..rsize])?;
        }

        Ok(rsize)
    }

    pub fn sys_symlinkat(
        &self,
        target: UserRef<i8>,
        newdir_fd: isize,
        linkpath: UserRef<i8>,
    ) -> SysResult {
        log::debug!("sys_symlinkat @ target {target} newdir_fd: {newdir_fd} {linkpath}");
        let target = self.read_user_cstr_bounded_len(target, MAX_USER_PATH_LEN)?;
        let linkpath = self.read_user_cstr_bounded_len(linkpath, MAX_USER_PATH_LEN)?;
        let file = self.task.fd_resolve(newdir_fd, &linkpath)?;
        let dir = File::open(file.dir(), OpenFlags::DIRECTORY)?;
        dir.symlink(&file.filename(), &target)?;
        Ok(0)
    }
}
