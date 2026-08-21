//! # LoongArch64 CSR 操作宏和常量定义
//!
//! LoongArch64 CSR (Control and Status Register) operation macros and constants.
//!
//! ## 来源说明 / Source Attribution
//!
//! CSR 读写宏（`csrrd!`/`csrwr!`/`csrxchg!`）和 TLB 指令封装
//! 参考自 PolyHAL 项目（https://github.com/Byte-OS/polyhal），
//! 遵循 MIT/Apache-2.0 双协议。
//!
//! The CSR read/write macros and TLB instruction wrappers are adapted from
//! the PolyHAL project (https://github.com/Byte-OS/polyhal),
//! licensed under MIT/Apache-2.0 dual license.
//!
//! ## 参考文档 / References
//!
//! - 《龙芯架构参考手册》卷一：基础架构
//! - PolyHAL: `polyhal/src/loongarch64/`
//! - NPUcore-IMPACT: LoongArch64 CSR 定义

// ============================================================================
// CSR 寄存器编号常量 / CSR Register Number Constants
// ============================================================================

/// 当前模式信息 CSR（PLV/IE/DA/PG/DATF/DATM）
/// Current Mode Information — controls privilege level, interrupt enable, etc.
pub const LOONGARCH_CSR_CRMD: u32 = 0x0;

/// 先前模式信息 CSR（PPLV/PIE/PWE）
/// Previous Mode Information — saved on exception entry
pub const LOONGARCH_CSR_PRMD: u32 = 0x1;

/// 扩展单元使能 CSR（FPE/SXE/ASXE/BTE）
/// Extended Unit Enable — controls FPU, LSX, LASX, etc.
pub const LOONGARCH_CSR_EUEN: u32 = 0x2;

/// 杂项控制 CSR
/// Miscellaneous Control
pub const LOONGARCH_CSR_MISC: u32 = 0x3;

/// 异常配置 CSR（中断使能位 + 异常子码使能）
/// Exception Configuration — interrupt enable bits
pub const LOONGARCH_CSR_ECFG: u32 = 0x4;

/// 异常状态 CSR（中断挂起 + 异常一级码 Ecode + 二级码 EsubCode）
/// Exception Status — interrupt pending + exception code
pub const LOONGARCH_CSR_ESTAT: u32 = 0x5;

/// 异常返回地址 CSR（对应 RISC-V sepc）
/// Exception Return Address
pub const LOONGARCH_CSR_ERA: u32 = 0x6;

/// 出错虚拟地址 CSR（对应 RISC-V stval）
/// Bad Virtual Address — faulting address
pub const LOONGARCH_CSR_BADV: u32 = 0x7;

/// 出错指令 CSR
/// Bad Instruction — faulting instruction encoding
pub const LOONGARCH_CSR_BADI: u32 = 0x8;

/// 异常入口地址 CSR（对应 RISC-V stvec）
/// Exception Entry Address
pub const LOONGARCH_CSR_EENTRY: u32 = 0xC;

/// TLB 索引 CSR
/// TLB Index
pub const LOONGARCH_CSR_TLBIDX: u32 = 0x10;

/// TLB 条目高位 CSR
/// TLB Entry High
pub const LOONGARCH_CSR_TLBEHI: u32 = 0x11;

/// TLB 条目低位 0 CSR
/// TLB Entry Low 0
pub const LOONGARCH_CSR_TLBELO0: u32 = 0x12;

/// TLB 条目低位 1 CSR
/// TLB Entry Low 1
pub const LOONGARCH_CSR_TLBELO1: u32 = 0x13;

/// ASID CSR（地址空间标识符）
/// Address Space Identifier
pub const LOONGARCH_CSR_ASID: u32 = 0x18;

/// 低半地址空间页目录基址 CSR
/// Page Global Directory Base for lower half address space
pub const LOONGARCH_CSR_PGDL: u32 = 0x19;

/// 高半地址空间页目录基址 CSR
/// Page Global Directory Base for upper half address space
pub const LOONGARCH_CSR_PGDH: u32 = 0x1A;

/// 页目录基址 CSR（只读，根据地址自动选择 PGDL/PGDH）
/// Page Global Directory Base (read-only, auto-selects PGDL/PGDH)
pub const LOONGARCH_CSR_PGD: u32 = 0x1B;

/// 页表遍历控制低位 CSR
/// Page Walk Controller for Lower half
pub const LOONGARCH_CSR_PWCL: u32 = 0x1C;

/// 页表遍历控制高位 CSR
/// Page Walk Controller for Upper half
pub const LOONGARCH_CSR_PWCH: u32 = 0x1D;

/// 地址翻译模式 STLB 页大小 CSR
/// STLB Page Size
pub const LOONGARCH_CSR_STLBPS: u32 = 0x1E;

/// 缩减虚地址配置 CSR
/// Reduced Virtual Address Configuration
pub const LOONGARCH_CSR_RVACFG: u32 = 0x1F;

/// CPU 编号 CSR
/// CPU Identifier
pub const LOONGARCH_CSR_CPUID: u32 = 0x20;

/// 特权资源配置 1 CSR
/// Privileged Resource Configuration 1
pub const LOONGARCH_CSR_PRCFG1: u32 = 0x21;

/// 特权资源配置 2 CSR
/// Privileged Resource Configuration 2
pub const LOONGARCH_CSR_PRCFG2: u32 = 0x22;

/// 特权资源配置 3 CSR
/// Privileged Resource Configuration 3
pub const LOONGARCH_CSR_PRCFG3: u32 = 0x23;

/// 数据保存寄存器 0（内核临时保存用）
/// Kernel Scratch Register 0
pub const LOONGARCH_CSR_SAVE0: u32 = 0x30;

/// 数据保存寄存器 1
/// Kernel Scratch Register 1
pub const LOONGARCH_CSR_SAVE1: u32 = 0x31;

/// 数据保存寄存器 2
/// Kernel Scratch Register 2
pub const LOONGARCH_CSR_SAVE2: u32 = 0x32;

/// 数据保存寄存器 3
/// Kernel Scratch Register 3
pub const LOONGARCH_CSR_SAVE3: u32 = 0x33;

/// 定时器编号 CSR
/// Timer Identifier
pub const LOONGARCH_CSR_TID: u32 = 0x40;

/// 定时器配置 CSR（使能 + 周期/单次 + 初始值）
/// Timer Configuration — enable, periodic/oneshot, initial value
pub const LOONGARCH_CSR_TCFG: u32 = 0x41;

/// 定时器当前值 CSR
/// Timer Value — current countdown value
pub const LOONGARCH_CSR_TVAL: u32 = 0x42;

/// 计数器补偿 CSR
/// Counter Compensation
pub const LOONGARCH_CSR_CNTC: u32 = 0x43;

/// 定时器中断清除 CSR
/// Timer Interrupt Clear
pub const LOONGARCH_CSR_TICLR: u32 = 0x44;

/// TLB 重填异常入口地址 CSR
/// TLB Refill Exception Entry Address
pub const LOONGARCH_CSR_TLBRENTRY: u32 = 0x88;

/// TLB 重填异常出错虚拟地址 CSR
/// TLB Refill Exception Bad Virtual Address
pub const LOONGARCH_CSR_TLBRBADV: u32 = 0x89;

/// TLB 重填异常返回地址 CSR
/// TLB Refill Exception Return Address
pub const LOONGARCH_CSR_TLBRERA: u32 = 0x8A;

/// TLB 重填异常数据保存 CSR
/// TLB Refill Exception Save Register
pub const LOONGARCH_CSR_TLBRSAVE: u32 = 0x8B;

/// TLB 重填异常条目低位 0 CSR
/// TLB Refill Exception Entry Low 0
pub const LOONGARCH_CSR_TLBRELO0: u32 = 0x8C;

/// TLB 重填异常条目低位 1 CSR
/// TLB Refill Exception Entry Low 1
pub const LOONGARCH_CSR_TLBRELO1: u32 = 0x8D;

/// TLB 重填异常条目高位 CSR
/// TLB Refill Exception Entry High
pub const LOONGARCH_CSR_TLBREHI: u32 = 0x8E;

/// TLB 重填异常先前模式 CSR
/// TLB Refill Exception Previous Mode
pub const LOONGARCH_CSR_TLBRPRMD: u32 = 0x8F;

/// 直接映射配置窗口 0 CSR
/// Direct Mapping Configuration Window 0
pub const LOONGARCH_CSR_DMW0: u32 = 0x180;

/// 直接映射配置窗口 1 CSR
/// Direct Mapping Configuration Window 1
pub const LOONGARCH_CSR_DMW1: u32 = 0x181;

/// 直接映射配置窗口 2 CSR
/// Direct Mapping Configuration Window 2
pub const LOONGARCH_CSR_DMW2: u32 = 0x182;

/// 直接映射配置窗口 3 CSR
/// Direct Mapping Configuration Window 3
pub const LOONGARCH_CSR_DMW3: u32 = 0x183;


// ============================================================================
// CRMD 寄存器位域 / CRMD Register Bit Fields
// ============================================================================

/// CRMD.PLV — 当前特权等级（2 位）
/// Current Privilege Level: 0=PLV0(kernel), 3=PLV3(user)
pub const CRMD_PLV_MASK: usize = 0x3;
pub const CRMD_PLV_SHIFT: usize = 0;

/// CRMD.IE — 全局中断使能
/// Interrupt Enable
pub const CRMD_IE: usize = 1 << 2;

/// CRMD.DA — 直接地址翻译模式
/// Direct Address translation mode
pub const CRMD_DA: usize = 1 << 3;

/// CRMD.PG — 映射地址翻译模式
/// Paged address translation mode
pub const CRMD_PG: usize = 1 << 4;

/// CRMD.DATF — 取指操作的存储访问类型
/// Data Access Type for Fetch
pub const CRMD_DATF_MASK: usize = 0x3 << 5;

/// CRMD.DATM — 访存操作的存储访问类型
/// Data Access Type for Memory
pub const CRMD_DATM_MASK: usize = 0x3 << 7;

// ============================================================================
// PRMD 寄存器位域 / PRMD Register Bit Fields
// ============================================================================

/// PRMD.PPLV — 先前特权等级（2 位）
/// Previous Privilege Level
pub const PRMD_PPLV_MASK: usize = 0x3;

/// PRMD.PIE — 先前中断使能
/// Previous Interrupt Enable
pub const PRMD_PIE: usize = 1 << 2;

/// PRMD.PWE — 先前看门狗使能
/// Previous Watchpoint Enable
pub const PRMD_PWE: usize = 1 << 3;

// ============================================================================
// ESTAT 寄存器位域 / ESTAT Register Bit Fields
// ============================================================================

/// ESTAT.IS — 中断状态位（13 位，bit 0-12）
/// Interrupt Status bits
pub const ESTAT_IS_MASK: usize = 0x1FFF;

/// ESTAT.Ecode — 异常一级码（6 位，bit 16-21）
/// Exception Code (first level)
pub const ESTAT_ECODE_MASK: usize = 0x3F << 16;
pub const ESTAT_ECODE_SHIFT: usize = 16;

/// ESTAT.EsubCode — 异常二级码（9 位，bit 22-30）
/// Exception Sub-Code (second level)
pub const ESTAT_ESUBCODE_MASK: usize = 0x1FF << 22;
pub const ESTAT_ESUBCODE_SHIFT: usize = 22;

// ============================================================================
// 异常一级码 / Exception Codes (Ecode)
// ============================================================================

/// 中断 / Interrupt
pub const ECODE_INT: usize = 0x0;
/// 页无效异常（Load 操作）/ Page Invalid — Load
pub const ECODE_PIL: usize = 0x1;
/// 页无效异常（Store 操作）/ Page Invalid — Store
pub const ECODE_PIS: usize = 0x2;
/// 页无效异常（取指操作）/ Page Invalid — Fetch
pub const ECODE_PIF: usize = 0x3;
/// 页修改异常 / Page Modification Exception
pub const ECODE_PME: usize = 0x4;
/// 页不可读异常 / Page Not Readable
pub const ECODE_PNR: usize = 0x5;
/// 页不可执行异常 / Page Not Executable
pub const ECODE_PNX: usize = 0x6;
/// 页特权等级不合规异常 / Page Privilege Violation
pub const ECODE_PPI: usize = 0x7;
/// 地址错误异常（取指）/ Address Error — Fetch
pub const ECODE_ADEF: usize = 0x8;
/// 地址错误异常（访存）/ Address Error — Memory
pub const ECODE_ADEM: usize = 0x8; // EsubCode 区分
/// 地址对齐错误异常 / Address Alignment Fault
pub const ECODE_ALE: usize = 0x9;
/// 系统调用异常 / System Call
pub const ECODE_SYS: usize = 0xB;
/// 断点异常 / Breakpoint
pub const ECODE_BRK: usize = 0xC;
/// 指令不存在异常 / Instruction Not Exist
pub const ECODE_INE: usize = 0xD;
/// 指令特权等级错误异常 / Instruction Privilege Error
pub const ECODE_IPE: usize = 0xE;
/// 浮点指令未使能异常 / Floating Point Unit Disabled
pub const ECODE_FPD: usize = 0xF;
/// LSX（128 位 SIMD）指令未使能异常 / LSX Disabled
pub const ECODE_SXD: usize = 0x10;
/// LASX（256 位 SIMD）指令未使能异常 / LASX Disabled
pub const ECODE_ASXD: usize = 0x11;
/// TLB 重填异常 / TLB Refill
pub const ECODE_TLBR: usize = 0x3F;

// ============================================================================
// EUEN 寄存器位域 / EUEN Register Bit Fields
// ============================================================================

/// EUEN.FPE — 浮点单元使能
/// Floating Point Unit Enable
pub const EUEN_FPE: usize = 1 << 0;

/// EUEN.SXE — LSX（128 位 SIMD）使能
/// LSX (128-bit SIMD) Enable
pub const EUEN_SXE: usize = 1 << 1;

/// EUEN.ASXE — LASX（256 位 SIMD）使能
/// LASX (256-bit SIMD) Enable
pub const EUEN_ASXE: usize = 1 << 2;

/// EUEN.BTE — 二进制翻译使能
/// Binary Translation Enable
pub const EUEN_BTE: usize = 1 << 3;

// ============================================================================
// TCFG 定时器配置位域 / Timer Configuration Bit Fields
// ============================================================================

/// TCFG.En — 定时器使能
/// Timer Enable
pub const TCFG_EN: usize = 1 << 0;

/// TCFG.Periodic — 周期模式（1=周期，0=单次）
/// Timer Periodic mode
pub const TCFG_PERIODIC: usize = 1 << 1;

/// TCFG.InitVal — 定时器初始值（bit 2 及以上）
/// Timer Initial Value (bits [N:2])
pub const TCFG_INITVAL_SHIFT: usize = 2;

// ============================================================================
// TICLR 定时器中断清除位域 / Timer Interrupt Clear Bit Fields
// ============================================================================

/// TICLR.CLR — 清除定时器中断（写 1 清除）
/// Clear Timer Interrupt (write 1 to clear)
pub const TICLR_CLR: usize = 1 << 0;

// ============================================================================
// CSR 读写宏 / CSR Read/Write Macros
// ============================================================================
//
// 🟢 直接复用自 PolyHAL 项目（MIT/Apache-2.0 双协议）
// 🟢 Adapted from PolyHAL project (MIT/Apache-2.0 dual license)
//
// LoongArch64 的 CSR 指令使用立即数编号（不同于 RISC-V 的字符串名称），
// 因此需要通过 `concat!` 宏拼接指令字符串。
//
// LoongArch64 CSR instructions use immediate number encoding (unlike RISC-V
// which uses string names), so we use `concat!` macro to build instruction strings.

/// 读取 CSR 寄存器
///
/// Read a CSR register value.
///
/// # 用法 / Usage
/// ```rust,ignore
/// let crmd = csrrd!(LOONGARCH_CSR_CRMD);
/// let era = csrrd!(0x6);
/// ```
///
/// # 来源 / Source
/// 🟢 复用自 PolyHAL（MIT/Apache-2.0）
#[macro_export]
macro_rules! csrrd {
    ($csr_num:expr) => {{
        let value: usize;
        unsafe {
            core::arch::asm!(
                concat!("csrrd {}, ", $csr_num),
                out(reg) value,
            );
        }
        value
    }};
}

/// 写入 CSR 寄存器
///
/// Write a value to a CSR register.
///
/// # 用法 / Usage
/// ```rust,ignore
/// csrwr!(new_crmd, LOONGARCH_CSR_CRMD);
/// csrwr!(entry_addr, 0xC);
/// ```
///
/// # 来源 / Source
/// 🟢 复用自 PolyHAL（MIT/Apache-2.0）
#[macro_export]
macro_rules! csrwr {
    ($val:expr, $csr_num:expr) => {{
        let old: usize;
        unsafe {
            core::arch::asm!(
                concat!("csrwr {}, ", $csr_num),
                inlateout(reg) $val => old,
            );
        }
        old
    }};
}

/// 交换 CSR 寄存器的部分位
///
/// Exchange (read-modify-write) specific bits of a CSR register.
/// 仅修改 `mask` 为 1 的位，其余位保持不变。
/// Only bits where `mask` is 1 are modified; other bits remain unchanged.
///
/// # 用法 / Usage
/// ```rust,ignore
/// // 设置 CRMD.IE 位（使能中断），不影响其他位
/// csrxchg!(CRMD_IE, CRMD_IE, LOONGARCH_CSR_CRMD);
/// ```
///
/// # 来源 / Source
/// 🟢 复用自 PolyHAL（MIT/Apache-2.0）
#[macro_export]
macro_rules! csrxchg {
    ($val:expr, $mask:expr, $csr_num:expr) => {{
        let old: usize;
        unsafe {
            // mask 操作数**必须固定到 $t0**：LoongArch 手册规定 `csrxchg` 的 rj
            // 位置不得是 `$r0`/`$r1`（编码上这两个值被复用为 csrrd/csrwr），
            // 而 `in(reg)` 允许分配器选到 `$ra`(=$r1)。寄存器压力变化时就会编出
            // `csrxchg $a0, $ra, 0x0` 并在汇编阶段报 `must not be $r0 or $r1`。
            core::arch::asm!(
                concat!("csrxchg {0}, $t0, ", $csr_num),
                inlateout(reg) $val => old,
                in("$t0") $mask,
            );
        }
        old
    }};
}

// ============================================================================
// TLB 指令封装 / TLB Instruction Wrappers
// ============================================================================
//
// 🟢 直接复用自 PolyHAL 项目（MIT/Apache-2.0 双协议）
// 🟢 Adapted from PolyHAL project (MIT/Apache-2.0 dual license)

/// TLB 查找指令
///
/// Search TLB for a matching entry.
/// 根据 TLBEHI 中的虚拟页号和 ASID 查找 TLB 条目，
/// 结果写入 TLBIDX 寄存器。
///
/// Searches TLB based on VPN in TLBEHI and ASID,
/// writes result to TLBIDX register.
#[inline]
pub fn tlbsrch() {
    unsafe {
        core::arch::asm!("tlbsrch");
    }
}

/// TLB 读取指令
///
/// Read a TLB entry.
/// 根据 TLBIDX 中的索引读取 TLB 条目，
/// 结果写入 TLBEHI/TLBELO0/TLBELO1 等寄存器。
///
/// Reads TLB entry at index in TLBIDX,
/// writes to TLBEHI/TLBELO0/TLBELO1 registers.
#[inline]
pub fn tlbrd() {
    unsafe {
        core::arch::asm!("tlbrd");
    }
}

/// TLB 写入指令（覆盖已有条目）
///
/// Write a TLB entry (overwrite existing).
/// 将 TLBEHI/TLBELO0/TLBELO1 中的值写入 TLBIDX 指定的 TLB 条目。
///
/// Writes TLBEHI/TLBELO0/TLBELO1 values to TLB entry at TLBIDX.
#[inline]
pub fn tlbwr() {
    unsafe {
        core::arch::asm!("tlbwr");
    }
}

/// TLB 填充指令（随机替换）
///
/// Fill a TLB entry (random replacement).
/// 将 TLBEHI/TLBELO0/TLBELO1 中的值随机写入一个 TLB 条目。
/// 用于 TLB 重填异常处理。
///
/// Writes TLBEHI/TLBELO0/TLBELO1 values to a random TLB entry.
/// Used in TLB refill exception handler.
#[inline]
pub fn tlbfill() {
    unsafe {
        core::arch::asm!("tlbfill");
    }
}

/// 无效化所有 TLB 条目
///
/// Invalidate all TLB entries.
/// `invtlb 0x0, $zero, $zero` — 清除所有 TLB 条目。
///
/// `invtlb 0x0, $zero, $zero` — clears all TLB entries.
///
/// # 来源 / Source
/// 🟢 复用自 PolyHAL（MIT/Apache-2.0）
#[inline]
pub fn invtlb_all() {
    unsafe {
        core::arch::asm!("invtlb 0x0, $zero, $zero");
    }
}

/// 按 ASID 无效化 TLB 条目
///
/// Invalidate TLB entries by ASID.
/// `invtlb 0x4, asid, $zero` — 清除指定 ASID 的所有 TLB 条目。
/// 微内核关键操作：进程地址空间切换时仅刷新对应 ASID 的 TLB。
///
/// `invtlb 0x4, asid, $zero` — clears all TLB entries for the given ASID.
/// Critical for microkernel: only flush TLB entries for the target ASID
/// when switching address spaces.
#[inline]
pub fn invtlb_asid(asid: usize) {
    unsafe {
        core::arch::asm!("invtlb 0x4, {}, $zero", in(reg) asid);
    }
}

/// 按 ASID 和虚拟地址无效化 TLB 条目
///
/// Invalidate TLB entries by ASID and virtual address.
/// `invtlb 0x5, asid, vaddr` — 清除指定 ASID 和虚拟地址的 TLB 条目。
///
/// `invtlb 0x5, asid, vaddr` — clears TLB entry matching both ASID and vaddr.
#[inline]
pub fn invtlb_addr(asid: usize, vaddr: usize) {
    unsafe {
        core::arch::asm!("invtlb 0x5, {}, {}", in(reg) asid, in(reg) vaddr);
    }
}

/// 按虚拟地址无效化全局 TLB 条目
///
/// Invalidate global TLB entries by virtual address.
/// `invtlb 0x6, $zero, vaddr` — 清除指定虚拟地址的全局 TLB 条目。
///
/// `invtlb 0x6, $zero, vaddr` — clears global TLB entry for the given vaddr.
#[inline]
pub fn invtlb_addr_global(vaddr: usize) {
    unsafe {
        core::arch::asm!("invtlb 0x6, $zero, {}", in(reg) vaddr);
    }
}

/// 按指定 ASID + 虚拟地址无效化 TLB 条目
///
/// Invalidate TLB entries by the given ASID and virtual address.
/// `invtlb 0x6, asid, vaddr` — 清除该 vaddr 上「G=1 全局条目」**或**
/// 「ASID 匹配 asid 的条目」。ASID-tagged TLB 启用后，改页表路径必须用
/// 该地址空间的**真实 ASID**就地失效，否则会漏掉该 ASID 的条目导致陈旧 TLB。
///
/// With ASID-tagged TLB enabled, page-table modifications must invalidate using
/// the owning address space's real ASID; using ASID 0 would miss the entries.
#[inline]
pub fn invtlb_addr_asid(asid: usize, vaddr: usize) {
    unsafe {
        core::arch::asm!("invtlb 0x6, {}, {}", in(reg) asid, in(reg) vaddr);
    }
}

// ============================================================================
// 辅助函数 / Helper Functions
// ============================================================================

/// 从 ESTAT 寄存器提取异常一级码 Ecode
///
/// Extract the exception code (Ecode) from an ESTAT register value.
#[inline]
pub fn estat_ecode(estat: usize) -> usize {
    (estat & ESTAT_ECODE_MASK) >> ESTAT_ECODE_SHIFT
}

/// 从 ESTAT 寄存器提取异常二级码 EsubCode
///
/// Extract the exception sub-code (EsubCode) from an ESTAT register value.
#[inline]
pub fn estat_esubcode(estat: usize) -> usize {
    (estat & ESTAT_ESUBCODE_MASK) >> ESTAT_ESUBCODE_SHIFT
}

/// 从 ESTAT 寄存器提取中断状态位
///
/// Extract the interrupt status bits from an ESTAT register value.
#[inline]
pub fn estat_is(estat: usize) -> usize {
    estat & ESTAT_IS_MASK
}

/// 从 CRMD 寄存器提取当前特权等级
///
/// Extract the current privilege level from a CRMD register value.
#[inline]
pub fn crmd_plv(crmd: usize) -> usize {
    (crmd & CRMD_PLV_MASK) >> CRMD_PLV_SHIFT
}
