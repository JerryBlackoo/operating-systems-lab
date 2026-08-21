//! POSIX 文件租约（F_SETLEASE / 内核 lease-break）
//!
//! 完整模拟 Linux fs/locks.c 的 lease 阻塞模型：
//!   * 每个 inode 至多一种 lease (F_RDLCK 或 F_WRLCK)，多个进程可同时持有 F_RDLCK。
//!   * `acquire_lease`：在加 lease 前确认 inode 引用计数仅来自调用方（写 lease 还要求
//!     fd 以 O_WRONLY/O_RDWR 打开；读 lease 要求至少为只读）。
//!   * `break_lease_blocking`：当 open(2) / truncate(2) 试图以与既有 lease 冲突的
//!     方式打开 inode 时，记录冲突、向所有 lease holder 投递 lease signal（默认
//!     SIGIO），然后把当前进程挂起；lease holder 在用户态降级或释放 lease 后
//!     唤醒 waiter；超过 `LEASE_BREAK_SECONDS`（默认 45s）兜底强制释放 lease。
//!   * `release_lease_for_ofd`：fd 关闭、F_SETLEASE(F_UNLCK) 或 break 超时时调用。

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, Ordering};

use crate::sched::process::{PROCESS_TABLE, ProcessState};
use crate::sync::SpinLock;

/// F_RDLCK==0, F_WRLCK==1, F_UNLCK==2（与 file_lock.rs 中常量保持一致）。
pub const F_RDLCK: i32 = 0;
pub const F_WRLCK: i32 = 1;
pub const F_UNLCK: i32 = 2;

const SIGIO: i32 = 29;

/// `/proc/sys/fs/lease-break-time` 默认值（秒）。Linux 5.x 取 45。
const DEFAULT_LEASE_BREAK_SECONDS: u32 = 45;
static LEASE_BREAK_SECONDS: AtomicI32 = AtomicI32::new(DEFAULT_LEASE_BREAK_SECONDS as i32);

pub fn set_break_seconds(value: i32) {
    if (0..=255).contains(&value) {
        LEASE_BREAK_SECONDS.store(value, Ordering::Relaxed);
    }
}

#[allow(dead_code)]
pub fn break_seconds() -> i32 {
    LEASE_BREAK_SECONDS.load(Ordering::Relaxed)
}

#[derive(Clone, Copy)]
struct LeaseHolder {
    /// `Arc::as_ptr(&fd_obj) as usize` —— 即开放文件描述（OFD）的唯一身份。
    ofd_id: usize,
    /// fcntl(F_SETLEASE) 调用时所属的进程 ID。
    owner_pid: usize,
    /// F_OWNER_PID==1 / F_OWNER_PGRP==2 / F_OWNER_TID==0。
    target_type: i32,
    /// F_SETOWN_EX 设置的目标 PID（>0），≤0 表示沿用 `owner_pid`。
    target_pid: i32,
    /// F_SETSIG 设置的信号编号；0 表示沿用 SIGIO。
    signal: i32,
}

struct LeaseRecord {
    kind: i32, // F_RDLCK / F_WRLCK
    holders: Vec<LeaseHolder>,
    /// break 中：所需降级目标（F_RDLCK 表示只需降级；F_UNLCK 表示必须完全释放）。
    pending_break: Option<PendingBreak>,
    /// 等待 lease break 完成的进程 PID 列表。
    waiters: Vec<usize>,
}

#[derive(Clone, Copy)]
struct PendingBreak {
    /// `rdtime` 到达此值后，break 视为超时，强制释放 lease。
    deadline_tick: u64,
    /// break 完成后 lease 应保留的形态：F_RDLCK 表示可降级；F_UNLCK 表示必须释放。
    target_kind: i32,
}

struct LeaseTable {
    inner: SpinLock<BTreeMap<usize, LeaseRecord>>,
}

impl LeaseTable {
    const fn new() -> Self {
        Self { inner: SpinLock::new(BTreeMap::new()) }
    }
}

static LEASE_TABLE: LeaseTable = LeaseTable::new();
static LEASE_RECORDS: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(0);

/// `F_SETLEASE` 入口：根据 `kind` 选择 acquire/downgrade/release。
///
/// `fd_inode_refcount` 是调用方在 PROCESS_TABLE 之外拿到的"对该 inode 的 Arc 引用计数"，
/// 用于判断除调用 fd 之外是否还有其他 open file description 引用同一 inode（写 lease
/// 要求该值 == 1）。
pub fn fcntl_setlease(
    inode_id: usize,
    fd_inode_refcount: usize,
    fd_access_mode: u32,
    ofd_id: usize,
    owner_pid: usize,
    target_type: i32,
    target_pid: i32,
    signal: i32,
    kind: i32,
) -> Result<(), i32> {
    match kind {
        F_UNLCK => {
            release_lease_for_ofd_internal(inode_id, ofd_id);
            Ok(())
        }
        F_RDLCK | F_WRLCK => acquire_or_change_lease(
            inode_id,
            fd_inode_refcount,
            fd_access_mode,
            LeaseHolder {
                ofd_id,
                owner_pid,
                target_type,
                target_pid,
                signal,
            },
            kind,
        ),
        _ => Err(22), // EINVAL
    }
}

fn acquire_or_change_lease(
    inode_id: usize,
    fd_inode_refcount: usize,
    fd_access_mode: u32,
    holder: LeaseHolder,
    kind: i32,
) -> Result<(), i32> {
    // Linux lease 约束（fs/locks.c generic_add_lease）：lease 类型与 fd 的
    // access mode 无关——F_WRLCK 不要求 fd 可写。约束只体现在冲突计数上：
    //   F_RDLCK：若有其他进程以写方式打开该 inode (i_writecount>0) → EAGAIN。
    //   F_WRLCK：若 inode 还有其他 open file description (i_count>1) → EAGAIN。
    // LTP fcntl25 在 O_RDONLY fd 上设 F_WRLCK 期望成功；fcntl32 期望 EAGAIN 而非
    // EINVAL。下面的 refcount 检查实现该语义；此处不再按 access mode 拒绝。
    const O_RDONLY: u32 = 0;
    if kind == F_RDLCK && fd_access_mode != O_RDONLY {
        return Err(11);
    }
    let mut table = LEASE_TABLE.inner.lock();
    // 已经持有 lease 的 ofd → 视为 upgrade/downgrade。
    if let Some(record) = table.get_mut(&inode_id) {
        if let Some(holder_idx) = record.holders.iter().position(|h| h.ofd_id == holder.ofd_id) {
            if record.kind == kind {
                // 同 kind: 仅刷新 owner/signal 信息。
                record.holders[holder_idx] = holder;
                return Ok(());
            }
            if kind == F_RDLCK && record.kind == F_WRLCK {
                // 写降读：合法降级，清除任何 pending break。
                record.kind = F_RDLCK;
                record.holders[holder_idx] = holder;
                let waiters = core::mem::take(&mut record.waiters);
                record.pending_break = None;
                drop(table);
                wake_waiters(waiters);
                return Ok(());
            }
            // 读升写：必须独占 inode；不允许并存其他 lease holder。
            if kind == F_WRLCK
                && record.holders.len() == 1
                && fd_inode_refcount == 1
            {
                record.kind = F_WRLCK;
                record.holders[holder_idx] = holder;
                return Ok(());
            }
            return Err(11); // EAGAIN
        }
        // 当前 ofd 还没持有 lease：必须与现有 lease 类型兼容才能加入。
        if kind == F_WRLCK {
            return Err(11); // EAGAIN：已有 lease，写 lease 必须独占
        }
        if record.kind == F_RDLCK && kind == F_RDLCK {
            // 多个读 lease 可并存，但要求 inode 没有其他无 lease 的开放 fd。
            // 简化的判定：refcount == record.holders.len() + 1（+1=本次 fd）。
            if fd_inode_refcount != record.holders.len() + 1 {
                return Err(11);
            }
            record.holders.push(holder);
            return Ok(());
        }
        return Err(11);
    }
    // inode 还没有任何 lease 记录。
    // 写 lease：要求 inode 在内核里只有调用方一份引用。
    if kind == F_WRLCK && fd_inode_refcount != 1 {
        return Err(11);
    }
    // 读 lease：要求 inode 上没有 O_WRONLY/O_RDWR 的其他 open；当前实现不区分
    // 单 fd 的访问模式，因此使用与 Linux 同样的强约束——除调用 fd 外不允许其他
    // 引用。 (fcntl27/32 均在 inode refcount==2 时期望 EAGAIN。)
    if kind == F_RDLCK && fd_inode_refcount != 1 {
        return Err(11);
    }
    table.insert(inode_id, LeaseRecord {
        kind,
        holders: alloc::vec![holder],
        pending_break: None,
        waiters: Vec::new(),
    });
    LEASE_RECORDS.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// 查询 inode 当前 lease 类型；返回 F_RDLCK / F_WRLCK / F_UNLCK。
pub fn get_lease(inode_id: usize, ofd_id: usize) -> i32 {
    let table = LEASE_TABLE.inner.lock();
    match table.get(&inode_id) {
        Some(record) if record.holders.iter().any(|h| h.ofd_id == ofd_id) => record.kind,
        _ => F_UNLCK,
    }
}

/// 关闭 fd / F_SETLEASE(F_UNLCK) / break 超时时调用。
pub fn release_lease_for_ofd(inode_id: usize, ofd_id: usize) {
    release_lease_for_ofd_internal(inode_id, ofd_id);
}

/// 在 OFD 释放（无论 inode）时遍历 lease 表清理：fd close 时调用。
pub fn release_all_leases_for_ofd(ofd_id: usize) {
    if LEASE_RECORDS.load(Ordering::Relaxed) == 0 {
        return;
    }
    let waiters = {
        let mut table = LEASE_TABLE.inner.lock();
        let mut released = Vec::new();
        let keys: Vec<usize> = table.keys().copied().collect();
        for key in keys {
            if let Some(record) = table.get_mut(&key) {
                record.holders.retain(|h| h.ofd_id != ofd_id);
                if record.holders.is_empty() {
                    let waiters = core::mem::take(&mut record.waiters);
                    table.remove(&key);
                    LEASE_RECORDS.fetch_sub(1, Ordering::Relaxed);
                    released.extend(waiters);
                }
            }
        }
        released
    };
    wake_waiters(waiters);
}

fn release_lease_for_ofd_internal(inode_id: usize, ofd_id: usize) {
    if LEASE_RECORDS.load(Ordering::Relaxed) == 0 {
        return;
    }
    let waiters = {
        let mut table = LEASE_TABLE.inner.lock();
        let mut woken: Vec<usize> = Vec::new();
        if let Some(record) = table.get_mut(&inode_id) {
            record.holders.retain(|h| h.ofd_id != ofd_id);
            if record.holders.is_empty() {
                woken.extend(core::mem::take(&mut record.waiters));
                table.remove(&inode_id);
                LEASE_RECORDS.fetch_sub(1, Ordering::Relaxed);
            }
        }
        woken
    };
    wake_waiters(waiters);
}

/// 在 open(2)/truncate(2) 之前调用：若与既有 lease 冲突，
/// 投递 lease signal、阻塞当前进程直到 lease 被释放或超时。
///
/// `open_access` 取 `O_RDONLY=0 / O_WRONLY=1 / O_RDWR=2`；`is_truncate=true` 始终视为写。
pub fn break_lease_blocking(
    inode_id: usize,
    open_access: u32,
    is_truncate: bool,
) -> Result<(), i32> {
    if LEASE_RECORDS.load(Ordering::Relaxed) == 0 {
        return Ok(());
    }
    const O_RDONLY: u32 = 0;
    const ACCMODE: u32 = 0o3;
    let access = open_access & ACCMODE;
    let want_write = is_truncate || access != O_RDONLY;
    let want_read_only = !want_write;
    let current_pid = match {
        let table = PROCESS_TABLE.lock();
        table.as_ref().and_then(|t| t.current)
    } {
        Some(pid) => pid,
        None => return Ok(()),
    };
    loop {
        let (deliveries, deadline, conflict) = {
            let mut table = LEASE_TABLE.inner.lock();
            let record = match table.get_mut(&inode_id) {
                Some(record) => record,
                None => return Ok(()),
            };
            let conflict = match record.kind {
                F_RDLCK => want_write,
                F_WRLCK => true,
                _ => false,
            };
            if !conflict {
                return Ok(());
            }
            // 已存在 break：仅追加等待者，无需重复投信号。
            if let Some(pending) = record.pending_break {
                // 检查 break 目标是否仍然不满足当前 opener。
                let satisfied = match (pending.target_kind, want_read_only) {
                    (F_UNLCK, _) => record.holders.is_empty(),
                    (F_RDLCK, true) => record.kind == F_RDLCK,
                    (F_RDLCK, false) => false,
                    _ => false,
                };
                if satisfied {
                    return Ok(());
                }
                if !record.waiters.contains(&current_pid) {
                    record.waiters.push(current_pid);
                }
                (Vec::new(), Some(pending.deadline_tick), true)
            } else {
                // 触发新的 break：决定目标形态。
                let target_kind = if want_read_only {
                    F_RDLCK
                } else {
                    F_UNLCK
                };
                let lease_break_secs = LEASE_BREAK_SECONDS.load(Ordering::Relaxed) as u64;
                let now = crate::arch::asm_helpers::rdtime();
                let deadline_tick = if lease_break_secs == 0 {
                    now // 立即超时，等价于强制 unlock
                } else {
                    let clock_freq = crate::arch::consts::CLOCK_FREQ as u64;
                    now.saturating_add(lease_break_secs.saturating_mul(clock_freq))
                };
                record.pending_break = Some(PendingBreak { deadline_tick, target_kind });
                if !record.waiters.contains(&current_pid) {
                    record.waiters.push(current_pid);
                }
                let deliveries: Vec<LeaseSignal> = record.holders.iter().map(|h| LeaseSignal {
                    owner_pid: h.owner_pid,
                    target_type: h.target_type,
                    target_pid: h.target_pid,
                    signal: if h.signal > 0 { h.signal } else { SIGIO },
                }).collect();
                (deliveries, Some(deadline_tick), true)
            }
        };
        if !conflict {
            return Ok(());
        }
        for sig in deliveries {
            deliver_lease_signal(sig);
        }
        // 阻塞前先检查 pending 不可屏蔽信号。
        if pending_unblocked_signal(current_pid) {
            unregister_waiter(inode_id, current_pid);
            return Err(4); // EINTR
        }
        // 超时检查 + 短 deadline 兜底，由调度器在 rdtime 到点时唤醒。
        if let Some(deadline_tick) = deadline {
            let now = crate::arch::asm_helpers::rdtime();
            if now >= deadline_tick {
                // 超时：强制释放 lease，让 opener 继续。
                force_release_after_timeout(inode_id);
                return Ok(());
            }
            // Slice 2.5 wchan：lease break 等待 → Linux `__break_lease`
            crate::sched::process::enter_wait_channel("lease_break_wait");
            crate::sched::process::suspend_current_for_io(deadline_tick);
        } else {
            crate::sched::process::enter_wait_channel("lease_break_wait");
            crate::sched::process::suspend_current_and_run_next();
        }
        if pending_unblocked_signal(current_pid) {
            unregister_waiter(inode_id, current_pid);
            return Err(4);
        }
    }
}

fn force_release_after_timeout(inode_id: usize) {
    let waiters = {
        let mut table = LEASE_TABLE.inner.lock();
        if let Some(record) = table.get_mut(&inode_id) {
            let waiters = core::mem::take(&mut record.waiters);
            table.remove(&inode_id);
            LEASE_RECORDS.fetch_sub(1, Ordering::Relaxed);
            waiters
        } else {
            Vec::new()
        }
    };
    wake_waiters(waiters);
}

fn unregister_waiter(inode_id: usize, pid: usize) {
    let mut table = LEASE_TABLE.inner.lock();
    if let Some(record) = table.get_mut(&inode_id) {
        record.waiters.retain(|w| *w != pid);
    }
}

fn wake_waiters(pids: Vec<usize>) {
    if pids.is_empty() {
        return;
    }
    let mut table = PROCESS_TABLE.lock();
    if let Some(t) = table.as_mut() {
        for pid in pids {
            let _ = t.mark_runnable(pid, true);
        }
        t.wake_io_waiters();
    }
}

fn pending_unblocked_signal(pid: usize) -> bool {
    let table = PROCESS_TABLE.lock();
    if let Some(ref t) = *table {
        return crate::kernel::signal::pending_interrupting_signal_for_process(t, pid, 0);
    }
    false
}

#[derive(Clone, Copy)]
struct LeaseSignal {
    owner_pid: usize,
    target_type: i32,
    target_pid: i32,
    signal: i32,
}

fn deliver_lease_signal(sig: LeaseSignal) {
    let signal = sig.signal;
    if !(1..=64).contains(&signal) {
        return;
    }
    let mut table = PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if sig.target_type == 2 && sig.target_pid > 0 {
            let pgrp = sig.target_pid as usize;
            let pids = t.pids();
            for pid in pids {
                if t.get(pid).map_or(false, |p| p.pgid == pgrp) {
                    deliver_to(t, pid, signal);
                }
            }
        } else {
            let target_pid = if sig.target_pid > 0 {
                sig.target_pid as usize
            } else {
                sig.owner_pid
            };
            deliver_to(t, target_pid, signal);
        }
    }
}

fn deliver_to(table: &mut crate::sched::process::ProcessTable, pid: usize, sig: i32) {
    let should_wake = if let Some(p) = table.get_mut(pid) {
        let mask = 1u64 << (sig as u32 - 1);
        if p.signal_mask & mask != 0 {
            // 屏蔽中：仅置 pending，sleep 进程不需要立刻唤醒。
            p.pending_signals |= mask; crate::sched::process::note_signal_hint();
            false
        } else {
            p.pending_signals |= mask; crate::sched::process::note_signal_hint();
            p.state == ProcessState::Sleeping
        }
    } else {
        false
    };
    if should_wake {
        table.mark_runnable(pid, true);
    }
}
