use alloc::sync::Arc;

use crate::sched::process::{ExitReason, Pid, ProcessControlBlock};
use crate::sync::SpinLock;

static PROCESS_ACCT_INODE: SpinLock<Option<Arc<dyn crate::fs::Inode>>> = SpinLock::new(None);

#[derive(Clone, Copy)]
pub(crate) struct ProcessAcctSnapshot {
    pid: Pid,
    ppid: Pid,
    uid: u32,
    gid: u32,
    start_ticks: u64,
    user_ticks: u64,
    kernel_ticks: u64,
    elapsed_ticks: u64,
    exit_status: u32,
    signaled: bool,
    core_dumped: bool,
    comm: [u8; 16],
}

pub fn set_process_accounting_file(inode: Option<Arc<dyn crate::fs::Inode>>) {
    *PROCESS_ACCT_INODE.lock() = inode;
}

fn process_accounting_file() -> Option<Arc<dyn crate::fs::Inode>> {
    PROCESS_ACCT_INODE.lock().clone()
}

fn acct_comp_t(value: u64) -> u16 {
    let mut mantissa = value;
    let mut exponent = 0u16;
    while mantissa > 0x1fff && exponent < 7 {
        mantissa = (mantissa + 4) >> 3;
        exponent += 1;
    }
    if mantissa > 0x1fff {
        mantissa = 0x1fff;
    }
    ((exponent & 0x7) << 13) | (mantissa as u16 & 0x1fff)
}

fn acct_ticks_to_user_hz(ticks: u64) -> u64 {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    ((ticks as u128 * 100u128) / freq as u128) as u64
}

fn acct_ticks_to_seconds(ticks: u64) -> u32 {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    (ticks / freq).min(u32::MAX as u64) as u32
}

pub(crate) fn acct_snapshot(
    process: &ProcessControlBlock,
    reason: ExitReason,
    now: u64,
) -> ProcessAcctSnapshot {
    let mut user_ticks = process.cpu_user_ticks;
    let mut kernel_ticks = process.cpu_kernel_ticks;
    if process.cpu_last_switched_in_tick != 0 && now > process.cpu_last_switched_in_tick {
        let delta = now - process.cpu_last_switched_in_tick;
        if process.cpu_in_kernel {
            kernel_ticks = kernel_ticks.saturating_add(delta);
        } else {
            user_ticks = user_ticks.saturating_add(delta);
        }
    }
    let mut comm = [0u8; 16];
    for (idx, byte) in process.comm.as_bytes().iter().take(16).enumerate() {
        comm[idx] = *byte;
    }
    let (exit_status, signaled, core_dumped) = match reason {
        ExitReason::Normal(code) => (((code & 0xff) as u32) << 8, false, false),
        ExitReason::Signaled { sig, core_dumped } => ((sig as u32) & 0x7f, true, core_dumped),
    };
    ProcessAcctSnapshot {
        pid: process.pid,
        ppid: process.parent_pid.unwrap_or(0),
        uid: process.uid,
        gid: process.gid,
        start_ticks: process.start_time_ticks,
        user_ticks,
        kernel_ticks,
        elapsed_ticks: now.saturating_sub(process.start_time_ticks),
        exit_status,
        signaled,
        core_dumped,
        comm,
    }
}

fn acct_record(snapshot: ProcessAcctSnapshot) -> [u8; 64] {
    let mut out = [0u8; 64];
    let mut flag = 0u8;
    if snapshot.signaled {
        flag |= 0x10;
    }
    if snapshot.core_dumped {
        flag |= 0x08;
    }
    out[0] = flag;
    out[2..4].copy_from_slice(&(snapshot.uid.min(u16::MAX as u32) as u16).to_le_bytes());
    out[4..6].copy_from_slice(&(snapshot.gid.min(u16::MAX as u32) as u16).to_le_bytes());
    out[6..8].copy_from_slice(&0u16.to_le_bytes());
    out[8..12].copy_from_slice(&acct_ticks_to_seconds(snapshot.start_ticks).to_le_bytes());
    out[12..14].copy_from_slice(&acct_comp_t(acct_ticks_to_user_hz(snapshot.user_ticks)).to_le_bytes());
    out[14..16].copy_from_slice(&acct_comp_t(acct_ticks_to_user_hz(snapshot.kernel_ticks)).to_le_bytes());
    out[16..18].copy_from_slice(&acct_comp_t(acct_ticks_to_user_hz(snapshot.elapsed_ticks)).to_le_bytes());
    out[18..20].copy_from_slice(&0u16.to_le_bytes());
    out[20..22].copy_from_slice(&0u16.to_le_bytes());
    out[22..24].copy_from_slice(&0u16.to_le_bytes());
    out[24..26].copy_from_slice(&0u16.to_le_bytes());
    out[26..28].copy_from_slice(&0u16.to_le_bytes());
    out[28..30].copy_from_slice(&0u16.to_le_bytes());
    out[32..36].copy_from_slice(&snapshot.exit_status.to_le_bytes());
    out[36..52].copy_from_slice(&snapshot.comm);
    let _ = (snapshot.pid, snapshot.ppid);
    out
}

pub(crate) fn write_process_accounting(snapshot: Option<ProcessAcctSnapshot>) {
    let Some(snapshot) = snapshot else { return };
    let Some(inode) = process_accounting_file() else { return };
    let Some(file) = inode.as_file() else { return };
    let Ok(stat) = file.stat() else { return };
    let record = acct_record(snapshot);
    if file.write(stat.size, &record).ok() == Some(record.len()) {
        let _ = file.sync();
    }
}
