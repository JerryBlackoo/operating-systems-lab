//! 基于代数（generation）的 ASID 分配器（单核）
//!
//! Generation-based ASID allocator (single-hart).
//!
//! 以"页表根物理地址"（root_paddr）为键分配 ASID：共享同一地址空间的线程
//! 天然共享同一 root_paddr → 共享同一 ASID。其目的是消除跨进程上下文切换时
//! 对整个 TLB 的全量失效——不同地址空间获得不同 ASID，其 TLB 条目互不别名，
//! 切换时无需全刷，仅在 ASID 复用（代数回绕）时才全刷一次。
//!
//! Keyed by page-table root physical address so threads sharing an address
//! space (same root) share one ASID. This avoids the full TLB flush on every
//! cross-process context switch: distinct address spaces get distinct ASIDs,
//! so their TLB entries don't alias and need not be flushed on switch.
//!
//! ## 正确性（单核）/ Correctness (single-hart)
//! - 某个 ASID 数值只有在**代数回绕**后才被复用，而回绕会全刷整个 TLB，
//!   因此复用前旧条目必被清除。
//! - 地址空间销毁时调用 [`forget`]，从映射中删除该 root_paddr（并返回其
//!   当前代 ASID 供调用方失效该 ASID 的残留条目），使被回收复用的 root_paddr
//!   绝不会继承旧地址空间的 ASID。
//! - `next` 单调递增直到回绕，故同一代内不会把同一 ASID 发给两个地址空间。

use crate::sync::SpinLock;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// LoongArch 为 10 位 ASID（0 保留为"未分配/无"）。RISC-V Sv39 支持更宽的
/// ASID，但此处统一上限以便一套分配器同时服务两个架构；回绕极少发生
/// （需同时存活 ≥1023 个地址空间才回绕一次）。
const MAX_ASID: u16 = 1023;

struct AsidState {
    /// root_paddr -> 打包 token：(generation << 16) | asid
    map: BTreeMap<u64, u64>,
    /// 当前代内下一个可分配 ASID
    next: u16,
    /// 代数，回绕时自增
    generation: u32,
}

static STATE: SpinLock<Option<AsidState>> = SpinLock::new(None);

/// 当前代数的免锁副本（与 `AsidState::generation` 同步，仅回绕时变）。
/// 供 [`activate_cached`] 快路径免锁校验缓存代数。
static CUR_GEN: AtomicU32 = AtomicU32::new(0);

/// 直接映射的 activate 结果缓存：槽位 = root_ppn % 16，每槽一个 u64 打包
/// `[root_ppn:28][asid:10][gen低26位:26]`。上下文切换每次都要 activate，
/// 锁+BTreeMap 查找在 lat_ctx 热路径上每次切换 ~90 条指令；命中缓存降到
/// 几条。单条 u64 原子读写无撞伴风险；`forget` 时清槽，回绕时靠 gen 失配。
const ACT_CACHE_SLOTS: usize = 16;
static ACT_CACHE: [AtomicU64; ACT_CACHE_SLOTS] = [const { AtomicU64::new(0) }; ACT_CACHE_SLOTS];

const GEN_BITS: u64 = 26;
const GEN_MASK: u64 = (1 << GEN_BITS) - 1;
const ASID_SHIFT: u64 = GEN_BITS;
const PPN_SHIFT: u64 = GEN_BITS + 10;
const PPN_MAX: u64 = (1 << 28) - 1;

#[inline]
fn with_state<R>(f: impl FnOnce(&mut AsidState) -> R) -> R {
    let mut guard = STATE.lock();
    if guard.is_none() {
        *guard = Some(AsidState {
            map: BTreeMap::new(),
            next: 1, // 0 保留
            generation: 0,
        });
    }
    // SAFETY: 上面已确保为 Some
    f(guard.as_mut().unwrap())
}

#[inline]
fn token(generation: u32, asid: u16) -> u64 {
    ((generation as u64) << 16) | (asid as u64)
}

/// 激活 `root_paddr` 对应的地址空间，返回 `(asid, need_full_flush)`。
///
/// 在上下文切换、运行该地址空间之前调用。`need_full_flush` 仅在代数回绕时
/// 为 true——此时调用方必须全量失效 TLB（`invtlb 0x0` / `sfence.vma`）。
pub fn activate(root_paddr: u64) -> (u16, bool) {
    with_state(|st| {
        let cur_gen = st.generation;
        if let Some(&tok) = st.map.get(&root_paddr) {
            if (tok >> 16) as u32 == cur_gen {
                return ((tok & 0xFFFF) as u16, false);
            }
        }
        // 需要分配一个当前代的新 ASID
        let mut flush = false;
        if st.next > MAX_ASID {
            // 回绕：升代 + 要求调用方全刷
            st.generation = st.generation.wrapping_add(1);
            st.next = 1;
            flush = true;
            CUR_GEN.store(st.generation, Ordering::Relaxed);
        }
        let gen = st.generation;
        let asid = st.next;
        st.next += 1;
        st.map.insert(root_paddr, token(gen, asid));
        (asid, flush)
    })
}

/// [`activate`] 的免锁缓存快路径：命中当前代的缓存直接复用 ASID，
/// 未命中回退 [`activate`] 并回填缓存。语义与 [`activate`] 完全一致。
#[inline]
pub fn activate_cached(root_paddr: u64) -> (u16, bool) {
    let ppn = root_paddr >> 12;
    if ppn == 0 || ppn > PPN_MAX {
        return activate(root_paddr);
    }
    let slot = &ACT_CACHE[(ppn as usize) & (ACT_CACHE_SLOTS - 1)];
    let e = slot.load(Ordering::Relaxed);
    let gen = CUR_GEN.load(Ordering::Relaxed) as u64 & GEN_MASK;
    if e != 0 && (e >> PPN_SHIFT) == ppn && (e & GEN_MASK) == gen {
        return (((e >> ASID_SHIFT) & 0x3FF) as u16, false);
    }
    let (asid, flush) = activate(root_paddr);
    let gen_now = CUR_GEN.load(Ordering::Relaxed) as u64 & GEN_MASK;
    slot.store((ppn << PPN_SHIFT) | ((asid as u64) << ASID_SHIFT) | gen_now, Ordering::Relaxed);
    (asid, flush)
}

/// 返回 `root_paddr` 当前代的 ASID；若它本代未被激活（=无存活 TLB 条目）
/// 则返回 0。用于"改页表后失效该地址空间某 VA"的就地失效。
pub fn peek(root_paddr: u64) -> u16 {
    with_state(|st| {
        if let Some(&tok) = st.map.get(&root_paddr) {
            if (tok >> 16) as u32 == st.generation {
                return (tok & 0xFFFF) as u16;
            }
        }
        0
    })
}

/// 地址空间销毁时删除其映射，使被回收复用的 `root_paddr` 获得全新 ASID。
/// 返回它当前代的 ASID（无则 0），供调用方失效该 ASID 的残留 TLB 条目。
pub fn forget(root_paddr: u64) -> u16 {
    let ppn = root_paddr >> 12;
    if ppn != 0 && ppn <= PPN_MAX {
        let slot = &ACT_CACHE[(ppn as usize) & (ACT_CACHE_SLOTS - 1)];
        if (slot.load(Ordering::Relaxed) >> PPN_SHIFT) == ppn {
            slot.store(0, Ordering::Relaxed);
        }
    }
    with_state(|st| {
        if let Some(tok) = st.map.remove(&root_paddr) {
            if (tok >> 16) as u32 == st.generation {
                return (tok & 0xFFFF) as u16;
            }
        }
        0
    })
}
