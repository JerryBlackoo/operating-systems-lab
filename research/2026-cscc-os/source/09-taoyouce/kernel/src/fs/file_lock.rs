//! POSIX advisory file lock implementation — Slice 2.x T1-1 (real).
//!
//! Supports both POSIX `F_SETLK / F_SETLKW / F_GETLK` (per-process owner) and
//! Linux open-file-description locks `F_OFD_SETLK / F_OFD_SETLKW / F_OFD_GETLK`
//! (per-open-file owner identified by the `FileDescriptor` Arc address). Both
//! variants share one `LOCK_TABLE` so cross-variant conflicts are honoured
//! (POSIX semantics require it).
//!
//! Key correctness properties:
//!   - **range split on unlock**: unlocking a strict sub-range of an existing
//!     lock leaves the surrounding regions still locked
//!   - **conflict detection**: write-write or write-read across different
//!     owners, with byte-range overlap test (`l_len==0` ⇒ EOF)
//!   - **real blocking** on `F_SETLKW`: caller is suspended and woken when
//!     a holder releases overlapping range or exits; pending signals abort
//!     the wait with `-EINTR`
//!   - **automatic release**: POSIX locks released when the *process* closes
//!     ANY fd pointing at the file (Linux behaviour) and on process exit;
//!     OFD locks released when the *last* fd reference dies
//!   - **stable owner id for OFD**: derived from `FileDescriptor` Arc raw
//!     pointer address — guaranteed unique while the open file is alive
//!
//! Linux `struct flock` (RV64 / LA64) layout — 32 bytes:
//!   i16 l_type; i16 l_whence; pad[4]; i64 l_start; i64 l_len; i32 l_pid; pad[4]
//!
//! Lock ordering:
//!   1. `LOCK_TABLE.inner`
//!   2. `LOCK_WAITERS.inner`
//!   3. (released) → `PROCESS_TABLE.inner`

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sched::process::ProcessState;
use crate::sync::SpinLock;

/// `l_type` 值
pub const F_RDLCK: i16 = 0; // shared read lock
pub const F_WRLCK: i16 = 1; // exclusive write lock
pub const F_UNLCK: i16 = 2; // unlock

/// `errno` constants (negated when returned to userspace).
pub const EAGAIN: i32 = 11;
pub const EINTR:  i32 = 4;
pub const EDEADLK: i32 = 35;

/// Lock owner — distinguishes POSIX (per-pid) and OFD (per-open-file) locks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockOwner {
    /// POSIX `F_SETLK*` — owner is the calling pid.
    Pid(usize),
    /// Linux `F_OFD_SETLK*` — owner is the open file description identified by
    /// the raw `Arc<FileDescriptor>` pointer address.
    Ofd(usize),
}

impl LockOwner {
    /// Report-facing pid used to fill `flock.l_pid` for `F_GETLK`. OFD locks
    /// report `-1` per Linux convention.
    pub fn reported_pid(self) -> i32 {
        match self {
            LockOwner::Pid(pid) => pid as i32,
            LockOwner::Ofd(_)   => -1,
        }
    }
}

/// 单条文件锁记录
#[derive(Debug, Clone)]
pub struct FileLockEntry {
    pub l_type: i16,
    pub l_whence: i16,
    pub l_start: i64,
    pub l_len: i64,   // 0 = 锁到 EOF
    pub owner: LockOwner,
}

impl FileLockEntry {
    /// Inclusive-exclusive byte range `[start, end)`. `len == 0` means EOF.
    fn range(&self) -> (i64, i64) {
        let end = if self.l_len == 0 { i64::MAX } else { self.l_start.saturating_add(self.l_len) };
        (self.l_start, end)
    }

    /// 检测两条锁是否冲突（即不能同时存在）。
    /// - 同 owner 不冲突（允许 upgrade / extension）
    /// - 两条都是 read 不冲突
    /// - 区间不相交不冲突
    pub fn conflicts(&self, other: &FileLockEntry) -> bool {
        if self.owner == other.owner {
            return false;
        }
        if self.l_type == F_RDLCK && other.l_type == F_RDLCK {
            return false;
        }
        let (a_s, a_e) = self.range();
        let (b_s, b_e) = other.range();
        !(a_e <= b_s || b_e <= a_s)
    }
}

/// 全局 inode-id → locks 列表。inode_id 来源于 `Inode::ino()`。
pub static LOCK_TABLE: SpinLock<BTreeMap<usize, Vec<FileLockEntry>>> = SpinLock::new(BTreeMap::new());
static LOCK_INODES: AtomicUsize = AtomicUsize::new(0);

/// Pids currently sleeping inside `F_SETLKW` / `F_OFD_SETLKW`. Each waiter is
/// pinned to a particular inode so we wake the right ones on release.
static LOCK_WAITERS: SpinLock<Vec<LockWaiter>> = SpinLock::new(Vec::new());

#[derive(Clone)]
struct LockWaiter {
    pid: usize,
    inode_id: usize,
    requested: FileLockEntry,
}

/// Compute a stable OFD owner id from an `Arc<FileDescriptor>` (raw pointer
/// address). Lifetime: valid as long as at least one fd still references it.
pub fn ofd_id_from_arc<T>(arc: &Arc<T>) -> usize {
    Arc::as_ptr(arc) as usize
}

pub fn posix_owner_for_pid(pid: usize) -> usize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.get(pid))
        .map(|p| p.tgid)
        .unwrap_or(pid)
}

/// 解析用户 `struct flock` 字节流为 `FileLockEntry`。`buf` 至少 24 字节。
/// 解析后的 owner 字段是占位（`LockOwner::Pid(0)`）；调用方必须用真实
/// owner 覆盖。
pub fn parse_flock(buf: &[u8], current_offset: i64, file_size: i64) -> Option<FileLockEntry> {
    if buf.len() < 24 { return None; }
    let l_type   = i16::from_le_bytes(buf[0..2].try_into().ok()?);
    let l_whence = i16::from_le_bytes(buf[2..4].try_into().ok()?);
    let l_start  = i64::from_le_bytes(buf[8..16].try_into().ok()?);
    let l_len    = i64::from_le_bytes(buf[16..24].try_into().ok()?);
    // F_OFD_SETLK 要求 l_whence == SEEK_SET 且解析后绝对偏移即 l_start。
    let abs_start = match l_whence {
        0 => l_start,                    // SEEK_SET
        1 => current_offset.checked_add(l_start)?,  // SEEK_CUR
        2 => file_size.checked_add(l_start)?,       // SEEK_END
        _ => return None,
    };
    if abs_start < 0 { return None; }
    // l_len < 0 in POSIX means "len bytes ending at l_start" (i.e. range
    // [start+len, start)). Normalise to non-negative len with adjusted start.
    let (norm_start, norm_len) = if l_len < 0 {
        let new_start = abs_start.checked_add(l_len)?;
        if new_start < 0 { return None; }
        (new_start, -l_len)
    } else {
        (abs_start, l_len)
    };
    Some(FileLockEntry {
        l_type, l_whence,
        l_start: norm_start,
        l_len: norm_len,
        owner: LockOwner::Pid(0), // placeholder — caller MUST overwrite
    })
}

/// 序列化 `FileLockEntry` 回用户 `struct flock` 32 字节。
pub fn serialize_flock(lock: &FileLockEntry) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[0..2].copy_from_slice(&lock.l_type.to_le_bytes());
    out[2..4].copy_from_slice(&lock.l_whence.to_le_bytes());
    out[8..16].copy_from_slice(&lock.l_start.to_le_bytes());
    out[16..24].copy_from_slice(&lock.l_len.to_le_bytes());
    out[24..28].copy_from_slice(&lock.owner.reported_pid().to_le_bytes());
    out
}

/// `F_GETLK / F_OFD_GETLK`：检查 `requested` 在该 inode 是否会冲突。返回
/// 首条冲突锁信息（按 POSIX 要求填入），或 None 表示可获得。
pub fn get_lock(inode_id: usize, requested: &FileLockEntry) -> Option<FileLockEntry> {
    if LOCK_INODES.load(Ordering::Relaxed) == 0 {
        return None;
    }
    let table = LOCK_TABLE.lock();
    table.get(&inode_id).and_then(|list| {
        list.iter()
            .filter(|existing| requested.conflicts(existing))
            .min_by_key(|existing| {
                let (start, end) = existing.range();
                (start, end)
            })
            .cloned()
    })
}

/// `F_SETLK / F_OFD_SETLK`（非阻塞）。成功 → `Ok(())`，冲突 → `Err(EAGAIN)`。
/// `F_UNLCK` 会按区间 split 既有同 owner 锁，必要时拆成两段。
pub fn set_lock_nb(inode_id: usize, requested: FileLockEntry) -> Result<(), i32> {
    let mut table = LOCK_TABLE.lock();

    if requested.l_type == F_UNLCK {
        if let Some(list) = table.get_mut(&inode_id) {
            unlock_range(list, requested.owner, requested.range());
            if list.is_empty() {
                table.remove(&inode_id);
                LOCK_INODES.fetch_sub(1, Ordering::Relaxed);
            }
        }
        drop(table);
        wake_lock_waiters(inode_id);
        return Ok(());
    }

    let new_inode_entry = !table.contains_key(&inode_id);
    let list = table.entry(inode_id).or_insert_with(Vec::new);

    // 冲突检查（仅考察不同 owner 的写/读冲突）。
    for existing in list.iter() {
        if requested.conflicts(existing) {
            return Err(EAGAIN);
        }
    }

    // 先 unlock 自己在重合区间内的既有锁，再插入新锁（实现 upgrade /
    // type-change 的 atomic 替换）。
    unlock_range(list, requested.owner, requested.range());
    list.push(requested);
    if new_inode_entry {
        LOCK_INODES.fetch_add(1, Ordering::Relaxed);
    }
    Ok(())
}

/// `F_SETLKW / F_OFD_SETLKW`（阻塞）。冲突时把当前 pid 入 wait queue 并挂起，
/// 直到所持 owner 释放或被信号打断。`current_pid` 为当前 PID（用于
/// `mark_runnable` / pending-signal 检查）；`signal_check` 为闭包，返回
/// true 表示有 pending unblocked signal，触发 `-EINTR`。
pub fn set_lock_w<F>(
    inode_id: usize,
    requested: FileLockEntry,
    current_pid: usize,
    signal_check: F,
) -> Result<(), i32>
where
    F: Fn() -> bool,
{
    // Linux `locks_lock_inode_wait` 模式：先入等待队列再探锁。否则
    // “探锁失败 → 入队”之间的窗口内若持有者正好释放，唤醒扫描
    // 看不到本 waiter，睡眠（无 deadline）就永久丢失唤醒。
    loop {
        register_waiter(current_pid, inode_id, requested.clone());
        match set_lock_nb(inode_id, requested.clone()) {
            Ok(()) => {
                unregister_waiter(current_pid, inode_id);
                return Ok(());
            }
            Err(EAGAIN) => {
                if deadlock_would_occur(inode_id, &requested) {
                    unregister_waiter(current_pid, inode_id);
                    return Err(EDEADLK);
                }
                if signal_check() {
                    unregister_waiter(current_pid, inode_id);
                    return Err(EINTR);
                }
                suspend_for_lock();
                if signal_check() {
                    unregister_waiter(current_pid, inode_id);
                    return Err(EINTR);
                }
                // 唤醒后 fall through 重试。
            }
            Err(e) => {
                unregister_waiter(current_pid, inode_id);
                return Err(e);
            }
        }
    }
}

fn deadlock_would_occur(inode_id: usize, requested: &FileLockEntry) -> bool {
    let target_owner = match requested.owner {
        LockOwner::Pid(owner) => owner,
        LockOwner::Ofd(_) => return false,
    };
    let table = LOCK_TABLE.lock();
    let waiters = LOCK_WAITERS.lock();
    if let Some(list) = table.get(&inode_id) {
        for existing in list.iter() {
            if requested.conflicts(existing) {
                if let LockOwner::Pid(blocking_owner) = existing.owner {
                    let mut visited = Vec::new();
                    if owner_wait_chain_reaches(&table, &waiters, blocking_owner, target_owner, &mut visited) {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn owner_wait_chain_reaches(
    table: &BTreeMap<usize, Vec<FileLockEntry>>,
    waiters: &[LockWaiter],
    owner: usize,
    target_owner: usize,
    visited: &mut Vec<usize>,
) -> bool {
    if owner == target_owner {
        return true;
    }
    if visited.iter().any(|seen| *seen == owner) {
        return false;
    }
    visited.push(owner);
    for waiter in waiters.iter() {
        if waiter.requested.owner != LockOwner::Pid(owner) {
            continue;
        }
        if let Some(list) = table.get(&waiter.inode_id) {
            for existing in list.iter() {
                if waiter.requested.conflicts(existing) {
                    if let LockOwner::Pid(next_owner) = existing.owner {
                        if owner_wait_chain_reaches(table, waiters, next_owner, target_owner, visited) {
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

fn register_waiter(pid: usize, inode_id: usize, requested: FileLockEntry) {
    let mut w = LOCK_WAITERS.lock();
    if let Some(waiter) = w.iter_mut().find(|x| x.pid == pid && x.inode_id == inode_id) {
        waiter.requested = requested;
    } else {
        w.push(LockWaiter { pid, inode_id, requested });
    }
}

fn unregister_waiter(pid: usize, inode_id: usize) {
    let mut w = LOCK_WAITERS.lock();
    w.retain(|x| !(x.pid == pid && x.inode_id == inode_id));
}

fn suspend_for_lock() {
    // Slice 2.5 wchan：fcntl(F_SETLKW) 阻塞 → Linux `posix_lock_file_wait` /
    // `flock_lock_inode_wait`。指名为 `fcntl_setlk_wait`。
    crate::sched::process::enter_wait_channel("fcntl_setlk_wait");
    crate::sched::process::suspend_current_for_lock_wait();
}

/// 释放完锁后，唤醒该 inode 上所有挂起的 waiter（让它们重试）。
fn wake_lock_waiters(inode_id: usize) {
    let waiters: Vec<usize> = {
        let mut w = LOCK_WAITERS.lock();
        let mut woken = Vec::new();
        w.retain(|x| {
            if x.inode_id == inode_id {
                woken.push(x.pid);
                false
            } else {
                true
            }
        });
        woken
    };
    if waiters.is_empty() {
        return;
    }
    let mut t = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(table) = t.as_mut() {
        for pid in waiters {
            let Some(state) = table.get(pid).map(|process| process.state) else {
                continue;
            };
            if state == ProcessState::Sleeping {
                let _ = table.mark_runnable(pid, true);
            } else {
                if let Some(process) = table.get_mut(pid) {
                    process.waitqueue_woken = true;
                    process.waiting_for = None;
                    process.wakeup_channel = 0;
                    process.wait_channel = None;
                }
                let _ = table.mark_runnable(pid, false);
            }
        }
    }
}

/// 按区间从 `list` 中切除属于 `owner` 的部分。重合既有锁会被 split 成
/// `[old.start, unlock_start)` 与 `[unlock_end, old.end)` 两段（任一段可能为空）。
fn unlock_range(list: &mut Vec<FileLockEntry>, owner: LockOwner, (u_s, u_e): (i64, i64)) {
    if u_s >= u_e {
        return;
    }
    let mut next: Vec<FileLockEntry> = Vec::with_capacity(list.len());
    for entry in list.drain(..) {
        if entry.owner != owner {
            next.push(entry);
            continue;
        }
        let (e_s, e_e) = entry.range();
        // No overlap → keep as-is.
        if e_e <= u_s || u_e <= e_s {
            next.push(entry);
            continue;
        }
        // Left fragment [e_s, u_s).
        if e_s < u_s {
            let new_len = u_s - e_s;
            next.push(FileLockEntry {
                l_type: entry.l_type,
                l_whence: entry.l_whence,
                l_start: e_s,
                l_len: new_len,
                owner: entry.owner,
            });
        }
        // Right fragment [u_e, e_e).
        if u_e < e_e {
            let new_len = if e_e == i64::MAX { 0 } else { e_e - u_e };
            next.push(FileLockEntry {
                l_type: entry.l_type,
                l_whence: entry.l_whence,
                l_start: u_e,
                l_len: new_len,
                owner: entry.owner,
            });
        }
        // The middle [u_s, u_e) is dropped (unlocked).
    }
    *list = next;
}

/// 进程退出时释放该 pid 在所有 inode 上的 POSIX 锁。OFD 锁不受影响
/// （由 finalize_closed_fd 在最后一个 fd 关闭时释放）。
pub fn release_all_locks_for_pid(pid: usize) {
    // 阻塞在 F_SETLKW 中被杀的进程不会走 unregister_waiter；残留
    // 条目会污染死锁检测的等待链并误唤醒复用该 pid 的新进程。
    LOCK_WAITERS.lock().retain(|w| w.pid != pid);
    if LOCK_INODES.load(Ordering::Relaxed) == 0 {
        return;
    }
    let owner = LockOwner::Pid(posix_owner_for_pid(pid));
    let mut touched: Vec<usize> = Vec::new();
    {
        let mut table = LOCK_TABLE.lock();
        let inodes: Vec<usize> = table.keys().copied().collect();
        for ino in inodes {
            if let Some(list) = table.get_mut(&ino) {
                let before = list.len();
                list.retain(|e| e.owner != owner);
                if list.len() != before {
                    touched.push(ino);
                }
                if list.is_empty() {
                    table.remove(&ino);
                    LOCK_INODES.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }
    for ino in touched {
        wake_lock_waiters(ino);
    }
}

/// fd close（last reference）时释放该 pid 在该 inode 上的所有 POSIX 锁。
/// POSIX 要求：close *任何*指向该文件的 fd 都释放所有 POSIX 锁。
pub fn release_posix_locks_for_inode_pid(inode_id: usize, pid: usize) {
    if LOCK_INODES.load(Ordering::Relaxed) == 0 {
        return;
    }
    let owner = LockOwner::Pid(posix_owner_for_pid(pid));
    let removed = {
        let mut table = LOCK_TABLE.lock();
        let mut removed = false;
        if let Some(list) = table.get_mut(&inode_id) {
            let before = list.len();
            list.retain(|e| e.owner != owner);
            removed = list.len() != before;
            if list.is_empty() {
                table.remove(&inode_id);
                LOCK_INODES.fetch_sub(1, Ordering::Relaxed);
            }
        }
        removed
    };
    if removed {
        wake_lock_waiters(inode_id);
    }
}

// ============================================================================
// BSD flock(2) advisory file lock — Slice 2.x T4 真化。独立 per-inode 表，
// 不与 POSIX/OFD byte-range 锁共用 list（与 Linux `fs/locks.c` 相同语义）。
// ============================================================================

/// `FlockEntry { fd_owner, kind }` —— per-fd-obj 全文件级 advisory lock。
/// `kind`：1 = `LOCK_SH`，2 = `LOCK_EX`。
#[derive(Debug, Clone, Copy)]
struct FlockEntry {
    fd_owner: usize, // Arc<FileDescriptor> 指针 hash（dup 共享同 owner）
    kind: u8,
}

static FLOCK_TABLE: SpinLock<alloc::collections::BTreeMap<usize, Vec<FlockEntry>>> =
    SpinLock::new(alloc::collections::BTreeMap::new());
static FLOCK_INODES: AtomicUsize = AtomicUsize::new(0);

/// 尝试给 `inode_id` 上 BSD flock，`kind` 为 1 (SH) 或 2 (EX)。冲突返 false。
///
/// 同 owner 重复加锁视为 atomic upgrade/downgrade（替换原 entry）。
pub fn flock_try_set(inode_id: usize, fd_owner: usize, kind: u8) -> bool {
    let mut table = FLOCK_TABLE.lock();
    let new_inode_entry = !table.contains_key(&inode_id);
    let list = table.entry(inode_id).or_insert_with(Vec::new);
    // 检查冲突（排除同 owner — 同 owner 是 upgrade/downgrade，不冲突）
    for e in list.iter() {
        if e.fd_owner == fd_owner { continue; }
        // 任一方 EX → 冲突；双方 SH → 共存
        if e.kind == 2 || kind == 2 {
            return false;
        }
    }
    // 移除同 owner 旧 entry 后再插入新 kind
    list.retain(|e| e.fd_owner != fd_owner);
    list.push(FlockEntry { fd_owner, kind });
    if new_inode_entry {
        FLOCK_INODES.fetch_add(1, Ordering::Relaxed);
    }
    true
}

/// 释放 `fd_owner` 在 `inode_id` 上的 BSD flock（LOCK_UN 或 fd close）。
pub fn flock_release(inode_id: usize, fd_owner: usize) {
    if FLOCK_INODES.load(Ordering::Relaxed) == 0 {
        return;
    }
    let removed = {
        let mut table = FLOCK_TABLE.lock();
        if let Some(list) = table.get_mut(&inode_id) {
            let before = list.len();
            list.retain(|e| e.fd_owner != fd_owner);
            let touched = list.len() != before;
            if list.is_empty() {
                table.remove(&inode_id);
                FLOCK_INODES.fetch_sub(1, Ordering::Relaxed);
            }
            touched
        } else {
            false
        }
    };
    if removed {
        // 复用 byte-range lock 的 waiter wakeup（flock 阻塞者也通过
        // suspend_current_and_run_next → mark_runnable 重试，由通用调度
        // 唤醒；这里只额外触发 byte-range waiter，对 flock 自身循环重试
        // 也无害）。
        wake_lock_waiters(inode_id);
    }
}

/// 全表清理：fd close 时由 `finalize_closed_fd` 调，确保即使 close 路径
/// 没显式 LOCK_UN 也释放该 fd_obj 的 flock。
pub fn release_all_flock_for_fd_obj(fd_owner: usize) {
    if FLOCK_INODES.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut touched: Vec<usize> = Vec::new();
    {
        let mut table = FLOCK_TABLE.lock();
        let inodes: Vec<usize> = table.keys().copied().collect();
        for ino in inodes {
            if let Some(list) = table.get_mut(&ino) {
                let before = list.len();
                list.retain(|e| e.fd_owner != fd_owner);
                if list.len() != before { touched.push(ino); }
                if list.is_empty() {
                    table.remove(&ino);
                    FLOCK_INODES.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }
    for ino in touched { wake_lock_waiters(ino); }
}

/// 释放该 OFD 在所有 inode 上的锁（由 finalize_closed_fd 在最后一个 fd
/// reference drop 时调）。
pub fn release_all_locks_for_ofd(ofd_id: usize) {
    if LOCK_INODES.load(Ordering::Relaxed) == 0 {
        return;
    }
    let owner = LockOwner::Ofd(ofd_id);
    let mut touched: Vec<usize> = Vec::new();
    {
        let mut table = LOCK_TABLE.lock();
        let inodes: Vec<usize> = table.keys().copied().collect();
        for ino in inodes {
            if let Some(list) = table.get_mut(&ino) {
                let before = list.len();
                list.retain(|e| e.owner != owner);
                if list.len() != before {
                    touched.push(ino);
                }
                if list.is_empty() {
                    table.remove(&ino);
                    LOCK_INODES.fetch_sub(1, Ordering::Relaxed);
                }
            }
        }
    }
    for ino in touched {
        wake_lock_waiters(ino);
    }
}
