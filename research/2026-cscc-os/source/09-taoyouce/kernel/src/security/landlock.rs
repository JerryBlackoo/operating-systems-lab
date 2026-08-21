//! Landlock — unprivileged filesystem access-control sandboxing.
//!
//! Implements Linux 5.13+ Landlock LSM (ABI v1-v4):
//! - landlock_create_ruleset(2): create a ruleset file descriptor
//! - landlock_add_rule(2): add path-based access rules to a ruleset
//! - landlock_restrict_self(2): enforce the ruleset on the calling thread
//!
//! Reference: https://github.com/torvalds/linux/tree/master/security/landlock
//!            https://docs.kernel.org/userspace-api/landlock.html

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use crate::sync::SpinLock;

// ============================================================================
// Landlock ABI constants — per include/uapi/linux/landlock.h
// ============================================================================

/// Current supported ABI version
pub const LANDLOCK_ABI_VERSION: u32 = 4;

// Access rights for files (LANDLOCK_ACCESS_FS_*)
pub const LANDLOCK_ACCESS_FS_EXECUTE: u64          = 1 << 0;
pub const LANDLOCK_ACCESS_FS_WRITE_FILE: u64       = 1 << 1;
pub const LANDLOCK_ACCESS_FS_READ_FILE: u64        = 1 << 2;
pub const LANDLOCK_ACCESS_FS_READ_DIR: u64         = 1 << 3;
pub const LANDLOCK_ACCESS_FS_REMOVE_DIR: u64       = 1 << 4;
pub const LANDLOCK_ACCESS_FS_REMOVE_FILE: u64      = 1 << 5;
pub const LANDLOCK_ACCESS_FS_MAKE_CHAR: u64        = 1 << 6;
pub const LANDLOCK_ACCESS_FS_MAKE_DIR: u64         = 1 << 7;
pub const LANDLOCK_ACCESS_FS_MAKE_REG: u64         = 1 << 8;
pub const LANDLOCK_ACCESS_FS_MAKE_SOCK: u64        = 1 << 9;
pub const LANDLOCK_ACCESS_FS_MAKE_FIFO: u64        = 1 << 10;
pub const LANDLOCK_ACCESS_FS_MAKE_BLOCK: u64       = 1 << 11;
pub const LANDLOCK_ACCESS_FS_MAKE_SYM: u64         = 1 << 12;
pub const LANDLOCK_ACCESS_FS_REFER: u64            = 1 << 13;  // ABI v2
pub const LANDLOCK_ACCESS_FS_TRUNCATE: u64         = 1 << 14;  // ABI v3
pub const LANDLOCK_ACCESS_FS_IOCTL_DEV: u64        = 1 << 15;  // ABI v5

/// All valid FS access bits for ABI v4
const ALL_FS_ACCESS_V4: u64 = (1 << 15) - 1; // bits 0..14

// Access rights for network (ABI v4)
pub const LANDLOCK_ACCESS_NET_BIND_TCP: u64        = 1 << 0;
pub const LANDLOCK_ACCESS_NET_CONNECT_TCP: u64     = 1 << 1;

const ALL_NET_ACCESS_V4: u64 = (1 << 2) - 1; // bits 0..1

// Rule types
pub const LANDLOCK_RULE_PATH_BENEATH: u32 = 1;
pub const LANDLOCK_RULE_NET_PORT: u32     = 2;  // ABI v4

// Flags for landlock_create_ruleset
pub const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;

// ============================================================================
// Ruleset data structures
// ============================================================================

/// A Landlock ruleset containing access control rules.
#[derive(Clone)]
pub struct LandlockRuleset {
    /// Bitmask of filesystem access rights handled by this ruleset
    pub handled_access_fs: u64,
    /// Bitmask of network access rights handled by this ruleset (ABI v4)
    pub handled_access_net: u64,
    /// Filesystem path-beneath rules: (path_prefix, allowed_access_bits)
    pub fs_rules: Vec<FsRule>,
    /// Network port rules: (port, allowed_access_bits)
    pub net_rules: Vec<NetRule>,
}

/// A filesystem access rule — grants `allowed_access` under `path_prefix`.
#[derive(Clone)]
pub struct FsRule {
    pub path_prefix: String,
    pub allowed_access: u64,
}

/// A network port rule — grants `allowed_access` for `port`.
#[derive(Clone)]
pub struct NetRule {
    pub port: u64,
    pub allowed_access: u64,
}

// ============================================================================
// Global ruleset table — indexed by ruleset FD
// ============================================================================

static RULESET_TABLE: SpinLock<BTreeMap<i32, LandlockRuleset>> =
    SpinLock::new(BTreeMap::new());

static NEXT_RULESET_FD: AtomicI32 = AtomicI32::new(1000);

/// 全局门控：是否有**任何**线程曾经 landlock_restrict_self 进入受限域。
/// 绝大多数进程从不使用 landlock，但 `check_access`/`check_net_access` 会在
/// 每个 open / 路径类 syscall 上被调用——若无此门控，每次都要抢 PROCESS_TABLE
/// 全局锁去查 `landlock_domain.is_restricted()`（结果几乎总是 false）。此标志
/// 仅在 restrict_self 时 false→true 置位、永不清零（保守），故为 false 时
/// 任何进程都必然 unrestricted，可 lock-free 直接放行（§1.13①，省每 open 一把锁）。
static LANDLOCK_ACTIVE: AtomicBool = AtomicBool::new(false);

fn alloc_ruleset_fd() -> i32 {
    NEXT_RULESET_FD.fetch_add(1, Ordering::Relaxed)
}

// ============================================================================
// Per-process Landlock domain
// ============================================================================

/// A Landlock domain is the stack of all rulesets applied to a thread.
/// Once restrict_self is called, the thread enters a domain with the given
/// rulesets merged (intersection of all layers).
#[derive(Clone, Default)]
pub struct LandlockDomain {
    /// Stack of rulesets (most recently applied last)
    pub layers: Vec<LandlockRuleset>,
}

impl LandlockDomain {
    pub fn new() -> Self {
        Self { layers: Vec::new() }
    }

    pub fn is_restricted(&self) -> bool {
        !self.layers.is_empty()
    }

    /// Check if a filesystem access is allowed.
    /// Per Linux: for each layer, if the layer handles the access type,
    /// at least one rule in that layer must grant it for the given path.
    pub fn check_fs_access(&self, path: &str, access: u64) -> bool {
        for layer in &self.layers {
            // Only check access types that this layer handles
            let relevant = access & layer.handled_access_fs;
            if relevant == 0 {
                continue; // this layer doesn't care about these access types
            }
            // At least one rule must grant the relevant access for this path
            let mut granted = 0u64;
            for rule in &layer.fs_rules {
                if path_is_beneath(path, &rule.path_prefix) {
                    granted |= rule.allowed_access & relevant;
                }
            }
            if granted & relevant != relevant {
                return false; // layer denies some of the requested access
            }
        }
        true
    }

    /// Check if a network access is allowed.
    pub fn check_net_access(&self, port: u16, access: u64) -> bool {
        for layer in &self.layers {
            let relevant = access & layer.handled_access_net;
            if relevant == 0 {
                continue;
            }
            let mut granted = 0u64;
            for rule in &layer.net_rules {
                if rule.port == port as u64 {
                    granted |= rule.allowed_access & relevant;
                }
            }
            if granted & relevant != relevant {
                return false;
            }
        }
        true
    }
}

/// Check if `path` is beneath (or equal to) `prefix`.
/// E.g. "/home/user/file" is beneath "/home/user" and "/home" and "/".
fn path_is_beneath(path: &str, prefix: &str) -> bool {
    if prefix == "/" {
        return true;
    }
    if path == prefix {
        return true;
    }
    // path must start with prefix followed by '/'
    path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/')
}

// ============================================================================
// Syscall implementations
// ============================================================================

/// landlock_create_ruleset(2)
///
/// If flags contains LANDLOCK_CREATE_RULESET_VERSION, return ABI version.
/// Otherwise create a new ruleset from the given attr and return its FD.
pub fn sys_landlock_create_ruleset(
    attr_ptr: *const u8,
    size: usize,
    flags: u32,
) -> isize {
    // Query ABI version
    if flags & LANDLOCK_CREATE_RULESET_VERSION != 0 {
        if !attr_ptr.is_null() || size != 0 {
            return -22; // EINVAL
        }
        // Unknown flags besides VERSION
        if flags & !LANDLOCK_CREATE_RULESET_VERSION != 0 {
            return -22;
        }
        return LANDLOCK_ABI_VERSION as isize;
    }

    // Flags must be 0 for creating a ruleset
    if flags != 0 {
        return -22; // EINVAL: unknown flags
    }

    // struct landlock_ruleset_attr {
    //     u64 handled_access_fs;     // offset 0
    //     u64 handled_access_net;    // offset 8 (ABI v4)
    // }
    if attr_ptr.is_null() {
        return -14; // EFAULT
    }
    if size < 8 {
        return -22; // EINVAL: attr too small
    }

    let handled_access_fs = unsafe {
        let ptr = attr_ptr as *const u64;
        core::ptr::read_unaligned(ptr)
    };

    let handled_access_net = if size >= 16 {
        unsafe {
            let ptr = (attr_ptr as *const u64).add(1);
            core::ptr::read_unaligned(ptr)
        }
    } else {
        0
    };

    // Validate: no unknown bits
    if handled_access_fs & !ALL_FS_ACCESS_V4 != 0 {
        return -22; // EINVAL
    }
    if handled_access_net & !ALL_NET_ACCESS_V4 != 0 {
        return -22; // EINVAL
    }
    // At least one access right must be handled
    if handled_access_fs == 0 && handled_access_net == 0 {
        return -22; // EINVAL
    }

    let ruleset = LandlockRuleset {
        handled_access_fs,
        handled_access_net,
        fs_rules: Vec::new(),
        net_rules: Vec::new(),
    };

    let fd = alloc_ruleset_fd();
    RULESET_TABLE.lock().insert(fd, ruleset);

    // Also install as a real FD in the calling process's fd table
    install_landlock_fd(fd);

    fd as isize
}

/// landlock_add_rule(2)
///
/// Add a rule to an existing ruleset FD.
pub fn sys_landlock_add_rule(
    ruleset_fd: i32,
    rule_type: u32,
    rule_attr_ptr: *const u8,
    flags: u32,
) -> isize {
    if flags != 0 {
        return -22; // EINVAL: no flags defined yet
    }
    if rule_attr_ptr.is_null() {
        return -14; // EFAULT
    }

    let mut table = RULESET_TABLE.lock();
    let ruleset = match table.get_mut(&ruleset_fd) {
        Some(r) => r,
        None => return -9, // EBADF
    };

    match rule_type {
        LANDLOCK_RULE_PATH_BENEATH => {
            // struct landlock_path_beneath_attr {
            //     u64 allowed_access;  // offset 0
            //     i32 parent_fd;       // offset 8
            // }
            let allowed_access = unsafe {
                core::ptr::read_unaligned(rule_attr_ptr as *const u64)
            };
            let parent_fd = unsafe {
                core::ptr::read_unaligned((rule_attr_ptr as *const u8).add(8) as *const i32)
            };

            // Validate: allowed_access must be subset of handled_access_fs
            if allowed_access & !ruleset.handled_access_fs != 0 {
                return -22; // EINVAL
            }
            if allowed_access == 0 {
                return -22; // EINVAL: must grant at least one access
            }

            // Resolve parent_fd to a path
            let path = resolve_fd_to_path(parent_fd);
            if path.is_empty() {
                return -9; // EBADF
            }

            ruleset.fs_rules.push(FsRule {
                path_prefix: path,
                allowed_access,
            });
        }
        LANDLOCK_RULE_NET_PORT => {
            // struct landlock_net_port_attr {
            //     u64 allowed_access;  // offset 0
            //     u64 port;            // offset 8
            // }
            let allowed_access = unsafe {
                core::ptr::read_unaligned(rule_attr_ptr as *const u64)
            };
            let port = unsafe {
                core::ptr::read_unaligned((rule_attr_ptr as *const u8).add(8) as *const u64)
            };

            if allowed_access & !ruleset.handled_access_net != 0 {
                return -22;
            }
            if allowed_access == 0 {
                return -22;
            }
            if port > 65535 {
                return -22;
            }

            ruleset.net_rules.push(NetRule {
                port,
                allowed_access,
            });
        }
        _ => return -22, // EINVAL: unknown rule type
    }

    0
}

/// landlock_restrict_self(2)
///
/// Enforce the given ruleset on the calling thread. The ruleset FD is consumed.
/// Thread must have no_new_privs set (prctl PR_SET_NO_NEW_PRIVS).
/// Per Linux: EPERM if no_new_privs is not set.
pub fn sys_landlock_restrict_self(ruleset_fd: i32, flags: u32) -> isize {
    if flags != 0 {
        return -22; // EINVAL
    }

    // Check no_new_privs (Linux requirement for unprivileged sandboxing)
    {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -22 };
        let cur_pid = match t.current { Some(p) => p, None => return -22 };
        let proc = match t.get(cur_pid) { Some(p) => p, None => return -22 };
        if !proc.no_new_privs {
            return -1; // EPERM: must set no_new_privs first
        }
    }

    // Take ruleset from table
    let ruleset = {
        let mut table = RULESET_TABLE.lock();
        match table.remove(&ruleset_fd) {
            Some(r) => r,
            None => return -9, // EBADF
        }
    };

    // Apply to current process's landlock domain
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() { Some(t) => t, None => return -22 };
    let cur_pid = match t.current { Some(p) => p, None => return -22 };
    let proc = match t.get_mut(cur_pid) { Some(p) => p, None => return -22 };

    proc.landlock_domain.layers.push(ruleset);
    LANDLOCK_ACTIVE.store(true, Ordering::Relaxed);
    0
}

// ============================================================================
// Access check hook — called from VFS operations
// ============================================================================

/// Check if the current process is allowed to perform `access` on `path`.
/// Returns true if allowed, false if denied.
/// If the process has no landlock domain, always returns true.
#[inline]
pub fn check_access(path: &str, access: u64) -> bool {
    // 无任何受限域时 lock-free 放行（见 LANDLOCK_ACTIVE）。
    if !LANDLOCK_ACTIVE.load(Ordering::Relaxed) {
        return true;
    }
    check_access_slow(path, access)
}

#[cold]
fn check_access_slow(path: &str, access: u64) -> bool {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_ref() { Some(t) => t, None => return true };
    let cur_pid = match t.current { Some(p) => p, None => return true };
    let proc = match t.get(cur_pid) { Some(p) => p, None => return true };

    if !proc.landlock_domain.is_restricted() {
        return true;
    }

    proc.landlock_domain.check_fs_access(path, access)
}

/// Check network access for the current process.
pub fn check_net_access(port: u16, access: u64) -> bool {
    if !LANDLOCK_ACTIVE.load(Ordering::Relaxed) {
        return true;
    }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_ref() { Some(t) => t, None => return true };
    let cur_pid = match t.current { Some(p) => p, None => return true };
    let proc = match t.get(cur_pid) { Some(p) => p, None => return true };

    if !proc.landlock_domain.is_restricted() {
        return true;
    }

    proc.landlock_domain.check_net_access(port, access)
}

// ============================================================================
// Helpers
// ============================================================================

/// Resolve an FD to its absolute path (for landlock_add_rule parent_fd).
fn resolve_fd_to_path(fd: i32) -> String {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_ref() { Some(t) => t, None => return String::new() };
    let cur_pid = match t.current { Some(p) => p, None => return String::new() };
    let proc = match t.get(cur_pid) { Some(p) => p, None => return String::new() };

    if let Some(Some(file_obj)) = proc.fd_table.get(fd as usize) {
        if let Some(ref path) = file_obj.path {
            return path.clone();
        }
    }
    String::new()
}

/// Install a landlock ruleset FD into the current process's fd table.
/// Uses /dev/null inode as backing (anonymous inode pattern).
fn install_landlock_fd(landlock_id: i32) {
    use alloc::sync::Arc;

    // Get /dev/null inode as the backing file for the anonymous fd
    let inode = match crate::fs::devfs::lookup_path("dev/null") {
        Ok(i) => i,
        Err(_) => return,
    };

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() { Some(t) => t, None => return };
    let cur_pid = match t.current { Some(p) => p, None => return };
    let proc = match t.get_mut(cur_pid) { Some(p) => p, None => return };

    // Create a FileDescriptor using the constructor
    let fd_obj = Arc::new(crate::fs::FileDescriptor::new_with_path(
        inode,
        0o2, // O_RDWR
        Some(alloc::format!("anon_inode:[landlock-ruleset]")),
    ));
    fd_obj.inc_fd_ref();

    // Insert at the landlock_id position (expand table if needed)
    let idx = landlock_id as usize;
    while proc.fd_table.len() <= idx {
        proc.fd_table.push(None);
    }
    proc.fd_table[idx] = Some(fd_obj);
}
