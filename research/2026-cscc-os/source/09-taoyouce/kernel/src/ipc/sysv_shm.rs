//! SysV shared memory implementation.
//!
//! Extracted from `syscall/shm.rs` to align with Linux `ipc/shm.c`.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, AtomicI32, Ordering};

use crate::sync::SpinLock;
use super::sysv::{IpcPerm, current_pid_locked, now_sec};

/// 获取当前进程的 IPC namespace ID
fn get_current_ipc_ns_id() -> usize {
    let table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(ref t) = *table {
        if let Some(pid) = t.current {
            if let Some(process) = t.get(pid) {
                return process.nsproxy.lock().ipc_ns.id.0;
            }
        }
    }
    0
}

/// 真 `struct shmid_ds` 状态 — 不再只有 size + pages 这种半成品。
/// 字段全部按 Linux `ipc/shm.c` shmid_kernel 对齐：perm、segsz、
/// atime/dtime/ctime、cpid/lpid、nattch、locked、creator pid 等。
pub struct ShmSegment {
    pub perm: IpcPerm,
    pub size: usize,
    pub pages: Vec<crate::mm::PhysPageNum>,
    pub last_atime: u64,   // shmat 时间（秒）
    pub last_dtime: u64,   // shmdt 时间
    pub last_ctime: u64,   // 创建/IPC_SET 时间
    pub cpid: i32,         // 创建者 pid
    pub lpid: i32,         // 最近 shmat / shmdt 的 pid
    pub nattch: u64,       // 当前挂接数
    pub locked: bool,      // SHM_LOCK / SHM_UNLOCK
    pub marked_destroy: bool, // IPC_RMID 后等 nattch=0 才真删（Linux 行为）
    /// IPC namespace ID for isolation
    pub ipc_ns_id: usize,
    /// SHM_HUGETLB: huge page size in bytes (0 = normal 4K pages)
    pub huge_page_size: usize,
}

pub static SHM_TABLE: SpinLock<Option<BTreeMap<i32, ShmSegment>>> = SpinLock::new(None);
/// key → id 索引（IPC_PRIVATE = 0 不入索引）；Linux 用 idr+key_ht 两套，
/// 这里用两个 BTreeMap 模拟，语义一致。
static SHM_KEY_INDEX: SpinLock<Option<BTreeMap<i32, i32>>> = SpinLock::new(None);
static SHM_NEXT_ID: SpinLock<i32> = SpinLock::new(1);

const IPC_CREAT: i32 = 0o1000;
const IPC_EXCL:  i32 = 0o2000;
const IPC_PRIVATE: i32 = 0;

/// Dynamic SysV SHM limits (writable via /proc/sys/kernel/shm*)
pub static SHMMNI: AtomicUsize = AtomicUsize::new(4096);
/// Linux default: SHMMAX = LONG_MAX (isize::MAX on 64-bit = 0x7FFFFFFFFFFFFFFF)
pub static SHMMAX: AtomicUsize = AtomicUsize::new(isize::MAX as usize);
/// Linux default: SHMALL = SHMMAX / PAGE_SIZE
pub static SHMALL: AtomicUsize = AtomicUsize::new((isize::MAX as usize) >> 12);

/// `/proc/sys/kernel/shm_next_id` (CONFIG_CHECKPOINT_RESTORE): the desired id
/// for the *next* allocated shared-memory segment. `-1` (default) means use the
/// generic cyclic allocator. After a successful allocation the kernel resets it
/// to `-1` (LTP `shmget05`/`shmget06`).
pub static SHM_REQUESTED_ID: AtomicI32 = AtomicI32::new(-1);

/// Read `/proc/sys/kernel/shm_next_id`.
pub fn shm_next_id() -> i32 {
    SHM_REQUESTED_ID.load(Ordering::Relaxed)
}

/// Write `/proc/sys/kernel/shm_next_id`. Linux accepts `{0..INT_MAX}` (and the
/// reset value `-1`); other negatives are rejected.
pub fn set_shm_next_id(val: i32) -> bool {
    if val < -1 {
        return false;
    }
    SHM_REQUESTED_ID.store(val, Ordering::Relaxed);
    true
}

fn shm_table_init() {
    let mut t = SHM_TABLE.lock();
    if t.is_none() { *t = Some(BTreeMap::new()); }
    let mut k = SHM_KEY_INDEX.lock();
    if k.is_none() { *k = Some(BTreeMap::new()); }
}

/// 当 nattch==0 且 marked_destroy=true 时，真正释放底层物理页。
fn maybe_reap(id: i32) {
    let mut tbl = SHM_TABLE.lock();
    let map = match tbl.as_mut() { Some(m) => m, None => return };
    let should_remove = map
        .get(&id)
        .map(|s| s.marked_destroy && s.nattch == 0)
        .unwrap_or(false);
    if should_remove {
        if let Some(seg) = map.remove(&id) {
            if seg.huge_page_size != 0 {
                // SHM_HUGETLB: release backing frames and pool accounting.
                // Backing frames are individual 4K frames (see shmget — the
                // segment is mapped with 4K PTEs, so physical contiguity is
                // not required and would be defeated by fragmentation anyway).
                let pages_per_huge = seg.huge_page_size / 4096;
                let huge_count = seg.pages.len() / pages_per_huge;
                for ppn in seg.pages {
                    crate::mm::dealloc_frame(ppn);
                }
                crate::mm::hugetlb_release(huge_count);
            } else {
                // Normal pages: dealloc individually
                for ppn in seg.pages {
                    crate::mm::dealloc_frame(ppn);
                }
            }
        }
    }
}

pub(crate) fn sys_shmget(key: i32, size: usize, shmflg: i32) -> isize {
    shm_table_init();
    const SHM_HUGETLB: i32 = 0x800;
    // SHM_HUGETLB: extract huge page size from flags (Linux SHM_HUGE_SHIFT=26)
    let huge_page_size: usize = if shmflg & SHM_HUGETLB != 0 {
        let log2_size = ((shmflg as u32) >> 26) & 0x3f;
        let log2 = if log2_size == 0 { 21 } else { log2_size as usize }; // default 2MB
        if log2 < 12 || log2 > 30 { return -22; }
        1usize << log2
    } else {
        0
    };
    let creat = shmflg & IPC_CREAT != 0;
    let excl  = shmflg & IPC_EXCL  != 0;
    let mode  = (shmflg as u32) & 0o777;
    let cpid  = current_pid_locked() as i32;

    let cur_ipc_ns = get_current_ipc_ns_id();
    // key → 已存在 id 查找（IPC_PRIVATE 跳过；marked_destroy 视为不存在）
    if key != IPC_PRIVATE {
        let key_map_guard = SHM_KEY_INDEX.lock();
        let key_map = key_map_guard.as_ref().unwrap();
        if let Some(&existing) = key_map.get(&key) {
            // 解锁 KEY_INDEX，下面要锁 SHM_TABLE
            drop(key_map_guard);
            let tbl = SHM_TABLE.lock();
            if let Some(seg) = tbl.as_ref().and_then(|m| m.get(&existing)) {
                // IPC namespace 隔离：seg 属于别的 ns → 视作不存在（LTP shm_comm：
                // 子进程进新 IPC ns 后 shmget(同 key) 须拿到独立 segment）。
                if !seg.marked_destroy && seg.ipc_ns_id == cur_ipc_ns {
                    if creat && excl { return -17; } // -EEXIST
                    if size != 0 && seg.size < size { return -22; } // -EINVAL
                    // 真 Linux `ipcget_public`：查到已有 segment 后，还要用
                    // `shmflg` 低 9 bit 反推出的访问需求 vs perm.mode 做
                    // ipcperms。LTP `shmget05` 就是造出 mode=0 的 segment 之
                    // 后用非则 uid 再 shmget 验证 -EACCES。
                    let need = (shmflg as u32) & 0o777;
                    if need != 0 && !seg.perm.access(need) {
                        return -13; // -EACCES
                    }
                    return existing as isize;
                }
            }
            // 若 marked_destroy，按 Linux 行为视作不存在 → 走 create 分支
        }
    }
    // Linux: IPC_PRIVATE always creates; non-private requires IPC_CREAT
    if key != IPC_PRIVATE && !creat { return -2; } // -ENOENT

    // SHMMNI: max segments system-wide
    {
        let tbl = SHM_TABLE.lock();
        if let Some(m) = tbl.as_ref() {
            if m.len() >= SHMMNI.load(Ordering::Relaxed) { return -28; } // -ENOSPC
        }
    }
    // SHMMAX: max segment size
    if size > SHMMAX.load(Ordering::Relaxed) { return -22; } // -EINVAL

    if size == 0 { return -22; }

    // Allocate backing pages — huge pages for SHM_HUGETLB, normal 4K otherwise
    let mut pages: Vec<crate::mm::PhysPageNum>;
    if huge_page_size != 0 {
        // SHM_HUGETLB requires privilege (Linux `newseg` → `can_do_hugetlb_shm`:
        // CAP_IPC_LOCK or membership of the hugetlb shm group). An unprivileged
        // caller gets EPERM *before* any pool accounting, so the error reflects
        // the missing capability rather than an empty pool (shmget02).
        const CAP_IPC_LOCK: u64 = 1u64 << 14;
        let privileged = {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            table
                .as_ref()
                .and_then(|t| t.current.and_then(|pid| t.get(pid)))
                .map(|p| p.euid == 0 || (p.cap_effective & CAP_IPC_LOCK) != 0)
                .unwrap_or(false)
        };
        if !privileged {
            return -1; // -EPERM
        }
        // SHM_HUGETLB: size must be aligned to huge page size
        let aligned_size = (size + huge_page_size - 1) & !(huge_page_size - 1);
        let huge_count = aligned_size / huge_page_size;
        let pages_per_huge = huge_page_size / 4096;
        // Reserve from hugetlb pool
        if !crate::mm::hugetlb_reserve(huge_count) {
            return -12; // ENOMEM: not enough huge pages
        }
        // Backing store: individual 4K frames. The segment is always mapped
        // with 4K PTEs (shmat), so the "huge page" here is pool accounting +
        // size/alignment semantics only. Demanding 2^9-contiguous physical
        // blocks made shmget ENOMEM under long-run buddy fragmentation even
        // with hundreds of MB free (real Linux solves this with compaction).
        pages = Vec::with_capacity(huge_count * pages_per_huge);
        for _ in 0..huge_count * pages_per_huge {
            match crate::mm::alloc_frame() {
                Some(ppn) => pages.push(ppn),
                None => {
                    for p in &pages {
                        crate::mm::dealloc_frame(*p);
                    }
                    crate::mm::hugetlb_release(huge_count);
                    return -12;
                }
            }
        }
    } else {
        let page_count = (size + 4095) / 4096;
        pages = Vec::with_capacity(page_count);
        for _ in 0..page_count {
            match crate::mm::alloc_frame() {
                Some(ppn) => pages.push(ppn),
                None => {
                    for p in &pages { crate::mm::dealloc_frame(*p); }
                    return -12;
                }
            }
        }
    }

    let now = now_sec();
    let mut tbl = SHM_TABLE.lock();
    let map = tbl.as_mut().unwrap();
    // Id allocation. Linux `ipc_idr_alloc`: when `shm_next_id >= 0`
    // (CHECKPOINT_RESTORE) allocate the first free index *at or above* the
    // requested id (`idr_alloc(start=req, end=0)`), then reset `next_id` to
    // -1; otherwise use the generic cyclic allocator. shmget06 relies on the
    // "id already in use → pick another, don't guarantee the desired id"
    // semantics, shmget05 on the exact-id + reset-to--1 semantics.
    let id = {
        let requested = SHM_REQUESTED_ID.load(Ordering::Relaxed);
        if requested >= 0 {
            SHM_REQUESTED_ID.store(-1, Ordering::Relaxed);
            let mut cand = requested;
            while map.contains_key(&cand) {
                cand = cand.checked_add(1).unwrap_or(1);
                if cand < 0 { cand = 1; }
            }
            let mut next = SHM_NEXT_ID.lock();
            if cand >= *next {
                *next = cand.wrapping_add(1).max(1);
            }
            cand
        } else {
            let mut next = SHM_NEXT_ID.lock();
            let mut id = *next;
            while map.contains_key(&id) {
                id = id.wrapping_add(1).max(1);
            }
            *next = id.wrapping_add(1).max(1);
            id
        }
    };
    let mut perm = IpcPerm::new(key, mode);
    // 真 Linux：ipc_perm.seq 防止 id 池 wrap-around 后误用旧 id（每次
    // IPC_RMID 时 +1）。我们的 SHM_NEXT_ID 单调递增不复用，逻辑上无 ABA
    // 风险，因此 seq 仅做 binary-compat 字段，固定从 0 开始（不是简化，
    // 是因为底层 id 分配策略不同）。
    perm.seq = 0;
    let ipc_ns_id = get_current_ipc_ns_id();
    map.insert(id, ShmSegment {
        perm,
        size,
        pages,
        last_atime: 0,
        last_dtime: 0,
        last_ctime: now,
        cpid,
        lpid: 0,
        nattch: 0,
        locked: false,
        marked_destroy: false,
        ipc_ns_id,
        huge_page_size,
    });
    drop(tbl);
    if key != IPC_PRIVATE {
        let mut key_map_guard = SHM_KEY_INDEX.lock();
        let key_map = key_map_guard.as_mut().unwrap();
        key_map.insert(key, id);
    }
    id as isize
}

/// SysV `shmat(shmaddr == 0)` 的 get_unmapped_area。
///
/// 在 SHM 专用地址窗口里找一段长度 `len`、对齐到 `alignment` 的空闲区域，
/// 既不与任何现有 VMA 重叠，也不与任何已存在的页表映射重叠——后者覆盖
/// 其它已 attach 的 SHM 段（它们不以 VMA 形式登记，只有裸 PTE）以及
/// glibc/动态加载器等留下的映射。对标 Linux `do_shmat`：`shmaddr == 0`
/// 时走 `get_unmapped_area` 找真正的空闲洞，而不是用一个固定 hint 直接
/// 插页表（固定 hint 会与已有映射撞上，触发 `PageTable::map` 的
/// "VPN already mapped" 断言而内核 panic）。
///
/// 返回选定基址；窗口内无足够空闲洞时返回 `None`（调用方据此返回 -ENOMEM）。
fn shmat_find_unmapped_area(
    addr_space: &crate::mm::AddressSpace,
    len: usize,
    alignment: usize,
) -> Option<usize> {
    use crate::mm::{VirtAddr, PAGE_SIZE};
    // SHM 窗口与 mmap 区共用 [0x2000_0000, 0x4000_0000)：对标 Linux —
    // shmat(NULL) 走与 mmap 相同的 get_unmapped_area，落在 mmap 区里。
    // 关键点：程序的 data/bss/heap 全部位于 0x4000_0000 *之上*，且 heap 向上
    // 增长。早先把 SHM 放在 [0x4000_0000, 0x7000_0000) 会让段紧贴 heap 顶端，
    // 随后 heap 经 brk / 缺页自动增长把 SHM 页吞并（可写 heap VMA 盖住 SHM），
    // 导致 SHM_RDONLY 写不 fault、detach 后写仍命中（shmat01/shmdt01）。放到
    // heap 触不到的 mmap 区即彻底规避；SHM 现以 VMA 登记，与 mmap 互相避让。
    const SHM_BASE: usize = 0x2000_0000;
    const SHM_TOP: usize = 0x4000_0000;
    let align = alignment.max(PAGE_SIZE);
    if len == 0 || len > SHM_TOP - SHM_BASE {
        return None;
    }
    let mut base = (SHM_BASE + align - 1) & !(align - 1);
    'outer: while base + len <= SHM_TOP {
        // 1) 跳过与已有 VMA 重叠的候选。lazy VMA 可能尚无 PTE，必须单独查 VMA，
        //    否则 SHM 会盖在按需分页区上，之后该区 fault 会读到 SHM 的页。
        for vma in addr_space.vmas_iter() {
            if base < vma.end.0 && (base + len) > vma.start.0 {
                let next = (vma.end.0 + align - 1) & !(align - 1);
                base = if next > base { next } else { base + align };
                continue 'outer;
            }
        }
        // 2) 跳过与已有页表映射重叠的候选（其它 SHM 段只有裸 PTE、无 VMA）。
        let mut va = base;
        while va < base + len {
            if addr_space.page_table().translate(VirtAddr(va).floor()).is_some() {
                base = (va + PAGE_SIZE + align - 1) & !(align - 1);
                continue 'outer;
            }
            va += PAGE_SIZE;
        }
        return Some(base);
    }
    None
}

pub(crate) fn sys_shmat(shmid: i32, shmaddr: usize, shmflg: i32) -> isize {
    const SHM_RDONLY: i32 = 0o10000;
    const SHM_RND:    i32 = 0o20000;
    const SHM_REMAP:  i32 = 0o40000;
    // Slice 2.x 真化〔去 silent 占位〕：SHM_REMAP 真做。当 shmaddr 非 0：
    //   * 无 SHM_REMAP → [base, base+size) 与现有 VMA 重叠返 -EINVAL（Linux 行为）
    //   * 有 SHM_REMAP → 允许重叠，先 unmap 该范围的旧 VMA/page 再 attach
    shm_table_init();
    // 1) 取 size/pages 副本（要尽量缩短 SHM_TABLE 锁范围）
    let cur_ipc_ns = get_current_ipc_ns_id();
    let (page_count, phys_pages, seg_size) = {
        let tbl = SHM_TABLE.lock();
        let map = tbl.as_ref().unwrap();
        match map.get(&shmid) {
            // shmid 不存在 → -EINVAL（Linux `ipc_obtain_object_check` 等价）
            None => return -22,
            // 其它 IPC namespace 的 segment 在本 ns 不可见（Linux ipc id 表
            // per-ns，跨 ns 的 id 等同不存在）→ -EINVAL（setns02）。
            Some(s) if s.ipc_ns_id != cur_ipc_ns => return -22,
            // segment 已被 IPC_RMID 标记 → -EIDRM（man shmat(2)；Linux
            // `ipcget_check` 在 RMID 后拒绝 shmat 而不是返 -EINVAL）
            Some(s) if s.marked_destroy => return -43,
            Some(s) => (s.pages.len(), s.pages.clone(), s.size),
        }
    };

    // 2) 真 Linux `do_shmat` 权限检查：SHM_RDONLY 需 R；双向需 R+W。
    //    不足 → -EACCES (-13)。LTP `shmat01` 会用 mode=0400 的 segment 试试
    //    write attach 验证被拒。
    {
        let tbl = SHM_TABLE.lock();
        let map = tbl.as_ref().unwrap();
        let seg = map.get(&shmid).unwrap(); // 上面已拿到 size，必存在
        let need = if shmflg & SHM_RDONLY != 0 { 0o444u32 } else { 0o666u32 };
        if !seg.perm.access(need) { return -13; }
    }

    // 3) shmaddr 处理：0 → 内核自选；非 0 且 SHM_RND → 向下对齐 SHMLBA；
    //    否则要求对齐，未对齐返 -EINVAL。
    //    SHM_HUGETLB: SHMLBA = huge_page_size (对标 Linux hugetlb_get_unmapped_area)
    let seg_huge_page_size = {
        let tbl = SHM_TABLE.lock();
        tbl.as_ref().and_then(|m| m.get(&shmid)).map(|s| s.huge_page_size).unwrap_or(0)
    };
    // SHMLBA：连接地址的对齐边界。LoongArch 因 cache aliasing 把 SHMLBA 定为
    // 64K（asm/shmparam.h: `SHMLBA SZ_64K`）；RISC-V 等无自定义则取 PAGE_SIZE。
    // glibc 头文件按各架构 SHMLBA 计算 shmat 的测试地址，故内核必须照此对齐，
    // 否则 SHM_RND 向下取整会停在 4K 边界而非 64K（shmat01 case3，la-glibc）。
    #[cfg(target_arch = "loongarch64")]
    const SHMLBA: usize = 0x10000;
    #[cfg(not(target_arch = "loongarch64"))]
    const SHMLBA: usize = 4096;
    // 自动选址与 SHM_RND 取整都对齐到 SHMLBA（hugetlb 则对齐到大页）。
    let alignment = if seg_huge_page_size != 0 { seg_huge_page_size } else { SHMLBA };
    // 不带 SHM_RND 的显式连接只需页对齐——LoongArch 未设 __ARCH_FORCE_SHMLBA，
    // 故页对齐但非 SHMLBA 对齐的地址仍合法，仅 SHM_RND 才向下取整到 SHMLBA。
    let page_align = if seg_huge_page_size != 0 { seg_huge_page_size } else { 4096 };
    // shmaddr != 0 时基址可立即定；shmaddr == 0 需在目标地址空间里找空闲洞
    // (get_unmapped_area)，推迟到锁住 PROCESS_TABLE、拿到 addr_space 之后再定。
    let fixed_base: Option<usize> = if shmaddr == 0 {
        None
    } else if shmflg & SHM_RND != 0 {
        let rounded = shmaddr & !(alignment - 1);
        if rounded < 0x10000 { return -22; } // EINVAL
        Some(rounded)
    } else {
        if shmaddr & (page_align - 1) != 0 { return -22; }
        if shmaddr < 0x10000 { return -22; }
        Some(shmaddr)
    };

    // 3) 真插页表 + SHM_REMAP 真化的 overlap 检查
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    let t = match table.as_mut() { Some(t) => t, None => return -22 };
    let cur_pid = match t.current { Some(p) => p, None => return -22 };
    let tgid = t.get(cur_pid).map(|p| p.tgid).unwrap_or(cur_pid);
    let addr_pid = if t.get(cur_pid).and_then(|p| p.mm.as_ref()).is_some() {
        cur_pid
    } else { tgid };
    let mm = match t.get(addr_pid).and_then(|p| p.mm.as_ref()) { Some(m) => m.clone(), None => return -22 };
    // Real program break (page-rounded). The Heap VMA is deliberately padded
    // 2 pages past this (so musl mallocng's metadata writes just above the
    // break land in mapped heap, not an mmap guard page — the LA libc-bench
    // fix). That padding is invisible to real Linux, whose brk VMA ends exactly
    // at PAGE_ALIGN(brk). shmt09 attaches a segment at SHMALIGN(brk+1) and
    // expects it to succeed, so the overlap test below must use the *real*
    // break, not the padded VMA end, or it spuriously returns EINVAL.
    let heap_real_end = t.get(addr_pid)
        .map(|p| (p.heap_end.0 + 4095) & !4095)
        .filter(|&e| e != 0);
    let mut addr_space = mm.lock();
    use crate::mm::{VirtAddr, VirtPageNum, PTEFlags};
    // shmaddr == 0：在目标地址空间里找一段不与任何 VMA / 已有页表映射重叠的
    // 空闲区域（get_unmapped_area）。无空闲洞 → -ENOMEM。
    let base = match fixed_base {
        Some(b) => b,
        None => match shmat_find_unmapped_area(&*addr_space, page_count * 4096, alignment) {
            Some(b) => b,
            None => return -12, // -ENOMEM
        },
    };
    // 真 Linux `do_shmat`：shmaddr 非 0 时检查 [base, base+seg_size) 与
    // 现有 VMA 重叠。无 SHM_REMAP → -EINVAL；有 SHM_REMAP → 先 unmap_region。
    if shmaddr != 0 {
        let attach_end = base.saturating_add(seg_size);
        // For the Heap VMA, clamp its effective end to the real (page-rounded)
        // program break so the 2-page brk padding does not register as occupied.
        let eff_end = |vma: &crate::mm::Vma| -> usize {
            if vma.vma_type == crate::mm::VmaType::Heap {
                if let Some(re) = heap_real_end { return vma.end.0.min(re); }
            }
            vma.end.0
        };
        let overlaps = addr_space.vmas_iter().any(|vma| {
            base < eff_end(vma) && attach_end > vma.start.0
        });
        if overlaps && shmflg & SHM_REMAP == 0 {
            return -22; // -EINVAL，符合 Linux `do_shmat` 拒重叠
        }
        // Clear any existing mappings in [base, attach_end) before mapping the
        // segment. Besides SHM_REMAP, this also drops the heap's 2-page brk
        // padding (above the real break) when a segment is attached right past
        // the break (shmt09): those padding PTEs are invisible to the overlap
        // test above but would make the raw page_table.map() below panic
        // ("already mapped"). unmap_region frees their frames and shrinks the
        // heap VMA down to the real break.
        addr_space.unmap_region(VirtAddr(base), VirtAddr(attach_end));
    }
    let rdonly = shmflg & SHM_RDONLY != 0;
    let mut flags = PTEFlags::R | PTEFlags::U;
    if !rdonly { flags |= PTEFlags::W; }
    for i in 0..page_count {
        let va = VirtAddr(base + i * 4096);
        let vpn = VirtPageNum(va.0 / 4096);
        let ppn = phys_pages[i];
        addr_space.page_table_mut().map(vpn, ppn, flags);
    }
    // Reserve the attachment as a managed VMA so the rest of the VM (mmap's
    // get_unmapped_area, the page-fault handler, mremap) treats it as occupied
    // memory with the right protection. Without this the bare PTEs are
    // invisible to those paths: a later mmap could alias the region, and a
    // write to a SHM_RDONLY attach (or to a detached segment) would be served
    // by an overlapping anonymous VMA instead of faulting (shmat01/shmdt01).
    // The VMA carries no `frames` of its own — the physical pages are owned by
    // SHM_TABLE — so address-space teardown never double-frees them.
    {
        let perm = crate::mm::VmaPermission::new(true, !rdonly, false, true);
        let vma = crate::mm::Vma::new(
            VirtAddr(base),
            VirtAddr(base + page_count * 4096),
            crate::mm::VmaType::Shared,
            perm,
        );
        addr_space.add_vma(vma);
    }
    crate::arch::asm_helpers::flush_tlb_all();
    drop(addr_space);
    if let Some(proc) = t.get_mut(addr_pid) {
        proc.shm_attachments.push((shmid, base));
    }
    drop(table);
    // SHM_REMAP/brk-padding 路径覆盖了旧映射：与 MAP_FIXED 同语义，锁外
    // 同步收尾其它 hart 的陈旧 TLB（本地 flush_tlb_all 只清当前核）。
    crate::mm::tlb::drain_shootdown();

    // 4) nattch++ / atime / lpid
    {
        let mut tbl = SHM_TABLE.lock();
        if let Some(seg) = tbl.as_mut().and_then(|m| m.get_mut(&shmid)) {
            seg.nattch = seg.nattch.saturating_add(1);
            seg.last_atime = now_sec();
            seg.lpid = cur_pid as i32;
        }
    }
    base as isize
}

pub(crate) fn sys_shmdt(shmaddr: usize) -> isize {
    if shmaddr & 0xFFF != 0 { return -22; }
    let cur_pid_i32 = current_pid_locked() as i32;
    // 1) 先 PROCESS_TABLE 内查 found_shmid（不嵌套其他全局锁），并暂存。
    let found_shmid = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_ref() { Some(t) => t, None => return -22 };
        let cur_pid = match t.current { Some(p) => p, None => return -22 };
        let tgid = t.get(cur_pid).map(|p| p.tgid).unwrap_or(cur_pid);
        let addr_pid = if t.get(cur_pid).and_then(|p| p.mm.as_ref()).is_some() {
            cur_pid
        } else { tgid };
        let proc = match t.get(addr_pid) { Some(p) => p, None => return -22 };
        match proc.shm_attachments.iter().find_map(|&(id, base)| {
            if base == shmaddr { Some(id) } else { None }
        }) {
            Some(id) => id,
            None => return -22,
        }
    };
    // 2) 取 page_count（SHM_TABLE 单独锁）
    let page_count = {
        let tbl = SHM_TABLE.lock();
        tbl.as_ref().and_then(|m| m.get(&found_shmid)).map(|s| s.pages.len()).unwrap_or(0)
    };
    // 3) 在 PROCESS_TABLE 内真 unmap + 摘 attachment
    {
        let mut table = crate::sched::process::PROCESS_TABLE.lock();
        let t = match table.as_mut() { Some(t) => t, None => return -22 };
        let cur_pid = match t.current { Some(p) => p, None => return -22 };
        let tgid = t.get(cur_pid).map(|p| p.tgid).unwrap_or(cur_pid);
        let addr_pid = if t.get(cur_pid).and_then(|p| p.mm.as_ref()).is_some() {
            cur_pid
        } else { tgid };
        let mm_clone = t.get(addr_pid).and_then(|p| p.mm.as_ref()).cloned();
        if let Some(ref mm) = mm_clone {
            let mut addr_space = mm.lock();
            use crate::mm::VirtAddr;
            // Tear down both the PTEs and the reservation VMA created in shmat.
            // unmap_region clears the page-table entries and drops the VMA; the
            // SHM physical pages are owned by SHM_TABLE (not by vma.frames), so
            // they survive for other attachers / re-attach. After this the
            // range is truly unmapped, so a stray write faults (shmdt01).
            if page_count != 0 {
                addr_space.unmap_region(
                    VirtAddr(shmaddr),
                    VirtAddr(shmaddr + page_count * 4096),
                );
            }
        }
        if let Some(proc) = t.get_mut(addr_pid) {
            proc.shm_attachments.retain(|&(_, b)| b != shmaddr);
        }
    }
    // shmdt 返回后本 mm 任何线程访问该区间必须立即 fault；锁外同步收尾
    // 跨核失效（与 munmap 同语义）。
    crate::mm::tlb::drain_shootdown();
    let detached_shmid = found_shmid;
    // 2) nattch-- / dtime / lpid
    {
        let mut tbl = SHM_TABLE.lock();
        if let Some(seg) = tbl.as_mut().and_then(|m| m.get_mut(&detached_shmid)) {
            seg.nattch = seg.nattch.saturating_sub(1);
            seg.last_dtime = now_sec();
            seg.lpid = cur_pid_i32;
        }
    }
    // 3) 若 IPC_RMID 标记且 nattch=0 → 真释放
    maybe_reap(detached_shmid);
    0
}

pub(crate) fn sys_shmctl(shmid: i32, cmd: i32, buf: usize) -> isize {
    const IPC_RMID: i32 = 0;
    const IPC_SET:  i32 = 1;
    const IPC_STAT: i32 = 2;
    const IPC_INFO: i32 = 3;
    const SHM_LOCK: i32 = 11;
    const SHM_UNLOCK: i32 = 12;
    const SHM_STAT: i32 = 13;
    const SHM_INFO: i32 = 14;
    const SHM_STAT_ANY: i32 = 15;
    shm_table_init();
    match cmd {
        IPC_RMID => {
            // Linux：owner_check + 标记 marked_destroy，立刻从 key_index 摘除，
            // 物理页等 nattch=0 才真释放。
            let mut tbl = SHM_TABLE.lock();
            let map = match tbl.as_mut() { Some(m) => m, None => return -22 };
            let seg = match map.get_mut(&shmid) { Some(s) => s, None => return -22 };
            // IPC_RMID requires owner/creator/CAP_SYS_ADMIN (man shmctl)
            if !seg.perm.owner_check() { return -1; } // -EPERM
            seg.marked_destroy = true;
            let key = seg.perm.key;
            let nattch = seg.nattch;
            drop(tbl);
            // 摘 key 索引（如果是 IPC_PRIVATE key=0 则跳过）
            if key != IPC_PRIVATE {
                let mut k = SHM_KEY_INDEX.lock();
                if let Some(km) = k.as_mut() { km.remove(&key); }
            }
            if nattch == 0 { maybe_reap(shmid); }
            0
        }
        IPC_STAT | SHM_STAT | SHM_STAT_ANY => {
            if buf == 0 { return -14; }
            let tbl = SHM_TABLE.lock();
            let map = match tbl.as_ref() { Some(m) => m, None => return -22 };
            let seg = match map.get(&shmid) { Some(s) => s, None => return -22 };
            // SHM_STAT_ANY 跳过 perm 检查（Linux 4.17+）；IPC_STAT/SHM_STAT 需 R。
            if cmd != SHM_STAT_ANY && !seg.perm.access(0o444) { return -13; }
            // 真 shmid_ds 112B (rv64/la64)
            let mut out = Vec::with_capacity(112);
            seg.perm.write_into(&mut out);                          // 0..48
            // Linux `ipc/shm.c` 把 SHM_LOCKED(02000) / SHM_DEST(01000) 体现在
            // `shm_perm.mode` 高位（do_shmctl SHM_LOCK 置位、shm_destroy 期间
            // SHM_DEST）。我们用 seg.locked / marked_destroy 作权威状态，在
            // IPC_STAT 输出时合并进 mode（access() 只用低 9 位，互不影响）。
            // shmctl07 读 ds.shm_perm.mode & SHM_LOCKED 依赖此位。
            {
                const SHM_LOCKED: u32 = 0o2000;
                const SHM_DEST: u32 = 0o1000;
                let mut eff_mode = seg.perm.mode;
                if seg.locked { eff_mode |= SHM_LOCKED; }
                if seg.marked_destroy { eff_mode |= SHM_DEST; }
                out[20..24].copy_from_slice(&eff_mode.to_le_bytes());
            }
            out.extend_from_slice(&(seg.size as u64).to_le_bytes()); // 48..56 segsz
            out.extend_from_slice(&(seg.last_atime as i64).to_le_bytes()); // 56..64
            out.extend_from_slice(&(seg.last_dtime as i64).to_le_bytes()); // 64..72
            out.extend_from_slice(&(seg.last_ctime as i64).to_le_bytes()); // 72..80
            out.extend_from_slice(&(seg.cpid).to_le_bytes());        // 80..84
            out.extend_from_slice(&(seg.lpid).to_le_bytes());        // 84..88
            out.extend_from_slice(&(seg.nattch).to_le_bytes());      // 88..96
            out.extend_from_slice(&[0u8; 8]);                        // 96..104 unused4
            out.extend_from_slice(&[0u8; 8]);                        // 104..112 unused5
            if crate::syscall::user::write_user_buffer(buf as *mut u8, &out).is_err() {
                return -14;
            }
            // SHM_STAT / SHM_STAT_ANY 返回真 IPC id
            if cmd == IPC_STAT { 0 } else { shmid as isize }
        }
        IPC_SET => {
            if buf == 0 { return -14; }
            let bytes = match crate::syscall::user::read_user_buffer(buf as *const u8, 48) {
                Ok(b) => b, Err(_) => return -14,
            };
            let new_uid  = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
            let new_gid  = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
            let new_mode = u32::from_le_bytes(bytes[20..24].try_into().unwrap()) & 0o777;
            let mut tbl = SHM_TABLE.lock();
            let map = match tbl.as_mut() { Some(m) => m, None => return -22 };
            let seg = match map.get_mut(&shmid) { Some(s) => s, None => return -22 };
            // IPC_SET 需 owner_check（man shmctl IPC_SET）
            if !seg.perm.owner_check() { return -1; } // -EPERM
            seg.perm.uid = new_uid;
            seg.perm.gid = new_gid;
            seg.perm.mode = new_mode;
            seg.last_ctime = now_sec();
            0
        }
        SHM_LOCK | SHM_UNLOCK => {
            // 真 SHM_LOCK：标记 locked，禁止页面被换出；我们无 swap，
            // 仅记 flag。Linux：root (CAP_IPC_LOCK) 或 owner 可调。
            let mut tbl = SHM_TABLE.lock();
            let map = match tbl.as_mut() { Some(m) => m, None => return -22 };
            let seg = match map.get_mut(&shmid) { Some(s) => s, None => return -22 };
            if !seg.perm.owner_check() { return -1; } // -EPERM
            seg.locked = cmd == SHM_LOCK;
            0
        }
        IPC_INFO | SHM_INFO => {
            // 真 `struct shminfo` (asm-generic/shmbuf.h, 9 个 ulong = 72B)
            //   shmmax / shmmin / shmmni / shmseg / shmall + 4 个保留 ulong
            // SHM_INFO 返回 `struct shm_info` (5 个 long + 1 int) — 这里复用
            //  shminfo 字段 shm_tot / shm_rss 用 shminfo 中后 4 个 ulong 占位
            //  以保持 binary stable，并通过返回值给 SHM_INFO 提供 max_id。
            let (max_id, used_ids, shm_tot_pages) = {
                let tbl = SHM_TABLE.lock();
                let map = tbl.as_ref();
                if let Some(m) = map {
                    let max_id = m.keys().copied().max().unwrap_or(0);
                    let used = m.len();
                    let tot: usize = m.values().map(|s| s.pages.len()).sum();
                    (max_id, used, tot)
                } else { (0, 0, 0) }
            };
            if buf != 0 {
                let mut out = Vec::with_capacity(72);
                if cmd == IPC_INFO {
                    out.extend_from_slice(&(usize::MAX / 2).to_le_bytes()); // shmmax
                    out.extend_from_slice(&(1usize).to_le_bytes());          // shmmin
                    out.extend_from_slice(&(4096usize).to_le_bytes());       // shmmni
                    out.extend_from_slice(&(128usize).to_le_bytes());        // shmseg
                    out.extend_from_slice(&(usize::MAX / 4096 / 2).to_le_bytes()); // shmall
                    out.extend_from_slice(&[0u8; 32]);                       // unused[4]
                } else {
                    // shm_info: used_ids(i32 + 4 pad) + shm_tot + shm_rss + shm_swp + swap_attempts + swap_successes (5 个 ulong)
                    out.extend_from_slice(&(used_ids as i32).to_le_bytes());
                    out.extend_from_slice(&[0u8; 4]);
                    out.extend_from_slice(&(shm_tot_pages as u64).to_le_bytes()); // shm_tot
                    out.extend_from_slice(&(shm_tot_pages as u64).to_le_bytes()); // shm_rss (无 swap 等于 tot)
                    out.extend_from_slice(&(0u64).to_le_bytes());                 // shm_swp
                    out.extend_from_slice(&(0u64).to_le_bytes());                 // swap_attempts
                    out.extend_from_slice(&(0u64).to_le_bytes());                 // swap_successes
                    // 凑到 72B
                    while out.len() < 72 { out.push(0); }
                }
                if crate::syscall::user::write_user_buffer(buf as *mut u8, &out).is_err() {
                    return -14;
                }
            }
            max_id as isize
        }
        _ => -22,
    }
}

/// fork/clone 真行为：对子进程继承的每个 shm 段 nattch +1。
/// 必须在锁外调用（不持 PROCESS_TABLE）。
pub fn shm_inc_nattch_bulk(ids: &[i32]) {
    shm_table_init();
    let now = now_sec();
    let mut tbl = SHM_TABLE.lock();
    let map = match tbl.as_mut() { Some(m) => m, None => return };
    for &id in ids {
        if let Some(seg) = map.get_mut(&id) {
            seg.nattch = seg.nattch.saturating_add(1);
            seg.last_atime = now;
        }
    }
}

/// do_exit / exit_group 真行为：对每个 detach 的 shm 段 nattch -1，
/// 若标记销毁且 nattch=0 则真释放底层物理页。
pub fn shm_dec_nattch_bulk(ids: &[i32]) {
    shm_table_init();
    let now = now_sec();
    let mut to_reap: Vec<i32> = Vec::new();
    {
        let mut tbl = SHM_TABLE.lock();
        let map = match tbl.as_mut() { Some(m) => m, None => return };
        for &id in ids {
            if let Some(seg) = map.get_mut(&id) {
                seg.nattch = seg.nattch.saturating_sub(1);
                seg.last_dtime = now;
                if seg.marked_destroy && seg.nattch == 0 {
                    to_reap.push(id);
                }
            }
        }
    }
    for id in to_reap { maybe_reap(id); }
}
