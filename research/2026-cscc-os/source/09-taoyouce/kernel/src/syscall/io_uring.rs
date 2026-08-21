use alloc::sync::Arc;
use alloc::string::String;

use crate::fs::{fd as fd_table, File, FileDescriptor, FileMode, FileStat, FileType, FsError, Inode};
use crate::io_uring::{IoUring, IoUringId};

struct IoUringFdInode {
    id: IoUringId,
}

impl File for IoUringFdInode {
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }

    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::CharDevice,
            size: 0,
            mode: FileMode(0o0600),
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

    fn close_notify_last(&self) {
        let _ = crate::io_uring::destroy(self.id);
    }
}

impl Inode for IoUringFdInode {
    fn ino(&self) -> usize {
        0x1_0a10_0000usize.wrapping_add(self.id.0 as usize)
    }

    fn file_type(&self) -> FileType {
        FileType::CharDevice
    }

    fn as_file(&self) -> Option<&dyn File> {
        Some(self)
    }

    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> {
        None
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        File::stat(self)
    }
}

fn io_uring_path(id: IoUringId) -> String {
    alloc::format!("anon_inode:[io_uring]:{}", id.0)
}

fn parse_io_uring_id(path: &str) -> Option<IoUringId> {
    path.strip_prefix("anon_inode:[io_uring]:")
        .and_then(|id| id.parse::<u32>().ok())
        .map(IoUringId)
}

fn ring_from_fd(fd: usize) -> Option<Arc<IoUring>> {
    let table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = table_guard.as_ref()?;
    let pid = table.current?;
    let process = table.get_fd_process(pid)?;
    let fd_obj = fd_table::get(process, fd)?;
    let id = parse_io_uring_id(fd_obj.path.as_ref()?)?;
    crate::io_uring::get(id)
}

/// io_uring 设置
///
/// Per Linux io_uring_setup(2):
/// - entries: requested SQ size (rounded up to power-of-two)
/// - params_ptr: pointer to struct io_uring_params (in/out)
///   The kernel reads flags/sq_entries/cq_entries from it and writes back
///   sq_off/cq_off/features after ring creation.
#[allow(dead_code)]
pub(in crate::syscall) fn sys_io_uring_setup(entries: u32, params_ptr: *mut u8) -> isize {
    use crate::io_uring::{self, IoUringParams, IoUringSetupFlags};

    if entries == 0 {
        return -22; // EINVAL
    }

    let mut params = IoUringParams::default();
    params.sq_entries = entries;

    // Read user-provided params (flags, cq_entries) if pointer is valid
    if !params_ptr.is_null() {
        // IoUringParams layout: flags at offset 0 (u32), sq_entries at offset 4 (u32),
        // cq_entries at offset 8 (u32). Read at least 12 bytes.
        if let Ok(raw) = super::user::read_user_buffer(params_ptr as *const u8, 12) {
            let flags = u32::from_ne_bytes(raw[0..4].try_into().unwrap());
            // sq_entries already set from the `entries` argument (Linux convention)
            let cq_entries = u32::from_ne_bytes(raw[8..12].try_into().unwrap());

            params.flags = IoUringSetupFlags::from_bits_truncate(flags);
            if cq_entries > 0 {
                params.cq_entries = cq_entries;
            }
        }
    }

    match io_uring::setup(&mut params) {
        Ok(ring) => {
            let id = ring.id();
            let inode = Arc::new(IoUringFdInode { id });
            const O_RDWR: u32 = 0o2;
            const O_CLOEXEC: u32 = 0o2000000;
            let fd_obj = Arc::new(FileDescriptor::new_with_path(
                inode,
                O_RDWR | O_CLOEXEC,
                Some(io_uring_path(id)),
            ));
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            let Some(table) = table.as_mut() else {
                let _ = io_uring::destroy(id);
                return -23;
            };
            let Some(pid) = table.current else {
                let _ = io_uring::destroy(id);
                return -23;
            };
            let Some(process) = table.get_fd_process_mut(pid) else {
                let _ = io_uring::destroy(id);
                return -23;
            };
            match fd_table::alloc(process, fd_obj, 0, true) {
                Ok(fd) => {
                    let _ = fd_table::set_cloexec(process, fd, true);
                    fd as isize
                }
                Err(errno) => {
                    let _ = io_uring::destroy(id);
                    errno
                }
            }
        }
        Err(_) => -22,
    }
}

/// io_uring 进入 - 提交 SQE 并获取 CQE
#[allow(dead_code)]
pub(in crate::syscall) fn sys_io_uring_enter(
    fd: usize,
    sqes_ptr: *const u8,
    to_submit: u32,
    cqes_ptr: *mut u8,
    cqe_size: u32,
    min_complete: u32
) -> isize {
    use crate::io_uring::{IoUringEnterFlags, IoUringOp, SubmissionQueueEntry, SqeFlags};
    use crate::sched::process::PROCESS_TABLE;
    use crate::mm::{VirtAddr, VirtPageNum};

    match ring_from_fd(fd) {
        Some(ring) => {
            // 用户空间 SQE 结构（与 IoUringSqe 匹配）
            #[repr(C)]
            #[derive(Clone, Copy)]
            struct UserSqe {
                opcode: u8,
                flags: u8,
                ioprio: u16,
                fd: i32,
                off: u64,
                addr: u64,
                len: u32,
                op_flags: u32,
                user_data: u64,
                buf_index: u16,
                personality: u16,
                splice_fd_in: i32,
                _pad: [u64; 2],
            }

            // 用户空间 CQE 结构（与 IoUringCqe 匹配）
            #[repr(C)]
            #[derive(Clone, Copy)]
            struct UserCqe {
                user_data: u64,
                res: i32,
                flags: u32,
            }

            let sqe_size = core::mem::size_of::<UserSqe>();
            let mut submitted = 0u32;

            // 获取当前进程的页表用于地址翻译：scope the borrow
            {
                let table_guard = PROCESS_TABLE.lock();
                let table = match table_guard.as_ref() {
                    Some(t) => t,
                    None => return -1,
                };
                let current_pid = match table.current {
                    Some(pid) => pid,
                    None => return -1,
                };
                let as_pid = match table.get(current_pid) {
                    Some(p) => if p.mm.is_some() { current_pid } else { p.tgid },
                    None => return -1,
                };
                let mm = match table.get(as_pid).and_then(|p| p.mm.as_ref()) {
                    Some(m) => m.clone(),
                    None => return -1,
                };
                let address_space = mm.lock();
                let page_table = address_space.page_table();

                // 从用户空间读取 SQE 并处理
                for i in 0..to_submit {
                    let sqe_user_addr = sqes_ptr as usize + (i as usize * sqe_size);

                    // 通过页表翻译读取用户空间的 SQE
                    let mut sqe_bytes = [0u8; 64]; // UserSqe 大小
                    let mut valid = true;

                    for j in 0..sqe_size {
                        let va = VirtAddr(sqe_user_addr + j);
                        let vpn = va.floor();
                        let offset = va.page_offset();

                        if let Some(ppn) = page_table.translate(vpn) {
                            let kva = super::ppn_offset_to_kernel_ptr(ppn, offset);
                            sqe_bytes[j] = unsafe { *(kva as *const u8) };
                        } else {
                            valid = false;
                            break;
                        }
                    }

                    if !valid {
                        break;
                    }

                    // 将字节转换为 UserSqe
                    let user_sqe: UserSqe = unsafe {
                        core::ptr::read(sqe_bytes.as_ptr() as *const UserSqe)
                    };

                    // 转换为内核 SQE
                    let kernel_sqe = SubmissionQueueEntry {
                        opcode: IoUringOp::from_u8(user_sqe.opcode),
                        flags: SqeFlags::from_bits_truncate(user_sqe.flags),
                        ioprio: user_sqe.ioprio,
                        fd: user_sqe.fd,
                        off: user_sqe.off,
                        addr: user_sqe.addr,
                        len: user_sqe.len,
                        op_flags: user_sqe.op_flags,
                        user_data: user_sqe.user_data,
                        buf_index: user_sqe.buf_index,
                        personality: user_sqe.personality,
                        file_index: user_sqe.splice_fd_in,
                        __pad: user_sqe._pad,
                    };

                    // 将 SQE 推入内核队列
                    if ring.push_sqe(kernel_sqe).is_ok() {
                        submitted += 1;
                    } else {
                        break; // 队列满
                    }
                }
            } // table_guard + address_space locks released here

            // 处理提交的请求
            let enter_flags = IoUringEnterFlags::empty();
            match ring.enter(submitted, 0, enter_flags) {
                Ok(_) => {}
                Err(e) => {
                    return e.to_errno() as isize;
                }
            }

            // 将完成的 CQE 写回用户空间
            let cqe_struct_size = core::mem::size_of::<UserCqe>();
            let mut completed = 0u32;

            // 等待至少 min_complete 个完成
            let mut spin_count = 0u32;
            while completed < min_complete && completed < cqe_size {
                if let Some(cqe) = ring.pop_cqe() {
                    let cqe_user_addr = cqes_ptr as usize + (completed as usize * cqe_struct_size);

                    // 构造 CQE 字节
                    let user_cqe = UserCqe {
                        user_data: cqe.user_data,
                        res: cqe.res,
                        flags: cqe.flags,
                    };
                    let cqe_bytes: [u8; 16] = unsafe {
                        core::mem::transmute(user_cqe)
                    };

                    // 通过页表翻译写入用户空间
                    {
                        let table_guard = PROCESS_TABLE.lock();
                        if let Some(table) = table_guard.as_ref() {
                            if let Some(current_pid) = table.current {
                                let as_pid = table.get(current_pid)
                                    .map(|p| if p.mm.is_some() { current_pid } else { p.tgid })
                                    .unwrap_or(current_pid);
                                if let Some(mm) = table.get(as_pid).and_then(|p| p.mm.as_ref()).cloned() {
                                    let address_space = mm.lock();
                                    let page_table = address_space.page_table();

                                    for j in 0..cqe_struct_size {
                                        let va = VirtAddr(cqe_user_addr + j);
                                        let vpn = va.floor();
                                        let offset = va.page_offset();

                                        if let Some(ppn) = page_table.translate(vpn) {
                                            let kva = super::ppn_offset_to_kernel_ptr(ppn, offset);
                                            unsafe { *(kva as *mut u8) = cqe_bytes[j]; }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    completed += 1;
                    spin_count = 0;
                } else if completed > 0 {
                    // 已经有一些完成，不再等待
                    break;
                } else {
                    // 没有完成事件，短暂等待
                    spin_count += 1;
                    if spin_count > 10000 {
                        // 防止无限等待，超时返回
                        break;
                    }
                    core::hint::spin_loop();
                }
            }

            // 返回完成的数量
            completed as isize
        }
        None => {
            -9 // EBADF
        }
    }
}

/// io_uring 注册 — 真按 Linux `io_uring/register.c` opcode 表分发。
///
/// 我们没有真 zero-copy 缓冲 / 真 sqpoll，但严格做 opcode 合法性 +
/// 参数边界校验，让 LTP `io_uring_register0?.c` 这类 negative-path 用例
/// 拿到正确的 `-EINVAL/-EFAULT/-EBUSY`，而不是 silent 0 误判 PASS。
#[allow(dead_code)]
pub(in crate::syscall) fn sys_io_uring_register(fd: usize, opcode: u32, _arg: *const u8, nr_args: u32) -> isize {
    // Linux 5.19+ opcode 集（`include/uapi/linux/io_uring.h`）。
    const REG_BUFFERS:           u32 = 0;
    const UNREG_BUFFERS:         u32 = 1;
    const REG_FILES:             u32 = 2;
    const UNREG_FILES:           u32 = 3;
    const REG_EVENTFD:           u32 = 4;
    const UNREG_EVENTFD:         u32 = 5;
    const REG_FILES_UPDATE:      u32 = 6;
    const REG_EVENTFD_ASYNC:     u32 = 7;
    const REG_PROBE:             u32 = 8;
    const REG_PERSONALITY:       u32 = 9;
    const UNREG_PERSONALITY:     u32 = 10;
    const REG_RESTRICTIONS:      u32 = 11;
    const REG_ENABLE_RINGS:      u32 = 12;
    const REG_FILES2:            u32 = 13;
    const REG_FILES_UPDATE2:     u32 = 14;
    const REG_BUFFERS2:          u32 = 15;
    const REG_BUFFERS_UPDATE:    u32 = 16;
    const REG_IOWQ_AFF:          u32 = 17;
    const UNREG_IOWQ_AFF:        u32 = 18;
    const REG_IOWQ_MAX_WORKERS:  u32 = 19;
    const REG_RING_FDS:          u32 = 20;
    const UNREG_RING_FDS:        u32 = 21;
    const REG_PBUF_RING:         u32 = 22;
    const UNREG_PBUF_RING:       u32 = 23;
    const REG_SYNC_CANCEL:       u32 = 24;
    const REG_FILE_ALLOC_RANGE:  u32 = 25;
    // Bit IORING_REGISTER_USE_REGISTERED_RING — 不影响 opcode 编号。

    let ring = match ring_from_fd(fd) {
        Some(r) => r,
        None => return -9, // -EBADF
    };

    // Per Linux io_uring_register(2): dispatch by opcode.
    match opcode {
        REG_BUFFERS | REG_BUFFERS2 => {
            if nr_args == 0 { return -22; }
            if nr_args > (1 << 14) { return -22; }
            if _arg.is_null() { return -14; } // -EFAULT
            // Parse iovec array from userspace: each entry is { *void iov_base, size_t iov_len }
            // On rv64: sizeof(struct iovec) = 16 (8+8)
            let iov_size = 16usize;
            let total = nr_args as usize * iov_size;
            let raw = match super::user::read_user_buffer(_arg, total) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let mut bufs = alloc::vec::Vec::with_capacity(nr_args as usize);
            for i in 0..nr_args as usize {
                let off = i * iov_size;
                let addr = usize::from_ne_bytes(raw[off..off+8].try_into().unwrap());
                let len = usize::from_ne_bytes(raw[off+8..off+16].try_into().unwrap());
                bufs.push((addr, len));
            }
            match ring.register_buffers(&bufs) {
                Ok(()) => 0,
                Err(e) => e.to_errno() as isize,
            }
        }
        REG_BUFFERS_UPDATE => {
            if nr_args == 0 { return -22; }
            if nr_args > (1 << 14) { return -22; }
            // Treat as re-register: unregister then register
            let _ = ring.unregister_buffers();
            if _arg.is_null() { return -14; }
            let iov_size = 16usize;
            let total = nr_args as usize * iov_size;
            let raw = match super::user::read_user_buffer(_arg, total) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let mut bufs = alloc::vec::Vec::with_capacity(nr_args as usize);
            for i in 0..nr_args as usize {
                let off = i * iov_size;
                let addr = usize::from_ne_bytes(raw[off..off+8].try_into().unwrap());
                let len = usize::from_ne_bytes(raw[off+8..off+16].try_into().unwrap());
                bufs.push((addr, len));
            }
            match ring.register_buffers(&bufs) {
                Ok(()) => 0,
                Err(e) => e.to_errno() as isize,
            }
        }
        UNREG_BUFFERS => {
            if nr_args != 0 { return -22; }
            match ring.unregister_buffers() {
                Ok(()) => 0,
                Err(e) => e.to_errno() as isize,
            }
        }
        REG_FILES | REG_FILES2 => {
            if nr_args == 0 || nr_args > 32_768 { return -22; }
            if _arg.is_null() { return -14; }
            // Parse i32 fd array from userspace
            let total = nr_args as usize * 4;
            let raw = match super::user::read_user_buffer(_arg, total) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let mut fds = alloc::vec::Vec::with_capacity(nr_args as usize);
            for i in 0..nr_args as usize {
                let off = i * 4;
                let fd_val = i32::from_ne_bytes(raw[off..off+4].try_into().unwrap());
                fds.push(fd_val);
            }
            match ring.register_files(&fds) {
                Ok(()) => 0,
                Err(e) => e.to_errno() as isize,
            }
        }
        REG_FILES_UPDATE | REG_FILES_UPDATE2 => {
            if nr_args == 0 || nr_args > 32_768 { return -22; }
            if _arg.is_null() { return -14; }
            // struct io_uring_files_update { u32 offset; u32 resv; i32[] fds; }
            // For simplicity: arg points to { u32 offset, u32 resv, fds[nr_args] }
            let hdr_size = 8usize; // offset(4) + resv(4)
            let total = hdr_size + nr_args as usize * 4;
            let raw = match super::user::read_user_buffer(_arg, total) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let offset = u32::from_ne_bytes(raw[0..4].try_into().unwrap());
            let mut fds = alloc::vec::Vec::with_capacity(nr_args as usize);
            for i in 0..nr_args as usize {
                let off = hdr_size + i * 4;
                let fd_val = i32::from_ne_bytes(raw[off..off+4].try_into().unwrap());
                fds.push(fd_val);
            }
            match ring.update_files(offset, &fds) {
                Ok(n) => n as isize,
                Err(e) => e.to_errno() as isize,
            }
        }
        UNREG_FILES => {
            if nr_args != 0 { return -22; }
            match ring.unregister_files() {
                Ok(()) => 0,
                Err(e) => e.to_errno() as isize,
            }
        }
        REG_EVENTFD | REG_EVENTFD_ASYNC => {
            if nr_args != 1 { return -22; }
            -38
        }
        UNREG_EVENTFD => {
            if nr_args != 0 { return -22; }
            0
        }
        REG_PROBE => {
            // arg 写出 io_uring_probe 结构。我们暂不公布 op map，返 -ENOSYS。
            if nr_args == 0 || nr_args > 256 { return -22; }
            -38
        }
        REG_PERSONALITY => {
            if nr_args != 0 { return -22; }
            -38
        }
        UNREG_PERSONALITY => {
            // nr_args 是 personality id
            if nr_args == 0 { return -22; }
            -38
        }
        REG_RESTRICTIONS | REG_ENABLE_RINGS => -38,
        REG_IOWQ_AFF | UNREG_IOWQ_AFF | REG_IOWQ_MAX_WORKERS => {
            if opcode == REG_IOWQ_MAX_WORKERS && nr_args != 2 { return -22; }
            -38
        }
        REG_RING_FDS | UNREG_RING_FDS => {
            if nr_args == 0 || nr_args > 16 { return -22; }
            -38
        }
        REG_PBUF_RING | UNREG_PBUF_RING | REG_SYNC_CANCEL | REG_FILE_ALLOC_RANGE => -38,
        _ => -22, // 未知 opcode：严格 -EINVAL（之前的 silent 0 是简化）
    }
}
