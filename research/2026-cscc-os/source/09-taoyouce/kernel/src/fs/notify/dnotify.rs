extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::fs::{FileDescriptor, FileType};
use crate::sched::process::{PROCESS_TABLE, ProcessState};
use crate::sync::IrqSpinLock;

pub const DN_ACCESS: u32 = 0x00000001;
pub const DN_MODIFY: u32 = 0x00000002;
pub const DN_CREATE: u32 = 0x00000004;
pub const DN_DELETE: u32 = 0x00000008;
pub const DN_RENAME: u32 = 0x00000010;
pub const DN_ATTRIB: u32 = 0x00000020;
pub const DN_MULTISHOT: u32 = 0x80000000;

const SIGIO: i32 = 29;
const MAX_QUEUED_SIGINFO_FDS: usize = 64;

#[derive(Clone)]
struct DnotifyRegistration {
    ofd_id: usize,
    owner_pid: usize,
    target_type: i32,
    target_pid: i32,
    fd: i32,
    mask: u32,
    signal: i32,
}

#[derive(Clone)]
struct DnotifyDelivery {
    owner_pid: usize,
    target_type: i32,
    target_pid: i32,
    fd: i32,
    signal: i32,
}

struct DnotifyRegistry {
    inner: IrqSpinLock<BTreeMap<String, Vec<DnotifyRegistration>>>,
}

impl DnotifyRegistry {
    const fn new() -> Self {
        Self { inner: IrqSpinLock::new(BTreeMap::new()) }
    }

    fn register(&self, path: &str, reg: DnotifyRegistration) {
        let mut inner = self.inner.lock();
        let path_key = path.to_string();
        let list = inner.entry(path_key.clone()).or_insert_with(Vec::new);
        let before = list.len();
        list.retain(|old| old.ofd_id != reg.ofd_id);
        let removed = before.saturating_sub(list.len());
        if removed != 0 {
            DNOTIFY_ACTIVE.fetch_sub(removed, Ordering::Relaxed);
        }
        if reg.mask & !DN_MULTISHOT != 0 {
            list.push(reg);
            DNOTIFY_ACTIVE.fetch_add(1, Ordering::Relaxed);
        }
        if list.is_empty() {
            inner.remove(&path_key);
        }
    }

    fn unregister_ofd(&self, ofd_id: usize) {
        let mut inner = self.inner.lock();
        let keys: Vec<String> = inner.keys().cloned().collect();
        for key in keys {
            if let Some(list) = inner.get_mut(&key) {
                let before = list.len();
                list.retain(|reg| reg.ofd_id != ofd_id);
                let removed = before.saturating_sub(list.len());
                if removed != 0 {
                    DNOTIFY_ACTIVE.fetch_sub(removed, Ordering::Relaxed);
                }
                if list.is_empty() {
                    inner.remove(&key);
                }
            }
        }
    }

    fn notify_exact(&self, path: &str, event: u32) {
        if DNOTIFY_ACTIVE.load(Ordering::Relaxed) == 0 {
            return;
        }
        let deliveries = {
            let mut inner = self.inner.lock();
            let mut deliveries = Vec::new();
            let mut empty = false;
            if let Some(list) = inner.get_mut(path) {
                let mut idx = 0;
                while idx < list.len() {
                    if list[idx].mask & event != 0 {
                        deliveries.push(DnotifyDelivery {
                            owner_pid: list[idx].owner_pid,
                            target_type: list[idx].target_type,
                            target_pid: list[idx].target_pid,
                            fd: list[idx].fd,
                            signal: list[idx].signal,
                        });
                        if list[idx].mask & DN_MULTISHOT == 0 {
                            list.remove(idx);
                            DNOTIFY_ACTIVE.fetch_sub(1, Ordering::Relaxed);
                            continue;
                        }
                    }
                    idx += 1;
                }
                empty = list.is_empty();
            }
            if empty {
                inner.remove(path);
            }
            deliveries
        };
        for delivery in deliveries {
            queue_delivery(delivery);
        }
    }
}

static DNOTIFY_REGISTRY: DnotifyRegistry = DnotifyRegistry::new();
static DNOTIFY_ACTIVE: AtomicUsize = AtomicUsize::new(0);

pub fn set_watch(fd_obj: Arc<FileDescriptor>, fd: i32, mask: u32) -> isize {
    if mask != 0 && fd_obj.file.file_type() != FileType::Directory {
        return -20;
    }
    let ofd_id = Arc::as_ptr(&fd_obj) as usize;
    if mask == 0 {
        DNOTIFY_REGISTRY.unregister_ofd(ofd_id);
        return 0;
    }
    let path = match fd_obj.path.as_ref() {
        Some(path) => path.clone(),
        None => return -22,
    };
    let owner_pid = {
        let table = PROCESS_TABLE.lock();
        match table.as_ref().and_then(|t| t.current) {
            Some(pid) => pid,
            None => return -3,
        }
    };
    let signal = fd_obj.f_owner_signal.load(Ordering::Relaxed);
    let signal = if signal > 0 { signal } else { SIGIO };
    let target_type = fd_obj.f_owner_type.load(Ordering::Relaxed);
    let target_pid = fd_obj.f_owner_pid.load(Ordering::Relaxed);
    DNOTIFY_REGISTRY.register(&path, DnotifyRegistration {
        ofd_id,
        owner_pid,
        target_type,
        target_pid,
        fd,
        mask,
        signal,
    });
    0
}

pub fn unregister_ofd(ofd_id: usize) {
    DNOTIFY_REGISTRY.unregister_ofd(ofd_id);
}

/// 热路径合并门：是否存在任何 dnotify 订阅。
#[inline]
pub fn watches_active() -> bool {
    DNOTIFY_ACTIVE.load(Ordering::Relaxed) != 0
}

pub fn notify_file_path(path: &str, event: u32) {
    DNOTIFY_REGISTRY.notify_exact(path, event);
    if let Some(parent) = parent_path(path) {
        DNOTIFY_REGISTRY.notify_exact(parent, event);
    }
}

pub fn notify_path(path: &str, event: u32) {
    DNOTIFY_REGISTRY.notify_exact(path, event);
}

pub fn notify_rename(old_parent_path: &str, new_parent_path: &str) {
    if old_parent_path == new_parent_path {
        notify_path(old_parent_path, DN_RENAME);
    }
}

fn parent_path(path: &str) -> Option<&str> {
    match path.rfind('/') {
        Some(pos) if pos > 0 => Some(&path[..pos]),
        Some(0) if path.len() > 1 => Some("/"),
        _ => None,
    }
}

fn queue_delivery(delivery: DnotifyDelivery) {
    let sig = delivery.signal;
    if !(1..=64).contains(&sig) {
        return;
    }
    let mut table = PROCESS_TABLE.lock();
    if let Some(ref mut t) = *table {
        if delivery.target_type == 2 && delivery.target_pid > 0 {
            let pgrp = delivery.target_pid as usize;
            let pids = t.pids();
            for pid in pids {
                if t.get(pid).map_or(false, |process| process.pgid == pgrp) {
                    queue_delivery_to_pid(t, pid, sig, delivery.fd);
                }
            }
        } else {
            let target_pid = if delivery.target_pid > 0 {
                delivery.target_pid as usize
            } else {
                delivery.owner_pid
            };
            queue_delivery_to_pid(t, target_pid, sig, delivery.fd);
        }
    }
}

fn queue_delivery_to_pid(
    table: &mut crate::sched::process::ProcessTable,
    pid: usize,
    sig: i32,
    fd: i32,
) {
    let should_wake = if let Some(process) = table.get_mut(pid) {
        if process.pending_signal_fds.len() >= MAX_QUEUED_SIGINFO_FDS {
            process.pending_signal_fds.remove(0);
        }
        process.pending_signal_fds.push((sig, fd));
        process.pending_signals |= 1u64 << (sig as u32 - 1); crate::sched::process::note_signal_hint();
        process.state == ProcessState::Sleeping
    } else {
        false
    };
    if should_wake {
        table.mark_runnable(pid, true);
    }
}
