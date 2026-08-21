// ---- NUMA / page migration / mempolicy ------------------------------------
// 单 NUMA 节点（node 0）模型；下面的 syscalls 仍按 Linux 语义做参数校验，
// 否则 LTP mbind/set_mempolicy/migrate_pages 的 negative-path 用例会因
// silent 0 而错判。校验范围参考 mm/mempolicy.c 中 sanitize_mpol_flags /
// mpol_check_policy / get_nodes 的最小约束。
//
// MPOL_DEFAULT=0, MPOL_PREFERRED=1, MPOL_BIND=2, MPOL_INTERLEAVE=3,
// MPOL_LOCAL=4, MPOL_PREFERRED_MANY=5  → 已知 mode 范围 [0,5]
// 上半 flags: MPOL_F_STATIC_NODES=0x8000_0000, MPOL_F_RELATIVE_NODES=0x4000_0000
// maxnode 上限：MAX_NUMNODES = 1<<10 = 1024 节点 → maxnode 字段值不能 >= 4096
// mbind flags: MPOL_MF_STRICT=1, MPOL_MF_MOVE=2, MPOL_MF_MOVE_ALL=4
// move_pages flags: MPOL_MF_MOVE=2, MPOL_MF_MOVE_ALL=4
const MPOL_MODE_MASK: i32 = 0x7;
const MPOL_F_FLAGS: u32 = 0xC000_0000;
const MPOL_MAX_NUMNODES: u64 = 1024;
const MBIND_FLAGS: u32 = 0x7; // STRICT | MOVE | MOVE_ALL
const MOVE_PAGES_FLAGS: i32 = 0x6; // MOVE | MOVE_ALL
const GET_MEMPOLICY_FLAGS: usize = 0x7;
const MPOL_F_NODE: usize = 0x1;
const MPOL_F_ADDR: usize = 0x2;
const MPOL_F_MEMS_ALLOWED: usize = 0x4;

fn mempolicy_mode_ok(mode: i32) -> bool {
    let bare = mode & MPOL_MODE_MASK;
    let upper = (mode as u32) & !((MPOL_MODE_MASK as u32) | MPOL_F_FLAGS);
    upper == 0 && (0..=5).contains(&bare)
}

fn read_mempolicy_mask(nodemask: *const u64, maxnode: u64) -> Result<u64, isize> {
    if maxnode > MPOL_MAX_NUMNODES + 1 {
        return Err(-22);
    }
    if nodemask.is_null() || maxnode == 0 {
        return Ok(0);
    }
    let words = ((maxnode as usize) + 63) / 64;
    let data = crate::syscall::user::read_user_buffer(nodemask as *const u8, words * 8)
        .map_err(|_| -14isize)?;
    let mut mask = 0u64;
    for (idx, chunk) in data.chunks_exact(8).enumerate() {
        let mut word = [0u8; 8];
        word.copy_from_slice(chunk);
        let value = u64::from_le_bytes(word);
        if idx == 0 {
            mask = value;
        } else if value != 0 {
            return Err(-22);
        }
    }
    if mask & !1 != 0 {
        return Err(-22);
    }
    Ok(mask)
}

fn normalize_mempolicy(mode: i32, nodemask: *const u64, maxnode: u64) -> Result<(i32, u64), isize> {
    if !mempolicy_mode_ok(mode) {
        return Err(-22);
    }
    let node_flags = (mode as u32) & MPOL_F_FLAGS;
    if node_flags == MPOL_F_FLAGS {
        return Err(-22);
    }
    let bare = mode & MPOL_MODE_MASK;
    let mask = read_mempolicy_mask(nodemask, maxnode)?;
    match bare {
        0 | 4 => {
            if mask != 0 {
                Err(-22)
            } else {
                Ok((bare, 0))
            }
        }
        1 => Ok((bare, mask)),
        2 | 3 | 5 => {
            if mask == 0 {
                Err(-22)
            } else {
                Ok((bare, mask))
            }
        }
        _ => Err(-22),
    }
}

fn mapped_user_addr_for_policy(process: &crate::sched::process::ProcessControlBlock, addr: usize) -> bool {
    if addr == 0 {
        return false;
    }
    process.mm.as_ref().map_or(false, |mm| {
        let address_space = mm.lock();
        let hit = address_space.vmas_iter().any(|vma| addr >= vma.start.0 && addr < vma.end.0);
        hit
    })
}

fn write_mempolicy_mask(nodemask: *mut u8, maxnode: usize, mask: u64) -> Result<(), isize> {
    if nodemask.is_null() || maxnode == 0 {
        return Ok(());
    }
    if maxnode as u64 > MPOL_MAX_NUMNODES + 1 {
        return Err(-22);
    }
    let bytes = (maxnode + 7) / 8;
    let mut buf = alloc::vec![0u8; bytes];
    if mask & 1 != 0 && !buf.is_empty() {
        buf[0] = 1;
    }
    crate::syscall::user::write_user_buffer(nodemask, &buf).map_err(|_| -14isize)
}

pub(crate) fn sys_move_pages(
    _pid: i32, _count: usize, _pages: *const usize, _nodes: *const i32, _status: *mut i32, flags: i32,
) -> isize {
    if flags & !MOVE_PAGES_FLAGS != 0 { return -22; }
    // 单节点：直接报"全部已在节点 0"（成功，无操作）。
    0
}

pub(crate) fn sys_mbind(
    start: usize, len: usize, mode: i32, nodemask: *const u64, maxnode: u64, flags: u32,
) -> isize {
    if flags & !MBIND_FLAGS != 0 { return -22; }
    if start & 0xfff != 0 { return -22; }
    if len == 0 { return 0; }
    let end = match start.checked_add(len) {
        Some(end) => end,
        None => return -22,
    };
    let (bare, mask) = match normalize_mempolicy(mode, nodemask, maxnode) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match table_guard.as_mut() { Some(t) => t, None => return -3 };
    let current = match table.current { Some(pid) => pid, None => return -3 };
    let as_pid = match table.get(current) {
        Some(process) if process.mm.is_some() => current,
        Some(process) => process.tgid,
        None => return -3,
    };
    let process = match table.get_mut(as_pid) { Some(p) => p, None => return -3 };
    let has_overlap = process.mm.as_ref().map_or(false, |mm| {
        let address_space = mm.lock();
        let hit = address_space.vmas_iter().any(|vma| start < vma.end.0 && end > vma.start.0);
        hit
    });
    if !has_overlap {
        return -12;
    }
    process.mbind_policies.push((start, end, bare, mask));
    0
}

pub(crate) fn sys_set_mempolicy(mode: i32, nodemask: *const u64, maxnode: u64) -> isize {
    let (bare, mask) = match normalize_mempolicy(mode, nodemask, maxnode) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut table_guard = crate::sched::process::PROCESS_TABLE.lock();
    let table = match table_guard.as_mut() { Some(t) => t, None => return -3 };
    let current = match table.current { Some(pid) => pid, None => return -3 };
    let process = match table.get_mut(current) { Some(p) => p, None => return -3 };
    process.mempolicy_mode = bare;
    process.mempolicy_nodemask = mask;
    0
}

pub(crate) fn sys_get_mempolicy(mode: *mut u8, nodemask: *mut u8, maxnode: usize, addr: usize, flags: usize) -> isize {
    if flags & !GET_MEMPOLICY_FLAGS != 0 {
        return -22;
    }
    if flags & MPOL_F_NODE != 0 && flags & MPOL_F_ADDR == 0 && flags & MPOL_F_MEMS_ALLOWED == 0 {
        return -22;
    }
    if maxnode as u64 > MPOL_MAX_NUMNODES + 1 {
        return -22;
    }
    let (policy, mask) = {
        let table_guard = crate::sched::process::PROCESS_TABLE.lock();
        let table = match table_guard.as_ref() { Some(t) => t, None => return -3 };
        let current = match table.current { Some(pid) => pid, None => return -3 };
        let process = match table.get(current) { Some(p) => p, None => return -3 };
        let as_pid = if process.mm.is_some() { current } else { process.tgid };
        let mm_process = match table.get(as_pid) { Some(p) => p, None => return -3 };
        if flags & MPOL_F_MEMS_ALLOWED != 0 {
            (0, 1)
        } else if flags & MPOL_F_ADDR != 0 {
            if !mapped_user_addr_for_policy(mm_process, addr) {
                return -14;
            }
            let mut result = (process.mempolicy_mode, process.mempolicy_nodemask);
            for &(start, end, mbind_mode, mbind_mask) in mm_process.mbind_policies.iter().rev() {
                if addr >= start && addr < end {
                    result = (mbind_mode, mbind_mask);
                    break;
                }
            }
            result
        } else {
            let mask = if process.mempolicy_mode == 0 && process.mempolicy_nodemask == 0 {
                1
            } else {
                process.mempolicy_nodemask
            };
            (process.mempolicy_mode, mask)
        }
    };
    let out_policy = if flags & MPOL_F_NODE != 0 { if mask & 1 != 0 { 0 } else { -1 } } else { policy };
    if !mode.is_null() {
        let bytes = (out_policy as i32).to_le_bytes();
        if crate::syscall::user::write_user_buffer(mode, &bytes).is_err() {
            return -14;
        }
    }
    if let Err(e) = write_mempolicy_mask(nodemask, maxnode, mask) {
        return e;
    }
    0
}

pub(crate) fn sys_migrate_pages(_pid: i32, maxnode: u64, _from: *const u64, _to: *const u64) -> isize {
    if maxnode > MPOL_MAX_NUMNODES + 1 { return -22; }
    0
}

/// process_madvise(pidfd, iov, iovcnt, advice, flags) — Linux 5.10+ 真化。
///
/// Slice 2.x TIER 4 真化（去 silent 0 stub）：
/// * 解析 pidfd → target_pid（沿用 `pidfd_resolve_pid` 同样的 `pidfd:<pid>`
///   path 解析）。无效 → `-EBADF(9)`。
/// * 要求 target 进程存在 & 与当前进程同 tgid 或就是当前 pid（PTRACE_MODE_READ
///   语义近似）。跨 tgid 访问 → `-EPERM(1)`（与 Linux 在缺权限时一致）。
/// * 遍历 `iov[iovcnt]`（`struct iovec { *base; len }`，16 字节）。每段对 target
///   进程地址空间调 `madvise_dontneed`（仅 DONTNEED/FREE/REMOVE/COLD/PAGEOUT
///   走真释放；其它 advice 静默成功）。
/// * `flags` 必须 0（Linux 5.10..5.18 行为，未引入新 flag 之前）。
///
/// 返回真处理的总字节数 — Linux 行为是返回 ≥ 0 字节数；出错时返负 errno。
pub(crate) fn sys_process_madvise(
    pidfd: i32,
    iov: *const u8,
    iovcnt: usize,
    advice: i32,
    flags: u32,
) -> isize {
    if flags != 0 { return -22; }
    if iovcnt == 0 { return 0; }
    if iovcnt > 1024 { return -22; }

    // 解析 pidfd → target pid（直接读 FileDescriptor.path = "pidfd:<pid>"）
    let target_pid: usize = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -9 };
        let pid = match t.current { Some(p) => p, None => return -9 };
        let proc = match t.get_fd_process(pid) { Some(p) => p, None => return -9 };
        let fd_obj = match crate::fs::fd::get(proc, pidfd as usize) { Some(f) => f, None => return -9 };
        let path = match fd_obj.path.as_ref() { Some(p) => p, None => return -9 };
        let suffix = match path.strip_prefix("pidfd:") { Some(s) => s, None => return -9 };
        // path 形如 "pidfd:<pid>:<instance_id>"（旧形式无 :<instance_id>）
        let pid_str = suffix.split(':').next().unwrap_or(suffix);
        match pid_str.parse::<usize>() { Ok(p) => p, Err(_) => return -9 }
    };

    // 同 tgid 校验（PTRACE_MODE_READ）
    let (current_tgid, target_tgid) = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -1 };
        let cur = match t.current { Some(p) => p, None => return -1 };
        let ct = t.get(cur).map(|p| p.tgid).unwrap_or(cur);
        let tt = match t.get(target_pid) {
            Some(p) => p.tgid,
            None => return -3, // -ESRCH
        };
        (ct, tt)
    };
    if current_tgid != target_tgid {
        return -1; // -EPERM
    }

    // 读取 iov 数组
    let iov_bytes = match crate::syscall::user::read_user_buffer(iov, iovcnt * 16) {
        Ok(b) => b,
        Err(_) => return -14,
    };

    // 解析 advice：只有 DONTNEED 系列走真 unmap；其余 noop（与单进程 madvise 一致）
    const MADV_DONTNEED: i32 = 4;
    const MADV_FREE:     i32 = 8;
    const MADV_REMOVE:   i32 = 9;
    const MADV_COLD:     i32 = 20;
    const MADV_PAGEOUT:  i32 = 21;
    let real_unmap = matches!(advice, MADV_DONTNEED | MADV_FREE | MADV_REMOVE | MADV_COLD | MADV_PAGEOUT);

    let mut processed: isize = 0;
    for i in 0..iovcnt {
        let off = i * 16;
        let base = usize::from_le_bytes(iov_bytes[off..off+8].try_into().unwrap());
        let len  = usize::from_le_bytes(iov_bytes[off+8..off+16].try_into().unwrap());
        if len == 0 { continue; }
        if base & 0xFFF != 0 { return -22; }
        let end = match base.checked_add(len) { Some(e) => e, None => return -22 };
        if real_unmap {
            use crate::mm::VirtAddr;
            let mut table = crate::sched::process::PROCESS_TABLE.lock();
            let t = match table.as_mut() { Some(t) => t, None => return -1 };
            // 取目标进程的 address-space 持有者（线程组 leader 或 tgid）
            let as_pid = match t.get(target_pid) {
                Some(p) => if p.mm.is_some() { target_pid } else { p.tgid },
                None => return -3,
            };
            if let Some(mm) = t.get(as_pid).and_then(|p| p.mm.as_ref()).cloned() {
                let mut addr_space = mm.lock();
                let _ = addr_space.madvise_dontneed(VirtAddr(base), VirtAddr(end));
                drop(addr_space);
                drop(table);
                // 与 sys_madvise(DONTNEED) 同语义：目标 mm 的线程随后必须
                // 读到零页；锁外同步收尾跨核失效。
                crate::mm::tlb::drain_shootdown();
            }
        }
        processed = processed.saturating_add(len as isize);
    }
    processed
}

// ---- pkey_* ----------------------------------------------------------------
//
// Linux PKU(Protection Keys for Userspace) 在 RV64/LA64 上无硬件支持，也没有
// 用户态 PKRU 寄存器——key 的访问权限只能在 pkey_alloc(init access) 时设定，
// 此后不可变。据此内核以"有效权限"软件仿真（可观测行为对标 x86 PKU）：
//   * 分配位图与权限表 per-mm（Linux：key 分配 per-mm，PKRU 随 fork 继承，
//     exec 重建地址空间即重置），key 0 默认、不可 alloc/free，共 16 key。
//   * pkey_alloc：flags 必须 0，access 仅允许
//     PKEY_DISABLE_ACCESS(0x1)|PKEY_DISABLE_WRITE(0x2)，否则 -EINVAL；满 -ENOSPC。
//   * pkey_mprotect：把 VMA 挂到 key，并把 prot 扣除 key 拒绝位后落 PTE，
//     被拒绝的访问走普通权限缺页 → SIGSEGV（LTP pkey01）。
//   * 普通 mprotect 保留 VMA 现有 key（Linux do_mprotect_pkey(pkey=-1)）。
const PKEY_MAX: i32 = 16;
const PKEY_DISABLE_ACCESS: u32 = 0x1;
const PKEY_DISABLE_WRITE: u32 = 0x2;

fn with_current_mm<R>(f: impl FnOnce(&mut crate::mm::AddressSpace) -> R) -> Option<R> {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let table = table.as_mut()?;
    let pid = table.current?;
    let as_pid = {
        let p = table.get(pid)?;
        if p.mm.is_some() { pid } else { p.tgid }
    };
    let mm = table.get(as_pid)?.mm.as_ref()?.clone();
    let mut addr_space = mm.lock();
    Some(f(&mut addr_space))
}

pub(crate) fn sys_pkey_mprotect(addr: usize, len: usize, prot: usize, pkey: i32) -> isize {
    let mut reduced = false;
    let rc = crate::syscall::memory::do_mprotect(addr, len, prot, Some(pkey), &mut reduced);
    // 同 sys_mprotect：返回前对本 mm 同步收尾跨核 TLB 失效（仅权限收紧时）。
    if rc == 0 && reduced {
        crate::mm::tlb::drain_shootdown();
    }
    rc
}

pub(crate) fn sys_pkey_alloc(flags: u32, access: u32) -> isize {
    if flags != 0 || access & !(PKEY_DISABLE_ACCESS | PKEY_DISABLE_WRITE) != 0 {
        return -22;
    }
    with_current_mm(|mm| {
        for k in 1..PKEY_MAX {
            if mm.pkey_alloc_mask & (1u32 << k) == 0 {
                mm.pkey_alloc_mask |= 1u32 << k;
                mm.pkey_rights[k as usize] = access as u8;
                return k as isize;
            }
        }
        -28 // ENOSPC
    })
    .unwrap_or(-22)
}

pub(crate) fn sys_pkey_free(pkey: i32) -> isize {
    if pkey <= 0 || pkey >= PKEY_MAX { return -22; }
    with_current_mm(|mm| {
        let bit = 1u32 << pkey;
        if mm.pkey_alloc_mask & bit == 0 { return -22; }
        mm.pkey_alloc_mask &= !bit;
        mm.pkey_rights[pkey as usize] = 0;
        0
    })
    .unwrap_or(-22)
}
