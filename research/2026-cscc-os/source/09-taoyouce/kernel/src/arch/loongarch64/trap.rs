//! # LoongArch64 异常/中断处理
//!
//! LoongArch64 trap (exception/interrupt) handling.
//!
//! ## 来源说明 / Source Attribution
//!
//! 异常处理框架参考自 PolyHAL 项目（https://github.com/Byte-OS/polyhal）
//! 和 NPUcore-IMPACT 项目的 LoongArch64 trap 处理代码。
//! 遵循 MIT/Apache-2.0 双协议。
//!
//! ## LoongArch64 异常模型 / Exception Model
//!
//! LoongArch64 使用 ESTAT 寄存器的 Ecode 字段区分异常类型：
//! - Ecode=0x0: 中断（通过 IS 位域区分具体中断源）
//! - Ecode=0x1-0x7: 页相关异常（PIL/PIS/PIF/PME/PNR/PNX/PPI）
//! - Ecode=0xB: 系统调用（syscall 指令）
//! - Ecode=0x3F: TLB 重填异常
//!
//! ## 与 RISC-V64 的对应关系
//!
//! | LoongArch64 | RISC-V64 | 说明 |
//! |-------------|----------|------|
//! | ESTAT.Ecode | scause | 异常原因 |
//! | ERA | sepc | 异常返回地址 |
//! | BADV | stval | 出错虚拟地址 |
//! | EENTRY | stvec | 异常入口地址 |
//! | PRMD | sstatus | 先前模式信息 |

#![allow(dead_code)]

use super::csr::*;
use super::context::La64TrapContext;

// ============================================================================
// 异常类型枚举 / Exception Type Enumeration
// ============================================================================

/// LoongArch64 异常/中断类型
///
/// LoongArch64 exception/interrupt type, derived from ESTAT.Ecode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum La64TrapCause {
    // ---- 中断 / Interrupts ----
    /// 软件中断 0 / Software Interrupt 0
    SoftwareInt0,
    /// 软件中断 1 / Software Interrupt 1
    SoftwareInt1,
    /// 硬件中断（HWI0-HWI7）/ Hardware Interrupt
    HardwareInt(u8),
    /// 定时器中断 / Timer Interrupt
    TimerInt,
    /// IPI 中断 / Inter-Processor Interrupt
    IpiInt,

    // ---- 页相关异常 / Page Exceptions ----
    /// 页无效异常（Load）/ Page Invalid — Load
    PageInvalidLoad,
    /// 页无效异常（Store）/ Page Invalid — Store
    PageInvalidStore,
    /// 页无效异常（Fetch）/ Page Invalid — Fetch
    PageInvalidFetch,
    /// 页修改异常 / Page Modification
    PageModification,
    /// 页不可读异常 / Page Not Readable
    PageNotReadable,
    /// 页不可执行异常 / Page Not Executable
    PageNotExecutable,
    /// 页特权等级不合规 / Page Privilege Violation
    PagePrivilegeViolation,

    // ---- 其他异常 / Other Exceptions ----
    /// 地址错误异常 / Address Error
    AddressError,
    /// 地址对齐错误 / Address Alignment Fault
    AlignmentFault,
    /// 系统调用 / System Call
    Syscall,
    /// 断点 / Breakpoint
    Breakpoint,
    /// 指令不存在 / Instruction Not Exist
    InstructionNotExist,
    /// 指令特权等级错误 / Instruction Privilege Error
    InstructionPrivilegeError,
    /// 浮点指令未使能 / Floating Point Unit Disabled
    FloatingPointDisabled,
    /// LSX（128 位 SIMD）指令未使能 / LSX Disabled
    LsxDisabled,
    /// LASX（256 位 SIMD）指令未使能 / LASX Disabled
    LasxDisabled,
    /// TLB 重填异常 / TLB Refill
    TlbRefill,

    /// 未知异常 / Unknown exception
    Unknown(usize),
}

impl La64TrapCause {
    /// 从 ESTAT 寄存器值解析异常类型
    ///
    /// Parse exception type from ESTAT register value.
    pub fn from_estat(estat: usize) -> Self {
        let ecode = estat_ecode(estat);
        let is_bits = estat_is(estat);

        match ecode {
            ECODE_INT => {
                // 中断：根据 IS 位域判断具体中断源
                // Interrupt: determine source from IS bits
                if is_bits & (1 << 0) != 0 {
                    Self::SoftwareInt0
                } else if is_bits & (1 << 1) != 0 {
                    Self::SoftwareInt1
                } else if is_bits & (1 << 11) != 0 {
                    Self::TimerInt
                } else if is_bits & (1 << 12) != 0 {
                    Self::IpiInt
                } else {
                    // HWI0-HWI7 对应 IS[2..10]
                    for i in 2..10 {
                        if is_bits & (1 << i) != 0 {
                            return Self::HardwareInt((i - 2) as u8);
                        }
                    }
                    Self::Unknown(estat)
                }
            }
            ECODE_PIL => Self::PageInvalidLoad,
            ECODE_PIS => Self::PageInvalidStore,
            ECODE_PIF => Self::PageInvalidFetch,
            ECODE_PME => Self::PageModification,
            ECODE_PNR => Self::PageNotReadable,
            ECODE_PNX => Self::PageNotExecutable,
            ECODE_PPI => Self::PagePrivilegeViolation,
            ECODE_ADEF | ECODE_ADEM => Self::AddressError,
            ECODE_ALE => Self::AlignmentFault,
            ECODE_SYS => Self::Syscall,
            ECODE_BRK => Self::Breakpoint,
            ECODE_INE => Self::InstructionNotExist,
            ECODE_IPE => Self::InstructionPrivilegeError,
            ECODE_FPD => Self::FloatingPointDisabled,
            ECODE_SXD => Self::LsxDisabled,
            ECODE_ASXD => Self::LasxDisabled,
            ECODE_TLBR => Self::TlbRefill,
            _ => Self::Unknown(estat),
        }
    }

    /// 是否是中断
    ///
    /// Check if this is an interrupt (not an exception).
    pub fn is_interrupt(&self) -> bool {
        matches!(
            self,
            Self::SoftwareInt0
                | Self::SoftwareInt1
                | Self::HardwareInt(_)
                | Self::TimerInt
                | Self::IpiInt
        )
    }

    /// 是否是页相关异常
    ///
    /// Check if this is a page-related exception.
    pub fn is_page_fault(&self) -> bool {
        matches!(
            self,
            Self::PageInvalidLoad
                | Self::PageInvalidStore
                | Self::PageInvalidFetch
                | Self::PageModification
                | Self::PageNotReadable
                | Self::PageNotExecutable
                | Self::PagePrivilegeViolation
        )
    }
}

// ============================================================================
// ECFG 中断使能位定义 / ECFG Interrupt Enable Bits
// ============================================================================

/// ECFG 中断使能：软件中断 0 (bit 0)
const ECFG_SWI0: usize = 1 << 0;
/// ECFG 中断使能：软件中断 1 (bit 1)
const ECFG_SWI1: usize = 1 << 1;
/// ECFG 中断使能：硬件中断 HWI0-HWI7 (bit 2-9)
const ECFG_HWI_ALL: usize = 0xFF << 2;
/// ECFG 中断使能：定时器中断 (bit 11)
const ECFG_TI: usize = 1 << 11;
/// ECFG 中断使能：IPI 中断 (bit 12)
const ECFG_IPI: usize = 1 << 12;

// ============================================================================
// Trap 初始化 / Trap Initialization
// ============================================================================

/// 初始化 LoongArch64 异常处理
///
/// Initialize LoongArch64 exception handling.
///
/// 设置 EENTRY 指向 trap 入口汇编，配置 ECFG 使能所需中断，
/// 初始化定时器，并将内核栈顶保存到 SAVE1。
///
/// # 调用时机
/// 在 rust_main 中，BSS 清零和基本初始化完成后调用。
pub fn init() {
    extern "C" {
        fn __la64_alltraps();
    }

    crate::println!("[la64-trap] Setting EENTRY...");
    // 1. 设置异常入口地址 EENTRY
    csrwr!(__la64_alltraps as usize, "0xC");

    crate::println!("[la64-trap] Setting ECFG (without timer)...");
    // 2. 配置 ECFG（先不使能 timer 中断）
    let ecfg_no_timer = ECFG_HWI_ALL | ECFG_IPI; // 先使能硬件和 IPI，不使能 timer
    csrwr!(ecfg_no_timer, "0x4");
    unsafe {
        crate::arch::loongarch64::iocsr::write32(
            crate::arch::loongarch64::iocsr::IPI_EN,
            u32::MAX,
        );
    }

    crate::println!("[la64-trap] Setting kernel SP in SAVE1...");
    // 3. 将内核栈顶保存到 SAVE1
    extern "C" {
        static boot_stack_top: u8;
    }
    let kernel_sp = unsafe { &boot_stack_top as *const u8 as usize };
    csrwr!(kernel_sp, "0x31");

    crate::println!("[la64-trap] Initializing timer...");
    // 4. 初始化定时器：one-shot 模式，每次 TimerInt 重 arm（hrtimer 风格，对齐 RV
    // stimecmp）。注意 TCFG.InitVal 字段占 bit[n:2]，真实倒计数初值是
    // {InitVal, 2'b0}，即写入的 cycles 低 2 位清零后直接作为计数值——旧周期
    // 模式代码 `(interval << 2)` 把 interval 写进 InitVal 字段，实际周期是预期
    // 的 4 倍（想要 0.25ms 实际 1ms，cyclictest tick-gap 实测 1007µs 实锤）。
    set_next_timer();
    crate::println!("[la64-trap] Timer armed one-shot (hrtimer style)");

    // 使能本 hart 的 IPI 接收（IOCSR IPI_EN 是 per-hart 的）。BSP 此前从未
    // 打开 ECFG.IPI / IPI_EN：AP 发往 BSP 的 shootdown/reschedule IPI 永远
    // 停在 pending，从不触发 IpiInt——与 IPI_SEND 位索引编码错误共同构成
    // `[tlbstat] acks≈0` 的两个断链点（init_secondary 早已为 AP 打开两者）。
    unsafe {
        crate::arch::loongarch64::iocsr::write32(
            crate::arch::loongarch64::iocsr::IPI_EN,
            u32::MAX,
        );
    }
    // 现在使能 ECFG 中的 timer + IPI 中断
    let ecfg_full = ECFG_TI | ECFG_HWI_ALL | ECFG_IPI;
    csrwr!(ecfg_full, "0x4");
    crate::println!("[la64-trap] ECFG updated with timer: {:#x}", ecfg_full);

    crate::println!("[la64-trap] Initializing TLB...");
    // 5. 初始化 TLB 子系统
    super::mm::init_tlb();

    crate::println!("[la64-trap] Initializing EIOINTC...");
    // 6. 初始化 EIOINTC（通过 IOCSR 指令）
    super::eiointc::init();

    crate::println!("[la64-trap] Trap initialization complete.");
}

/// 按「周期 tick 上界 + 最近定时睡眠到期点 + need_resched 立即性」三者取最紧，
/// 以 one-shot 模式重 arm TCFG（对齐 RV `set_next_timer` 的 hrtimer 风格）。
/// cyclictest 的 clock_nanosleep deadline 登记后能在到期点 µs 级精确触发中断，
/// 而不必等满一个周期 tick（旧 1ms 周期 + 相位随机 → 唤醒迟到最高数 ms）。
pub fn set_next_timer() {
    let now = crate::arch::asm_helpers::rdtime();
    // 周期兄弟 tick 上界 1ms（与旧实测周期一致，tick 负载零增量）。
    let tick: u64 = super::consts::CLOCK_FREQ as u64 / 1000;
    let min_ahead: u64 = tick / 100; // ~10µs：过期 deadline 立即重触发的最小提前量
    let mut delta = tick;

    // `nearest_deadline()` / `need_resched_pending()` 都是**全局**状态，表达的是
    // 「某个正在执行任务的 hart」的唤醒/抢占需求。空闲 hart（本 hart 没有当前
    // 任务）若也据此把周期收窄到 ~10µs，就会形成中断风暴：LA 8 核下 7 个空闲
    // AP 各自每 ~10µs 触发一次定时器中断，实测把 QEMU 占到 ~515% 宿主 CPU，
    // 而 QEMU TCG 下每个 vCPU 是一个宿主线程，这直接稀释真正在干活的 BSP。
    // Linux 的对应做法是空闲 CPU 进入 NOHZ_IDLE、停掉周期 tick。
    // 这里取最小等价行为：空闲 hart 只保留 1ms 基础 tick，不参与收窄。
    let hart_has_task = !crate::sched::process_sched::HART_CURRENT_PCB
        .load(core::sync::atomic::Ordering::Relaxed)
        .is_null();

    if hart_has_task {
        let dl = crate::sched::process::nearest_deadline();
        if dl != u64::MAX {
            let d = dl.saturating_sub(now).max(min_ahead);
            if d < delta {
                delta = d;
            }
        }
        // need_resched 待决但本次未能立即抢占：把下次中断收窄到 ~10µs，使抢占点
        // µs 级到来（与 RV 侧同一拖延根因的同一解法）。
        if crate::sched::process::need_resched_pending() && delta > min_ahead {
            delta = min_ahead;
        }
    }
    // TCFG 真实计数 = 写入值低 2 位清零（InitVal 字段占 bit[n:2]）。用周期模式
    // 自动重载（delta 即周期）作停摆兜底：即便某次中断在处理竞态中丢失，delta
    // 后硬件自动重触发；每次 TimerInt 尾部都会按最新 deadline 重写周期，不会
    // 留下过小周期造成中断风暴。最小钳 8 cycles 防写 0 关闭定时器。
    let tcfg_val: usize = ((delta.max(8) as usize) & !3) | TCFG_PERIODIC | TCFG_EN;
    unsafe {
        core::arch::asm!("csrwr {val}, 0x41", val = in(reg) tcfg_val, options(nostack));
    }
}

/// 立即按当前最近到期点重新 arm 定时器（与 RV `rearm_timer` 对齐）。入睡登记新
/// deadline 后调用，使该 deadline 不必等到下一个周期 tick 才生效。
#[inline]
pub fn rearm_timer() {
    set_next_timer();
}

// ============================================================================
// Trap Handler / 异常处理函数
// ============================================================================

/// 页错误 trap 处理（冷路径提取：让 la64_trap_handler 的 syscall 热路径少背
/// 这段大体量代码的寄存器压力/栈帧成本）。
#[inline(never)]
fn handle_page_fault_trap(cx: &mut La64TrapContext, cause: La64TrapCause) {
    let badv = csrrd!("0x7");
    let is_write = matches!(cause, La64TrapCause::PageInvalidStore);

    // Two-phase page fault handling (matches Linux mmap_lock protocol):
    //   Phase 1: Under PROCESS_TABLE lock — resolve non-IO faults or
    //            extract metadata for file-backed demand paging.
    //   Phase 2: OUTSIDE lock — perform file I/O + readahead.
    //   Phase 3: Re-acquire lock — install the populated page(s).
    let fault_addr = crate::mm::VirtAddr(badv);
    let (handled, fault_sig, needs_tlb_drain) = {
        use crate::sched::process::PROCESS_TABLE;
        use crate::mm::address_space::FaultAction;

        // --- Phase 1 ---
        let (action, as_pid_out) = {
            // PT 只解析 pid/mm 身份（对标 Linux fault 路径只持 mmap_lock、不持
            // tasklist 锁）：旧实现持 PT 等 mm 锁——缺页风暴（编译/lint 的分配
            // 潮）下各 hart 在 PT 上互排，实测单次持锁 100ms、全体 syscall 被
            // 征 6-30ms 税（ftrace-200501 全量 trace 证据，与 sys_mmap 收窄
            // 同一模式）。
            let (mm_arc, as_pid) = {
                let mut table = PROCESS_TABLE.lock();
                let r = if let Some(table) = table.as_mut() {
                    if let Some(current_pid) = table.current {
                        let as_pid = table.get(current_pid)
                            .map(|p| if p.mm.is_some() { current_pid } else { p.tgid })
                            .unwrap_or(current_pid);
                        (table.get(as_pid).and_then(|p| p.mm.as_ref()).cloned(), as_pid)
                    } else {
                        (None, 0)
                    }
                } else {
                    (None, 0)
                };
                r
                // PROCESS_TABLE lock released here
            };
            match mm_arc {
                Some(mm) => {
                    let mut addr_space = mm.lock();
                    (addr_space.try_resolve_fault(fault_addr, is_write), as_pid)
                }
                None => (FaultAction::Failed(11), as_pid),
            }
        };

        let __res = match action {
            FaultAction::Handled => (true, 11, false),
            FaultAction::HandledNeedsTlbDrain => (true, 11, true),
            FaultAction::Failed(sig) => (false, sig, false),
            FaultAction::NeedFileRead(info) => {
                // --- Phase 2: File I/O outside lock ---
                use alloc::sync::Arc;
                use crate::mm::page_cache;
                use crate::mm::frame_alloc_uninit;

                let page_index = info.file_offset / crate::mm::PAGE_SIZE;

                // Check page cache first (equivalent to filemap_fault)
                let cached = page_cache::lookup(info.inode_id, page_index);
                let fault_frame = if let Some(cached_frame) = cached {
                    if info.is_shared || !info.pte_flags.contains(crate::mm::PTEFlags::W) {
                        // 只读/可执行 MAP_PRIVATE 文件页与 MAP_SHARED 一样直接
                        // 共享页缓存帧（Linux filemap_fault 语义）；写路径才 CoW。
                        Some(cached_frame)
                    } else {
                        if let Some(private) = frame_alloc_uninit() {
                            let src = cached_frame.ppn.as_bytes_mut();
                            let dst = private.ppn.as_bytes_mut();
                            dst.copy_from_slice(src);
                            Some(crate::mm::SharedFrame::from_alloc(private))
                        } else {
                            None
                        }
                    }
                } else {
                    // Page cache miss: read from file + readahead
                    if let Some(file) = info.inode.as_file() {
                        let ra_count = {
                            let remaining_in_vma = (info.vma_end.0 - info.vma_start.0) / crate::mm::PAGE_SIZE;
                            let pages_from_fault = remaining_in_vma.saturating_sub(
                                (info.file_offset - info.vma_file_offset) / crate::mm::PAGE_SIZE
                            );
                            pages_from_fault.min(page_cache::DEFAULT_READAHEAD_PAGES)
                        };
                        page_cache::readahead(
                            info.inode_id,
                            page_index,
                            ra_count,
                            info.file_size,
                            |off, buf| {
                                match file.read(off, buf) {
                                    Ok(n) => n,
                                    Err(_) => 0,
                                }
                            },
                        );
                        let cached = page_cache::lookup(info.inode_id, page_index);
                        if let Some(cached_frame) = cached {
                            if info.is_shared || !info.pte_flags.contains(crate::mm::PTEFlags::W) {
                                Some(cached_frame)
                            } else {
                                if let Some(private) = frame_alloc_uninit() {
                                    let src = cached_frame.ppn.as_bytes_mut();
                                    let dst = private.ppn.as_bytes_mut();
                                    dst.copy_from_slice(src);
                                    Some(crate::mm::SharedFrame::from_alloc(private))
                                } else {
                                    None
                                }
                            }
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };

                // --- Phase 3: 重取 mm Arc（PT 只解析身份）后装页；不持 PT 等
                // mm 锁（与 Phase 1 同理由）。exec 并发换 mm 时本装页落在旧
                // 地址空间（Arc 兜底存活），返回用户态后重执行即在新 mm 上再
                // 缺页一次正确解析——对标 Linux 与 exec 竞态的 fault 语义。
                if let Some(frame) = fault_frame {
                    let mm2 = {
                        let table = PROCESS_TABLE.lock();
                        table.as_ref()
                            .and_then(|t| t.get(as_pid_out))
                            .and_then(|p| p.mm.as_ref())
                            .cloned()
                    };
                    if let Some(mm) = mm2 {
                        let mut addr_space = mm.lock();
                        let ok = addr_space.install_demand_page(
                            info.vpn,
                            frame,
                            info.vma_idx,
                            info.pte_flags,
                            info.vma_start,
                        );
                        if ok {
                            let ra_start = page_index + 1;
                            let ra_count = {
                                let remaining = (info.vma_end.0 - info.vma_start.0) / crate::mm::PAGE_SIZE;
                                let from_fault = remaining.saturating_sub(
                                    (info.file_offset - info.vma_file_offset) / crate::mm::PAGE_SIZE + 1
                                );
                                from_fault.min(page_cache::DEFAULT_READAHEAD_PAGES.saturating_sub(1))
                            };
                            if ra_count > 0 {
                                addr_space.install_readahead_pages(
                                    info.inode_id,
                                    info.vma_idx,
                                    info.vma_start,
                                    info.vma_end,
                                    info.vma_file_offset,
                                    info.pte_flags,
                                    ra_start,
                                    ra_count,
                                    info.is_shared,
                                );
                            }
                        }
                        (ok, 11, false)
                    } else {
                        (false, 11, false)
                    }
                } else {
                    (false, 11, false)
                }
            }
        };
        // cgroup v1 memory：缺页解析完成、锁均已释放后，做 memcg 计账 +
        // 压力回收（域内 RSS 超 limit 则回收）。快路径仅一次原子读即返回。
        if __res.0 {
            crate::mm::memcg::account_and_reclaim(as_pid_out);
        }
        __res
    };

    if handled {
        // 页错误处理成功（lazy allocation 或 CoW）。
        // LoongArch64: 必须刷新该地址的 TLB 条目，否则 TLB 中可能
        // 残留旧的无效条目，ertn 返回后重新执行指令会再次触发异常。
        // 仅本地失效：需要跨核同步的路径（CoW/权限升级）已在 mm 内部
        // flush_tlb_va 广播；fresh map 对其它 hart 无有效陈旧条目。
        crate::arch::asm_helpers::flush_tlb_va_local(badv);
        if needs_tlb_drain {
            // 强序必须在 PROCESS_TABLE / AddressSpace 锁**全部释放后**等待。
            // 仅 CoW/权限关键故障需要；普通 demand-zero / 文件按需装页无需全核
            // 同步（此前每个 page fault 都 drain，使 trivial 编译 >15min）。
            // 同时回收本进程 quarantine：CoW 摘除的旧帧在 drain 后才归还 buddy，
            // 杜绝「其它 hart 陈旧 TLB 命中已复用帧」的 rustc SIGSEGV 家族。
            crate::mm::address_space::AddressSpace::drain_and_reap_current_mm();
        }
    } else {
        // 检查是否是用户态页错误
        // Check if this is a user-mode page fault
        let prmd = cx.sstatus;
        let is_user_mode = (prmd & PRMD_PPLV_MASK) == 3; // PLV3 = 用户态

        if is_user_mode {
            // SIGSEGV (or SIGBUS for a file mapping page past EOF —
            // fault_sig): deliver to a catchable user handler (mprotect04
            // siglongjmps out of SIGSEGV, mmap13 catches SIGBUS) if
            // installed/unblocked, else terminate by signal.
            // check_and_deliver_signals builds the handler frame.
            // si_code matches Linux (see riscv64 trap): file-backed page
            // past EOF → BUS_ADRERR; bad access in a mapped region →
            // SEGV_ACCERR; unmapped address → SEGV_MAPERR.
            let si_code = if fault_sig == 7 {
                3 // BUS_ADRERR
            } else if crate::mm::current_user_vma_present(badv) {
                2 // SEGV_ACCERR
            } else {
                1 // SEGV_MAPERR
            };
            // 用户态致命访存的第一手事实：把 ERA/RA/SP/BADV 各自落在哪条 VMA、
            // 对应对象基址与文件偏移一次性打全（LAVMADBG 门控，默认零成本），
            // 避免只看到 rustc 侧 "interrupted by SIGSEGV" 而无法归属。
            if crate::cmdline::lavmadbg_enabled() && lavmadbg_take_fault_budget() {
                crate::println!(
                    "[LAVMADBG] user-fault sig={} si_code={} ERA={:#x} BADV={:#x} write={} cause={:?} tp={:#x} ra={:#x} sp={:#x} fp={:#x} s2={:#x}",
                    fault_sig, si_code, cx.sepc, badv, is_write, cause,
                    cx.x[2], cx.x[1], cx.x[3], cx.x[22], cx.x[25]
                );
                lavmadbg_dump(cx, badv);
            }
            segvdbg_fault("pf", cx, badv, cause);
            if crate::syscall::signal::try_queue_fault_signal(fault_sig, badv, si_code) {
                crate::syscall::check_and_deliver_signals(cx);
            } else {
                crate::sched::process::exit_group_with_reason(
                    crate::sched::process::ExitReason::signaled_with_dump(fault_sig),
                ); // SIGSEGV/SIGBUS by signal
            }
        } else {
            // 内核态页错误 — panic
            // Kernel page fault — panic
            crate::println!(
                "[la64-trap] Kernel page fault at ERA={:#x}, BADV={:#x}, cause={:?}",
                cx.sepc, badv, cause
            );
            panic!("Unhandled kernel page fault");
        }
    }
}

/// 打印预算：故障可能原地重抛形成死循环，若每次都全量 dump 会淹没串口并
/// 显著改变时序。只 dump 前 N 次，之后静默。
static LAVMADBG_DUMP_BUDGET: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(24);
static LAVMADBG_FAULT_BUDGET: core::sync::atomic::AtomicUsize =
    core::sync::atomic::AtomicUsize::new(96);

/// SEGVDBG=1：所有 `try_queue_fault_signal` 站点的统一第一手打印。
/// 与 LAVMADBG 的差异：LAVMADBG 只覆盖 handle_page_fault_trap 主路径且带 VMA
/// 邻域全量转储；SEGVDBG 在 PME / TLB-refill / 访问违规等**所有**投递点打一行
/// pid/ERA/BADV/cause/ra/sp，240 条预算（sigdbg_budget_ok）防嵌套信号循环
/// 淹没串口。默认关闭，零成本。
fn segvdbg_fault(tag: &'static str, cx: &La64TrapContext, badv: usize, cause: La64TrapCause) {
    if crate::cmdline::segvdbg_enabled() && crate::kernel::signal::sigdbg_budget_ok() {
        let pid = crate::sched::process_sched::HART_CURRENT_PID
            .load(core::sync::atomic::Ordering::Relaxed);
        crate::println!(
            "[SEGVDBG] site={} pid={} era={:#x} badv={:#x} cause={:?} ra={:#x} sp={:#x}",
            tag, pid, cx.sepc, badv, cause, cx.x[1], cx.x[3]
        );
    }
}

fn lavmadbg_take_fault_budget() -> bool {
    LAVMADBG_FAULT_BUDGET
        .fetch_update(
            core::sync::atomic::Ordering::Relaxed,
            core::sync::atomic::Ordering::Relaxed,
            |n| if n == 0 { None } else { Some(n - 1) },
        )
        .is_ok()
}

fn lavmadbg_dump(cx: &La64TrapContext, badv: usize) {
    if !crate::cmdline::lavmadbg_enabled() {
        return;
    }
    if LAVMADBG_DUMP_BUDGET
        .fetch_update(
            core::sync::atomic::Ordering::Relaxed,
            core::sync::atomic::Ordering::Relaxed,
            |n| if n == 0 { None } else { Some(n - 1) },
        )
        .is_err()
    {
        return;
    }

    let snapshot = {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        let Some(table) = table.as_ref() else {
            crate::println!("[LAVMADBG] process table unavailable");
            return;
        };
        let Some(pid) = table.current else {
            crate::println!("[LAVMADBG] no current process");
            return;
        };
        let Some(process) = table.get(pid) else {
            crate::println!("[LAVMADBG] current pid={} missing", pid);
            return;
        };
        let as_pid = if process.mm.is_some() { pid } else { process.tgid };
        let mm = table.get(as_pid).and_then(|p| p.mm.as_ref()).cloned();
        (pid, process.tgid, process.exec_path.clone(), as_pid, mm)
    };

    let (pid, tgid, exec_path, as_pid, Some(mm)) = snapshot else {
        crate::println!(
            "[LAVMADBG] pid={} tgid={} exec={} has no mm (as_pid={})",
            snapshot.0, snapshot.1, snapshot.2, snapshot.3
        );
        return;
    };
    let (sd_reqs, sd_acks, sd_timeouts) = crate::mm::tlb::shootdown_counters();
    crate::println!(
        "[LAVMADBG] pid={} tgid={} as_pid={} cpu={} shootdown reqs={} acks={} timeouts={} exec={}",
        pid, tgid, as_pid,
        crate::arch::asm_helpers::read_cpu_id(),
        sd_reqs, sd_acks, sd_timeouts, exec_path
    );
    {
        let (si, so_ok, so_cow, so_noslot, so_other, _) =
            crate::mm::address_space::dbg_swap_stats();
        crate::println!(
            "[LAVMADBG] swap si={} so_ok={} so_cow={} so_noslot={} so_other={}",
            si, so_ok, so_cow, so_noslot, so_other
        );
    }
    // 内核侧上下文归属：内核栈范围、TrapContext 位置与栈底 canary 状态。
    // 若寄存器污染源自内核栈越界写或内核栈复用，这三项能直接指认。
    {
        let table = crate::sched::process::PROCESS_TABLE.lock();
        if let Some(p) = table.as_ref().and_then(|t| t.get(pid)) {
            let (kb, kt) = p
                .kernel_stack
                .as_ref()
                .map(|ks| (ks.bottom, ks.top()))
                .unwrap_or((0, 0));
            crate::println!(
                "[LAVMADBG] kstack pid={} bottom={:#x} top={:#x} tcp={:#x} canary_ok={}",
                pid, kb, kt, p.trap_context_ptr, p.check_kernel_stack_canary()
            );
        }
    }

    let address_space = mm.lock();
    for (label, addr) in [
        ("ERA", cx.sepc),
        ("RA", cx.x[1]),
        ("SP", cx.x[3]),
        ("BADV", badv),
    ] {
        if let Some(vma) = address_space.find_vma(crate::mm::VirtAddr(addr)) {
            let file_offset = vma.file_offset.saturating_add(addr.saturating_sub(vma.start.0));
            let object_base = vma.start.0.saturating_sub(vma.file_offset);
            let inode = vma.backing.as_ref().map(|inode| inode.ino()).unwrap_or(0);
            crate::println!(
                "[LAVMADBG] {}={:#x} vma=[{:#x},{:#x}) type={:?} perm={}{}{} file_off={:#x} object_base={:#x} inode={} path={}",
                label,
                addr,
                vma.start.0,
                vma.end.0,
                vma.vma_type,
                if vma.permission.read { 'r' } else { '-' },
                if vma.permission.write { 'w' } else { '-' },
                if vma.permission.execute { 'x' } else { '-' },
                file_offset,
                object_base,
                inode,
                vma.file_path.as_deref().unwrap_or("<anonymous>"),
            );
        } else {
            crate::println!("[LAVMADBG] {}={:#x} no-vma", label, addr);
        }
    }
    // 整张 VMA 表 + `[VMAOP]` 生命周期日志：只有两者对照，才能把"该地址从未
    // 建立 VMA"与"曾建立后被哪次 unmap/mprotect 切没"分开。
    address_space.dump_all_vmas("user-fault");
    {
        let neighbors = address_space.vmas_near(crate::mm::VirtAddr(badv), 3);
        for (start, end, ty, r, w, x) in neighbors {
            crate::println!(
                "[LAVMADBG] near-badv vma=[{:#x},{:#x}) type={:?} perm={}{}{}",
                start, end, ty,
                if r { 'r' } else { '-' },
                if w { 'w' } else { '-' },
                if x { 'x' } else { '-' },
            );
        }
    }
    // 故障指令原文：把 ERA 前后各 4 条 32-bit 指令原样打出来，配合上面的
    // file_off/object_base 即可在宿主侧 objdump 精确定位到 DSO 内的指令，
    // 从而确定“写的是哪个寄存器算出的地址”。
    {
        let mut words = [0u32; 8];
        let base = cx.sepc.saturating_sub(16);
        let mut ok = true;
        for (i, w) in words.iter_mut().enumerate() {
            match address_space
                .page_table()
                .translate_va_to_kernel_ptr(base + i * 4)
            {
                Some(ptr) => *w = unsafe { core::ptr::read_volatile(ptr as *const u32) },
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            crate::println!(
                "[LAVMADBG] insn base={:#x} {:08x} {:08x} {:08x} {:08x} | {:08x} {:08x} {:08x} {:08x}",
                base, words[0], words[1], words[2], words[3],
                words[4], words[5], words[6], words[7]
            );
        } else {
            crate::println!("[LAVMADBG] insn base={:#x} unreadable", base);
        }
    }
    // 寄存器指向内存的内核视角快照：内核经页表直接读物理内存（不经用户 TLB），
    // 所以「内核读到的值」与「用户态刚读到的值」不一致 ⟹ 用户侧命中陈旧 TLB；
    // 一致 ⟹ 内存内容本身就是错的（写丢失 / CoW 复制错 / 分配未初始化）。
    for i in 4..32usize {
        let va = cx.x[i];
        if va < 0x1000 || va >= 0x1_0000_0000_0000 {
            continue;
        }
        let Some(vma) = address_space.find_vma(crate::mm::VirtAddr(va)) else {
            continue;
        };
        let vstart = vma.start.0;
        let vend = vma.end.0;
        let vty = vma.vma_type;
        let vperm = (
            vma.permission.read,
            vma.permission.write,
            vma.permission.execute,
        );
        let aligned = va & !7;
        let mut vals = [0u64; 4];
        let mut readable = true;
        for (k, v) in vals.iter_mut().enumerate() {
            match address_space
                .page_table()
                .translate_va_to_kernel_ptr(aligned + k * 8)
            {
                Some(ptr) => *v = unsafe { core::ptr::read_volatile(ptr as *const u64) },
                None => {
                    readable = false;
                    break;
                }
            }
        }
        if readable {
            let ppn = address_space
                .page_table()
                .translate(crate::mm::VirtAddr(aligned).floor())
                .map(|p| p.0)
                .unwrap_or(0);
            crate::println!(
                "[LAVMADBG] r{}={:#x} vma=[{:#x},{:#x}) {:?} {}{}{} ppn={:#x} mem[{:#x}..]={:#x} {:#x} {:#x} {:#x}",
                i, va, vstart, vend, vty,
                if vperm.0 { 'r' } else { '-' },
                if vperm.1 { 'w' } else { '-' },
                if vperm.2 { 'x' } else { '-' },
                ppn, aligned, vals[0], vals[1], vals[2], vals[3]
            );
            if ppn != 0 {
                crate::mm::frame_ownership_dump_dbg(ppn << 12, "lavmadbg-reg");
                // PTE 指向的帧 vs VMA frames 登记的帧：不一致 = 页表被外部
                // 修改过（或 frames 陈旧），是「读到错误帧内容」的铁证。
                let vpn = crate::mm::VirtPageNum(aligned >> 12);
                let fppn = address_space.frames_ppn_for_dbg(vpn);
                match fppn {
                    Some(fppn) if fppn.0 != ppn => crate::println!(
                        "[LAVMADBG] PTE-vs-FRAMES-MISMATCH va={:#x} pte_ppn={:#x} frames_ppn={:#x}",
                        aligned, ppn, fppn.0
                    ),
                    None => crate::println!(
                        "[LAVMADBG] NO-FRAMES-ENTRY va={:#x} pte_ppn={:#x}",
                        aligned, ppn
                    ),
                    _ => {}
                }
            }
        } else {
            crate::println!(
                "[LAVMADBG] r{}={:#x} vma=[{:#x},{:#x}) {:?} mem unreadable",
                i, va, vstart, vend, vty
            );
        }
    }
    // 栈帧原文：故障指令的上游操作数（局部变量、溢出寄存器、caller 保存值）
    // 都在 $fp/$sp 帧里。故障时上游寄存器往往已被覆盖，只有帧内容能回答
    // 「那个错的指针是从哪个槽位读出来的」。
    for (label, base, lo, hi) in [
        ("fp", cx.x[22], -64isize, 224isize),
        ("sp", cx.x[3], 0isize, 128isize),
    ] {
        if base < 0x1000 {
            continue;
        }
        let mut off = lo;
        while off < hi {
            let addr = (base as isize + off) as usize;
            let mut vals = [0u64; 4];
            let mut ok = true;
            for (k, v) in vals.iter_mut().enumerate() {
                match address_space
                    .page_table()
                    .translate_va_to_kernel_ptr(addr + k * 8)
                {
                    Some(ptr) => *v = unsafe { core::ptr::read_volatile(ptr as *const u64) },
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                crate::println!(
                    "[LAVMADBG] {}{}{:#x} @{:#x} = {:#x} {:#x} {:#x} {:#x}",
                    label,
                    if off < 0 { "-" } else { "+" },
                    off.unsigned_abs(),
                    addr,
                    vals[0], vals[1], vals[2], vals[3]
                );
            }
            off += 32;
        }
    }
    // 全量通用寄存器：NULL 写的地址一定来自某个寄存器（或 reg+imm），
    // 只有全量快照才能判断是哪个上游值变成了 0。
    crate::println!(
        "[LAVMADBG] regs r0-r7 {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x}",
        cx.x[0], cx.x[1], cx.x[2], cx.x[3], cx.x[4], cx.x[5], cx.x[6], cx.x[7]
    );
    crate::println!(
        "[LAVMADBG] regs r8-r15 {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x}",
        cx.x[8], cx.x[9], cx.x[10], cx.x[11], cx.x[12], cx.x[13], cx.x[14], cx.x[15]
    );
    crate::println!(
        "[LAVMADBG] regs r16-r23 {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x}",
        cx.x[16], cx.x[17], cx.x[18], cx.x[19], cx.x[20], cx.x[21], cx.x[22], cx.x[23]
    );
    crate::println!(
        "[LAVMADBG] regs r24-r31 {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x} {:#x}",
        cx.x[24], cx.x[25], cx.x[26], cx.x[27], cx.x[28], cx.x[29], cx.x[30], cx.x[31]
    );
}

/// BSCRASH=1 一次性无锁崩溃取证胶囊（FINAL-2026 BuildStorm 定位）。
///
/// 在首个致命异常点直接落盘全部裁决所需状态：CPU/PID/栈归属、故障 CSR 组、
/// 完整 TrapContext、当前 PCB 的内核栈边界，以及**故障 PC 附近的内存原文**。
/// 后者是本轮的关键：LA 上已观测 `ERA == trap_context_ptr`（CPU 把上下文
/// 当代码执行），dump 出该地址的 0x130 字节即可看到被写坏的上下文内容，
/// 从而直接指认写入者。全程只用 `bust_locks` 后的无锁 UART，不碰任何自旋锁。
fn bscrash_capsule(cx: &La64TrapContext, tag: &str) {
    use core::sync::atomic::Ordering;
    if !crate::cmdline::bscrash_enabled() {
        return;
    }
    crate::console::bust_locks();
    let hart = crate::arch::asm_helpers::read_cpu_id();
    let pid = crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed);
    let kowner = crate::sched::process_sched::HART_KSTACK_OWNER.load(Ordering::Relaxed);
    let staged_prev = crate::sched::process_sched::staged_previous_pid_at(hart);
    let staged_next = crate::sched::process_sched::staged_pid_at(hart);
    let badi = csrrd!("0x8");
    let estat = csrrd!("0x5");
    let crmd = csrrd!("0x0");
    let save1 = csrrd!("0x31");
    let pgdl = csrrd!("0x19");
    let sp_now: usize;
    unsafe { core::arch::asm!("move {}, $sp", out(reg) sp_now) };
    crate::println!(
        "[BSCRASH] tag={} hart={} pid={} kstack_owner={} staged_prev={} staged_next={} cx={:#x} sp_now={:#x}",
        tag, hart, pid, kowner, staged_prev, staged_next, cx as *const _ as usize, sp_now
    );
    crate::println!(
        "[BSCRASH] era={:#x} prmd={:#x} badv={:#x} badi={:#x} estat={:#x} crmd={:#x} save1={:#x} pgdl={:#x} cx.ksp={:#x}",
        cx.sepc, cx.sstatus, cx.badv, badi, estat, crmd, save1, pgdl, cx.kernel_sp
    );
    let pcb = crate::sched::process_sched::HART_CURRENT_PCB.load(Ordering::Relaxed);
    if !pcb.is_null() {
        let p = unsafe { &*pcb };
        crate::println!(
            "[BSCRASH] pcb pid={} tgid={} tcp={:#x} kstack_top={:#x} pt_root={:#x}",
            p.pid, p.tgid, p.trap_context_ptr, p.kernel_stack_top(), p.page_table_root.0
        );
    } else {
        crate::println!("[BSCRASH] pcb=null");
    }
    for base in 0..8 {
        let i = base * 4;
        crate::println!(
            "[BSCRASH] r{:02}-r{:02} {:#x} {:#x} {:#x} {:#x}",
            i, i + 3, cx.x[i], cx.x[i + 1], cx.x[i + 2], cx.x[i + 3]
        );
    }
    // 故障 PC 附近内存原文：仅 DMW0 内核直映射窗口（0x9000_...）可安全直读。
    let pc = cx.sepc & !0x7;
    if pc >> 60 == 0x9 {
        let lo = pc.saturating_sub(0x40);
        let mut addr = lo;
        while addr < pc.saturating_add(0x130) {
            let v0 = unsafe { core::ptr::read_volatile(addr as *const u64) };
            let v1 = unsafe { core::ptr::read_volatile((addr + 8) as *const u64) };
            let v2 = unsafe { core::ptr::read_volatile((addr + 16) as *const u64) };
            let v3 = unsafe { core::ptr::read_volatile((addr + 24) as *const u64) };
            crate::println!(
                "[BSCRASH] mem {:#x}: {:#018x} {:#018x} {:#018x} {:#018x}",
                addr, v0, v1, v2, v3
            );
            addr += 32;
        }
    }
    crate::println!("[BSCRASH] end tag={}", tag);
}

/// 所有 `la64_trap_handler → trap.S::__la64_restore` 返回路径的统一 KSP 收口。
///
/// 显式调度路径会调用 `usertrapret`（已按当前 PCB 栈顶修正），但普通 syscall /
/// timer / page-fault 的返回直接把本函数返回的 `cx` 交给汇编；此前这些路径会从
/// `cx.kernel_sp` 把陈旧的另一任务栈顶写进 per-hart SAVE1。下一次用户陷入便落到
/// 别人的内核栈（capsule: pid2→init 栈），导致上下文踩踏与 kernel-illegal-insn。
///
/// 对标 Linux 每次 return-to-user 刷 `TSS.sp0=current->thread.sp0`：以 per-hart
/// CURRENT_PCB 的 `kstack_top` 为唯一权威，覆盖即将恢复的 TrapContext。内核嵌套
/// trap 同样更新 SAVE1 为当前任务栈顶是安全的；仅用户态 mismatch 输出取证。
#[inline(always)]
fn authoritative_return_ksp(cx: &mut La64TrapContext) -> &mut La64TrapContext {
    use core::sync::atomic::Ordering;
    let returning_user = (cx.sstatus & PRMD_PPLV_MASK) == 3;
    // 若 trap handler 调度到了另一任务，switch_to 只把 next 暂存在 per-hart
    // staged 槽。先借 staged PCB 完成所有可能 fault 的收尾；真正发布 current
    // 必须推迟到函数末尾，否则 nested trap 会观察到 next pid + previous 栈。
    crate::mm::tlb::complete_current_pending_flush();
    let (return_pid, pcb, _) = if returning_user {
        crate::sched::process_sched::user_return_target()
    } else {
        (
            crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed),
            crate::sched::process_sched::HART_CURRENT_PCB.load(Ordering::Relaxed),
            false,
        )
    };
    if !pcb.is_null() {
        if returning_user {
            let user_pgd = unsafe { (*pcb).page_table_root.0 };
            if user_pgd != 0 {
                let root_ppn = (user_pgd >> 12) as u64;
                let reentering_mm = crate::mm::tlb::note_active_mm(root_ppn) != root_ppn;
                if crate::arch::asm_helpers::read_satp() != user_pgd || reentering_mm {
                    crate::arch::asm_helpers::activate_user_satp(user_pgd);
                }
                crate::mm::tlb::complete_current_pending_flush();
            }
        }
        let top = unsafe { (*pcb).kernel_stack_top() };
        if top != 0 {
            if crate::cmdline::bscrash_enabled()
                && returning_user
                && cx.kernel_sp != top
            {
                crate::println!(
                    "[BSCRASH_KSP] trap-return pid={} ctx_ksp={:#x} authoritative_top={:#x} era={:#x}",
                    return_pid,
                    cx.kernel_sp,
                    top,
                    cx.sepc,
                );
            }
            cx.kernel_sp = top;
        }
    }
    cx
}

/// LoongArch64 trap 处理函数（Rust 部分）
///
/// LoongArch64 trap handler (Rust part).
///
/// 🟡 微内核适配：读取 ESTAT 判断异常类型，分发到对应处理逻辑。
///
/// # 参数 / Parameters
/// - `cx`: 指向保存在内核栈上的 La64TrapContext
///
/// # 返回 / Returns
/// 返回 La64TrapContext 指针，用于恢复上下文。
/// 调度器可能返回不同进程的上下文指针。
#[no_mangle]
pub extern "C" fn la64_trap_handler(cx: &mut La64TrapContext) -> &mut La64TrapContext {
    // 只有「从用户态陷入」时 trap.S 才从 SAVE1 切到当前任务的真实内核栈；
    // 内核嵌套 trap 继续使用当时物理所在的旧栈。尤其 `switch_to` 已把
    // HART_CURRENT_PID 改成 next、但 Rust 仍在 previous 栈上执行的窗口里，
    // timer nested trap 若无条件把 OWNER 写成 next，就会提前撤销 previous 栈的
    // 保护，reaper 可回收/复用仍在执行的栈 → trap frame 被踩成 ERA=0。
    // 因此仅 user→kernel 真切栈时发布 current_pid；kernel nested 保留原 owner。
    let current_pid = crate::sched::process_sched::HART_CURRENT_PID
        .load(core::sync::atomic::Ordering::Relaxed);
    if (cx.sstatus & PRMD_PPLV_MASK) == 3 {
        crate::sched::process_sched::HART_KSTACK_OWNER
            .store(current_pid, core::sync::atomic::Ordering::SeqCst);
    }

    // BSCRASH=1 内核栈归属不变量：一次 trap 进来时，`cx`（=本次 trap 的
    // 内核 sp）必须落在「本 hart 当前 pid 自己的内核栈」区间内。若不落在，
    // 说明本 hart 以 pid=X 的身份却运行在别的任务的内核栈上（capsule 实证
    // hart6/pid2 跑在 init 栈 0x…09ab0000 上）。在**第一次**这样的 trap 处
    // 就地取证：打印 hart、running pid、sp、pid 自己的栈界，以及真正拥有该
    // sp 区间的任务 pid，直接区分「调度切栈错」与「野指针跳栈」。仅门控开销。
    if crate::cmdline::bscrash_enabled() {
        let sp = cx as *const _ as usize;
        let pcb = crate::sched::process_sched::HART_CURRENT_PCB
            .load(core::sync::atomic::Ordering::Relaxed);
        if !pcb.is_null() {
            let p = unsafe { &*pcb };
            // 只在 sp 明确落在**另一个任务**的内核栈里时报警：既排除本任务自己
            // 的栈，也排除 hart idle 栈（kstack_owner_of 对 idle 栈返回 None）。
            if p.kernel_stack_top() != 0 && !p.kernel_stack_contains(sp) {
                if let Some(owner) = crate::sched::process::kstack_owner_of(sp) {
                    if owner != current_pid {
                        crate::console::bust_locks();
                        crate::println!(
                            "[BSCRASH_STACK] hart={} run_pid={} sp={:#x} own_kstack_top={:#x} real_owner_pid={} era={:#x} ra={:#x}",
                            crate::arch::asm_helpers::read_cpu_id(),
                            current_pid, sp, p.kernel_stack_top(),
                            owner, cx.sepc, cx.x[1],
                        );
                    }
                }
            }
        }
    }

    if crate::RTDIAG {
        use core::sync::atomic::Ordering;
        let run_pid = crate::sched::process::RT_RUN_PID.load(Ordering::Relaxed);
        if run_pid != 0
            && crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed) == run_pid
        {
            let t0 = crate::sched::process::RT_RUN_TS.load(Ordering::Relaxed);
            let us = crate::arch::asm_helpers::rdtime().saturating_sub(t0) * 1_000_000
                / crate::arch::consts::CLOCK_FREQ as u64;
            crate::sched::process::rtstat_record(5, us);
            let estat_d = csrrd!("0x5");
            let tlbr_d = csrrd!("0x33");
            if tlbr_d != 0 {
                crate::sched::process::rtstat_record(6, 1);
            } else {
                let ecode = super::csr::estat_ecode(estat_d) as u64;
                if ecode != super::csr::ECODE_SYS as u64 {
                    crate::sched::process::rtstat_record(7, ecode);
                }
            }
        }
    }
    // 内核栈完整性抽检（EXECDBG 门控）：TrapContext 位于内核栈顶，若内核路径
    // 越界写（典型：路径/字符串缓冲区）冲掉它，用户寄存器恢复后就会带上无关
    // 数据（曾观测到 s2=0x2f2f2f2f2f2f2f2e，即 ASCII "///////."），随后在用户态
    // 以垃圾指针触发 SIGSEGV。这里只在栈底 canary 被破坏时打印，正常路径零输出。
    crate::sched::process::reap_detached_zombies();

    // canary 检查已验证无溢出，去掉每 trap 扫描以减少噪音
    // 如需复查溢出，取消下列注释：
    // let _ = crate::sched::process::PROCESS_TABLE.lock();

    // 读取 ESTAT 寄存器，判断异常类型
    let estat = csrrd!("0x5");
    let tlbr_fallback = csrrd!("0x33");
    if tlbr_fallback != 0 {
        csrwr!(0usize, "0x33");
    }
    let cause = if tlbr_fallback != 0 {
        La64TrapCause::TlbRefill
    } else {
        La64TrapCause::from_estat(estat)
    };

    // 软 tick（与 RV 侧对齐）：QEMU TCG 下硬件 timer 中断注入可迟到数 ms～百 ms，
    // 期间已到期的定时睡眠者（cyclictest RT 线程）迟迟不被唤醒。任何非中断 user
    // trap 入口发现最近到期点已过，就主动跑一次 timer_tick 服务到期唤醒。
    if !cause.is_interrupt() {
        // LA 汇编入口直接调本函数（trap_handler_rust 不在路径上），syscall 路径
        // 须在此现读 rdtime 并存入 HART_KERNEL_ENTER_TICK：既供 syscall 时间
        // 分账，也保证软 tick 用的是新鲜时钟——否则陈旧值使软 tick 对 syscall
        // trap 永不触发，长 syscall（clone 风暴）期间到期的 RT 睡眠者迟到数 ms。
        // 与 RV 侧对齐：无 armed deadline 且非精确分账进程时免读时钟。
        let is_sys = matches!(cause, La64TrapCause::Syscall);
        let need_acct = is_sys && crate::sched::process_sched::current_cputime_precise();
        let dl = crate::sched::process::nearest_deadline();
        if need_acct || dl != u64::MAX {
            let now = crate::arch::asm_helpers::rdtime();
            if need_acct {
                crate::sched::process::HART_KERNEL_ENTER_TICK
                    .store(now, core::sync::atomic::Ordering::Relaxed);
            }
            if now >= dl {
                if crate::RTDIAG {
                    let t0 = crate::arch::asm_helpers::rdtime();
                    crate::sched::timer_tick();
                    let us = crate::arch::asm_helpers::rdtime().saturating_sub(t0) * 1_000_000
                        / crate::arch::consts::CLOCK_FREQ as u64;
                    if us > 150 {
                        crate::rtdiag_println!("[rtdiag] soft-tick us={}", us);
                    }
                } else {
                    crate::sched::timer_tick();
                }
            }
        }
    }

    match cause {
        // ==================================================================
        // 系统调用处理 / Syscall Handling
        // ==================================================================
        La64TrapCause::Syscall => {
            let syscall_id = cx.syscall_id();
            let args = cx.syscall_args_la();
            // 真化〔seccomp pc 透传〕：LA64 `syscall 0` 指令的 era（=sepc）
            // 是该指令本身的地址，+4 之后跳过；保留原 PC 给 seccomp_data。
            let syscall_pc = cx.sepc;
            cx.sepc += 4;
            let diag_t0 = if crate::RTDIAG { crate::arch::asm_helpers::rdtime() } else { 0 };
            let result = crate::syscall::syscall(syscall_id, args, syscall_pc);
            if crate::RTDIAG {
                let dt = crate::arch::asm_helpers::rdtime().saturating_sub(diag_t0);
                let us = dt * 1_000_000 / crate::arch::consts::CLOCK_FREQ as u64;
                if us > 500 {
                    crate::rtdiag_println!("[rtdiag] slow-syscall nr={} us={}", syscall_id, us);
                }
            }
            if result == crate::syscall::SYSCALL_YIELD_SENTINEL {
                cx.x[4] = 0;
                if let Some((trap_cx_ptr, user_satp, deliver_signal)) =
                    crate::sched::process::switch_current_to_next(cx, crate::sched::trace::SWITCH_YIELD)
                {
                    if deliver_signal {
                        crate::syscall::check_and_deliver_signals(unsafe { &mut *trap_cx_ptr });
                    }
                    unsafe { usertrapret(&mut *trap_cx_ptr, user_satp); }
                }
            } else {
                cx.x[4] = result as usize;
                // child-runs-first 让位由 trap 出口的公共 handoff_child_for_trap
                // 调用点统一处理（免每 syscall 双调用）。
            }
            // 确保 PRMD.PIE=1（中断使能），PRMD.PPLV=3（用户态）
            // execve 后 sstatus 已被重置，但 syscall 处理中可能被修改
            if cx.sstatus & 0x4 == 0 || cx.sstatus & 0x3 != 3 {
                crate::println!("[la64-WARN] PRMD wrong after syscall {}: prmd={:#x}, fixing", syscall_id, cx.sstatus);
                cx.sstatus = 3 | (1 << 2); // PPLV=3, PIE=1
            }
            // sigreturn 与其它 syscall 一样走完整投递检查（Linux
            // exit_to_user_mode 语义）：handler 返回、掩码恢复后若仍有
            // 未屏蔽 pending 信号（如 sigprocmask 一次解除多个被阻塞
            // 信号），必须在回用户态前继续投递下一个（sigpending02）。
            if crate::sched::process::HART_PENDING_SIGNALS
                .load(core::sync::atomic::Ordering::Relaxed)
                != 0
            {
                // syscall() 内 account_syscall_combined_exit 刚在 PROCESS_TABLE
                // 锁内刷新了 HART_PENDING_SIGNALS（= current 的 pending & !mask），
                // 且本 trap 处理期间中断已关、其间无新信号投递窗口；故缓存为 0 时
                // lock-free 跳过 check_and_deliver_signals 的全局锁（§1.13①）。
                crate::syscall::check_and_deliver_signals(cx);
            }
        }

        // ==================================================================
        // 定时器中断处理 / Timer Interrupt Handling
        // ==================================================================
        La64TrapCause::TimerInt => {
            let diag_ti_t0 = if crate::RTDIAG { crate::arch::asm_helpers::rdtime() } else { 0 };
            let interrupted_user = (cx.sstatus & PRMD_PPLV_MASK) == 3;

            // 清除定时器中断（写 TICLR.CLR=1）
            super::timer::clear_timer_interrupt();
            crate::sched::record_interrupt(0);

            // HUNGDBG: coarse liveness sampler (~every 2s). Prints where the
            // hart is (user/kernel pc) plus the current task, so kernel-side
            // spins that never idle (invisible to the idle watchdog) leave a
            // trace on the serial log.
            if crate::cmdline::hungdbg_enabled() {
                static HDBG_TICKS: core::sync::atomic::AtomicU64 =
                    core::sync::atomic::AtomicU64::new(0);
                let n = HDBG_TICKS.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                if n % 8192 == 8191 {
                    let (cur, nr) = crate::sched::process::PROCESS_TABLE
                        .try_lock()
                        .and_then(|g| {
                            g.as_ref().map(|t| {
                                let cur = t.current.unwrap_or(0);
                                let nr = t.get(cur).map(|p| p.last_syscall.0 as i64).unwrap_or(-1);
                                (cur, nr)
                            })
                        })
                        .unwrap_or((0, -2));
                    crate::println!(
                        "[HUNGDBG] tick-sample user={} era={:#x} ra={:#x} cur={} nr={}",
                        interrupted_user, cx.sepc, cx.x[1], cur, nr
                    );
                }
            }

            // [DIAG] tick 间隔探针：周期 0.25ms，间隔 >1ms 说明中断被长内核路径关了
            {
                static LAST_TICK: core::sync::atomic::AtomicU64 =
                    core::sync::atomic::AtomicU64::new(0);
                let now = crate::arch::asm_helpers::rdtime();
                let last = LAST_TICK.swap(now, core::sync::atomic::Ordering::Relaxed);
                let freq = crate::arch::consts::CLOCK_FREQ as u64;
                let gap_us = now.saturating_sub(last) * 1_000_000 / freq;
                if false && last != 0 && gap_us > 1000 {
                    let (cur, nr) = crate::sched::process::PROCESS_TABLE
                        .try_lock()
                        .and_then(|g| {
                            g.as_ref().map(|t| {
                                let cur = t.current.unwrap_or(0);
                                let nr =
                                    t.get(cur).map(|p| p.last_syscall.0 as i64).unwrap_or(-1);
                                (cur, nr)
                            })
                        })
                        .unwrap_or((0, -2));
                    crate::rtdiag_println!(
                        "[rtdiag] tick-gap us={} user={} cur={} nr={} era={:#x}",
                        gap_us, interrupted_user, cur, nr, cx.sepc
                    );
                }
            }

            // [DIAG] 内核态采样 profiler：TimerInt 打断内核路径时打印内核 PC
            if false && !interrupted_user {
                crate::rtdiag_println!("[rtdiag] ksample era={:#x} ra={:#x}", cx.sepc, cx.x[1]);
            }

            // 触发调度器 tick
            // Trigger scheduler tick
            if crate::RTDIAG {
                let t0 = crate::arch::asm_helpers::rdtime();
                crate::sched::timer_tick();
                let us = crate::arch::asm_helpers::rdtime().saturating_sub(t0) * 1_000_000
                    / crate::arch::consts::CLOCK_FREQ as u64;
                crate::sched::process::rtstat_record(8, us);
            } else {
                crate::sched::timer_tick();
            }

            if crate::RTDIAG {
                let t1 = crate::arch::asm_helpers::rdtime();
                crate::net::net_stack::on_timer_tick();
                let t2 = crate::arch::asm_helpers::rdtime();
                let freq = crate::arch::consts::CLOCK_FREQ as u64;
                crate::sched::process::rtstat_record(9, (t2 - t1) * 1_000_000 / freq);
                crate::sched::process::rtstat_record(
                    6,
                    (t1 - diag_ti_t0) * 1_000_000 / freq,
                );
            } else {
                crate::net::net_stack::on_timer_tick();
            }

            // one-shot 模式：按最新 nearest_deadline/need_resched 重 arm 下一次中断
            set_next_timer();

            if interrupted_user {
                let diag_t1 = if crate::RTDIAG { crate::arch::asm_helpers::rdtime() } else { 0 };
                crate::syscall::check_and_deliver_signals(cx);
                if crate::RTDIAG {
                    let t2 = crate::arch::asm_helpers::rdtime();
                    crate::sched::process::rtstat_record(
                        10,
                        (t2 - diag_t1) * 1_000_000 / crate::arch::consts::CLOCK_FREQ as u64,
                    );
                }
                if let Some((trap_cx_ptr, user_satp, deliver_signal)) =
                    crate::sched::process::preempt_current_for_trap(cx, true, crate::sched::trace::SWITCH_TIMER)
                {
                    if deliver_signal {
                        crate::syscall::check_and_deliver_signals(unsafe { &mut *trap_cx_ptr });
                    }
                    unsafe { usertrapret(&mut *trap_cx_ptr, user_satp); }
                }
                if crate::RTDIAG {
                    let t3 = crate::arch::asm_helpers::rdtime();
                    crate::sched::process::rtstat_record(
                        11,
                        (t3 - diag_t1) * 1_000_000 / crate::arch::consts::CLOCK_FREQ as u64,
                    );
                }
            }
        }

        // ==================================================================
        // 硬件中断处理 / Hardware Interrupt Handling
        // ==================================================================
        La64TrapCause::HardwareInt(hwi) => {
            crate::sched::record_interrupt(hwi as usize);
            // 🟡 微内核适配：EIOINTC 中断路由
            //
            // LoongArch64 QEMU virt 平台的硬件中断通过 EIOINTC 路由：
            // PCI 设备中断 → EIOINTC → HWI 引脚 → CPU ESTAT.IS[2+hwi]
            //
            // 调用 EIOINTC 驱动查询具体中断源并分发到设备驱动。
            //
            // Hardware interrupts on LoongArch64 QEMU virt are routed through EIOINTC:
            // PCI device IRQ → EIOINTC → HWI pin → CPU ESTAT.IS[2+hwi]
            if super::eiointc::handle_irq(hwi) {
                crate::net::net_stack::on_net_interrupt();
            }
        }

        // ==================================================================
        // 页错误处理框架 / Page Fault Handling Framework
        // ==================================================================
        La64TrapCause::PageInvalidLoad
        | La64TrapCause::PageInvalidStore
        | La64TrapCause::PageInvalidFetch => {
            handle_page_fault_trap(cx, cause);
        }

        // ==================================================================
        // 页修改异常 / Page Modification Exception
        // ==================================================================
        //
        // PME 硬件语义：CPU store 命中 V=1 但 D=0 的 PTE。
        // 三层处理顺序：
        //   1. mark_dirty_la：PTE 已存在且软件 W 位 (bit8) 为 1，直接 OR 入 D 位。
        //      这是最常见的合法情形——新分配/CoW 后页面，HW dirty 跟踪初值 D=0。
        //   2. handle_page_fault(true)：CoW 真正发生（父子共享页）或 lazy 分配。
            //   3. 软件 PTE 已不存在：硬件命中的必是陈旧 TLB，本地失效后重试，
            //      让硬件按新页表产生精确 PageInvalidStore。
            //   4. 都失败：真正的访问违规，SIGSEGV。
        La64TrapCause::PageModification => {
            let badv = csrrd!("0x7");
            let fault_addr = crate::mm::VirtAddr(badv);
            let vpn = fault_addr.floor();
            // 调试：跟踪哪条路径处理了这次 PME
            //   0 = 都没处理成功；1 = mark_dirty_la；2 = handle_page_fault；
            //   3 = stale PME TLB invalidated
            let mut handled_by: u8 = 0;
            let handled = {
                use crate::sched::process::PROCESS_TABLE;
                let mut table = PROCESS_TABLE.lock();
                if let Some(table) = table.as_mut() {
                    if let Some(current_pid) = table.current {
                        let as_pid = table.get(current_pid)
                            .map(|p| if p.mm.is_some() { current_pid } else { p.tgid })
                            .unwrap_or(current_pid);
                        if let Some(mm) = table.get(as_pid).and_then(|p| p.mm.as_ref()).cloned() {
                            let mut addr_space = mm.lock();
                            if addr_space.page_table_mut().mark_dirty_la(vpn) {
                                handled_by = 1;
                                true
                            } else if addr_space.handle_page_fault(fault_addr, true) {
                                handled_by = 2;
                                true
                            } else if addr_space.page_table().translate(vpn).is_none() {
                                // PME 只能来自硬件命中的 present 条目；软件页表已无
                                // 映射时，它必然是本 hart 的陈旧 TLB。不能直接把
                                // SEGV_ACCERR 投给用户（rustc/collect2 会随机崩）；
                                // 本地全失效后重执行，由新页表给出 PIS/真实语义。
                                super::csr::invtlb_all();
                                handled_by = 3;
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            // PME only fires on a present (V=1) page, so this is always a
            // protection violation (writing a read-only page): SEGV_ACCERR.
            segvdbg_fault("pme", cx, badv, cause);
            if !handled && crate::syscall::signal::try_queue_fault_signal(11, badv, 2) {
                // Catchable SIGSEGV (write to a read-only page the task handles):
                // deliver to the user handler instead of terminating.
                crate::syscall::check_and_deliver_signals(cx);
            } else if !handled {
                crate::println!(
                    "[la64-trap] Unhandled PME ERA={:#x} BADV={:#x}",
                    cx.sepc, badv
                );
                // 一次性诊断 dump（仅前 3 次打印，避免刷屏）
                // 按 badv 区分独立点
                use core::sync::atomic::{AtomicUsize, Ordering};
                static DUMP_COUNT: AtomicUsize = AtomicUsize::new(0);
                let n = DUMP_COUNT.fetch_add(1, Ordering::Relaxed);
                if n < 3 {
                    use crate::sched::process::PROCESS_TABLE;
                    let table = PROCESS_TABLE.lock();
                    if let Some(t) = table.as_ref() {
                        if let Some(cur_pid) = t.current {
                            let as_pid = t.get(cur_pid)
                                .map(|p| if p.mm.is_some() { cur_pid } else { p.tgid })
                                .unwrap_or(cur_pid);
                            crate::println!("  [DUMP] cur_pid={} tgid_pid={}", cur_pid, as_pid);
                            if let Some(mm) = t.get(as_pid).and_then(|p| p.mm.as_ref()).cloned() {
                                let addr_space = mm.lock();
                                // Dump PTE raw value
                                if let Some(raw) = addr_space.page_table().peek_pte_raw_la(vpn) {
                                        crate::println!("  [DUMP] PTE raw={:#x} V={} D={} W(bit8)={} NR={} NX={} PLV={}",
                                            raw,
                                            (raw>>0)&1,
                                            (raw>>1)&1,
                                            (raw>>8)&1,
                                            (raw>>61)&1,
                                            (raw>>62)&1,
                                            (raw>>2)&3,
                                        );
                                    } else {
                                        crate::println!("  [DUMP] PTE: no leaf entry at vpn={:#x}", vpn.0);
                                    }
                                    // Dump VMAs covering or near the fault
                                    let mut printed = 0usize;
                                    for vma in addr_space.vmas_iter() {
                                        let in_range = fault_addr >= vma.start && fault_addr < vma.end;
                                        let near_range = {
                                            let gap = if badv < vma.start.0 {
                                                vma.start.0 - badv
                                            } else if badv >= vma.end.0 {
                                                badv - vma.end.0
                                            } else {
                                                0
                                            };
                                            gap < 0x10000
                                        };
                                        if in_range || near_range {
                                            crate::println!(
                                                "  [DUMP] VMA {:?} [{:#x},{:#x}) perm R={} W={} X={} U={} cow={}  {}",
                                                vma.vma_type,
                                                vma.start.0, vma.end.0,
                                                vma.permission.read as u8,
                                                vma.permission.write as u8,
                                                vma.permission.execute as u8,
                                                vma.permission.user as u8,
                                                vma.cow as u8,
                                                if in_range { "<== FAULT" } else { "(near)" },
                                            );
                                            printed += 1;
                                            if printed >= 6 { break; }
                                        }
                                    }
                                    if printed == 0 {
                                        crate::println!("  [DUMP] no VMA covers or near {:#x}; total VMAs={}",
                                            badv, addr_space.vma_count());
                                    }
                            }
                        }
                    }
                }
                let _ = handled_by;
                crate::sched::process::exit_group_with_reason(
                    crate::sched::process::ExitReason::signaled_with_dump(11),
                ); // SIGSEGV by signal
            } else {
                // 处理成功，本地刷新该地址旧条目（需跨核同步的路径已在
                // mm 内部广播，同 handled 路径的说明）
                crate::arch::asm_helpers::flush_tlb_va_local(badv);
            }
        }

        // ==================================================================
        // 地址对齐错误 / Address Alignment Fault
        // ==================================================================
        La64TrapCause::AlignmentFault => {
            let badv = csrrd!("0x7");
            crate::println!(
                "[la64-trap] Alignment fault at ERA={:#x}, BADV={:#x}",
                cx.sepc, badv
            );
            // 向用户进程发送 SIGBUS
            crate::sched::process::exit(-7); // SIGBUS
        }

        // ==================================================================
        // 非法指令 / Illegal Instruction
        // ==================================================================
        La64TrapCause::InstructionNotExist
        | La64TrapCause::InstructionPrivilegeError => {
            let is_user = (cx.sstatus & PRMD_PPLV_MASK) == 3;
            if is_user {
                // 用户态非法指令 → SIGILL(ILL_ILLOPC=1)，对标 Linux：装了 handler
                // 则投递让其恢复（crashme 捕获 SIGILL 后 siglongjmp 继续下一 trip），
                // 否则按信号终止该进程——绝不因用户态垃圾指令拖垮内核。
                segvdbg_fault("ill", cx, cx.sepc, cause);
                let q = crate::syscall::signal::try_queue_fault_signal(4, cx.sepc, 1);
                if q {
                    crate::syscall::check_and_deliver_signals(cx);
                } else {
                    crate::sched::process::exit_group_with_reason(
                        crate::sched::process::ExitReason::signaled_with_dump(4),
                    );
                }
            } else {
                let badi = csrrd!("0x8");
                // Fatal exception diagnostics must not recurse into a UART lock that may
                // already be held by this hart.  Match panic's oops_enter/bust_spinlocks
                // ordering before emitting the first diagnostic byte.
                crate::console::bust_locks();
                bscrash_capsule(cx, "kernel-illegal-insn");
                crate::println!(
                    "[la64-trap] Illegal instruction in kernel at ERA={:#x}, ESTAT={:#x}, BADI={:#x}, cause={:?}",
                    cx.sepc, estat, badi, cause
                );
                panic!("Illegal instruction in kernel");
            }
        }

        // ==================================================================
        // 浮点异常 / Floating Point Disabled
        // ==================================================================
        La64TrapCause::FloatingPointDisabled => {
            // 使能 FPU（EUEN.FPE = 1）并重试
            // Enable FPU (EUEN.FPE = 1) and retry
            csrxchg!(EUEN_FPE, EUEN_FPE, "0x2");
            // 不修改 ERA，重新执行触发异常的指令
        }

        // ==================================================================
        // LSX/LASX 向量单元未使能 / LSX & LASX Disabled
        //
        // 对标 Linux do_lsx/do_lasx：惰性使能对应向量单元后重试触发指令
        // （不修改 ERA）。LA464 含 LSX+LASX；若 CPU 实际不支持，使能写入
        // 无效、重试将转为 INE → 走非法指令路径投递 SIGILL。
        // ==================================================================
        La64TrapCause::LsxDisabled => {
            csrxchg!(EUEN_SXE, EUEN_SXE, "0x2");
        }
        La64TrapCause::LasxDisabled => {
            csrxchg!(EUEN_SXE | EUEN_ASXE, EUEN_SXE | EUEN_ASXE, "0x2");
        }

        // ==================================================================
        // 断点 / Breakpoint
        // ==================================================================
        La64TrapCause::Breakpoint => {
            crate::println!("[la64-trap] Breakpoint at ERA={:#x}", cx.sepc);
            cx.sepc += 4; // 跳过断点指令
        }

        // ==================================================================
        // TLB 重填异常 / TLB Refill
        // ==================================================================
        La64TrapCause::TlbRefill => {
            // 🔴 自己写：TLB 重填异常处理（Ecode=0x3F）
            //
            // 正常情况下，TLB 重填由 tlb_refill.S 中的汇编代码
            // 通过 LDDIR/LDPTE 硬件辅助指令直接处理，不会到达这里。
            //
            // 如果到达这里，说明汇编快速路径失败（例如页表中无映射），
            // 使用 Rust 软件回退路径处理。
            let badv = csrrd!("0x89");

            // 尝试软件 TLB 重填
            if !super::mm::la64_tlb_refill_handler(badv) {
                let is_user_mode = (cx.sstatus & PRMD_PPLV_MASK) == 3;
                // 缺页解析失败时投递的信号：默认 SIGSEGV，hugetlb 池耗尽则 SIGBUS。
                let fault_sig = 11;
                if is_user_mode {
                    match super::mm::handle_page_invalid(badv, false, false) {
                        Ok(()) => {
                            if super::mm::la64_tlb_refill_handler(badv) {
                                return authoritative_return_ksp(cx);
                            }
                        }
                        Err(_) => {
                            // refill 语境丢失访问类型，此处只能按读语义解析；
                            // 读解析失败 ≠ 该访问非法（如 PROT_WRITE-only VMA
                            // 的 store 首触）。填一条 V=0 条目后重执行，让硬件
                            // 抛精确的 PIL/PIS/PIF，由对应路径按真实访问类型
                            // 解析或投递带正确 si_code 的信号。
                            super::mm::fill_invalid_tlb_pair(badv);
                            return authoritative_return_ksp(cx);
                        }
                    }
                }
                // 重填失败：页表中无映射
                let prmd = cx.sstatus;
                let is_user_mode = (prmd & PRMD_PPLV_MASK) == 3;

                if is_user_mode {
                    // 投递 fault_sig：SIGBUS（hugetlb 池耗尽，hugemmap27）或 SIGSEGV。
                    // 若安装了可捕获 handler 则交给用户处理，否则按信号终止。
                    // si_code 对标 Linux：SIGBUS→BUS_ADRERR；映射存在但不可缺页→
                    // SEGV_ACCERR；地址完全未映射→SEGV_MAPERR。
                    let si_code = if fault_sig == 7 {
                        3 // BUS_ADRERR
                    } else if crate::mm::current_user_vma_present(badv) {
                        2 // SEGV_ACCERR
                    } else {
                        1 // SEGV_MAPERR
                    };
                    segvdbg_fault("tlbr", cx, badv, cause);
                    if crate::syscall::signal::try_queue_fault_signal(fault_sig, badv, si_code) {
                        // tlb_refill.S 的 .Lrefill_fail 已彻底退出 TLBR 语境
                        // （清 IsTLBR、CRMD 恢复 PG=1/DA=0），这里按普通异常
                        // 语义投递即可：check_and_deliver 改写 cx.sepc 指向
                        // handler，末尾普通 ertn 用 ERA/PRMD 返回用户态。
                        // （hugemmap27 LA：SIGBUS handler 由此可被正确投递。）
                        crate::syscall::check_and_deliver_signals(cx);
                    } else {
                        let cur_pid = {
                            let t = crate::sched::process::PROCESS_TABLE.lock();
                            t.as_ref().and_then(|t| t.current).unwrap_or(0)
                        };
                        // 读取当前 PGDL（硬件用来 walk 的页表根）
                        let pgdl_now = csrrd!("0x19");
                        let cx_addr = cx as *const La64TrapContext as usize;
                        let cur_tcp = {
                            let t = crate::sched::process::PROCESS_TABLE.lock();
                            t.as_ref()
                                .and_then(|t| t.get(cur_pid))
                                .map(|p| p.trap_context_ptr)
                                .unwrap_or(0)
                        };
                        crate::println!(
                            "[la64-TLB] pid={} era={:#x} badv={:#x} pgdl={:#x} sp={:#x} ra={:#x} cx={:#x} cur_tcp={:#x}",
                            cur_pid, cx.sepc, badv, pgdl_now, cx.x[3], cx.x[1], cx_addr, cur_tcp
                        );
                        bscrash_capsule(cx, "user-fatal-tlb");

                        crate::sched::process::exit_group_with_reason(
                            crate::sched::process::ExitReason::signaled_with_dump(fault_sig),
                        ); // SIGBUS/SIGSEGV by signal
                    }
                } else {
                    crate::console::bust_locks();
                    bscrash_capsule(cx, "kernel-tlb-refill-fail");
                    crate::println!(
                        "[la64-trap] Kernel TLB refill failed: ERA={:#x}, BADV={:#x}",
                        cx.sepc, badv
                    );
                    panic!("Unhandled kernel TLB refill");
                }
            }
        }

        // ==================================================================
        // 软件中断 / Software Interrupts
        // ==================================================================
        La64TrapCause::SoftwareInt0 | La64TrapCause::SoftwareInt1 => {
            // 清除软件中断（通过写 ESTAT 对应位）
            // Clear software interrupt
            // TODO: 实现 IPI 处理
        }

        // ==================================================================
        // IPI 中断 / Inter-Processor Interrupt
        // ==================================================================
        La64TrapCause::IpiInt => {
            let action = unsafe {
                crate::arch::loongarch64::iocsr::read32(
                    crate::arch::loongarch64::iocsr::IPI_STATUS,
                )
            };
            unsafe {
                crate::arch::loongarch64::iocsr::write32(
                    crate::arch::loongarch64::iocsr::IPI_CLEAR,
                    action,
                )
            };
            let cpu_id = crate::mm::percpu_cache::current_cpu_id();
            // `lazy_flush_pending` 是发起方在发 IPI 前置位的**持久**真相，而
            // IPI_STATUS 位是易失边沿：若本次 IpiInt 的 STATUS 读/清与另一发起方
            // 的置位撞车，shootdown 边沿会被 IPI_CLEAR 抹掉，action 里读不到该位
            // → 旧代码漏刷 → 发起方自旋到超时 → 残留旧 TLB（实测 timeouts=389 +
            // `realloc(): invalid old size` 堆损坏）。这里不再只信 action：只要本核
            // 有挂起的 lazy_flush 就执行本核失效并清标志，把丢边沿 race 彻底关闭。
            let shootdown_pending = crate::mm::tlb::get_manager()
                .cpu_state(cpu_id)
                .map(|s| s.has_unacked_flush())
                .unwrap_or(false);
            if shootdown_pending
                || action & crate::arch::loongarch64::iocsr::ACTION_TLB_SHOOTDOWN != 0
            {
                crate::mm::tlb::get_manager().handle_shootdown_ipi(cpu_id);
            }
            if action & crate::arch::loongarch64::iocsr::ACTION_RESCHEDULE != 0 {
                crate::sched::process::set_need_resched();
            }
        }

        // ==================================================================
        // 其他页相关异常 / Other Page Exceptions
        // ==================================================================
        La64TrapCause::PageNotReadable
        | La64TrapCause::PageNotExecutable
        | La64TrapCause::PagePrivilegeViolation => {
            let badv = csrrd!("0x7");
            // SIGSEGV: deliver to a catchable user handler if installed, else
            // terminate by signal. A read/execute/privilege violation always
            // hits a present mapping with insufficient permission → SEGV_ACCERR.
            segvdbg_fault("accvio", cx, badv, cause);
            if crate::syscall::signal::try_queue_fault_signal(11, badv, 2) {
                crate::syscall::check_and_deliver_signals(cx);
            } else {
                crate::println!(
                    "[la64-trap] Page access violation at ERA={:#x}, BADV={:#x}, cause={:?}",
                    cx.sepc, badv, cause
                );
                crate::sched::process::exit_group_with_reason(
                    crate::sched::process::ExitReason::signaled_with_dump(11),
                ); // SIGSEGV by signal
            }
        }

        // ==================================================================
        // 地址错误 / Address Error
        // ==================================================================
        La64TrapCause::AddressError => {
            let badv = csrrd!("0x7");
            let crmd = csrrd!("0x0");
            let dmw0 = csrrd!("0x180");
            let dmw1 = csrrd!("0x181");
            let pgdl = csrrd!("0x19");
            let prmd = cx.sstatus;
            let is_user = (prmd & PRMD_PPLV_MASK) == 3;
            let estat = csrrd!("0x5");
            let esubcode = (estat >> 22) & 0x1FF;
            crate::println!(
                "[la64-ADDRR] ERA={:#x} BADV={:#x} user={} CRMD={:#x} EsubCode={:#x}",
                cx.sepc, badv, is_user, crmd, esubcode
            );
            crate::println!(
                "[la64-ADDRR] DMW0={:#x} DMW1={:#x} PGDL={:#x}",
                dmw0, dmw1, pgdl
            );
            crate::println!(
                "[la64-ADDRR] ra={:#x} sp={:#x} a0={:#x} a1={:#x} a2={:#x}",
                cx.x[1], cx.x[3], cx.x[4], cx.x[5], cx.x[6]
            );
            crate::println!(
                "[la64-ADDRR] s0={:#x} s1={:#x} s2={:#x} s3={:#x} s4={:#x} fp={:#x} tp={:#x}",
                cx.x[23], cx.x[24], cx.x[25], cx.x[26], cx.x[27], cx.x[22], cx.x[2]
            );
            lavmadbg_dump(cx, badv);
            // 检查是否是用户态地址错误
            let prmd = cx.sstatus;
            let is_user = (prmd & PRMD_PPLV_MASK) == 3;
            if is_user {
                crate::sched::process::exit_group_with_reason(
                    crate::sched::process::ExitReason::signaled_with_dump(11),
                ); // SIGSEGV by signal
            } else if super::SHUTDOWN_IN_PROGRESS.load(core::sync::atomic::Ordering::Acquire) {
                // 关机 MMIO 写失败：QEMU 的 DMW 翻译在 LA virt 上会间歇把
                // PM1A/GED 写报成 AddressError。跳过当前这条 MMIO 写指令
                // 继续执行下一套关机方案；全部失败则自然落入 shutdown 末尾
                // 的 idle 死循环。这样既不伪造成功，也不制造 KERNEL PANIC。
                cx.sepc = cx.sepc.wrapping_add(4);
                return cx;
            } else {
                panic!("Kernel AddressError at ERA={:#x} BADV={:#x}", cx.sepc, badv);
            }
        }

        // ==================================================================
        // 未知异常 / Unknown Exception
        // ==================================================================
        La64TrapCause::Unknown(estat) => {
            let prmd = cx.sstatus;
            let is_user = (prmd & PRMD_PPLV_MASK) == 3;
            crate::println!(
                "[la64-trap] Unknown exception: ESTAT={:#x}, ERA={:#x}, is_user={}",
                estat, cx.sepc, is_user
            );
            if is_user {
                // 用户态未知异常 → 杀该进程，不打挂整个内核
                crate::sched::process::exit_group_with_reason(
                    crate::sched::process::ExitReason::signaled_with_dump(11),
                ); // SIGSEGV by signal
            } else {
                panic!("Unhandled LoongArch64 exception in kernel");
            }
        }
    }

    // need_resched 抢占点（与 RV 侧对齐）：本任务处于内核态期间若有 RT/DEADLINE
    // 任务被（软 tick / 定时器）唤醒，当时抢占被跳过；在此返回用户态前据标志补上，
    // 否则被唤醒的高优先级任务要空等到下一个硬件 TimerInt（迟到可达数 ms）。
    let interrupted_user = (cx.sstatus & PRMD_PPLV_MASK) == 3;

    // child-runs-first: clone 后立即让出给子线程。须在 need_resched 抢占之前
    // 判定——刚 clone 完的父进程若先被普通抢占选走他人，子线程首跑被推迟
    // （b_malloc_thread_stress icount 实测退化）。
    if let Some((trap_cx_ptr, user_satp, deliver_signal)) =
        crate::sched::process::handoff_child_for_trap(cx, crate::sched::trace::SWITCH_YIELD)
    {
        if deliver_signal {
            crate::syscall::check_and_deliver_signals(unsafe { &mut *trap_cx_ptr });
        }
        unsafe { usertrapret(&mut *trap_cx_ptr, user_satp); }
    }

    if interrupted_user && crate::sched::process::take_need_resched() {
        if let Some((trap_cx_ptr, user_satp, deliver_signal)) =
            crate::sched::process::preempt_current_for_trap(cx, true, crate::sched::trace::SWITCH_TIMER)
        {
            if deliver_signal {
                crate::syscall::check_and_deliver_signals(unsafe { &mut *trap_cx_ptr });
            }
            unsafe { usertrapret(&mut *trap_cx_ptr, user_satp); }
        }
    }

    // LoongArch64: PRMD 在 trap entry 时 hardware 已保存正确值（前一态 PPLV/PIE）
    // 不能强制改 PPLV — 如果前一态是 kernel (PPLV=0)，强制改成 PPLV=3 会让
    // ertn 后 CPU 切到 user mode，但 ERA 仍是 kernel 地址，触发 AddressError。
    // 唯一合法的修正：确保 PIE=1（避免 ertn 后中断被禁用）。

    authoritative_return_ksp(cx)
}

// ============================================================================
// 统一接口占位函数 / Unified Interface Placeholder Functions
// ============================================================================
//
// 以下函数为 crate::arch 统一导出所需的接口占位。
// 与 RISC-V64 的 usertrapret / trap_handler_rust 对应。
// 待 LoongArch64 用户态返回流程完善后替换为真实实现。

/// 用户态返回函数（LoongArch64）
///
/// 对应 RISC-V64 的 `usertrapret`。
/// 负责设置好 trap 上下文后通过 `ertn` 返回用户态。
///
/// 在 PG=1 模式下运行：
/// - DMW0 (VSEG=0x9000, PLV0) 让内核代码（链接在 0x9000_0000_002xxxxx）可访问
/// - DMW1 (VSEG=0x8000, PLV0) 让设备 MMIO 可访问
/// - 不使用 DMW2/DMW3，避免与用户态地址冲突
/// - 用户态地址通过 TLB 翻译
/// - PGDL 指向用户进程页表
///
/// # 参数
/// - `trap_cx`: 指向 TrapContext 的可变引用
/// - `user_satp`: 用户页表根地址（LoongArch64: PGDL 物理地址）
pub fn usertrapret(trap_cx: &mut La64TrapContext, user_satp: usize) -> ! {
    let (return_pid, return_pcb, _) =
        crate::sched::process_sched::user_return_target();
    // 先结清旧 active_mm 上已发布的请求；current 身份保持为物理栈 owner，
    // 直到所有 Rust 收尾完成。
    crate::mm::tlb::complete_current_pending_flush();
    // execve 可原地替换当前 pid 的 mm 而不经过 scheduler switch_to；若只在
    // switch_to 维护 active_mm，本 hart 会继续登记旧 PGDL，新 mm 的跨核
    // shootdown 漏掉它（capsule: 同 rustc 线程实际 root=0xb381，active_mm
    // 仍是旧 0xb065）。每次 return-to-user 都以即将写入的真实 PGDL 刷新
    // mm_cpumask，等价 Linux switch_mm_irqs_off 的最终权威提交点。
    let reentering_mm = if user_satp != 0 {
        let root_ppn = (user_satp >> 12) as u64;
        crate::mm::tlb::note_active_mm(root_ppn) != root_ppn
    } else {
        false
    };
    // 内核 → 用户的唯一出口：把当前进程"截至此刻"还没结账的内核段时间
    // 推到 cpu_kernel_ticks 桶里、cpu_in_kernel=false，让下一段用户态运行
    // 进入 cpu_user_ticks。与 RV64 `usertrapret` 完全对齐。
    {
        let now = crate::arch::asm_helpers::rdtime();
        if crate::RTDIAG {
            use core::sync::atomic::Ordering;
            let run_pid = crate::sched::process::RT_RUN_PID.load(Ordering::Relaxed);
            if run_pid != 0
                && crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed)
                    == run_pid
            {
                let t0 = crate::sched::process::RT_RUN_TS.load(Ordering::Relaxed);
                let us =
                    now.saturating_sub(t0) * 1_000_000 / crate::arch::consts::CLOCK_FREQ as u64;
                crate::sched::process::rtstat_record(0, us);
            }
        }
        crate::sched::process_sched::account_exit_kernel_for(return_pcb, now);
    }

    // 关闭中断
    super::disable_interrupts();

    // 设置 EENTRY 指向 trap 入口
    extern "C" { fn __la64_alltraps(); }
    csrwr!(__la64_alltraps as usize, "0xC");

    // 确保在 PG=1 模式下运行
    let crmd = csrrd!("0x0");
    let da_mode = (crmd & super::csr::CRMD_DA) != 0;

    if da_mode {
        // 首次从 DA=1 切换到 PG=1
        // 配置 DMW0: VSEG=0x9, MAT=1(CC), PLV0=1
        let dmw0: usize = 0x9000_0000_0000_0011;
        csrwr!(dmw0, "0x180");
        // 配置 DMW1: VSEG=0x8, MAT=0(SUC), PLV0=1
        let dmw1: usize = 0x8000_0000_0000_0001;
        csrwr!(dmw1, "0x181");
        // 不配置 DMW2 — 内核已链接在 DMW0 窗口内
        csrwr!(0usize, "0x182");
        csrwr!(0usize, "0x183");

        // 切换 CRMD: DA=0, PG=1
        let new_crmd = (crmd & !0x18) | 0x10;
        csrwr!(new_crmd, "0x0");
    }

    // 设置 PGDL（用户态页目录基址）。仅当目标地址空间与当前 PGDL
    // 不同（= 真正跨进程上下文切换）时才写 PGDL + 全量 invtlb。返回同一
    // 地址空间（绝大多数 syscall/中断/信号返回）时 PGDL 不变、TLB 仍有效
    // ——所有改用户页表的路径（缺页/COW/换入/munmap/mprotect/exec/fork）都已
    // 就地 invtlb 自己的条目，故此处无需再全量刷 TLB。内核走 DMW 直映射、
    // 不占用 PGDL，所以同进程返回时当前 PGDL == user_satp，原先无条件的
    // invtlb 是纯浪费。
    if user_satp != 0 && (csrrd!("0x19") != user_satp || reentering_mm) {
        // inactive hart 不在 shootdown 目标集合中，但可能保留该 ASID 的旧条目；
        // 即使 PGDL 恰好未变（idle 后恢复同 mm），重新进入也必须按 ASID 失效。
        crate::arch::asm_helpers::activate_user_satp(user_satp);
    }
    // active_mm 发布后可能并发收到新请求；返回用户态前再做一次最终结账。
    crate::mm::tlb::complete_current_pending_flush();

    // 设置 SAVE1 = kernel_sp（供下次 trap 使用）。
    //
    // 权威来源修正（对标 Linux 每次返回用户态刷 TSS.sp0 = 当前 task 的
    // 内核栈顶）：不再无条件信 `trap_cx.kernel_sp` 字段——它在某些恢复路径
    // 上可能已被写成别的任务的栈顶（capsule 实证：pid2 被恢复时 SAVE1/
    // kernel_sp = init 栈顶 → pid2 一陷入就落到 init 栈，踩坏双方上下文）。
    // 以本 hart 当前任务的 `kstack_top()` 为准，同时把 trap_cx.kernel_sp
    // 修回该值，令 __la64_restore 快路径也自洽。
    {
        if !return_pcb.is_null() {
            let top = unsafe { (*return_pcb).kernel_stack_top() };
            if top != 0 {
                if crate::cmdline::bscrash_enabled() && trap_cx.kernel_sp != top {
                    crate::println!(
                        "[BSCRASH_KSP] usertrapret pid={} ctx_ksp={:#x} authoritative_top={:#x}",
                        return_pid,
                        trap_cx.kernel_sp, top,
                    );
                }
                trap_cx.kernel_sp = top;
            }
        }
    }
    csrwr!(trap_cx.kernel_sp, "0x31");

    if crate::RTDIAG {
        use core::sync::atomic::Ordering;
        let run_pid = crate::sched::process::RT_RUN_PID.load(Ordering::Relaxed);
        if run_pid != 0
            && crate::sched::process_sched::HART_CURRENT_PID.load(Ordering::Relaxed) == run_pid
        {
            let t0 = crate::sched::process::RT_RUN_TS.load(Ordering::Relaxed);
            let us = crate::arch::asm_helpers::rdtime().saturating_sub(t0) * 1_000_000
                / crate::arch::consts::CLOCK_FREQ as u64;
            crate::sched::process::rtstat_record(4, us);
        }
    }

    // 硬件浮点寄存器组切换到即将运行的进程（owner 惰性切换，
    // 同进程返回只付一次原子比较）。对标 Linux switch_to 的 FPU 语义。
    if return_pid != 0 {
        super::fpu::switch_to(return_pid);
    }

    extern "C" {
        fn __la64_commit_and_restore(cx: *mut La64TrapContext) -> !;
    }
    unsafe { __la64_commit_and_restore(trap_cx as *mut La64TrapContext) }
}

/// trap 处理入口（LoongArch64 统一接口占位）
///
/// 对应 RISC-V64 的 `trap_handler_rust`。
/// 实际处理由 `la64_trap_handler` 完成，此函数作为统一接口的转发。
#[no_mangle]
pub extern "C" fn trap_handler_rust(cx: &mut La64TrapContext) -> &mut La64TrapContext {
    // 用户 → 内核唯一入口分账（与 RV64 `trap_handler_rust` 对齐）。
    {
        let now = crate::arch::asm_helpers::rdtime();
        let estat = csrrd!("0x5");
        let tlbr = csrrd!("0x33");
        let is_syscall = tlbr == 0
            && super::csr::estat_ecode(estat) == super::csr::ECODE_SYS;
        if is_syscall {
            // Syscall: store enter time, combined accounting in syscall exit
            crate::sched::process::HART_KERNEL_ENTER_TICK
                .store(now, core::sync::atomic::Ordering::Relaxed);
            if crate::RTDIAG && (estat & (1 << 11)) != 0 {
                let crmd = csrrd!("0x0");
                let prmd = csrrd!("0x1");
                crate::rtdiag_println!(
                    "[rtdiag] ti-pending-at-syscall estat={:#x} crmd={:#x} prmd={:#x}",
                    estat, crmd, prmd
                );
            }
        } else {
            if crate::RTDIAG {
                crate::sched::process::HART_KERNEL_ENTER_TICK
                    .store(now, core::sync::atomic::Ordering::Relaxed);
            }
            crate::sched::process::account_enter_kernel(now);
        }
    }
    la64_trap_handler(cx)
}

/// 按 PID 返回用户态（LoongArch64）
///
/// ## 地址翻译模式切换方案
///
/// 内核链接在 DMW0 窗口地址 0x9000_0000_0020_0000。
/// entry.S 已经在启动时完成 DA→PG 切换并跳转到高地址。
/// 因此 usertrapret_by_pid 被调用时已经在 PG=1 模式下运行。
///
/// DMW 配置：
/// - DMW0: VSEG=0x9000, MAT=CC, PLV0=1（内核代码和数据访问）
/// - DMW1: VSEG=0x8000, MAT=SUC, PLV0=1（设备 MMIO 访问）
/// - DMW2/DMW3: 不使用（避免与用户态地址冲突）
///
/// 用户态地址通过 TLB 翻译（PLV3 不匹配任何 DMW）。
pub fn usertrapret_by_pid(pid: usize) -> ! {
    use crate::sched::process::PROCESS_TABLE;

    if crate::RTDIAG {
        let now = crate::arch::asm_helpers::rdtime();
        let t0 = crate::sched::process::HART_KERNEL_ENTER_TICK
            .load(core::sync::atomic::Ordering::Relaxed);
        let us = now.saturating_sub(t0) * 1_000_000 / crate::arch::consts::CLOCK_FREQ as u64;
        if t0 != 0 && us > 500 {
            crate::rtdiag_println!("[rtdiag] slow-kpath-bypid us={} next={}", us, pid);
        }
    }

    // 本 hart 即将运行 `pid`：先在锁内完成"上 CPU"记账（置 Running、摘出就绪环、
    // 设本 hart 的 current）。否则该进程仍是 Runnable 且留在就绪环里，另一个 hart
    // 的 `select_next` 会同时选中它，两个 hart 共用同一份 TrapContext 与内核栈。
    // 对标 Linux `context_switch` 的 dequeue + `TASK_RUNNING` + per-CPU `current`。
    // `switch_to` 返回 false = 已被别的 hart 抢先运行，本 hart 转去挑别的任务。
    let took_cpu = {
        let mut guard = PROCESS_TABLE.lock();
        guard
            .as_mut()
            .map_or(false, |t| t.switch_to(pid, crate::sched::trace::SWITCH_YIELD))
    };
    if !took_cpu {
        crate::sched::enter_idle_scheduler_loop();
    }

    // 获取进程的 TrapContext 地址和页表根地址
    let (trap_cx_ptr, user_pgd) = {
        let table = PROCESS_TABLE.lock();
        if let Some(table) = table.as_ref() {
            if let Some(process) = table.get(pid) {
                let pgd = process.page_table_root.0; // 页表根物理地址
                (process.trap_context_ptr, pgd)
            } else {
                panic!("usertrapret_by_pid: process {} not found", pid);
            }
        } else {
            panic!("usertrapret_by_pid: process table not initialized");
        }
    };

    if trap_cx_ptr == 0 {
        panic!("usertrapret_by_pid: trap_cx_ptr is null for pid {}", pid);
    }

    crate::mm::tlb::complete_current_pending_flush();
    let reentering_mm = if user_pgd != 0 {
        let root_ppn = (user_pgd >> 12) as u64;
        crate::mm::tlb::note_active_mm(root_ppn) != root_ppn
    } else {
        false
    };

    // 关闭中断
    super::disable_interrupts();

    // 设置 EENTRY 指向 trap 入口
    extern "C" { fn __la64_alltraps(); }
    csrwr!(__la64_alltraps as usize, "0xC");

    // 调试输出（仅首次切换时打印）
    #[cfg(feature = "debug")]
    crate::println!("[la64] usertrapret: trap_cx={:#x} pgd={:#x}", trap_cx_ptr, user_pgd);

    // ====================================================================
    // 切换到 PG=1 分页模式（仅首次需要，后续 trap 返回时已在 PG=1 模式）
    // Switch to PG=1 paged mode
    // ====================================================================
    let crmd = csrrd!("0x0");
    let da_mode = (crmd & super::csr::CRMD_DA) != 0;

    if da_mode {
        // entry.S 应该已经完成了 DA→PG 切换，正常不应该走到这里
        // 但作为安全保障，仍然处理这种情况
        #[cfg(feature = "debug")]
        crate::println!("[la64] WARNING: Still in DA=1 mode at usertrapret_by_pid!");

        // 1. 配置 DMW0: VSEG=0x9, MAT=1(CC), PLV0=1
        let dmw0: usize = 0x9000_0000_0000_0011;
        csrwr!(dmw0, "0x180");

        // 2. 配置 DMW1: VSEG=0x8, MAT=0(SUC), PLV0=1
        let dmw1: usize = 0x8000_0000_0000_0001;
        csrwr!(dmw1, "0x181");

        // 3. 不配置 DMW2 — 内核已链接在 DMW0 窗口内
        csrwr!(0usize, "0x182");

        // 4. 清除 DMW3
        csrwr!(0usize, "0x183");

        // 5. 设置 PGDL（用户态页目录基址）+ ASID（首次 DA→PG 切换，TLB 已干净）
        if user_pgd != 0 {
            crate::arch::asm_helpers::activate_user_satp(user_pgd);
        }

        // 6. 切换 CRMD: DA=0, PG=1
        let new_crmd = (crmd & !0x18) | 0x10;
        csrwr!(new_crmd, "0x0");

        #[cfg(feature = "debug")]
        crate::println!("[la64] PG=1 mode enabled, CRMD={:#x}", csrrd!("0x0"));
    } else {
        // 已经在 PG=1 模式，只需更新 PGDL；仅当跨地址空间（PGDL 变化）
        // 时才写 PGDL + 全量 invtlb（同理见上方 usertrapret 说明）。
        if user_pgd != 0 && (csrrd!("0x19") != user_pgd || reentering_mm) {
            crate::arch::asm_helpers::activate_user_satp(user_pgd);
        }
    }
    crate::mm::tlb::complete_current_pending_flush();

    // 在跳转到 __la64_restore 之前，手动设置 SAVE1 = kernel_sp。
    // 权威来源修正（见 usertrapret 同款注释）：以 `pid` 自己的 kstack_top
    // 为准，并修回 ctx.kernel_sp，堵死「用别的任务栈顶恢复本任务」。
    unsafe {
        let cx = &mut *(trap_cx_ptr as *mut La64TrapContext);
        let top = {
            let table = PROCESS_TABLE.lock();
            table.as_ref().and_then(|t| t.get(pid)).map(|p| p.kernel_stack_top()).unwrap_or(0)
        };
        if top != 0 {
            if crate::cmdline::bscrash_enabled() && cx.kernel_sp != top {
                crate::println!(
                    "[BSCRASH_KSP] usertrapret_by_pid pid={} ctx_ksp={:#x} authoritative_top={:#x}",
                    pid, cx.kernel_sp, top,
                );
            }
            cx.kernel_sp = top;
        }
        csrwr!(cx.kernel_sp, "0x31"); // SAVE1 = kernel_sp
    }

    // 硬件浮点寄存器组切换到即将运行的进程（同 usertrapret）。
    super::fpu::switch_to(pid);

    extern "C" {
        fn __la64_commit_and_restore(cx: *mut La64TrapContext) -> !;
    }
    unsafe { __la64_commit_and_restore(trap_cx_ptr as *mut La64TrapContext) }
}

/// 初始化副核心的本地异常、IPI 和定时器状态。
pub fn init_secondary() {
    extern "C" {
        fn __la64_alltraps();
        static boot_stack_top: u8;
    }
    // EENTRY (CSR 0xC): route this hart's exceptions to the common trap entry.
    csrwr!(__la64_alltraps as usize, "0xC");
    let hart = crate::arch::loongarch64::hartid();
    if hart >= crate::sched::smp::MAX_CPUS {
        crate::println!("[smp] refusing out-of-range LoongArch hart {}", hart);
        loop {
            core::hint::spin_loop();
        }
    }
    let stack_top = unsafe { &boot_stack_top as *const u8 as usize }
        .saturating_sub(hart << 16);
    // SAVE1 (CSR 0x31): per-hart kernel exception stack, matching boot setup.
    csrwr!(stack_top, "0x31");
    unsafe {
        crate::arch::loongarch64::iocsr::write32(
            crate::arch::loongarch64::iocsr::IPI_EN,
            u32::MAX,
        );
    }
    // PWCL/PWCH/STLBPS/TLBRENTRY are per-hart CSRs.  Every AP must install
    // the page-walk format and refill vector before it can return to user mode.
    super::mm::init_tlb();
    set_next_timer();
    // ECFG (CSR 0x4): enable timer, hardware, and IPI interrupts locally.
    csrwr!(ECFG_TI | ECFG_HWI_ALL | ECFG_IPI, "0x4");
}
