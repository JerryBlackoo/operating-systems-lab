//! Linux keyring subsystem — add_key / request_key / keyctl
//!
//! Extracted from security.rs for low-coupling subsystem architecture.
//! Implements the full Linux key management facility per `keyctl(2)`,
//! `add_key(2)`, `request_key(2)`.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::security::{current_pid, current_uid, read_optional_cstring, read_path};
use super::user::{read_user_buffer, write_user_buffer};
use crate::sync::SpinLock;

// ── types ───────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct KeyEntry {
    owner: u32,
    gid: u32,
    is_keyring: bool,
    key_type: String,
    description: String,
    payload: Vec<u8>,
    perm: u32,
    timeout: u64, // 0 = no timeout
    /// Monotonic-seconds deadline set by KEYCTL_SET_TIMEOUT (0 = never expires).
    expiry: u64,
    /// Revoked by KEYCTL_REVOKE: stays linked but yields EKEYREVOKED.
    revoked: bool,
    children: Vec<i32>, // for keyrings: linked key IDs
    /// Negatively instantiated key (request_key construction failed): reading
    /// it yields ENOKEY, never the payload (keyctl07 / CVE-2017-12192).
    negative: bool,
}

impl KeyEntry {
    fn new_keyring(owner: u32, desc: &str) -> Self {
        Self {
            owner, gid: 0, is_keyring: true,
            key_type: "keyring".into(), description: desc.into(),
            payload: Vec::new(),
            perm: 0x3f3f0000, // KEY_POS_ALL | KEY_USR_ALL
            timeout: 0, expiry: 0, revoked: false, children: Vec::new(),
            negative: false,
        }
    }
    fn new_user(owner: u32, desc: &str, payload: Vec<u8>) -> Self {
        Self {
            owner, gid: 0, is_keyring: false,
            key_type: "user".into(), description: desc.into(),
            payload,
            perm: 0x3f3f0000,
            timeout: 0, expiry: 0, revoked: false, children: Vec::new(),
            negative: false,
        }
    }
    fn new_negative(owner: u32, key_type: &str, desc: &str) -> Self {
        Self {
            owner, gid: 0, is_keyring: false,
            key_type: key_type.into(), description: desc.into(),
            payload: Vec::new(),
            perm: 0x3f3f0000,
            timeout: 0, expiry: 0, revoked: false, children: Vec::new(),
            negative: true,
        }
    }
    /// key_validate(): EKEYREVOKED / EKEYEXPIRED, else Ok.
    fn validate(&self) -> Result<(), isize> {
        if self.revoked { return Err(-128); } // EKEYREVOKED
        if self.expiry != 0 && key_now_sec() >= self.expiry {
            return Err(-127); // EKEYEXPIRED
        }
        Ok(())
    }
}

/// Monotonic seconds since boot (for key expiry bookkeeping).
fn key_now_sec() -> u64 {
    crate::arch::asm_helpers::rdtime() / crate::arch::consts::CLOCK_FREQ as u64
}

// ── statics ─────────────────────────────────────────────────────────────────

static KEY_ENTRIES: SpinLock<BTreeMap<i32, KeyEntry>> =
    SpinLock::new(BTreeMap::new());
static SPECIAL_KEYRINGS: SpinLock<BTreeMap<(i32, usize), i32>> =
    SpinLock::new(BTreeMap::new());
static NEXT_KEY_ID: SpinLock<i32> = SpinLock::new(1000);
/// Per-process default request-key keyring setting
static REQKEY_KEYRING: SpinLock<BTreeMap<usize, i32>> =
    SpinLock::new(BTreeMap::new());

// ── helpers ─────────────────────────────────────────────────────────────────

fn key_next_id() -> i32 {
    let mut next = NEXT_KEY_ID.lock();
    let id = *next;
    *next = next.saturating_add(1);
    id
}

fn special_key_slot(ringid: i32) -> Option<(i32, usize)> {
    let pid = current_pid();
    let uid = current_uid() as usize;
    match ringid {
        -1 => Some((-1, pid)),
        -2 => Some((-2, pid)),
        -3 => Some((-3, pid)),
        -4 => Some((-4, uid)),
        -5 => Some((-5, uid)),
        _ => None,
    }
}

fn ensure_special_keyring(ringid: i32) -> Result<i32, isize> {
    let slot = special_key_slot(ringid).ok_or(-22isize)?;
    {
        let special = SPECIAL_KEYRINGS.lock();
        if let Some(id) = special.get(&slot) {
            return Ok(*id);
        }
    }
    let desc = match ringid {
        -1 => "_tid-keyring",
        -2 => "_pid-keyring",
        -3 => "_ses-keyring",
        -4 => "_uid-keyring",
        -5 => "_uid_ses-keyring",
        _ => "_unknown-keyring",
    };
    let id = key_next_id();
    KEY_ENTRIES.lock().insert(id, KeyEntry::new_keyring(current_uid(), desc));
    SPECIAL_KEYRINGS.lock().insert(slot, id);
    Ok(id)
}

fn keyring_exists(id: i32) -> bool {
    if id < 0 {
        return special_key_slot(id).is_some();
    }
    KEY_ENTRIES.lock().get(&id).map(|entry| entry.is_keyring).unwrap_or(false)
}

fn resolve_key_id(id: i32, create: bool) -> Result<i32, isize> {
    if id < 0 {
        if create {
            ensure_special_keyring(id)
        } else {
            let slot = special_key_slot(id).ok_or(-22isize)?;
            let special = SPECIAL_KEYRINGS.lock();
            special.get(&slot).copied().ok_or(-126isize) // ENOKEY
        }
    } else {
        if KEY_ENTRIES.lock().contains_key(&id) {
            Ok(id)
        } else {
            Err(-126) // ENOKEY
        }
    }
}

/// Linux KEYRING_SEARCH_MAX_DEPTH: nested-keyring search recursion cap.
const KEYRING_SEARCH_MAX_DEPTH: usize = 6;

fn collect_keyring_keys(ring_id: i32, entries: &BTreeMap<i32, KeyEntry>) -> Vec<i32> {
    let mut result = Vec::new();
    let mut visited = Vec::new();
    collect_keyring_keys_inner(ring_id, entries, &mut result, &mut visited, 0);
    result
}

fn collect_keyring_keys_inner(
    ring_id: i32,
    entries: &BTreeMap<i32, KeyEntry>,
    result: &mut Vec<i32>,
    visited: &mut Vec<i32>,
    depth: usize,
) {
    if depth >= KEYRING_SEARCH_MAX_DEPTH || visited.contains(&ring_id) {
        return;
    }
    visited.push(ring_id);
    if let Some(ring) = entries.get(&ring_id) {
        for &child_id in &ring.children {
            result.push(child_id);
            if let Some(child) = entries.get(&child_id) {
                if child.is_keyring {
                    collect_keyring_keys_inner(child_id, entries, result, visited, depth + 1);
                }
            }
        }
    }
}

/// keyring_link cycle detection: linking `kid` into `rid` must not create a
/// cycle (EDEADLK), including the trivial self-link.
fn link_would_cycle(kid: i32, rid: i32, entries: &BTreeMap<i32, KeyEntry>) -> bool {
    if kid == rid { return true; }
    if !entries.get(&kid).map(|e| e.is_keyring).unwrap_or(false) {
        return false;
    }
    collect_keyring_keys(kid, entries).contains(&rid)
}

// ── syscalls ────────────────────────────────────────────────────────────────

pub(crate) fn sys_add_key(
    type_: *const u8,
    description: *const u8,
    payload: *const u8,
    plen: usize,
    keyring: isize,
) -> isize {
    let key_type = match read_path(type_) {
        Ok(value) => value,
        Err(errno) => return errno,
    };
    let desc = match read_optional_cstring(description) {
        Ok(Some(s)) => s,
        Ok(None) => String::new(),
        Err(_) => return -14,
    };
    let ringid = keyring as i32;
    if !keyring_exists(ringid) {
        return -126; // ENOKEY
    }
    // Linux sys_add_key: the payload is copied from userspace *before* the key
    // type is looked up (commit 5649645d725c, CVE-2017-15274) — a NULL payload
    // with nonzero plen faults with EFAULT for every type, known or not, and
    // plen > 1MiB-1 is rejected with EINVAL up front.
    if plen > (1usize << 20) - 1 {
        return -22;
    }
    let data = if plen > 0 {
        if payload.is_null() { return -14; }
        match read_user_buffer(payload, plen) {
            Ok(d) => d, Err(_) => return -14,
        }
    } else {
        Vec::new()
    };
    let entry = match key_type.as_str() {
        "keyring" => {
            if plen != 0 { return -22; }
            KeyEntry::new_keyring(current_uid(), &desc)
        }
        "user" | "logon" | "big_key" => {
            // Linux per-type payload caps: user/logon quota is 32767 bytes;
            // big_key allows up to 1 MiB - 1 (add_key01 big_key 1048575 must
            // succeed, 1048576 must fail with EINVAL).
            let max_plen = if key_type == "big_key" { (1usize << 20) - 1 } else { 32767 };
            if plen > max_plen { return -22; }
            KeyEntry::new_user(current_uid(), &desc, data)
        }
        // Linux key_type_lookup() returns -ENODEV for a key type that isn't
        // registered (add_key02/request_key03 probe unsupported types and
        // TCONF-skip on ENODEV; ENOKEY would be a spurious TBROK/TFAIL).
        _ => return -19, // ENODEV: unknown/unsupported key type
    };
    let id = key_next_id();
    let mut entries = KEY_ENTRIES.lock();
    entries.insert(id, entry);
    let ring_real_id = if ringid < 0 {
        drop(entries);
        match ensure_special_keyring(ringid) {
            Ok(rid) => rid,
            Err(e) => return e,
        }
    } else {
        drop(entries);
        ringid
    };
    let mut entries = KEY_ENTRIES.lock();
    if let Some(ring) = entries.get_mut(&ring_real_id) {
        if !ring.children.contains(&id) {
            ring.children.push(id);
        }
    }
    id as isize
}

pub(crate) fn sys_keyctl(cmd: i32, arg2: usize, arg3: usize, arg4: usize, _arg5: usize) -> isize {
    const KEYCTL_GET_KEYRING_ID: i32 = 0;
    const KEYCTL_JOIN_SESSION_KEYRING: i32 = 1;
    const KEYCTL_UPDATE: i32 = 2;
    const KEYCTL_REVOKE: i32 = 3;
    const KEYCTL_CHOWN: i32 = 4;
    const KEYCTL_SETPERM: i32 = 5;
    const KEYCTL_DESCRIBE: i32 = 6;
    const KEYCTL_CLEAR: i32 = 7;
    const KEYCTL_LINK: i32 = 8;
    const KEYCTL_UNLINK: i32 = 9;
    const KEYCTL_SEARCH: i32 = 10;
    const KEYCTL_READ: i32 = 11;
    const KEYCTL_INSTANTIATE: i32 = 12;
    const KEYCTL_NEGATE: i32 = 13;
    const KEYCTL_SET_REQKEY_KEYRING: i32 = 14;
    const KEYCTL_SET_TIMEOUT: i32 = 15;
    const KEYCTL_ASSUME_AUTHORITY: i32 = 16;
    const KEYCTL_GET_SECURITY: i32 = 17;
    const KEYCTL_SESSION_TO_PARENT: i32 = 18;
    const KEYCTL_REJECT: i32 = 19;
    const KEYCTL_INSTANTIATE_IOV: i32 = 20;
    const KEYCTL_INVALIDATE: i32 = 21;
    const KEYCTL_GET_PERSISTENT: i32 = 22;
    const KEYCTL_RESTRICT_KEYRING: i32 = 29;

    match cmd {
        KEYCTL_GET_KEYRING_ID => {
            let id = arg2 as isize as i32;
            let create = arg3 != 0;
            match resolve_key_id(id, create) {
                Ok(kid) => kid as isize,
                Err(e) => e,
            }
        }
        KEYCTL_JOIN_SESSION_KEYRING => {
            // CVE-2016-9604: a named session keyring whose name begins with '.'
            // is reserved for the kernel (e.g. ".builtin_trusted_keys") and must
            // not be joinable from userspace — return EPERM (keyctl08).
            let name_ptr = arg2 as *const u8;
            if !name_ptr.is_null() {
                match read_optional_cstring(name_ptr) {
                    Ok(Some(name)) if name.starts_with('.') => return -1, // EPERM
                    Err(_) => return -14, // EFAULT
                    _ => {}
                }
            }
            match ensure_special_keyring(-3) {
                Ok(key) => key as isize,
                Err(errno) => errno,
            }
        }
        KEYCTL_UPDATE => {
            let id = arg2 as isize as i32;
            let payload_ptr = arg3 as *const u8;
            let plen = arg4;
            match resolve_key_id(id, false) {
                Ok(kid) => {
                    let mut entries = KEY_ENTRIES.lock();
                    if let Some(entry) = entries.get_mut(&kid) {
                        if entry.is_keyring { return -95; }
                        if plen > 0 && !payload_ptr.is_null() {
                            match read_user_buffer(payload_ptr, plen) {
                                Ok(data) => entry.payload = data,
                                Err(_) => return -14,
                            }
                        } else {
                            entry.payload.clear();
                        }
                        0
                    } else { -126 }
                }
                Err(e) => e,
            }
        }
        KEYCTL_REVOKE => {
            let id = arg2 as isize as i32;
            match resolve_key_id(id, false) {
                Ok(kid) => {
                    let mut entries = KEY_ENTRIES.lock();
                    if let Some(entry) = entries.get_mut(&kid) {
                        if entry.revoked { return -128; } // EKEYREVOKED
                        entry.revoked = true;
                        0
                    } else { -126 }
                }
                Err(e) => e,
            }
        }
        KEYCTL_CHOWN => {
            let id = arg2 as isize as i32;
            let uid = arg3 as u32;
            let gid = arg4 as u32;
            match resolve_key_id(id, false) {
                Ok(kid) => {
                    let mut entries = KEY_ENTRIES.lock();
                    if let Some(entry) = entries.get_mut(&kid) {
                        if uid != u32::MAX { entry.owner = uid; }
                        if gid != u32::MAX { entry.gid = gid; }
                        0
                    } else { -126 }
                }
                Err(e) => e,
            }
        }
        KEYCTL_SETPERM => {
            let id = arg2 as isize as i32;
            let perm = arg3 as u32;
            match resolve_key_id(id, false) {
                Ok(kid) => {
                    let mut entries = KEY_ENTRIES.lock();
                    if let Some(entry) = entries.get_mut(&kid) {
                        entry.perm = perm;
                        0
                    } else { -126 }
                }
                Err(e) => e,
            }
        }
        KEYCTL_DESCRIBE => {
            let id = arg2 as isize as i32;
            let buf = arg3 as *mut u8;
            let buflen = arg4;
            match resolve_key_id(id, false) {
                Ok(kid) => {
                    let entries = KEY_ENTRIES.lock();
                    if let Some(entry) = entries.get(&kid) {
                        let desc = format!("{};{};{};{:08x};{}",
                            entry.key_type, entry.owner, entry.gid,
                            entry.perm, entry.description);
                        let desc_bytes = desc.as_bytes();
                        let total = desc_bytes.len() + 1;
                        if buflen == 0 { return total as isize; }
                        let copy_len = core::cmp::min(desc_bytes.len(), buflen.saturating_sub(1));
                        let mut out = desc_bytes[..copy_len].to_vec();
                        out.push(0);
                        drop(entries);
                        if write_user_buffer(buf, &out).is_err() { return -14; }
                        total as isize
                    } else { -126 }
                }
                Err(e) => e,
            }
        }
        KEYCTL_CLEAR => {
            let id = arg2 as isize as i32;
            match resolve_key_id(id, false) {
                Ok(kid) => {
                    let mut entries = KEY_ENTRIES.lock();
                    if let Some(entry) = entries.get_mut(&kid) {
                        if !entry.is_keyring { return -95; }
                        entry.children.clear();
                        0
                    } else { -126 }
                }
                Err(e) => e,
            }
        }
        KEYCTL_LINK => {
            let key_id = arg2 as isize as i32;
            let ring_id = arg3 as isize as i32;
            let kid = match resolve_key_id(key_id, false) { Ok(k) => k, Err(e) => return e };
            let rid = match resolve_key_id(ring_id, false) { Ok(k) => k, Err(e) => return e };
            let mut entries = KEY_ENTRIES.lock();
            let is_ring = entries.get(&rid).map(|e| e.is_keyring).unwrap_or(false);
            if !is_ring { return -95; }
            if link_would_cycle(kid, rid, &entries) { return -35; } // EDEADLK
            if let Some(ring) = entries.get_mut(&rid) {
                if !ring.children.contains(&kid) {
                    ring.children.push(kid);
                }
            }
            0
        }
        KEYCTL_UNLINK => {
            let key_id = arg2 as isize as i32;
            let ring_id = arg3 as isize as i32;
            let kid = match resolve_key_id(key_id, false) { Ok(k) => k, Err(e) => return e };
            let rid = match resolve_key_id(ring_id, false) { Ok(k) => k, Err(e) => return e };
            let mut entries = KEY_ENTRIES.lock();
            if let Some(ring) = entries.get_mut(&rid) {
                ring.children.retain(|&c| c != kid);
            }
            0
        }
        KEYCTL_SEARCH => {
            let ring_id = arg2 as isize as i32;
            let type_ptr = arg3 as *const u8;
            let desc_ptr = arg4 as *const u8;
            let _dest = _arg5 as isize as i32;
            let rid = match resolve_key_id(ring_id, false) { Ok(k) => k, Err(e) => return e };
            let stype = match read_path(type_ptr) { Ok(s) => s, Err(e) => return e };
            let sdesc = match read_path(desc_ptr) { Ok(s) => s, Err(e) => return e };
            let entries = KEY_ENTRIES.lock();
            if let Some(ring) = entries.get(&rid) {
                for &child_id in &ring.children {
                    if let Some(child) = entries.get(&child_id) {
                        if child.key_type == stype && child.description == sdesc {
                            if let Err(e) = child.validate() { return e; }
                            return child_id as isize;
                        }
                    }
                }
            }
            -126 // ENOKEY
        }
        KEYCTL_READ => {
            let id = arg2 as isize as i32;
            let buf = arg3 as *mut u8;
            let buflen = arg4;
            match resolve_key_id(id, false) {
                Ok(kid) => {
                    let entries = KEY_ENTRIES.lock();
                    if let Some(entry) = entries.get(&kid) {
                        if entry.is_keyring {
                            let ids: Vec<u8> = entry.children.iter()
                                .flat_map(|&id| (id as i32).to_ne_bytes().to_vec())
                                .collect();
                            let total = ids.len();
                            if buflen == 0 { return total as isize; }
                            let copy_len = core::cmp::min(total, buflen);
                            drop(entries);
                            if copy_len > 0 {
                                if write_user_buffer(buf, &ids[..copy_len]).is_err() { return -14; }
                            }
                            total as isize
                        } else if entry.negative {
                            // KEYCTL_READ on a negatively instantiated key must
                            // fail with ENOKEY (37863c43b2c6 / keyctl07).
                            -126
                        } else {
                            let total = entry.payload.len();
                            if buflen == 0 { return total as isize; }
                            let copy_len = core::cmp::min(total, buflen);
                            let data = entry.payload[..copy_len].to_vec();
                            drop(entries);
                            if copy_len > 0 {
                                if write_user_buffer(buf, &data).is_err() { return -14; }
                            }
                            total as isize
                        }
                    } else { -126 }
                }
                Err(e) => e,
            }
        }
        KEYCTL_INSTANTIATE | KEYCTL_NEGATE | KEYCTL_REJECT | KEYCTL_INSTANTIATE_IOV => {
            0
        }
        KEYCTL_SET_REQKEY_KEYRING => {
            let val = arg2 as i32;
            if val < -1 || val > 6 { return -22; }
            let pid = current_pid();
            REQKEY_KEYRING.lock().insert(pid, val);
            val as isize
        }
        KEYCTL_SET_TIMEOUT => {
            let id = arg2 as isize as i32;
            let timeout = arg3 as u64;
            match resolve_key_id(id, false) {
                Ok(kid) => {
                    let mut entries = KEY_ENTRIES.lock();
                    if let Some(entry) = entries.get_mut(&kid) {
                        if entry.revoked { return -128; } // EKEYREVOKED
                        entry.timeout = timeout;
                        entry.expiry = if timeout == 0 { 0 } else { key_now_sec() + timeout };
                        0
                    } else { -126 }
                }
                Err(e) => e,
            }
        }
        KEYCTL_ASSUME_AUTHORITY => { 0 }
        KEYCTL_GET_SECURITY => {
            let id = arg2 as isize as i32;
            let buf = arg3 as *mut u8;
            let buflen = arg4;
            match resolve_key_id(id, false) {
                Ok(_kid) => {
                    let label = b"unconfined\0";
                    let total = label.len();
                    if buflen == 0 { return total as isize; }
                    let copy_len = core::cmp::min(total, buflen);
                    if write_user_buffer(buf, &label[..copy_len]).is_err() { return -14; }
                    total as isize
                }
                Err(e) => e,
            }
        }
        KEYCTL_SESSION_TO_PARENT => { 0 }
        KEYCTL_INVALIDATE => {
            let id = arg2 as isize as i32;
            match resolve_key_id(id, false) {
                Ok(kid) => {
                    KEY_ENTRIES.lock().remove(&kid);
                    0
                }
                Err(e) => e,
            }
        }
        KEYCTL_GET_PERSISTENT => {
            let uid = arg2 as u32;
            let ring_id = arg3 as isize as i32;
            let _rid = match resolve_key_id(ring_id, true) { Ok(k) => k, Err(e) => return e };
            let id = key_next_id();
            let mut entries = KEY_ENTRIES.lock();
            entries.insert(id, KeyEntry::new_keyring(uid, "_persistent"));
            id as isize
        }
        KEYCTL_RESTRICT_KEYRING => {
            let id = arg2 as isize as i32;
            match resolve_key_id(id, false) {
                Ok(_kid) => 0,
                Err(e) => e,
            }
        }
        _ => -95, // EOPNOTSUPP
    }
}

/// request_key(type, description, callout_info, dest_keyring) → key serial
pub(crate) fn sys_request_key(
    type_: *const u8, desc: *const u8, _callout: *const u8, keyring: isize,
) -> isize {
    let key_type = match read_path(type_) { Ok(s) => s, Err(e) => return e };
    let description = match read_path(desc) { Ok(s) => s, Err(e) => return e };

    if description.starts_with('.') {
        let uid = current_uid();
        if uid != 0 { return -13; } // EACCES
    }

    let entries = KEY_ENTRIES.lock();
    let special = SPECIAL_KEYRINGS.lock();
    let pid = current_pid();
    let uid = current_uid() as usize;

    let ring_slots: &[(i32, usize)] = &[
        (-1, pid), (-2, pid), (-3, pid),
        (-4, uid), (-5, uid),
    ];

    for slot in ring_slots {
        if let Some(&ring_id) = special.get(slot) {
            let reachable = collect_keyring_keys(ring_id, &entries);
            for kid in reachable {
                if let Some(entry) = entries.get(&kid) {
                    if entry.key_type == key_type && entry.description == description {
                        let is_neg = entry.negative;
                        let valid = entry.validate();
                        drop(special);
                        drop(entries);
                        // A previously cached *negative* key short-circuits to
                        // ENOKEY without re-running construction (keyctl07).
                        if is_neg { return -126; }
                        // key_validate: revoked/expired keys are found but
                        // yield EKEYREVOKED / EKEYEXPIRED (request_key02).
                        if let Err(e) = valid { return e; }
                        if keyring != 0 {
                            let ring_id = keyring as i32;
                            if let Ok(rid) = resolve_key_id(ring_id, true) {
                                let mut entries = KEY_ENTRIES.lock();
                                let can_link = entries
                                    .get(&rid)
                                    .map(|r| r.is_keyring && !r.children.contains(&kid))
                                    .unwrap_or(false)
                                    && !link_would_cycle(kid, rid, &entries);
                                if can_link {
                                    if let Some(ring) = entries.get_mut(&rid) {
                                        ring.children.push(kid);
                                    }
                                }
                            }
                        }
                        return kid as isize;
                    }
                }
            }
        }
    }
    drop(special);
    drop(entries);

    // Not found. Without callout_info request_key does not construct anything.
    if _callout.is_null() {
        return -126; // ENOKEY
    }

    // request_key constructs a *negative* key and links it into the destination
    // keyring: the explicit `keyring` arg if given, else the per-process
    // default request-key keyring (KEYCTL_SET_REQKEY_KEYRING). Linking needs
    // WRITE on that keyring — request_key04 strips it (only VIEW|READ|SEARCH)
    // so construction fails with EACCES instead of caching a negative key.
    let dest_spec: i32 = if keyring != 0 {
        keyring as i32
    } else {
        match REQKEY_KEYRING.lock().get(&pid).copied().unwrap_or(0) {
            1 => -1, // thread
            2 => -2, // process
            4 => -4, // user
            5 => -5, // user-session
            _ => -3, // session (also the default)
        }
    };
    let dest_id = match resolve_key_id(dest_spec, true) {
        Ok(i) => i,
        Err(e) => return e,
    };
    {
        let entries = KEY_ENTRIES.lock();
        if let Some(ring) = entries.get(&dest_id) {
            // WRITE bit (0x04) in any of the POS/USR/GRP/OTH permission bytes.
            if ring.perm & 0x0404_0404 == 0 {
                return -13; // EACCES
            }
            // Idempotent: a matching cached negative key already exists.
            for &cid in &ring.children {
                if let Some(c) = entries.get(&cid) {
                    if c.negative && c.key_type == key_type && c.description == description {
                        return -126; // ENOKEY
                    }
                }
            }
        }
    }
    let kid = key_next_id();
    {
        let mut entries = KEY_ENTRIES.lock();
        entries.insert(kid, KeyEntry::new_negative(current_uid(), &key_type, &description));
        if let Some(ring) = entries.get_mut(&dest_id) {
            if ring.is_keyring && !ring.children.contains(&kid) {
                ring.children.push(kid);
            }
        }
    }
    -126 // ENOKEY (negative key cached)
}
