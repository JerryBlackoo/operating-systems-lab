use core::sync::atomic::{AtomicIsize, AtomicU64, AtomicUsize, Ordering};

pub const ENABLED: bool = false;

pub const EV_CLONE_THREAD: usize = 1;
pub const EV_SETSCHED: usize = 2;
pub const EV_SLEEP_ENTER: usize = 3;
pub const EV_SLEEP_WAKE: usize = 4;
pub const EV_SWITCH: usize = 5;
pub const EV_EXIT_GROUP: usize = 6;
pub const EV_SLEEP_BUSY_DONE: usize = 7;

pub const SWITCH_TIMER: u64 = 1;
pub const SWITCH_NANOSLEEP: u64 = 2;
pub const SWITCH_YIELD: u64 = 3;

const TRACE_SIZE: usize = 4096;

struct TraceSlot {
    seq: AtomicUsize,
    ts_us: AtomicU64,
    event: AtomicUsize,
    pid: AtomicUsize,
    tgid: AtomicUsize,
    policy: AtomicIsize,
    priority: AtomicIsize,
    a: AtomicU64,
    b: AtomicU64,
}

impl TraceSlot {
    const fn new() -> Self {
        Self {
            seq: AtomicUsize::new(0),
            ts_us: AtomicU64::new(0),
            event: AtomicUsize::new(0),
            pid: AtomicUsize::new(0),
            tgid: AtomicUsize::new(0),
            policy: AtomicIsize::new(0),
            priority: AtomicIsize::new(0),
            a: AtomicU64::new(0),
            b: AtomicU64::new(0),
        }
    }
}

static NEXT_SEQ: AtomicUsize = AtomicUsize::new(0);
static FOCUS_TGID: AtomicUsize = AtomicUsize::new(0);
static DUMP_COUNT: AtomicUsize = AtomicUsize::new(0);
static TRACE: [TraceSlot; TRACE_SIZE] = [const { TraceSlot::new() }; TRACE_SIZE];
static EVENT_COUNTS: [AtomicUsize; 8] = [const { AtomicUsize::new(0) }; 8];

#[inline]
fn now_us() -> u64 {
    let freq = crate::arch::consts::CLOCK_FREQ as u64;
    let ticks_per_us = freq / 1_000_000;
    let now = crate::arch::asm_helpers::rdtime();
    if ticks_per_us == 0 { now } else { now / ticks_per_us }
}

#[inline]
pub fn focus_tgid() -> usize {
    FOCUS_TGID.load(Ordering::Relaxed)
}

#[inline]
pub fn focus_if_rt(tgid: usize, policy: i32, priority: i32) {
    if !ENABLED || tgid == 0 {
        return;
    }
    if (policy == 1 || policy == 2) && priority >= 90 {
        let _ = FOCUS_TGID.compare_exchange(0, tgid, Ordering::Relaxed, Ordering::Relaxed);
    }
}

#[inline]
pub fn is_focus_tgid(tgid: usize) -> bool {
    let focus = focus_tgid();
    focus != 0 && focus == tgid
}

#[inline]
fn should_record_tgid(tgid: usize) -> bool {
    let focus = focus_tgid();
    focus == 0 || focus == tgid
}

fn clear_once_filters() {
    let mut i = 0;
    while i < EVENT_COUNTS.len() {
        EVENT_COUNTS[i].store(0, Ordering::Relaxed);
        i += 1;
    }
}

fn event_limit(event: usize) -> usize {
    match event {
        EV_SLEEP_ENTER => 96,
        EV_SLEEP_WAKE => 96,
        EV_SLEEP_BUSY_DONE => 32,
        EV_SWITCH => 128,
        _ => usize::MAX,
    }
}

fn should_record_event(event: usize, _pid: usize) -> bool {
    if event >= EVENT_COUNTS.len() {
        return true;
    }
    EVENT_COUNTS[event].fetch_add(1, Ordering::Relaxed) < event_limit(event)
}
pub fn record_event(event: usize, pid: usize, tgid: usize, policy: i32, priority: i32, a: u64, b: u64) {
    if !ENABLED || !should_record_tgid(tgid) || !should_record_event(event, pid) {
        return;
    }
    let seq = NEXT_SEQ.fetch_add(1, Ordering::Relaxed);
    let slot = &TRACE[seq % TRACE_SIZE];
    slot.ts_us.store(now_us(), Ordering::Relaxed);
    slot.event.store(event, Ordering::Relaxed);
    slot.pid.store(pid, Ordering::Relaxed);
    slot.tgid.store(tgid, Ordering::Relaxed);
    slot.policy.store(policy as isize, Ordering::Relaxed);
    slot.priority.store(priority as isize, Ordering::Relaxed);
    slot.a.store(a, Ordering::Relaxed);
    slot.b.store(b, Ordering::Relaxed);
    slot.seq.store(seq + 1, Ordering::Release);
}

pub fn record_current_event(event: usize, a: u64, b: u64) {
    if !ENABLED {
        return;
    }
    let table_guard = super::process::PROCESS_TABLE.lock();
    if let Some(table) = table_guard.as_ref() {
        if let Some(pid) = table.current {
            if let Some(p) = table.get(pid) {
                record_event(event, pid, p.tgid, p.sched_policy, p.sched_priority, a, b);
            }
        }
    }
}

pub fn record_switch(table: &super::process::ProcessTable, prev_pid: usize, next_pid: usize, reason: u64) {
    if !ENABLED {
        return;
    }
    let focus = focus_tgid();
    if focus == 0 {
        return;
    }
    let prev_tgid = table.get(prev_pid).map(|p| p.tgid).unwrap_or(0);
    let next = match table.get(next_pid) {
        Some(p) => p,
        None => return,
    };
    if prev_tgid != focus && next.tgid != focus {
        return;
    }
    record_event(EV_SWITCH, next_pid, next.tgid, next.sched_policy, next.sched_priority, prev_pid as u64, reason);
}

fn event_name(event: usize) -> &'static str {
    match event {
        EV_CLONE_THREAD => "clone_thread",
        EV_SETSCHED => "setsched",
        EV_SLEEP_ENTER => "sleep_enter",
        EV_SLEEP_WAKE => "sleep_wake",
        EV_SWITCH => "switch",
        EV_EXIT_GROUP => "exit_group",
        EV_SLEEP_BUSY_DONE => "sleep_busy_done",
        _ => "unknown",
    }
}

fn is_struct_event(event: usize) -> bool {
    matches!(event, EV_CLONE_THREAD | EV_SETSCHED | EV_EXIT_GROUP)
}

fn take_local_once(bits_array: &mut [usize; 4], pid: usize) -> bool {
    let bits = core::mem::size_of::<usize>() * 8;
    let word = pid / bits;
    if word >= bits_array.len() {
        return true;
    }
    let bit = 1usize << (pid % bits);
    let old = bits_array[word];
    bits_array[word] |= bit;
    old & bit == 0
}

fn should_dump_dynamic(event: usize, pid: usize, sleep_bits: &mut [usize; 4], wake_bits: &mut [usize; 4], busy_bits: &mut [usize; 4], switch_bits: &mut [usize; 4]) -> bool {
    match event {
        EV_SLEEP_ENTER => take_local_once(sleep_bits, pid),
        EV_SLEEP_WAKE => take_local_once(wake_bits, pid),
        EV_SLEEP_BUSY_DONE => take_local_once(busy_bits, pid),
        EV_SWITCH => take_local_once(switch_bits, pid),
        _ => false,
    }
}

pub fn dump_once() {
    if !ENABLED {
        return;
    }
    let focus = focus_tgid();
    if focus == 0 {
        return;
    }
    if DUMP_COUNT.fetch_add(1, Ordering::AcqRel) >= 4 {
        return;
    }
    let end = NEXT_SEQ.load(Ordering::Acquire);
    let start = end.saturating_sub(TRACE_SIZE);
    crate::println!("[sched-trace] begin focus_tgid={} events={} window={}", focus, end, end - start);
    let mut sleep_bits = [0usize; 4];
    let mut wake_bits = [0usize; 4];
    let mut busy_bits = [0usize; 4];
    let mut switch_bits = [0usize; 4];
    let mut dynamic_printed = 0usize;
    let mut seq = start;
    while seq < end {
        let slot = &TRACE[seq % TRACE_SIZE];
        if slot.seq.load(Ordering::Acquire) == seq + 1 {
            let event = slot.event.load(Ordering::Relaxed);
            let tgid = slot.tgid.load(Ordering::Relaxed);
            let pid = slot.pid.load(Ordering::Relaxed);
            if tgid == focus {
                let should_print = is_struct_event(event)
                    || (dynamic_printed < 48 && should_dump_dynamic(event, pid, &mut sleep_bits, &mut wake_bits, &mut busy_bits, &mut switch_bits));
                if should_print {
                    crate::println!(
                        "[sched-trace] seq={} us={} ev={} pid={} tgid={} pol={} prio={} a={} b={}",
                        seq,
                        slot.ts_us.load(Ordering::Relaxed),
                        event_name(event),
                        pid,
                        tgid,
                        slot.policy.load(Ordering::Relaxed),
                        slot.priority.load(Ordering::Relaxed),
                        slot.a.load(Ordering::Relaxed),
                        slot.b.load(Ordering::Relaxed)
                    );
                    if !is_struct_event(event) {
                        dynamic_printed += 1;
                    }
                }
            }
        }
        seq += 1;
    }
    crate::println!("[sched-trace] end focus_tgid={}", focus);
    FOCUS_TGID.store(0, Ordering::Release);
    clear_once_filters();
}
