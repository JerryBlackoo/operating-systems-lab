use alloc::fmt::Write;
use alloc::string::String;

use crate::fs::procfs::{ProcError, ProcFileGenerator};

/// /proc/sysvipc/sem — Linux format for semctl09 / ipcs
pub struct SysvipcSemGenerator;

impl ProcFileGenerator for SysvipcSemGenerator {
    fn generate(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{:>10} {:>10} {:>6} {:>10} {:>5} {:>5} {:>5} {:>5} {:>10} {:>10}",
            "key", "semid", "perms", "nsems", "uid", "gid", "cuid", "cgid", "otime", "ctime");
        let tbl = crate::ipc::sysv::SEM_TABLE.lock();
        for (&id, set) in tbl.iter() {
            let _ = writeln!(s, "{:>10} {:>10} {:>6o} {:>10} {:>5} {:>5} {:>5} {:>5} {:>10} {:>10}",
                set.perm.key, id, set.perm.mode, set.sems.len(),
                set.perm.uid, set.perm.gid, set.perm.cuid, set.perm.cgid,
                set.last_otime, set.last_ctime);
        }
        s
    }
}

/// /proc/sysvipc/msg
pub struct SysvipcMsgGenerator;

impl ProcFileGenerator for SysvipcMsgGenerator {
    fn generate(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{:>10} {:>10} {:>6} {:>10} {:>5} {:>5} {:>5} {:>5} {:>5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
            "key", "msqid", "perms", "cbytes", "qnum", "lspid", "lrpid", "uid", "gid", "cuid", "cgid", "stime", "rtime", "ctime", "qbytes");
        let tbl = crate::ipc::sysv::MSG_TABLE.lock();
        for (&id, q) in tbl.iter() {
            let cbytes: usize = q.messages.iter().map(|m| m.data.len()).sum();
            let _ = writeln!(s, "{:>10} {:>10} {:>6o} {:>10} {:>5} {:>5} {:>5} {:>5} {:>5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
                q.perm.key, id, q.perm.mode, cbytes, q.messages.len(),
                q.last_spid, q.last_rpid, q.perm.uid, q.perm.gid, q.perm.cuid, q.perm.cgid,
                q.last_stime, q.last_rtime, q.last_ctime, q.max_bytes);
        }
        s
    }
}

/// /proc/sysvipc/shm — Linux format for shmget03 / ipcs
pub struct SysvipcShmGenerator;

impl ProcFileGenerator for SysvipcShmGenerator {
    fn generate(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "{:>10} {:>10} {:>6} {:>10} {:>5} {:>5} {:>5} {:>5} {:>5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
            "key", "shmid", "perms", "size", "cpid", "lpid", "nattch", "uid", "gid", "cuid", "cgid",
            "atime", "dtime", "ctime", "rss", "swap");
        let tbl = crate::syscall::shm::SHM_TABLE.lock();
        if let Some(ref map) = *tbl {
            for (&id, seg) in map.iter() {
                let rss = seg.pages.len() * 4096;
                let _ = writeln!(s, "{:>10} {:>10} {:>6o} {:>10} {:>5} {:>5} {:>5} {:>5} {:>5} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10} {:>10}",
                    seg.perm.key, id, seg.perm.mode, seg.size, seg.cpid, seg.lpid,
                    seg.nattch, seg.perm.uid, seg.perm.gid, seg.perm.cuid, seg.perm.cgid,
                    seg.last_atime, seg.last_dtime, seg.last_ctime, rss, 0);
            }
        }
        s
    }
}

/// /proc/sys/kernel/shmmax — max SysV shared memory segment size (bytes)
pub struct ShmmaxGenerator;

impl ProcFileGenerator for ShmmaxGenerator {
    fn generate(&self) -> String {
        use crate::syscall::shm::SHMMAX;
        alloc::format!("{}\n", SHMMAX.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        use crate::syscall::shm::SHMMAX;
        let val: usize = data.trim().parse().map_err(|_| ProcError::InvalidData)?;
        SHMMAX.store(val, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/kernel/shmmni — max number of SysV shared memory segments
pub struct ShmmniGenerator;

impl ProcFileGenerator for ShmmniGenerator {
    fn generate(&self) -> String {
        use crate::syscall::shm::SHMMNI;
        alloc::format!("{}\n", SHMMNI.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        use crate::syscall::shm::SHMMNI;
        let val: usize = data.trim().parse().map_err(|_| ProcError::InvalidData)?;
        SHMMNI.store(val, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/kernel/shm_next_id — CHECKPOINT_RESTORE: desired id for the next
/// allocated SysV shm segment (default -1 = generic allocator). Reset to -1 by
/// the kernel after a successful `shmget` (LTP shmget05/shmget06).
pub struct ShmNextIdGenerator;

impl ProcFileGenerator for ShmNextIdGenerator {
    fn generate(&self) -> String {
        alloc::format!("{}\n", crate::syscall::shm::shm_next_id())
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let val: i32 = data.trim().parse().map_err(|_| ProcError::InvalidData)?;
        if crate::syscall::shm::set_shm_next_id(val) {
            Ok(())
        } else {
            Err(ProcError::InvalidData)
        }
    }
}

/// /proc/sys/kernel/msg_next_id — CHECKPOINT_RESTORE: desired id for the next
/// allocated SysV message queue (default -1). Reset to -1 after a successful
/// `msgget` (LTP msgget04/msgget05).
pub struct MsgNextIdGenerator;

impl ProcFileGenerator for MsgNextIdGenerator {
    fn generate(&self) -> String {
        alloc::format!("{}\n", crate::ipc::sysv_msg::msg_next_id())
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let val: i32 = data.trim().parse().map_err(|_| ProcError::InvalidData)?;
        if crate::ipc::sysv_msg::set_msg_next_id(val) {
            Ok(())
        } else {
            Err(ProcError::InvalidData)
        }
    }
}

/// /proc/sys/kernel/sem_next_id — CHECKPOINT_RESTORE: desired id for the next
/// allocated SysV semaphore set (default -1). Reset to -1 after a successful
/// `semget` (LTP semget05/semget06).
pub struct SemNextIdGenerator;

impl ProcFileGenerator for SemNextIdGenerator {
    fn generate(&self) -> String {
        alloc::format!("{}\n", crate::ipc::sysv_sem::sem_next_id())
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        let val: i32 = data.trim().parse().map_err(|_| ProcError::InvalidData)?;
        if crate::ipc::sysv_sem::set_sem_next_id(val) {
            Ok(())
        } else {
            Err(ProcError::InvalidData)
        }
    }
}

/// /proc/sys/kernel/shmall — total SysV shared memory pages available
pub struct ShmallGenerator;

impl ProcFileGenerator for ShmallGenerator {
    fn generate(&self) -> String {
        use crate::syscall::shm::SHMALL;
        alloc::format!("{}\n", SHMALL.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        use crate::syscall::shm::SHMALL;
        let val: usize = data.trim().parse().map_err(|_| ProcError::InvalidData)?;
        SHMALL.store(val, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/kernel/sem — semaphore limits: SEMMSL SEMMNS SEMOPM SEMMNI
pub struct KernelSemGenerator;

impl ProcFileGenerator for KernelSemGenerator {
    fn generate(&self) -> String { String::from("250\t32000\t32\t128\n") }
}

/// /proc/sys/kernel/msgmax — max message size (bytes)
pub struct MsgmaxGenerator;

impl ProcFileGenerator for MsgmaxGenerator {
    fn generate(&self) -> String { String::from("8192\n") }
}

/// /proc/sys/kernel/msgmnb — max bytes in a single message queue
pub struct MsgmnbGenerator;

impl ProcFileGenerator for MsgmnbGenerator {
    fn generate(&self) -> String { String::from("16384\n") }
}

/// /proc/sys/kernel/msgmni — max number of message queues
pub struct MsgmniGenerator;

impl ProcFileGenerator for MsgmniGenerator {
    fn generate(&self) -> String {
        use crate::ipc::sysv::MSGMNI;
        alloc::format!("{}\n", MSGMNI.load(core::sync::atomic::Ordering::Relaxed))
    }
    fn writable(&self) -> bool { true }
    fn write(&self, data: &str) -> Result<(), ProcError> {
        use crate::ipc::sysv::MSGMNI;
        let val: usize = data.trim().parse().map_err(|_| ProcError::InvalidData)?;
        MSGMNI.store(val, core::sync::atomic::Ordering::Relaxed);
        Ok(())
    }
}

/// /proc/sys/kernel/keys/maxbytes — per-user max total bytes of key data
pub struct KeysMaxbytesGenerator;

impl ProcFileGenerator for KeysMaxbytesGenerator {
    fn generate(&self) -> String { String::from("20000\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/kernel/keys/maxkeys — per-user max number of keys
pub struct KeysMaxkeysGenerator;

impl ProcFileGenerator for KeysMaxkeysGenerator {
    fn generate(&self) -> String { String::from("200\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/kernel/keys/root_maxbytes — root user max total bytes
pub struct KeysRootMaxbytesGenerator;

impl ProcFileGenerator for KeysRootMaxbytesGenerator {
    fn generate(&self) -> String { String::from("25000000\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/kernel/keys/root_maxkeys — root user max keys
pub struct KeysRootMaxkeysGenerator;

impl ProcFileGenerator for KeysRootMaxkeysGenerator {
    fn generate(&self) -> String { String::from("1000000\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/kernel/keys/gc_delay — garbage collection delay (seconds)
pub struct KeysGcDelayGenerator;

impl ProcFileGenerator for KeysGcDelayGenerator {
    fn generate(&self) -> String { String::from("300\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}

/// /proc/sys/kernel/keys/persistent_keyring_expiry — persistent keyring timeout
pub struct KeysPersistentExpiryGenerator;

impl ProcFileGenerator for KeysPersistentExpiryGenerator {
    fn generate(&self) -> String { String::from("259200\n") }
    fn writable(&self) -> bool { true }
    fn write(&self, _data: &str) -> Result<(), ProcError> { Ok(()) }
}
