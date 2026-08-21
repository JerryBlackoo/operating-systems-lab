//! 架构无关的内联汇编辅助函数
//! 
//! 封装 RISC-V64 和 LoongArch64 的常用内联汇编操作，
//! 让上层代码无需关心具体架构的汇编指令。

/// 读取当前时间计数器
#[inline]
pub fn rdtime() -> u64 {
    let time: u64;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("rdtime {}", out(reg) time); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("rdtime.d {}, $zero", out(reg) time); }
    time
}

/// 读取 SATP/PGDL 寄存器（页表基地址）
#[inline]
pub fn read_satp() -> usize {
    let val: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("csrr {}, satp", out(reg) val); }
    #[cfg(target_arch = "loongarch64")]
    { val = crate::csrrd!("0x19"); } // PGDL
    val
}

/// 写入 SATP/PGDL 寄存器
#[inline]
pub fn write_satp(val: usize) {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("csrw satp, {}", in(reg) val); }
    #[cfg(target_arch = "loongarch64")]
    { crate::csrwr!(val, "0x19"); } // PGDL
}

/// 读取 sstatus/CRMD 寄存器
#[inline]
pub fn read_sstatus() -> usize {
    let val: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("csrr {}, sstatus", out(reg) val); }
    #[cfg(target_arch = "loongarch64")]
    { val = crate::csrrd!("0x0"); } // CRMD
    val
}

/// 写入 stvec/EENTRY 寄存器（trap 入口地址）
#[inline]
pub fn write_stvec(addr: usize) {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("csrw stvec, {}", in(reg) addr); }
    #[cfg(target_arch = "loongarch64")]
    { crate::csrwr!(addr, "0xC"); } // EENTRY
}

#[inline]
pub fn write_kernel_trap_entry() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        extern "C" { fn __trap_entry(); }
        write_stvec(__trap_entry as usize);
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        extern "C" { fn __la64_alltraps(); }
        write_stvec(__la64_alltraps as usize);
    }
}

/// 刷新 TLB（全局）
///
/// SMP 正确性同 `flush_tlb_va`：`sfence.vma` / `invtlb` 只作用本 hart。本函数
/// 的调用者是 fork 写保护（`clone_cow` 结尾）、`mprotect`、`munmap`、swap 回收
/// 等**改变当前 mm 页表语义**的路径；若不向运行同一 mm 的其它 hart 广播，父
/// 进程的其它线程会用陈旧的可写条目绕过 CoW 直接写旧帧，破坏 fork 语义（表现
/// 为多线程程序读到半更新的数据结构、解出垃圾指针后 SIGSEGV）。
#[inline]
pub fn flush_tlb_all() {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("sfence.vma"); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("invtlb 0, $zero, $zero"); }
    crate::mm::tlb::shootdown_current_mm();
}

/// 刷新 TLB（指定虚拟地址）
///
/// SMP 正确性（对标 Linux `flush_tlb_page`/`flush_tlb_mm`）：`sfence.vma` 与
/// `invtlb` **都只作用于执行它的那一个 CPU**。当同一 mm 被多个线程分布在多个
/// hart 上运行时（CLONE_THREAD 共享地址空间），只刷本核会让其它 hart 继续用
/// 陈旧 TLB 条目访问已被 CoW/换页/解除映射替换掉的旧物理页——表现为多线程
/// 程序确定性读到旧内容、解出垃圾指针后 SIGSEGV。因此这里在本地失效之后，
/// 必须对「正在运行同一 mm 的其它 hart」发起 TLB shootdown IPI。
#[inline]
pub fn flush_tlb_va(va: usize) {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("sfence.vma {}, x0", in(reg) va); }
    #[cfg(target_arch = "loongarch64")]
    // LoongArch64: invtlb 0x5 需要正确的 ASID，使用 invtlb 0x0 全局刷新
    // 确保页错误处理后 TLB 中不残留旧的无效条目
    unsafe { core::arch::asm!("invtlb 0, $zero, $zero"); }
    crate::mm::tlb::shootdown_current_mm();
}

/// 本地单页失效，**不发跨核 shootdown**。仅可用于「PTE 由无效→有效」的
/// fresh-map 路径（demand-zero / 文件按需装页首建映射）：其它 hart 不可能
/// 持有该页的**有效**陈旧条目；至多缓存了无效条目（LA TLB refill 成对装载），
/// 触发的 spurious fault 会命中「页面已映射→返回 true 刷 TLB 重试」的容忍
/// 路径自愈（对标 Linux：fresh fault 仅 update_mmu_cache，不 IPI 广播）。
#[inline]
pub fn flush_tlb_va_local(va: usize) {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("sfence.vma {}, x0", in(reg) va); }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        let _ = va;
        core::arch::asm!("invtlb 0, $zero, $zero");
    }
}

/// 失效单个虚拟地址的 TLB 条目，限定到**当前地址空间的 ASID**（不波及其它
/// 地址空间的条目，也不全刷整个 TLB）。仅可用于修改 *当前* 进程页表的热路径
/// （mprotect / 缺页 / COW）——此时 CSR 中的 ASID 即被改地址空间的 owning ASID。
/// RISC-V：`sfence.vma va, x0` 按地址刷（覆盖全部 ASID 的该地址条目，正确）。
/// LoongArch：`invtlb 0x6, asid, va`（清该 va 上 G=1 全局条目 **或** ASID 匹配
/// 条目），与跨进程切换/缺页路径使用的 `invtlb_addr_asid` 同构。
#[inline]
pub fn flush_tlb_va_current_asid(va: usize) {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("sfence.vma {}, x0", in(reg) va); }
    #[cfg(target_arch = "loongarch64")]
    {
        let asid = crate::arch::loongarch64::mm::get_asid() as usize;
        crate::arch::loongarch64::csr::invtlb_addr_asid(asid, va);
    }
    // 单页精确失效同样只作用本 hart，必须向同 mm 的其它 hart 广播。
    crate::mm::tlb::shootdown_current_mm();
}

/// 失效 `[start, end)` 范围内当前 ASID 的 TLB 条目（调用约束同
/// `flush_tlb_va_current_asid`：只能用于修改当前进程自身页表的路径）。
/// RISC-V：逐页 `sfence.vma va, x0`——QEMU softmmu 对按地址 sfence 只清
/// 单页条目，逐页是最小代价。
/// LoongArch：判题 QEMU（10.0.2）中**任何一条 invtlb（含 0x5/0x6 单页型）
/// 都会整体清空 softmmu TLB**（上游 2025-09 之后才改为按页失效），因此
/// 多页范围逐页循环 = N 次全清；此处 >1 页时改为**单次** `invtlb 0x4,asid`
/// （按 ASID 清），把 softmmu 全清次数压到 1。单页仍走 0x6 精确失效。
#[inline]
pub fn flush_tlb_range_current_asid(start: usize, end: usize) {
    const PAGE_SIZE: usize = 4096;
    let s = start & !(PAGE_SIZE - 1);
    if end <= s {
        return;
    }
    #[cfg(target_arch = "riscv64")]
    {
        let mut va = s;
        while va < end {
            unsafe { core::arch::asm!("sfence.vma {}, x0", in(reg) va) };
            va += PAGE_SIZE;
        }
    }
    #[cfg(target_arch = "loongarch64")]
    {
        let asid = crate::arch::loongarch64::mm::get_asid() as usize;
        if end - s <= PAGE_SIZE {
            crate::arch::loongarch64::csr::invtlb_addr_asid(asid, s);
        } else {
            crate::arch::loongarch64::csr::invtlb_asid(asid);
        }
    }
    // 与 `flush_tlb_all` 同理：范围失效同样只覆盖本 hart，必须广播。
    crate::mm::tlb::shootdown_current_mm();
}

/// 失效某个 ASID 的全部 TLB 条目。
/// RISC-V: `sfence.vma x0, asid`（rs1=x0 → 该 ASID 全部地址）。
/// LoongArch: 退化为全刷（地址空间销毁路径非热点）。
#[inline]
pub fn flush_tlb_asid(asid: usize) {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("sfence.vma x0, {}", in(reg) asid); }
    #[cfg(target_arch = "loongarch64")]
    {
        let _ = asid;
        unsafe { core::arch::asm!("invtlb 0, $zero, $zero"); }
    }
}

// ── RISC-V Sv39 satp.ASID 支持（消除跨地址空间切换的全量 TLB 失效）──
// satp 格式: MODE[63:60] | ASID[59:44] | PPN[43:0]
#[cfg(target_arch = "riscv64")]
const RV_SATP_PPN_MASK: usize = (1usize << 44) - 1;
#[cfg(target_arch = "riscv64")]
const RV_SATP_ASID_SHIFT: usize = 44;
#[cfg(target_arch = "riscv64")]
const RV_SATP_ASID_MASK: usize = 0xFFFFusize << RV_SATP_ASID_SHIFT;

/// 探测当前实现是否支持 satp.ASID：写全 1 到 ASID 字段读回是否保留非零位
/// （PPN 不变，故不影响映射）。结果缓存。不支持时所有切换回退为全量 sfence.vma，
/// 行为与启用 ASID 前完全一致（安全兜底）。
#[cfg(target_arch = "riscv64")]
fn rv_asid_supported() -> bool {
    use core::sync::atomic::{AtomicU8, Ordering::Relaxed};
    static OK: AtomicU8 = AtomicU8::new(0); // 0=未知 1=支持 2=不支持
    match OK.load(Relaxed) {
        1 => true,
        2 => false,
        _ => {
            let supported = unsafe {
                let orig: usize;
                core::arch::asm!("csrr {}, satp", out(reg) orig);
                let probe = orig | RV_SATP_ASID_MASK;
                core::arch::asm!("csrw satp, {}", in(reg) probe);
                let rb: usize;
                core::arch::asm!("csrr {}, satp", out(reg) rb);
                core::arch::asm!("csrw satp, {}", in(reg) orig); // 立即恢复
                (rb & RV_SATP_ASID_MASK) != 0
            };
            OK.store(if supported { 1 } else { 2 }, Relaxed);
            supported
        }
    }
}

/// 当前 satp 与目标 user_satp 是否属于同一地址空间（比较 PPN，忽略 ASID 字段）。
/// 用于切换路径判定"同进程返回"——此时 satp 不变、TLB 仍有效，无需重写/失效。
#[cfg(target_arch = "riscv64")]
#[inline]
pub fn satp_same_address_space(cur_satp: usize, user_satp: usize) -> bool {
    (cur_satp & RV_SATP_PPN_MASK) == (user_satp & RV_SATP_PPN_MASK)
}

/// 激活用户地址空间页表（带 ASID），用于跨地址空间上下文切换。
///
/// `active_mm` 只覆盖当前正在运行的地址空间，故某 hart 切走后不会收到原 mm
/// 后续的 shootdown，但 ASID-tagged TLB 条目仍可能留在本 hart。切回目标 mm
/// 时必须先失效目标 ASID；同 mm 的普通 syscall 返回不会调用本函数，仍保留
/// 零刷新快路径。ASID 不支持时回退为全量失效。
#[cfg(target_arch = "riscv64")]
#[inline]
pub fn activate_user_satp(user_satp: usize) {
    if !rv_asid_supported() {
        unsafe { core::arch::asm!("csrw satp, {}", "sfence.vma", in(reg) user_satp); }
        return;
    }
    let root_paddr = ((user_satp & RV_SATP_PPN_MASK) as u64) << 12;
    let (asid, need_full_flush) = crate::mm::asid::activate_cached(root_paddr);
    let satp = (user_satp & !RV_SATP_ASID_MASK) | ((asid as usize) << RV_SATP_ASID_SHIFT);
    unsafe {
        if need_full_flush {
            core::arch::asm!("csrw satp, {}", "sfence.vma", in(reg) satp);
        } else {
            // reenter 失效由调用方（usertrapret 的 reentering_mm 分支）负责
            // 触发本函数；此处按 ASID 精确失效目标地址空间的残留条目。
            core::arch::asm!(
                "csrw satp, {satp}",
                "sfence.vma x0, {asid}",
                satp = in(reg) satp,
                asid = in(reg) asid as usize,
            );
        }
    }
}

/// LoongArch64 版用户地址空间激活：写 PGDL/ASID，并在每次 inactive→active
/// 重新进入时失效目标 ASID。这样 inactive hart 被 mm_cpumask 省略期间错过的
/// CoW/munmap shootdown 不会在稍后迁回该 mm 时重新生效。
#[cfg(target_arch = "loongarch64")]
#[inline]
pub fn activate_user_satp(user_pgd: usize) {
    if user_pgd == 0 {
        return;
    }
    write_satp(user_pgd);
    let (asid, need_full_flush) = crate::mm::asid::activate_cached(user_pgd as u64);
    crate::arch::loongarch64::mm::set_asid(asid);
    unsafe {
        core::arch::asm!("dbar 0");
    }
    if need_full_flush {
        crate::arch::loongarch64::csr::invtlb_all();
    } else {
        crate::arch::loongarch64::csr::invtlb_asid(asid as usize);
    }
    unsafe {
        core::arch::asm!("ibar 0");
    }
}

/// 等待中断 + 启用/禁用中断的原子操作
#[inline]
pub fn enable_interrupt_and_wfi() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!("csrs sstatus, {}", in(reg) 1usize << 1);
        core::arch::asm!("wfi");
        core::arch::asm!("csrc sstatus, {}", in(reg) 1usize << 1);
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        core::arch::asm!("idle 0");
    }
}

/// 读取 CPU ID（tp 寄存器 / CPUID CSR）
#[inline]
pub fn read_cpu_id() -> usize {
    let id: usize;
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("mv {}, tp", out(reg) id); }
    #[cfg(target_arch = "loongarch64")]
    { id = crate::csrrd!("0x20"); }
    id
}

/// 设置 CPU ID（tp 寄存器）
#[inline]
pub fn write_cpu_id(id: usize) {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("mv tp, {}", in(reg) id); }
    #[cfg(target_arch = "loongarch64")]
    { let _ = id; } // LoongArch64 不需要手动设置
}

/// 停掉**本 hart** 的定时器中断源。
///
/// 用于「本 hart 没有任何可执行任务」的空闲路径：Linux 在 NOHZ_IDLE 下同样会
/// 停掉空闲 CPU 的周期 tick，避免纯粹为了轮询而反复唤醒。这里更进一步——本内核
/// 的 AP 目前不执行用户任务（见 `main.rs` AP 循环与 `get_task_pid` 的 TODO），
/// 若仍保留周期 tick，7 个空闲 AP 在 QEMU TCG 下就是 7 个持续被唤醒的宿主线程，
/// 直接稀释真正在干活的 hart。停 tick 后这些 hart 只会被 IPI 唤醒。
#[inline]
pub fn disable_local_timer() {
    #[cfg(target_arch = "riscv64")]
    unsafe {
        // stimecmp = u64::MAX：Sstc 下写入大于 time 的值即原子清除 STIP，
        // 且此后不再触发定时器中断。
        core::arch::asm!("csrw 0x14D, {}", in(reg) u64::MAX);
    }
    #[cfg(target_arch = "loongarch64")]
    unsafe {
        // TCFG=0：清 EN 位关闭定时器（同时清除 InitVal/Periodic）。
        core::arch::asm!("csrwr {val}, 0x41", val = in(reg) 0usize, options(nostack));
    }
}

/// WFI（低功耗等待）
#[inline]
pub fn wfi_once() {
    #[cfg(target_arch = "riscv64")]
    unsafe { core::arch::asm!("wfi", options(nomem, nostack)); }
    #[cfg(target_arch = "loongarch64")]
    unsafe { core::arch::asm!("idle 0", options(nomem, nostack)); }
}
