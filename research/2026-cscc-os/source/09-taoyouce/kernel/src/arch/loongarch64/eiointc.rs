//! # LoongArch64 扩展 I/O 中断控制器（EIOINTC/EXTIOI）驱动
//!
//! 通过 IOCSR 指令访问 EXTIOI 寄存器（不是 MMIO！）。
//!
//! ## IOCSR 寄存器地址（来自 Linux 内核 arch/loongarch/include/asm/loongarch.h）
//!
//! | 寄存器 | IOCSR 地址 | 说明 |
//! |--------|-----------|------|
//! | NODEMAP | 0x14a0 | 中断节点映射 |
//! | IPMAP | 0x14c0 | 中断到 CPU 引脚映射 |
//! | ENABLE | 0x1600 | 中断使能 |
//! | BOUNCE | 0x1680 | 中断弹跳控制 |
//! | ISR | 0x1800 | 中断状态 |
//! | ROUTE | 0x1c00 | 中断路由（到核心映射）|

#![allow(dead_code)]

use super::iocsr::{read32 as iocsr_read32, read64 as iocsr_read64};
use super::iocsr::{write32 as iocsr_write32, write64 as iocsr_write64, write8 as iocsr_write8};

// ============================================================================
// IOCSR 地址定义（来自 Linux 内核）
// ============================================================================

const IOCSR_MISC_FUNC: u32 = 0x420;
const IOCSR_MISC_FUNC_EXT_IOI_EN: u64 = 1 << 48;

const EXTIOI_NODEMAP_BASE: u32 = 0x14a0;
const EXTIOI_IPMAP_BASE: u32 = 0x14c0;
const EXTIOI_EN_BASE: u32 = 0x1600;
const EXTIOI_BOUNCE_BASE: u32 = 0x1680;
const EXTIOI_ISR_BASE: u32 = 0x1800;
const EXTIOI_ROUTE_BASE: u32 = 0x1c00;

const EXTIOI_IRQ_COUNT: usize = 256;

/// PCI 中断号
pub const PCI_IRQ_BASE: u32 = 16;
pub const PCI_IRQ_INTA: u32 = PCI_IRQ_BASE;
pub const PCI_IRQ_INTB: u32 = PCI_IRQ_BASE + 1;
pub const PCI_IRQ_INTC: u32 = PCI_IRQ_BASE + 2;
pub const PCI_IRQ_INTD: u32 = PCI_IRQ_BASE + 3;

/// 映射到的 HWI 引脚
pub const EIOINTC_HWI_PIN: u8 = 1;

// ============================================================================
// IOCSR 读写指令封装
// ============================================================================

// ============================================================================
// 初始化
// ============================================================================

/// 初始化 EIOINTC（通过 IOCSR 指令）
pub fn init() {
    crate::println!("[eiointc] Initializing EXTIOI via IOCSR...");

    unsafe {
        // 1. 使能 EXTIOI（MISC_FUNC bit 48）
        let misc = iocsr_read64(IOCSR_MISC_FUNC);
        iocsr_write64(IOCSR_MISC_FUNC, misc | IOCSR_MISC_FUNC_EXT_IOI_EN);

        // 2. 设置节点映射（NODEMAP）— 全部映射到节点 0
        for i in 0..4u32 {
            iocsr_write32(EXTIOI_NODEMAP_BASE + i * 4, 0x0);
        }

        // 3. 配置 IPMAP — 将中断映射到 HWI1
        let ipmap_val: u8 = 1 << EIOINTC_HWI_PIN;
        for i in 0..8u32 {
            iocsr_write8(EXTIOI_IPMAP_BASE + i, ipmap_val);
        }

        // 4. 将所有中断路由到 CPU 0（ROUTE）
        for i in 0..(EXTIOI_IRQ_COUNT as u32) {
            iocsr_write8(EXTIOI_ROUTE_BASE + i, 0x01);
        }

        // 5. 禁用弹跳
        for i in 0..8u32 {
            iocsr_write32(EXTIOI_BOUNCE_BASE + i * 4, 0x0);
        }

        // 6. 先清除所有使能，再使能 PCI 中断
        for i in 0..8u32 {
            iocsr_write32(EXTIOI_EN_BASE + i * 4, 0x0);
        }
        // 使能 PCI INTx（IRQ 16-19）
        let pci_mask: u32 = (1 << PCI_IRQ_INTA)
            | (1 << PCI_IRQ_INTB)
            | (1 << PCI_IRQ_INTC)
            | (1 << PCI_IRQ_INTD);
        iocsr_write32(EXTIOI_EN_BASE, pci_mask);
    }

    crate::println!("[eiointc] EXTIOI initialized (PCI IRQ {}-{})", PCI_IRQ_INTA, PCI_IRQ_INTD);
}

// ============================================================================
// 中断处理
// ============================================================================

/// 处理 EIOINTC 中断
pub fn handle_irq(_hwi: u8) -> bool {
    let mut handled_pci_intx = false;
    for reg in 0..8u32 {
        let isr = unsafe { iocsr_read32(EXTIOI_ISR_BASE + reg * 4) };
        if isr == 0 { continue; }

        let mut pending = isr;
        while pending != 0 {
            let bit = pending.trailing_zeros();
            let irq = reg * 32 + bit;

            // 清除中断（写 1 清除）
            unsafe { iocsr_write32(EXTIOI_ISR_BASE + reg * 4, 1 << bit); }

            // 分发
            match irq {
                16..=19 => {
                    handled_pci_intx = true;
                    // PCI INTx 中断 — VirtIO PCI 设备中断处理
                    // PCI INTx interrupt — VirtIO PCI device interrupt handling
                    //
                    // virtio-drivers crate 的 PciTransport 使用轮询模式，
                    // 不需要在中断处理中做特殊操作。
                    // 但我们可以在这里唤醒等待 I/O 的进程。
                }
                _ => {}
            }

            pending &= !(1 << bit);
        }
    }
    handled_pci_intx
}

/// 使能指定中断
pub fn enable_irq(irq: u32) {
    if irq as usize >= EXTIOI_IRQ_COUNT { return; }
    let reg = (irq / 32) as u32;
    let bit = irq % 32;
    unsafe {
        let cur = iocsr_read32(EXTIOI_EN_BASE + reg * 4);
        iocsr_write32(EXTIOI_EN_BASE + reg * 4, cur | (1 << bit));
    }
}

/// 禁用指定中断
pub fn disable_irq(irq: u32) {
    if irq as usize >= EXTIOI_IRQ_COUNT { return; }
    let reg = (irq / 32) as u32;
    let bit = irq % 32;
    unsafe {
        let cur = iocsr_read32(EXTIOI_EN_BASE + reg * 4);
        iocsr_write32(EXTIOI_EN_BASE + reg * 4, cur & !(1 << bit));
    }
}
