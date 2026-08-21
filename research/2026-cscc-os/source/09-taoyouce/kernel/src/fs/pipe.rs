//! 管道实现
//!
//! 基于环形缓冲区的同步管道，支持 read/write 端分离。

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use crate::sched::wait_queue::{
    complete_wake, complete_wake_all, fire_poll_hooks, wake_drained, wake_entries, PollWakeHook,
    WaitEntry, WaitQueue, WaitReason,
};
use crate::sync::SpinLock;
use super::{
    File, FileMode, FileStat, FileType, FsError, Inode, READINESS_HUP, READINESS_READ,
    READINESS_WRITE,
};

/// Linux `pipe_poll` / `wake_up_interruptible_sync_poll` keys (`__poll_t`).
/// Bit values match EPOLLIN / EPOLLOUT / EPOLLRDNORM / EPOLLWRNORM / EPOLLHUP.
const PIPE_KEY_IN: u32 = READINESS_READ | 0x0040;
const PIPE_KEY_OUT: u32 = READINESS_WRITE | 0x0100;
const PIPE_KEY_IN_HUP: u32 = PIPE_KEY_IN | READINESS_HUP;
const PIPE_KEY_OUT_HUP: u32 = PIPE_KEY_OUT | READINESS_HUP;

pub const PIPE_MIN_SIZE: usize = 4096;
pub const PIPE_DEFAULT_SIZE: usize = 16 * PIPE_MIN_SIZE;
pub const PIPE_MAX_SIZE_DEFAULT: usize = 1024 * 1024;

/// 默认 pipe 环大小（页数）。Linux `PIPE_DEF_BUFFERS = 16`。
pub const PIPE_DEF_BUFFERS: usize = PIPE_DEFAULT_SIZE / PIPE_MIN_SIZE; // 16

use core::sync::atomic::AtomicUsize;

/// `/proc/sys/fs/pipe-user-pages-soft`：单用户所有 pipe 占用页数软上限。
/// Linux 默认 `PIPE_DEF_BUFFERS * INR_OPEN_CUR = 16 * 1024 = 16384`。
static PIPE_USER_PAGES_SOFT: AtomicUsize = AtomicUsize::new(PIPE_DEF_BUFFERS * 1024);
/// `/proc/sys/fs/pipe-user-pages-hard`：单用户 pipe 占用页数硬上限，0 = 关闭。
static PIPE_USER_PAGES_HARD: AtomicUsize = AtomicUsize::new(0);

pub fn pipe_user_pages_soft() -> usize {
    PIPE_USER_PAGES_SOFT.load(Ordering::Relaxed)
}
pub fn set_pipe_user_pages_soft(v: usize) {
    PIPE_USER_PAGES_SOFT.store(v, Ordering::Relaxed);
}
pub fn pipe_user_pages_hard() -> usize {
    PIPE_USER_PAGES_HARD.load(Ordering::Relaxed)
}
pub fn set_pipe_user_pages_hard(v: usize) {
    PIPE_USER_PAGES_HARD.store(v, Ordering::Relaxed);
}

/// 每用户（按 uid）已计费的 pipe 缓冲页数。对应 Linux `user_struct::pipe_bufs`。
static PIPE_USER_BUFS: SpinLock<BTreeMap<u32, usize>> = SpinLock::new(BTreeMap::new());

/// 未初始化的 u8 临时缓冲：读侧随后由 `ring.peek` 填入前 `got` 字节并
/// `truncate(got)`，未写入的尾部不可达；u8 无非法位型。每次 read 都
/// `resize(n, 0)` 会对大消息（如 lat_ctx 32KB token）做整段 memset 纯浪费。
fn uninit_u8_vec(n: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(n);
    unsafe { v.set_len(n) };
    v
}

/// 给 uid 的 pipe 页计费增减 `delta`（可负），返回更新后的总页数。
/// 对应 Linux `account_pipe_buffers()`（atomic_long_add_return）。
fn account_pipe_buffers(uid: u32, old_pages: usize, new_pages: usize) -> usize {
    let mut map = PIPE_USER_BUFS.lock();
    let cur = map.entry(uid).or_insert(0);
    // new - old，按饱和处理避免下溢
    *cur = cur.saturating_add(new_pages).saturating_sub(old_pages);
    let total = *cur;
    if total == 0 {
        map.remove(&uid);
    }
    total
}

fn too_many_pipe_buffers_soft(user_bufs: usize) -> bool {
    let soft = pipe_user_pages_soft();
    soft != 0 && user_bufs > soft
}
fn too_many_pipe_buffers_hard(user_bufs: usize) -> bool {
    let hard = pipe_user_pages_hard();
    hard != 0 && user_bufs > hard
}

/// 当前进程是否为非特权用户（pipe 软/硬上限只对非特权用户生效）。
/// Linux：`!capable(CAP_SYS_RESOURCE) && !capable(CAP_SYS_ADMIN)`。
fn pipe_is_unprivileged_user() -> bool {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let euid = table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid).map(|p| p.euid)))
        .unwrap_or(0);
    euid != 0
}

fn current_euid() -> u32 {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid).map(|p| p.euid)))
        .unwrap_or(0)
}

/// 为新建 pipe 计算应分配的环字节大小，并完成 per-user 页计费。
/// 对应 Linux `alloc_pipe_info()` 的软/硬上限逻辑：
///   * 默认 16 页；非特权用户超软上限 → 收缩为 1 页；
///   * 非特权用户超硬上限 → 回退计费并返回 `Err(ENOMEM)`。
/// 返回 `(capacity_bytes, accounted_pages, uid)`，调用方需把后两者存入 PipeBuffer
/// 以便释放时归还计费。
pub fn account_new_pipe() -> Result<(usize, usize, u32), FsError> {
    let uid = current_euid();
    let unpriv = pipe_is_unprivileged_user();
    // 默认页数：root 取 PIPE_DEF_BUFFERS；非特权用户受 pipe-max-size 约束。
    let base_bytes = PipeBuffer::initial_capacity();
    let base_pages = (base_bytes / PIPE_MIN_SIZE).max(1);

    let mut pipe_pages = base_pages;
    let mut user_bufs = account_pipe_buffers(uid, 0, pipe_pages);
    if too_many_pipe_buffers_soft(user_bufs) && unpriv {
        user_bufs = account_pipe_buffers(uid, pipe_pages, 1);
        pipe_pages = 1;
    }
    if too_many_pipe_buffers_hard(user_bufs) && unpriv {
        account_pipe_buffers(uid, pipe_pages, 0);
        return Err(FsError::OutOfMemory);
    }
    Ok((pipe_pages * PIPE_MIN_SIZE, pipe_pages, uid))
}

/// 释放一个 pipe 时归还其 per-user 页计费。对应 Linux `free_pipe_info()`。
pub fn unaccount_pipe(uid: u32, pages: usize) {
    if pages > 0 {
        account_pipe_buffers(uid, pages, 0);
    }
}

/// 管道环形缓冲区
pub struct PipeBuffer {
    buf: Vec<u8>,
    head: usize,
    tail: usize,
    len: usize,
    capacity: usize,
    packet_mode: bool,
    packet_lens: Vec<usize>,
    /// 写端是否已关闭
    write_closed: AtomicBool,
    /// 读端是否已关闭
    read_closed: AtomicBool,
    read_waiters: WaitQueue,
    write_waiters: WaitQueue,
    /// 写端直递登记表（b126 读端直递的镜像）：pid → 阻塞写者的剩余工作
    /// （用户缓冲起点 / 已写入环的字节数 / 总长）。读者消费出环空间后按此
    /// 替写者把剩余数据拷入环并完成其 syscall（Linux pipe_write 阻塞语义，
    /// 写者不回放、数据不重复）。
    write_ctx: alloc::collections::BTreeMap<usize, PipeWriteCtx>,
    async_owner_type: AtomicI32,
    async_owner_pid: AtomicI32,
    async_signal: AtomicI32,
    async_enabled: AtomicBool,
    /// per-user pipe 页计费量（页数）。0 表示该 buffer 未计费（FIFO/socketpair/测试），
    /// Drop 时不归还。对应 Linux `pipe_inode_info::ring_size` 的计费部分。
    accounted_pages: usize,
    /// 计费所属用户 uid（Drop 时据此归还）。
    owner_uid: u32,
}

/// 阻塞写者的直递登记项。
struct PipeWriteCtx {
    /// 剩余待写数据的**内核 staging 缓冲**（阻塞点由 syscall 层拷入；读者代写
    /// 时直接从中拷入环，免用户页翻译/缺页/EFAULT 分支）。
    data: Vec<u8>,
    /// staging 缓冲内已由读者拷入环的偏移。
    off: usize,
    /// 阻塞前已写入环的字节数（完成时 a0 = a0_base + data.len()）。
    a0_base: usize,
}

impl PipeBuffer {
    pub fn new() -> Self {
        Self::new_with_packet_mode(false)
    }

    pub fn new_with_packet_mode(packet_mode: bool) -> Self {
        PipeBuffer {
            buf: {
                let mut buf = Vec::new();
                buf.resize(Self::initial_capacity(), 0);
                buf
            },
            head: 0,
            tail: 0,
            len: 0,
            capacity: Self::initial_capacity(),
            packet_mode,
            packet_lens: Vec::new(),
            write_closed: AtomicBool::new(false),
            read_closed: AtomicBool::new(false),
            read_waiters: WaitQueue::new(),
            write_waiters: WaitQueue::new(),
            write_ctx: alloc::collections::BTreeMap::new(),
            async_owner_type: AtomicI32::new(1),
            async_owner_pid: AtomicI32::new(0),
            async_signal: AtomicI32::new(0),
            async_enabled: AtomicBool::new(false),
            accounted_pages: 0,
            owner_uid: 0,
        }
    }

    /// 创建一个完成 per-user 页计费的 pipe buffer（pipe(2)/pipe2(2) 专用）。
    /// 对应 Linux `alloc_pipe_info()`：默认 16 页；非特权用户超软上限收缩为 1 页；
    /// 超硬上限回退计费并返回 `Err(OutOfMemory)`。
    pub fn new_accounted(packet_mode: bool) -> Result<Self, FsError> {
        let (capacity, pages, uid) = account_new_pipe()?;
        let mut b = PipeBuffer::new_with_packet_mode(packet_mode);
        if b.capacity != capacity {
            b.buf.clear();
            b.buf.resize(capacity, 0);
            b.capacity = capacity;
            b.head = 0;
            b.tail = 0;
            b.len = 0;
        }
        b.accounted_pages = pages;
        b.owner_uid = uid;
        Ok(b)
    }

    fn available_read(&self) -> usize {
        self.len
    }

    /// 当通过 `set_capacity` 调整 pipe 容量时，同步 per-user 页计费（fcntl
    /// F_SETPIPE_SZ）。对应 Linux `pipe_set_size()` 的 account_pipe_buffers 调用。
    fn reaccount_capacity(&mut self, new_capacity: usize) {
        if self.accounted_pages == 0 {
            return; // 未计费的 buffer（FIFO/socketpair/测试）不参与
        }
        let new_pages = (new_capacity / PIPE_MIN_SIZE).max(1);
        if new_pages != self.accounted_pages {
            account_pipe_buffers(self.owner_uid, self.accounted_pages, new_pages);
            self.accounted_pages = new_pages;
        }
    }

    fn initial_capacity() -> usize {
        let pipe_max = crate::fs::proc::sys::pipe_max_size();
        let is_root = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table
                .as_ref()
                .and_then(|t| t.current.and_then(|pid| t.get(pid).map(|p| p.euid == 0)))
                .unwrap_or(true)
        };
        if is_root {
            PIPE_DEFAULT_SIZE
        } else {
            PIPE_DEFAULT_SIZE.min(pipe_max.max(PIPE_MIN_SIZE))
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn set_capacity(&mut self, requested: usize) -> Result<usize, FsError> {
        let rounded = requested.max(PIPE_MIN_SIZE).next_power_of_two();
        if rounded > crate::fs::proc::sys::pipe_max_size() || rounded > i32::MAX as usize {
            return Err(FsError::OperationNotPermitted);
        }
        if rounded < self.len {
            return Err(FsError::Busy);
        }
        if rounded == self.capacity {
            return Ok(self.capacity);
        }
        let mut new_buf = Vec::new();
        new_buf.resize(rounded, 0);
        let mut i = 0;
        while i < self.len {
            new_buf[i] = self.buf[(self.head + i) % self.capacity];
            i += 1;
        }
        self.buf = new_buf;
        self.capacity = rounded;
        self.head = 0;
        self.tail = self.len % self.capacity;
        // F_SETPIPE_SZ 改变环大小后同步 per-user 页计费。
        self.reaccount_capacity(self.capacity);
        Ok(self.capacity)
    }

    fn peek(&self, buf: &mut [u8]) -> usize {
        if !self.packet_mode {
            let mut copied = 0usize;
            let want = buf.len().min(self.len);
            let mut pos = self.head;
            while copied < want {
                let chunk = (self.capacity - pos).min(want - copied);
                buf[copied..copied + chunk].copy_from_slice(&self.buf[pos..pos + chunk]);
                copied += chunk;
                pos = (pos + chunk) % self.capacity;
            }
            return copied;
        }
        let mut count = 0;
        let mut pos = self.head;
        while count < buf.len() && count < self.len {
            buf[count] = self.buf[pos];
            pos = (pos + 1) % self.capacity;
            count += 1;
        }
        count
    }

    fn read(&mut self, buf: &mut [u8]) -> usize {
        if self.packet_mode {
            if buf.is_empty() {
                return 0;
            }
            let Some(packet_len) = self.packet_lens.first().copied() else {
                return 0;
            };
            self.packet_lens.remove(0);
            let copy_len = buf.len().min(packet_len);
            let mut copied = 0;
            while copied < copy_len && self.len > 0 {
                buf[copied] = self.buf[self.head];
                self.head = (self.head + 1) % self.capacity;
                self.len -= 1;
                copied += 1;
            }
            let mut discarded = copied;
            while discarded < packet_len && self.len > 0 {
                self.head = (self.head + 1) % self.capacity;
                self.len -= 1;
                discarded += 1;
            }
            return copied;
        }
        let mut count = 0;
        let want = buf.len().min(self.len);
        while count < want {
            let chunk = (self.capacity - self.head).min(want - count);
            buf[count..count + chunk].copy_from_slice(&self.buf[self.head..self.head + chunk]);
            self.head = (self.head + chunk) % self.capacity;
            self.len -= chunk;
            count += chunk;
        }
        count
    }

    fn write(&mut self, buf: &[u8]) -> usize {
        if self.packet_mode {
            let mut total = 0;
            while total < buf.len() {
                let packet_target = (buf.len() - total).min(self.capacity);
                let mut packet_written = 0;
                while packet_written < packet_target {
                    if self.len == self.capacity {
                        break;
                    }
                    self.buf[self.tail] = buf[total + packet_written];
                    self.tail = (self.tail + 1) % self.capacity;
                    self.len += 1;
                    packet_written += 1;
                }
                if packet_written == 0 {
                    break;
                }
                self.packet_lens.push(packet_written);
                total += packet_written;
                if packet_written < packet_target {
                    break;
                }
            }
            return total;
        }
        let mut count = 0;
        let want = buf.len().min(self.capacity - self.len);
        while count < want {
            let chunk = (self.capacity - self.tail).min(want - count);
            self.buf[self.tail..self.tail + chunk].copy_from_slice(&buf[count..count + chunk]);
            self.tail = (self.tail + chunk) % self.capacity;
            self.len += chunk;
            count += chunk;
        }
        count
    }

    fn set_async_owner(&self, owner_type: i32, owner_pid: i32, signal: i32, enabled: bool) {
        self.async_owner_type.store(owner_type, Ordering::Relaxed);
        self.async_owner_pid.store(owner_pid, Ordering::Relaxed);
        self.async_signal.store(signal, Ordering::Relaxed);
        self.async_enabled.store(enabled, Ordering::Relaxed);
    }

    fn notify_async_readable(&self) {
        if !self.async_enabled.load(Ordering::Relaxed) {
            return;
        }
        let owner_type = self.async_owner_type.load(Ordering::Relaxed);
        let owner_pid = self.async_owner_pid.load(Ordering::Relaxed);
        if owner_pid <= 0 {
            return;
        }
        let mut sig = self.async_signal.load(Ordering::Relaxed);
        if sig == 0 {
            sig = 29;
        }
        if !(1..=64).contains(&sig) {
            return;
        }
        let mask = 1u64 << (sig as u32 - 1);
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            let mut signal_pid = |pid: usize, table: &mut crate::sched::process::ProcessTable| {
                let should_wake = if let Some(process) = table.get_mut(pid) {
                    process.pending_signals |= mask; crate::sched::process::note_signal_hint();
                    process.state == crate::sched::process::ProcessState::Sleeping
                } else {
                    false
                };
                if should_wake {
                    table.mark_runnable(pid, true);
                }
            };
            if owner_type == 2 {
                let pgrp = owner_pid as usize;
                let pids = t.pids();
                for pid in pids {
                    if t.get(pid).map_or(false, |p| p.pgid == pgrp) {
                        signal_pid(pid, t);
                    }
                }
            } else {
                signal_pid(owner_pid as usize, t);
            }
        }
    }
}

/// 释放 pipe buffer 时归还其 per-user 页计费（对应 Linux `free_pipe_info()`）。
impl Drop for PipeBuffer {
    fn drop(&mut self) {
        if self.accounted_pages > 0 {
            unaccount_pipe(self.owner_uid, self.accounted_pages);
            self.accounted_pages = 0;
        }
    }
}

/// 管道读端
pub struct PipeReadEnd {
    buffer: Arc<SpinLock<PipeBuffer>>,
    /// 每管道唯一 st_ino（两端共享，对标 Linux pipefs 递增 ino）
    ino: usize,
}

/// 管道写端
pub struct PipeWriteEnd {
    buffer: Arc<SpinLock<PipeBuffer>>,
    /// 每管道唯一 st_ino（两端共享，对标 Linux pipefs 递增 ino）
    ino: usize,
}

/// 管道 ino 分配器：从 0x2000_0000 起递增，避开 devfs（1..N）、tty
/// （0x1000_0000/1）等其它固定 ino 空间，防止 SAME_INODE 类误判。
static NEXT_PIPE_INO: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0x2000_0000);

/// 创建管道，返回 (读端, 写端)
pub fn make_pipe() -> (Arc<PipeReadEnd>, Arc<PipeWriteEnd>) {
    make_pipe_with_packet_mode(false)
}

pub fn make_pipe_with_packet_mode(packet_mode: bool) -> (Arc<PipeReadEnd>, Arc<PipeWriteEnd>) {
    let buffer = Arc::new(SpinLock::new(PipeBuffer::new_with_packet_mode(packet_mode)));
    let ino = NEXT_PIPE_INO.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    (
        Arc::new(PipeReadEnd { buffer: buffer.clone(), ino }),
        Arc::new(PipeWriteEnd { buffer, ino }),
    )
}

/// 创建一个完成 per-user 页计费的管道（pipe(2)/pipe2(2) 专用），返回 (读端, 写端)。
/// 当非特权用户达到 `pipe-user-pages-hard` 硬上限时返回 `Err(OutOfMemory)`。
pub fn make_pipe_accounted(
    packet_mode: bool,
) -> Result<(Arc<PipeReadEnd>, Arc<PipeWriteEnd>), FsError> {
    let buffer = Arc::new(SpinLock::new(PipeBuffer::new_accounted(packet_mode)?));
    let ino = NEXT_PIPE_INO.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    Ok((
        Arc::new(PipeReadEnd { buffer: buffer.clone(), ino }),
        Arc::new(PipeWriteEnd { buffer, ino }),
    ))
}

// ============================================================================
// Slice 2.x T3-4 — Named FIFO support
//
// 当 mknod/mknodat 在 ramfs 创建 FileType::Pipe inode 后，对同一 inode 多次
// `open(O_RDONLY)` 应返回同一份缓冲区的读端，`open(O_WRONLY)` 应返回同一份
// 缓冲区的写端。通过 inode id → Arc<SpinLock<PipeBuffer>> 全局映射实现。
//
// 注意：close 时不能再走 PipeReadEnd::drop / PipeWriteEnd::drop 的
// "整端关闭"语义（命名管道的多个 reader/writer 可独立 close），
// 这里 NamedPipeRead/Write 用 weak 计数实现：
//   - 读端 reader_count，到 0 时 read_closed=true（让 writer 拿到 EPIPE）
//   - 写端 writer_count，到 0 时 write_closed=true（让 reader 读到 EOF）
// ============================================================================
extern crate alloc;

use alloc::collections::BTreeMap;
use crate::sync::SpinLock as KernelSpinLock;

pub struct FifoState {
    pub buffer: Arc<SpinLock<PipeBuffer>>,
    pub reader_count: core::sync::atomic::AtomicUsize,
    pub writer_count: core::sync::atomic::AtomicUsize,
}

static FIFO_REGISTRY: KernelSpinLock<BTreeMap<usize, Arc<FifoState>>> =
    KernelSpinLock::new(BTreeMap::new());

/// 取/建给定 inode 的 FifoState；多次 open 同一 FIFO 共享。
pub fn get_or_create_fifo(inode_id: usize) -> Arc<FifoState> {
    let mut reg = FIFO_REGISTRY.lock();
    if let Some(s) = reg.get(&inode_id) {
        return s.clone();
    }
    let state = Arc::new(FifoState {
        buffer: Arc::new(SpinLock::new(PipeBuffer::new())),
        reader_count: core::sync::atomic::AtomicUsize::new(0),
        writer_count: core::sync::atomic::AtomicUsize::new(0),
    });
    reg.insert(inode_id, state.clone());
    state
}

pub struct NamedFifoRead {
    pub state: Arc<FifoState>,
    pub ino: usize,
}

impl NamedFifoRead {
    pub fn new(state: Arc<FifoState>, ino: usize) -> Arc<Self> {
        state.reader_count.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        Arc::new(Self { state, ino })
    }
}

impl Drop for NamedFifoRead {
    fn drop(&mut self) {
        let prev = self.state.reader_count.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
        if prev == 1 {
            let ring = self.state.buffer.lock();
            ring.read_closed.store(true, core::sync::atomic::Ordering::Release);
        }
    }
}

pub struct NamedFifoWrite {
    pub state: Arc<FifoState>,
    pub ino: usize,
}

impl NamedFifoWrite {
    pub fn new(state: Arc<FifoState>, ino: usize) -> Arc<Self> {
        state.writer_count.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        Arc::new(Self { state, ino })
    }
}

impl Drop for NamedFifoWrite {
    fn drop(&mut self) {
        let prev = self.state.writer_count.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
        if prev == 1 {
            let ring = self.state.buffer.lock();
            ring.write_closed.store(true, core::sync::atomic::Ordering::Release);
        }
    }
}

// File + Inode impls reuse the buffer logic from PipeReadEnd / PipeWriteEnd.
impl File for NamedFifoRead {
    fn read(&self, _o: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let pid = current_pid();
        let mut ring = self.state.buffer.lock();
        let n = ring.read(buf);
        if n > 0 {
            let (waiters, hooks) = ring.write_waiters.prepare_wake();
            drop(ring);
            complete_wake(waiters, &hooks, PIPE_KEY_OUT, true);
            return Ok(n);
        }
        if ring.write_closed.load(core::sync::atomic::Ordering::Acquire) { return Ok(0); }
        if let Some(pid) = pid {
            ring.read_waiters.enqueue_pid(pid, WaitReason::PipeRead, None);
        }
        Err(FsError::WouldBlock)
    }
    fn write(&self, _o: usize, _b: &[u8]) -> Result<usize, FsError> { Err(FsError::InvalidArgument) }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Pipe, size: 0,
            mode: FileMode(0o010600),
            nlink: 1, uid: 0, gid: 0, atime: 0, mtime: 0, ctime: 0,
        })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
    fn poll_read(&self) -> bool {
        let ring = self.state.buffer.lock();
        ring.available_read() > 0 || ring.write_closed.load(core::sync::atomic::Ordering::Acquire)
    }
    fn poll_write(&self) -> bool { false }
    fn register_waiter(&self, events: u32, pid: usize, deadline: Option<u64>) {
        if events & READINESS_READ != 0 {
            let ring = self.state.buffer.lock();
            if ring.available_read() == 0 && !ring.write_closed.load(core::sync::atomic::Ordering::Acquire) {
                ring.read_waiters.enqueue_pid(pid, WaitReason::Poll, deadline);
            }
        }
    }
    fn attach_poll_hook(&self, hook: alloc::sync::Arc<dyn PollWakeHook>) {
        self.state.buffer.lock().read_waiters.add_poll_hook(hook);
    }
    fn detach_poll_hook(&self, hook_id: usize) {
        self.state.buffer.lock().read_waiters.remove_poll_hook(hook_id);
    }
    fn close_notify_last(&self) {
        let (entries, hooks) = {
            let ring = self.state.buffer.lock();
            ring.write_waiters.prepare_wake_all()
        };
        complete_wake_all(entries, &hooks, PIPE_KEY_OUT_HUP, true);
    }
    fn set_async_owner(&self, owner_type: i32, owner_pid: i32, signal: i32, enabled: bool) {
        let ring = self.state.buffer.lock();
        ring.set_async_owner(owner_type, owner_pid, signal, enabled);
    }

    fn tee_to(&self, dst: &dyn File, len: usize) -> Result<usize, FsError> {
        let pid = current_pid();
        let n = {
            let ring = self.state.buffer.lock();
            let available = ring.available_read();
            if available == 0 {
                if ring.write_closed.load(core::sync::atomic::Ordering::Acquire) {
                    return Ok(0);
                }
                if let Some(pid) = pid {
                    ring.read_waiters.enqueue_pid(pid, WaitReason::PipeRead, None);
                }
                return Err(FsError::WouldBlock);
            }
            available.min(len)
        };
        if n == 0 {
            return Ok(0);
        }
        let mut tmp = uninit_u8_vec(n);
        {
            let ring = self.state.buffer.lock();
            let got = ring.peek(&mut tmp);
            tmp.truncate(got);
        }
        if tmp.is_empty() {
            return Ok(0);
        }
        dst.write(0, &tmp)
    }

    fn pipe_buffer_id(&self) -> Option<usize> {
        Some(Arc::as_ptr(&self.state.buffer) as usize)
    }

    fn pipe_capacity(&self) -> Result<usize, FsError> {
        Ok(self.state.buffer.lock().capacity())
    }

    fn set_pipe_capacity(&self, requested: usize) -> Result<usize, FsError> {
        self.state.buffer.lock().set_capacity(requested)
    }

    fn pipe_bytes_queued(&self) -> Option<usize> {
        Some(self.state.buffer.lock().available_read())
    }

    fn splice_to(&self, dst: &dyn File, len: usize) -> Result<usize, FsError> {
        if self.pipe_buffer_id() == dst.pipe_buffer_id() {
            return Err(FsError::InvalidArgument);
        }
        let pid = current_pid();
        let mut tmp = {
            let ring = self.state.buffer.lock();
            let available = ring.available_read();
            if available == 0 {
                if ring.write_closed.load(Ordering::Acquire) {
                    return Ok(0);
                }
                if let Some(pid) = pid {
                    ring.read_waiters.enqueue_pid(pid, WaitReason::PipeRead, None);
                }
                return Err(FsError::WouldBlock);
            }
            let n = available.min(len);
            if n == 0 {
                return Ok(0);
            }
            let mut tmp = uninit_u8_vec(n);
            let got = ring.peek(&mut tmp);
            tmp.truncate(got);
            tmp
        };
        if tmp.is_empty() {
            return Ok(0);
        }
        let written = dst.write(0, &tmp)?;
        if written == 0 {
            return Ok(0);
        }
        let consumed = {
            let mut ring = self.state.buffer.lock();
            let consumed = ring.read(&mut tmp[..written]);
            let (wake_entries_out, hooks) = if consumed > 0 {
                ring.write_waiters.prepare_wake_all()
            } else {
                (Vec::new(), Vec::new())
            };
            drop(ring);
            complete_wake_all(wake_entries_out, &hooks, PIPE_KEY_OUT, true);
            consumed
        };
        Ok(consumed)
    }
}

impl Inode for NamedFifoRead {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Pipe }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}

impl File for NamedFifoWrite {
    fn read(&self, _o: usize, _b: &mut [u8]) -> Result<usize, FsError> { Err(FsError::InvalidArgument) }
    fn write(&self, _o: usize, buf: &[u8]) -> Result<usize, FsError> {
        let pid = current_pid();
        let mut ring = self.state.buffer.lock();
        if ring.read_closed.load(core::sync::atomic::Ordering::Acquire) { return Err(FsError::BrokenPipe); }
        let n = ring.write(buf);
        if n > 0 {
            let (waiters, hooks) = ring.read_waiters.prepare_wake();
            ring.notify_async_readable();
            drop(ring);
            complete_wake(waiters, &hooks, PIPE_KEY_IN, true);
            return Ok(n);
        }
        if buf.is_empty() { return Ok(0); }
        if let Some(pid) = pid {
            ring.write_waiters.enqueue_pid(pid, WaitReason::PipeWrite, None);
        }
        Err(FsError::WouldBlock)
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Pipe, size: 0,
            mode: FileMode(0o010600),
            nlink: 1, uid: 0, gid: 0, atime: 0, mtime: 0, ctime: 0,
        })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
    fn poll_read(&self) -> bool { false }
    fn poll_write(&self) -> bool {
        let ring = self.state.buffer.lock();
        ring.available_read() < ring.capacity()
    }
    fn register_waiter(&self, events: u32, pid: usize, deadline: Option<u64>) {
        if events & READINESS_WRITE != 0 {
            let ring = self.state.buffer.lock();
            if ring.available_read() == ring.capacity() && !ring.read_closed.load(core::sync::atomic::Ordering::Acquire) {
                ring.write_waiters.enqueue_pid(pid, WaitReason::Poll, deadline);
            }
        }
    }
    fn attach_poll_hook(&self, hook: alloc::sync::Arc<dyn PollWakeHook>) {
        self.state.buffer.lock().write_waiters.add_poll_hook(hook);
    }
    fn detach_poll_hook(&self, hook_id: usize) {
        self.state.buffer.lock().write_waiters.remove_poll_hook(hook_id);
    }
    fn close_notify_last(&self) {
        let (entries, hooks) = {
            let ring = self.state.buffer.lock();
            ring.read_waiters.prepare_wake_all()
        };
        complete_wake_all(entries, &hooks, PIPE_KEY_IN_HUP, true);
    }

    fn pipe_buffer_id(&self) -> Option<usize> {
        Some(Arc::as_ptr(&self.state.buffer) as usize)
    }

    fn pipe_capacity(&self) -> Result<usize, FsError> {
        Ok(self.state.buffer.lock().capacity())
    }

    fn set_pipe_capacity(&self, requested: usize) -> Result<usize, FsError> {
        self.state.buffer.lock().set_capacity(requested)
    }

    fn pipe_bytes_queued(&self) -> Option<usize> {
        Some(self.state.buffer.lock().available_read())
    }
}

impl Inode for NamedFifoWrite {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Pipe }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}

/// FIFO opened with O_RDWR: both read+write on the same buffer.
pub struct NamedFifoRdwr {
    pub state: Arc<FifoState>,
    pub ino: usize,
}

impl NamedFifoRdwr {
    pub fn new(state: Arc<FifoState>, ino: usize) -> Arc<Self> {
        state.reader_count.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        state.writer_count.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        Arc::new(Self { state, ino })
    }
}

impl Drop for NamedFifoRdwr {
    fn drop(&mut self) {
        let prev_r = self.state.reader_count.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
        if prev_r == 1 {
            self.state.buffer.lock().read_closed.store(true, core::sync::atomic::Ordering::Release);
        }
        let prev_w = self.state.writer_count.fetch_sub(1, core::sync::atomic::Ordering::AcqRel);
        if prev_w == 1 {
            self.state.buffer.lock().write_closed.store(true, core::sync::atomic::Ordering::Release);
        }
    }
}

impl File for NamedFifoRdwr {
    fn read(&self, _o: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let pid = current_pid();
        let mut ring = self.state.buffer.lock();
        let n = ring.read(buf);
        if n > 0 {
            let (waiters, hooks) = ring.write_waiters.prepare_wake();
            drop(ring);
            complete_wake(waiters, &hooks, PIPE_KEY_OUT, true);
            return Ok(n);
        }
        if ring.write_closed.load(core::sync::atomic::Ordering::Acquire) { return Ok(0); }
        if let Some(pid) = pid {
            ring.read_waiters.enqueue_pid(pid, WaitReason::PipeRead, None);
        }
        Err(FsError::WouldBlock)
    }
    fn write(&self, _o: usize, buf: &[u8]) -> Result<usize, FsError> {
        let pid = current_pid();
        let mut ring = self.state.buffer.lock();
        if ring.read_closed.load(core::sync::atomic::Ordering::Acquire) { return Err(FsError::BrokenPipe); }
        let n = ring.write(buf);
        if n > 0 {
            let (waiters, hooks) = ring.read_waiters.prepare_wake();
            ring.notify_async_readable();
            drop(ring);
            complete_wake(waiters, &hooks, PIPE_KEY_IN, true);
            return Ok(n);
        }
        if buf.is_empty() { return Ok(0); }
        if let Some(pid) = pid {
            ring.write_waiters.enqueue_pid(pid, WaitReason::PipeWrite, None);
        }
        Err(FsError::WouldBlock)
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Pipe, size: 0,
            mode: FileMode(0o010600),
            nlink: 1, uid: 0, gid: 0, atime: 0, mtime: 0, ctime: 0,
        })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
    fn poll_read(&self) -> bool {
        let ring = self.state.buffer.lock();
        ring.available_read() > 0 || ring.write_closed.load(core::sync::atomic::Ordering::Acquire)
    }
    fn poll_write(&self) -> bool {
        let ring = self.state.buffer.lock();
        ring.available_read() < ring.capacity()
    }
    fn register_waiter(&self, events: u32, pid: usize, deadline: Option<u64>) {
        let ring = self.state.buffer.lock();
        if events & READINESS_READ != 0 {
            if ring.available_read() == 0 && !ring.write_closed.load(core::sync::atomic::Ordering::Acquire) {
                ring.read_waiters.enqueue_pid(pid, WaitReason::Poll, deadline);
            }
        }
        if events & READINESS_WRITE != 0 {
            if ring.available_read() == ring.capacity() && !ring.read_closed.load(core::sync::atomic::Ordering::Acquire) {
                ring.write_waiters.enqueue_pid(pid, WaitReason::Poll, deadline);
            }
        }
    }
    fn attach_poll_hook(&self, hook: alloc::sync::Arc<dyn PollWakeHook>) {
        let ring = self.state.buffer.lock();
        ring.read_waiters.add_poll_hook(hook.clone());
        ring.write_waiters.add_poll_hook(hook);
    }
    fn detach_poll_hook(&self, hook_id: usize) {
        let ring = self.state.buffer.lock();
        ring.read_waiters.remove_poll_hook(hook_id);
        ring.write_waiters.remove_poll_hook(hook_id);
    }
    fn close_notify_last(&self) {
        let (entries, hooks) = {
            let ring = self.state.buffer.lock();
            let (mut e, mut h) = ring.read_waiters.prepare_wake_all();
            let (e2, h2) = ring.write_waiters.prepare_wake_all();
            e.extend(e2);
            h.extend(h2);
            (e, h)
        };
        // Both sides closed — notify IN|OUT|HUP (Linux pipe peer hangup).
        complete_wake_all(entries, &hooks, PIPE_KEY_IN_HUP | PIPE_KEY_OUT_HUP, true);
    }
    fn set_async_owner(&self, owner_type: i32, owner_pid: i32, signal: i32, enabled: bool) {
        let ring = self.state.buffer.lock();
        ring.set_async_owner(owner_type, owner_pid, signal, enabled);
    }
    fn pipe_buffer_id(&self) -> Option<usize> {
        Some(Arc::as_ptr(&self.state.buffer) as usize)
    }
    fn pipe_capacity(&self) -> Result<usize, FsError> {
        Ok(self.state.buffer.lock().capacity())
    }
    fn set_pipe_capacity(&self, requested: usize) -> Result<usize, FsError> {
        self.state.buffer.lock().set_capacity(requested)
    }
    fn pipe_bytes_queued(&self) -> Option<usize> {
        Some(self.state.buffer.lock().available_read())
    }
}

impl Inode for NamedFifoRdwr {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Pipe }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}

impl Drop for PipeReadEnd {
    fn drop(&mut self) {
        // Mirror close_notify_last: last reader going away must wake blocked
        // writers (and any poll/epoll waiter parked on write_waiters). Setting
        // the flag alone used to leave them sleeping forever when the Arc was
        // dropped without an OFD close path.
        let (entries, hooks) = {
            let ring = self.buffer.lock();
            ring.read_closed.store(true, Ordering::Release);
            ring.write_waiters.prepare_wake_all()
        };
        complete_wake_all(entries, &hooks, PIPE_KEY_OUT_HUP, true);
    }
}

impl Drop for PipeWriteEnd {
    fn drop(&mut self) {
        // Mirror close_notify_last: last writer → EOF/EPOLLHUP for readers.
        let (entries, hooks) = {
            let ring = self.buffer.lock();
            ring.write_closed.store(true, Ordering::Release);
            ring.read_waiters.prepare_wake_all()
        };
        complete_wake_all(entries, &hooks, PIPE_KEY_IN_HUP, true);
    }
}

// ---- PipeReadEnd 实现 File + Inode ----

impl File for PipeReadEnd {
    fn read(&self, _offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        let pid = current_pid();
        let mut ring = self.buffer.lock();
        let n = ring.read(buf);
        if n > 0 {
            // 写端直递：本次消费释放了环空间，替阻塞写者续写剩余数据
            // （持环锁内做，ring→PROCESS_TABLE 锁序与 b126 直递一致）。
            try_pipe_write_handoff(&self.buffer, &mut ring);
            let (waiters, hooks) = ring.write_waiters.prepare_wake();
            drop(ring);
            complete_wake(waiters, &hooks, PIPE_KEY_OUT, true);
            return Ok(n);
        }
        // 缓冲区空：若写端已关闭则 EOF(Ok(0))，否则 WouldBlock
        if ring.write_closed.load(Ordering::Acquire) {
            Ok(0) // EOF
        } else {
            if let Some(pid) = pid {
                ring.read_waiters.enqueue_pid(pid, WaitReason::PipeRead, None);
            }
            Err(FsError::WouldBlock) // 暂无数据，调用方应重试
        }
    }

    /// 零拷贝读：把环形缓冲直接写进用户页（省去内核中转缓冲的第二次
    /// 整段 memcpy，32KB 消息一次 read 就省 32KB 拷贝）。语义与 `read`
    /// 一致；用户页映射解析失败回退 NotSupported，由通用路径报 EFAULT。
    fn read_to_user(&self, _offset: usize, user_ptr: *mut u8, len: usize) -> Result<usize, FsError> {
        let segs = crate::syscall::user::user_writable_segments(user_ptr, len)
            .map_err(|_| FsError::NotSupported)?;
        let pid = current_pid();
        let mut ring = self.buffer.lock();
        let mut total = 0usize;
        for (ptr, seg_len) in segs {
            // SAFETY: user_writable_segments 已按页解析出可写内核直通映射，
            // 每段不跨页且总长 = len。
            let slice = unsafe { core::slice::from_raw_parts_mut(ptr, seg_len) };
            let n = ring.read(slice);
            total += n;
            if n < seg_len {
                break;
            }
        }
        if total > 0 {
            // 写端直递：同 read 路径（释放环空间后替阻塞写者续写）。
            try_pipe_write_handoff(&self.buffer, &mut ring);
            let (waiters, hooks) = ring.write_waiters.prepare_wake();
            drop(ring);
            complete_wake(waiters, &hooks, PIPE_KEY_OUT, true);
            return Ok(total);
        }
        if ring.write_closed.load(Ordering::Acquire) {
            Ok(0)
        } else {
            if let Some(pid) = pid {
                ring.read_waiters.enqueue_pid(pid, WaitReason::PipeRead, None);
            }
            Err(FsError::WouldBlock)
        }
    }

    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument) // 读端不可写
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Pipe,
            size: 0,
            // S_IFIFO | 0600：fstat 直接回传 mode，st_mode 必须含类型位
            // （cargo jobserver 用 S_ISFIFO 验证继承的管道 fd）。
            mode: FileMode(0o010600),
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
        let ring = self.buffer.lock();
        ring.available_read() > 0 || ring.write_closed.load(Ordering::Acquire)
    }

    fn poll_write(&self) -> bool {
        false // 读端不可写
    }

    fn readiness(&self) -> u32 {
        let ring = self.buffer.lock();
        let mut mask = 0;
        if ring.available_read() > 0 {
            mask |= READINESS_READ;
        }
        if ring.write_closed.load(Ordering::Acquire) {
            mask |= READINESS_READ | READINESS_HUP;
        }
        mask
    }

    fn register_waiter(&self, events: u32, pid: usize, deadline: Option<u64>) {
        if events & READINESS_READ != 0 {
            let ring = self.buffer.lock();
            if ring.available_read() == 0 && !ring.write_closed.load(Ordering::Acquire) {
                ring.read_waiters.enqueue_pid(pid, WaitReason::Poll, deadline);
            }
        }
    }

    fn attach_poll_hook(&self, hook: alloc::sync::Arc<dyn PollWakeHook>) {
        // Linux pipe_poll: FMODE_READ → poll_wait(&pipe->rd_wait).
        self.buffer.lock().read_waiters.add_poll_hook(hook);
    }

    fn detach_poll_hook(&self, hook_id: usize) {
        self.buffer.lock().read_waiters.remove_poll_hook(hook_id);
    }

    fn close_notify_last(&self) {
        let (entries, hooks) = {
            let ring = self.buffer.lock();
            ring.read_closed.store(true, Ordering::Release);
            ring.write_waiters.prepare_wake_all()
        };
        complete_wake_all(entries, &hooks, PIPE_KEY_OUT_HUP, true);
    }
    fn set_async_owner(&self, owner_type: i32, owner_pid: i32, signal: i32, enabled: bool) {
        let ring = self.buffer.lock();
        ring.set_async_owner(owner_type, owner_pid, signal, enabled);
    }

    fn tee_to(&self, dst: &dyn File, len: usize) -> Result<usize, FsError> {
        let pid = current_pid();
        let n = {
            let ring = self.buffer.lock();
            let available = ring.available_read();
            if available == 0 {
                if ring.write_closed.load(Ordering::Acquire) {
                    return Ok(0);
                }
                if let Some(pid) = pid {
                    ring.read_waiters.enqueue_pid(pid, WaitReason::PipeRead, None);
                }
                return Err(FsError::WouldBlock);
            }
            available.min(len)
        };
        if n == 0 {
            return Ok(0);
        }
        let mut tmp = uninit_u8_vec(n);
        {
            let ring = self.buffer.lock();
            let got = ring.peek(&mut tmp);
            tmp.truncate(got);
        }
        if tmp.is_empty() {
            return Ok(0);
        }
        dst.write(0, &tmp)
    }

    fn pipe_buffer_id(&self) -> Option<usize> {
        Some(Arc::as_ptr(&self.buffer) as usize)
    }

    fn pipe_capacity(&self) -> Result<usize, FsError> {
        Ok(self.buffer.lock().capacity())
    }

    fn set_pipe_capacity(&self, requested: usize) -> Result<usize, FsError> {
        self.buffer.lock().set_capacity(requested)
    }

    fn pipe_bytes_queued(&self) -> Option<usize> {
        Some(self.buffer.lock().available_read())
    }

    fn splice_to(&self, dst: &dyn File, len: usize) -> Result<usize, FsError> {
        if self.pipe_buffer_id() == dst.pipe_buffer_id() {
            return Err(FsError::InvalidArgument);
        }
        let pid = current_pid();
        let mut tmp = {
            let ring = self.buffer.lock();
            let available = ring.available_read();
            if available == 0 {
                if ring.write_closed.load(Ordering::Acquire) {
                    return Ok(0);
                }
                if let Some(pid) = pid {
                    ring.read_waiters.enqueue_pid(pid, WaitReason::PipeRead, None);
                }
                return Err(FsError::WouldBlock);
            }
            let n = available.min(len);
            if n == 0 {
                return Ok(0);
            }
            let mut tmp = uninit_u8_vec(n);
            let got = ring.peek(&mut tmp);
            tmp.truncate(got);
            tmp
        };
        if tmp.is_empty() {
            return Ok(0);
        }
        let written = dst.write(0, &tmp)?;
        if written == 0 {
            return Ok(0);
        }
        let consumed = {
            let mut ring = self.buffer.lock();
            let consumed = ring.read(&mut tmp[..written]);
            let (wake_entries_out, hooks) = if consumed > 0 {
                ring.write_waiters.prepare_wake_all()
            } else {
                (Vec::new(), Vec::new())
            };
            drop(ring);
            complete_wake_all(wake_entries_out, &hooks, PIPE_KEY_OUT, true);
            consumed
        };
        Ok(consumed)
    }
}

impl Inode for PipeReadEnd {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Pipe }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn super::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}

// ---- PipeWriteEnd 实现 File + Inode ----

impl File for PipeWriteEnd {
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument) // 写端不可读
    }

    fn write(&self, _offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        if buf.is_empty() { return Ok(0); }
        let pid = current_pid();
        let mut ring = self.buffer.lock();
        if ring.read_closed.load(Ordering::Acquire) {
            return Err(FsError::BrokenPipe);
        }
        // 直递快路径：环空 + 有阻塞读者 → 绕过环形缓冲直写读者用户页
        // （packet 模式的「短读丢尾包」语义与直递不兼容，不走）。
        if ring.len == 0 && !ring.packet_mode {
            if let Some(entry) = ring.read_waiters.dequeue_reason(WaitReason::PipeRead) {
                drop(ring);
                match try_pipe_read_handoff(&self.buffer, entry.pid, buf) {
                    Ok(n) => return Ok(n),
                    Err(()) => {
                        // 回退：waiter 放回队列，走常规「入环 + 唤醒」路径
                        //（读者若已 runnable 会重放 read 自取数据）。
                        let r = self.buffer.lock();
                        if r.read_closed.load(Ordering::Acquire) {
                            return Err(FsError::BrokenPipe);
                        }
                        r.read_waiters.enqueue(entry);
                        ring = r;
                    }
                }
            }
        }
        let n = ring.write(buf);
        if n > 0 {
            let (waiters, hooks) = ring.read_waiters.prepare_wake();
            ring.notify_async_readable();
            drop(ring);
            complete_wake(waiters, &hooks, PIPE_KEY_IN, true);
            Ok(n)
        } else {
            if let Some(pid) = pid {
                ring.write_waiters.enqueue_pid(pid, WaitReason::PipeWrite, None);
            }
            Err(FsError::WouldBlock)
        }
    }

    /// 写端直递登记（由 sys_write/sys_writev 在环满且已有部分进度时调用）：
    /// 记录当前写者的剩余数据（内核 staging 缓冲，所有权移交本 pipe）与阻塞前
    /// 已写字节数，读者消费后替其续写完成。
    fn register_write_handoff(&self, data: Vec<u8>, a0_base: usize) -> bool {
        if data.is_empty() {
            return false;
        }
        let Some(pid) = current_pid() else { return false };
        if crate::cmdline::wrdbg_enabled() {
            static B: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(64);
            if B.fetch_update(core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed, |n| if n==0 {None} else {Some(n-1)}).is_ok() {
                crate::println!("[WRHDBG] register pid={} a0_base={} data={}", pid, a0_base, data.len());
            }
        }
        let mut ring = self.buffer.lock();
        ring.write_ctx.insert(
            pid,
            PipeWriteCtx {
                data,
                off: 0,
                a0_base,
            },
        );
        true
    }

    /// 写者 syscall 重放入口消费：返回「阻塞前已写入 + 读者已代写」的总字节数
    /// 并移除登记（数据 Vec 随 ctx 释放）。
    ///
    /// 只返回 a0_base 会重复：deadline 唤醒前读者可能已代写 ctx.off 字节入环并
    /// 被对端消费，写者按 a0_base 短写重试会把 len-a0_base 整段再写一遍，
    /// 对端最终收到 a0_base + ctx.off + 重试 = 重复 ctx.off 字节
    /// （复现：dd 1MB|cat|wc 输出 1835008 = 1048576 + 786432）。
    fn take_write_handoff(&self, pid: usize) -> Option<usize> {
        let mut ring = self.buffer.lock();
        let taken = ring.write_ctx.remove(&pid).map(|c| {
            if crate::cmdline::wrdbg_enabled() {
                static B: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(64);
                if B.fetch_update(core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed, |n| if n==0 {None} else {Some(n-1)}).is_ok() {
                    crate::println!("[WRHDBG] take pid={} a0_base={} off={} data={} -> {}", pid, c.a0_base, c.off, c.data.len(), c.a0_base + c.off);
                }
            }
            c.a0_base + c.off
        });
        taken
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Pipe,
            size: 0,
            // S_IFIFO | 0600：fstat 直接回传 mode，st_mode 必须含类型位
            // （cargo jobserver 用 S_ISFIFO 验证继承的管道 fd）。
            mode: FileMode(0o010600),
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
        false // 写端不可读
    }

    fn poll_write(&self) -> bool {
        let ring = self.buffer.lock();
        // Linux pipe_poll: EPOLLOUT is reported only when at least one full
        // buffer slot (PAGE_SIZE = 4096 bytes) is free. A partial read within a
        // buffer does not make the pipe "writable" from poll's perspective.
        // This matches Linux's page-buffer-slot semantics for epoll ET.
        let free = ring.capacity() - ring.available_read();
        free >= PIPE_MIN_SIZE || ring.read_closed.load(Ordering::Acquire)
    }

    fn readiness(&self) -> u32 {
        let ring = self.buffer.lock();
        let mut mask = 0;
        // Match poll_write: require at least one full page slot free
        let free = ring.capacity() - ring.available_read();
        if free >= PIPE_MIN_SIZE {
            mask |= READINESS_WRITE;
        }
        if ring.read_closed.load(Ordering::Acquire) {
            mask |= READINESS_WRITE | READINESS_HUP;
        }
        mask
    }

    fn register_waiter(&self, events: u32, pid: usize, deadline: Option<u64>) {
        if events & READINESS_WRITE != 0 {
            let ring = self.buffer.lock();
            let free = ring.capacity() - ring.available_read();
            if free < PIPE_MIN_SIZE && !ring.read_closed.load(Ordering::Acquire) {
                ring.write_waiters.enqueue_pid(pid, WaitReason::Poll, deadline);
            }
        }
    }

    fn attach_poll_hook(&self, hook: alloc::sync::Arc<dyn PollWakeHook>) {
        // Linux pipe_poll: FMODE_WRITE → poll_wait(&pipe->wr_wait).
        self.buffer.lock().write_waiters.add_poll_hook(hook);
    }

    fn detach_poll_hook(&self, hook_id: usize) {
        self.buffer.lock().write_waiters.remove_poll_hook(hook_id);
    }

    fn close_notify_last(&self) {
        let (entries, hooks) = {
            let ring = self.buffer.lock();
            ring.write_closed.store(true, Ordering::Release);
            ring.read_waiters.prepare_wake_all()
        };
        complete_wake_all(entries, &hooks, PIPE_KEY_IN_HUP, true);
    }

    fn pipe_buffer_id(&self) -> Option<usize> {
        Some(Arc::as_ptr(&self.buffer) as usize)
    }

    fn pipe_capacity(&self) -> Result<usize, FsError> {
        Ok(self.buffer.lock().capacity())
    }

    fn set_pipe_capacity(&self, requested: usize) -> Result<usize, FsError> {
        self.buffer.lock().set_capacity(requested)
    }

    fn pipe_bytes_queued(&self) -> Option<usize> {
        Some(self.buffer.lock().available_read())
    }
}

impl Inode for PipeWriteEnd {
    fn ino(&self) -> usize { self.ino }
    fn file_type(&self) -> FileType { FileType::Pipe }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn super::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}

/// b126「pipe 阻塞读直递」的镜像——「pipe 阻塞写直递」：读者在消费释放环空间后
/// （持环锁），替阻塞写者把其剩余的 **内核 staging 缓冲**直接拷入环；全部拷完则
/// 撤销写者的 syscall 重放（sepc+=4）、置 a0=总长并 mark_runnable_handoff。
///
/// 对标 Linux `pipe_write` 的 `wait_event_interruptible(pipe->wr_wait,
/// pipe_writable(pipe))`：写者在环满时阻塞、读者消费后由内核代为续写直至完成，
/// 写者不回放 syscall（无数据重复、无 shadow 状态）。任一校验不满足即跳过该
/// 写者（留给其重放入口消费 ctx 按短写返回，行为不劣于旧实现）。
fn try_pipe_write_handoff(buffer: &Arc<SpinLock<PipeBuffer>>, ring: &mut PipeBuffer) {
    if ring.write_ctx.is_empty() {
        return;
    }
    loop {
        let free = ring.capacity - ring.len;
        if free == 0 {
            break;
        }
        let first_pid = ring.write_ctx.keys().next().copied();
        let Some(pid) = first_pid else { break };
        match try_write_handoff_once(buffer, ring, pid) {
            Some(true) => continue, // 写完一个，接着下一个写者
            Some(false) => break, // 写者状态不满足/环又满：留给重放入口，避免锁内空转
            None => break,
        }
    }
}

/// 单写者直递尝试。返回 Some(true)=已完成并移除、Some(false)=跳过（写者不在
/// 等待态/不是本 pipe 的 write(2)/writev(2)/环又满）。
fn try_write_handoff_once(
    buffer: &Arc<SpinLock<PipeBuffer>>,
    ring: &mut PipeBuffer,
    pid: usize,
) -> Option<bool> {
    #[cfg(target_arch = "riscv64")]
    const REG_A0: usize = 10;
    #[cfg(target_arch = "loongarch64")]
    const REG_A0: usize = 4;
    #[cfg(target_arch = "riscv64")]
    const REG_SYSCALL: usize = 17;
    #[cfg(target_arch = "loongarch64")]
    const REG_SYSCALL: usize = 11;
    const SYS_WRITE: usize = 64;
    const SYS_WRITEV: usize = 66;

    use crate::sched::process::{ProcessState, IO_WAIT_SENTINEL, PROCESS_TABLE};

    // ===== 环锁 → PROCESS_TABLE：校验写者确实阻塞在本 pipe 的 write/writev 上 =====
    let (a0_base, data_len, off) = {
        let mut guard = PROCESS_TABLE.lock();
        let table = guard.as_mut()?;
        let p = table.get(pid)?;
        if p.state != ProcessState::Sleeping
            || p.waiting_for != Some(IO_WAIT_SENTINEL)
            || !p.io_wait_trap_context.is_some()
            || p.trap_context_ptr == 0
        {
            // 写者已被唤醒（deadline/信号）或尚未入睡：ctx 留给其重放入口消费，
            // 此处绝不代笔，避免与重放竞态造成重复写入。
            return Some(false);
        }
        let cx = unsafe { &*(p.trap_context_ptr as *const crate::arch::TrapContext) };
        if cx.x[REG_SYSCALL] != SYS_WRITE && cx.x[REG_SYSCALL] != SYS_WRITEV {
            return Some(false);
        }
        let fd = cx.x[REG_A0];
        let fdp = table.get_fd_process(pid)?;
        let file = fdp.fd_table.get(fd).and_then(|s| s.as_ref())?;
        let f = file.file.as_file()?;
        if f.pipe_buffer_id() != Some(Arc::as_ptr(buffer) as usize) {
            return Some(false);
        }
        let ctx = ring.write_ctx.get(&pid)?;
        if crate::cmdline::wrdbg_enabled() {
            static B: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(64);
            if B.fetch_update(core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed, |n| if n==0 {None} else {Some(n-1)}).is_ok() {
                crate::println!("[WRHDBG] once pid={} a0_base={} off={} data={}", pid, ctx.a0_base, ctx.off, ctx.data.len());
            }
        }
        (ctx.a0_base, ctx.data.len(), ctx.off)
    };

    // ===== 读者代写：把 ctx 的 staging 缓冲拷入环（内核内存直拷，无用户页
    // 翻译、无缺页、无 EFAULT 分支）=====
    let (completed, _n) = {
        let PipeBuffer {
            write_ctx,
            buf,
            tail,
            len,
            capacity,
            ..
        } = ring;
        let ctx = write_ctx.get_mut(&pid)?;
        let take = (*capacity - *len).min(ctx.data.len() - ctx.off);
        if take == 0 {
            return Some(false); // 环又满（free 按拷贝前计算，罕见）
        }
        let mut copied = 0usize;
        while copied < take {
            let chunk = (*capacity - *tail).min(take - copied);
            buf[*tail..*tail + chunk]
                .copy_from_slice(&ctx.data[ctx.off + copied..ctx.off + copied + chunk]);
            *tail = (*tail + chunk) % *capacity;
            *len += chunk;
            copied += chunk;
        }
        ctx.off += copied;
        (ctx.off == ctx.data.len(), copied)
    };
    if !completed {
        return Some(false); // 部分进展已入 ctx.off，等下一轮读者消费
    }
    // ===== 完成：撤销重放、置 a0 = a0_base + **ctx 数据总长**、直切唤醒 =====
    // 2026-08-14 根因修复：初版写 a0 = a0_base + n（n=本轮拷贝量）——读者缓冲
    // 小于剩余数据时直递分多轮拷贝，末轮 n 只是最后一小段 → 写者收到短 a0 →
    // write_all 把尾部重写一遍 → cargo metadata JSON 尾部重复损坏 → 反复重试
    // 25 分钟爬行（verify-pipeoff 二分实锤）。a0 必须 = a0_base + data.len()。
    let total = a0_base + data_len;
    {
        let mut guard = PROCESS_TABLE.lock();
        let table = guard.as_mut()?;
        let p = table.get_mut(pid)?;
        let cx = unsafe { &mut *(p.trap_context_ptr as *mut crate::arch::TrapContext) };
        cx.sepc = cx.sepc.wrapping_add(4);
        cx.x[REG_A0] = total;
        p.io_wait_trap_context.clear();
        if crate::sched::process_sched::IO_ACCOUNT_ACTIVE
            .load(core::sync::atomic::Ordering::Relaxed)
        {
            p.io_wchar = p.io_wchar.saturating_add(total as u64);
            p.io_syscw = p.io_syscw.saturating_add(1);
        }
        table.mark_runnable_handoff(pid);
    }
    ring.write_ctx.remove(&pid);
    ring.write_waiters.remove_pid(pid);
    if crate::cmdline::wrdbg_enabled() {
        static B: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(32);
        if B.fetch_update(core::sync::atomic::Ordering::Relaxed, core::sync::atomic::Ordering::Relaxed, |n| if n==0 {None} else {Some(n-1)}).is_ok() {
            crate::println!("[WRHDBG] complete pid={} a0={}", pid, total);
        }
    }
    Some(true)
}

/// b126「pipe 阻塞读直递」：写者确认读者 Sleeping + io_wait armed 且正阻塞在对
/// 本 pipe 的 read(2) 上后，直接把数据写进读者已驻留可写的用户页、撤销 syscall
/// 重放（sepc+=4）并置 a0=n，再 mark_runnable。读者被调度后立即带返回值回用户
/// 态，省去整条 read syscall 重放 + 环形缓冲双拷贝。
///
/// 任一校验不满足即返回 Err，由调用方回退常规「入环 + 唤醒 + 重放」路径兜底：
/// - 读者已被他人唤醒 / 尚未完成入睡（state != Sleeping 或 io_wait 未 armed）；
/// - 阻塞点不是 read(2)（readv/splice/tee 等同样入队 PipeRead）；
/// - 读者 fd 与本 pipe buffer 不对应（dup/CLONE_FILES 情形按 fd 表 owner 解析）；
/// - 读者缓冲小于本次写长（直递会引入短写 + 与并发写者交织的原子性问题）；
/// - 用户页未驻留可写（CoW/缺页需 handle_page_fault，可能分配内存/取其他锁，
///   持 PROCESS_TABLE 期间不做）。
fn try_pipe_read_handoff(
    buffer: &Arc<SpinLock<PipeBuffer>>,
    pid: usize,
    data: &[u8],
) -> Result<usize, ()> {
    #[cfg(target_arch = "riscv64")]
    const REG_A0: usize = 10;
    #[cfg(target_arch = "loongarch64")]
    const REG_A0: usize = 4;
    #[cfg(target_arch = "riscv64")]
    const REG_SYSCALL: usize = 17;
    #[cfg(target_arch = "loongarch64")]
    const REG_SYSCALL: usize = 11;
    const SYS_READ: usize = 63;

    use crate::mm::{VirtAddr, PAGE_SIZE};
    use crate::sched::process::{ProcessState, IO_WAIT_SENTINEL, PROCESS_TABLE};

    let mut guard = PROCESS_TABLE.lock();
    let table = guard.as_mut().ok_or(())?;
    let (user_ptr, mm) = {
        let p = table.get(pid).ok_or(())?;
        if p.state != ProcessState::Sleeping
            || p.waiting_for != Some(IO_WAIT_SENTINEL)
            || !p.io_wait_trap_context.is_some()
            || p.trap_context_ptr == 0
        {
            return Err(());
        }
        // 睡眠期间 trap context 原地留在读者 kstack 顶（io_wait 重放模型），
        // 寄存器即 read(2) 原始入参：a0=fd a1=buf a2=len，sepc 已回退到 ecall。
        let cx = unsafe { &*(p.trap_context_ptr as *const crate::arch::TrapContext) };
        if cx.x[REG_SYSCALL] != SYS_READ {
            return Err(());
        }
        if cx.x[REG_A0 + 2] < data.len() {
            return Err(());
        }
        let fd = cx.x[REG_A0];
        let fdp = table.get_fd_process(pid).ok_or(())?;
        let file = fdp.fd_table.get(fd).and_then(|s| s.as_ref()).ok_or(())?;
        let f = file.file.as_file().ok_or(())?;
        if f.pipe_buffer_id() != Some(Arc::as_ptr(buffer) as usize) {
            return Err(());
        }
        let p = table.get(pid).ok_or(())?;
        (cx.x[REG_A0 + 1], p.mm.as_ref().ok_or(())?.clone())
    };
    {
        let addr_space = mm.lock();
        let end = user_ptr.checked_add(data.len()).ok_or(())?;
        // 两遍：先确认全部页已驻留可写（失败零副作用回退），再拷贝。
        let mut segs: [(usize, usize); 3] = [(0, 0); 3];
        let mut nsegs = 0usize;
        let mut covered = user_ptr;
        while covered < end {
            let va = VirtAddr(covered);
            let seg_len = (PAGE_SIZE - va.page_offset()).min(end - covered);
            let ppn = addr_space
                .page_table()
                .translate_writable(va.floor())
                .ok_or(())?;
            if nsegs == segs.len() {
                return Err(()); // 超大缓冲不走直递（pipe 单笔写本就有限）
            }
            segs[nsegs] = (
                crate::mm::phys_to_kernel_vaddr((ppn.0 << 12) | va.page_offset()),
                seg_len,
            );
            nsegs += 1;
            covered += seg_len;
        }
        let mut off = 0usize;
        for &(kva, seg_len) in segs[..nsegs].iter() {
            unsafe {
                core::ptr::copy_nonoverlapping(data.as_ptr().add(off), kva as *mut u8, seg_len);
            }
            off += seg_len;
        }
    }
    {
        let p = table.get_mut(pid).ok_or(())?;
        let cx = unsafe { &mut *(p.trap_context_ptr as *mut crate::arch::TrapContext) };
        cx.sepc = cx.sepc.wrapping_add(4);
        cx.x[REG_A0] = data.len();
        p.io_wait_trap_context.clear();
        if crate::sched::process_sched::IO_ACCOUNT_ACTIVE
            .load(core::sync::atomic::Ordering::Relaxed)
        {
            p.io_rchar = p.io_rchar.saturating_add(data.len() as u64);
            p.io_syscr = p.io_syscr.saturating_add(1);
        }
    }
    // 同步交接点：写者随即阻塞在自己的 read 上，读者走直切槽免入队/选队。
    table.mark_runnable_handoff(pid);
    Ok(data.len())
}

fn current_pid() -> Option<usize> {
    let pid = crate::sched::process::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed);
    if pid == 0 { None } else { Some(pid) }
}
