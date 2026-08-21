//! SysV semaphore implementation.
//!
//! Extracted from `sysv.rs` to align with Linux `ipc/sem.c`.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, Ordering};
use crate::sched::wait_queue::{WaitQueue, WaitReason};
use crate::sync::SpinLock;
use crate::syscall::user::{read_user_buffer, write_user_buffer};
use super::sysv::{IpcPerm, now_sec, current_pid_locked};
use crate::sched::process::{MQ_WOKEN_SENTINEL, MQ_SLEEP_SENTINEL};

/// 获取当前进程的 IPC namespace ID
fn get_current_ipc_ns_id() -> usize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get(pid) {
                return process.nsproxy.lock().ipc_ns.id.0;
            }
        }
    }
    0
}

// ============================================================================
// SysV semaphore — 真化（perm + 时间戳 + sempid + blocking + IPC_STAT 真布局）
// ============================================================================

/// 单个 sem 的真状态。`sempid` 记录最后一次成功对它执行 op 的 pid（Linux
/// `GETPID` 命令读这个字段）。
#[derive(Debug, Clone)]
pub struct SemVal {
    pub value: i32,
    pub sempid: i32,
}

pub struct SemSet {
    pub perm: IpcPerm,
    pub sems: Vec<SemVal>,
    pub last_otime: u64, // 最后一次 op 时间（秒）
    pub last_ctime: u64, // semget 或 IPC_SET 时间
    /// SEM_UNDO 真实现（Linux `ipc/sem.c::sem_undo_list`）：per-pid 累计
    /// 反向 adjustment 数组（长度 = sems.len()）。进程异常退出时，每个
    /// `undo[pid][num]` 会被加回 `sems[num].value`（结果 clip 到
    /// `[0, SEMVMX=32767]`）。这样守护进程崩了不会留下永远 hold 的 sem。
    /// SETVAL / SETALL / IPC_RMID 会清除对应 sem 的 undo。
    pub undo: BTreeMap<usize, Vec<i32>>,
    /// IPC namespace ID for isolation
    pub ipc_ns_id: usize,
}

pub static SEM_TABLE: SpinLock<BTreeMap<i32, SemSet>> = SpinLock::new(BTreeMap::new());
static SEM_KEY_INDEX: SpinLock<BTreeMap<i32, i32>> = SpinLock::new(BTreeMap::new());
static SEM_NEXT_ID: SpinLock<i32> = SpinLock::new(200);

/// `/proc/sys/kernel/sem_next_id` (CONFIG_CHECKPOINT_RESTORE): the desired id
/// for the *next* allocated semaphore set. `-1` (default) means use the generic
/// cyclic allocator. Reset to `-1` by the kernel after a successful `semget`
/// (LTP `semget05`/`semget06`).
pub static SEM_REQUESTED_ID: AtomicI32 = AtomicI32::new(-1);

/// Read `/proc/sys/kernel/sem_next_id`.
pub fn sem_next_id() -> i32 {
    SEM_REQUESTED_ID.load(Ordering::Relaxed)
}

/// Write `/proc/sys/kernel/sem_next_id`. Linux accepts `{0..INT_MAX}` (and the
/// reset value `-1`); other negatives are rejected.
pub fn set_sem_next_id(val: i32) -> bool {
    if val < -1 {
        return false;
    }
    SEM_REQUESTED_ID.store(val, Ordering::Relaxed);
    true
}

/// 阻塞 sem op（sem<0 或 sem!=0 等待 ZERO）的等待表。key = semid。
/// 每个等待 entry 在 `WaitEntry.reason = Custom(sem_num * 2 + (zero?0:1))`
/// 中编码"等的是哪个 sem 上的什么条件"，唤醒侧 try once。
/// IDs that have been IPC_RMID'd — lookup on these returns EIDRM not EINVAL.
static SEM_REMOVED_IDS: SpinLock<alloc::collections::BTreeSet<i32>> = SpinLock::new(alloc::collections::BTreeSet::new());
static SEM_WAITERS: SpinLock<BTreeMap<i32, WaitQueue>> = SpinLock::new(BTreeMap::new());

pub fn sys_semget(key: i32, nsems: i32, semflg: i32) -> isize {
    const IPC_CREAT: i32 = 0o1000;
    const IPC_EXCL:  i32 = 0o2000;
    const IPC_PRIVATE: i32 = 0;
    if nsems < 0 || nsems > 250 { return -22; }
    let cur_ipc_ns = get_current_ipc_ns_id();
    let mut key_idx = SEM_KEY_INDEX.lock();
    if key != IPC_PRIVATE {
        // IPC namespace 隔离：只有当 set 属于当前 ns 才算"已存在"（LTP
        // sem_comm：子进程进入新 IPC ns 后 semget(同 key) 必须拿到独立的 set，
        // 不得操作到父进程 ns 里的同 key set）。
        let same_ns = key_idx.get(&key).map(|&existing| {
            SEM_TABLE.lock().get(&existing).map(|s| s.ipc_ns_id == cur_ipc_ns).unwrap_or(false)
        }).unwrap_or(false);
        if same_ns {
            let existing = *key_idx.get(&key).unwrap();
            if semflg & (IPC_CREAT | IPC_EXCL) == (IPC_CREAT | IPC_EXCL) {
                return -17;
            }
            // 真 Linux `ipcget_public` for semget：查到已有 set 后还要根据
            // semflg 低 9 bit 反推访问需求 → ipcperms 。LTP `semget05` 测试
            // mode=0 的 set 被非则 uid 拒 -EACCES 。
            let need = (semflg as u32) & 0o777;
            if need != 0 {
                let tbl = SEM_TABLE.lock();
                if let Some(s) = tbl.get(&existing) {
                    if !s.perm.access(need) {
                        return -13; // -EACCES
                    }
                    // 另一个 Linux 检查：如果 nsems != 0 且 > set.sems.len() → -EINVAL
                    if nsems != 0 && (nsems as usize) > s.sems.len() {
                        return -22;
                    }
                }
            }
            return existing as isize;
        }
        if semflg & IPC_CREAT == 0 {
            return -2;
        }
        // create 分支还需 nsems > 0。Linux: 仅在 IPC_CREAT 且 set 不存在时发生。
        if nsems == 0 { return -22; }
    } else {
        // IPC_PRIVATE 同样需要 nsems > 0
        if nsems == 0 { return -22; }
    }
    // Enforce SEMMNI limit (max number of semaphore arrays)
    let tbl_count = { SEM_TABLE.lock().len() };
    if tbl_count >= 128 { return -28; } // -ENOSPC
    // Id allocation. Linux `ipc_idr_alloc`: when `sem_next_id >= 0`
    // (CHECKPOINT_RESTORE) allocate the first free index *at or above* the
    // requested id, then reset `next_id` to -1; otherwise generic cyclic.
    let id = {
        let mut next = SEM_NEXT_ID.lock();
        let requested = SEM_REQUESTED_ID.load(Ordering::Relaxed);
        if requested >= 0 {
            SEM_REQUESTED_ID.store(-1, Ordering::Relaxed);
            let tbl = SEM_TABLE.lock();
            let mut cand = requested;
            while tbl.contains_key(&cand) {
                cand = cand.checked_add(1).unwrap_or(1);
                if cand < 0 { cand = 1; }
            }
            if cand >= *next {
                *next = cand.wrapping_add(1).max(1);
            }
            cand
        } else {
            let id = *next;
            *next += 1;
            id
        }
    };
    let mode = (semflg as u32) & 0o777;
    let now = now_sec();
    let mut tbl = SEM_TABLE.lock();
    let ipc_ns_id = get_current_ipc_ns_id();
    tbl.insert(id, SemSet {
        perm: IpcPerm::new(key, mode),
        sems: alloc::vec![SemVal { value: 0, sempid: 0 }; nsems as usize],
        last_otime: 0,
        last_ctime: now,
        undo: BTreeMap::new(),
        ipc_ns_id,
    });
    if key != IPC_PRIVATE {
        key_idx.insert(key, id);
    }
    id as isize
}

/// `struct semid_ds` (Linux rv64/la64) = 48 perm + 8 otime + 8 ctime +
/// 8 nsems + 16 reserved = 88B.
fn write_semid_ds(s: &SemSet, buf: *mut u8) -> isize {
    let mut out = Vec::with_capacity(88);
    s.perm.write_into(&mut out);                                 // 0..48
    out.extend_from_slice(&(s.last_otime as i64).to_le_bytes()); // 48..56
    out.extend_from_slice(&(s.last_ctime as i64).to_le_bytes()); // 56..64
    out.extend_from_slice(&(s.sems.len() as u64).to_le_bytes()); // 64..72
    out.extend_from_slice(&[0u8; 16]);                           // 72..88 reserved[2]
    if write_user_buffer(buf, &out).is_err() { return -14; }
    0
}

pub fn sys_semctl(semid: i32, semnum: i32, cmd: i32, arg: usize) -> isize {
    const IPC_RMID: i32 = 0;
    const IPC_STAT: i32 = 2;
    const IPC_SET:  i32 = 1;
    const IPC_INFO: i32 = 3;
    const SEM_INFO: i32 = 19;
    const SEM_STAT: i32 = 18;
    const SEM_STAT_ANY: i32 = 20;
    const GETPID:   i32 = 11;
    const GETVAL:   i32 = 12;
    const GETALL:   i32 = 13;
    const GETNCNT:  i32 = 14;
    const GETZCNT:  i32 = 15;
    const SETVAL:   i32 = 16;
    const SETALL:   i32 = 17;
    match cmd {
        IPC_RMID => {
            // 锁外检 owner：避免 “remove后发现无权”需要重插。
            {
                let tbl = SEM_TABLE.lock();
                match tbl.get(&semid) {
                    Some(s) if !s.perm.owner_check() => return -1, // -EPERM
                    None => return -22,
                    _ => {}
                }
            }
            let mut tbl = SEM_TABLE.lock();
            if let Some(s) = tbl.remove(&semid) {
                let mut k = SEM_KEY_INDEX.lock();
                k.remove(&s.perm.key);
                drop(k);
                drop(tbl);
                SEM_REMOVED_IDS.lock().insert(semid);
                // 唤醒所有 waiter（它们再入会看到 -EIDRM）
                let mut w = SEM_WAITERS.lock();
                if let Some(q) = w.remove(&semid) {
                    while let Some(e) = q.dequeue_one() {
                        let mut tbl = crate::sched::process::PROCESS_TABLE.lock();
                        if let Some(t) = tbl.as_mut() {
                            if let Some(p) = t.get_mut(e.pid) {
                                p.waiting_for = Some(MQ_WOKEN_SENTINEL);
                            }
                            t.mark_runnable(e.pid, false);
                        }
                    }
                }
                return 0;
            }
            -22
        }
        IPC_STAT => {
            let tbl = SEM_TABLE.lock();
            match tbl.get(&semid) {
                Some(s) => {
                    if !s.perm.access(0o444) { return -13; } // -EACCES
                    write_semid_ds(s, arg as *mut u8)
                }
                None => -22,
            }
        }
        SEM_STAT | SEM_STAT_ANY => {
            // Linux 行为：SEM_STAT/SEM_STAT_ANY 把 semid 当作 internal id
            // 数组下标（不是 IPC id），写 semid_ds 并 **返回真 IPC id**。
            // SEM_STAT 需 R 权限；SEM_STAT_ANY (Linux 4.17+) 跳过 perm 检查。
            let tbl = SEM_TABLE.lock();
            match tbl.get(&semid) {
                Some(s) => {
                    if cmd == SEM_STAT && !s.perm.access(0o444) { return -13; }
                    let r = write_semid_ds(s, arg as *mut u8);
                    if r < 0 { r } else { semid as isize }
                }
                None => -22,
            }
        }
        IPC_SET => {
            // 用户写 perm.uid/gid/mode；其他字段忽略
            let bytes = match read_user_buffer(arg as *const u8, 48) {
                Ok(b) => b, Err(_) => return -14,
            };
            let new_uid  = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            let new_gid  = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
            let new_mode = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) & 0o777;
            let mut tbl = SEM_TABLE.lock();
            match tbl.get_mut(&semid) {
                Some(s) => {
                    if !s.perm.owner_check() { return -1; } // -EPERM
                    s.perm.uid = new_uid;
                    s.perm.gid = new_gid;
                    s.perm.mode = new_mode;
                    s.last_ctime = now_sec();
                    0
                }
                None => -22,
            }
        }
        IPC_INFO | SEM_INFO => {
            // struct seminfo: 10 ints = 40B.
            // For SEM_INFO, semusz = # used sets, semaem = total # semaphores.
            let (max_id, used_count, total_sems) = {
                let tbl = SEM_TABLE.lock();
                let total: usize = tbl.values().map(|s| s.sems.len()).sum();
                (tbl.keys().copied().max().unwrap_or(0), tbl.len(), total)
            };
            let mut out = Vec::with_capacity(40);
            out.extend_from_slice(&(0i32).to_le_bytes());      // semmap
            out.extend_from_slice(&(32000i32).to_le_bytes());  // semmni
            out.extend_from_slice(&(32000i32 * 250).to_le_bytes()); // semmns
            out.extend_from_slice(&(32000i32).to_le_bytes());  // semmnu
            out.extend_from_slice(&(250i32).to_le_bytes());    // semmsl
            out.extend_from_slice(&(32i32).to_le_bytes());     // semopm
            out.extend_from_slice(&(32i32).to_le_bytes());     // semume
            if cmd == SEM_INFO {
                out.extend_from_slice(&(used_count as i32).to_le_bytes()); // semusz = # sets
            } else {
                out.extend_from_slice(&(20i32).to_le_bytes()); // semusz = sizeof undo
            }
            out.extend_from_slice(&(32767i32).to_le_bytes());  // semvmx
            if cmd == SEM_INFO {
                out.extend_from_slice(&(total_sems as i32).to_le_bytes()); // semaem = total sems
            } else {
                out.extend_from_slice(&(32767i32).to_le_bytes()); // semaem = max adj-on-exit
            }
            if arg != 0 {
                if write_user_buffer(arg as *mut u8, &out).is_err() { return -14; }
            }
            // Both IPC_INFO and SEM_INFO return max used index
            max_id as isize
        }
        GETPID => {
            let tbl = SEM_TABLE.lock();
            if let Some(s) = tbl.get(&semid) {
                if !s.perm.access(0o444) { return -13; } // -EACCES
                if (semnum as usize) < s.sems.len() {
                    return s.sems[semnum as usize].sempid as isize;
                }
            }
            -22
        }
        GETVAL => {
            let tbl = SEM_TABLE.lock();
            if let Some(s) = tbl.get(&semid) {
                if !s.perm.access(0o444) { return -13; } // -EACCES
                if (semnum as usize) < s.sems.len() {
                    return s.sems[semnum as usize].value as isize;
                }
            }
            -22
        }
        GETNCNT | GETZCNT => {
            // Linux `count_semncnt(sma, semnum)` / `count_semzcnt`：在该 semid
            // 的 pending op 链上找 sem_num 匹配 + 等的是 sem<0 (NCNT) /
            // sem==0 (ZCNT) 的 entry。我们入队 sem_sleep_self 时把 (num,
            // kind) 编进 WaitReason::Custom(num*2 + kind)：
            //   kind=0 → wait-for-zero  → GETZCNT 命中
            //   kind=1 → wait-for-pos   → GETNCNT 命中
            {
                let tbl = SEM_TABLE.lock();
                match tbl.get(&semid) {
                    Some(s) if !s.perm.access(0o444) => return -13,
                    None => return -22,
                    _ => {}
                }
            }
            let tbl = SEM_WAITERS.lock();
            if let Some(q) = tbl.get(&semid) {
                let target_kind = if cmd == GETZCNT { 0 } else { 1 };
                let n = q.count_matching(|e| {
                    if let WaitReason::Custom(c) = e.reason {
                        // c == num*2 + kind；sem_num 越界 → 不命中。
                        let num = c / 2;
                        let kind = c & 1;
                        num == (semnum as usize) && kind == target_kind
                    } else { false }
                });
                return n as isize;
            }
            0
        }
        SETVAL => {
            // Linux `ipc/sem.c::semctl_setval`：`if (val > SEMVMX || val < 0)
            // return -ERANGE` 在取对象之前先判定（SEMVMX = 32767）。旧实现
            // 直接 `value = arg as i32` 缺范围校验，semctl05 (val=-1 / 65535)
            // 期望 ERANGE 却拿到成功，TFAIL。
            const SEMVMX: i32 = 32767;
            let val = arg as i32;
            if val < 0 || val > SEMVMX {
                return -34; // -ERANGE
            }
            let mut tbl = SEM_TABLE.lock();
            if let Some(s) = tbl.get_mut(&semid) {
                if !s.perm.access(0o222) { return -13; } // -EACCES
                if (semnum as usize) < s.sems.len() {
                    s.sems[semnum as usize].value = val;
                    // Linux: SETVAL also sets sempid to current pid
                    let pid = {
                        let ptbl = crate::sched::process::PROCESS_TABLE.lock();
                        ptbl.as_ref().and_then(|t| t.current).unwrap_or(0)
                    };
                    s.sems[semnum as usize].sempid = pid as i32;
                    s.last_ctime = now_sec();
                    // 真 Linux 行为：SETVAL 清除所有 pid 在 semnum 上的 undo
                    for adjs in s.undo.values_mut() {
                        if (semnum as usize) < adjs.len() {
                            adjs[semnum as usize] = 0;
                        }
                    }
                    drop(tbl);
                    sem_wake_all(semid);
                    return 0;
                }
            }
            -22
        }
        GETALL => {
            let tbl = SEM_TABLE.lock();
            if let Some(s) = tbl.get(&semid) {
                if !s.perm.access(0o444) { return -13; } // -EACCES
                let mut bytes = Vec::new();
                for v in &s.sems {
                    bytes.extend_from_slice(&(v.value as u16).to_le_bytes());
                }
                if write_user_buffer(arg as *mut u8, &bytes).is_err() { return -14; }
                return 0;
            }
            -22
        }
        SETALL => {
            let nsems = {
                let tbl = SEM_TABLE.lock();
                match tbl.get(&semid) {
                    Some(s) if !s.perm.access(0o222) => return -13, // -EACCES
                    Some(s) => Some(s.sems.len()),
                    None => None,
                }
            };
            let nsems = match nsems { Some(n) => n, None => return -22 };
            let bytes = match read_user_buffer(arg as *const u8, nsems * 2) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            // Linux `semctl_main` SETALL：copy 后逐项 `if (sem_io[i] > SEMVMX)
            // return -ERANGE`（值是 unsigned short，下界天然 ≥0）。semctl05
            // 第 2 子项传 65535 全数组，期望 ERANGE。
            const SEMVMX: u16 = 32767;
            let mut parsed = Vec::with_capacity(nsems);
            for i in 0..nsems {
                let v = u16::from_le_bytes(bytes[i*2..i*2+2].try_into().unwrap());
                if v > SEMVMX {
                    return -34; // -ERANGE
                }
                parsed.push(v as i32);
            }
            let mut tbl = SEM_TABLE.lock();
            if let Some(s) = tbl.get_mut(&semid) {
                for i in 0..nsems {
                    s.sems[i].value = parsed[i];
                }
                s.last_ctime = now_sec();
                // 真 Linux 行为：SETALL 清除整张 undo 表（`semctl_main`
                // SETALL 分支显式 `semundo->semadj[i] = 0` 遍全部 i）。
                s.undo.clear();
                drop(tbl);
                sem_wake_all(semid);
                return 0;
            }
            -22
        }
        _ => -22, // 未知 cmd 严格返 -EINVAL（之前的 `0` 是简化）
    }
}

/// 把 sembuf 数组解析出来。返 `(num, op, flg)` 三元组列表。
fn parse_sembuf(sops: *const u8, nsops: usize) -> Result<Vec<(usize, i32, i32)>, isize> {
    if sops.is_null() || nsops == 0 { return Err(-22); }
    // glibc rv64/la64: `struct sembuf { unsigned short sem_num; short sem_op;
    //                                    short sem_flg; }`  packed 6B, 但 musl
    // 与多数 ABI 实际按 8B 对齐。我们按 6B 紧排（与 musl `bits/sem.h` 一致）。
    let entry_size = 6;
    let bytes = read_user_buffer(sops, nsops * entry_size).map_err(|_| -14isize)?;
    let mut ops = Vec::with_capacity(nsops);
    for i in 0..nsops {
        let off = i * entry_size;
        let num = u16::from_le_bytes(bytes[off..off+2].try_into().unwrap()) as usize;
        let op  = i16::from_le_bytes(bytes[off+2..off+4].try_into().unwrap()) as i32;
        let flg = i16::from_le_bytes(bytes[off+4..off+6].try_into().unwrap()) as i32;
        ops.push((num, op, flg));
    }
    Ok(ops)
}

/// 真原子：尝试一次 apply 所有 ops。返回：
///   * `Ok(())` —— 全部 apply 成功，已修改 sem.value 并设 sempid；
///                 对带 SEM_UNDO 的 op 累计 `undo[pid][num] += -op`（清 0
///                 等价后回滚），值 clip 到 `[-SEMAEM, +SEMAEM]`（Linux
///                 行为，溢出返 -ERANGE 由调用方判断）。
///   * `Err((-11, _))` —— 至少一个 op 会阻塞，**不应用任何 op**
///   * `Err((err, _))` —— 其他错误
fn sem_try_apply(s: &mut SemSet, ops: &[(usize, i32, i32)], pid: i32) -> Result<(), (isize, usize)> {
    const IPC_NOWAIT: i32 = 0o4000;
    const SEM_UNDO:   i32 = 0o10000; // 0x1000，与 Linux 一致
    const SEMVMX:     i32 = 32767;
    const SEMAEM:     i32 = 32767;
    // 先 dry run — 收集每个 op 的新值
    let mut new_vals: Vec<(usize, i32)> = Vec::with_capacity(ops.len());
    // 用 working copy（避免污染真值）
    let mut working: Vec<i32> = s.sems.iter().map(|v| v.value).collect();
    // 计算 dry-run undo 累加（pid 一致）—— 全部 op 通过且 SEMAEM 不溢出才提交
    let mut undo_delta: Vec<(usize, i32)> = Vec::new();
    let nsems = s.sems.len();
    let prev_undo: Vec<i32> = match s.undo.get(&(pid as usize)) {
        Some(v) => v.clone(),
        None => alloc::vec![0i32; nsems],
    };
    let mut new_undo = prev_undo.clone();
    for (i, &(num, op, flg)) in ops.iter().enumerate() {
        if num >= s.sems.len() { return Err((-22, i)); }
        let cur = working[num];
        if op == 0 {
            // wait for zero
            if cur != 0 {
                if flg & IPC_NOWAIT != 0 { return Err((-11, i)); }
                return Err((-11, i));
            }
            new_vals.push((num, cur));
        } else if op > 0 {
            // increment — 总是可立即应用，但要 clip 上限 SEMVMX
            let new = cur.saturating_add(op);
            if new > SEMVMX { return Err((-34, i)); } // -ERANGE
            working[num] = new;
            new_vals.push((num, new));
            if flg & SEM_UNDO != 0 {
                let adj = new_undo[num].saturating_sub(op);
                if adj.abs() > SEMAEM { return Err((-34, i)); } // -ERANGE
                new_undo[num] = adj;
                undo_delta.push((num, adj));
            }
        } else {
            // op < 0 → decrement
            let new = cur + op;
            if new < 0 {
                if flg & IPC_NOWAIT != 0 { return Err((-11, i)); }
                return Err((-11, i));
            }
            working[num] = new;
            new_vals.push((num, new));
            if flg & SEM_UNDO != 0 {
                // -op = +|op| (op<0)，所以 undo += |op|
                let adj = new_undo[num].saturating_sub(op); // sub(-x) = +x
                if adj.abs() > SEMAEM { return Err((-34, i)); }
                new_undo[num] = adj;
                undo_delta.push((num, adj));
            }
        }
    }
    // 全部 dry run 通过 — 提交
    for (num, v) in new_vals {
        s.sems[num].value = v;
        s.sems[num].sempid = pid;
    }
    if !undo_delta.is_empty() {
        let entry = s.undo.entry(pid as usize).or_insert_with(|| alloc::vec![0i32; nsems]);
        // 直接用 new_undo（已含 dry-run 累计），保持原子性。
        *entry = new_undo;
    }
    s.last_otime = now_sec();
    Ok(())
}

/// 唤醒该 semid 上所有 waiter（典型在 op>0 / SETVAL / SETALL / IPC_RMID
/// 之后）。每个被唤醒方在 re-entry 时自己再 try once。
fn sem_wake_all(semid: i32) {
    let pids: Vec<usize> = {
        let mut tbl = SEM_WAITERS.lock();
        let mut v = Vec::new();
        if let Some(q) = tbl.get(&semid) {
            while let Some(e) = q.dequeue_one() {
                v.push(e.pid);
            }
        }
        if let Some(q) = tbl.get(&semid) { if q.is_empty() { tbl.remove(&semid); } }
        v
    };
    if pids.is_empty() { return; }
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(t) = table.as_mut() {
        for pid in pids {
            if let Some(p) = t.get_mut(pid) {
                p.waiting_for = Some(MQ_WOKEN_SENTINEL);
            }
            t.mark_runnable(pid, false);
        }
    }
}

pub fn sys_semop(semid: i32, sops: *const u8, nsops: usize) -> isize {
    sys_semtimedop(semid, sops, nsops, core::ptr::null())
}

pub fn sys_semtimedop(semid: i32, sops: *const u8, nsops: usize, timeout: *const u8) -> isize {
    let ops = match parse_sembuf(sops, nsops) {
        Ok(o) => o, Err(e) => return e,
    };
    // `timeout` 是 *const struct timespec*（相对！）。Linux semtimedop(2) 用
    // CLOCK_MONOTONIC + 相对 deadline。NULL → 无超时。
    let deadline = if timeout.is_null() {
        None
    } else {
        let buf = match read_user_buffer(timeout, 16) { Ok(b) => b, Err(_) => return -14 };
        let mut a = [0u8; 8];
        a.copy_from_slice(&buf[0..8]);  let sec  = i64::from_le_bytes(a);
        a.copy_from_slice(&buf[8..16]); let nsec = i64::from_le_bytes(a);
        if nsec < 0 || nsec >= 1_000_000_000 || sec < 0 { return -22; }
        let now_ticks = crate::arch::asm_helpers::rdtime();
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        let add_ticks = (sec as u64).saturating_mul(freq)
            + ((nsec as u64).saturating_mul(freq)) / 1_000_000_000;
        Some(now_ticks.saturating_add(add_ticks))
    };
    let pid = current_pid_locked() as i32;
    // 真 Linux `semtimedop`：进入循环前一次性做 perm 检查。
    // 如果任一 op 是 sem_op != 0 → 需 ALTER (W)；都是 sem_op == 0 → 需 R。
    // 与 LTP `semop05` 验证 mode=0400 set 拒绝 sem_op>0 一致。
    let need_alter = ops.iter().any(|&(_, op, _)| op != 0);
    let need = if need_alter { 0o222u32 } else { 0o444u32 };
    {
        let tbl = SEM_TABLE.lock();
        match tbl.get(&semid) {
            None => {
                drop(tbl);
                return if SEM_REMOVED_IDS.lock().contains(&semid) { -43 } else { -22 };
            }
            Some(s) if !s.perm.access(need) => return -13, // -EACCES
            Some(s) => {
                if let Some(&(num, _, _)) = ops.iter().find(|&&(n, _, _)| n >= s.sems.len()) {
                    let _ = num;
                    return -27; // -EFBIG
                }
            }
        }
    }
    loop {
        let block_op: (usize, i32) = {
            let mut tbl = SEM_TABLE.lock();
            let s = match tbl.get_mut(&semid) {
                Some(s) => s,
                None => {
                    drop(tbl);
                    return if SEM_REMOVED_IDS.lock().contains(&semid) { -43 } else { -22 };
                }
            };
            match sem_try_apply(s, &ops, pid) {
                Ok(()) => {
                    drop(tbl);
                    if ops.iter().any(|&(_, op, _)| op > 0) {
                        sem_wake_all(semid);
                    }
                    return 0;
                }
                Err((-11, idx)) => {
                    if ops.iter().any(|&(_, _, flg)| flg & 0o4000 != 0) {
                        return -11; // -EAGAIN
                    }
                    let (num, op, _) = ops[idx];
                    (num, if op == 0 { 0 } else { 1 })
                }
                Err((e, _)) => return e,
            }
        };
        if let Some(err) = sem_check_reentry(semid, deadline) { return err; }
        sem_sleep_self(semid, deadline, block_op.0, block_op.1);
    }
}

fn sem_check_reentry(semid: i32, deadline: Option<u64>) -> Option<isize> {
    let pid = current_pid_locked();
    {
        let mut tbl = SEM_WAITERS.lock();
        if let Some(q) = tbl.get(&semid) {
            q.remove_pid(pid);
            if q.is_empty() { tbl.remove(&semid); }
        }
    }
    // 真 -EIDRM：sem set 已被 IPC_RMID 删除 → 返 -EIDRM 而非 -EINVAL。
    {
        let sem_alive = SEM_TABLE.lock().contains_key(&semid);
        if !sem_alive {
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_mut() {
                if let Some(p) = t.get_mut(pid) {
                    p.clear_wait_state();
                }
            }
            return Some(-43); // -EIDRM
        }
    }
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = table.as_mut()?;
    let pending_sig = crate::kernel::signal::pending_interrupting_signal_for_process(t, pid, 0);
    let p = t.get_mut(pid)?;
    let woken = p.waiting_for == Some(MQ_WOKEN_SENTINEL);
    let sleeping = p.waiting_for == Some(MQ_SLEEP_SENTINEL);
    if sleeping {
        if let Some(d) = deadline {
            let now = crate::arch::asm_helpers::rdtime();
            if now >= d {
                p.clear_wait_state();
                return Some(-110);
            }
        }
    }
    if pending_sig {
        p.clear_wait_state();
        return Some(-4);
    }
    if woken {
        p.clear_wait_state();
    }
    None
}

/// **进程退出 hook（Linux `ipc/sem.c::exit_sem`）**：对每张 SemSet 找到
/// 该 pid 的 undo 数组，应用回 sem.value（结果 clip 到 `[0, SEMVMX]`），
/// 然后删除 entry。若回滚导致某 sem 变正，唤醒该 sem 上等待者重新 try。
///
/// 不持任何锁调用。pid 取自当前进程上下文（do_exit/exit_group 时已确定）。
pub fn sem_undo_for_pid(pid: usize) {
    const SEMVMX: i32 = 32767;
    // 收集需要唤醒的 semid 列表（应用 undo 让 sem 变正后通知 waiter）
    let mut wake_ids: Vec<i32> = Vec::new();
    {
        let mut tbl = SEM_TABLE.lock();
        for (&id, s) in tbl.iter_mut() {
            let adj = match s.undo.remove(&pid) {
                Some(v) => v,
                None => continue,
            };
            let mut any_increase = false;
            for (i, delta) in adj.iter().enumerate() {
                if *delta == 0 || i >= s.sems.len() { continue; }
                let new = s.sems[i].value.saturating_add(*delta).clamp(0, SEMVMX);
                if new > s.sems[i].value { any_increase = true; }
                s.sems[i].value = new;
                s.sems[i].sempid = pid as i32;
            }
            s.last_otime = now_sec();
            if any_increase { wake_ids.push(id); }
        }
    }
    for id in wake_ids { sem_wake_all(id); }
}

fn sem_sleep_self(semid: i32, deadline: Option<u64>, sem_num: usize, kind: i32) {
    let pid = current_pid_locked();
    {
        let mut tbl = SEM_WAITERS.lock();
        let q = tbl.entry(semid).or_insert_with(WaitQueue::new);
        // 真编码 (sem_num, kind) 进 reason，供 GETNCNT/GETZCNT introspect。
        // kind=0 wait-for-zero (op==0)；kind=1 wait-for-pos (op<0)。
        q.enqueue_pid(pid, WaitReason::Custom(sem_num * 2 + kind as usize), deadline);
    }
    crate::sched::process::enter_wait_channel("sem_wait");
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            let wc = deadline.unwrap_or(0) as usize;
            t.mark_sleeping(pid, Some(MQ_SLEEP_SENTINEL), wc);
        }
    }
    crate::sched::process::suspend_current_and_run_next();
}

// ── shared helpers ──────────────────────────────────────────────────────────

