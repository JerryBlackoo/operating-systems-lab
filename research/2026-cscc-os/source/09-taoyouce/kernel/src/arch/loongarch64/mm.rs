//! # LoongArch64 页表和内存管理
//!
//! LoongArch64 page table and memory management.
//!
//! ## 来源说明 / Source Attribution
//!
//! PTE 位域定义参考自 PolyHAL 项目（https://github.com/Byte-OS/polyhal）
//! 和《龙芯架构参考手册》卷一。遵循 MIT/Apache-2.0 双协议。
//!
//! PTE bit field definitions adapted from PolyHAL project and
//! "LoongArch Reference Manual Volume 1". Licensed under MIT/Apache-2.0.
//!
//! ## LoongArch64 页表特点 / Page Table Characteristics
//!
//! - **软件管理 TLB**：与 RISC-V 的硬件 page walk 不同，LoongArch64 使用
//!   软件 TLB 重填（Ecode=0x3F 异常触发重填处理程序）
//! - **三级页表**：4KB 页，每级 512 条目（9 位索引），类似 Sv39
//! - **双页目录基址**：PGDL（低半地址空间，用户态）和 PGDH（高半地址空间，内核态）
//! - **ASID 支持**：10 位 ASID，支持按 ASID 精确刷新 TLB
//! - **PWCL/PWCH**：页表遍历控制寄存器，配置各级页表的位宽和偏移
//!
//! ## 三级页表结构（4KB 页）
//!
//! ```text
//! 虚拟地址 [47:0]:
//!   [38:30] PGD 索引 (9 位, 512 条目)  — 第一级
//!   [29:21] PMD 索引 (9 位, 512 条目)  — 第二级
//!   [20:12] PTE 索引 (9 位, 512 条目)  — 第三级
//!   [11:0]  页内偏移 (12 位)
//! ```

#![allow(dead_code)]

use crate::arch::hal_trait::{PageTableOps, MmError, pte_flags};
use alloc::vec::Vec;

// ============================================================================
// 页表常量 / Page Table Constants
// ============================================================================

/// 页大小（4KB）
/// Page size (4KB)
pub const PAGE_SIZE: usize = 4096;

/// 页大小位数
/// Page size bits
pub const PAGE_SIZE_BITS: usize = 12;

/// 每级页表的条目数（4KB 页 → 512 条目）
/// Number of entries per page table level (4KB page → 512 entries)
pub const PTE_PER_PAGE: usize = PAGE_SIZE / core::mem::size_of::<u64>();

/// 页表级数（三级页表）
/// Number of page table levels
pub const PT_LEVELS: usize = 3;

/// 每级页表索引位数（9 位 → 512 条目）
/// Bits per page table level index
pub const PT_INDEX_BITS: usize = 9;

// ============================================================================
// PWCL/PWCH 配置常量 / Page Walk Control Constants
// ============================================================================
//
// 🔴 自己写：PWCL/PWCH 配置
//
// PWCL 格式（龙芯手册 / QEMU cpu-csr.h 实锤，每字段 5 位）：
//   [4:0]   PTbase  — PTE 级别的起始位（12，即页内偏移位数）
//   [9:5]   PTwidth — PTE 级别的位宽（9）
//   [14:10] Dir1base — PMD 级别的起始位（21）
//   [19:15] Dir1width — PMD 级别的位宽（9）
//   [24:20] Dir2base — 未使用（0）
//   [29:25] Dir2width — 未使用（0）
//   [31:30] PTEwidth — PTE 条目大小（0=64位）
//
// PWCH 格式（每字段 6 位）：
//   [5:0]   Dir3base — PGD 级别的起始位（30）
//   [11:6]  Dir3width — PGD 级别的位宽（9）
//   [17:12] Dir4base — 未使用（0）
//   [23:18] Dir4width — 未使用（0）
//
// LDDIR/LDPTE 硬件走表按这些字段取目录索引，编码错位会走到错误偏移
// （软走表时代 PWCL 无人消费、错位无害，启用 LDDIR 后即致命）。

/// PWCL 值：PTbase=12, PTwidth=9, Dir1base=21, Dir1width=9, PTEwidth=0
pub const PWCL_VALUE: usize = (PAGE_SIZE_BITS << 0)
    | (PT_INDEX_BITS << 5)
    | (21 << 10)
    | (PT_INDEX_BITS << 15);

/// PWCH 值：Dir3base=30, Dir3width=9
pub const PWCH_VALUE: usize = (30 << 0) | (PT_INDEX_BITS << 6);

/// STLBPS 值：4KB 页 → PS=12（页大小 = 2^12 = 4096）
pub const STLBPS_VALUE: usize = PAGE_SIZE_BITS;

// ============================================================================
// LoongArch64 PTE 位域定义 / PTE Bit Field Definitions
// ============================================================================
//
// 🟢 PTE 位域定义复用自 PolyHAL（MIT/Apache-2.0）
// 🟢 PTE bit field definitions adapted from PolyHAL (MIT/Apache-2.0)
//
// LoongArch64 PTE 格式（4KB 页）：
// [63:RPLV][62:NX][61:NR][...][8:W][7:P][6:G][5:4:MAT][3:2:PLV][1:D][0:V]
// [物理页号在 bit 12-47]

/// PTE 有效位 / Valid bit
pub const PTE_V: u64 = 1 << 0;

/// PTE 脏位 / Dirty bit
pub const PTE_D: u64 = 1 << 1;

/// PTE 特权等级位（2 位）/ Privilege Level (2 bits)
pub const PTE_PLV_SHIFT: u32 = 2;
pub const PTE_PLV_MASK: u64 = 0x3 << PTE_PLV_SHIFT;

/// PTE 存储访问类型位（2 位）/ Memory Access Type (2 bits)
/// MAT=0: 强序非缓存, MAT=1: 一致可缓存
pub const PTE_MAT_SHIFT: u32 = 4;
pub const PTE_MAT_MASK: u64 = 0x3 << PTE_MAT_SHIFT;

/// MAT=1: 一致可缓存 / Coherent Cacheable
pub const PTE_MAT_CC: u64 = 1 << PTE_MAT_SHIFT;

/// PTE 全局位 / Global bit
pub const PTE_G: u64 = 1 << 6;

/// PTE 存在位 / Present bit (物理页存在)
pub const PTE_P: u64 = 1 << 7;

/// PTE 可写位 / Writable bit
pub const PTE_W: u64 = 1 << 8;

/// PTE 不可读位 / No Read bit
pub const PTE_NR: u64 = 1 << 61;

/// PTE 不可执行位 / No Execute bit
pub const PTE_NX: u64 = 1 << 62;

/// PTE 受限特权等级位 / Restricted Privilege Level bit
pub const PTE_RPLV: u64 = 1 << 63;

/// PTE 物理页号掩码（bit 12-47 for 4KB page）
/// Physical Page Number mask
pub const PTE_PPN_MASK: u64 = 0x0000_FFFF_FFFF_F000;

// ============================================================================
// La64PTE — 页表条目 / Page Table Entry
// ============================================================================

/// LoongArch64 页表条目
///
/// LoongArch64 Page Table Entry.
///
/// 🟡 基于 PolyHAL 的 PTE 定义，适配微内核的能力检查（MIT/Apache-2.0）
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct La64PTE(pub u64);

impl La64PTE {
    /// 创建空的 PTE（无效）
    /// Create an empty (invalid) PTE.
    #[inline]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// 从原始值创建 PTE
    /// Create PTE from raw value.
    #[inline]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// 获取原始值
    /// Get raw PTE value.
    #[inline]
    pub const fn raw(&self) -> u64 {
        self.0
    }

    /// 是否有效
    /// Check if PTE is valid.
    #[inline]
    pub const fn is_valid(&self) -> bool {
        self.0 & PTE_V != 0
    }

    /// 是否可写
    /// Check if PTE is writable.
    #[inline]
    pub const fn is_writable(&self) -> bool {
        self.0 & PTE_W != 0
    }

    /// 是否可读（NR 位为 0 表示可读）
    /// Check if PTE is readable (NR=0 means readable).
    #[inline]
    pub const fn is_readable(&self) -> bool {
        self.0 & PTE_NR == 0
    }

    /// 是否可执行（NX 位为 0 表示可执行）
    /// Check if PTE is executable (NX=0 means executable).
    #[inline]
    pub const fn is_executable(&self) -> bool {
        self.0 & PTE_NX == 0
    }

    /// 是否是脏页
    /// Check if PTE is dirty.
    #[inline]
    pub const fn is_dirty(&self) -> bool {
        self.0 & PTE_D != 0
    }

    /// 是否是全局页
    /// Check if PTE is global.
    #[inline]
    pub const fn is_global(&self) -> bool {
        self.0 & PTE_G != 0
    }

    /// 获取物理页号
    /// Get physical page number.
    #[inline]
    pub const fn ppn(&self) -> usize {
        ((self.0 & PTE_PPN_MASK) >> PAGE_SIZE_BITS) as usize
    }

    /// 获取物理地址
    /// Get physical address.
    #[inline]
    pub const fn paddr(&self) -> usize {
        (self.0 & PTE_PPN_MASK) as usize
    }

    /// 获取特权等级
    /// Get privilege level.
    #[inline]
    pub const fn plv(&self) -> u8 {
        ((self.0 >> PTE_PLV_SHIFT) & 0x3) as u8
    }

    /// 创建叶子 PTE（映射到物理页）
    ///
    /// Create a leaf PTE mapping to a physical page.
    ///
    /// # 参数 / Parameters
    /// - `ppn`: 物理页号
    /// - `flags`: PTE 标志位
    pub fn new_leaf(ppn: usize, flags: u64) -> Self {
        Self(((ppn as u64) << PAGE_SIZE_BITS) | flags)
    }

    /// 创建非叶子 PTE（指向下一级页表）
    ///
    /// Create a non-leaf PTE pointing to the next level page table.
    /// 目录项 = 裸物理指针（无标志位，LDDIR 硬件走表要求低 12 位为 0），
    /// 有效性用「非零」判定（`dir_present`）。
    ///
    /// # 参数 / Parameters
    /// - `ppn`: 下一级页表的物理页号
    pub fn new_table(ppn: usize) -> Self {
        Self((ppn as u64) << PAGE_SIZE_BITS)
    }

    /// 目录项（非叶子级）是否存在：裸物理指针，非零即存在。
    #[inline]
    pub const fn dir_present(&self) -> bool {
        self.0 != 0
    }

    /// 是否是叶子 PTE（非页表目录项）
    ///
    /// 在 LoongArch64 中，非叶子 PTE 只有 V 位，
    /// 叶子 PTE 还有 D/PLV/MAT/W/P 等位。
    /// 简单判断：如果除了 V 和 PPN 之外还有其他位被设置，则为叶子。
    /// 实际上，非叶子 PTE 只设置 V 位。
    #[inline]
    pub fn is_leaf(&self) -> bool {
        // 非叶子 PTE 只有 V 位和 PPN，没有 D/W/P 等位
        // 如果有 D 或 W 或 P 位，则为叶子
        self.is_valid() && (self.0 & (PTE_D | PTE_W | PTE_P | PTE_MAT_MASK | PTE_PLV_MASK)) != 0
    }
}

impl core::fmt::Debug for La64PTE {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("La64PTE")
            .field("raw", &format_args!("{:#018x}", self.0))
            .field("valid", &self.is_valid())
            .field("ppn", &format_args!("{:#x}", self.ppn()))
            .finish()
    }
}

// ============================================================================
// 架构无关标志位 → LoongArch64 PTE 标志位转换
// Architecture-independent flags → LoongArch64 PTE flags conversion
// ============================================================================

/// 将架构无关的页表标志位转换为 LoongArch64 PTE 标志位
///
/// Convert architecture-independent page table flags to LoongArch64 PTE flags.
///
/// LoongArch64 PTE 使用"否定位"（NR=不可读, NX=不可执行），
/// 而架构无关标志使用"肯定位"（READ=可读, EXEC=可执行），
/// 因此需要反转逻辑。
fn flags_to_la64_pte(flags: usize) -> u64 {
    let mut pte_flags: u64 = PTE_V; // 始终设置有效位

    // 可写
    if flags & pte_flags::WRITE != 0 {
        pte_flags |= PTE_W | PTE_D; // 可写页同时设置脏位
    }

    // 不可读 → 设置 NR 位
    if flags & pte_flags::READ == 0 {
        pte_flags |= PTE_NR;
    }

    // 不可执行 → 设置 NX 位
    if flags & pte_flags::EXEC == 0 {
        pte_flags |= PTE_NX;
    }

    // 用户态可访问 → PLV=3
    if flags & pte_flags::USER != 0 {
        pte_flags |= 3u64 << PTE_PLV_SHIFT; // PLV=3 (用户态)
    }
    // 否则 PLV=0（内核态，默认值）

    // 全局页
    if flags & pte_flags::GLOBAL != 0 {
        pte_flags |= PTE_G;
    }

    // 脏位
    if flags & pte_flags::DIRTY != 0 {
        pte_flags |= PTE_D;
    }

    // 存在位
    pte_flags |= PTE_P;

    // 存储访问类型：一致可缓存（MAT=1）
    pte_flags |= PTE_MAT_CC;

    pte_flags
}


// ============================================================================
// 页帧分配辅助函数 / Page Frame Allocation Helpers
// ============================================================================
//
// 🟡 微内核适配：页帧分配通过内核的 Buddy 分配器
// 在 LoongArch64 DA=1（直接地址翻译）模式下，物理地址 == 虚拟地址
// 在 PG=1（映射模式）下，内核使用直接映射窗口（DMW）访问物理内存

/// 分配一个清零的物理页帧，返回物理地址
///
/// Allocate a zeroed physical page frame, return physical address.
/// 使用内核的 Buddy 分配器。
fn alloc_page_frame() -> Result<usize, MmError> {
    let ppn = crate::mm::alloc_frame().ok_or(MmError::OutOfMemory)?;
    // alloc_frame 已经清零了页帧并设置引用计数为 1
    // PhysPageNum.0 是页号，左移 12 位得到物理地址
    let paddr = ppn.0 << PAGE_SIZE_BITS;
    Ok(paddr)
}

/// 释放一个物理页帧
///
/// Deallocate a physical page frame.
fn dealloc_page_frame(paddr: usize) {
    let ppn = crate::mm::PhysPageNum(paddr >> PAGE_SIZE_BITS);
    crate::mm::dealloc_frame(ppn);
}

/// 物理地址转换为页表条目数组指针
///
/// Convert physical address to page table entry array pointer.
///
/// 在 LoongArch64 中，内核使用直接映射窗口（DMW0/DMW1）访问物理内存。
/// DMW0 通常配置为 0x9000_0000_0000_0000 + paddr（一致可缓存）。
/// 在 DA=1 模式下，物理地址可直接访问。
///
/// 当前实现假设 DA=1 模式或 DMW 已正确配置。
#[inline]
fn paddr_to_pte_array(paddr: usize) -> &'static mut [La64PTE; PTE_PER_PAGE] {
    // 在 DA=1 模式下，物理地址 == 虚拟地址
    // 在 PG=1 模式下，需要通过 DMW 窗口访问
    // DMW0 通常映射为 0x9000_0000_0000_0000 | paddr
    let vaddr = phys_to_kern_vaddr(paddr);
    unsafe { &mut *(vaddr as *mut [La64PTE; PTE_PER_PAGE]) }
}

/// 物理地址 → 内核虚拟地址
///
/// Convert physical address to kernel virtual address.
///
/// LoongArch64 使用直接映射窗口（DMW）：
/// - entry.S 在启动时已切换到 PG=1 模式
/// - DMW0: 0x9000_0000_0000_0000 | paddr（一致可缓存，MAT=1）
#[inline]
pub fn phys_to_kern_vaddr(paddr: usize) -> usize {
    // 如果地址已经在 DMW 窗口范围内，直接返回
    if paddr >= 0x8000_0000_0000_0000 {
        return paddr;
    }
    // entry.S 在启动时已经切换到 PG=1 模式
    // 直接通过 DMW0 窗口访问物理内存
    0x9000_0000_0000_0000 | paddr
}

/// 内核虚拟地址 → 物理地址
///
/// Convert kernel virtual address to physical address.
#[inline]
pub fn kern_vaddr_to_phys(vaddr: usize) -> usize {
    vaddr & 0x0000_FFFF_FFFF_FFFF
}

// ============================================================================
// 虚拟地址索引提取 / Virtual Address Index Extraction
// ============================================================================

/// 从虚拟页号提取三级页表索引
///
/// Extract three-level page table indices from virtual page number.
///
/// ```text
/// VPN [26:0]:
///   [26:18] PGD 索引 (9 位)  — 第一级
///   [17:9]  PMD 索引 (9 位)  — 第二级
///   [8:0]   PTE 索引 (9 位)  — 第三级
/// ```
#[inline]
fn vpn_indices(vpn: usize) -> [usize; PT_LEVELS] {
    [
        (vpn >> 18) & 0x1FF, // PGD 索引（第一级）
        (vpn >> 9) & 0x1FF,  // PMD 索引（第二级）
        vpn & 0x1FF,         // PTE 索引（第三级）
    ]
}

// ============================================================================
// La64PageTable — 页表包装器 / Page Table Wrapper
// ============================================================================

/// LoongArch64 页表包装器
///
/// LoongArch64 page table wrapper implementing `PageTableOps` trait.
///
/// 🟡 微内核适配：与宏内核不同，微内核仅映射内核代码/数据 + 设备 MMIO，
/// 不直接映射全部物理内存。每个用户进程有独立的页表。
///
/// ## 三级页表遍历
///
/// ```text
/// root_paddr → PGD[512] → PMD[512] → PTE[512] → 物理页
///              (第一级)    (第二级)    (第三级)
/// ```
///
/// ## 内存管理
///
/// La64PageTable 拥有所有中间页表页的物理地址，在 Drop 时释放。
/// 根页表页在 new() 时分配，中间页表页在 map 时按需分配。
pub struct La64PageTable {
    /// 根页表物理地址（PGDL 寄存器值）
    /// Root page table physical address (PGDL register value)
    root_paddr: usize,

    /// 所有分配的页表页物理地址（用于 Drop 时释放）
    /// All allocated page table page physical addresses (for deallocation on Drop)
    frames: Vec<usize>,
}

impl La64PageTable {
    /// 查找或创建指定 VPN 的叶子 PTE
    ///
    /// Find or create the leaf PTE for the given VPN.
    /// 遍历三级页表，如果中间页表页不存在则分配新页。
    ///
    /// # 返回
    /// - `Ok(&mut La64PTE)`: 叶子 PTE 的可变引用
    /// - `Err(MmError::OutOfMemory)`: 无法分配中间页表页
    fn find_pte_create(&mut self, vpn: usize) -> Result<&mut La64PTE, MmError> {
        let indices = vpn_indices(vpn);
        let mut current_paddr = self.root_paddr;

        // 遍历前两级（PGD → PMD），第三级返回叶子 PTE
        for level in 0..PT_LEVELS {
            let table = paddr_to_pte_array(current_paddr);
            let idx = indices[level];

            if level == PT_LEVELS - 1 {
                // 最后一级：返回叶子 PTE
                return Ok(&mut table[idx]);
            }

            // 非最后一级：检查是否需要分配下一级页表
            if !table[idx].dir_present() {
                // 分配新的页表页
                let new_page_paddr = alloc_page_frame()?;
                // 创建非叶子 PTE 指向新页表页
                table[idx] = La64PTE::new_table(new_page_paddr >> PAGE_SIZE_BITS);
                // 记录分配的页帧
                self.frames.push(new_page_paddr);
            }

            // 进入下一级页表
            current_paddr = table[idx].paddr();
        }

        // 不应到达这里
        Err(MmError::LevelError)
    }

    /// 查找指定 VPN 的叶子 PTE（只读）
    ///
    /// Find the leaf PTE for the given VPN (read-only).
    /// 如果中间页表页不存在，返回 None。
    fn find_pte(&self, vpn: usize) -> Option<&La64PTE> {
        let indices = vpn_indices(vpn);
        let mut current_paddr = self.root_paddr;

        for level in 0..PT_LEVELS {
            let table = paddr_to_pte_array(current_paddr);
            let idx = indices[level];

            if level == PT_LEVELS - 1 {
                // 最后一级：返回叶子 PTE
                return Some(&table[idx]);
            }

            if !table[idx].dir_present() {
                return None;
            }

            current_paddr = table[idx].paddr();
        }

        None
    }
}

impl PageTableOps for La64PageTable {
    /// 创建新的空页表
    ///
    /// Create a new empty page table with a freshly allocated root page.
    /// 分配根页表页（PGD），清零后返回。
    fn new() -> Self {
        let root_paddr = alloc_page_frame()
            .expect("La64PageTable::new: failed to allocate root page table frame");

        Self {
            root_paddr,
            frames: alloc::vec![root_paddr],
        }
    }

    /// 从页表 token 恢复页表结构
    ///
    /// Reconstruct a page table from its token (PGDL value).
    /// 注意：从 token 恢复的页表不拥有页帧的所有权，
    /// Drop 时不会释放页帧。
    fn from_token(token: usize) -> Self {
        Self {
            root_paddr: token,
            frames: Vec::new(), // 不拥有页帧
        }
    }

    /// 获取页表 token（PGDL 寄存器值）
    ///
    /// Get the page table token for loading into PGDL CSR.
    /// 返回根页表的物理地址。
    fn token(&self) -> usize {
        self.root_paddr
    }

    /// 带能力授权的页面映射
    ///
    /// Map a virtual page to a physical frame with capability authorization.
    ///
    /// 三级页表遍历：PGD → PMD → PTE，创建中间页表页，设置叶子 PTE。
    ///
    /// # 参数
    /// - `vpn`: 虚拟页号
    /// - `ppn`: 物理页号
    /// - `flags`: 架构无关的页表标志位（pte_flags::*）
    /// - `cap_rights`: 能力授权的权限位（usize::MAX 表示不检查）
    fn map_with_cap(
        &mut self,
        vpn: usize,
        ppn: usize,
        flags: usize,
        _cap_rights: usize,
    ) -> Result<(), MmError> {
        // TODO: 能力权限检查（当 cap_rights != usize::MAX 时）
        // if cap_rights != usize::MAX {
        //     verify_capability(flags, cap_rights)?;
        // }

        let pte = self.find_pte_create(vpn)?;

        if pte.is_valid() {
            return Err(MmError::AlreadyMapped);
        }

        // 将架构无关标志位转换为 LoongArch64 PTE 标志位
        let la64_flags = flags_to_la64_pte(flags);

        // 创建叶子 PTE
        *pte = La64PTE::new_leaf(ppn, la64_flags);

        Ok(())
    }

    /// 取消页面映射
    ///
    /// Unmap a virtual page. Clear the leaf PTE and flush TLB.
    fn unmap(&mut self, vpn: usize) {
        if let Ok(pte) = self.find_pte_create(vpn) {
            *pte = La64PTE::empty();
            // 刷新该虚拟地址的 TLB 条目。ASID-tagged TLB 下必须用本地址空间的
            // 真实 ASID 就地失效（不能再用 $zero=ASID0，否则漏掉本 ASID 的条目）。
            // peek 为 0 表示该地址空间本代尚未运行（无存活 TLB 条目），失效即空操作。
            let vaddr = vpn << PAGE_SIZE_BITS;
            let asid = crate::mm::asid::peek(self.root_paddr as u64);
            super::csr::invtlb_addr_asid(asid as usize, vaddr);
        }
    }

    /// 虚拟页号到物理页号的翻译
    ///
    /// Translate a virtual page number to a physical page number.
    fn translate(&self, vpn: usize) -> Option<usize> {
        self.find_pte(vpn)
            .filter(|pte| pte.is_valid())
            .map(|pte| pte.ppn())
    }

    /// 激活页表（写入 PGDL CSR）
    ///
    /// Activate this page table by loading its token into PGDL CSR.
    /// 同时刷新 TLB 确保新页表生效。
    fn activate(&self) {
        csrwr!(self.root_paddr, "0x19"); // PGDL = root_paddr
        // 刷新所有 TLB 条目
        super::csr::invtlb_all();
    }

    /// 按 ASID 刷新 TLB（微内核关键操作）
    ///
    /// Flush TLB by ASID (critical for microkernel).
    fn flush_asid(&self, asid: u16) {
        super::csr::invtlb_asid(asid as usize);
    }
}

impl Drop for La64PageTable {
    fn drop(&mut self) {
        // 仅「拥有页帧」的页表（new() 而非 from_token）销毁时才回收 ASID 映射：
        // 从映射删除本 root_paddr，使被回收复用的物理页获得全新 ASID（防止新
        // 地址空间继承旧 ASID → 别名命中旧条目 → 内存损坏）；并失效该 ASID
        // 残留的 TLB 条目，保持 TLB 干净。
        if !self.frames.is_empty() {
            let stale_asid = crate::mm::asid::forget(self.root_paddr as u64);
            if stale_asid != 0 {
                super::csr::invtlb_asid(stale_asid as usize);
            }
        }
        // 释放所有分配的页表页帧
        // 注意：从 from_token 创建的页表 frames 为空，不会释放
        for &paddr in &self.frames {
            dealloc_page_frame(paddr);
        }
    }
}


// ============================================================================
// TLB 管理和初始化 / TLB Management and Initialization
// ============================================================================
//
// 🔴 自己写：TLB 重填异常处理、页无效异常处理、PWCL/PWCH 配置

/// 初始化 LoongArch64 页表和 TLB 子系统
///
/// Initialize LoongArch64 page table and TLB subsystem.
///
/// 配置 PWCL/PWCH（页表遍历控制寄存器）和 STLBPS（STLB 页大小），
/// 设置 TLB 重填异常入口地址（TLBRENTRY）。
///
/// # 调用时机
/// 在内存管理初始化之后、用户态程序启动之前调用。
pub fn init_tlb() {
    extern "C" {
        fn __la64_tlb_refill();
    }

    // 1. 配置 PWCL — 页表遍历控制低位
    //    PTbase=12, PTwidth=9, Dir1base=21, Dir1width=9
    csrwr!(PWCL_VALUE, "0x1C");

    // 2. 配置 PWCH — 页表遍历控制高位
    //    Dir3base=30, Dir3width=9
    csrwr!(PWCH_VALUE, "0x1D");

    // 3. 配置 STLBPS — STLB 页大小（4KB → PS=12）
    csrwr!(STLBPS_VALUE, "0x1E");

    // 4. 设置 TLB 重填异常入口地址
    csrwr!(__la64_tlb_refill as usize, "0x88"); // TLBRENTRY

    // 4.5 预设 TLBREHI.PS=12（4KB）。TLBR 异常入口硬件只自动填
    //     TLBREHI.VPPN、保留 PS 字段，故快路径无需每次重写 TLBREHI。
    csrwr!(PAGE_SIZE_BITS, "0x8E"); // TLBREHI

    // 5. 清空所有 TLB 条目
    super::csr::invtlb_all();
}

/// 配置 PGDL 和 PGDH
///
/// Configure PGDL (user space page directory) and PGDH (kernel space page directory).
///
/// # 参数
/// - `pgdl`: 用户态页目录基址（低半地址空间）
/// - `pgdh`: 内核态页目录基址（高半地址空间）
pub fn set_pgd(pgdl: usize, pgdh: usize) {
    csrwr!(pgdl, "0x19"); // PGDL
    csrwr!(pgdh, "0x1A"); // PGDH
}

/// 设置 ASID
///
/// Set the current Address Space Identifier.
/// LoongArch64 支持 10 位 ASID（0-1023）。
///
/// # 参数
/// - `asid`: 地址空间标识符（0-1023）
pub fn set_asid(asid: u16) {
    let asid_val = (asid as usize) & 0x3FF; // 10 位 ASID
    csrwr!(asid_val, "0x18"); // ASID CSR
}

/// 获取当前 ASID
///
/// Get the current Address Space Identifier.
pub fn get_asid() -> u16 {
    let asid = csrrd!("0x18");
    (asid & 0x3FF) as u16
}

/// TLB 重填处理（Rust 部分）
///
/// TLB refill handler (Rust part).
///
/// 🔴 自己写：当 TLB 重填汇编入口无法通过 LDDIR/LDPTE 硬件辅助指令
/// 完成重填时，回退到此 Rust 函数进行软件页表遍历。
///
/// 此函数由 `tlb_refill.S` 中的汇编代码在需要时调用。
///
/// # 参数
/// - `badv`: 触发 TLB miss 的虚拟地址
///
/// # 返回
/// - `true`: 重填成功
/// - `false`: 页表中无映射（应触发页错误）
#[no_mangle]
pub extern "C" fn la64_tlb_refill_handler(badv: usize) -> bool {
    // 读取 PGD（硬件根据地址高位自动选择 PGDL 或 PGDH）
    let pgd = csrrd!("0x1B"); // PGD CSR（只读，自动选择 PGDL/PGDH）

    if pgd == 0 {
        return false;
    }

    let vpn = badv >> PAGE_SIZE_BITS;
    let indices = vpn_indices(vpn);

    // 三级页表遍历
    let mut current_paddr = pgd;

    // 第一级：PGD
    let pgd_table = paddr_to_pte_array(current_paddr);
    let pgd_entry = pgd_table[indices[0]];
    if !pgd_entry.dir_present() {
        return false;
    }
    current_paddr = pgd_entry.paddr();

    // 第二级：PMD
    let pmd_table = paddr_to_pte_array(current_paddr);
    let pmd_entry = pmd_table[indices[1]];
    if !pmd_entry.dir_present() {
        return false;
    }
    current_paddr = pmd_entry.paddr();

    // 第三级：PTE
    let pte_table = paddr_to_pte_array(current_paddr);
    let pte_idx = indices[2];

    // LoongArch64 TLB 使用奇偶页对（paired pages）
    // 每个 TLB 条目映射两个相邻的虚拟页（偶数页和奇数页）
    // 偶数页使用 TLBELO0，奇数页使用 TLBELO1
    let even_idx = pte_idx & !1; // 偶数索引
    let odd_idx = even_idx | 1;  // 奇数索引

    let pte_even = pte_table[even_idx];
    let pte_odd = pte_table[odd_idx];

    // 故障侧 PTE 无效 → 返回失败（与 tlb_refill.S 同一判据）：不把 V=0
    // 条目填进 TLB，装页后重执行经 refill 装载有效 PTE，免去 invtlb
    // （QEMU ≤10.x 单页 invtlb = 整个 softmmu TLB flush）。
    let fault_side = if pte_idx & 1 == 0 { pte_even } else { pte_odd };
    if !fault_side.is_valid() {
        return false;
    }

    // 设置 TLBELO0（偶数页）和 TLBELO1（奇数页）
    let elo0 = if pte_even.is_valid() { pte_even.raw() as usize } else { 0 };
    let elo1 = if pte_odd.is_valid() { pte_odd.raw() as usize } else { 0 };

    // 写入 TLB 重填相关 CSR
    csrwr!(elo0, "0x8C"); // TLBRELO0
    csrwr!(elo1, "0x8D"); // TLBRELO1

    // 设置 TLBREHI（虚拟页号 + 页大小）
    // TLBREHI 格式：[VPPN | PS]
    // VPPN = badv 的高位（去掉页内偏移和奇偶位）
    let vppn = badv & !((PAGE_SIZE << 1) - 1); // 对齐到偶数页
    let tlbrehi = vppn | (PAGE_SIZE_BITS << 0); // PS = 12 (4KB)
    csrwr!(tlbrehi, "0x8E"); // TLBREHI

    // 执行 TLB 填充。本函数只被 trap.rs 的 TLBR 慢路径调用，此时
    // tlb_refill.S 已清 TLBRERA.IsTLBR——而 tlbfill 仅在 IsTLBR=1 时
    // 才取上面写的 TLBREHI/TLBRELO*（否则取 TLBEHI/TLBELO* 陈旧值，
    // 会填入错误映射）。临时置回 IsTLBR 再恢复。
    let tlbrera = csrrd!("0x8A");
    csrwr!(tlbrera | 1, "0x8A");
    super::csr::tlbfill();
    csrwr!(tlbrera & !1usize, "0x8A");

    true
}

/// 为 `badv` 所在奇偶页对填入一条全无效（V=0）TLB 条目。
///
/// refill 快路径对 V=0 PTE 不再 tlbfill（避免 QEMU 全 TLB flush 成本），
/// 代价是 refill 慢路径丢失了访问类型（load/store/fetch）。当以读语义
/// 解析缺页失败时（如 PROT_WRITE-only VMA 的 store 首触），不能直接判
/// SIGSEGV——先填一条无效条目并重执行，让硬件抛出精确的 PIL/PIS/PIF，
/// 由对应处理路径按真实访问类型解析或投递信号。
///
/// tlbfill 在 TLBRERA.IsTLBR=1 时才取 TLBREHI/TLBRELO*，故临时置回
/// IsTLBR（tlb_refill.S 慢路径已将其清零）再恢复。
pub fn fill_invalid_tlb_pair(badv: usize) {
    let vppn = badv & !((PAGE_SIZE << 1) - 1);
    let tlbrera = csrrd!("0x8A");
    csrwr!(tlbrera | 1, "0x8A");
    csrwr!(0usize, "0x8C"); // TLBRELO0 = 无效
    csrwr!(0usize, "0x8D"); // TLBRELO1 = 无效
    csrwr!(vppn | PAGE_SIZE_BITS, "0x8E"); // TLBREHI
    super::csr::tlbfill();
    csrwr!(tlbrera & !1usize, "0x8A");
}

/// 页无效异常处理（Ecode=1/2/3）
///
/// Page invalid exception handler (PIL/PIS/PIF).
///
/// 🔴 自己写：当用户态访问的虚拟地址在 TLB 中存在但 PTE 无效时触发。
/// 这与 TLB 重填异常不同——TLB 重填是 TLB 中完全没有该地址的条目，
/// 而页无效异常是 TLB 条目存在但标记为无效。
///
/// # 参数
/// - `badv`: 触发异常的虚拟地址
/// - `is_write`: 是否是写操作（PIS）
/// - `is_fetch`: 是否是取指操作（PIF）
///
/// # 返回
/// TLB 重填回退路径的缺页处理。返回 `Ok(())` 表示页已装入，`Err(sig)` 表示
/// 失败并附带应投递的 POSIX 信号：7 = SIGBUS（hugetlb 池耗尽，对标 Linux
/// hugetlb 缺页语义，hugemmap27），11 = SIGSEGV（其它情形）。
///
/// 走 `try_resolve_fault`（与 PageInvalid 路径同一权威解析器），从而让 refill
/// 回退路径与 PageInvalid 路径在信号判定上完全一致；文件映射的按需调页
/// （`NeedFileRead`）返回 `Err(0)`，由 trap.rs 填 V=0 条目后重执行，交给
/// PIL/PIS 的两阶段处理器（共享/私有与访问类型均精确）。
pub fn handle_page_invalid(badv: usize, is_write: bool, _is_fetch: bool) -> Result<(), i32> {
    use crate::mm::address_space::FaultAction;
    let fault_addr = crate::mm::VirtAddr(badv);

    let (result, needs_tlb_drain) = {
        use crate::sched::process::PROCESS_TABLE;

        let mut table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_mut() {
            if let Some(current_pid) = table.current {
                let as_pid = table.get(current_pid)
                    .map(|p| if p.mm.is_some() { current_pid } else { p.tgid })
                    .unwrap_or(current_pid);
                if let Some(mm) = table.get(as_pid).and_then(|p| p.mm.as_ref()).cloned() {
                    let mut addr_space = mm.lock();
                    match addr_space.try_resolve_fault(fault_addr, is_write) {
                        FaultAction::Handled => (Ok(()), false),
                        FaultAction::HandledNeedsTlbDrain => (Ok(()), true),
                        FaultAction::Failed(sig) => (Err(sig as i32), false),
                        // 文件按需调页交回精确异常路径（PIL/PIS 的两阶段
                        // 处理器，正确区分共享/私有映射与访问类型）：返回
                        // Err 让 trap.rs 填 V=0 条目后重执行。内联
                        // handle_page_fault 会把 MAP_SHARED 页装成私有副本，
                        // 丢失跨进程写可见性（LTP tst_test 共享结果页）。
                        FaultAction::NeedFileRead(_) => (Err(0), false),
                    }
                } else {
                    (Err(11), false)
                }
            } else {
                (Err(11), false)
            }
        } else {
            (Err(11), false)
        }
    };

    if needs_tlb_drain {
        // 复用前强序 + quarantine 回收：CoW 摘除的旧帧必须等全部对端 TLB
        // 失效后才归还 buddy（rustc SIGSEGV 家族根因，见 address_space 注释）。
        crate::mm::address_space::AddressSpace::drain_and_reap_current_mm();
    }
    // 无需 invtlb：本函数只在 TLBR（TLB 中无该页对条目）慢路径被调用，
    // 装页后不存在陈旧条目；重执行经 refill 装载新 PTE。QEMU ≤10.x 的
    // 单页 invtlb 等价整个 softmmu TLB flush，省去它是按需调页的主要收益。
    result
}

// ============================================================================
// 切换到映射模式 / Switch to Paged Mode
// ============================================================================

/// 从直接地址翻译模式（DA=1）切换到映射地址翻译模式（PG=1）
///
/// Switch from Direct Address translation mode (DA=1) to
/// Paged address translation mode (PG=1).
///
/// 注意：entry.S 已经在启动时完成了 DA→PG 切换。
/// 此函数仅作为备用，正常情况下不应被调用。
///
/// # Safety
/// 调用者必须确保 DMW 和页表配置已完成。
pub unsafe fn enable_paging() {
    // 配置 DMW0：一致可缓存，PLV0 可访问
    let dmw0: usize = 0x9000_0000_0000_0011; // VSEG=0x9, MAT=CC, PLV0=1
    csrwr!(dmw0, "0x180"); // DMW0

    // 配置 DMW1：强序非缓存，PLV0 可访问
    let dmw1: usize = 0x8000_0000_0000_0001; // VSEG=0x8, MAT=SUC, PLV0=1
    csrwr!(dmw1, "0x181"); // DMW1

    // 不配置 DMW2/DMW3 — 内核已链接在 DMW0 窗口内
    csrwr!(0usize, "0x182");
    csrwr!(0usize, "0x183");

    // 切换 CRMD：DA=0, PG=1
    let crmd = csrrd!("0x0");
    let new_crmd = (crmd & !0x18) | 0x10;
    csrwr!(new_crmd, "0x0");
}
