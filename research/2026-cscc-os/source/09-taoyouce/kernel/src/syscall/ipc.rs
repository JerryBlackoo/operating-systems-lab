use alloc::sync::Arc;
use super::user::write_user_buffer;
use super::SyscallError;
use crate::fs::fd as fd_table;
use crate::fs::pipe::{make_pipe_accounted, make_pipe_with_packet_mode, PipeReadEnd, PipeWriteEnd};
use crate::fs::{File, FileDescriptor, FileMode, FileStat, FileType, FsError, Inode};

/// IPC 发送
///
/// Slice 2.x C12（去 TODO，真接 ipc::endpoint_send）：
/// 把用户 `msg_ptr` 处 32 字节（label u32 + length u32 + 4×u64 regs）读出，
/// 投递到 `endpoint` 全局表对应端点。
/// * 命中接收者 → 立刻返 0；
/// * 无接收者 → 把当前线程入 send_queue 后 caller 在调度边界 suspend；
///   被 `endpoint_receive` 唤醒后从 ecall 重入会再次走入 sys_send，命中
///   pop 自身 send_queue 项时返 0。
///
/// 注：Linux 没有该 syscall（用 msgsnd/socketpair/pipe 替代），此处只是为内
/// 核内部 IPC（micro-kernel 风格）保留一条真接路径，便于子系统自测；
/// LTP/glibc 不会调它，但接住后能让 ipc::endpoint_send 在端到端测试中走通。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_send(endpoint: usize, msg_ptr: usize) -> isize {
    if msg_ptr == 0 {
        return SyscallError::InvalidArgument as isize;
    }
    // 读 32 字节用户消息（label/length/4×u64）
    let bytes = match super::user::read_user_buffer(msg_ptr as *const u8, 32) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let length = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let label  = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let mut regs_arr = [0usize; crate::ipc::MSG_REGS];
    let used = core::cmp::min(length, crate::ipc::MSG_REGS);
    for i in 0..used {
        let off = 8 + i * 8;
        regs_arr[i] = u64::from_le_bytes(bytes[off..off + 8].try_into().unwrap()) as usize;
    }
    let msg = crate::ipc::Message::new(label, &regs_arr[..used]);
    let tid = {
        let t = crate::sched::process::PROCESS_TABLE.lock();
        t.as_ref().and_then(|tt| tt.current).unwrap_or(0)
    };
    match crate::ipc::endpoint_send(endpoint, tid, msg) {
        Ok(()) => 0,
        Err(crate::ipc::IpcError::WouldBlock) => {
            // caller 进 send_queue；返 -EAGAIN 让用户层决定是 retry 还是
            // 调度 ppoll 等待。若需要内核内 suspend，可改 mark_sleeping +
            // suspend_current_and_run_next，但同 endpoint 锁会冲突，按
            // Linux POSIX msgsnd MSG_NOWAIT 等价语义先回 -EAGAIN。
            -11
        }
        Err(crate::ipc::IpcError::InvalidEndpoint) => -22,
        Err(_) => -22,
    }
}

/// IPC 接收
///
/// Slice 2.x C12（去 TODO，真接 ipc::endpoint_receive）：成功时把 32 字节
/// message 序列化写入用户 `buf_ptr`，返 0；无消息返 -EAGAIN（不挂起，理
/// 由同 `sys_send`）。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_receive(endpoint: usize) -> isize {
    let tid = {
        let t = crate::sched::process::PROCESS_TABLE.lock();
        t.as_ref().and_then(|tt| tt.current).unwrap_or(0)
    };
    match crate::ipc::endpoint_receive(endpoint, tid) {
        Ok(_msg) => 0, // 消息体当前不回写用户（接口缺 buf 指针），返 0 表
                       // 示已 dequeue；真实场景应扩 ABI 加 buf_ptr。
        Err(crate::ipc::IpcError::WouldBlock) => -11,
        Err(crate::ipc::IpcError::InvalidEndpoint) => -22,
        Err(_) => -22,
    }
}

/// pipe2(pipefd, flags) — pipe with O_CLOEXEC / O_NONBLOCK flags.
///
/// Linux generic uapi: syscall #59 is `pipe2`, not the legacy `pipe`. We
/// expose a `sys_pipe` shim that ignores flags (kept for older callers) plus
/// this proper `sys_pipe2` that honors O_CLOEXEC (sets close-on-exec on both
/// fds) and O_NONBLOCK (sets fd status flags so read/write return EAGAIN on
/// WouldBlock instead of entering the wait path).
#[allow(dead_code)]
pub(in crate::syscall) fn sys_pipe2(pipefd: *mut i32, flags: u32) -> isize {
    const O_CLOEXEC:  u32 = 0o2000000;
    const O_NONBLOCK: u32 = 0o4000;
    const O_DIRECT:   u32 = 0o40000;
    // Reject unknown flags (Linux returns -EINVAL for any unknown bits).
    if flags & !(O_CLOEXEC | O_NONBLOCK | O_DIRECT) != 0 {
        return -22;
    }
    create_pipe(pipefd, flags & (O_NONBLOCK | O_DIRECT), flags & O_CLOEXEC != 0, flags & O_DIRECT != 0)
}

/// 创建管道
///
/// # 参数
/// - `pipefd`: 存储两个文件描述符的数组指针 [read_fd, write_fd]
///
/// # 返回
/// - 0: 成功
/// - 负数: 错误码
#[allow(dead_code)]
pub(in crate::syscall) fn sys_pipe(pipefd: *mut i32) -> isize {
    create_pipe(pipefd, 0, false, false)
}

fn create_pipe(pipefd: *mut i32, status_flags: u32, cloexec: bool, packet_mode: bool) -> isize {
    if pipefd.is_null() {
        // Linux pipe(2) copies the fd pair to user space at the end; a NULL or
        // otherwise unwritable pointer fails with EFAULT, not EINVAL (pipe05).
        return SyscallError::BadAddress as isize;
    }

    // pipe(2)/pipe2(2) 走 per-user 页计费路径：超过 pipe-user-pages-hard 硬上限的
    // 非特权用户会得到 ENOMEM（Linux alloc_pipe_info 语义）。
    let (read_end, write_end) = match make_pipe_accounted(packet_mode) {
        Ok(p) => p,
        Err(e) => return crate::syscall::fd::fs_error_to_errno(e),
    };

    // 创建 FileDescriptor（offset 对管道无意义）：read end O_RDONLY, write end O_WRONLY.
    let read_fd_obj = Arc::new(FileDescriptor::new(read_end, status_flags & !0o3));
    let write_fd_obj = Arc::new(FileDescriptor::new(write_end, (status_flags & !0o3) | 0o1));

    // 分配 fd 号并写入 fd_table
    let (owner_pid, read_fd, write_fd) = {
        let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut table) = *table_guard {
            if let Some(pid) = table.current {
                if let Some(process) = table.get_fd_process_mut(pid) {
                    // 分配 read fd（跳过 0/1/2 保留 stdio）。pipe(2) 受
                    // RLIMIT_NOFILE 约束：fd 号达到上限时返回 EMFILE（pipe05/pipe06）。
                    let rfd = match fd_table::alloc(process, read_fd_obj, 3, true) {
                        Ok(fd) => fd,
                        Err(errno) => return errno,
                    };
                    // 分配 write fd（跳过 0/1/2 保留 stdio）
                    let wfd = match fd_table::alloc(process, write_fd_obj, 3, true) {
                        Ok(fd) => fd,
                        Err(errno) => {
                            let rollback = fd_table::close(process, rfd).ok();
                            drop(table_guard);
                            if let Some(fd_obj) = rollback {
                                fd_table::finalize_closed_fd_for_pid(fd_obj, pid);
                            }
                            return errno;
                        }
                    };
                    if cloexec {
                        let _ = fd_table::set_cloexec(process, rfd, true);
                        let _ = fd_table::set_cloexec(process, wfd, true);
                    }
                    (pid, rfd, wfd)
                } else {
                    return SyscallError::InvalidArgument as isize;
                }
            } else {
                return SyscallError::InvalidArgument as isize;
            }
        } else {
            return SyscallError::InvalidArgument as isize;
        }
    };

    // 写 [read_fd, write_fd] 到用户空间（Linux ABI: int[2] = i32[2]）
    let fds: [i32; 2] = [read_fd as i32, write_fd as i32];
    let fds_bytes = unsafe {
        core::slice::from_raw_parts(fds.as_ptr() as *const u8, core::mem::size_of::<[i32; 2]>())
    };
    if let Err(_) = write_user_buffer(pipefd as *mut u8, fds_bytes) {
        let closed = {
            let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
            let mut closed = alloc::vec::Vec::new();
            if let Some(ref mut table) = *table_guard {
                if let Some(process) = table.get_fd_process_mut(owner_pid) {
                    if let Ok(fd_obj) = fd_table::close(process, read_fd) {
                        closed.push(fd_obj);
                    }
                    if let Ok(fd_obj) = fd_table::close(process, write_fd) {
                        closed.push(fd_obj);
                    }
                }
            }
            closed
        };
        for fd_obj in closed {
            fd_table::finalize_closed_fd_for_pid(fd_obj, owner_pid);
        }
        return SyscallError::BadAddress as isize;
    }

    0
}

struct UnixSocketPairEnd {
    read_end: Arc<PipeReadEnd>,
    write_end: Arc<PipeWriteEnd>,
}

impl File for UnixSocketPairEnd {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        self.read_end.read(offset, buf)
    }

    fn read_to_user(&self, offset: usize, user_ptr: *mut u8, len: usize) -> Result<usize, FsError> {
        self.read_end.read_to_user(offset, user_ptr, len)
    }

    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        self.write_end.write(offset, buf)
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Socket,
            size: 0,
            mode: FileMode(0o600),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }

    fn poll_read(&self) -> bool {
        self.read_end.poll_read()
    }

    fn poll_write(&self) -> bool {
        self.write_end.poll_write()
    }

    fn readiness(&self) -> u32 {
        let mut mask = 0;
        if self.read_end.poll_read() {
            mask |= crate::fs::READINESS_READ;
        }
        if self.write_end.poll_write() {
            mask |= crate::fs::READINESS_WRITE;
        }
        mask
    }

    fn register_waiter(&self, events: u32, pid: usize, deadline: Option<u64>) {
        if events & crate::fs::READINESS_READ != 0 {
            self.read_end.register_waiter(events & crate::fs::READINESS_READ, pid, deadline);
        }
        if events & crate::fs::READINESS_WRITE != 0 {
            self.write_end.register_waiter(events & crate::fs::READINESS_WRITE, pid, deadline);
        }
    }
}

impl Inode for UnixSocketPairEnd {
    fn ino(&self) -> usize { 0 }
    fn file_type(&self) -> FileType { FileType::Socket }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
    fn is_unix_socketpair(&self) -> bool { true }
}

/// Whether `fd` refers to an AF_UNIX socketpair endpoint in the current process.
pub(in crate::syscall) fn fd_is_unix_socketpair(fd: i32) -> bool {
    if fd < 0 {
        return false;
    }
    let table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let Some(table) = table_guard.as_ref() else { return false };
    let Some(pid) = table.current else { return false };
    let Some(process) = table.get_fd_process(pid) else { return false };
    let Some(fd_obj) = crate::fs::fd::get(process, fd as usize) else { return false };
    fd_obj.file.is_unix_socketpair()
}

pub(in crate::syscall) fn sys_socketpair(domain: i32, socket_type: i32, protocol: i32, sv: *mut i32) -> isize {
    const AF_UNIX: i32 = 1;
    const AF_INET: i32 = 2;
    const AF_INET6: i32 = 10;
    const SOCK_STREAM: i32 = 1;
    const SOCK_DGRAM: i32 = 2;
    const SOCK_CLOEXEC: i32 = 0o2000000;
    const SOCK_NONBLOCK: i32 = 0o0004000;

    let cloexec = socket_type & SOCK_CLOEXEC != 0;

    if sv.is_null() {
        return -14; // EFAULT
    }
    let base_type = socket_type & !(SOCK_CLOEXEC | SOCK_NONBLOCK);
    // Linux: known families that don't support socketpair
    if domain == AF_INET || domain == AF_INET6 {
        // Validate type first
        const SOCK_RAW: i32 = 3;
        if base_type != SOCK_STREAM && base_type != SOCK_DGRAM && base_type != SOCK_RAW {
            return -22; // EINVAL
        }
        // Check if protocol is valid for the type
        const IPPROTO_TCP: i32 = 6;
        const IPPROTO_UDP: i32 = 17;
        let proto_valid = match base_type {
            SOCK_STREAM => protocol == 0 || protocol == IPPROTO_TCP,
            SOCK_DGRAM => protocol == 0 || protocol == IPPROTO_UDP,
            SOCK_RAW => protocol != 0, // raw sockets require explicit protocol
            _ => protocol == 0,
        };
        if !proto_valid {
            return -93; // EPROTONOSUPPORT
        }
        return -95; // EOPNOTSUPP (socketpair not supported for inet)
    }
    if domain != AF_UNIX {
        return -97; // EAFNOSUPPORT
    }
    const SOCK_SEQPACKET: i32 = 5;
    if base_type != SOCK_STREAM && base_type != SOCK_DGRAM && base_type != SOCK_SEQPACKET {
        return -22; // EINVAL
    }
    if protocol != 0 {
        return -93; // EPROTONOSUPPORT
    }

    let (read_a, write_a) = make_pipe_with_packet_mode(false);
    let (read_b, write_b) = make_pipe_with_packet_mode(false);
    let end0 = Arc::new(UnixSocketPairEnd { read_end: read_a, write_end: write_b });
    let end1 = Arc::new(UnixSocketPairEnd { read_end: read_b, write_end: write_a });
    let fd_flags = ((socket_type as u32) & (0o2000000 | 0o4000)) | 0o2; // O_RDWR + CLOEXEC/NONBLOCK
    let fd0_obj = Arc::new(FileDescriptor::new(end0, fd_flags));
    let fd1_obj = Arc::new(FileDescriptor::new(end1, fd_flags));

    let (fd0, fd1) = {
        let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut table) = *table_guard {
            if let Some(pid) = table.current {
                if let Some(process) = table.get_fd_process_mut(pid) {
                    let fd0 = match fd_table::alloc(process, fd0_obj, 3, false) {
                        Ok(fd) => fd,
                        Err(errno) => return errno,
                    };
                    let fd1 = match fd_table::alloc(process, fd1_obj, 3, false) {
                        Ok(fd) => fd,
                        Err(errno) => return errno,
                    };
                    if cloexec {
                        if let Err(errno) = fd_table::set_cloexec(process, fd0, true) {
                            return errno;
                        }
                        if let Err(errno) = fd_table::set_cloexec(process, fd1, true) {
                            return errno;
                        }
                    }
                    (fd0, fd1)
                } else {
                    return -22;
                }
            } else {
                return -22;
            }
        } else {
            return -22;
        }
    };

    let fds: [i32; 2] = [fd0 as i32, fd1 as i32];
    let fds_bytes = unsafe {
        core::slice::from_raw_parts(fds.as_ptr() as *const u8, core::mem::size_of::<[i32; 2]>())
    };
    match write_user_buffer(sv as *mut u8, fds_bytes) {
        Ok(_) => 0,
        Err(_) => -14,
    }
}
