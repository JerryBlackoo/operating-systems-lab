//! 页表实现
//!
//! RISC-V64: Sv39 三级页表
//! LoongArch64: 三级页表（4KB 页，9 位索引/级）
//!
//! 两种架构使用相同的三级页表遍历逻辑，但 PTE 编码不同。

#![allow(dead_code)]

use super::{PhysAddr, PhysPageNum, VirtAddr, VirtPageNum};
use super::frame::{frame_alloc, FrameTracker};
use bitflags::bitflags;
use alloc::vec::Vec;
use alloc::vec;

/// [DEBUG 插桩] 用户叶子 PTE 映射某帧 → map_count += 1（仅 U 位页）。
#[inline]
fn dbg_map_inc(ppn: PhysPageNum, flags: PTEFlags) {
    if !super::frame::frame_dbg() {
        return;
    }
    if flags.contains(PTEFlags::U) {
        if let Some(m) = super::page_meta::try_meta_of(ppn) {
            m.inc_map();
        }
    }
}

/// [DEBUG 插桩] 覆盖/清除一个已有叶子 PTE 前 → 若它是用户叶子，map_count -= 1。
#[inline]
fn dbg_map_dec_pte(pte: &PageTableEntry) {
    if !super::frame::frame_dbg() {
        return;
    }
    if pte.is_valid() && pte.is_leaf() && pte.flags().contains(PTEFlags::U) {
        if let Some(m) = super::page_meta::try_meta_of(pte.ppn()) {
            m.dec_map();
        }
    }
}

/// [DEBUG 插桩] PTESITE 看门：对目标 VPN（PTE-LEAK 泄漏页）的 map/remap/unmap
/// 即时打印调用者 RA，定位是哪条内核路径在 VMA 区间之外安装了用户映射。
/// 必须在函数最前调用（RA 寄存器此时仍是调用者地址）。
#[inline(always)]
fn ptesite_watch(action: &str, vpn: VirtPageNum) {
    if !crate::cmdline::ptesite_enabled() {
        return;
    }
    const WATCH_VPN: usize = 0x7fffff0000usize >> 12;
    if vpn.0 != WATCH_VPN {
        return;
    }
    let ra: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("mv {}, ra", out(reg) ra, options(nomem, nostack)); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("move {}, $ra", out(reg) ra, options(nomem, nostack)); }
    #[cfg(not(any(target_arch = "riscv64", target_arch = "loongarch64")))]
    { ra = 0; }
    crate::println!("[PTESITE] {} vpn={:#x} ra={:#x}", action, vpn.0, ra);
}

// ============================================================================
// PTEFlags — 架构无关的页表标志位接口
// ============================================================================

bitflags! {
    /// 页表项标志位（架构无关接口，内部编码架构特定）
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PTEFlags: u8 {
        /// Valid - 页表项有效
        const V = 1 << 0;
        /// Read - 可读
        const R = 1 << 1;
        /// Write - 可写
        const W = 1 << 2;
        /// Execute - 可执行
        const X = 1 << 3;
        /// User - 用户态可访问
        const U = 1 << 4;
        /// Global - 全局映射
        const G = 1 << 5;
        /// Accessed - 已访问
        const A = 1 << 6;
        /// Dirty - 已修改
        const D = 1 << 7;
    }
}

// ============================================================================
// PageTableEntry — 架构特定的 PTE 编码
// ============================================================================

/// 页表项
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry(pub usize);

impl PageTableEntry {
    pub const fn empty() -> Self { Self(0) }

    #[cfg(target_arch = "riscv64")]
    pub fn new(ppn: PhysPageNum, flags: PTEFlags) -> Self {
        Self((ppn.0 << 10) | flags.bits() as usize)
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn new(ppn: PhysPageNum, flags: PTEFlags) -> Self {
        // LoongArch64 叶子 PTE 编码：
        // [47:12] PPN, bit0=V, bit1=D, [3:2]=PLV, [5:4]=MAT, bit6=G, bit7=P, bit8=W
        // 否定位：NR(bit61)=不可读, NX(bit62)=不可执行
        let mut pte: usize = 0;
        let ppn_val = ppn.0 & 0xF_FFFF_FFFF; // 36 位 PPN
        pte |= ppn_val << 12; // PPN

        if flags.contains(PTEFlags::V) { pte |= 1 << 0; } // V
        if flags.contains(PTEFlags::W) { pte |= 1 << 1; } // D = writable/dirty
        if flags.contains(PTEFlags::U) { pte |= 3 << 2; } else { pte |= 0 << 2; } // PLV: 3=user, 0=kernel
        // MAT = 1 (Coherent Cached) for normal memory
        pte |= 1 << 4;
        if flags.contains(PTEFlags::G) { pte |= 1 << 6; } // G
        pte |= 1 << 7; // P (Present) — 标记为叶子节点
        if flags.contains(PTEFlags::W) { pte |= 1 << 8; } // W

        // 否定位：默认可读可执行，通过 NR/NX 位禁用
        if !flags.contains(PTEFlags::R) { pte |= 1 << 61; } // NR
        // NX（取指保护）：QEMU-LoongArch 无法投递"取指 NX"异常——一旦用户态取指
        // 命中 NX 页，QEMU 在 do_interrupt 内死循环重抛 PNX(exc 6)、永不更新 ERA/PC、
        // 永不进入异常入口（实测 2500 万次空转、CRMD.IE=0 连带时钟中断饿死）。crashme
        // 这类"执行随机内存"用例必然取指越界落到本应 NX 的页（如 libc.so 的数据段），
        // 从而整核卡死。鉴于该平台硬件层面根本无法强制取指保护，对用户页不设 NX：
        // 越界取指改为解码执行→非法指令(INE)→SIGILL（可正常投递）或被 SIGALRM 抢占，
        // 与真机上 crashme 的恢复路径一致。内核页仍保留 NX。
        if !flags.contains(PTEFlags::X) && !flags.contains(PTEFlags::U) { pte |= 1 << 62; } // NX (仅内核页)

        Self(pte)
    }

    /// LoongArch64 目录项构造（非叶子 = 裸物理指针，无任何标志位）。
    /// LDDIR 硬件走表要求目录项低 12 位为 0（QEMU helper_lddir 直接
    /// `base | index << 3` 拼物理地址，不剥标志位；bit6 会被误判为大页），
    /// 目录项有效性用「非零」判定（见 `dir_present`），对齐 Linux 语义。
    #[cfg(target_arch = "loongarch64")]
    pub fn new_directory(ppn: PhysPageNum) -> Self {
        let ppn_val = ppn.0 & 0xF_FFFF_FFFF;
        Self(ppn_val << 12)
    }

    /// 目录项（非叶子级）是否存在。RISC-V 目录项带 V 位；LoongArch64 目录项
    /// 是裸物理指针（为 LDDIR 硬件走表），非零即存在。
    #[inline]
    pub fn dir_present(&self) -> bool {
        #[cfg(target_arch = "riscv64")]
        { self.is_valid() }
        #[cfg(target_arch = "loongarch64")]
        { self.0 != 0 }
    }

    #[cfg(target_arch = "riscv64")]
    pub fn ppn(&self) -> PhysPageNum {
        PhysPageNum((self.0 >> 10) & 0xfff_ffff_ffff)
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn ppn(&self) -> PhysPageNum {
        PhysPageNum((self.0 >> 12) & 0xF_FFFF_FFFF)
    }

    #[cfg(target_arch = "riscv64")]
    pub fn flags(&self) -> PTEFlags {
        PTEFlags::from_bits_truncate(self.0 as u8)
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn flags(&self) -> PTEFlags {
        let raw = self.0;
        let mut f = PTEFlags::empty();
        if raw & (1 << 0) != 0 { f |= PTEFlags::V; }
        // bit1 = D (硬件 dirty / store-enable)。bit8 = W (软件可写意图)。
        // 两者独立解码：D 表示 HW 是否允许 store；W 表示 VMA 软件层面是否可写。
        if raw & (1 << 1) != 0 { f |= PTEFlags::D; }
        if raw & (1 << 8) != 0 { f |= PTEFlags::W; }
        if (raw >> 2) & 3 == 3 { f |= PTEFlags::U; }
        if raw & (1 << 6) != 0 { f |= PTEFlags::G; }
        // 否定位：NR=0 表示可读，NX=0 表示可执行
        if raw & (1 << 61) == 0 { f |= PTEFlags::R; }
        if raw & (1 << 62) == 0 { f |= PTEFlags::X; }
        f |= PTEFlags::A; // LoongArch64 没有 A 位，视为已访问
        f
    }

    pub fn is_valid(&self) -> bool {
        self.0 & 1 != 0 // bit 0 = V for both architectures
    }

    /// Swap PTE 软件 marker 位。两架构在 V=0（present=0）时硬件忽略其余位，
    /// 故复用 bit 9（RISC-V RSW 软件位 / LoongArch 未用位）标记"这是 swap entry"。
    /// 对标 Linux `swp_entry_to_pte`：present=0 但 PTE 非空，编码 (type, offset)。
    const SWAP_MARKER: usize = 1 << 9;
    const SWAP_SHIFT: usize = 10;

    /// 构造一个 swap PTE（present=0 + marker + swp_entry 编码于高位）。
    pub fn new_swap(swp_entry: usize) -> Self {
        Self((swp_entry << Self::SWAP_SHIFT) | Self::SWAP_MARKER)
    }

    /// 是否是 swap entry：V=0（硬件不可达）且 marker 置位。
    #[inline]
    pub fn is_swap(&self) -> bool {
        (self.0 & 1) == 0 && (self.0 & Self::SWAP_MARKER) != 0
    }

    /// 取出 swap PTE 中编码的 swp_entry 值。
    #[inline]
    pub fn swap_entry(&self) -> usize {
        self.0 >> Self::SWAP_SHIFT
    }

    #[cfg(target_arch = "riscv64")]
    pub fn is_leaf(&self) -> bool {
        let flags = self.flags();
        flags.contains(PTEFlags::R) || flags.contains(PTEFlags::W) || flags.contains(PTEFlags::X)
    }

    #[cfg(target_arch = "loongarch64")]
    pub fn is_leaf(&self) -> bool {
        // LoongArch64: 非目录项（有 PPN 且 V=1）就是叶子
        // 目录项只有 V 位和 PPN，没有权限位
        self.is_valid() && (self.0 & (1 << 7)) != 0 // P bit = present = leaf
    }
}

/// 页表（Sv39 三级页表）
pub struct PageTable {
    /// 根页表物理页号
    root_ppn: PhysPageNum,
    /// 页表页帧追踪器（用于自动释放）
    frames: Vec<FrameTracker>,
    /// 用户可读翻译单槽微 TLB（对标硬件 TLB 的软件版）：缓存最近一次
    /// `translate_readable` 命中的 (vpn, ppn)。失效点按构造完备：本结构是
    /// PTE 的唯一属主，一切可能改变既有映射的入口（unmap/remap/swap/清区间/
    /// 交出 `&mut PTE` 的 find_pte_mut*）统一 `rd_tlb_flush`；新增映射
    /// （map/install，断言原 PTE 无效）不可能使已缓存翻译过期，无需失效。
    /// 仅 4KiB 用户叶（U+R）入槽——大页叶不缓存（ppn 需加页内块偏移）。
    rd_tlb_vpn: core::sync::atomic::AtomicUsize,
    rd_tlb_ppn: core::sync::atomic::AtomicUsize,
}

impl PageTable {
    /// 创建新页表
    pub fn new() -> Self {
        Self::try_new().expect("Failed to allocate page table frame")
    }

    /// Fallible variant of `new`: returns `None` instead of panicking when
    /// the root page-table frame cannot be allocated (OOM). Used by the fork
    /// path so an out-of-memory fork fails gracefully (-ENOMEM).
    pub fn try_new() -> Option<Self> {
        // frame_alloc 已清零页帧（zero_frame），PTE empty()==0，无需二次清零
        let frame = frame_alloc()?;
        let ppn = frame.ppn;
        Some(Self {
            root_ppn: ppn,
            frames: vec![frame],
            rd_tlb_vpn: core::sync::atomic::AtomicUsize::new(usize::MAX),
            rd_tlb_ppn: core::sync::atomic::AtomicUsize::new(0),
        })
    }

    #[inline]
    fn rd_tlb_flush(&self) {
        self.rd_tlb_vpn
            .store(usize::MAX, core::sync::atomic::Ordering::Relaxed);
    }

    /// 获取根页表物理页号
    pub fn root_ppn(&self) -> PhysPageNum {
        self.root_ppn
    }

    /// 获取页表根地址（用于切换地址空间）
    /// RISC-V64: SATP 寄存器值（Sv39 模式）
    /// LoongArch64: PGDL 寄存器值（页目录基址的物理地址）
    pub fn satp(&self) -> usize {
        #[cfg(target_arch = "riscv64")]
        { (8usize << 60) | self.root_ppn.0 }
        #[cfg(target_arch = "loongarch64")]
        { self.root_ppn.0 << 12 } // 物理地址 = PPN << 12
    }

    /// 查找或创建页表项
    fn find_pte_create(&mut self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        let indices = vpn.indices();
        let mut ppn = self.root_ppn;

        for (i, &idx) in indices.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[idx];
            if i == 2 {
                // 最后一级，返回页表项
                return Some(pte);
            }
            if !pte.dir_present() {
                // 分配新页表页（frame_alloc 已清零，PTE empty()==0，免二次清零）
                let frame = frame_alloc()?;
                let new_ppn = frame.ppn;
                // 目录项：RISC-V 只需 V 位，LoongArch64 用专用构造函数
                #[cfg(target_arch = "riscv64")]
                { *pte = PageTableEntry::new(new_ppn, PTEFlags::V); }
                #[cfg(target_arch = "loongarch64")]
                { *pte = PageTableEntry::new_directory(new_ppn); }
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        None
    }

    /// 查找已映射叶子页表项的可变引用（**不创建**中间级，语义与只读 `find_pte`
    /// 完全一致，仅返回 `&mut`）。供 fork CoW 路径一次走表即可「读 ppn + 就地改权限」，
    /// 取代旧 `translate()`(读走表) + `try_remap()`(写走表) 的二次走表。
    /// 与 `find_pte` 同样处理中间级大页叶子（`is_leaf`），对未映射页返回 `None`，
    /// 故不会像 `find_pte_create` 那样为稀疏 VMA 的空洞页分配中间级页表。
    pub fn find_pte_mut(&mut self, vpn: VirtPageNum) -> Option<&mut PageTableEntry> {
        self.rd_tlb_flush();
        let indices = vpn.indices();
        let mut ppn = self.root_ppn;

        for (i, &idx) in indices.iter().enumerate() {
            let pte = &mut ppn.get_pte_array()[idx];
            if i == 2 {
                return Some(pte);
            }
            if !pte.dir_present() {
                return None;
            }
            if pte.is_leaf() {
                return Some(pte);
            }
            ppn = pte.ppn();
        }
        None
    }

    /// 定位 vpn 所在 L0（叶级）页表页，只走 L2/L1 两级。
    /// `Ok(Some(ppn))`=L0 表存在；`Ok(None)`=中间级缺失（整个 512 页块未映射）；
    /// `Err(())`=途中撞到大页叶子（调用方须回退逐页 `find_pte_mut` 语义）。
    pub fn l0_table_lookup(&self, vpn: VirtPageNum) -> Result<Option<PhysPageNum>, ()> {
        let indices = vpn.indices();
        let mut ppn = self.root_ppn;
        for &idx in &indices[..2] {
            let pte = &ppn.get_pte_array()[idx];
            if !pte.dir_present() {
                return Ok(None);
            }
            if pte.is_leaf() {
                return Err(());
            }
            ppn = pte.ppn();
        }
        Ok(Some(ppn))
    }

    /// 查找或创建 vpn 所在 L0 页表页（中间级逻辑与 `find_pte_create` 一致）。
    /// `None`=中间级页表页分配 OOM。
    pub fn l0_table_create(&mut self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        let indices = vpn.indices();
        let mut ppn = self.root_ppn;
        for &idx in &indices[..2] {
            let pte = &mut ppn.get_pte_array()[idx];
            if !pte.dir_present() {
                let frame = frame_alloc()?;
                let new_ppn = frame.ppn;
                #[cfg(target_arch = "riscv64")]
                { *pte = PageTableEntry::new(new_ppn, PTEFlags::V); }
                #[cfg(target_arch = "loongarch64")]
                { *pte = PageTableEntry::new_directory(new_ppn); }
                self.frames.push(frame);
            }
            ppn = pte.ppn();
        }
        Some(ppn)
    }

    /// 带 L0 表缓存的 `find_pte_mut`：升序 vpn 扫描场景（fork）下同一 512 页块
    /// 只做一次 L2/L1 下钻。cache = (块号 vpn>>9, 该块 L0 查询结果)。
    pub fn find_pte_mut_cached(
        &mut self,
        vpn: VirtPageNum,
        cache: &mut (usize, Result<Option<PhysPageNum>, ()>),
    ) -> Option<&mut PageTableEntry> {
        let block = vpn.0 >> 9;
        if cache.0 != block {
            *cache = (block, self.l0_table_lookup(vpn));
        }
        match cache.1 {
            Ok(Some(l0)) => {
                self.rd_tlb_flush();
                Some(&mut l0.get_pte_array()[vpn.0 & 0x1ff])
            }
            Ok(None) => None,
            Err(()) => self.find_pte_mut(vpn),
        }
    }

    /// 带 L0 表缓存的 `try_map`（语义与 `try_map` 一致，含 V|A|D 与去重断言）。
    /// 仅用于升序 vpn 的批量建映射（fork 子页表），同块免重复中间级下钻。
    pub fn try_map_cached(
        &mut self,
        vpn: VirtPageNum,
        ppn: PhysPageNum,
        flags: PTEFlags,
        cache: &mut (usize, Option<PhysPageNum>),
    ) -> bool {
        let block = vpn.0 >> 9;
        ptesite_watch("try_map_cached", vpn);
        if cache.0 != block {
            *cache = (block, self.l0_table_create(vpn));
        }
        let Some(l0) = cache.1 else {
            return false;
        };
        let pte = &mut l0.get_pte_array()[vpn.0 & 0x1ff];
        assert!(!pte.is_valid(), "VPN {:?} is already mapped", vpn);
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V | PTEFlags::A | PTEFlags::D);
        dbg_map_inc(ppn, flags);
        true
    }

    /// 查找页表项（只读）
    fn find_pte(&self, vpn: VirtPageNum) -> Option<&PageTableEntry> {
        let indices = vpn.indices();
        let mut ppn = self.root_ppn;

        for (i, &idx) in indices.iter().enumerate() {
            let pte = &ppn.get_pte_array()[idx];
            if i == 2 {
                return Some(pte);
            }
            if !pte.dir_present() {
                return None;
            }
            // 中间层叶子（大页 huge leaf）：该 PTE 本身即此 VA 的映射，**不是**指向
            // 下一级目录的指针。每个地址空间的 root[2] 都含内核监督态 1GiB 恒等大页
            // （映射物理 RAM）；若不在此停下、而把叶子的 ppn 当作页表页继续下钻，就会
            // 把物理 RAM 当页表读取 → 乱走/卡死（crash02 向 mkdirat 传入落在该大页区间
            // 的随机指针 0xbec6f6a1 触发整核冻结）。与 unmap_user 的大页处理保持一致。
            if pte.is_leaf() {
                return Some(pte);
            }
            ppn = pte.ppn();
        }
        None
    }

    /// 映射虚拟页到物理页
    ///
    /// 自动设置 V (Valid)、A (Accessed) 和 D (Dirty) 标志位。
    /// RISC-V 规范要求：
    /// - 如果 A 位为 0，任何访问都会触发页面错误
    /// - 如果 D 位为 0，任何写操作都会触发页面错误
    /// 为了避免不必要的页面错误，我们预先设置这些位。
    pub fn map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        ptesite_watch("map", vpn);
        let pte = self.find_pte_create(vpn).expect("Failed to create PTE");
        assert!(!pte.is_valid(), "VPN {:?} is already mapped", vpn);
        // 自动添加 V、A、D 标志
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V | PTEFlags::A | PTEFlags::D);
        dbg_map_inc(ppn, flags);
    }

    /// 可失败版 `map`：与 `map` 语义一致，但当中间级页表分配 OOM 时返回
    /// `false` 而非 panic。供缺页处理（懒分配）路径使用——OOM 由上层转为
    /// 终止触发缺页的当前进程（回收其内存），对标 Linux 而非 halt 内核。
    pub fn try_map(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) -> bool {
        ptesite_watch("try_map", vpn);
        let Some(pte) = self.find_pte_create(vpn) else {
            return false;
        };
        assert!(!pte.is_valid(), "VPN {:?} is already mapped", vpn);
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V | PTEFlags::A | PTEFlags::D);
        dbg_map_inc(ppn, flags);
        true
    }

    /// 直接写一个调用方已定位的**无效**叶子 PTE（批量别名映射路径：调用方
    /// 经 L0 表缓存拿到叶级槽位，免每页三级走表）。语义同 `map`（V|A|D）。
    pub fn install_leaf_pte(
        &mut self,
        pte: &mut PageTableEntry,
        ppn: PhysPageNum,
        flags: PTEFlags,
    ) {
        debug_assert!(!pte.is_valid());
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V | PTEFlags::A | PTEFlags::D);
        dbg_map_inc(ppn, flags);
    }

    /// 覆盖一个调用方已定位的**有效**叶子 PTE（批量别名映射路径）。
    /// 语义同 `remap`（V|A），调用方负责 TLB 失效。
    pub fn replace_leaf_pte(
        &mut self,
        pte: &mut PageTableEntry,
        ppn: PhysPageNum,
        flags: PTEFlags,
    ) {
        self.rd_tlb_flush();
        dbg_map_dec_pte(pte);
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V | PTEFlags::A);
        dbg_map_inc(ppn, flags);
    }

    /// 取消映射
    pub fn unmap(&mut self, vpn: VirtPageNum) {
        ptesite_watch("unmap", vpn);
        self.rd_tlb_flush();
        let pte = self.find_pte_create(vpn).expect("Failed to find PTE");
        assert!(pte.is_valid(), "VPN {:?} is not mapped", vpn);
        dbg_map_dec_pte(pte);
        *pte = PageTableEntry::empty();
    }

    /// 重新映射（修改已有映射的物理页号和标志位）
    pub fn remap(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) {
        ptesite_watch("remap", vpn);
        self.rd_tlb_flush();
        let pte = self.find_pte_create(vpn).expect("Failed to find PTE for remap");
        dbg_map_dec_pte(pte);
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V | PTEFlags::A);
        dbg_map_inc(ppn, flags);
    }

    /// Fallible variant of `remap`: returns `false` instead of panicking when
    /// intermediate page-table nodes cannot be allocated (OOM). Used by the
    /// fork CoW path so an out-of-memory fork fails gracefully.
    pub fn try_remap(&mut self, vpn: VirtPageNum, ppn: PhysPageNum, flags: PTEFlags) -> bool {
        self.rd_tlb_flush();
        let Some(pte) = self.find_pte_create(vpn) else {
            return false;
        };
        dbg_map_dec_pte(pte);
        *pte = PageTableEntry::new(ppn, flags | PTEFlags::V | PTEFlags::A);
        dbg_map_inc(ppn, flags);
        true
    }

    /// 把一个 vpn 的叶子 PTE 替换为 swap entry（换出）。调用方应已把该页内容
    /// 写入 swap slot、并移除对应 `Arc<FrameTracker>`。若原 PTE 是已映射的用户
    /// 叶子，`dbg_map_dec_pte` 会同步减少 map_count。返回是否成功。
    pub fn set_swap(&mut self, vpn: VirtPageNum, swp_entry: usize) -> bool {
        self.rd_tlb_flush();
        let Some(pte) = self.find_pte_create(vpn) else {
            return false;
        };
        dbg_map_dec_pte(pte);
        *pte = PageTableEntry::new_swap(swp_entry);
        true
    }

    /// 读取 vpn 上的 swap entry（若该 PTE 是 swap entry）。
    pub fn get_swap(&self, vpn: VirtPageNum) -> Option<usize> {
        self.find_pte(vpn).filter(|pte| pte.is_swap()).map(|pte| pte.swap_entry())
    }

    /// 清除 vpn 上的 swap PTE（换入完成、由 `map` 重新建立映射前可用；
    /// 但 `map` 断言 !is_valid，swap PTE 满足，故通常直接 `map` 覆盖即可）。
    pub fn clear_swap(&mut self, vpn: VirtPageNum) {
        self.rd_tlb_flush();
        if let Some(pte) = self.find_pte_create(vpn) {
            if pte.is_swap() {
                *pte = PageTableEntry::empty();
            }
        }
    }

    /// 翻译虚拟页号到物理页号
    pub fn translate(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.find_pte(vpn)
            .filter(|pte| pte.is_valid())
            .map(|pte| pte.ppn())
    }

    /// Translate VPN → PPN, but only if the page is user-readable: must be a
    /// valid leaf carrying both R (readable) and U (user-accessible). 要求 U：
    /// 用户态指针落入监督态映射（如每个地址空间都存在的内核恒等大页，R=1 但 U=0）
    /// 时必须视为不可读 → 调用方按 EFAULT 处理，与 Linux `access_ok`/`copy_from_user`
    /// 对非用户地址返回 -EFAULT 一致（crash02 mkdirat 传入内核大页区间的随机指针）。
    pub fn translate_readable(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        use core::sync::atomic::Ordering::Relaxed;
        if self.rd_tlb_vpn.load(Relaxed) == vpn.0 {
            return Some(PhysPageNum(self.rd_tlb_ppn.load(Relaxed)));
        }
        let pte = self.find_pte(vpn).filter(|pte| {
            pte.is_valid()
                && pte.flags().contains(PTEFlags::R)
                && pte.flags().contains(PTEFlags::U)
        })?;
        let ppn = pte.ppn();
        // 入槽：通过 U+R 过滤的只能是 4KiB 用户叶（本内核用户映射全为 4K 页；
        // 中间级大页叶仅内核恒等映射，无 U 位，走不到这）。
        self.rd_tlb_vpn.store(vpn.0, Relaxed);
        self.rd_tlb_ppn.store(ppn.0, Relaxed);
        Some(ppn)
    }

    /// Translate VPN → PPN, but only if the page is a valid writable leaf
    /// (V+W)。单次走表同时拿到权限与 ppn，供 `write_user_buffer` 等写路径
    /// 在常见"页已可写"情形下免去 `get_pte_flags` + `translate` 的二次走表；
    /// 不可写/未映射时返回 `None`，由调用方走缺页/CoW 慢路径。
    pub fn translate_writable(&self, vpn: VirtPageNum) -> Option<PhysPageNum> {
        self.find_pte(vpn)
            .filter(|pte| pte.is_valid() && pte.flags().contains(PTEFlags::W))
            .map(|pte| pte.ppn())
    }

    /// 获取页表项的标志位
    pub fn get_pte_flags(&self, vpn: VirtPageNum) -> Option<PTEFlags> {
        self.find_pte(vpn)
            .filter(|pte| pte.is_valid())
            .map(|pte| pte.flags())
    }

    /// 调试：返回叶子 PTE 的原始 usize 值（未经解码）
    ///
    /// 用于在 PME 失败时 dump PTE 状态（V/D/W/NR/NX/PLV 等 bit 位）。
    /// 只读访问，不修改页表。
    pub fn peek_pte_raw_la(&self, vpn: VirtPageNum) -> Option<usize> {
        self.find_pte(vpn).map(|pte| pte.0)
    }

    /// 调试：返回叶子 PTE 的原始值（通用，RISC-V / LoongArch 均可用）。
    pub fn raw_pte_dbg(&self, vpn: VirtPageNum) -> Option<usize> {
        self.find_pte(vpn).map(|pte| pte.0)
    }

    /// LoongArch64 专用：在已存在 PTE 上置 D 位（dirty/store-enable）。
    ///
    /// PageModify (PME) 异常的硬件语义为：页 V=1 但 D=0 时执行 store。
    /// 处理方法是直接把 bit1 (D) OR 上去并刷 TLB，而不是重新建立映射。
    ///
    /// 仅当软件 W 位 (bit8) 已设置时才置 D 位。这避免对：
    ///   - 真正只读的页（.rodata）放权
    ///   - CoW 页（V=1 W=0 D=0 PLV=3）绕过 handle_cow，否则父子共享页会
    ///     被同一物理页直接写穿，破坏隔离性。
    ///
    /// W=0 的情形必须 fallthrough 到 `handle_page_fault`，由 VMA 层基于
    /// `cow` 标志和 `permission.write` 决定是 CoW 拷贝、lazy 分配或 SIGSEGV。
    /// 返回是否处理成功。
    #[cfg(target_arch = "loongarch64")]
    pub fn mark_dirty_la(&mut self, vpn: VirtPageNum) -> bool {
        let pte = match self.find_pte_create(vpn) {
            Some(p) => p,
            None => return false,
        };
        if !pte.is_valid() {
            return false;
        }
        if pte.0 & (1 << 8) == 0 {
            return false;
        }
        pte.0 |= 1 << 1; // 置 D 位
        true
    }

    /// 读取叶子 PTE 的硬件「已访问」标志（reclaim clock 算法用）。
    /// RISC-V：A 位（bit 6）。LoongArch：无 A 位，用 D 位（bit 1，写访问由 PME
    /// 硬件置位）近似——reclaim 候选均为可写匿名页，写访问即代表"在用"。
    /// 返回 `None` 表示 PTE 不存在或非有效叶子。
    pub fn pte_accessed(&self, vpn: VirtPageNum) -> Option<bool> {
        let pte = self.find_pte(vpn)?;
        if !pte.is_valid() {
            return None;
        }
        #[cfg(target_arch = "riscv64")]
        {
            Some(pte.0 & (1 << 6) != 0)
        }
        #[cfg(target_arch = "loongarch64")]
        {
            Some(pte.0 & (1 << 1) != 0)
        }
    }

    /// 清除叶子 PTE 的硬件「已访问」标志，使下一次访问由硬件重新置位（RISC-V
    /// A 位 / LoongArch D 位经 PME）。返回清除前是否已置位。
    ///
    /// **不刷 TLB**：批量回收时由调用方在扫描结束统一 `flush_tlb_all` 一次，避免
    /// 每页一次 `sfence.vma`（TCG 下昂贵）的吞吐瓶颈。刷新前 stale TLB 项仍可能让
    /// 硬件跳过 A 位更新，故调用方**必须**在依赖重新置位前刷 TLB。
    pub fn clear_accessed(&mut self, vpn: VirtPageNum) -> bool {
        let Some(pte) = self.find_pte_create(vpn) else {
            return false;
        };
        if !pte.is_valid() {
            return false;
        }
        #[cfg(target_arch = "riscv64")]
        let was = {
            let w = pte.0 & (1 << 6) != 0;
            pte.0 &= !(1 << 6);
            w
        };
        #[cfg(target_arch = "loongarch64")]
        let was = {
            let w = pte.0 & (1 << 1) != 0;
            pte.0 &= !(1 << 1);
            w
        };
        was
    }

    /// 读取叶子 PTE 的硬件「脏」标志（`/proc/kpageflags` 的 KPF_DIRTY）。
    /// RISC-V：D 位（bit 7）。LoongArch：D 位（bit 1，store 由 PME 硬件置位）。
    /// 返回 `None` 表示 PTE 不存在或非有效叶子。
    pub fn pte_dirty(&self, vpn: VirtPageNum) -> Option<bool> {
        let pte = self.find_pte(vpn)?;
        if !pte.is_valid() {
            return None;
        }
        #[cfg(target_arch = "riscv64")]
        {
            Some(pte.0 & (1 << 7) != 0)
        }
        #[cfg(target_arch = "loongarch64")]
        {
            Some(pte.0 & (1 << 1) != 0)
        }
    }

    /// 清除叶子 PTE 的硬件「脏」标志（`msync(MS_SYNC)` 写回后）。返回清除前是否
    /// 已置位。RISC-V 清 D 位（bit 7）/ LoongArch 清 D 位（bit 1）。清除后下一次
    /// 写访问会再次缺页（RISC-V store-fault / LoongArch PME），由 `try_resolve_fault`
    /// 的 present-page 分支 remap 重置 D 位，故不破坏后续写。**不刷 TLB**：由调用方
    /// 统一 `flush_tlb_all`。
    pub fn clear_dirty(&mut self, vpn: VirtPageNum) -> bool {
        let Some(pte) = self.find_pte_create(vpn) else {
            return false;
        };
        if !pte.is_valid() {
            return false;
        }
        #[cfg(target_arch = "riscv64")]
        let was = {
            let w = pte.0 & (1 << 7) != 0;
            pte.0 &= !(1 << 7);
            w
        };
        #[cfg(target_arch = "loongarch64")]
        let was = {
            let w = pte.0 & (1 << 1) != 0;
            pte.0 &= !(1 << 1);
            w
        };
        was
    }

    /// 翻译虚拟地址到物理地址
    pub fn translate_va(&self, va: VirtAddr) -> Option<PhysAddr> {
        let vpn = va.floor();
        let offset = va.page_offset();
        self.translate(vpn).map(|ppn| {
            PhysAddr((ppn.0 << 12) | offset)
        })
    }

    /// 翻译虚拟地址到内核可访问的指针地址
    ///
    /// RISC-V64: 返回物理地址（恒等映射）
    /// LoongArch64 PG=1: 返回 DMW0 窗口地址
    pub fn translate_va_to_kernel_ptr(&self, va: usize) -> Option<usize> {
        let vaddr = VirtAddr(va);
        let vpn = vaddr.floor();
        let offset = vaddr.page_offset();
        self.translate(vpn).map(|ppn| {
            super::address::phys_to_kernel_vaddr((ppn.0 << 12) | offset)
        })
    }

    /// 遍历所有有效叶子 PTE 并清除（设为 empty）。
    ///
    /// 三级 Sv39 页表递归 walk：只访问有效的中间目录项，跳过空子树，
    /// 因此代价 = O(已映射页数)，不受地址空间大小影响。
    /// 用户空间限制：RISC-V Sv39 低半部 = L2 索引 0..256。
    /// 返回被清除的叶子 PTE 数量。
    pub fn clear_all_user_leaf_ptes(&mut self) -> u32 {
        self.rd_tlb_flush();
        let mut count = 0u32;
        // FRAMEDBG 取证：逐个打印被清除的用户叶子 PTE（VA/ppn/flags），用于
        // 定位「VMA 区间外的泄漏映射」的确切地址——泄漏页的帧被释放后重新
        // 分配给他人，残留 PTE 就是 UAF 读写通道。
        let dbg = super::frame::frame_dbg();
        let l2_table = self.root_ppn.get_pte_array();
        #[cfg(target_arch = "riscv64")]
        let l2_range = 0..256usize; // Sv39 低半空间
        #[cfg(target_arch = "loongarch64")]
        let l2_range = 0..512usize;
        for l2_idx in l2_range {
            let l2_pte = &mut l2_table[l2_idx];
            if !l2_pte.dir_present() && !l2_pte.is_valid() {
                continue;
            }
            if l2_pte.is_leaf() {
                // 1GiB 大页叶子 — 只清除用户页（带 U 位）
                if l2_pte.flags().contains(PTEFlags::U) {
                    if dbg {
                        crate::println!(
                            "[PTE-LEAK] va={:#x} ppn={:#x} flags={:#x} (l2-leaf)",
                            (l2_idx as u64) << 30,
                            l2_pte.ppn().0,
                            l2_pte.flags().bits()
                        );
                    }
                    dbg_map_dec_pte(l2_pte);
                    *l2_pte = PageTableEntry::empty();
                    count += 1;
                }
                continue;
            }
            let l1_ppn = l2_pte.ppn();
            let l1_table = l1_ppn.get_pte_array();
            for l1_idx in 0..512usize {
                let l1_pte = &mut l1_table[l1_idx];
                if !l1_pte.dir_present() && !l1_pte.is_valid() {
                    continue;
                }
                if l1_pte.is_leaf() {
                    if l1_pte.flags().contains(PTEFlags::U) {
                        if dbg {
                            crate::println!(
                                "[PTE-LEAK] va={:#x} ppn={:#x} flags={:#x} (l1-leaf)",
                                ((l2_idx as u64) << 30) | ((l1_idx as u64) << 21),
                                l1_pte.ppn().0,
                                l1_pte.flags().bits()
                            );
                        }
                        dbg_map_dec_pte(l1_pte);
                        *l1_pte = PageTableEntry::empty();
                        count += 1;
                    }
                    continue;
                }
                let l0_ppn = l1_pte.ppn();
                let l0_table = l0_ppn.get_pte_array();
                for l0_idx in 0..512usize {
                    let l0_pte = &mut l0_table[l0_idx];
                    if l0_pte.is_valid() && l0_pte.flags().contains(PTEFlags::U) {
                        if dbg {
                            let leak_vpn =
                                ((l2_idx as u64) << 30) | ((l1_idx as u64) << 21) | ((l0_idx as u64) << 12);
                            let site = crate::mm::address_space::pte_site::last_any(leak_vpn as usize);
                            crate::println!(
                                "[PTE-LEAK] va={:#x} ppn={:#x} flags={:#x} site={:?}",
                                leak_vpn,
                                l0_pte.ppn().0,
                                l0_pte.flags().bits(),
                                site
                            );
                        }
                        dbg_map_dec_pte(l0_pte);
                        *l0_pte = PageTableEntry::empty();
                        count += 1;
                    } else if l0_pte.is_swap() {
                        // 换出页：归还 swap slot 引用，清除 swap entry。
                        let off = super::swap::swp_offset(l0_pte.swap_entry());
                        super::swap::free_slot(off);
                        *l0_pte = PageTableEntry::empty();
                        count += 1;
                    }
                }
            }
        }
        count
    }

    /// 按 vpn 区间清除用户叶子 PTE（含 swap entry 归还），语义与
    /// `clear_all_user_leaf_ptes` 的对应分支一致，但只走 `[start, end)`：
    /// 每个 512 页 L0 块只做一次 L2/L1 下钻，代价 = O(区间页数)，
    /// 不再全表扫描每张 L0 表的全部 512 项。撞大页叶子时回退逐页处理。
    pub fn clear_user_range(&mut self, start: VirtPageNum, end: VirtPageNum) -> u32 {
        self.rd_tlb_flush();
        let mut count = 0u32;
        let mut vpn = start.0;
        while vpn < end.0 {
            let block_end = (((vpn >> 9) + 1) << 9).min(end.0);
            match self.l0_table_lookup(VirtPageNum(vpn)) {
                Ok(Some(l0_ppn)) => {
                    let l0_table = l0_ppn.get_pte_array();
                    for cur in vpn..block_end {
                        let pte = &mut l0_table[cur & 0x1ff];
                        if pte.is_valid() && pte.flags().contains(PTEFlags::U) {
                            dbg_map_dec_pte(pte);
                            *pte = PageTableEntry::empty();
                            count += 1;
                        } else if pte.is_swap() {
                            let off = super::swap::swp_offset(pte.swap_entry());
                            super::swap::free_slot(off);
                            *pte = PageTableEntry::empty();
                            count += 1;
                        }
                    }
                }
                Ok(None) => {}
                Err(()) => {
                    // 大页叶子（L2/L1 级）：逐页回退，find_pte_mut 会停在叶子上
                    for cur in vpn..block_end {
                        if let Some(pte) = self.find_pte_mut(VirtPageNum(cur)) {
                            if pte.is_valid() && pte.flags().contains(PTEFlags::U) {
                                dbg_map_dec_pte(pte);
                                *pte = PageTableEntry::empty();
                                count += 1;
                            }
                        }
                    }
                }
            }
            vpn = block_end;
        }
        count
    }
}

impl Drop for PageTable {
    fn drop(&mut self) {
        // ASID-tagged TLB（仅 RV）：拥有页帧的用户页表销毁时回收其 ASID 映射，
        // 使被复用的 root_ppn 获得全新 ASID（防新地址空间继承旧 ASID → 别名命中
        // 旧条目 → 内存损坏）；并失效该 ASID 残留 TLB 条目，保持 TLB 干净。
        // from_token 等共享页表 frames 为空，不参与 ASID 记账。
        #[cfg(target_arch = "riscv64")]
        {
            if !self.frames.is_empty() {
                let root_paddr = (self.root_ppn.0 as u64) << 12;
                let stale_asid = crate::mm::asid::forget(root_paddr);
                if stale_asid != 0 {
                    crate::arch::asm_helpers::flush_tlb_asid(stale_asid as usize);
                }
            }
        }
        // frames 会自动释放
    }
}

/// 刷新 TLB
pub fn flush_tlb() {
    crate::arch::asm_helpers::flush_tlb_all();
}

/// 刷新指定虚拟地址的 TLB
pub fn flush_tlb_va(va: usize) {
    crate::arch::asm_helpers::flush_tlb_va(va);
}
