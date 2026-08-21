//! Linux-AIO subsystem — io_setup / io_submit / io_cancel / io_getevents / io_destroy
//!
//! Linux-AIO (not POSIX AIO) uses io_setup to create an AIO context (returned as
//! an opaque u64 id), io_submit to enqueue iocbs into that context, and
//! io_getevents to dequeue completion events.
//!
//! This implementation is *synchronous*: io_submit performs each read/write/fsync
//! inline and pushes the result into the context's completion queue immediately.
//! io_getevents drains from that queue. This matches Linux behaviour for regular
//! files (which never actually block in the kernel AIO path) and is sufficient to
//! pass all LTP aio tests.

use crate::sync::SpinLock;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;

// ── internal types ──────────────────────────────────────────────────────────

#[derive(Clone)]
struct AioEvent {
    data: u64,
    obj: u64,
    res: i64,
    res2: i64,
}

struct AioCtx {
    #[allow(dead_code)]
    nr_events: u32,
    completed: VecDeque<AioEvent>,
}

// ── global state ────────────────────────────────────────────────────────────

static AIO_TABLE: SpinLock<BTreeMap<u64, AioCtx>> = SpinLock::new(BTreeMap::new());
static AIO_NEXT_CTX: SpinLock<u64> = SpinLock::new(1);

// ── syscall implementations ─────────────────────────────────────────────────

/// io_setup(nr_events, ctx_idp)
///
/// Creates a new AIO context capable of receiving at least `nr_events` events.
/// The context id is written to `*ctx_idp`.
///
/// Linux semantics:
/// - nr_events must be > 0 and <= 0x7FFFFFFF
/// - *ctx_idp must be 0 (not already initialized)
/// - EFAULT if ctx_idp is NULL or not writable
pub(in crate::syscall) fn sys_io_setup(nr_events: u32, ctx_idp: *mut u64) -> isize {
    if ctx_idp.is_null() {
        return -14;
    } // EFAULT
    if nr_events == 0 || nr_events > 0x7FFFFFFF {
        return -22;
    } // EINVAL

    // Linux reserves 2*nr_events ring slots per context and rejects the request
    // with EAGAIN when the system-wide reservation would exceed aio-max-nr
    // (fs/aio.c ioctx_alloc). aio-max-nr is reported as 65536 by procfs.
    const AIO_MAX_NR: u64 = 65536;
    let want = (nr_events.max(1) as u64).saturating_mul(2);
    let used: u64 = {
        let tbl = AIO_TABLE.lock();
        tbl.values().map(|c| (c.nr_events.max(1) as u64).saturating_mul(2)).sum()
    };
    if used + want > AIO_MAX_NR {
        return -11; // EAGAIN
    }

    // Linux: *ctx_idp must be 0 (not already initialized).
    let existing = match super::user::read_user_buffer(ctx_idp as *const u8, 8) {
        Ok(b) => u64::from_le_bytes(b[0..8].try_into().unwrap()),
        Err(_) => return -14,
    };
    if existing != 0 {
        return -22;
    } // EINVAL: context already initialized

    let mut next = AIO_NEXT_CTX.lock();
    let id = *next;
    *next += 1;
    drop(next);

    let mut tbl = AIO_TABLE.lock();
    tbl.insert(
        id,
        AioCtx {
            nr_events,
            completed: VecDeque::new(),
        },
    );
    drop(tbl);

    if super::user::write_user_buffer(ctx_idp as *mut u8, &id.to_le_bytes()).is_err() {
        return -14;
    }
    0
}

/// io_destroy(ctx_id) — destroy an AIO context.
pub(in crate::syscall) fn sys_io_destroy(ctx_id: u64) -> isize {
    let mut tbl = AIO_TABLE.lock();
    if tbl.remove(&ctx_id).is_some() {
        0
    } else {
        -22
    }
}

/// io_submit(ctx_id, nr, iocbpp) — submit iocbs for processing.
///
/// Reads `nr` pointers from `iocbpp`, each pointing to a `struct iocb`.
/// For each iocb, performs the requested operation synchronously and pushes
/// a completion event into the context.
///
/// struct iocb layout (64 bytes, Linux ABI — `include/uapi/linux/aio_abi.h`):
///   u64 aio_data       [0..8]
///   u32 aio_key        [8..12]
///   i32 aio_rw_flags   [12..16]
///   u16 aio_lio_opcode [16..18]
///   i16 aio_reqprio    [18..20]
///   u32 aio_fildes     [20..24]
///   u64 aio_buf        [24..32]
///   u64 aio_nbytes     [32..40]
///   i64 aio_offset     [40..48]
///   u64 aio_reserved2  [48..56]
///   u32 aio_flags      [56..60]
///   u32 aio_resfd      [60..64]
pub(in crate::syscall) fn sys_io_submit(ctx_id: u64, nr: i64, iocbpp: *const usize) -> isize {
    // Linux fs/aio.c do_io_submit: nr < 0 is rejected with EINVAL; nr == 0 is
    // a no-op returning 0.
    if nr < 0 {
        return -22;
    }
    if nr == 0 {
        return 0;
    }

    let ptr_bytes = match super::user::read_user_buffer(iocbpp as *const u8, (nr as usize) * 8) {
        Ok(b) => b,
        Err(_) => return -14,
    };

    let mut tbl = AIO_TABLE.lock();
    let ctx = match tbl.get_mut(&ctx_id) {
        Some(c) => c,
        None => return -22,
    };

    let mut completed = 0i64;
    for i in 0..nr as usize {
        // Linux io_submit returns the negated errno of the *first* iocb that
        // fails preparation, but only when no iocb has been queued yet; once at
        // least one has been accepted it returns the accepted count and leaves
        // the rest unprocessed (io_submit03).
        macro_rules! fail {
            ($errno:expr) => {{
                if completed == 0 {
                    return $errno;
                } else {
                    break;
                }
            }};
        }

        let iocb_addr = usize::from_le_bytes(ptr_bytes[i * 8..i * 8 + 8].try_into().unwrap());
        // NULL or otherwise unreadable iocb pointer → EFAULT.
        if iocb_addr == 0 {
            fail!(-14);
        }
        let iocb_bytes = match super::user::read_user_buffer(iocb_addr as *const u8, 64) {
            Ok(b) => b,
            Err(_) => fail!(-14),
        };

        let data = u64::from_le_bytes(iocb_bytes[0..8].try_into().unwrap());
        let opcode = u16::from_le_bytes(iocb_bytes[16..18].try_into().unwrap());
        let fildes = u32::from_le_bytes(iocb_bytes[20..24].try_into().unwrap()) as i32;
        let buf = u64::from_le_bytes(iocb_bytes[24..32].try_into().unwrap()) as usize;
        let nbytes = u64::from_le_bytes(iocb_bytes[32..40].try_into().unwrap()) as usize;
        let offset = i64::from_le_bytes(iocb_bytes[40..48].try_into().unwrap());

        const IOCB_PREAD: u16 = 0;
        const IOCB_PWRITE: u16 = 1;
        const IOCB_FSYNC: u16 = 2;
        const IOCB_FDSYNC: u16 = 3;

        // Validate the descriptor and its access mode before performing the op
        // (Linux aio_prep_rw: bad fd → EBADF; read on a write-only fd or write
        // on a read-only fd → EBADF). io_submit03.
        if fildes < 0 {
            fail!(-9);
        }
        match crate::syscall::fd::get_fd_obj(fildes as usize) {
            None => fail!(-9),
            Some(fdobj) => match opcode {
                IOCB_PREAD if !fdobj.readable() => fail!(-9),
                IOCB_PWRITE if !fdobj.writable() => fail!(-9),
                _ => {}
            },
        }

        let res = match opcode {
            IOCB_PREAD => crate::fs::positional_io::sys_pread64(
                fildes as usize,
                buf as *mut u8,
                nbytes,
                offset as usize,
            ),
            IOCB_PWRITE => crate::fs::positional_io::sys_pwrite64(
                fildes as usize,
                buf as *const u8,
                nbytes,
                offset as usize,
            ),
            IOCB_FSYNC | IOCB_FDSYNC => super::sync::sys_fsync(fildes as usize),
            _ => -22,
        };

        ctx.completed.push_back(AioEvent {
            data,
            obj: iocb_addr as u64,
            res: res as i64,
            res2: 0,
        });
        completed += 1;
    }
    completed as isize
}

/// io_cancel(ctx_id, iocb, result) — cancel an outstanding AIO request.
///
/// Our AIO is synchronous (io_submit completes immediately), so there is never
/// an in-flight iocb to cancel.
/// - ctx_id not found → EINVAL
/// - iocb NULL → EFAULT
/// - otherwise → EAGAIN (no cancellable request found)
pub(in crate::syscall) fn sys_io_cancel(ctx_id: u64, iocb: *const u8, _result: *mut u8) -> isize {
    if iocb.is_null() {
        return -14;
    } // EFAULT
    let tbl = AIO_TABLE.lock();
    if tbl.get(&ctx_id).is_none() {
        return -22;
    }
    drop(tbl);
    -11 // EAGAIN
}

/// io_getevents(ctx_id, min_nr, nr, events, timeout) — read AIO events.
///
/// Drains up to `nr` completion events from the context into `events`.
/// If fewer than `min_nr` events are available:
/// - timeout=NULL → block indefinitely (in practice returns ETIMEDOUT since
///   our synchronous model has no wakeup source)
/// - timeout given → sleep up to that duration, then return whatever is available
///
/// struct io_event layout (32 bytes each):
///   u64 data, u64 obj, i64 res, i64 res2
pub(in crate::syscall) fn sys_io_getevents(
    ctx_id: u64,
    min_nr: i64,
    nr: i64,
    events: *mut u8,
    timeout: *const u8,
) -> isize {
    if AIO_TABLE.lock().get(&ctx_id).is_none() {
        return -22;
    } // EINVAL
    if nr <= 0 {
        return 0;
    }

    // Parse timeout (struct timespec: i64 tv_sec + i64 tv_nsec = 16 bytes)
    let timeout_ns: Option<u64> = if timeout.is_null() {
        None
    } else {
        match super::user::read_user_buffer(timeout, 16) {
            Ok(b) => {
                let sec = i64::from_le_bytes(b[0..8].try_into().unwrap()).max(0) as u64;
                let nsec = i64::from_le_bytes(b[8..16].try_into().unwrap()).max(0) as u64;
                Some(sec.saturating_mul(1_000_000_000).saturating_add(nsec))
            }
            Err(_) => return -14,
        }
    };

    let drain_into = |buf: &mut Vec<u8>, max: i64| -> i64 {
        let mut tbl = AIO_TABLE.lock();
        let ctx = match tbl.get_mut(&ctx_id) {
            Some(c) => c,
            None => return 0,
        };
        let mut out = 0i64;
        while out < max {
            let ev = match ctx.completed.pop_front() {
                Some(e) => e,
                None => break,
            };
            buf.extend_from_slice(&ev.data.to_le_bytes());
            buf.extend_from_slice(&ev.obj.to_le_bytes());
            buf.extend_from_slice(&ev.res.to_le_bytes());
            buf.extend_from_slice(&ev.res2.to_le_bytes());
            out += 1;
        }
        out
    };

    let mut buf = Vec::new();
    let mut out = drain_into(&mut buf, nr);

    if out < min_nr {
        let freq = crate::arch::consts::CLOCK_FREQ as u64;
        let deadline = if let Some(ns) = timeout_ns {
            let ticks = (ns as u128 * freq as u128 / 1_000_000_000u128) as u64;
            crate::arch::asm_helpers::rdtime().saturating_add(ticks)
        } else {
            u64::MAX
        };

        if deadline != u64::MAX {
            crate::sched::trace::record_current_event(
                crate::sched::trace::EV_SLEEP_ENTER,
                deadline,
                deadline.saturating_sub(crate::arch::asm_helpers::rdtime()),
            );
            crate::sched::process::enter_wait_channel("io_getevents");
            crate::sched::process::suspend_current_for_nanosleep(deadline);
            out = drain_into(&mut buf, nr - out).saturating_add(out);
        }

        if out < min_nr && timeout_ns.is_none() {
            return -110; // ETIMEDOUT
        }
    }

    if !events.is_null() && !buf.is_empty() {
        if super::user::write_user_buffer(events, &buf).is_err() {
            return -14;
        }
    }
    out as isize
}
