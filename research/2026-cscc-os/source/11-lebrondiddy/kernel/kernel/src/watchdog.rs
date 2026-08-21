use alloc::{sync::Weak, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use core::time::Duration;

use crate::shutdown::shutdown;
use crate::tasks::UserTask;
use polyhal::timer::current_time;
use sync::Mutex;

const NANOS_PER_SEC: u64 = 1_000_000_000;

static GLOBAL_DEADLINE_NS: AtomicU64 = AtomicU64::new(0);
static GLOBAL_DEADLINE_FIRED: AtomicBool = AtomicBool::new(false);
static CHILD_TASK_BUDGET_NS: AtomicU64 = AtomicU64::new(0);
static CHILD_TASK_BUDGET_EXEMPT_ID: AtomicUsize = AtomicUsize::new(0);
static WATCHED_USER_TASKS: Mutex<Vec<Weak<UserTask>>> = Mutex::new(Vec::new());

pub fn duration_to_ns(duration: Duration) -> u64 {
    duration
        .as_secs()
        .saturating_mul(NANOS_PER_SEC)
        .saturating_add(duration.subsec_nanos() as u64)
}

pub fn current_time_ns() -> u64 {
    duration_to_ns(current_time())
}

pub fn arm_global_deadline_after(duration: Duration) {
    let deadline = duration_to_ns(current_time().saturating_add(duration));
    GLOBAL_DEADLINE_FIRED.store(false, Ordering::SeqCst);
    GLOBAL_DEADLINE_NS.store(deadline, Ordering::SeqCst);
}

pub fn disarm_global_deadline() {
    GLOBAL_DEADLINE_NS.store(0, Ordering::SeqCst);
}

pub fn set_child_task_budget(duration: Duration, exempt_task_id: usize) {
    CHILD_TASK_BUDGET_EXEMPT_ID.store(exempt_task_id, Ordering::SeqCst);
    CHILD_TASK_BUDGET_NS.store(duration_to_ns(duration), Ordering::SeqCst);
}

pub fn clear_child_task_budget() {
    CHILD_TASK_BUDGET_NS.store(0, Ordering::SeqCst);
    CHILD_TASK_BUDGET_EXEMPT_ID.store(0, Ordering::SeqCst);
}

pub fn active_child_task_budget_ns(task_id: usize) -> u64 {
    if CHILD_TASK_BUDGET_EXEMPT_ID.load(Ordering::Relaxed) == task_id {
        0
    } else {
        CHILD_TASK_BUDGET_NS.load(Ordering::Relaxed)
    }
}

pub fn new_child_task_deadline_ns(task_id: usize) -> u64 {
    let budget = active_child_task_budget_ns(task_id);
    if budget == 0 {
        0
    } else {
        current_time_ns().saturating_add(budget)
    }
}

pub fn deadline_after(duration: Duration) -> u64 {
    duration_to_ns(current_time().saturating_add(duration))
}

pub fn register_user_task(task: Weak<UserTask>) {
    WATCHED_USER_TASKS.lock().push(task);
}

pub fn check_user_task_deadlines_from_timer() {
    let Some(mut tasks) = WATCHED_USER_TASKS.try_lock() else {
        return;
    };
    let mut index = 0;
    while index < tasks.len() {
        match tasks[index].upgrade() {
            Some(task) => {
                task.check_wall_deadline_from_timer();
                index += 1;
            }
            None => {
                tasks.swap_remove(index);
            }
        }
    }
}

pub fn check_global_deadline_from_timer() {
    let deadline = GLOBAL_DEADLINE_NS.load(Ordering::Relaxed);
    if deadline == 0 {
        return;
    }
    if duration_to_ns(current_time()) < deadline {
        return;
    }
    if GLOBAL_DEADLINE_FIRED.swap(true, Ordering::SeqCst) {
        return;
    }

    println!("kernel watchdog: global deadline reached from timer interrupt; forcing shutdown");
    shutdown();
}
