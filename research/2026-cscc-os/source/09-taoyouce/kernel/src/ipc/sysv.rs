//! SysV IPC shared types and helpers.
//!
//! Per-subsystem implementations live in `sysv_msg.rs` and `sysv_sem.rs`.

extern crate alloc;

use alloc::vec::Vec;

use crate::sync::SpinLock;

/// 与 Linux `struct ipc_perm` (asm-generic/ipcbuf.h) 对齐，rv64/la64 = 48B。
#[derive(Debug, Clone, Copy)]
pub struct IpcPerm {
    pub key:  i32,
    pub uid:  u32,
    pub gid:  u32,
    pub cuid: u32,
    pub cgid: u32,
    pub mode: u32, // S_IRUGO | S_IWUGO 等
    pub seq:  u16,
}

impl IpcPerm {
    pub(crate) fn new(key: i32, mode: u32) -> Self {
        // 真 Linux: 用 current_euid()/current_egid()。如果 PROCESS_TABLE 不
        // 可用（极早期初始化），退回 0 (root)。这样后续 IPC_SET / ipcperms
        // 才能反映真用户身份变化。
        let (uid, gid) = current_euid_egid_or_root();
        IpcPerm { key, uid, gid, cuid: uid, cgid: gid, mode: mode & 0o777, seq: 0 }
    }
    /// 写入 user `struct ipc_perm`（48B，含 2 个 reserved ulong）。
    pub(crate) fn write_into(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.key  as i32).to_le_bytes()); // 0..4
        out.extend_from_slice(&(self.uid  as u32).to_le_bytes()); // 4..8
        out.extend_from_slice(&(self.gid  as u32).to_le_bytes()); // 8..12
        out.extend_from_slice(&(self.cuid as u32).to_le_bytes()); // 12..16
        out.extend_from_slice(&(self.cgid as u32).to_le_bytes()); // 16..20
        out.extend_from_slice(&(self.mode as u32).to_le_bytes()); // 20..24
        out.extend_from_slice(&(0u16).to_le_bytes());             // 24..26 __pad1
        out.extend_from_slice(&(self.seq as u16).to_le_bytes());  // 26..28
        out.extend_from_slice(&[0u8; 4]);                         // 28..32 __pad2 + 4
        out.extend_from_slice(&[0u8; 16]);                        // 32..48 reserved[2]
    }

    /// 真 `ipcperms(ipcp, flag)`（Linux `ipc/util.c::ipcperms`）：
    /// * `flag` 取 `S_IRUGO`(0o444) / `S_IWUGO`(0o222) 二选一或按位或。
    /// * root (uid=0 / CAP_IPC_OWNER) → 直接放行。
    /// * 否则按 owner(uid 匹配) / group(gid 匹配) / other 三段提取 mode
    ///   对应位，全部满足才放行；任一缺失返 false → 调用方应返 -EACCES(-13)。
    pub(crate) fn access(&self, flag: u32) -> bool {
        let (euid, egid) = current_euid_egid_or_root();
        if euid == 0 { return true; } // CAP_IPC_OWNER (root)
        // 提取 owner/group/other mode 段
        let granted: u32 = if euid == self.uid {
            (self.mode >> 6) & 0o7
        } else if egid == self.gid {
            (self.mode >> 3) & 0o7
        } else {
            self.mode & 0o7
        };
        // 把 flag (S_IRUGO/S_IWUGO) 折叠成 3-bit RWX 需求
        let need = ((flag >> 6) | (flag >> 3) | flag) & 0o7;
        (granted & need) == need
    }

    /// 真 `ipc_owner_check(ipcp)`（Linux `ipc/util.c::ipc_check_perms`
    /// 中 IPC_SET / IPC_RMID 的前置校验）：必须是 owner、creator 或 root。
    pub(crate) fn owner_check(&self) -> bool {
        let (euid, _) = current_euid_egid_or_root();
        euid == 0 || euid == self.uid || euid == self.cuid
    }
}

/// 取当前进程 (euid, egid)；PROCESS_TABLE 未初始化时退 (0, 0) = root。
pub(crate) fn current_euid_egid_or_root() -> (u32, u32) {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(t) = table.as_ref() {
        if let Some(pid) = t.current {
            if let Some(p) = t.get(pid) {
                return (p.euid, p.egid);
            }
        }
    }
    (0, 0)
}

pub(crate) fn now_sec() -> u64 {
    // IPC 时间戳取 CLOCK_REALTIME 整秒（Linux ktime_get_real_seconds），
    // 与 clock_gettime 同源，避免 wallclock 偏移下 ±1 秒漂移。
    crate::syscall::time::realtime_sec().max(0) as u64
}

/// Return the PID of the current process (used by SysV IPC, POSIX MQ, and shm).
pub(crate) fn current_pid_locked() -> usize {
    crate::sched::process::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed)
}

// Re-export msg/sem/shm public API for backward compatibility
pub use super::sysv_msg::{
    SysvMessage, MsgQueue, MSG_TABLE, MSGMNI,
    sys_msgget, sys_msgctl, sys_msgsnd, sys_msgrcv,
};
pub use super::sysv_sem::{
    SemVal, SemSet, SEM_TABLE,
    sys_semget, sys_semctl, sys_semop, sys_semtimedop,
    sem_undo_for_pid,
};
pub use super::sysv_shm::{
    ShmSegment, SHM_TABLE, SHMMNI, SHMMAX, SHMALL,
    shm_inc_nattch_bulk, shm_dec_nattch_bulk,
};
pub(crate) use super::sysv_shm::{
    sys_shmget, sys_shmat, sys_shmdt, sys_shmctl,
};
