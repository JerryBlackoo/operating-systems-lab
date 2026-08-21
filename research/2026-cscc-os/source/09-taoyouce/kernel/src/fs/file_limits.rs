//! File-size resource limit helpers for filesystem write paths.

/// Returns `Some(-EFBIG)` if growing a file to `length` would exceed the
/// current process's `RLIMIT_FSIZE` soft limit.
pub(crate) fn rlimit_fsize_exceeded(length: usize) -> Option<isize> {
    let soft = rlimit_fsize_soft()?;
    if soft != u64::MAX && (length as u64) > soft {
        return Some(-27);
    }
    None
}

/// Apply Linux `generic_write_check_limits` to a regular-file write at byte
/// position `pos` for `len` bytes.
pub(crate) fn rlimit_fsize_clamp(pos: usize, len: usize) -> Result<usize, isize> {
    match rlimit_fsize_soft() {
        Some(soft) if soft != u64::MAX => {
            if pos as u64 >= soft {
                Err(-27)
            } else {
                let room = (soft - pos as u64) as usize;
                Ok(core::cmp::min(len, room))
            }
        }
        _ => Ok(len),
    }
}

fn rlimit_fsize_soft() -> Option<u64> {
    const RLIMIT_FSIZE: usize = 1;
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table.as_ref().and_then(|t| {
        t.current
            .and_then(|pid| t.get(pid))
            .map(|p| p.rlimits[RLIMIT_FSIZE].0)
    })
}
