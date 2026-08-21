//! SysV message queue implementation.
//!
//! Extracted from `sysv.rs` to align with Linux `ipc/msg.c`.

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use crate::sched::wait_queue::{WaitQueue, WaitReason};
use crate::sync::SpinLock;
use crate::syscall::user::{read_user_buffer, write_user_buffer};
use crate::sched::process::{MQ_WOKEN_SENTINEL, MQ_SLEEP_SENTINEL};
use super::sysv::{IpcPerm, now_sec, current_pid_locked, current_euid_egid_or_root};

/// 获取当前进程的 IPC namespace ID（用于 IPC 资源隔离）
fn get_current_ipc_ns_id() -> usize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get(pid) {
                return process.nsproxy.lock().ipc_ns.id.0;
            }
        }
    }
    0 // init namespace
}

// ============================================================================
// SysV msg queue — 真化（perm + 时间戳 + lspid/lrpid + blocking + IPC_STAT 真布局）
// ============================================================================

#[derive(Debug, Clone)]
pub struct SysvMessage {
    pub mtype: isize,
    pub data: Vec<u8>,
}

/// Dynamic MSGMNI limit (writable via /proc/sys/kernel/msgmni)
pub static MSGMNI: AtomicUsize = AtomicUsize::new(32000);

pub struct MsgQueue {
    pub perm: IpcPerm,
    pub messages: VecDeque<SysvMessage>,
    pub max_bytes: usize,
    pub last_stime: u64, // 秒
    pub last_rtime: u64,
    pub last_ctime: u64,
    pub last_spid:  i32,
    pub last_rpid:  i32,
    /// IPC namespace ID that owns this queue (for namespace isolation)
    pub ipc_ns_id: usize,
}

pub static MSG_TABLE: SpinLock<BTreeMap<i32, MsgQueue>> = SpinLock::new(BTreeMap::new());
static MSG_KEY_INDEX: SpinLock<BTreeMap<i32, i32>> = SpinLock::new(BTreeMap::new());
static MSG_NEXT_ID: SpinLock<i32> = SpinLock::new(100);

/// `/proc/sys/kernel/msg_next_id` (CONFIG_CHECKPOINT_RESTORE): the desired id
/// for the *next* allocated message queue. `-1` (default) means use the generic
/// cyclic allocator. After a successful `msgget` the kernel resets it to `-1`
/// (LTP `msgget04`/`msgget05`).
pub static MSG_REQUESTED_ID: AtomicI32 = AtomicI32::new(-1);

/// Read `/proc/sys/kernel/msg_next_id`.
pub fn msg_next_id() -> i32 {
    MSG_REQUESTED_ID.load(Ordering::Relaxed)
}

/// Write `/proc/sys/kernel/msg_next_id`. Linux accepts `{0..INT_MAX}` (and the
/// reset value `-1`); other negatives are rejected.
pub fn set_msg_next_id(val: i32) -> bool {
    if val < -1 {
        return false;
    }
    MSG_REQUESTED_ID.store(val, Ordering::Relaxed);
    true
}
/// IDs that have been IPC_RMID'd — lookup on these returns EIDRM not EINVAL.
static MSG_REMOVED_IDS: SpinLock<alloc::collections::BTreeSet<i32>> = SpinLock::new(alloc::collections::BTreeSet::new());

/// 阻塞 sender (queue 满) / receiver (queue 空 or 无匹配 mtype) 的等待表。
/// key = msqid。
static MSG_SND_WAITERS: SpinLock<BTreeMap<i32, WaitQueue>> = SpinLock::new(BTreeMap::new());
static MSG_RCV_WAITERS: SpinLock<BTreeMap<i32, WaitQueue>> = SpinLock::new(BTreeMap::new());


fn ipc_wake_one(map: &SpinLock<BTreeMap<i32, WaitQueue>>, id: i32) {
    let target = {
        let mut tbl = map.lock();
        let entry = tbl.get(&id).and_then(|q| q.dequeue_one());
        if let Some(q) = tbl.get(&id) {
            if q.is_empty() { tbl.remove(&id); }
        }
        entry
    };
    let pid = match target { Some(e) => e.pid, None => return };
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() { Some(t) => t, None => return };
    let was_sleeping = t.get(pid).map_or(false, |p| {
        p.state == crate::sched::process::ProcessState::Sleeping
    });
    if !was_sleeping { return; }
    if let Some(p) = t.get_mut(pid) {
        p.waiting_for = Some(MQ_WOKEN_SENTINEL);
    }
    t.mark_runnable(pid, false);
}

pub fn sys_msgget(key: i32, msgflg: i32) -> isize {
    const IPC_CREAT: i32 = 0o1000;
    const IPC_EXCL:  i32 = 0o2000;
    const IPC_PRIVATE: i32 = 0;

    let cur_ipc_ns = get_current_ipc_ns_id();
    let mut key_idx = MSG_KEY_INDEX.lock();
    if key != IPC_PRIVATE {
        if let Some(&existing) = key_idx.get(&key) {
            // IPC namespace 隔离：检查 queue 是否属于当前 ns
            let tbl = MSG_TABLE.lock();
            if let Some(q) = tbl.get(&existing) {
                if q.ipc_ns_id != cur_ipc_ns {
                    drop(tbl);
                    if msgflg & IPC_CREAT == 0 {
                        return -2; // -ENOENT: not visible in this ns
                    }
                    // Fall through to create new queue
                } else {
                    if msgflg & (IPC_CREAT | IPC_EXCL) == (IPC_CREAT | IPC_EXCL) {
                        return -17; // -EEXIST
                    }
                    let need = (msgflg as u32) & 0o777;
                    if need != 0 && !q.perm.access(need) {
                        return -13; // -EACCES
                    }
                    return existing as isize;
                }
            } else {
                drop(tbl);
                if msgflg & IPC_CREAT == 0 {
                    return -2; // -ENOENT
                }
            }
        } else {
            if msgflg & IPC_CREAT == 0 {
                return -2; // -ENOENT
            }
        }
    }
    // Enforce MSGMNI limit (max number of message queues)
    let tbl_count = { MSG_TABLE.lock().len() };
    if tbl_count >= MSGMNI.load(Ordering::Relaxed) { return -28; } // -ENOSPC
    // Id allocation. Linux `ipc_idr_alloc`: when `msg_next_id >= 0`
    // (CHECKPOINT_RESTORE) allocate the first free index *at or above* the
    // requested id, then reset `next_id` to -1; otherwise use the generic
    // cyclic allocator. msgget05 relies on "id already in use → pick another,
    // don't error", msgget04 on the exact-id + reset-to--1 semantics.
    let id = {
        let mut next = MSG_NEXT_ID.lock();
        let requested = MSG_REQUESTED_ID.load(Ordering::Relaxed);
        if requested >= 0 {
            MSG_REQUESTED_ID.store(-1, Ordering::Relaxed);
            let tbl = MSG_TABLE.lock();
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
    let mode = (msgflg as u32) & 0o777;
    let now = now_sec();
    let mut tbl = MSG_TABLE.lock();
    let ipc_ns_id = get_current_ipc_ns_id();
    tbl.insert(id, MsgQueue {
        perm: IpcPerm::new(key, mode),
        messages: VecDeque::new(),
        max_bytes: 16 * 1024,
        last_stime: 0, last_rtime: 0, last_ctime: now,
        last_spid: 0, last_rpid: 0,
        ipc_ns_id,
    });
    if key != IPC_PRIVATE {
        key_idx.insert(key, id);
    }
    id as isize
}

/// 写入 `struct msqid64_ds` (Linux rv64/la64 uapi, 共 120B = 48 perm + 24 三
/// time + 24 三 ulong (cbytes/qnum/qbytes) + 8 lspid+lrpid + 16 __unused[2])。
/// 注意：musl/glibc 的 `struct msqid_ds` 在这两个 64 位架构上正好 120B，
/// 多写 1 字节都会越界踩到测例相邻全局变量（msgctl12 的 results 指针被清零
/// 后 LTP harness NULL 解引用 SIGSEGV）。
fn write_msqid_ds(q: &MsgQueue, buf: *mut u8) -> isize {
    let used: usize = q.messages.iter().map(|m| m.data.len()).sum();
    let mut out = Vec::with_capacity(120);
    q.perm.write_into(&mut out);                                  // 0..48
    out.extend_from_slice(&(q.last_stime as i64).to_le_bytes());  // 48..56
    out.extend_from_slice(&(q.last_rtime as i64).to_le_bytes());  // 56..64
    out.extend_from_slice(&(q.last_ctime as i64).to_le_bytes());  // 64..72
    out.extend_from_slice(&(used as u64).to_le_bytes());          // 72..80 msg_cbytes
    out.extend_from_slice(&(q.messages.len() as u64).to_le_bytes()); // 80..88 msg_qnum
    out.extend_from_slice(&(q.max_bytes as u64).to_le_bytes());   // 88..96 msg_qbytes
    out.extend_from_slice(&(q.last_spid as i32).to_le_bytes());   // 96..100
    out.extend_from_slice(&(q.last_rpid as i32).to_le_bytes());   // 100..104
    out.extend_from_slice(&[0u8; 16]);                            // 104..120 __unused[2]
    if write_user_buffer(buf, &out).is_err() { return -14; }
    0
}

pub fn sys_msgctl(msqid: i32, cmd: i32, buf: *mut u8) -> isize {
    let buf = buf as usize;
    const IPC_RMID: i32 = 0;
    const IPC_STAT: i32 = 2;
    const IPC_SET:  i32 = 1;
    const IPC_INFO: i32 = 3;
    const MSG_STAT: i32 = 11;
    const MSG_INFO: i32 = 12;
    const MSG_STAT_ANY: i32 = 13;
    match cmd {
        IPC_RMID => {
            // 锁外检 owner，避免 remove 后发现无权需重插。
            {
                let tbl = MSG_TABLE.lock();
                match tbl.get(&msqid) {
                    Some(q) if !q.perm.owner_check() => return -1, // -EPERM
                    None => return -22,
                    _ => {}
                }
            }
            let mut tbl = MSG_TABLE.lock();
            if let Some(q) = tbl.remove(&msqid) {
                let mut key_idx = MSG_KEY_INDEX.lock();
                key_idx.remove(&q.perm.key);
                drop(key_idx);
                drop(tbl);
                // Record this ID as removed so future lookups return EIDRM
                MSG_REMOVED_IDS.lock().insert(msqid);
                // 唤醒所有 sender + receiver（它们再入会看到 -EIDRM）
                let mut snd = MSG_SND_WAITERS.lock();
                if let Some(q) = snd.remove(&msqid) {
                    while let Some(e) = q.dequeue_one() {
                        let mut tbl = crate::sched::process::PROCESS_TABLE.lock();
                        if let Some(t) = tbl.as_mut() {
                            if let Some(p) = t.get_mut(e.pid) { p.waiting_for = Some(MQ_WOKEN_SENTINEL); }
                            t.mark_runnable(e.pid, false);
                        }
                    }
                }
                let mut rcv = MSG_RCV_WAITERS.lock();
                if let Some(q) = rcv.remove(&msqid) {
                    while let Some(e) = q.dequeue_one() {
                        let mut tbl = crate::sched::process::PROCESS_TABLE.lock();
                        if let Some(t) = tbl.as_mut() {
                            if let Some(p) = t.get_mut(e.pid) { p.waiting_for = Some(MQ_WOKEN_SENTINEL); }
                            t.mark_runnable(e.pid, false);
                        }
                    }
                }
                return 0;
            }
            -22
        }
        IPC_STAT => {
            let tbl = MSG_TABLE.lock();
            match tbl.get(&msqid) {
                Some(q) => {
                    if !q.perm.access(0o444) { return -13; } // -EACCES
                    write_msqid_ds(q, buf as *mut u8)
                }
                None => -22,
            }
        }
        MSG_STAT | MSG_STAT_ANY => {
            // Linux 同 SEM_STAT*：MSG_STAT 需 R；MSG_STAT_ANY (Linux 4.17+) 跳过。
            let tbl = MSG_TABLE.lock();
            match tbl.get(&msqid) {
                Some(q) => {
                    if cmd == MSG_STAT && !q.perm.access(0o444) { return -13; }
                    let r = write_msqid_ds(q, buf as *mut u8);
                    if r < 0 { r } else { msqid as isize }
                }
                None => -22,
            }
        }
        MSG_INFO => {
            // 真 `struct msginfo` + 返回值是 "max msqid 在 used 数组里的索引"。
            // Linux 这里 msginfo 字段 msgpool/msgmap/msgtql 被复用为运行时统
            // 计：msgpool = # queues, msgmap = # msgs in system, msgtql = # bytes.
            let (max_id, n_queues, n_msgs, n_bytes) = {
                let tbl = MSG_TABLE.lock();
                let max_id = tbl.keys().copied().max().unwrap_or(0);
                let n_queues = tbl.len();
                let (n_msgs, n_bytes) = tbl.values().fold((0usize, 0usize), |(c, b), q| {
                    let bb: usize = q.messages.iter().map(|m| m.data.len()).sum();
                    (c + q.messages.len(), b + bb)
                });
                (max_id, n_queues, n_msgs, n_bytes)
            };
            let mut out = Vec::with_capacity(32);
            out.extend_from_slice(&(n_queues as i32).to_le_bytes()); // msgpool→#queues
            out.extend_from_slice(&(n_msgs as i32).to_le_bytes());   // msgmap→#msgs
            out.extend_from_slice(&(8192i32).to_le_bytes());         // msgmax
            out.extend_from_slice(&(16384i32).to_le_bytes());        // msgmnb
            out.extend_from_slice(&(32000i32).to_le_bytes());        // msgmni
            out.extend_from_slice(&(16i32).to_le_bytes());           // msgssz
            out.extend_from_slice(&(n_bytes as i32).to_le_bytes());  // msgtql→#bytes
            out.extend_from_slice(&(0xffffu16).to_le_bytes());       // msgseg
            out.extend_from_slice(&[0u8; 2]);                        // pad
            if buf != 0 {
                if write_user_buffer(buf as *mut u8, &out).is_err() { return -14; }
            }
            max_id as isize
        }
        IPC_SET => {
            // 用户写入：mode / uid / gid / qbytes（其他字段忽略）
            let bytes = match read_user_buffer(buf as *const u8, 96) {
                Ok(b) => b, Err(_) => return -14,
            };
            let new_uid  = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            let new_gid  = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
            let new_mode = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) & 0o777;
            let new_qbytes = u64::from_le_bytes(bytes[88..96].try_into().unwrap()) as usize;
            // 真 Linux：qbytes 只有 root (CAP_SYS_RESOURCE) 可扩大（超过
            // msgmnb=16384）；所有人可缩小或设为低于上限。
            let (euid, _) = current_euid_egid_or_root();
            const MSGMNB: usize = 16384;
            if new_qbytes > MSGMNB && euid != 0 { return -1; } // -EPERM
            let mut tbl = MSG_TABLE.lock();
            match tbl.get_mut(&msqid) {
                Some(q) => {
                    if !q.perm.owner_check() { return -1; } // -EPERM
                    q.perm.uid = new_uid;
                    q.perm.gid = new_gid;
                    q.perm.mode = new_mode;
                    if new_qbytes > 0 { q.max_bytes = new_qbytes; }
                    q.last_ctime = now_sec();
                    0
                }
                None => -22,
            }
        }
        IPC_INFO => {
            // 真 `struct msginfo` (Linux uapi/asm-generic/msg.h, 14B header +
            // 2B trailing seg = packed 30B；glibc 按 4B 对齐占 32B)。
            //   int msgpool = total bytes available system-wide (we report 1 MiB)
            //   int msgmap  = max msg map entries (deprecated, report 0)
            //   int msgmax  = max bytes per single message (per-queue max_bytes)
            //   int msgmnb  = max bytes per queue (== msgmax in our model)
            //   int msgmni  = max # queues system-wide (32000 = Linux default)
            //   int msgssz  = segment size (deprecated, 16)
            //   int msgtql  = system msg headers (dynamic, report 0)
            //   u16 msgseg  = max segments (0xffff)
            // 调用方传 IPC_INFO 时 buf 是 *struct msginfo*，返回值是最大 msqid
            // 索引（Linux 行为：return highest used index in id array）。
            let highest = {
                let tbl = MSG_TABLE.lock();
                tbl.keys().copied().max().unwrap_or(0)
            };
            let mut out = Vec::with_capacity(32);
            out.extend_from_slice(&(1_048_576i32).to_le_bytes()); // msgpool
            out.extend_from_slice(&(0i32).to_le_bytes());         // msgmap
            out.extend_from_slice(&(8192i32).to_le_bytes());      // msgmax
            out.extend_from_slice(&(16384i32).to_le_bytes());     // msgmnb
            out.extend_from_slice(&(32000i32).to_le_bytes());     // msgmni
            out.extend_from_slice(&(16i32).to_le_bytes());        // msgssz
            out.extend_from_slice(&(0i32).to_le_bytes());         // msgtql
            out.extend_from_slice(&(0xffffu16).to_le_bytes());    // msgseg
            out.extend_from_slice(&[0u8; 2]);                     // pad to 32B
            if buf != 0 {
                if write_user_buffer(buf as *mut u8, &out).is_err() { return -14; }
            }
            highest as isize
        }
        _ => -22, // 未知 cmd 严格返 -EINVAL（之前的 `0` 是简化）
    }
}

pub fn sys_msgsnd(msqid: i32, msgp: *const u8, msgsz: usize, msgflg: i32) -> isize {
    const IPC_NOWAIT: i32 = 0o4000;
    if msgp.is_null() { return -14; } // -EFAULT: NULL msgp
    // Linux `do_msgsnd`: `if ((long)msgsz < 0) return -EINVAL`
    if (msgsz as isize) < 0 { return -22; }
    // struct msgbuf { long mtype; char mtext[msgsz]; }
    let total = match 8usize.checked_add(msgsz) {
        Some(t) => t,
        None => return -22, // -EINVAL: msgsz overflow
    };
    let buf = match read_user_buffer(msgp, total) {
        Ok(b) => b,
        Err(_) => return -14,
    };
    let mtype = i64::from_le_bytes(buf[0..8].try_into().unwrap()) as isize;
    if mtype <= 0 { return -22; }
    let pid = current_pid_locked() as i32;
    // 真 Linux `do_msgsnd`：最首次进入时检 W 权限（SEM_NONE 不会进这里）。
    {
        let tbl = MSG_TABLE.lock();
        match tbl.get(&msqid) {
            None => {
                drop(tbl);
                return if MSG_REMOVED_IDS.lock().contains(&msqid) { -43 } else { -22 };
            }
            Some(q) if !q.perm.access(0o222) => return -13, // -EACCES
            _ => {}
        }
    }
    let mut entered_block = false;
    loop {
        // 尝试 push。msqid 被 IPC_RMID 删除 → -EIDRM；从未存在 → -EINVAL。
        {
            let mut tbl = MSG_TABLE.lock();
            let q = match tbl.get_mut(&msqid) {
                Some(q) => q,
                None => {
                    drop(tbl);
                    return if MSG_REMOVED_IDS.lock().contains(&msqid) { -43 } else { -22 };
                }
            };
            let used: usize = q.messages.iter().map(|m| m.data.len()).sum();
            if used + msgsz <= q.max_bytes {
                q.messages.push_back(SysvMessage { mtype, data: buf[8..8+msgsz].to_vec() });
                q.last_stime = now_sec();
                q.last_spid = pid;
                drop(tbl);
                // 唤醒一个 receiver
                ipc_wake_one(&MSG_RCV_WAITERS, msqid);
                return 0;
            }
        }
        if msgflg & IPC_NOWAIT != 0 { return -11; } // -EAGAIN
        // 入睡 — 检查 signal / wake_by_peer
        if let Some(err) = msg_check_reentry(msqid, true) { return err; }
        msg_sleep_self(msqid, true);
        entered_block = true;
    }
}

pub fn sys_msgrcv(msqid: i32, msgp: *mut u8, msgsz: usize, msgtyp: isize, msgflg: i32) -> isize {
    const IPC_NOWAIT:  i32 = 0o4000;
    const MSG_NOERROR: i32 = 0o10000;
    const MSG_EXCEPT:  i32 = 0o20000;
    const MSG_COPY:    i32 = 0o40000;
    // Linux `do_msgrcv`：`if ((long) msgsz < 0) return -EINVAL`（msgsz 是
    // size_t，但内核按 long 判负）。msgrcv02 第 5 子项传 msgsz=-1 期望 EINVAL，
    // 旧实现缺该校验会把 -1 当成巨大 usize 直接成功 → TFAIL。
    if (msgsz as isize) < 0 { return -22; }
    let pid = current_pid_locked() as i32;
    // 真 Linux `do_msgrcv`：首次进入检 R 权限。
    {
        let tbl = MSG_TABLE.lock();
        match tbl.get(&msqid) {
            None => {
                drop(tbl);
                return if MSG_REMOVED_IDS.lock().contains(&msqid) { -43 } else { -22 };
            }
            Some(q) if !q.perm.access(0o444) => return -13, // -EACCES
            _ => {}
        }
    }
    // 缓冲区不可访问 → EFAULT（Linux 在 EACCES 之后、出队拷贝时报 EFAULT）。
    // msgrcv02 第 3 子项传 msgp=NULL 期望 EFAULT；旧实现在最前面错报 EINVAL。
    if msgp.is_null() { return -14; }
    let mut entered_block = false;
    loop {
        // 尝试 pop — msqid 被 IPC_RMID 删除 → -EIDRM；从未存在 → -EINVAL
        {
            let mut tbl = MSG_TABLE.lock();
            let q = match tbl.get_mut(&msqid) {
                Some(q) => q,
                None => {
                    drop(tbl);
                    return if MSG_REMOVED_IDS.lock().contains(&msqid) { -43 } else { -22 };
                }
            };
            // MSG_COPY: copy message at position `msgtyp` without removing (peek)
            if msgflg & MSG_COPY != 0 {
                // Linux requires IPC_NOWAIT with MSG_COPY
                if msgflg & IPC_NOWAIT == 0 { return -22; }
                let pos = msgtyp as usize;
                let msg = match q.messages.get(pos) {
                    Some(m) => m,
                    None => return -42, // -ENOMSG
                };
                if msg.data.len() > msgsz && msgflg & MSG_NOERROR == 0 {
                    return -7; // -E2BIG
                }
                let copy_len = msg.data.len().min(msgsz);
                let mtype = msg.mtype;
                let data_slice = msg.data[..copy_len].to_vec();
                drop(tbl);
                let mut out = Vec::with_capacity(8 + copy_len);
                out.extend_from_slice(&(mtype as i64).to_le_bytes());
                out.extend_from_slice(&data_slice);
                if write_user_buffer(msgp, &out).is_err() { return -14; }
                return copy_len as isize;
            }
            // 找符合 msgtyp 规则的消息：
            //   msgtyp==0: any message
            //   msgtyp>0 + MSG_EXCEPT: first message NOT of type msgtyp
            //   msgtyp>0: first message of type msgtyp
            //   msgtyp<0: lowest mtype ≤ |msgtyp|
            let idx = if msgtyp == 0 {
                if q.messages.is_empty() { None } else { Some(0) }
            } else if msgtyp > 0 {
                if msgflg & MSG_EXCEPT != 0 {
                    q.messages.iter().position(|m| m.mtype != msgtyp)
                } else {
                    q.messages.iter().position(|m| m.mtype == msgtyp)
                }
            } else {
                // 负值：找最小 mtype ≤ |msgtyp|（Linux 真实现要找最小，不是第一个）
                let limit = -msgtyp;
                let mut best: Option<(usize, isize)> = None;
                for (i, m) in q.messages.iter().enumerate() {
                    if m.mtype <= limit {
                        if let Some((_, bm)) = best {
                            if m.mtype < bm { best = Some((i, m.mtype)); }
                        } else {
                            best = Some((i, m.mtype));
                        }
                    }
                }
                best.map(|(i, _)| i)
            };
            if let Some(i) = idx {
                let msg = q.messages.remove(i).unwrap();
                if msg.data.len() > msgsz && msgflg & MSG_NOERROR == 0 {
                    // 放回去并返 -E2BIG
                    q.messages.insert(i, msg);
                    return -7;
                }
                let copy_len = msg.data.len().min(msgsz);
                q.last_rtime = now_sec();
                q.last_rpid = pid;
                drop(tbl);
                let mut out = Vec::with_capacity(8 + copy_len);
                out.extend_from_slice(&(msg.mtype as i64).to_le_bytes());
                out.extend_from_slice(&msg.data[..copy_len]);
                if write_user_buffer(msgp, &out).is_err() { return -14; }
                // 唤醒一个 sender
                ipc_wake_one(&MSG_SND_WAITERS, msqid);
                return copy_len as isize;
            }
        }
        if msgflg & IPC_NOWAIT != 0 { return -42; } // -ENOMSG
        if let Some(err) = msg_check_reentry(msqid, false) { return err; }
        msg_sleep_self(msqid, false);
        entered_block = true;
    }
}

/// SysV msg 的 re-entry 检查（无 deadline 路径 — msgsnd/msgrcv 不支持 timeout）。
fn msg_check_reentry(msqid: i32, is_send: bool) -> Option<isize> {
    let pid = current_pid_locked();
    let map = if is_send { &MSG_SND_WAITERS } else { &MSG_RCV_WAITERS };
    {
        let mut tbl = map.lock();
        if let Some(q) = tbl.get(&msqid) {
            q.remove_pid(pid);
            if q.is_empty() { tbl.remove(&msqid); }
        }
    }
    // 真 -EIDRM：msg queue 已被 IPC_RMID 删除 → -EIDRM 而非 -EINVAL。
    {
        let alive = MSG_TABLE.lock().contains_key(&msqid);
        if !alive {
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
    if pending_sig {
        p.clear_wait_state();
        return Some(-4); // -EINTR
    }
    if woken {
        p.clear_wait_state();
    }
    None
}

fn msg_sleep_self(msqid: i32, is_send: bool) {
    let pid = current_pid_locked();
    let map = if is_send { &MSG_SND_WAITERS } else { &MSG_RCV_WAITERS };
    {
        let mut tbl = map.lock();
        let q = tbl.entry(msqid).or_insert_with(WaitQueue::new);
        q.enqueue_pid(pid, WaitReason::Custom(if is_send { 100 } else { 101 }), None);
    }
    crate::sched::process::enter_wait_channel(
        if is_send { "msg_sender_wait" } else { "msg_receiver_wait" }
    );
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(t) = table.as_mut() {
            t.mark_sleeping(pid, Some(MQ_SLEEP_SENTINEL), 0);
        }
    }
    crate::sched::process::suspend_current_and_run_next();
}
