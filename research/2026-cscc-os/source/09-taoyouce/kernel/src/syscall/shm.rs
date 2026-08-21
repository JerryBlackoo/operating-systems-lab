//! SysV shared memory syscall glue — delegates to `ipc/sysv_shm.rs`.
//!
//! Re-exports maintain backward `crate::syscall::shm::` paths for external callers.

pub use crate::ipc::sysv_shm::{
    ShmSegment, SHM_TABLE, SHMMNI, SHMMAX, SHMALL,
    shm_inc_nattch_bulk, shm_dec_nattch_bulk,
    shm_next_id, set_shm_next_id,
};

pub(crate) use crate::ipc::sysv_shm::{
    sys_shmget, sys_shmat, sys_shmdt, sys_shmctl,
};
