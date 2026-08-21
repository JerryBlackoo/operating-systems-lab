use alloc::vec::Vec;

use super::user::{read_user_buffer, write_user_buffer};

/// Maximum supplementary groups exposed by this target userspace ABI.
///
/// The LTP/musl headers used by the contest rootfs expose `NGROUPS == 32` via
/// `<sys/param.h>` and test `setgroups(NGROUPS + 1)` for `EINVAL`. Keep the
/// kernel-visible contract aligned with that ABI while still preserving the
/// Linux errno ordering and credential semantics for `getgroups(2)` and
/// `setgroups(2)`.
const NGROUPS_MAX: usize = 32;
const CAP_SETGID: u64 = 1u64 << 6;
const CAP_SETUID: u64 = 1u64 << 7;
const SECBIT_NO_SETUID_FIXUP: u32 = 1 << 2;

fn with_current<R>(default: R, f: impl FnOnce(&crate::sched::process::ProcessControlBlock) -> R) -> R {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)))
        .map(f)
        .unwrap_or(default)
}

/// 只读当前 PCB 的无表锁快路径。HART_CURRENT_PCB 契约：本 hart 在内核态
/// 关中断，PCB 指针在 current 切换前不会被释放；这里只读当前 hart 自己才会
/// 修改的 cred/identity 字段（setuid 等均由本任务自行发起），无需 PROCESS_TABLE。
fn with_current_pcb<R>(default: R, f: impl FnOnce(&crate::sched::process::ProcessControlBlock) -> R) -> R {
    let pcb_ptr = crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed);
    if !pcb_ptr.is_null() {
        return f(unsafe { &*pcb_ptr });
    }
    with_current(default, f)
}

fn with_current_mut(default: isize, f: impl FnOnce(&mut crate::sched::process::ProcessControlBlock) -> isize) -> isize {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_mut()
        .and_then(|t| t.current.and_then(|pid| t.get_mut(pid)))
        .map(f)
        .unwrap_or(default)
}

fn has_cap(p: &crate::sched::process::ProcessControlBlock, cap: u64) -> bool {
    p.euid == 0 || (p.cap_effective & cap) != 0
}

fn uid_allowed(p: &crate::sched::process::ProcessControlBlock, uid: u32) -> bool {
    has_cap(p, CAP_SETUID) || uid == p.uid || uid == p.euid || uid == p.suid
}

fn gid_allowed(p: &crate::sched::process::ProcessControlBlock, gid: u32) -> bool {
    has_cap(p, CAP_SETGID) || gid == p.gid || gid == p.egid || gid == p.sgid
}

/// Linux commoncap-style setxuid fixup. Losing euid 0 clears effective caps;
/// regaining euid 0 restores them from permitted. Dropping the last root uid
/// clears permitted too unless PR_SET_KEEPCAPS/securebits requested otherwise.
fn apply_uid_cap_fixup(
    p: &mut crate::sched::process::ProcessControlBlock,
    old_uid: u32,
    old_euid: u32,
    old_suid: u32,
) {
    if (p.securebits & SECBIT_NO_SETUID_FIXUP) != 0 {
        return;
    }

    let old_had_root = old_uid == 0 || old_euid == 0 || old_suid == 0;
    let new_has_root = p.uid == 0 || p.euid == 0 || p.suid == 0;
    if old_had_root && !new_has_root && !p.keepcaps {
        p.cap_permitted = 0;
        p.cap_effective = 0;
        p.cap_ambient = 0;
        return;
    }

    if old_euid == 0 && p.euid != 0 {
        p.cap_effective = 0;
        p.cap_ambient = 0;
    } else if old_euid != 0 && p.euid == 0 {
        p.cap_effective = p.cap_permitted;
    }

    p.cap_ambient &= p.cap_permitted & p.cap_inheritable;
}

/// Validate a user range against the process VMA model before copying.
///
/// `read_user_buffer`/`write_user_buffer` work at PTE granularity and may
/// consider the page that contains `sbrk(0)` valid because this kernel keeps a
/// small Heap VMA padding for musl malloc metadata. Linux `copy_from_user`,
/// however, must reject the logical address at/after current brk. Therefore
/// Heap ranges are additionally clamped by `ProcessControlBlock::heap_end`.
fn current_user_range_accessible(start: usize, len: usize, write: bool) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = start.checked_add(len) else {
        return false;
    };
    if start == 0 {
        return false;
    }

    let table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(t) = table.as_ref() else { return false; };
    let Some(current_pid) = t.current else { return false; };
    let Some(current) = t.get(current_pid) else { return false; };
    let as_pid = if current.mm.is_some() { current_pid } else { current.tgid };
    let Some(process) = t.get(as_pid) else { return false; };
    let Some(address_space) = process.address_space_lock() else { return false; };

    let mut cursor = start;
    while cursor < end {
        let mut next = None;
        for vma in address_space.vmas_iter() {
            if cursor < vma.start.0 || cursor >= vma.end.0 || !vma.permission.user {
                continue;
            }
            let access_ok = if write {
                vma.permission.write
            } else {
                vma.permission.read || vma.permission.write
            };
            if !access_ok {
                continue;
            }

            let mut seg_end = vma.end.0.min(end);
            if vma.vma_type == crate::mm::VmaType::Heap {
                seg_end = seg_end.min(process.heap_end.0);
            }
            if seg_end > cursor {
                next = Some(seg_end);
                break;
            }
        }
        match next {
            Some(seg_end) => cursor = seg_end,
            None => return false,
        }
    }
    true
}

// getuid/geteuid/getgid/getegid report the credential as seen *inside* the
// caller's user namespace (对标 from_kuid_munged): the process stores a kernel
// (outside) id; we translate it through the active user_ns id map. An unmapped
// id (e.g. a fresh user namespace with no uid_map written yet) reports the
// kernel overflow id 65534. The init user ns owns a full identity map, so this
// is a no-op for ordinary processes.
fn munge_uid(p: &crate::sched::process::ProcessControlBlock, raw: u32) -> isize {
    let ns = p.nsproxy.lock();
    let mapped = ns.user_ns.map_uid_from_outside(raw);
    mapped.unwrap_or(crate::kernel::namespace::OVERFLOW_UID) as isize
}

fn munge_gid(p: &crate::sched::process::ProcessControlBlock, raw: u32) -> isize {
    let ns = p.nsproxy.lock();
    ns.user_ns
        .map_gid_from_outside(raw)
        .unwrap_or(crate::kernel::namespace::OVERFLOW_UID) as isize
}

pub(in crate::syscall) fn sys_getuid() -> isize {
    with_current_pcb(0, |p| munge_uid(p, p.uid))
}

pub(in crate::syscall) fn sys_geteuid() -> isize {
    with_current_pcb(0, |p| munge_uid(p, p.euid))
}

pub(in crate::syscall) fn sys_getgid() -> isize {
    with_current_pcb(0, |p| munge_gid(p, p.gid))
}

pub(in crate::syscall) fn sys_getegid() -> isize {
    with_current_pcb(0, |p| munge_gid(p, p.egid))
}

pub(in crate::syscall) fn sys_setuid(uid: u32) -> isize {
    with_current_mut(-22, |p| {
        if !uid_allowed(p, uid) {
            return -1;
        }
        let old = (p.uid, p.euid, p.suid);
        if has_cap(p, CAP_SETUID) {
            p.uid = uid;
            p.euid = uid;
            p.suid = uid;
        } else {
            p.euid = uid;
        }
        apply_uid_cap_fixup(p, old.0, old.1, old.2);
        0
    })
}

pub(in crate::syscall) fn sys_seteuid(euid: u32) -> isize {
    with_current_mut(-22, |p| {
        if !uid_allowed(p, euid) {
            return -1;
        }
        let old = (p.uid, p.euid, p.suid);
        p.euid = euid;
        apply_uid_cap_fixup(p, old.0, old.1, old.2);
        0
    })
}

pub(in crate::syscall) fn sys_setgid(gid: u32) -> isize {
    with_current_mut(-22, |p| {
        if !gid_allowed(p, gid) {
            return -1;
        }
        if has_cap(p, CAP_SETGID) {
            p.gid = gid;
            p.egid = gid;
            p.sgid = gid;
        } else {
            p.egid = gid;
        }
        0
    })
}

pub(in crate::syscall) fn sys_setegid(egid: u32) -> isize {
    with_current_mut(-22, |p| {
        if !gid_allowed(p, egid) {
            return -1;
        }
        p.egid = egid;
        0
    })
}

pub(in crate::syscall) fn sys_getresuid(ruid: *mut u32, euid: *mut u32, suid: *mut u32) -> isize {
    let (r, e, s) = with_current((0u32, 0u32, 0u32), |p| (p.uid, p.euid, p.suid));
    if !ruid.is_null() && write_user_buffer(ruid as *mut u8, &r.to_ne_bytes()).is_err() {
        return -14;
    }
    if !euid.is_null() && write_user_buffer(euid as *mut u8, &e.to_ne_bytes()).is_err() {
        return -14;
    }
    if !suid.is_null() && write_user_buffer(suid as *mut u8, &s.to_ne_bytes()).is_err() {
        return -14;
    }
    0
}

pub(in crate::syscall) fn sys_getresgid(rgid: *mut u32, egid: *mut u32, sgid: *mut u32) -> isize {
    let (r, e, s) = with_current((0u32, 0u32, 0u32), |p| (p.gid, p.egid, p.sgid));
    if !rgid.is_null() && write_user_buffer(rgid as *mut u8, &r.to_ne_bytes()).is_err() {
        return -14;
    }
    if !egid.is_null() && write_user_buffer(egid as *mut u8, &e.to_ne_bytes()).is_err() {
        return -14;
    }
    if !sgid.is_null() && write_user_buffer(sgid as *mut u8, &s.to_ne_bytes()).is_err() {
        return -14;
    }
    0
}

/// `getgroups(size, list)` — return supplementary group IDs.
///
/// Linux semantics:
/// - `size < 0` appears as a huge `usize` after syscall ABI conversion and
///   fails with `EINVAL`.
/// - `size == 0` returns the number of supplementary groups and does not touch
///   `list`.
/// - `0 < size < ngroups` fails with `EINVAL`.
/// - `list` points to a user `gid_t[]` (`u32` on RV64/LA64 musl); bad user
///   addresses fail with `EFAULT`.
pub(in crate::syscall) fn sys_getgroups(size: usize, list: *mut u32) -> isize {
    if size > NGROUPS_MAX {
        return -22; // EINVAL
    }
    let groups = with_current(Vec::new(), |p| p.supplementary_groups.clone());
    let ngroups = groups.len();
    if size == 0 {
        return ngroups as isize;
    }
    if list.is_null() {
        return -14; // EFAULT
    }
    if size < ngroups {
        return -22; // EINVAL
    }
    let mut bytes = Vec::with_capacity(ngroups * core::mem::size_of::<u32>());
    for gid in groups {
        bytes.extend_from_slice(&gid.to_ne_bytes());
    }
    if !current_user_range_accessible(list as usize, bytes.len(), true) {
        return -14; // EFAULT
    }
    if write_user_buffer(list as *mut u8, &bytes).is_err() {
        return -14; // EFAULT
    }
    ngroups as isize
}

/// `setgroups(size, list)` — replace supplementary group IDs.
///
/// Root or `CAP_SETGID` may set the list. `setgroups(0, NULL)` clears it.
/// Count validation happens before user memory reads, matching Linux errno
/// priority (`EINVAL` before `EFAULT` when size is too large).
pub(in crate::syscall) fn sys_setgroups(size: usize, list: *const u32) -> isize {
    if size > NGROUPS_MAX {
        return -22; // EINVAL
    }
    let can_set = with_current(false, |p| p.euid == 0 || (p.cap_effective & CAP_SETGID) != 0);
    if !can_set {
        return -1; // EPERM
    }
    let groups = if size == 0 {
        Vec::new()
    } else {
        if list.is_null() {
            return -14; // EFAULT
        }
        let byte_len = match size.checked_mul(core::mem::size_of::<u32>()) {
            Some(v) => v,
            None => return -22,
        };
        if !current_user_range_accessible(list as usize, byte_len, false) {
            return -14; // EFAULT
        }
        let raw = match read_user_buffer(list as *const u8, byte_len) {
            Ok(v) => v,
            Err(_) => return -14,
        };
        let mut groups = Vec::with_capacity(size);
        for chunk in raw.chunks_exact(4) {
            groups.push(u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
        }
        groups
    };
    with_current_mut(-3, |p| {
        p.supplementary_groups = groups;
        0
    })
}

// ----------------------------------------------------------------------------
// Slice 1.9 — POSIX identity extensions
// ----------------------------------------------------------------------------

/// `getsid(pid)` — POSIX 真实现：返回目标进程的 session id。
///
///   * `pid == 0` → 调用进程自己的 sid
///   * `pid >  0` → 目标进程的 sid；不存在返回 `-ESRCH (-3)`
///   * `pid <  0` → POSIX 未定义；与 Linux 一致返 `-ESRCH`
///
/// session id 来源是 `ProcessControlBlock::sid`，由 `setsid()` 设置、
/// fork/clone 时继承。
pub(in crate::syscall) fn sys_getsid(pid: isize) -> isize {
    if pid < 0 {
        return -3;
    }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_ref() { Some(t) => t, None => return -3 };
    let target = if pid == 0 {
        match t.current { Some(p) => p, None => return -3 }
    } else {
        pid as usize
    };
    // session id 按调用者所在 PID namespace 翻译：会话 leader 在祖先 ns
    // 时不可见返 0，否则按偏移翻译（pidns02）。
    let caller_root = t.current.and_then(|c| t.get(c)).map(|p| p.pidns_root).unwrap_or(0);
    match t.get(target) {
        Some(p) => super::process_info::ns_view_leader_or_zero(p.sid, caller_root),
        None => -3,
    }
}

/// setreuid(ruid, euid) — set real & effective uid. `-1` means "unchanged".
///
/// POSIX/Linux 派生 saved-uid 规则：若 `ruid >= 0`，或者 `euid >= 0` 且新 euid
/// 不等于旧 uid，则 saved_uid 被设为新 euid。这保证了 `seteuid(new)` 后
/// `setuid(old_uid)` 仍能恢复原 euid。
pub(in crate::syscall) fn sys_setreuid(ruid: i32, euid: i32) -> isize {
    with_current_mut(-22, |p| {
        let old_uid = p.uid;
        let new_uid = if ruid >= 0 { ruid as u32 } else { p.uid };
        let new_euid = if euid >= 0 { euid as u32 } else { p.euid };
        if (ruid >= 0 && !uid_allowed(p, new_uid)) || (euid >= 0 && !uid_allowed(p, new_euid)) {
            return -1;
        }
        let old = (p.uid, p.euid, p.suid);
        p.uid = new_uid;
        p.euid = new_euid;
        if ruid >= 0 || (euid >= 0 && new_euid != old_uid) {
            p.suid = new_euid;
        }
        apply_uid_cap_fixup(p, old.0, old.1, old.2);
        0
    })
}

/// setregid(rgid, egid) — set real & effective gid. saved-gid 派生规则同上。
pub(in crate::syscall) fn sys_setregid(rgid: i32, egid: i32) -> isize {
    with_current_mut(-22, |p| {
        let old_gid = p.gid;
        let new_gid = if rgid >= 0 { rgid as u32 } else { p.gid };
        let new_egid = if egid >= 0 { egid as u32 } else { p.egid };
        if (rgid >= 0 && !gid_allowed(p, new_gid)) || (egid >= 0 && !gid_allowed(p, new_egid)) {
            return -1;
        }
        p.gid = new_gid;
        p.egid = new_egid;
        if rgid >= 0 || (egid >= 0 && new_egid != old_gid) {
            p.sgid = new_egid;
        }
        0
    })
}

/// setresuid(ruid, euid, suid) — set real, effective, saved uid。
/// `-1` 表示"对应字段不变"，与 Linux 一致。
pub(in crate::syscall) fn sys_setresuid(ruid: i32, euid: i32, suid: i32) -> isize {
    with_current_mut(-22, |p| {
        let new_uid = if ruid >= 0 { ruid as u32 } else { p.uid };
        let new_euid = if euid >= 0 { euid as u32 } else { p.euid };
        let new_suid = if suid >= 0 { suid as u32 } else { p.suid };
        if (ruid >= 0 && !uid_allowed(p, new_uid))
            || (euid >= 0 && !uid_allowed(p, new_euid))
            || (suid >= 0 && !uid_allowed(p, new_suid))
        {
            return -1;
        }
        let old = (p.uid, p.euid, p.suid);
        p.uid = new_uid;
        p.euid = new_euid;
        p.suid = new_suid;
        apply_uid_cap_fixup(p, old.0, old.1, old.2);
        0
    })
}

/// setresgid(rgid, egid, sgid) — set real, effective, saved gid。
pub(in crate::syscall) fn sys_setresgid(rgid: i32, egid: i32, sgid: i32) -> isize {
    with_current_mut(-22, |p| {
        let new_gid = if rgid >= 0 { rgid as u32 } else { p.gid };
        let new_egid = if egid >= 0 { egid as u32 } else { p.egid };
        let new_sgid = if sgid >= 0 { sgid as u32 } else { p.sgid };
        if (rgid >= 0 && !gid_allowed(p, new_gid))
            || (egid >= 0 && !gid_allowed(p, new_egid))
            || (sgid >= 0 && !gid_allowed(p, new_sgid))
        {
            return -1;
        }
        p.gid = new_gid;
        p.egid = new_egid;
        p.sgid = new_sgid;
        0
    })
}

/// setfsuid(fsuid) — set filesystem uid. Linux returns the previous fsuid.
/// Only changes if caller is privileged or new fsuid matches ruid/euid/suid/current fsuid.
pub(in crate::syscall) fn sys_setfsuid(fsuid: i32) -> isize {
    with_current_mut(0, |p| {
        let prev = p.euid as isize;
        if fsuid < 0 {
            return prev;
        }
        let new_uid = fsuid as u32;
        // Linux: only change if CAP_SETUID (root) or new matches real/effective/saved/current
        let allowed = p.euid == 0
            || new_uid == p.uid
            || new_uid == p.euid
            || new_uid == p.suid;
        if allowed {
            p.euid = new_uid;
        }
        prev
    })
}

/// setfsgid(fsgid) — set filesystem gid. Linux returns the previous fsgid.
/// Only changes if caller is privileged or new fsgid matches rgid/egid/sgid/current fsgid.
pub(in crate::syscall) fn sys_setfsgid(fsgid: i32) -> isize {
    with_current_mut(0, |p| {
        let prev = p.egid as isize;
        if fsgid < 0 {
            return prev;
        }
        let new_gid = fsgid as u32;
        // Linux: only change if CAP_SETGID (root) or new matches real/effective/saved/current
        let allowed = p.euid == 0
            || new_gid == p.gid
            || new_gid == p.egid
            || new_gid == p.sgid;
        if allowed {
            p.egid = new_gid;
        }
        prev
    })
}
