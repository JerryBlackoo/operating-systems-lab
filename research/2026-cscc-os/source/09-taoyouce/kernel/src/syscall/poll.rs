use alloc::sync::Arc;
use alloc::vec::Vec;

use super::user::{read_user_buffer, read_user_to_slice, write_user_buffer};
use crate::fs::{
    fd as fd_table, FileDescriptor, READINESS_ERROR, READINESS_HUP, READINESS_READ,
    READINESS_WRITE,
};
use crate::sched::process::POLL_WAIT_SENTINEL;

const POLLIN: u32 = 0x0001;
const POLLOUT: u32 = 0x0004;
const POLLERR: u32 = 0x0008;
const POLLHUP: u32 = 0x0010;
const POLLNVAL: u32 = 0x0020;
const MAX_POLL_FDS: usize = 1024;



#[allow(dead_code)]
pub(in crate::syscall) fn sys_pselect6(nfds: i32, readfds: *mut u8, writefds: *mut u8, _exceptfds: *mut u8, timeout_ptr: usize, sigmask_arg: usize) -> isize {
    if nfds < 0 || nfds as usize > MAX_POLL_FDS {
        return -22;
    }
    // pselect6's sigmask argument is a pointer to {sigset_t*, size_t}
    let has_sigmask = sigmask_arg != 0;
    if has_sigmask {
        let arg_buf = match read_user_buffer(sigmask_arg as *const u8, 16) {
            Ok(buf) => buf,
            Err(_) => return -14,
        };
        let sig_ptr = usize::from_le_bytes(arg_buf[0..8].try_into().unwrap());
        let sig_size = usize::from_le_bytes(arg_buf[8..16].try_into().unwrap());
        if sig_ptr != 0 {
            if sig_size != 8 {
                return -22;
            }
            let mask_buf = match read_user_buffer(sig_ptr as *const u8, 8) {
                Ok(buf) => buf,
                Err(_) => return -14,
            };
            let new_mask = u64::from_le_bytes(mask_buf[0..8].try_into().unwrap());
            let mut guard = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(table) = guard.as_mut() {
                if let Some(pid) = table.current {
                    if let Some(p) = table.get_mut(pid) {
                        if p.sigsuspend_restore_mask.is_none() {
                            p.sigsuspend_restore_mask = Some(p.signal_mask);
                        }
                        p.signal_mask = new_mask;
                    }
                }
            }
        }
    }

    let result = sys_pselect6_inner(nfds as usize, readfds, writefds, _exceptfds, timeout_ptr);

    // Restore original signal mask
    if has_sigmask {
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            if let Some(pid) = table.current {
                if let Some(p) = table.get_mut(pid) {
                    if let Some(restore) = p.sigsuspend_restore_mask.take() {
                        p.signal_mask = restore;
                    }
                }
            }
        }
    }
    result
}

fn sys_pselect6_inner(nfds: usize, readfds: *mut u8, writefds: *mut u8, _exceptfds: *mut u8, timeout_ptr: usize) -> isize {
    // Slice 2.5 wchan：pselect6 → Linux `do_pselect`/`core_sys_select`
    let wait_label = "do_pselect";
    let set_bytes = (nfds + 7) / 8;
    if take_poll_timeout_reentry(timeout_ptr != 0) {
        return write_select_results(
            readfds,
            writefds,
            _exceptfds,
            set_bytes,
            !readfds.is_null() && set_bytes > 0,
            !writefds.is_null() && set_bytes > 0,
            &alloc::vec![0u8; set_bytes],
            &alloc::vec![0u8; set_bytes],
            0,
        );
    }
    // fd_set is at most FD_SETSIZE (1024) bits = 128 bytes: stack buffers
    // avoid a heap allocation per set; larger nfds fall back to the heap.
    let mut rs_store = [0u8; 128];
    let mut ws_store = [0u8; 128];
    let mut es_store = [0u8; 128];
    let mut rs_heap: Vec<u8> = Vec::new();
    let mut ws_heap: Vec<u8> = Vec::new();
    let mut es_heap: Vec<u8> = Vec::new();
    let read_set: Option<&[u8]> = match read_fd_set(readfds, set_bytes, &mut rs_store, &mut rs_heap) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let write_set: Option<&[u8]> = match read_fd_set(writefds, set_bytes, &mut ws_store, &mut ws_heap) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let except_set: Option<&[u8]> = match read_fd_set(_exceptfds, set_bytes, &mut es_store, &mut es_heap) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let deadline = match timespec_deadline(timeout_ptr) {
        Ok(deadline) => deadline,
        Err(errno) => return errno,
    };

    if signal_pending_for_current() {
        return -4;
    }
    if nfds == 0 {
        return wait_for_deadline_only(deadline, wait_label);
    }

    // fd_set is at most FD_SETSIZE (1024) bits = 128 bytes: stack buffers
    // avoid two heap allocations per wakeup iteration.
    let mut read_buf = [0u8; 128];
    let mut write_buf = [0u8; 128];
    loop {
        let read_result = &mut read_buf[..set_bytes];
        let write_result = &mut write_buf[..set_bytes];
        read_result.fill(0);
        write_result.fill(0);
        let mut ready_count = 0isize;
        let mut wait_items: Vec<(Arc<FileDescriptor>, u32)> = Vec::new();

        // Single PROCESS_TABLE lock: walk the requested bits, resolve each fd
        // by borrow (no per-fd Arc clone) and evaluate readiness in place.
        // readiness()/poll_read()/poll_write() implementations only take their
        // own object locks, never PROCESS_TABLE, so this cannot self-deadlock.
        {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let Some(table) = table.as_ref() else { return -9 };
            let Some(pid) = table.current else { return -9 };
            let Some(process) = table.get_fd_process(pid) else { return -9 };
            // Byte-wise walk: skip whole bytes where no set requests any fd,
            // and only touch the bits actually set (trailing_zeros scan).
            let last_byte = (nfds + 7) / 8;
            for byte_idx in 0..last_byte {
                let rb = read_set.map_or(0u8, |set| set[byte_idx]);
                let wb = write_set.map_or(0u8, |set| set[byte_idx]);
                let eb = except_set.map_or(0u8, |set| set[byte_idx]);
                let mut bits = rb | wb | eb;
                if byte_idx == last_byte - 1 && nfds % 8 != 0 {
                    bits &= (1u8 << (nfds % 8)) - 1;
                }
                while bits != 0 {
                    let bit = bits.trailing_zeros() as usize;
                    let bit_mask = 1u8 << bit;
                    bits &= bits - 1;
                    let fd = byte_idx * 8 + bit;
                    let want_read = rb & bit_mask != 0;
                    let want_write = wb & bit_mask != 0;
                    let Some(fd_obj) = process.fd_table.get(fd).and_then(|e| e.as_ref()) else {
                        return -9; // EBADF
                    };
                    let mut requested = 0u32;
                    if want_read { requested |= POLLIN; }
                    if want_write { requested |= POLLOUT; }
                    let revents = revents_for(fd_obj, requested);
                    if want_read && revents & (POLLIN | POLLERR | POLLHUP) != 0 {
                        read_result[byte_idx] |= bit_mask;
                        ready_count += 1;
                    }
                    if want_write && revents & (POLLOUT | POLLERR | POLLHUP) != 0 {
                        write_result[byte_idx] |= bit_mask;
                        ready_count += 1;
                    }
                    if revents == 0 {
                        wait_items.push((fd_obj.clone(), requested));
                    }
                }
            }
        }

        if ready_count > 0 || deadline_expired(deadline) {
            return write_select_results(
                readfds,
                writefds,
                _exceptfds,
                set_bytes,
                read_set.is_some(),
                write_set.is_some(),
                &read_result,
                &write_result,
                ready_count,
            );
        }

        if signal_pending_for_current() {
            return -4;
        }
        register_wait_items(&wait_items, deadline);
        let mut ready_after_register = false;
        for (fd_obj, requested) in &wait_items {
            if revents_for(fd_obj, *requested) != 0 {
                ready_after_register = true;
                break;
            }
        }
        if ready_after_register {
            continue;
        }
        if super::has_other_runnable() {
            drop(wait_items);
            block_current_for_poll(deadline, wait_label);
        }
        super::inline_wfi_tick();
    }
}

#[allow(dead_code)]
pub(in crate::syscall) fn sys_ppoll(fds_ptr: *mut u8, nfds: usize, timeout_ptr: usize, sigmask_ptr: usize, sigsetsize: usize) -> isize {
    // Slice 2.5 wchan：ppoll → Linux `do_sys_ppoll`
    let wait_label = "do_sys_ppoll";

    // Atomically swap signal mask if sigmask_ptr is non-null (ppoll semantics).
    // Use sigsuspend_restore_mask to persist the original mask across ecall re-entry
    // (block_current_for_poll saves io_wait_trap_context and re-executes ecall).
    let has_sigmask = sigmask_ptr != 0;
    if has_sigmask {
        if sigsetsize != 8 {
            return -22; // EINVAL
        }
        let mask_buf = match read_user_buffer(sigmask_ptr as *const u8, 8) {
            Ok(buf) => buf,
            Err(_) => return -14,
        };
        let new_mask = u64::from_le_bytes(mask_buf[0..8].try_into().unwrap());
        // Only save original mask on first entry (not re-entry after io_wait wakeup)
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            if let Some(pid) = table.current {
                if let Some(p) = table.get_mut(pid) {
                    if p.sigsuspend_restore_mask.is_none() {
                        p.sigsuspend_restore_mask = Some(p.signal_mask);
                    }
                    p.signal_mask = new_mask;
                }
            }
        }
        drop(guard);
    }

    let result = sys_ppoll_inner(fds_ptr, nfds, timeout_ptr, wait_label);

    // Restore original signal mask on normal return
    if has_sigmask {
        let mut guard = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(table) = guard.as_mut() {
            if let Some(pid) = table.current {
                if let Some(p) = table.get_mut(pid) {
                    if let Some(restore) = p.sigsuspend_restore_mask.take() {
                        p.signal_mask = restore;
                    }
                }
            }
        }
    }
    result
}

fn sys_ppoll_inner(fds_ptr: *mut u8, nfds: usize, timeout_ptr: usize, wait_label: &'static str) -> isize {
    if nfds == 0 {
        if signal_pending_for_current() {
            return -4;
        }
        return wait_for_deadline_only(match timespec_deadline(timeout_ptr) {
            Ok(deadline) => deadline,
            Err(errno) => return errno,
        }, wait_label);
    }
    if fds_ptr.is_null() {
        return -14;
    }
    if nfds > MAX_POLL_FDS {
        return -22;
    }
    let deadline = match timespec_deadline(timeout_ptr) {
        Ok(deadline) => deadline,
        Err(errno) => return errno,
    };
    if signal_pending_for_current() {
        return -4;
    }
    let total_size = nfds.saturating_mul(8);
    let fds_buf = match read_user_buffer(fds_ptr as *const u8, total_size) {
        Ok(buf) => buf,
        Err(_) => return -14,
    };
    if crate::syscall::scdbg_on() && nfds <= 4 {
        for i in 0..nfds {
            let off = i * 8;
            let fd = i32::from_le_bytes(fds_buf[off..off + 4].try_into().unwrap());
            let ev = i16::from_le_bytes(fds_buf[off + 4..off + 6].try_into().unwrap());
            crate::println!("[sc]   ppoll pfd[{}] fd={} events={:#x}", i, fd, ev);
        }
    }
    if take_poll_timeout_reentry(deadline.is_some()) {
        if signal_pending_for_current() {
            return -4;
        }
        let mut result_buf = fds_buf.clone();
        for i in 0..nfds {
            let off = i * 8;
            result_buf[off + 6] = 0;
            result_buf[off + 7] = 0;
        }
        return if write_user_buffer(fds_ptr, &result_buf).is_ok() {
            0
        } else {
            -14
        };
    }

    loop {
        let mut result_buf = fds_buf.clone();
        let mut ready_count = 0isize;
        let mut wait_items: Vec<(Arc<FileDescriptor>, u32)> = Vec::new();

        // Batch FD lookup: collect valid fds, single PROCESS_TABLE lock.
        let mut poll_fds: Vec<(usize, usize, u32)> = Vec::new(); // (poll_idx, fd, events)
        for i in 0..nfds {
            let off = i * 8;
            // Linux zeroes revents for every entry on return, including
            // negative fds (ignored entries); stale user revents must not
            // survive the syscall.
            result_buf[off + 6] = 0;
            result_buf[off + 7] = 0;
            let fd = i32::from_le_bytes(result_buf[off..off + 4].try_into().unwrap());
            let events = i16::from_le_bytes(result_buf[off + 4..off + 6].try_into().unwrap()) as u32;
            if fd >= 0 {
                poll_fds.push((i, fd as usize, events));
            }
        }

        // Single lock for all FD lookups.
        let fd_lookup: Vec<Option<Arc<FileDescriptor>>> = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(table) = table.as_ref() {
                if let Some(pid) = table.current {
                    if let Some(process) = table.get_fd_process(pid) {
                        poll_fds.iter().map(|&(_, fd, _)| fd_table::get(process, fd)).collect()
                    } else {
                        alloc::vec![None; poll_fds.len()]
                    }
                } else {
                    alloc::vec![None; poll_fds.len()]
                }
            } else {
                alloc::vec![None; poll_fds.len()]
            }
        };

        for (j, &(idx, _fd, events)) in poll_fds.iter().enumerate() {
            let off = idx * 8;
            let mut revents = 0u32;
            if let Some(ref fd_obj) = fd_lookup[j] {
                revents = revents_for(fd_obj, events);
                if revents == 0 && events & (POLLIN | POLLOUT) != 0 {
                    wait_items.push((fd_obj.clone(), events));
                }
            } else {
                revents = POLLNVAL;
            }
            if revents != 0 {
                ready_count += 1;
            }
            let rev_bytes = (revents as i16).to_le_bytes();
            result_buf[off + 6] = rev_bytes[0];
            result_buf[off + 7] = rev_bytes[1];
        }

        if ready_count > 0 || deadline_expired(deadline) {
            if deadline_expired(deadline) && signal_pending_for_current() {
                clear_poll_deadline();
                return -4;
            }
            clear_poll_deadline();
            return if write_user_buffer(fds_ptr, &result_buf).is_ok() {
                ready_count
            } else {
                -14
            };
        }

        if signal_pending_for_current() {
            clear_poll_deadline();
            return -4;
        }
        register_wait_items(&wait_items, deadline);
        let mut ready_after_register = false;
        for (fd_obj, events) in &wait_items {
            if revents_for(fd_obj, *events) != 0 {
                ready_after_register = true;
                break;
            }
        }
        if ready_after_register {
            continue;
        }
        if super::has_other_runnable() {
            drop(wait_items);
            drop(result_buf);
            drop(fds_buf);
            block_current_for_poll(deadline, wait_label);
        }
        super::inline_wfi_tick();
    }
}

fn current_fd(fd: usize) -> Option<Arc<FileDescriptor>> {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table.as_ref().and_then(|table| {
        table.current.and_then(|pid| {
            table.get_fd_process(pid).and_then(|process| fd_table::get(process, fd))
        })
    })
}

/// Batch-lookup multiple FDs with a single PROCESS_TABLE lock acquisition.
/// Returns a Vec of (fd_index, Arc<FileDescriptor>) for FDs that exist.
/// Eliminates N separate lock acquisitions for select/poll on N fds.
fn read_fd_set<'a>(
    ptr: *mut u8,
    set_bytes: usize,
    stack: &'a mut [u8; 128],
    heap: &'a mut Vec<u8>,
) -> Result<Option<&'a [u8]>, isize> {
    if ptr.is_null() || set_bytes == 0 {
        return Ok(None);
    }
    if set_bytes <= stack.len() {
        let out = &mut stack[..set_bytes];
        match read_user_to_slice(ptr as *const u8, out) {
            Ok(_) => Ok(Some(out)),
            Err(_) => Err(-14),
        }
    } else {
        match read_user_buffer(ptr as *const u8, set_bytes) {
            Ok(buf) => {
                *heap = buf;
                Ok(Some(heap.as_slice()))
            }
            Err(_) => Err(-14),
        }
    }
}

#[allow(dead_code)]
fn batch_current_fds(fds: &[usize]) -> Result<Vec<(usize, Arc<FileDescriptor>)>, isize> {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let table = table.as_ref().ok_or(-9isize)?;
    let pid = table.current.ok_or(-9isize)?;
    let process = table.get_fd_process(pid).ok_or(-9isize)?;
    let mut result = Vec::with_capacity(fds.len());
    for &fd in fds {
        match fd_table::get(process, fd) {
            Some(fd_obj) => result.push((fd, fd_obj)),
            None => return Err(-9), // EBADF
        }
    }
    Ok(result)
}

fn current_pid() -> Option<usize> {
    let pid = crate::sched::process::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed);
    if pid == 0 { None } else { Some(pid) }
}

fn signal_pending_for_current() -> bool {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table.as_ref().and_then(|table| {
        table.current.map(|pid| {
            crate::kernel::signal::pending_interrupting_signal_for_process(table, pid, 0)
        })
    }).unwrap_or(false)
}

fn file_readiness(fd_obj: &FileDescriptor) -> u32 {
    if let Some(file) = fd_obj.file.as_file() {
        file.readiness()
    } else {
        READINESS_READ | READINESS_WRITE
    }
}

fn revents_for(fd_obj: &FileDescriptor, events: u32) -> u32 {
    if cfg!(feature = "poll_always_ready_experiment") {
        return events & (POLLIN | POLLOUT);
    }
    if fd_obj.always_ready {
        // Regular file: Linux vfs_poll reports IN|OUT unconditionally.
        return events & (POLLIN | POLLOUT);
    }
    let readiness = file_readiness(fd_obj);
    let mut revents = 0u32;
    if events & POLLIN != 0 && readiness & READINESS_READ != 0 {
        revents |= POLLIN;
    }
    if events & POLLOUT != 0 && readiness & READINESS_WRITE != 0 {
        revents |= POLLOUT;
    }
    if readiness & READINESS_ERROR != 0 {
        revents |= POLLERR;
    }
    if readiness & READINESS_HUP != 0 {
        revents |= POLLHUP;
    }
    revents
}

fn register_wait_items(wait_items: &[(Arc<FileDescriptor>, u32)], deadline: Option<u64>) {
    let Some(pid) = current_pid() else {
        return;
    };
    if let Some(deadline) = deadline {
        if deadline > 1 {
            remember_poll_deadline(pid, deadline);
        }
    }
    for (fd_obj, events) in wait_items {
        if let Some(file) = fd_obj.file.as_file() {
            let mut readiness_events = 0u32;
            if events & POLLIN != 0 {
                readiness_events |= READINESS_READ;
            }
            if events & POLLOUT != 0 {
                readiness_events |= READINESS_WRITE;
            }
            file.register_waiter(readiness_events, pid, deadline);
        }
    }
}

fn remember_poll_deadline(pid: usize, deadline: u64) {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(table) = table.as_mut() {
        if let Some(process) = table.get_mut(pid) {
            process.waiting_for = Some(POLL_WAIT_SENTINEL);
            process.wakeup_channel = deadline as usize;
        }
    }
}

fn take_poll_timeout_reentry(has_deadline: bool) -> bool {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(table) = table.as_mut() {
        if let Some(pid) = table.current {
            if let Some(process) = table.get_mut(pid) {
                if process.waiting_for == Some(POLL_WAIT_SENTINEL) && process.wakeup_channel > 1 {
                    if !has_deadline {
                        // Infinite timeout can never time out: drop any stale
                        // deadline sentinel left over from a previous poll.
                        process.clear_wait_state();
                        return false;
                    }
                    let now = crate::arch::asm_helpers::rdtime();
                    if now >= process.wakeup_channel as u64 {
                        process.clear_wait_state();
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn clear_poll_deadline() {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(table) = table.as_mut() {
        if let Some(pid) = table.current {
            if let Some(process) = table.get_mut(pid) {
                if process.waiting_for == Some(POLL_WAIT_SENTINEL) {
                    process.clear_wait_state();
                }
            }
        }
    }
}

fn timespec_deadline(timeout_ptr: usize) -> Result<Option<u64>, isize> {
    if timeout_ptr == 0 {
        return Ok(None);
    }
    let buf = read_user_buffer(timeout_ptr as *const u8, 16).map_err(|_| -14isize)?;
    let sec = i64::from_le_bytes(buf[0..8].try_into().unwrap());
    let nsec = i64::from_le_bytes(buf[8..16].try_into().unwrap());
    if sec < 0 || nsec < 0 || nsec >= 1_000_000_000 {
        return Err(-22);
    }
    if sec == 0 && nsec == 0 {
        return Ok(Some(0));
    }
    const CLOCK_FREQ: u64 = crate::arch::consts::CLOCK_FREQ as u64;
    let now = crate::arch::asm_helpers::rdtime();
    let ticks = (sec as u64)
        .saturating_mul(CLOCK_FREQ)
        .saturating_add((nsec as u64).saturating_mul(CLOCK_FREQ) / 1_000_000_000);
    Ok(Some(now.saturating_add(ticks)))
}

fn deadline_expired(deadline: Option<u64>) -> bool {
    match deadline {
        Some(0) => true,
        Some(deadline) => crate::arch::asm_helpers::rdtime() >= deadline,
        None => false,
    }
}

fn wait_for_deadline_only(deadline: Option<u64>, wait_label: &'static str) -> isize {
    match deadline {
        Some(0) => 0,
        Some(_) | None => {
            // Use the same WFI+block loop as nfds>0 poll:
            // When woken by signal, ecall re-enters sys_ppoll → nfds=0 →
            // signal_pending check at top returns -4 (EINTR).
            loop {
                if signal_pending_for_current() {
                    return -4;
                }
                if deadline_expired(deadline) {
                    // Re-check: signal takes priority over timeout (Linux semantics)
                    if signal_pending_for_current() {
                        return -4;
                    }
                    return 0;
                }
                if super::has_other_runnable() {
                    block_current_for_poll(deadline, wait_label);
                }
                super::inline_wfi_tick();
            }
        }
    }
}

fn block_current_for_poll(deadline: Option<u64>, wait_label: &'static str) -> ! {
    crate::sched::process::enter_wait_channel(wait_label);
    if let Some(deadline) = deadline {
        crate::sched::process::suspend_current_for_io(deadline);
    } else {
        crate::sched::process::suspend_current_and_run_next();
    }
}

fn write_select_results(
    readfds: *mut u8,
    writefds: *mut u8,
    exceptfds: *mut u8,
    set_bytes: usize,
    has_read: bool,
    has_write: bool,
    read_result: &[u8],
    write_result: &[u8],
    ready_count: isize,
) -> isize {
    clear_poll_deadline();
    if has_read && write_user_buffer(readfds, read_result).is_err() {
        return -14;
    }
    if has_write && write_user_buffer(writefds, write_result).is_err() {
        return -14;
    }
    if !exceptfds.is_null() && set_bytes > 0 {
        // Write zeros in stack-sized chunks: no heap allocation.
        let zeros = [0u8; 128];
        let mut off = 0usize;
        while off < set_bytes {
            let n = (set_bytes - off).min(zeros.len());
            if write_user_buffer(unsafe { exceptfds.add(off) }, &zeros[..n]).is_err() {
                return -14;
            }
            off += n;
        }
    }
    ready_count
}
