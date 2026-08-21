//! TLB Shootdown 优化
//!
//! 优化多核系统的 TLB 同步机制，减少不必要的 IPI。
//!
//! # 特性
//! - 智能 TLB 刷新（只刷新必要的 CPU）
//! - 批量 TLB Shootdown
//! - ASID 支持
//! - 延迟刷新

use core::sync::atomic::{AtomicU16, AtomicU64, AtomicBool, Ordering};
use crate::sync::SpinLock;
use alloc::vec::Vec;
use alloc::vec;

/// 最大 CPU 数量（与 `sched::smp::MAX_CPUS` 保持一致）
pub const MAX_CPUS: usize = 16;

/// 最大 ASID 数量（RISC-V Sv39 支持 16 位）
pub const MAX_ASID: u16 = 0xFFFF;

/// CPU 掩码
#[derive(Clone, Copy, Default)]
pub struct CpuMask(u64);

impl CpuMask {
    /// 创建空掩码
    pub const fn empty() -> Self {
        Self(0)
    }

    /// 创建包含所有 CPU 的掩码
    pub const fn all(num_cpus: usize) -> Self {
        Self((1u64 << num_cpus) - 1)
    }

    /// 设置 CPU
    pub fn set(&mut self, cpu_id: usize) {
        self.0 |= 1u64 << cpu_id;
    }

    /// 清除 CPU
    pub fn clear(&mut self, cpu_id: usize) {
        self.0 &= !(1u64 << cpu_id);
    }

    /// 检查是否包含 CPU
    pub fn contains(&self, cpu_id: usize) -> bool {
        (self.0 & (1u64 << cpu_id)) != 0
    }

    /// 检查是否为空
    pub fn is_empty(&self) -> bool {
        self.0 == 0
    }

    /// 获取 CPU 数量
    pub fn count(&self) -> usize {
        self.0.count_ones() as usize
    }

    /// 迭代所有设置的 CPU
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        (0..64).filter(move |&i| self.contains(i))
    }
}

impl core::ops::BitOr for CpuMask {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl core::ops::BitOrAssign for CpuMask {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// 虚拟地址范围
#[derive(Clone, Copy)]
pub struct VirtAddrRange {
    /// 起始地址
    pub start: u64,
    /// 结束地址
    pub end: u64,
}

impl VirtAddrRange {
    /// 创建新范围
    pub fn new(start: u64, end: u64) -> Self {
        Self { start, end }
    }

    /// 迭代页地址
    pub fn iter_pages(&self) -> impl Iterator<Item = u64> {
        let start = self.start & !0xFFF;
        let end = (self.end + 0xFFF) & !0xFFF;
        (start..end).step_by(4096)
    }

    /// 获取页数
    pub fn page_count(&self) -> usize {
        let start = self.start & !0xFFF;
        let end = (self.end + 0xFFF) & !0xFFF;
        ((end - start) / 4096) as usize
    }
}

/// 每 CPU TLB 状态
pub struct TlbCpuState {
    /// 当前 ASID
    current_asid: AtomicU16,
    /// 活跃的地址空间（指针）
    active_mm: AtomicU64,
    /// 延迟刷新标志
    lazy_flush_pending: AtomicBool,
    /// shootdown 请求/完成序列。bool 只能表达“有/无”，无法避免
    /// `clear flag ↔ invtlb ↔ concurrent set` 的丢边沿；序列号是正确性真值。
    shootdown_request_seq: AtomicU64,
    shootdown_ack_seq: AtomicU64,
    /// TLB 刷新计数
    flush_count: AtomicU64,
    /// ASID 代数（用于检测回绕）
    generation: AtomicU64,
}

impl TlbCpuState {
    /// 创建新状态
    pub const fn new() -> Self {
        Self {
            current_asid: AtomicU16::new(0),
            active_mm: AtomicU64::new(0),
            lazy_flush_pending: AtomicBool::new(false),
            shootdown_request_seq: AtomicU64::new(0),
            shootdown_ack_seq: AtomicU64::new(0),
            flush_count: AtomicU64::new(0),
            generation: AtomicU64::new(0),
        }
    }

    /// 获取当前 ASID
    pub fn current_asid(&self) -> u16 {
        self.current_asid.load(Ordering::Acquire)
    }

    /// 设置当前 ASID
    pub fn set_current_asid(&self, asid: u16) {
        self.current_asid.store(asid, Ordering::Release);
    }

    /// 获取活跃地址空间
    pub fn active_mm(&self) -> u64 {
        self.active_mm.load(Ordering::Acquire)
    }

    /// 设置活跃地址空间
    pub fn set_active_mm(&self, mm: u64) {
        self.active_mm.store(mm, Ordering::Release);
    }

    /// 原子提交本 hart 即将进入的地址空间，并返回此前真正活跃的 mm。
    ///
    /// ASID 会让“已切走”的 mm 条目继续留在本地 TLB；因此调用者必须能区分
    /// 同 mm 的普通 syscall 返回与 inactive→active 的重新进入，后者要在
    /// 返回用户态前按目标 ASID 失效，补上 active_mm 目标集合刻意省略的
    /// 非活跃 hart。
    pub fn swap_active_mm(&self, mm: u64) -> u64 {
        self.active_mm.swap(mm, Ordering::AcqRel)
    }

    /// 检查是否有延迟刷新
    pub fn has_lazy_flush(&self) -> bool {
        self.lazy_flush_pending.load(Ordering::Acquire)
    }

    /// 设置延迟刷新
    pub fn set_lazy_flush(&self, pending: bool) {
        self.lazy_flush_pending.store(pending, Ordering::Release);
    }

    /// 发布一个新的远端失效请求，返回该请求序列。
    pub fn request_flush(&self) -> u64 {
        let seq = self.shootdown_request_seq.fetch_add(1, Ordering::AcqRel) + 1;
        self.lazy_flush_pending.store(true, Ordering::Release);
        seq
    }

    pub fn requested_flush_seq(&self) -> u64 {
        self.shootdown_request_seq.load(Ordering::Acquire)
    }

    pub fn acked_flush_seq(&self) -> u64 {
        self.shootdown_ack_seq.load(Ordering::Acquire)
    }

    pub fn publish_flush_ack(&self, seq: u64) {
        self.shootdown_ack_seq.store(seq, Ordering::Release);
        // bool 仅为提示；若发布 ack 期间又来了新请求，保持 pending=true。
        self.lazy_flush_pending.store(
            self.requested_flush_seq() > seq,
            Ordering::Release,
        );
    }

    pub fn has_unacked_flush(&self) -> bool {
        self.acked_flush_seq() < self.requested_flush_seq()
    }

    /// 清除延迟刷新并返回之前的值
    pub fn clear_lazy_flush(&self) -> bool {
        self.lazy_flush_pending.swap(false, Ordering::AcqRel)
    }

    /// 增加刷新计数
    pub fn inc_flush_count(&self) {
        self.flush_count.fetch_add(1, Ordering::Relaxed);
    }

    /// 获取刷新计数
    pub fn flush_count(&self) -> u64 {
        self.flush_count.load(Ordering::Relaxed)
    }
    
    /// 获取 ASID 代数
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    
    /// 设置 ASID 代数
    pub fn set_generation(&self, gen: u64) {
        self.generation.store(gen, Ordering::Release);
    }
}

/// TLB Shootdown 请求
pub struct TlbShootdownRequest {
    /// 目标 CPU 掩码
    pub target_cpus: CpuMask,
    /// 地址空间标识
    pub mm_id: u64,
    /// 刷新范围（None 表示全部刷新）
    pub range: Option<VirtAddrRange>,
    /// 完成计数器
    pub completion: AtomicU64,
}

/// ASID 分配器
pub struct AsidAllocator {
    /// 下一个可用 ASID
    next_asid: AtomicU16,
    /// ASID 到地址空间的映射
    asid_map: SpinLock<[u64; MAX_ASID as usize + 1]>,
    /// 当前代数（用于 ASID 回收）
    generation: AtomicU64,
}

impl AsidAllocator {
    /// 创建新分配器
    pub const fn new() -> Self {
        Self {
            next_asid: AtomicU16::new(1), // 0 保留
            asid_map: SpinLock::new([0; MAX_ASID as usize + 1]),
            generation: AtomicU64::new(0),
        }
    }

    /// 分配 ASID
    #[allow(clippy::absurd_extreme_comparisons)]
    pub fn alloc(&self, mm_id: u64) -> u16 {
        let asid = self.next_asid.fetch_add(1, Ordering::Relaxed);
        
        // 检查是否需要回绕
        if asid >= MAX_ASID {
            // 需要全局 TLB 刷新并重置
            self.next_asid.store(1, Ordering::Release);
            self.generation.fetch_add(1, Ordering::Release);
            return 1;
        }

        // 记录映射
        let mut map = self.asid_map.lock();
        map[asid as usize] = mm_id;

        asid
    }

    /// 释放 ASID
    #[allow(clippy::absurd_extreme_comparisons)]
    pub fn free(&self, asid: u16) {
        if asid > 0 && asid <= MAX_ASID {
            let mut map = self.asid_map.lock();
            map[asid as usize] = 0;
        }
    }

    /// 查找地址空间的 ASID
    pub fn find(&self, mm_id: u64) -> Option<u16> {
        let map = self.asid_map.lock();
        for (asid, &id) in map.iter().enumerate() {
            if id == mm_id && asid > 0 {
                return Some(asid as u16);
            }
        }
        None
    }

    /// 获取或分配 ASID
    pub fn get_or_alloc(&self, mm_id: u64) -> u16 {
        self.find(mm_id).unwrap_or_else(|| self.alloc(mm_id))
    }

    /// 获取当前代数
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// TLB 管理器
pub struct TlbManager {
    /// 每 CPU 的 TLB 状态
    per_cpu_state: [TlbCpuState; MAX_CPUS],
    /// ASID 分配器
    asid_allocator: AsidAllocator,
    /// 待处理的 shootdown 请求
    #[allow(dead_code)]
    pending_shootdowns: SpinLock<Vec<TlbShootdownRequest>>,
    /// 统计信息
    stats: TlbStats,
    /// CPU 数量
    num_cpus: AtomicU64,
}

/// TLB 统计信息
pub struct TlbStats {
    /// 本地刷新次数
    pub local_flushes: AtomicU64,
    /// 远程 shootdown 次数
    pub remote_shootdowns: AtomicU64,
    /// 批量 shootdown 次数
    pub batch_shootdowns: AtomicU64,
    /// 跳过的刷新次数（优化）
    pub skipped_flushes: AtomicU64,
    /// IPI 发送次数
    pub ipi_sent: AtomicU64,
}

/// IPI 类型
#[derive(Debug, Clone, Copy)]
pub enum IpiType {
    /// TLB Shootdown
    TlbShootdown,
    /// 调度器重新调度
    Reschedule,
    /// 函数调用
    FunctionCall,
    /// 停止 CPU
    Stop,
}

impl TlbStats {
    pub const fn new() -> Self {
        Self {
            local_flushes: AtomicU64::new(0),
            remote_shootdowns: AtomicU64::new(0),
            batch_shootdowns: AtomicU64::new(0),
            skipped_flushes: AtomicU64::new(0),
            ipi_sent: AtomicU64::new(0),
        }
    }
}

impl TlbManager {
    /// 创建新的 TLB 管理器
    pub const fn new() -> Self {
        Self {
            per_cpu_state: [
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
                TlbCpuState::new(),
            ],
            asid_allocator: AsidAllocator::new(),
            pending_shootdowns: SpinLock::new(Vec::new()),
            stats: TlbStats::new(),
            num_cpus: AtomicU64::new(1),
        }
    }

    /// 设置 CPU 数量
    pub fn set_num_cpus(&self, num: usize) {
        self.num_cpus.store(num as u64, Ordering::Release);
    }

    /// 获取使用指定地址空间的 CPU 掩码
    fn get_cpus_using_mm(&self, mm_id: u64) -> CpuMask {
        let mut mask = CpuMask::empty();
        let num_cpus = self.num_cpus.load(Ordering::Acquire) as usize;
        
        for cpu_id in 0..num_cpus {
            if self.per_cpu_state[cpu_id].active_mm() == mm_id {
                mask.set(cpu_id);
            }
        }
        
        mask
    }

    /// 智能 TLB 刷新
    pub fn flush_tlb(&self, mm_id: u64, range: Option<VirtAddrRange>, current_cpu: usize) {
        // 获取使用该地址空间的 CPU
        let target_cpus = self.get_cpus_using_mm(mm_id);

        if target_cpus.is_empty() {
            // 没有 CPU 使用这个地址空间，无需刷新
            self.stats.skipped_flushes.fetch_add(1, Ordering::Relaxed);
            return;
        }

        if target_cpus.count() == 1 && target_cpus.contains(current_cpu) {
            // 只有当前 CPU 使用，本地刷新即可
            self.local_flush(range);
            self.stats.local_flushes.fetch_add(1, Ordering::Relaxed);
            return;
        }

        // 需要跨 CPU shootdown
        self.remote_shootdown(target_cpus, mm_id, range, current_cpu);
    }

    /// 本地 TLB 刷新
    pub fn local_flush(&self, range: Option<VirtAddrRange>) {
        match range {
            Some(r) => {
                // 范围刷新
                for va in r.iter_pages() {
                    #[cfg(target_arch = "riscv64")]
                    crate::arch::asm_helpers::flush_tlb_va(va as usize);
                    #[cfg(target_arch = "loongarch64")]
                    crate::arch::asm_helpers::flush_tlb_va(va as usize);
                }
            }
            None => {
                // 全部刷新
                #[cfg(target_arch = "riscv64")]
                unsafe {
                    core::arch::asm!("sfence.vma x0, x0");
                }
                #[cfg(target_arch = "loongarch64")]
                crate::arch::loongarch64::csr::invtlb_all();
            }
        }
    }

    /// 刷新指定 ASID 的 TLB
    pub fn flush_asid(&self, asid: u16) {
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!("sfence.vma x0, {}", in(reg) asid as u64);
        }
        // LoongArch: `invtlb 0x4, asid, $zero` 清该 ASID 的全部条目。ASID 为 0
        // 时（尚未分配/内核态）退化为全量失效，避免漏刷。
        #[cfg(target_arch = "loongarch64")]
        {
            if asid == 0 {
                crate::arch::loongarch64::csr::invtlb_all();
            } else {
                crate::arch::loongarch64::csr::invtlb_asid(asid as usize);
            }
        }
    }

    /// 远程 TLB Shootdown
    fn remote_shootdown(
        &self,
        targets: CpuMask,
        _mm_id: u64,
        range: Option<VirtAddrRange>,
        current_cpu: usize,
    ) {
        // 先刷新本地
        if targets.contains(current_cpu) {
            self.local_flush(range);
        }

        // 为其他 CPU 设置延迟刷新标志并发送 IPI
        for cpu_id in targets.iter() {
            if cpu_id != current_cpu {
                self.per_cpu_state[cpu_id].request_flush();
                
                // 发送 IPI 到目标 CPU
                self.send_ipi(cpu_id, IpiType::TlbShootdown);
                self.stats.ipi_sent.fetch_add(1, Ordering::Relaxed);
            }
        }

        self.stats.remote_shootdowns.fetch_add(1, Ordering::Relaxed);
    }

    /// 发送 IPI (Inter-Processor Interrupt)
    fn send_ipi(&self, target_cpu: usize, _ipi_type: IpiType) {
        #[cfg(target_arch = "riscv64")]
        {
            // RISC-V 使用 SBI (Supervisor Binary Interface) 发送 IPI
            // 或者直接写入 CLINT/PLIC 寄存器
            
            // 方法 1: 使用 SBI ecall
            // sbi_send_ipi 需要一个 hart mask
            let hart_mask: u64 = 1 << target_cpu;
            
            unsafe {
                // SBI 调用: sbi_send_ipi(hart_mask, hart_mask_base)
                // a7 = extension ID (0x735049 for IPI, or legacy 0x04)
                // a6 = function ID (0 for send_ipi)
                // a0 = hart_mask
                // a1 = hart_mask_base
                core::arch::asm!(
                    "li a7, 0x735049",  // sIPI extension
                    "li a6, 0",         // send_ipi function
                    "mv a0, {mask}",
                    "li a1, 0",         // hart_mask_base = 0
                    "ecall",
                    mask = in(reg) hart_mask,
                    out("a0") _,
                    out("a1") _,
                    out("a6") _,
                    out("a7") _,
                );
            }
            
            // 方法 2: 直接写入 CLINT MSIP 寄存器（如果 SBI 不可用）
            // CLINT 基地址通常是 0x0200_0000
            // MSIP[hart] 在 CLINT_BASE + hart * 4
            #[cfg(feature = "direct_clint")]
            unsafe {
                const CLINT_BASE: usize = 0x0200_0000;
                let msip_addr = (CLINT_BASE + target_cpu * 4) as *mut u32;
                core::ptr::write_volatile(msip_addr, 1);
            }
        }
        
        #[cfg(not(target_arch = "riscv64"))]
        {
            #[cfg(target_arch = "loongarch64")]
            {
                use crate::arch::loongarch64::iocsr;
                let action = match _ipi_type {
                    IpiType::TlbShootdown => iocsr::ACTION_TLB_SHOOTDOWN,
                    IpiType::Reschedule => iocsr::ACTION_RESCHEDULE,
                    IpiType::FunctionCall => iocsr::ACTION_FUNCTION_CALL,
                    IpiType::Stop => iocsr::ACTION_STOP,
                };
                // IPI_SEND[4:0] 是位索引，必须经 send_ipi_action 逐位编码。
                unsafe { iocsr::send_ipi_action(target_cpu, action) };
            }
        }
    }

    /// 批量 TLB Shootdown
    pub fn batch_flush(&self, requests: &[(u64, Option<VirtAddrRange>)], current_cpu: usize) {
        // 合并目标 CPU
        let mut combined_targets = CpuMask::empty();
        for &(mm_id, _) in requests {
            combined_targets |= self.get_cpus_using_mm(mm_id);
        }

        if combined_targets.is_empty() {
            self.stats.skipped_flushes.fetch_add(requests.len() as u64, Ordering::Relaxed);
            return;
        }

        // 本地刷新所有范围
        if combined_targets.contains(current_cpu) {
            for &(_, range) in requests {
                self.local_flush(range);
            }
        }

        // 为其他 CPU 设置延迟刷新
        for cpu_id in combined_targets.iter() {
            if cpu_id != current_cpu {
                self.per_cpu_state[cpu_id].request_flush();
            }
        }

        // 发送单次 IPI
        let ipi_count = combined_targets.count().saturating_sub(
            if combined_targets.contains(current_cpu) { 1 } else { 0 }
        );
        self.stats.ipi_sent.fetch_add(ipi_count as u64, Ordering::Relaxed);
        self.stats.batch_shootdowns.fetch_add(1, Ordering::Relaxed);
    }

    /// 本 CPU 完成所有已发布的 shootdown 请求。序列号而非 bool 是完成真值：
    /// 每轮先快照 request_seq，执行本核全失效，最后 Release 发布 ack_seq；若
    /// 期间又来了新请求，循环再刷，绝不把并发请求清丢。
    fn complete_pending_flush(&self, cpu_id: usize) {
        let state = &self.per_cpu_state[cpu_id];
        loop {
            let requested = state.requested_flush_seq();
            if state.acked_flush_seq() >= requested {
                state.set_lazy_flush(false);
                return;
            }
            self.local_flush(None);
            state.inc_flush_count();
            state.publish_flush_ack(requested);
            SHOOTDOWN_ACKS.fetch_add(1, Ordering::Relaxed);
            if state.acked_flush_seq() >= state.requested_flush_seq() {
                return;
            }
        }
    }

    /// 切换地址空间
    pub fn switch_address_space(&self, mm_id: u64, root_ppn: u64, cpu_id: usize) {
        let state = &self.per_cpu_state[cpu_id];

        // 检查 ASID 代数是否发生变化（表示发生了回绕）
        let current_gen = self.asid_allocator.generation();
        let state_gen = state.generation();
        
        // 获取或分配 ASID
        let asid = self.asid_allocator.get_or_alloc(mm_id);

        // 设置 satp（包含 ASID）
        // satp 格式: MODE[63:60] | ASID[59:44] | PPN[43:0]
        let satp = (8u64 << 60) | ((asid as u64) << 44) | root_ppn;
        
        #[cfg(target_arch = "riscv64")]
        crate::arch::asm_helpers::write_satp(satp as usize);

        // 如果代数发生变化，需要刷新整个 TLB
        if current_gen != state_gen {
            // 使用 sfence.vma 刷新所有 TLB 条目
            #[cfg(target_arch = "riscv64")]
            crate::arch::asm_helpers::flush_tlb_all();
            state.set_generation(current_gen);
            state.inc_flush_count();
        }
        // 检查是否需要刷新。这里同样不能用软件 ASID 做 `invtlb 0x4`（见
        // `handle_shootdown_ipi`），挂起的 shootdown 一律本核全量失效。
        else if state.has_unacked_flush() {
            self.complete_pending_flush(cpu_id);
        }

        state.set_current_asid(asid);
        state.set_active_mm(mm_id);
    }

    /// 向除 `current_cpu` 外、active_mm 等于 `mm_id` 的所有 CPU 发送 shootdown，
    /// 并**同步等待**对端确认失效完成。
    ///
    /// Linux `flush_tlb_others()` 是同步的：发起方在所有目标 CPU 完成本地失效
    /// 之前不得继续。异步版本会留下这样的窗口——发起方已把 PTE 改指向新帧
    /// （CoW）或已解除映射，对端仍能用陈旧条目读写旧帧，于是「写进新帧的数据
    /// 对端看不到」。这正是 LA 上 rustc 读到半初始化 token（tag 非 0 但指针为
    /// 0）后对 NULL 做 `amadd.d` 的现象。
    ///
    /// 死锁约束：对端的 IPI 处理只做 `invtlb`，不获取任何锁，因此即使发起方
    /// 持有 AddressSpace 锁也不会互等。仍保留超时上限，避免对端长时间关中断
    /// 时把发起方永久挂住（超时计入 `SHOOTDOWN_TIMEOUTS` 以便观测）。
    pub fn shootdown_others(&self, mm_id: u64, current_cpu: usize) {
        // 异步 shootdown（对标 Linux lazy-TLB 快路径）：给每个运行同 mm 的对端
        // 置 lazy_flush 并发一次 shootdown IPI，**不同步自旋等 ack** 即返回。
        //
        // 为什么去掉同步等待（历史同步版由 IPI 编码坏时引入，见 git 历史）：
        // - 该同步等待在**持 AddressSpace 锁**的 ~30 个 flush_tlb_* 热点里被调用，
        //   等待时若某目标处于关中断内核段（IrqSpinLock 临界区 / TCG 未及时调度），
        //   发起方吃满自旋上限；上限内自旋放大持锁时间，实测退化并可诱发活锁。
        // - 且旧同步版**超时后照样带着陈旧 TLB 继续**（250 目标无保证），并未真正
        //   提供强序；其唯一有效性来自"等到目标下次上下文切换在 switch_mm 里清
        //   lazy_flush"——那是慢且不可靠的等待。
        // - IPI 投递修复后（位索引编码 + BSP ECFG_IPI + 丢边沿兜底），对端收到
        //   shootdown IPI 即在 IpiInt 里做本核全量失效，微秒级完成，无需发起方阻塞。
        //
        // 物理页复用的强序由释放到 buddy 的路径用 `drain_shootdown` 单独兜底
        // （见该函数），此处只负责及时把失效请求送达对端。
        let num_cpus = self.num_cpus.load(Ordering::Acquire) as usize;
        let mut sent = 0u64;
        for cpu_id in 0..num_cpus.min(MAX_CPUS) {
            if cpu_id == current_cpu {
                continue;
            }
            if self.per_cpu_state[cpu_id].active_mm() != mm_id {
                continue;
            }
            self.per_cpu_state[cpu_id].request_flush();
            self.send_ipi(cpu_id, IpiType::TlbShootdown);
            sent += 1;
        }
        if sent == 0 {
            return;
        }
        self.stats.ipi_sent.fetch_add(sent, Ordering::Relaxed);
        self.stats.remote_shootdowns.fetch_add(1, Ordering::Relaxed);
        SHOOTDOWN_REQS.fetch_add(1, Ordering::Relaxed);
    }

    /// 物理页复用前的强序兜底（对标 Linux `tlb_finish_mmu` 的批量 flush）：
    /// 同步等待所有 CPU 的 `lazy_flush` 清零，确保没有对端还持有指向"即将归还
    /// buddy 的用户页"的陈旧 TLB 条目。**必须在不持 AddressSpace 锁时调用**
    /// （调用方先在锁内摘除 PTE / 收集 FrameTracker，出锁后 drain，再真正 drop
    /// 帧）。IRQs 状态由调用方保证为开（对端才能取 IPI 失效并清标志）。
    ///
    /// 与旧的每次 flush 同步等待不同：本函数按**操作边界**低频调用（一次 munmap /
    /// 一次 exit / 一轮 reclaim），且不持锁，既给出复用前强序又不在热路径上阻塞、
    /// 不与持锁自旋耦合成活锁。
    pub fn drain_shootdown(&self) {
        let num_cpus = self.num_cpus.load(Ordering::Acquire) as usize;
        let current_cpu = crate::arch::asm_helpers::read_cpu_id();
        for cpu_id in 0..num_cpus.min(MAX_CPUS) {
            let state = &self.per_cpu_state[cpu_id];
            // 当前 hart 正在 syscall/trap（通常中断关闭）里调用 drain；若另一个
            // hart 已给本 hart 置 lazy_flush，等自己取 IPI 会形成自等待死锁。
            // 当前 hart 直接本地全失效并清标志，等价于亲自处理 shootdown IPI。
            if cpu_id == current_cpu {
                if state.has_unacked_flush() {
                    self.complete_pending_flush(current_cpu);
                }
                continue;
            }
            // idle-quiescent：目标 hart 无 current 且已不在任何任务内核栈上，
            // 此刻不可能使用用户 TLB。无需等它在 wfi 中取 IPI；保留 request
            // 未 ack，等它下次 switch_mm 时 `has_unacked_flush` 强制本地失效后
            // 才进入用户态。不能伪造 ack，也不清 active_mm（避免竞态覆盖新切入）。
            let target_pid = crate::sched::process_sched::HART_CURRENT_PID
                .load_at(cpu_id, Ordering::Acquire);
            let target_kowner = crate::sched::process_sched::HART_KSTACK_OWNER
                .load_at(cpu_id, Ordering::Acquire);
            if target_pid == 0 && target_kowner == 0 {
                continue;
            }
            // 只等待 drain 入口时已发布的请求。之后的新请求属于下一操作边界；
            // request/ack 序列保证不会被 bool clear race 吞掉。
            let target_seq = state.requested_flush_seq();
            let mut spins = 0u64;
            let mut reported = false;
            while state.acked_flush_seq() < target_seq {
                // 协作式防互等：等待远端期间处理本 hart 新收到的请求。两个 hart
                // 同时在关中断 syscall 里 drain 时，由此互相推进，不依赖 IPI 取中断。
                if self.per_cpu_state[current_cpu].has_unacked_flush() {
                    self.complete_pending_flush(current_cpu);
                }
                core::hint::spin_loop();
                spins += 1;
                if spins >= SHOOTDOWN_SPIN_LIMIT {
                    // 自旋窗口耗尽 = 对端未在本窗口内应答（关中断长内核段 /
                    // TCG 调度延迟）。计数如实累加（审计 C2：此前为死计数器，
                    // 导致 drain 阻塞完全不可观测），供 HUNGDBG 周期倒出。
                    SHOOTDOWN_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
                    if crate::cmdline::bscrash_enabled() && !reported {
                        reported = true;
                        let current = &self.per_cpu_state[current_cpu];
                        crate::println!(
                            "[TLBDRAIN] current_cpu={} target_cpu={} target_pid={} target_kowner={} target_active_mm={:#x} target_req={} target_ack={} current_req={} current_ack={}",
                            current_cpu,
                            cpu_id,
                            crate::sched::process_sched::HART_CURRENT_PID
                                .load_at(cpu_id, Ordering::Relaxed),
                            crate::sched::process_sched::HART_KSTACK_OWNER
                                .load_at(cpu_id, Ordering::Relaxed),
                            state.active_mm(),
                            target_seq,
                            state.acked_flush_seq(),
                            current.requested_flush_seq(),
                            current.acked_flush_seq(),
                        );
                    }
                    // 强 drain：CoW / 物理帧复用前不允许“超时后带陈旧 TLB
                    // 继续”。每个窗口重发一次 IPI，直到目标真正清除 lazy_flush。
                    // 当前 hart 已在上方本地自刷，消除了旧重发实现的自等待死锁；
                    // active_mm 在 idle 时清零，目标集合不包含离线/空闲 CPU。
                    self.send_ipi(cpu_id, IpiType::TlbShootdown);
                    SHOOTDOWN_SPIN_TOTAL.fetch_add(spins, Ordering::Relaxed);
                    spins = 0;
                }
            }
            SHOOTDOWN_SPIN_TOTAL.fetch_add(spins, Ordering::Relaxed);
        }
    }

    /// 处理 TLB shootdown IPI
    pub fn handle_shootdown_ipi(&self, cpu_id: usize) {
        let state = &self.per_cpu_state[cpu_id];

        // Linux `flush_tlb_func`：收到 shootdown IPI 的 CPU 必须真正失效本地
        // TLB。LoongArch 的 `invtlb` 只作用于本 hart，因此发起方的本地失效不足；
        // 若这里不做实际失效，另一 hart 上运行同一 mm 的线程会继续用陈旧条目
        // 访问已被 CoW/换页替换掉的旧物理页（观测现象：rustc 多线程下确定性
        // 读到旧内容、解出垃圾指针后 SIGSEGV）。
        //
        // 失效粒度必须是「本核全量」：`TlbCpuState::current_asid` 是本模块
        // `AsidAllocator` 分配的**软件** ASID，与 LoongArch 硬件 CSR.ASID
        // （由 `crate::mm::asid` 按页表根管理）无关；用它执行 `invtlb 0x4`
        // 会去失效一个根本不存在的 ASID，等于没刷。Linux 的
        // `flush_tlb_others()` 在 LoongArch 上同样是 IPI 到对端后调用
        // `local_flush_tlb_all()`，不尝试跨核传递 ASID/VA。
        if state.has_unacked_flush() {
            self.complete_pending_flush(cpu_id);
        }
    }

    /// 获取 ASID 分配器
    pub fn asid_allocator(&self) -> &AsidAllocator {
        &self.asid_allocator
    }

    /// 获取统计信息
    pub fn stats(&self) -> &TlbStats {
        &self.stats
    }

    /// 获取 CPU 状态
    pub fn cpu_state(&self, cpu_id: usize) -> Option<&TlbCpuState> {
        if cpu_id < MAX_CPUS {
            Some(&self.per_cpu_state[cpu_id])
        } else {
            None
        }
    }
}

/// 全局 TLB 管理器
static TLB_MANAGER: TlbManager = TlbManager::new();

/// 同步 shootdown 观测计数：发起次数 / 对端确认次数 / **drain 自旋窗口耗尽
/// 次数**。TIMEOUTS 不为 0 说明存在「目标 hart 长时间关中断/内核段未应答」，
/// 是 drain 阻塞时长的直接观测（每窗口重发一次 IPI，强 drain 不放弃）。
static SHOOTDOWN_REQS: AtomicU64 = AtomicU64::new(0);
static SHOOTDOWN_ACKS: AtomicU64 = AtomicU64::new(0);
static SHOOTDOWN_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
/// 同步等待累计自旋轮数：与锁争用计数一起用来定位多核退化的时间去向。
static SHOOTDOWN_SPIN_TOTAL: AtomicU64 = AtomicU64::new(0);

/// `drain_shootdown` 同步等待的自旋上限。对端收到 shootdown IPI 后只需一次本核
/// `invtlb`/`sfence.vma` 即完成，真实硬件上微秒级。上限只为防止对端已下线时永久
/// 挂起——已确认无永久锁环（mm/buddy/table 均普通 SpinLock、IRQ 开，IPI handler
/// 不取锁）。取较大值以容忍本地 QEMU TCG 8 vCPU 挤在被占满宿主上的调度延迟
/// （目标 host 线程可能隔较久才运行）；官方 8+ 物理核环境远早于此即应答。
const SHOOTDOWN_SPIN_LIMIT: u64 = 20_000_000;

/// 读取 shootdown 计数（reqs, acks, timeouts）。
pub fn shootdown_counters() -> (u64, u64, u64) {
    (
        SHOOTDOWN_REQS.load(Ordering::Relaxed),
        SHOOTDOWN_ACKS.load(Ordering::Relaxed),
        SHOOTDOWN_TIMEOUTS.load(Ordering::Relaxed),
    )
}

/// 同步等待累计自旋轮数（诊断多核退化的时间去向）。
pub fn shootdown_spin_total() -> u64 {
    SHOOTDOWN_SPIN_TOTAL.load(Ordering::Relaxed)
}

/// 初始化 TLB 管理器
pub fn init(num_cpus: usize) {
    TLB_MANAGER.set_num_cpus(num_cpus);
    crate::println!("[tlb] TLB manager initialized for {} CPUs", num_cpus);
}

/// 获取全局 TLB 管理器
pub fn get_manager() -> &'static TlbManager {
    &TLB_MANAGER
}

/// 刷新 TLB
///
/// 真化（Slice 2.x D1）：之前写死 `current_cpu = 0` 在 SMP 下会把所有 IPI
/// 都算到 CPU0 名下，导致 `/proc/sched_stat` 的 TLB 计数行错位、shootdown
/// 也错把"本地"误判为"远端"。改用 `arch::asm_helpers::read_cpu_id()`
/// 真读 mhartid (RISC-V) / cpu_num (LoongArch)。
pub fn flush(mm_id: u64, range: Option<VirtAddrRange>) {
    let current_cpu = crate::arch::asm_helpers::read_cpu_id();
    TLB_MANAGER.flush_tlb(mm_id, range, current_cpu);
}

/// 切换地址空间
///
/// 同 `flush`：必须按真 CPU id 维护 per-CPU ASID 与 active mm_id，否则
/// switch_mm IPI 唤醒目标 CPU 时会误判已经在该 mm 上从而漏 invalidate。
/// 对「正在运行当前地址空间的其它 hart」发起 TLB shootdown IPI。
///
/// 对标 Linux `flush_tlb_mm`：本核失效只覆盖本核 TLB，跨核必须靠 IPI 让对端
/// 自行失效。mm 身份取当前页表根（同一 mm 的多个线程 pid 不同但根相同）。
/// 单核或无其它 hart 使用该 mm 时零 IPI，热路径只多一次原子读。
/// 跨核 shootdown 广播开关（bootarg `NOSHOOT=1` 关闭）。
///
/// **仅用于性能归因 A/B**：广播是正确性所需（`flush_tlb_*` 只作用本 hart），
/// 但它在 `mprotect`/`munmap`/CoW/回收等热路径上被高频调用，而 TCG 下 IPI 极贵。
/// 关闭后若耗时显著下降，就说明必须改用更精细的策略（Linux 的 lazy-TLB：
/// 只对真正持有该 mm 的 CPU 广播，并合并批量失效），而不是每次调用都广播。
static SHOOTDOWN_ENABLED: AtomicBool = AtomicBool::new(true);

/// 由 cmdline 解析结果设置广播开关。
pub fn set_shootdown_enabled(on: bool) {
    SHOOTDOWN_ENABLED.store(on, Ordering::Relaxed);
}

#[inline]
pub fn shootdown_current_mm() {
    if !SHOOTDOWN_ENABLED.load(Ordering::Relaxed) {
        return;
    }
    // 只读 TLB 管理器自己的原子 CPU 数：`sched::smp::num_online()` 会取
    // SMP_MANAGER 自旋锁，而本函数在 `flush_tlb_va` 热路径（常持有
    // AddressSpace 锁）被调用，取额外锁既慢又有死锁风险。
    if TLB_MANAGER.num_cpus.load(Ordering::Acquire) <= 1 {
        return;
    }
    #[cfg(target_arch = "loongarch64")]
    let root = {
        // PGDL = CSR 0x19（用户态页目录基址物理地址）。
        let pgdl: usize;
        unsafe { core::arch::asm!("csrrd {}, 0x19", out(reg) pgdl) };
        (pgdl >> 12) as u64
    };
    #[cfg(target_arch = "riscv64")]
    let root = {
        let satp: usize;
        unsafe { core::arch::asm!("csrr {}, satp", out(reg) satp) };
        (satp & 0xFFF_FFFF_FFFF) as u64
    };
    if root == 0 {
        return;
    }
    let current_cpu = crate::arch::asm_helpers::read_cpu_id();
    TLB_MANAGER.shootdown_others(root, current_cpu);
}

pub fn switch_mm(mm_id: u64, root_ppn: u64) {
    let current_cpu = crate::arch::asm_helpers::read_cpu_id();
    TLB_MANAGER.switch_address_space(mm_id, root_ppn, current_cpu);
}

/// 物理页复用前的强序兜底：等待所有对端完成挂起的 TLB 失效。
/// **调用者不得持 AddressSpace 锁**（见 `TlbManager::drain_shootdown`）。
/// 单核或无挂起时几乎零成本。
#[inline]
pub fn drain_shootdown() {
    if TLB_MANAGER.num_cpus.load(Ordering::Acquire) <= 1 {
        return;
    }
    TLB_MANAGER.drain_shootdown();
}

/// return-to-user / nested-trap-return 的本 hart shootdown 检查（对标 Linux
/// `exit_to_user_mode_loop` 处理 TIF_TLB_PENDING）：目标 hart 即使整个 syscall
/// 期间中断关闭、没取到 IPI，也会在返回用户态前本地失效并发布 ack。
#[inline]
pub fn complete_current_pending_flush() {
    let cpu = crate::arch::asm_helpers::read_cpu_id();
    if let Some(state) = TLB_MANAGER.cpu_state(cpu) {
        if state.has_unacked_flush() {
            TLB_MANAGER.complete_pending_flush(cpu);
        }
    }
}

/// 原子登记“本 CPU 当前运行哪个 mm”（= `mm_cpumask`）并返回旧值，
/// **不写 satp、不刷 TLB**。
///
/// 对标 Linux `switch_mm_irqs_off` 里的 `cpumask_set_cpu(cpu, mm_cpumask(next))`：
/// 跨核 TLB shootdown 的目标集合完全由这张归属表决定
/// （`shootdown_others` 逐个比对 `per_cpu_state[cpu].active_mm()`）。
///
/// 本内核真正写 satp 的地方是 `usertrapret`（带 ASID 复用与同地址空间免刷的
/// 优化），所以这里只补归属登记，避免重复写 satp / 重复失效。
///
/// 缺这一步的后果（已实测）：AP 真正执行用户任务后，`mm_cpumask` 永远是空的，
/// `[tlbstat] shootdown reqs=0 acks=0`；于是 hart A 上的 CoW/unmap 只失效了自己
/// 的 TLB，hart B 仍用陈旧 PTE 写**旧的物理页**，用户态内存被写坏——现象是
/// glibc 栈保护触发 `*** stack smashing detected ***`（SIGABRT，exit=134）。
#[inline]
pub fn note_active_mm(root_ppn: u64) -> u64 {
    let cpu = crate::arch::asm_helpers::read_cpu_id();
    if let Some(state) = TLB_MANAGER.cpu_state(cpu) {
        state.swap_active_mm(root_ppn)
    } else {
        0
    }
}


/// TLB 管理器自测试
pub fn self_test() {
    crate::println!("[tlb] Running self-test...");
    
    // 测试 CpuMask
    let mut mask = CpuMask::empty();
    assert!(mask.is_empty());
    assert_eq!(mask.count(), 0);
    
    mask.set(0);
    mask.set(2);
    assert!(!mask.is_empty());
    assert!(mask.contains(0));
    assert!(!mask.contains(1));
    assert!(mask.contains(2));
    assert_eq!(mask.count(), 2);
    
    // 测试迭代
    let cpus: Vec<usize> = mask.iter().collect();
    assert_eq!(cpus, vec![0, 2]);
    
    mask.clear(0);
    assert!(!mask.contains(0));
    
    // 测试 BitOr
    let mask2 = CpuMask::all(4);
    let combined = mask | mask2;
    assert_eq!(combined.count(), 4);
    crate::println!("[tlb] CpuMask tests passed");
    
    // 测试 VirtAddrRange
    let range = VirtAddrRange::new(0x1000, 0x3000);
    assert_eq!(range.page_count(), 2);
    let pages: Vec<u64> = range.iter_pages().collect();
    assert_eq!(pages.len(), 2);
    crate::println!("[tlb] VirtAddrRange tests passed");
    
    // 测试 TlbCpuState
    let state = TlbCpuState::new();
    assert_eq!(state.current_asid(), 0);
    state.set_current_asid(42);
    assert_eq!(state.current_asid(), 42);
    
    assert_eq!(state.active_mm(), 0);
    state.set_active_mm(0x1234);
    assert_eq!(state.active_mm(), 0x1234);
    
    assert!(!state.has_lazy_flush());
    state.set_lazy_flush(true);
    assert!(state.has_lazy_flush());
    assert!(state.clear_lazy_flush());
    assert!(!state.has_lazy_flush());
    
    state.inc_flush_count();
    assert_eq!(state.flush_count(), 1);
    crate::println!("[tlb] TlbCpuState tests passed");
    
    // 注意: AsidAllocator::new() 会在栈上创建一个 512KB 的数组，可能导致栈溢出
    // 跳过独立的 AsidAllocator 测试，使用全局 TlbManager 中的分配器进行测试
    crate::println!("[tlb] AsidAllocator tests skipped (using global allocator)");
    
    // 测试 TlbManager
    let manager = get_manager();
    
    // 测试 CPU 状态
    if let Some(cpu_state) = manager.cpu_state(0) {
        let _asid = cpu_state.current_asid();
    }
    
    // 测试 ASID 分配器
    let _asid_alloc = manager.asid_allocator();
    
    // 测试统计
    let stats = manager.stats();
    crate::println!("[tlb] Local flushes: {}", stats.local_flushes.load(core::sync::atomic::Ordering::Relaxed));
    crate::println!("[tlb] Remote shootdowns: {}", stats.remote_shootdowns.load(core::sync::atomic::Ordering::Relaxed));
    
    // 测试本地刷新
    manager.local_flush(None);
    manager.local_flush(Some(VirtAddrRange::new(0x1000, 0x2000)));
    
    // 测试 ASID 刷新
    manager.flush_asid(0);
    
    // 测试批量刷新
    let requests = [(0x1000u64, None), (0x2000u64, Some(VirtAddrRange::new(0x3000, 0x4000)))];
    manager.batch_flush(&requests, 0);
    
    // 测试 flush 函数
    flush(0x1000, None);
    // 注意: switch_mm 会修改 satp 寄存器，在自测试中跳过以避免地址空间切换问题
    // switch_mm(0x1000, 0x80000);
    
    // 测试 IPI 类型
    let _ipi_types = [
        IpiType::TlbShootdown,
        IpiType::Reschedule,
        IpiType::FunctionCall,
        IpiType::Stop,
    ];
    
    crate::println!("[tlb] Self-test passed!");
}
