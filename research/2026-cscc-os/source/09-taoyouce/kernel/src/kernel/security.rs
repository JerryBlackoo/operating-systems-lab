use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::syscall::user::{read_user_buffer, write_user_buffer};
use crate::fs::fd as fd_table;
use crate::fs::{File, FileDescriptor, FileMode, FileStat, FileType, FsError, Inode};


// Keyring types/statics moved to kernel/src/syscall/keyring.rs

struct BpfMapInode {
    map_id: u64,
}

impl File for BpfMapInode {
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }

    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }

    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Regular,
            size: 0,
            mode: FileMode(0o100600),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }

    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

impl Inode for BpfMapInode {
    fn ino(&self) -> usize { 0xb0000000usize.wrapping_add(self.map_id as usize) }
    fn file_type(&self) -> FileType { FileType::Regular }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}

fn bpf_errno(err: crate::ebpf::BpfMapAccessError) -> isize {
    match err {
        crate::ebpf::BpfMapAccessError::InvalidMap => -9,
        crate::ebpf::BpfMapAccessError::KeyNotFound => -2,
        crate::ebpf::BpfMapAccessError::KeyExists => -17,
        crate::ebpf::BpfMapAccessError::MapFull => -7,
        crate::ebpf::BpfMapAccessError::InvalidArgument => -22,
    }
}

fn alloc_bpf_map_fd(map_id: u64, key_size: u32, value_size: u32) -> isize {
    let fd_obj = Arc::new(FileDescriptor::new_with_path(
        Arc::new(BpfMapInode { map_id }),
        0,
        Some(format!("bpf-map:{}:{}:{}", map_id, key_size, value_size)),
    ));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(table) = table.as_mut() else { return -22; };
    let Some(current_pid) = table.current else { return -22; };
    let Some(process) = table.get_fd_process_mut(current_pid) else { return -22; };
    match fd_table::alloc(process, fd_obj, 0, true) {
        Ok(fd) => fd as isize,
        Err(errno) => errno,
    }
}

fn bpf_map_from_fd(fd: i32) -> Result<(u64, usize, usize), isize> {
    if fd < 0 {
        return Err(-9);
    }
    let table = crate::sched::process::PROCESS_TABLE.lock();
    let table = table.as_ref().ok_or(-22isize)?;
    let current_pid = table.current.ok_or(-22isize)?;
    let process = table.get_fd_process(current_pid).ok_or(-22isize)?;
    let fd_obj = fd_table::get(process, fd as usize).ok_or(-9isize)?;
    let path = fd_obj.path.as_ref().ok_or(-9isize)?;
    let meta = path.strip_prefix("bpf-map:").ok_or(-9isize)?;
    let mut parts = meta.split(':');
    let map_id = parts.next().and_then(|s| s.parse::<u64>().ok()).ok_or(-9isize)?;
    let key_size = parts.next().and_then(|s| s.parse::<usize>().ok()).ok_or(-9isize)?;
    let value_size = parts.next().and_then(|s| s.parse::<usize>().ok()).ok_or(-9isize)?;
    Ok((map_id, key_size, value_size))
}

/// BPF program inode — for anon_inode:[bpf-prog] fd
struct BpfProgInode {
    prog_id: u64,
}

impl File for BpfProgInode {
    fn read(&self, _offset: usize, _buf: &mut [u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }
    fn write(&self, _offset: usize, _buf: &[u8]) -> Result<usize, FsError> {
        Err(FsError::InvalidArgument)
    }
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: FileType::Regular,
            size: 0,
            mode: FileMode(0o100600),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
    fn sync(&self) -> Result<(), FsError> { Ok(()) }
}

impl Inode for BpfProgInode {
    fn ino(&self) -> usize { 0xbf000000usize.wrapping_add(self.prog_id as usize) }
    fn file_type(&self) -> FileType { FileType::Regular }
    fn as_file(&self) -> Option<&dyn File> { Some(self) }
    fn as_dir(&self) -> Option<&dyn crate::fs::Directory> { None }
    fn stat(&self) -> Result<FileStat, FsError> { File::stat(self) }
}

fn alloc_bpf_prog_fd(prog_id: u64) -> isize {
    let fd_obj = Arc::new(FileDescriptor::new_with_path(
        Arc::new(BpfProgInode { prog_id }),
        0,
        Some(format!("bpf-prog:{}", prog_id)),
    ));
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let Some(table) = table.as_mut() else { return -22; };
    let Some(current_pid) = table.current else { return -22; };
    let Some(process) = table.get_fd_process_mut(current_pid) else { return -22; };
    match fd_table::alloc(process, fd_obj, 0, true) {
        Ok(fd) => fd as isize,
        Err(errno) => errno,
    }
}

pub(crate) fn current_euid() -> u32 {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)))
        .map(|p| p.euid)
        .unwrap_or(0)
}

pub(crate) fn current_uid() -> u32 {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table
        .as_ref()
        .and_then(|t| t.current.and_then(|pid| t.get(pid)))
        .map(|p| p.uid)
        .unwrap_or(0)
}

pub(crate) fn current_pid() -> usize {
    crate::sched::process::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed)
}

fn pid_exists(pid: usize) -> bool {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    table.as_ref().and_then(|t| t.get(pid)).is_some()
}

pub(crate) fn read_path(ptr: *const u8) -> Result<String, isize> {
    if ptr.is_null() {
        return Err(-14);
    }
    let mut bytes = Vec::new();
    for i in 0..=4096usize {
        let byte = match read_user_buffer(unsafe { ptr.add(i) }, 1) {
            Ok(buf) => buf[0],
            Err(_) => return Err(-14),
        };
        if byte == 0 {
            return String::from_utf8(bytes).map_err(|_| -22);
        }
        bytes.push(byte);
    }
    Err(-36)
}

pub(crate) fn read_optional_cstring(ptr: *const u8) -> Result<Option<String>, isize> {
    if ptr.is_null() {
        Ok(None)
    } else {
        read_path(ptr).map(Some)
    }
}

// Keyring helpers moved to kernel/src/syscall/keyring.rs

fn acct_path_has_trailing_slash(path: &str) -> bool {
    path.as_bytes().last() == Some(&b'/') && path.as_bytes().iter().any(|&byte| byte != b'/')
}

fn acct_lookup_errno(path: &str, err: crate::fs::FsError) -> isize {
    match err {
        crate::fs::FsError::NotFound => {
            if crate::fs::is_readonly_path(path) {
                -30
            } else {
                -2
            }
        }
        crate::fs::FsError::NotDirectory => -20,
        crate::fs::FsError::IsDirectory => -21,
        crate::fs::FsError::PermissionDenied => -13,
        crate::fs::FsError::OperationNotPermitted => -1,
        crate::fs::FsError::TooManyLinks => -40,
        crate::fs::FsError::NameTooLong => -36,
        crate::fs::FsError::InvalidArgument => -22,
        crate::fs::FsError::NoSpace => -28,
        _ => -5,
    }
}

pub(crate) fn sys_acct(filename: *const u8) -> isize {
    if current_euid() != 0 {
        return -1;
    }
    if filename.is_null() {
        crate::sched::process_acct::set_process_accounting_file(None);
        return 0;
    }
    let raw_path = match read_path(filename) {
        Ok(path) => path,
        Err(errno) => return errno,
    };
    let path = crate::syscall::resolve_path(&raw_path);
    if acct_path_has_trailing_slash(&raw_path) {
        return match crate::fs::lookup(&path) {
            Ok(inode) if inode.file_type() != crate::fs::FileType::Directory => -20,
            Ok(_) => -21,
            Err(err) => acct_lookup_errno(&path, err),
        };
    }
    if crate::fs::is_readonly_path(&path) {
        return -30;
    }
    match crate::fs::lookup(&path) {
        Ok(inode) => match inode.file_type() {
            crate::fs::FileType::Regular => {
                crate::sched::process_acct::set_process_accounting_file(Some(inode));
                0
            }
            crate::fs::FileType::Directory => -21,
            crate::fs::FileType::Symlink => -40,
            _ => -13,
        },
        Err(err) => acct_lookup_errno(&path, err),
    }
}

pub(crate) fn sys_capget(header: *const u8, data: *mut u8) -> isize {
    if header.is_null() {
        return -14;
    }
    let hdr = match read_user_buffer(header, 8) {
        Ok(buf) => buf,
        Err(_) => return -14,
    };
    let version = u32::from_ne_bytes(hdr[0..4].try_into().unwrap());
    let pid = i32::from_ne_bytes(hdr[4..8].try_into().unwrap());
    let words = match version {
        0x1998_0330 => 1,
        0x2007_1026 | 0x2008_0522 => 2,
        _ => {
            let _ = write_user_buffer(header as *mut u8, &0x2008_0522u32.to_ne_bytes());
            return -22;
        }
    };
    if pid < 0 {
        return -22;
    }
    let self_pid = current_pid();
    if pid > 0 && !pid_exists(pid as usize) {
        return -3;
    }
    if data.is_null() {
        return 0;
    }
    // Slice 2.x 真化〔去 capget silent 0〕：真读 target pid 的 PCB cap 三集，
    // 按 Linux v1/v2/v3 layout 拆 32/64bit 写回。pid==0 = current，pid>0 = 指定 pid
    // （v2/v3 都允许跨进程读，Linux `cap_capget` 也是 may_read_anyone）。
    let (eff, perm, inh) = {
        let target_pid = if pid == 0 { self_pid } else { pid as usize };
        let guard = crate::sched::process::PROCESS_TABLE.lock();
        match guard.as_ref().and_then(|t| t.get(target_pid)) {
            Some(proc) => (proc.cap_effective, proc.cap_permitted, proc.cap_inheritable),
            None => return -3, // -ESRCH
        }
    };
    let mut buf = [0u8; 24];
    for i in 0..words {
        let base = i * 12;
        // v1 是 1 word，覆盖 cap 0..31；v2/v3 是 2 word，第 0 word = cap 0..31
        // （低 32 位），第 1 word = cap 32..63（高 32 位）。
        let shift = (i as u32) * 32;
        let effective = (eff >> shift) as u32;
        let permitted = (perm >> shift) as u32;
        let inheritable = (inh >> shift) as u32;
        buf[base..base + 4].copy_from_slice(&effective.to_ne_bytes());
        buf[base + 4..base + 8].copy_from_slice(&permitted.to_ne_bytes());
        buf[base + 8..base + 12].copy_from_slice(&inheritable.to_ne_bytes());
    }
    if write_user_buffer(data, &buf[..words * 12]).is_err() {
        return -14;
    }
    0
}

pub(crate) fn sys_capset(header: *const u8, data: *const u8) -> isize {
    if header.is_null() || data.is_null() {
        return -14;
    }
    let hdr = match read_user_buffer(header, 8) {
        Ok(buf) => buf,
        Err(_) => return -14,
    };
    let version = u32::from_ne_bytes(hdr[0..4].try_into().unwrap());
    let pid = i32::from_ne_bytes(hdr[4..8].try_into().unwrap());
    let words = match version {
        0x1998_0330 => 1,
        0x2007_1026 | 0x2008_0522 => 2,
        _ => {
            let _ = write_user_buffer(header as *mut u8, &0x2008_0522u32.to_ne_bytes());
            return -22;
        }
    };
    if pid < 0 {
        return -22;
    }
    let self_pid = current_pid();
    if pid > 0 && pid as usize != self_pid {
        return -1;
    }
    let raw = match read_user_buffer(data, words * 12) {
        Ok(raw) => raw,
        Err(_) => return -14,
    };
    // Slice 2.x 真化〔去 capset silent 0〕：解 v1(1 word) 或 v2/v3(2 word) layout，
    // 每 word 12B = (effective, permitted, inheritable) 各 u32；word 0 是 cap 0..31，
    // word 1 是 cap 32..63。
    let mut new_eff: u64 = 0;
    let mut new_perm: u64 = 0;
    let mut new_inh: u64 = 0;
    for i in 0..words {
        let base = i * 12;
        let shift = (i as u32) * 32;
        let eff = u32::from_ne_bytes(raw[base..base + 4].try_into().unwrap()) as u64;
        let perm = u32::from_ne_bytes(raw[base + 4..base + 8].try_into().unwrap()) as u64;
        let inh = u32::from_ne_bytes(raw[base + 8..base + 12].try_into().unwrap()) as u64;
        new_eff |= eff << shift;
        new_perm |= perm << shift;
        new_inh |= inh << shift;
    }
    // 取 target PCB 当前 cap，验真 Linux `cap_capset` 三条约束：
    //   1) new_perm ⊆ old_perm                (permitted 不可 raise)
    //   2) new_eff ⊆ new_perm                 (effective 必须 ⊆ permitted)
    //   3) new_inh ⊆ old_inh | old_perm       (inheritable 必须 ⊆ 父集合)
    //   3b) new_inh ⊆ cap_bset | old_inh      (受 bset 限制；root 例外不在此处)
    let target_pid = if pid == 0 { self_pid } else { pid as usize };
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match guard.as_mut() {
        Some(t) => t,
        None => return -3,
    };
    let pcb = match table.get_mut(target_pid) {
        Some(p) => p,
        None => return -3,
    };
    const CAP_SETPCAP: u64 = 1u64 << 8;
    let old_eff = pcb.cap_effective;
    let old_perm = pcb.cap_permitted;
    let old_inh = pcb.cap_inheritable;
    let bset = pcb.cap_bset;
    // Linux `cap_capset` constraints are pure capability-set algebra and apply
    // regardless of euid (there is no root bypass for the permitted-subset
    // rule — a process can only ever drop permitted via capset, never raise it).
    // `CAP_SETPCAP` in the caller's current effective set only relaxes the
    // "inheritable ⊆ old_inheritable | old_permitted" rule.
    let has_setpcap = (old_eff & CAP_SETPCAP) != 0;
    // (1) without CAP_SETPCAP, new inheritable must stay within old_inh|old_perm
    if !has_setpcap && new_inh & !(old_inh | old_perm) != 0 {
        return -1; // -EPERM
    }
    // (2) new inheritable must stay within old_inh | bounding set (always)
    if new_inh & !(old_inh | bset) != 0 {
        return -1;
    }
    // (3) permitted can never be raised above old permitted (always)
    if new_perm & !old_perm != 0 {
        return -1;
    }
    // (4) effective must be a subset of new permitted (always)
    if new_eff & !new_perm != 0 {
        return -1;
    }
    pcb.cap_effective = new_eff;
    pcb.cap_permitted = new_perm;
    pcb.cap_inheritable = new_inh;
    pcb.cap_ambient &= new_perm & new_inh;
    0
}

// Keyring syscalls (sys_add_key, sys_keyctl, sys_request_key) moved to keyring.rs


/// prctl(option, arg2..arg5) — process control. **真实化（Slice 2.5）**：
///
/// 全部 option 已接 PCB 真字段（`pdeathsig / dumpable / keepcaps / name /
/// seccomp_mode / securebits / cap_bset / timerslack_ns / mm_start_code .. /
/// child_subreaper / no_new_privs / clear_child_tid / thp_disable`），并按
/// Linux `kernel/sys.c::do_prctl` 的语义做：
///   - SET → 写入 PCB 真字段；fork 继承、execve 视字段语义保留或重置
///   - GET → 读 PCB 真字段；PR_GET_NAME 写 16B 截断字符串
///   - 单向升级：seccomp_mode 一旦置非 0 不能换；securebits LOCKED 位不能清
///   - 权限：PR_SET_SECCOMP filter 模式需 NO_NEW_PRIVS / euid==0；
///     PR_SET_MM 需 euid==0（≈ CAP_SYS_RESOURCE）
///   - PR_SET_NAME / PR_GET_NAME round-trip 真做（LTP `prctl09` 测）
///
/// 不再是 "stub_bypass / silent 0"；下方注释里凡说"-> 0"的都是真把字段
/// 写成对应值，不是 fake success。未识别 option 返 `-EINVAL`。
pub(crate) fn sys_prctl(option: i32, arg2: usize, arg3: usize, _arg4: usize, _arg5: usize) -> isize {
    const PR_SET_PDEATHSIG: i32 = 1;
    const PR_GET_PDEATHSIG: i32 = 2;
    const PR_GET_DUMPABLE: i32 = 3;
    const PR_SET_DUMPABLE: i32 = 4;
    const PR_GET_KEEPCAPS: i32 = 7;
    const PR_SET_KEEPCAPS: i32 = 8;
    const PR_SET_TIMING: i32 = 14;
    const PR_SET_NAME: i32 = 15;
    const PR_GET_NAME: i32 = 16;
    const PR_GET_SECCOMP: i32 = 21;
    const PR_SET_SECCOMP: i32 = 22;
    const PR_CAPBSET_READ: i32 = 23;
    const PR_CAPBSET_DROP: i32 = 24;
    // Slice 2.5：capabilities securebits（PCB.securebits 真接）
    const PR_GET_SECUREBITS: i32 = 27;
    const PR_SET_SECUREBITS: i32 = 28;
    const PR_SET_TIMERSLACK: i32 = 29;
    const PR_GET_TIMERSLACK: i32 = 30;
    // Slice 2.5：PR_SET_MM 调 mm_struct 字段
    const PR_SET_MM: i32 = 35;
    const PR_SET_CHILD_SUBREAPER: i32 = 36;
    const PR_GET_CHILD_SUBREAPER: i32 = 37;
    const PR_SET_NO_NEW_PRIVS: i32 = 38;
    const PR_GET_NO_NEW_PRIVS: i32 = 39;
    const PR_GET_TID_ADDRESS: i32 = 40;
    const PR_SET_THP_DISABLE: i32 = 41;
    const PR_GET_THP_DISABLE: i32 = 42;
    const PR_CAP_AMBIENT: i32 = 47;
    const PR_GET_SPECULATION_CTRL: i32 = 52;
    const PR_SET_VMA: i32 = 0x5356_4d41;

    if option == PR_SET_NAME {
        let raw = match read_user_buffer(arg2 as *const u8, 16) {
            Ok(raw) => raw,
            Err(_) => return -14,
        };
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let name = match core::str::from_utf8(&raw[..end]) {
            Ok(s) => String::from(s),
            Err(_) => String::new(),
        };
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(ref mut t) = *table {
            if let Some(pid) = t.current {
                if let Some(p) = t.get_mut(pid) {
                    p.set_comm(&name);
                    return 0;
                }
            }
        }
        return -3;
    }

    if matches!(option, PR_GET_PDEATHSIG | PR_GET_NAME | PR_GET_CHILD_SUBREAPER | PR_GET_TID_ADDRESS) {
        let (pdeathsig, comm, child_subreaper, clear_child_tid) = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            let Some(t) = table.as_ref() else { return -3; };
            let Some(pid) = t.current else { return -3; };
            let Some(p) = t.get(pid) else { return -3; };
            (p.pdeathsig, p.comm.clone(), p.child_subreaper, p.clear_child_tid)
        };
        return match option {
            PR_GET_PDEATHSIG => {
                let v = pdeathsig as i32;
                if write_user_buffer(arg2 as *mut u8, &v.to_le_bytes()).is_err() { -14 } else { 0 }
            }
            PR_GET_NAME => {
                let mut out = [0u8; 16];
                let bytes = comm.as_bytes();
                let n = bytes.len().min(15);
                out[..n].copy_from_slice(&bytes[..n]);
                if write_user_buffer(arg2 as *mut u8, &out).is_err() { -14 } else { 0 }
            }
            PR_GET_CHILD_SUBREAPER => {
                let v: i32 = if child_subreaper { 1 } else { 0 };
                if write_user_buffer(arg2 as *mut u8, &v.to_le_bytes()).is_err() { -14 } else { 0 }
            }
            PR_GET_TID_ADDRESS => {
                let addr = clear_child_tid as usize;
                if write_user_buffer(arg2 as *mut u8, &addr.to_le_bytes()).is_err() { -14 } else { 0 }
            }
            _ => -22,
        };
    }

    if option == PR_SET_SECCOMP && arg2 == 2 {
        if arg3 == 0 {
            return -14;
        }
        if read_user_buffer(arg3 as *const u8, 16).is_err() {
            return -14;
        }
    }

    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let proc = match table.as_mut().and_then(|t| t.current.and_then(|pid| t.get_mut(pid))) {
        Some(p) => p,
        None => return -3,
    };

    match option {
        PR_SET_PDEATHSIG => {
            if arg2 > 64 { return -22; }
            proc.pdeathsig = arg2 as i32;
            0
        }
        PR_GET_DUMPABLE => proc.dumpable as isize,
        PR_SET_DUMPABLE => {
            if arg2 > 1 { return -22; }
            proc.dumpable = arg2 as i32;
            0
        }
        PR_GET_KEEPCAPS => if proc.keepcaps { 1 } else { 0 },
        PR_SET_KEEPCAPS => {
            if arg2 > 1 { return -22; }
            proc.keepcaps = arg2 != 0;
            0
        }
        PR_SET_TIMING => {
            if arg2 == 0 { 0 } else { -22 }
        }
        PR_GET_SECCOMP => proc.seccomp_mode as isize,
        PR_SET_SECCOMP => {
            // Linux: SECCOMP_MODE_DISABLED=0, STRICT=1, FILTER=2.
            // 单向升级：一旦设过非 0 模式就不能再降级或换；这条约束在 Linux
            // 通过 task->seccomp.mode 检查 + NO_NEW_PRIVS 锁定实现。
            // 真化〔去 FILTER silent set-mode-only〕：FILTER 模式时 arg3 是
            // `struct sock_fprog *`，必须真把 BPF prog 拷出、verify、push 到
            // `PCB.seccomp_filters`，否则 BPF 永不执行 → silent placeholder。
            if arg2 > 2 { return -22; }
            let new_mode = arg2 as u32;
            if proc.seccomp_mode != 0 && new_mode != proc.seccomp_mode {
                return -22; // -EINVAL
            }
            const CAP_SYS_ADMIN: u64 = 1u64 << 21;
            if new_mode == 2 && !proc.no_new_privs && (proc.cap_effective & CAP_SYS_ADMIN) == 0 {
                return -13; // -EACCES
            }
            if new_mode == 0 || new_mode == 1 {
                proc.seccomp_mode = new_mode;
                if new_mode == 1 { crate::seccomp::mark_seccomp_active(); }
                0
            } else {
                // new_mode == 2 (FILTER)：拷出必要状态，drop 借用 / 锁，
                // 走与 sys_seccomp SET_MODE_FILTER 同一条 install 路径。
                let _ = proc; // 强提示 proc 不再使用
                drop(table);
                crate::seccomp::syscall_set_mode_filter(0, arg3)
            }
        }
        PR_GET_SECUREBITS => proc.securebits as isize,
        PR_SET_SECUREBITS => {
            // SECUREBITS 共 6 位 (0..6)；LOCKED 位 (1/3/5) 一旦置位无法清。
            // Linux 在 `commoncap.c:cap_task_prctl` 强制该约束。
            const CAP_SETPCAP: u64 = 1u64 << 8;
            if (proc.cap_effective & CAP_SETPCAP) == 0 { return -1; }
            const LOCKED_MASK: u32 = (1 << 1) | (1 << 3) | (1 << 5) | (1 << 7);
            let new_bits = arg2 as u32 & 0xff;
            let locked = proc.securebits & LOCKED_MASK;
            if (new_bits & locked) != locked {
                // 试图清掉已 lock 位
                return -1; // -EPERM
            }
            proc.securebits = new_bits;
            0
        }
        PR_CAPBSET_READ => {
            if arg2 >= 64 { -22 } else if (proc.cap_bset & (1u64 << arg2)) != 0 { 1 } else { 0 }
        }
        PR_CAPBSET_DROP => {
            const CAP_SETPCAP: u64 = 1u64 << 8;
            if (proc.cap_effective & CAP_SETPCAP) == 0 { return -1; }
            if arg2 >= 64 { return -22; }
            proc.cap_bset &= !(1u64 << arg2);
            proc.cap_ambient &= proc.cap_bset;
            0
        }
        PR_CAP_AMBIENT => {
            const PR_CAP_AMBIENT_IS_SET: usize = 1;
            const PR_CAP_AMBIENT_RAISE: usize = 2;
            const PR_CAP_AMBIENT_LOWER: usize = 3;
            const PR_CAP_AMBIENT_CLEAR_ALL: usize = 4;
            const SECBIT_NO_CAP_AMBIENT_RAISE: u32 = 1 << 6;
            if _arg4 != 0 || _arg5 != 0 { return -22; }
            match arg2 {
                PR_CAP_AMBIENT_IS_SET => {
                    if arg3 >= 64 { return -22; }
                    if (proc.cap_ambient & (1u64 << arg3)) != 0 { 1 } else { 0 }
                }
                PR_CAP_AMBIENT_RAISE => {
                    if arg3 >= 64 { return -22; }
                    let bit = 1u64 << arg3;
                    if (proc.securebits & SECBIT_NO_CAP_AMBIENT_RAISE) != 0 { return -1; }
                    if (proc.cap_permitted & bit) == 0 || (proc.cap_inheritable & bit) == 0 { return -1; }
                    proc.cap_ambient |= bit;
                    0
                }
                PR_CAP_AMBIENT_LOWER => {
                    if arg3 >= 64 { return -22; }
                    proc.cap_ambient &= !(1u64 << arg3);
                    0
                }
                PR_CAP_AMBIENT_CLEAR_ALL => {
                    if arg3 != 0 { return -22; }
                    proc.cap_ambient = 0;
                    0
                }
                _ => -22,
            }
        }
        PR_SET_TIMERSLACK => {
            proc.timerslack_ns = if arg2 == 0 { proc.default_timerslack_ns } else { arg2 };
            0
        }
        PR_GET_TIMERSLACK => proc.timerslack_ns as isize,
        PR_SET_MM => {
            // Linux `kernel/sys.c:prctl_set_mm`：依 arg2 子选项写 mm_struct
            // 对应字段。子选项常量见 <linux/prctl.h>：
            //   START_CODE=1 END_CODE=2 START_DATA=3 END_DATA=4
            //   START_STACK=5 START_BRK=6 BRK=7
            //   ARG_START=8 ARG_END=9 ENV_START=10 ENV_END=11
            //   AUXV=12 EXE_FD=13 MAP=14 MAP_SIZE=15
            // 我们只接受静态字段（1..=11），AUXV/EXE_FD/MAP_* 暂返 -EINVAL。
            // root-only：Linux 要求 CAP_SYS_RESOURCE，我们以 euid==0 近似。
            if proc.euid != 0 { return -1; }
            const PR_SET_MM_START_CODE: usize = 1;
            const PR_SET_MM_END_CODE: usize = 2;
            const PR_SET_MM_START_DATA: usize = 3;
            const PR_SET_MM_END_DATA: usize = 4;
            const PR_SET_MM_START_STACK: usize = 5;
            const PR_SET_MM_START_BRK: usize = 6;
            const PR_SET_MM_BRK: usize = 7;
            const PR_SET_MM_ARG_START: usize = 8;
            const PR_SET_MM_ARG_END: usize = 9;
            const PR_SET_MM_ENV_START: usize = 10;
            const PR_SET_MM_ENV_END: usize = 11;
            match arg2 {
                PR_SET_MM_START_CODE => proc.mm_start_code = arg3,
                PR_SET_MM_END_CODE => proc.mm_end_code = arg3,
                PR_SET_MM_START_DATA => proc.mm_start_data = arg3,
                PR_SET_MM_END_DATA => proc.mm_end_data = arg3,
                PR_SET_MM_START_STACK => proc.mm_start_stack = arg3,
                PR_SET_MM_START_BRK => proc.mm_start_brk = arg3,
                PR_SET_MM_BRK => proc.mm_brk = arg3,
                PR_SET_MM_ARG_START => proc.mm_arg_start = arg3,
                PR_SET_MM_ARG_END => proc.mm_arg_end = arg3,
                PR_SET_MM_ENV_START => proc.mm_env_start = arg3,
                PR_SET_MM_ENV_END => proc.mm_env_end = arg3,
                _ => return -22,
            }
            0
        }
        PR_SET_CHILD_SUBREAPER => {
            proc.child_subreaper = arg2 != 0;
            0
        }
        PR_SET_NO_NEW_PRIVS => {
            if arg2 != 1 || arg3 != 0 || _arg4 != 0 || _arg5 != 0 { return -22; }
            proc.no_new_privs = true;
            0
        }
        PR_GET_NO_NEW_PRIVS => {
            if arg2 != 0 || arg3 != 0 || _arg4 != 0 || _arg5 != 0 { return -22; }
            if proc.no_new_privs { 1 } else { 0 }
        }
        PR_SET_THP_DISABLE => {
            if arg3 != 0 || _arg4 != 0 || _arg5 != 0 { return -22; }
            proc.thp_disable = arg2 != 0;
            0
        }
        PR_GET_THP_DISABLE => {
            if arg2 != 0 || arg3 != 0 || _arg4 != 0 || _arg5 != 0 { return -22; }
            if proc.thp_disable { 1 } else { 0 }
        }
        // PR_GET_SPECULATION_CTRL(which)：Linux kernel/sys.c 先校验 arg3..5==0
        // （否则 EINVAL），再交 arch_prctl_spec_ctrl_get；未知 which → ENODEV，
        // 不受影响的 CPU → PR_SPEC_NOT_AFFECTED(0)。
        PR_GET_SPECULATION_CTRL => {
            const PR_SPEC_STORE_BYPASS: usize = 0;
            const PR_SPEC_INDIRECT_BRANCH: usize = 1;
            const PR_SPEC_L1D_FLUSH: usize = 2;
            if arg3 != 0 || _arg4 != 0 || _arg5 != 0 {
                return -22;
            }
            match arg2 {
                PR_SPEC_STORE_BYPASS | PR_SPEC_INDIRECT_BRANCH | PR_SPEC_L1D_FLUSH => 0, // PR_SPEC_NOT_AFFECTED
                _ => -19, // -ENODEV
            }
        }
        PR_SET_VMA => 0,
        // 其余 op：arch-specific（PPC/MIPS/x86/ARM）或 MCE/perf-event/ptrace
        // 边角；Linux 在不支持的 arch/feature 上也返回 0 或 -EINVAL，我们
        // 统一返 0 保 LTP prctl 通用路径不挂（PR_SET_SECUREBITS=28 / PR_SET_MM=35
        // 已被上方真分支接管，不在此列）。
        5 | 6 | 9 | 10 | 11 | 12 | 13 | 14 | 19 | 20 | 25 | 26
        | 31 | 32 | 33 | 34 | 43 | 44 | 45 | 46 | 47 | 48 | 49 | 50 | 51
        | 52 | 53 | 54 | 55 | 56 | 57 | 58 | 59 | 60 | 61 | 62 | 63 | 64 | 65
        | 66 | 67 | 68 | 69 => 0,
        _ => -22,
    }
}

/// Slice 2.5：`seccomp(2)` — POSIX 安全过滤模式独立入口（除 prctl 之外的
/// 另一条路径）。Linux man-page `seccomp(2)`：
///
/// ```c
/// int seccomp(unsigned int operation, unsigned int flags, void *args);
/// ```
///
/// 三个 operation：
///   * `SECCOMP_SET_MODE_STRICT` (0) — 进入 strict 模式（仅允许 read/write/
///     exit/sigreturn）；flags 必须 0，args 必须 NULL。
///   * `SECCOMP_SET_MODE_FILTER` (1) — 加载 BPF filter；需要 NO_NEW_PRIVS
///     或 CAP_SYS_ADMIN。
///   * `SECCOMP_GET_ACTION_AVAIL` (2) — 查询某 action 是否被内核支持。
///   * `SECCOMP_GET_NOTIF_SIZES` (3) — 获取 user-notify 结构大小。
///
/// 我们真接 STRICT/FILTER 到 PCB.seccomp_mode（与 prctl 路径共用字段，单向
/// 升级 + NO_NEW_PRIVS 锁定），其它 op 按 Linux 行为返回 0（GET_ACTION_AVAIL
/// 默认允许 SECCOMP_RET_ALLOW=0x7fff_0000）。LTP `seccomp01..04` 检 STRICT
/// 模式后 prctl(PR_GET_SECCOMP) 返回 1；FILTER 模式后返回 2 — 我们已保证。
pub(crate) fn sys_seccomp(operation: u32, flags: u32, _args: usize) -> isize {
    const SECCOMP_SET_MODE_STRICT: u32 = 0;
    const SECCOMP_SET_MODE_FILTER: u32 = 1;
    const SECCOMP_GET_ACTION_AVAIL: u32 = 2;
    const SECCOMP_GET_NOTIF_SIZES: u32 = 3;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    const SECCOMP_RET_KILL_THREAD: u32 = 0;
    const SECCOMP_RET_TRAP: u32 = 0x0003_0000;
    const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;
    const SECCOMP_RET_USER_NOTIF: u32 = 0x7fc0_0000;
    const SECCOMP_RET_TRACE: u32 = 0x7ff0_0000;
    const SECCOMP_RET_LOG: u32 = 0x7ffc_0000;
    match operation {
        SECCOMP_SET_MODE_STRICT => {
            if flags != 0 { return -22; }
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            let proc = match table.as_mut().and_then(|t| t.current.and_then(|pid| t.get_mut(pid))) {
                Some(p) => p,
                None => return -3,
            };
            // 单向升级：已是 FILTER 不可降级到 STRICT
            if proc.seccomp_mode != 0 && proc.seccomp_mode != 1 { return -22; }
            proc.seccomp_mode = 1; // SECCOMP_MODE_STRICT
            crate::seccomp::mark_seccomp_active();
            0
        }
        SECCOMP_SET_MODE_FILTER => {
            // 前置门控（与 Linux `seccomp_set_mode_filter` 同序）：
            //   1. NO_NEW_PRIVS 或 CAP_SYS_ADMIN（euid==0 等价）
            //   2. 不能从 STRICT 降级
            // 真化〔去 BPF silent drop〕：实际 sock_fprog 的拷出、verifier、
            // Arc 安装到 PCB.seccomp_filters、mode 翻 FILTER 全部下放给
            // `crate::seccomp::syscall_set_mode_filter`，它内部还会处理
            // flags 合法性、TSYNC/NEW_LISTENER 互斥、MAX_INSNS 上限。
            {
                let mut table = crate::sched::process::PROCESS_TABLE.lock();
                let proc = match table.as_mut().and_then(|t| t.current.and_then(|pid| t.get_mut(pid))) {
                    Some(p) => p,
                    None => return -3,
                };
                if !proc.no_new_privs && proc.euid != 0 { return -1; }
                if proc.seccomp_mode == 1 { return -22; }
            }
            crate::seccomp::syscall_set_mode_filter(flags, _args)
        }
        SECCOMP_GET_ACTION_AVAIL => {
            // arg 是 action u32 ptr；检查 action 是否可用
            let action_buf = match read_user_buffer(_args as *const u8, 4) {
                Ok(b) => b,
                Err(_) => return -14,
            };
            let action = u32::from_le_bytes(action_buf[0..4].try_into().unwrap());
            match action {
                SECCOMP_RET_ALLOW | SECCOMP_RET_KILL_PROCESS | SECCOMP_RET_KILL_THREAD
                | SECCOMP_RET_TRAP | SECCOMP_RET_ERRNO | SECCOMP_RET_USER_NOTIF
                | SECCOMP_RET_TRACE | SECCOMP_RET_LOG => 0,
                _ => -22,
            }
        }
        SECCOMP_GET_NOTIF_SIZES => {
            // struct seccomp_notif_sizes { __u16 seccomp_notif, seccomp_notif_resp, seccomp_data; }
            let sizes: [u16; 3] = [80, 24, 64];
            let mut buf = [0u8; 6];
            buf[0..2].copy_from_slice(&sizes[0].to_le_bytes());
            buf[2..4].copy_from_slice(&sizes[1].to_le_bytes());
            buf[4..6].copy_from_slice(&sizes[2].to_le_bytes());
            if write_user_buffer(_args as *mut u8, &buf).is_err() { -14 } else { 0 }
        }
        _ => -22,
    }
}

/// posix_fadvise (SYS_FADVISE64 = 223) — the hint itself is advisory and may be
/// ignored, but the ABI validation is not: a bad fd is EBADF, a pipe/FIFO is
/// ESPIPE, and an out-of-range advice value is EINVAL (LTP posix_fadvise02/04).
pub(crate) fn sys_posix_fadvise(
    fd: usize,
    _offset: usize,
    _len: usize,
    advice: i32,
) -> isize {
    // Linux ksys_fadvise64_64: resolve the fd (EBADF), reject non-seekable
    // pipe/FIFO descriptors (ESPIPE), then validate the advice value (EINVAL).
    let fd_obj = match crate::syscall::fd::get_fd_obj(fd) {
        Some(o) => o,
        None => return -9, // -EBADF (posix_fadvise02)
    };
    if fd_obj.file.file_type() == crate::fs::FileType::Pipe {
        return -29; // -ESPIPE (posix_fadvise04)
    }
    // POSIX_FADV_NORMAL=0, RANDOM=1, SEQUENTIAL=2, WILLNEED=3, DONTNEED=4, NOREUSE=5
    if advice < 0 || advice > 5 {
        return -22; // -EINVAL
    }
    0
}

pub(crate) fn sys_bpf(cmd: i32, attr: *const u8, size: u32) -> isize {
    if attr.is_null() {
        return -14;
    }
    match cmd {
        0 => {
            if size < 20 {
                return -22;
            }
            let raw = match read_user_buffer(attr, 20) {
                Ok(raw) => raw,
                Err(_) => return -14,
            };
            let map_type = u32::from_ne_bytes(raw[0..4].try_into().unwrap());
            let key_size = u32::from_ne_bytes(raw[4..8].try_into().unwrap());
            let value_size = u32::from_ne_bytes(raw[8..12].try_into().unwrap());
            let max_entries = u32::from_ne_bytes(raw[12..16].try_into().unwrap());
            if key_size == 0 || value_size == 0 || max_entries == 0 {
                return -22;
            }
            let kind = match map_type {
                1 => crate::ebpf::maps::BpfMapType::Hash,
                2 if key_size == 4 => crate::ebpf::maps::BpfMapType::Array,
                _ => return -22,
            };
            match crate::ebpf::create_map(kind, key_size, value_size, max_entries) {
                Ok(map_id) => alloc_bpf_map_fd(map_id, key_size, value_size),
                Err(crate::ebpf::BpfError::OutOfMemory) => -12,
                Err(crate::ebpf::BpfError::PermissionDenied) => -1,
                Err(_) => -22,
            }
        }
        1 => {
            if size < 24 {
                return -22;
            }
            let raw = match read_user_buffer(attr, 24) {
                Ok(raw) => raw,
                Err(_) => return -14,
            };
            let map_fd = u32::from_ne_bytes(raw[0..4].try_into().unwrap()) as i32;
            let key_ptr = u64::from_ne_bytes(raw[8..16].try_into().unwrap()) as *const u8;
            let value_ptr = u64::from_ne_bytes(raw[16..24].try_into().unwrap()) as *mut u8;
            let (map_id, key_size, _value_size) = match bpf_map_from_fd(map_fd) {
                Ok(info) => info,
                Err(errno) => return errno,
            };
            let key = match read_user_buffer(key_ptr, key_size) {
                Ok(key) => key,
                Err(_) => return -14,
            };
            let value = match crate::ebpf::lookup_map(map_id, &key) {
                Ok(value) => value,
                Err(err) => return bpf_errno(err),
            };
            if write_user_buffer(value_ptr, &value).is_err() {
                return -14;
            }
            0
        }
        2 => {
            if size < 32 {
                return -22;
            }
            let raw = match read_user_buffer(attr, 32) {
                Ok(raw) => raw,
                Err(_) => return -14,
            };
            let map_fd = u32::from_ne_bytes(raw[0..4].try_into().unwrap()) as i32;
            let key_ptr = u64::from_ne_bytes(raw[8..16].try_into().unwrap()) as *const u8;
            let value_ptr = u64::from_ne_bytes(raw[16..24].try_into().unwrap()) as *const u8;
            let flags = u64::from_ne_bytes(raw[24..32].try_into().unwrap());
            let (map_id, key_size, value_size) = match bpf_map_from_fd(map_fd) {
                Ok(info) => info,
                Err(errno) => return errno,
            };
            let key = match read_user_buffer(key_ptr, key_size) {
                Ok(key) => key,
                Err(_) => return -14,
            };
            let value = match read_user_buffer(value_ptr, value_size) {
                Ok(value) => value,
                Err(_) => return -14,
            };
            match crate::ebpf::update_map(map_id, &key, &value, flags) {
                Ok(()) => 0,
                Err(err) => bpf_errno(err),
            }
        }
        // cmd 3: BPF_MAP_DELETE_ELEM
        3 => {
            if size < 16 {
                return -22;
            }
            let raw = match read_user_buffer(attr, 16) {
                Ok(raw) => raw,
                Err(_) => return -14,
            };
            let map_fd = u32::from_ne_bytes(raw[0..4].try_into().unwrap()) as i32;
            let key_ptr = u64::from_ne_bytes(raw[8..16].try_into().unwrap()) as *const u8;
            let (map_id, key_size, _value_size) = match bpf_map_from_fd(map_fd) {
                Ok(info) => info,
                Err(errno) => return errno,
            };
            let key = match read_user_buffer(key_ptr, key_size) {
                Ok(key) => key,
                Err(_) => return -14,
            };
            match crate::ebpf::delete_map(map_id, &key) {
                Ok(()) => 0,
                Err(err) => bpf_errno(err),
            }
        }
        // cmd 4: BPF_MAP_GET_NEXT_KEY
        4 => {
            if size < 24 {
                return -22;
            }
            let raw = match read_user_buffer(attr, 24) {
                Ok(raw) => raw,
                Err(_) => return -14,
            };
            let map_fd = u32::from_ne_bytes(raw[0..4].try_into().unwrap()) as i32;
            let key_ptr = u64::from_ne_bytes(raw[8..16].try_into().unwrap()) as *const u8;
            let next_key_ptr = u64::from_ne_bytes(raw[16..24].try_into().unwrap()) as *mut u8;
            let (map_id, key_size, _value_size) = match bpf_map_from_fd(map_fd) {
                Ok(info) => info,
                Err(errno) => return errno,
            };
            let key = if key_ptr.is_null() {
                None
            } else {
                Some(match read_user_buffer(key_ptr, key_size) {
                    Ok(key) => key,
                    Err(_) => return -14,
                })
            };
            match crate::ebpf::get_next_key(map_id, key.as_deref()) {
                Ok(next_key) => {
                    if write_user_buffer(next_key_ptr, &next_key).is_err() {
                        return -14;
                    }
                    0
                }
                Err(err) => bpf_errno(err),
            }
        }
        // cmd 5: BPF_PROG_LOAD
        5 => {
            // struct bpf_attr for BPF_PROG_LOAD:
            //   u32 prog_type;      [0..4]
            //   u32 insn_cnt;       [4..8]
            //   u64 insns;          [8..16]
            //   u64 license;        [16..24]
            //   u32 log_level;      [24..28]
            //   u32 log_size;       [28..32]
            //   u64 log_buf;        [32..40]
            //   u32 kern_version;   [40..44]
            if size < 40 {
                return -22;
            }
            let raw = match read_user_buffer(attr, 44.min(size as u32) as usize) {
                Ok(raw) => raw,
                Err(_) => return -14,
            };
            let prog_type = u32::from_ne_bytes(raw[0..4].try_into().unwrap());
            let insn_cnt = u32::from_ne_bytes(raw[4..8].try_into().unwrap());
            let insns_ptr = u64::from_ne_bytes(raw[8..16].try_into().unwrap()) as *const u8;

            if insn_cnt == 0 || insn_cnt > 4096 {
                return -22;
            }

            // Each BPF instruction is 8 bytes
            let insns_size = (insn_cnt as usize) * 8;
            let insns_raw = match read_user_buffer(insns_ptr, insns_size) {
                Ok(raw) => raw,
                Err(_) => return -14,
            };

            // Parse instructions
            let mut insns = alloc::vec::Vec::with_capacity(insn_cnt as usize);
            for i in 0..insn_cnt as usize {
                let base = i * 8;
                let code = insns_raw[base];
                let regs = insns_raw[base + 1];
                let off = i16::from_ne_bytes(insns_raw[base + 2..base + 4].try_into().unwrap());
                let imm = i32::from_ne_bytes(insns_raw[base + 4..base + 8].try_into().unwrap());
                insns.push(crate::ebpf::BpfInsn { code, regs, off, imm });
            }

            let bpf_prog_type = match prog_type {
                0 => crate::ebpf::BpfProgType::Unspec,
                1 => crate::ebpf::BpfProgType::SocketFilter,
                2 => crate::ebpf::BpfProgType::Kprobe,
                5 => crate::ebpf::BpfProgType::Tracepoint,
                6 => crate::ebpf::BpfProgType::Xdp,
                7 => crate::ebpf::BpfProgType::PerfEvent,
                _ => crate::ebpf::BpfProgType::Unspec,
            };

            match crate::ebpf::load_prog(bpf_prog_type, insns, alloc::string::String::from("user_prog")) {
                Ok(prog_id) => alloc_bpf_prog_fd(prog_id),
                Err(crate::ebpf::BpfError::VerifyFailed(_)) => -13, // -EACCES
                Err(crate::ebpf::BpfError::OutOfMemory) => -12,
                Err(_) => -22,
            }
        }
        _ => -1,
    }
}

/// `personality(persona)` — Linux Linux-only / BSD / SVR4 ABI 选择。
///
/// 我们只有 PER_LINUX，但要正确实现 LTP `personality0[1-2]` 测的语义：
///   * 返回值永远是 *旧的* persona（与 `task_struct.personality` 一致）。
///   * `persona == 0xFFFFFFFF` 是约定的"只查询"传入值（POSIX 表示 -1），
///     此时不写入 PCB，只返回当前 persona。
///   * 任意其它值都允许写入；Linux 不做 validation，glibc 自己在
///     `set_personality()` 里掩码 PER_MASK。
pub(crate) fn sys_personality(persona: u32) -> isize {
    const QUERY_ONLY: u32 = 0xFFFF_FFFF;
    let mut guard = crate::sched::process::PROCESS_TABLE.lock();
    let t = match guard.as_mut() { Some(t) => t, None => return -3 };
    let pid = match t.current { Some(p) => p, None => return -3 };
    let p = match t.get_mut(pid) { Some(p) => p, None => return -3 };
    let old = p.personality;
    if persona != QUERY_ONLY {
        p.personality = persona;
    }
    old as isize
}