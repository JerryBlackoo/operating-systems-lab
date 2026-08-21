//! LoongArch64 PCI Transport 支持 — 基于 virtio-drivers crate
//!
//! 🟢 直接复用 virtio-drivers crate 的 `PciTransport`（PCI 配置空间读写、BAR 解析、
//! VirtIO PCI capability 解析）
//! 🟡 微内核适配：PCI ECAM MMIO 地址映射、PCI 设备枚举逻辑、BAR 地址分配
//!
//! 来源: virtio-drivers crate (MIT License, https://github.com/rcore-os/virtio-drivers)
//!
//! # LoongArch64 QEMU virt PCI 信息
//!
//! - PCI ECAM 基地址：0x20000000（PCI 配置空间 MMIO）
//! - PCI MMIO 区域：0x40000000-0x7FFFFFFF（PCI BAR 映射）
//! - VirtIO 设备使用 PCI transport（`virtio-blk-pci`、`virtio-net-pci`）
//!
//! # BAR 地址分配
//!
//! LoongArch64 QEMU virt 在裸机模式下（直接加载 kernel ELF，无 BIOS/UEFI），
//! PCI 设备的 BAR 地址不会被固件自动分配。需要内核手动分配 BAR 地址。
//!
//! 分配策略：从 PCI_MMIO_BASE (0x40000000) 开始，按 BAR 大小对齐分配。
//!
//! # 参考
//!
//! - virtio-drivers crate: https://github.com/rcore-os/virtio-drivers
//! - ArceOS PCI 枚举: https://github.com/arceos-org/arceos
//! - LoongArch QEMU virt 平台 PCI 配置: QEMU hw/loongarch/virt.c

#![allow(dead_code)]

use virtio_drivers::transport::pci::bus::{
    BarInfo, Cam, Command, DeviceFunction, MemoryBarType, PciRoot,
};
use virtio_drivers::transport::pci::{virtio_device_type, PciTransport};
use virtio_drivers::transport::{DeviceType, Transport};

use super::hal_impl::VirtioHalImpl;

// ============================================================================
// LoongArch64 QEMU virt PCI 常量 / PCI Constants
// ============================================================================

/// PCI ECAM（Enhanced Configuration Access Mechanism）基地址
///
/// LoongArch64 QEMU virt 平台的 PCI 配置空间 MMIO 基地址。
/// 在 PG=1 模式下通过 DMW1 窗口 (VSEG=0x8000, SUC) 访问。
/// 参考: QEMU `hw/loongarch/virt.c` 中的 `VIRT_PCI_CFG_BASE`
pub const PCI_ECAM_BASE_PHYS: usize = 0x2000_0000;
pub const PCI_ECAM_BASE: usize = 0x8000_0000_2000_0000; // DMW1 窗口

/// PCI MMIO 区域起始地址（物理地址，用于 BAR 分配）
///
/// PCI BAR 映射的 MMIO 区域起始地址。
/// 参考: QEMU `hw/loongarch/virt.c` 中的 `VIRT_PCI_MEM_BASE`
pub const PCI_MMIO_BASE: usize = 0x4000_0000;

/// PCI MMIO 区域结束地址（物理地址）
pub const PCI_MMIO_END: usize = 0x8000_0000;

/// PCI IO 区域起始地址
pub const PCI_IO_BASE: usize = 0x1800_0000;

/// VirtIO PCI 设备的 Vendor ID（Red Hat）
pub const VIRTIO_VENDOR_ID: u16 = 0x1AF4;

/// PCI 总线号（QEMU virt 平台只有 bus 0）
pub const PCI_BUS_0: u8 = 0;

// ============================================================================
// BAR 地址分配器 / BAR Address Allocator
// ============================================================================

/// 简单的 PCI BAR 地址分配器
///
/// 从 PCI_MMIO_BASE 开始，按 BAR 大小对齐分配 MMIO 地址。
/// 裸机模式下没有固件分配 BAR，需要内核手动分配。
struct BarAllocator {
    /// 下一个可用的 MMIO 地址
    next_mmio: usize,
    /// MMIO 区域结束地址
    mmio_end: usize,
}

impl BarAllocator {
    /// 创建 BAR 地址分配器
    const fn new() -> Self {
        Self {
            next_mmio: PCI_MMIO_BASE,
            mmio_end: PCI_MMIO_END,
        }
    }

    /// 分配一个 MMIO 地址
    ///
    /// 按 `size` 对齐分配，返回分配的地址。
    /// 如果空间不足返回 None。
    fn alloc(&mut self, size: u32) -> Option<u64> {
        if size == 0 {
            return None;
        }
        // 对齐到 size（PCI BAR 要求地址按大小对齐）
        let align = size as usize;
        let aligned = (self.next_mmio + align - 1) & !(align - 1);
        let end = aligned + size as usize;
        if end > self.mmio_end {
            return None;
        }
        self.next_mmio = end;
        Some(aligned as u64)
    }
}

// ============================================================================
// PCI 设备枚举和初始化 / PCI Device Enumeration and Initialization
// ============================================================================

/// 为 PCI 设备分配 BAR 地址并启用设备
///
/// 遍历设备的所有 BAR，如果地址为 0 则分配新地址。
/// 然后启用设备的 Memory Space 和 Bus Master。
fn assign_bars_and_enable(
    pci_root: &mut PciRoot,
    device_function: DeviceFunction,
    allocator: &mut BarAllocator,
) {
    // 读取所有 BAR 信息
    let bars = match pci_root.bars(device_function) {
        Ok(bars) => bars,
        Err(e) => {
            crate::println!(
                "[virtio-pci] Failed to read BARs for {:?}: {:?}",
                device_function, e
            );
            return;
        }
    };

    // 为未分配地址的 BAR 分配地址
    for (i, bar) in bars.iter().enumerate() {
        if let Some(bar_info) = bar {
            match bar_info {
                BarInfo::Memory {
                    address_type,
                    prefetchable: _,
                    address,
                    size,
                } => {
                    if *address == 0 && *size > 0 {
                        // BAR 未分配，需要分配地址
                        if let Some(new_addr) = allocator.alloc(*size) {
                            match address_type {
                                MemoryBarType::Width32 => {
                                    pci_root.set_bar_32(
                                        device_function,
                                        i as u8,
                                        new_addr as u32,
                                    );
                                    crate::println!(
                                        "[virtio-pci]   BAR{}: allocated 32-bit MMIO {:#x} (size={:#x})",
                                        i, new_addr, size
                                    );
                                }
                                MemoryBarType::Width64 => {
                                    pci_root.set_bar_64(
                                        device_function,
                                        i as u8,
                                        new_addr,
                                    );
                                    crate::println!(
                                        "[virtio-pci]   BAR{}: allocated 64-bit MMIO {:#x} (size={:#x})",
                                        i, new_addr, size
                                    );
                                }
                                _ => {}
                            }
                        } else {
                            crate::println!(
                                "[virtio-pci]   BAR{}: MMIO allocation failed (size={:#x})",
                                i, size
                            );
                        }
                    }
                }
                BarInfo::IO { address, size } => {
                    if *address == 0 && *size > 0 {
                        crate::println!(
                            "[virtio-pci]   BAR{}: IO BAR (size={:#x}), skipping",
                            i, size
                        );
                    }
                }
            }
        }
    }

    // 启用设备的 Memory Space 和 Bus Master
    //
    // PCI Command Register:
    // - bit 1: Memory Space Enable
    // - bit 2: Bus Master Enable
    let (_status, cmd) = pci_root.get_status_command(device_function);
    pci_root.set_command(
        device_function,
        cmd | Command::MEMORY_SPACE | Command::BUS_MASTER,
    );
}

/// 探测并初始化所有 PCI VirtIO 设备
///
/// 遍历 LoongArch64 QEMU virt 平台的 PCI 总线 0，通过 virtio-drivers crate 的
/// `PciRoot` 枚举设备，识别 VirtIO 设备并使用 `PciTransport` 初始化。
///
/// # 设备发现流程
///
/// ```text
/// PCI ECAM (0x20000000)
///   → PciRoot::new(ecam_base, Cam::Ecam)
///   → enumerate_bus(0)
///   → 对每个设备：分配 BAR 地址 + 启用 Memory Space + Bus Master
///   → virtio_device_type() 判断设备类型
///   → PciTransport::new() 初始化 VirtIO transport
///   → 根据设备类型初始化 VirtIOBlk / VirtIONet
/// ```
///
/// # Safety
///
/// 此函数访问 PCI ECAM MMIO 区域，要求：
/// - PCI ECAM 基地址（0x20000000）已正确映射且可访问
/// - 在 LoongArch64 DA=1 模式下，物理地址可直接访问
pub fn init() {
    crate::println!("[virtio-pci] Probing PCI VirtIO devices on LoongArch64...");
    crate::println!("[virtio-pci] PCI ECAM base: {:#x}, MMIO range: {:#x}-{:#x}",
        PCI_ECAM_BASE, PCI_MMIO_BASE, PCI_MMIO_END);

    // 创建 PCI 根总线
    //
    // 🟢 直接复用 virtio-drivers crate 的 PciRoot
    // Safety: PCI ECAM 基地址 0x20000000 是 LoongArch64 QEMU virt 平台的
    // 标准 PCI 配置空间地址，在 DA=1 模式下可直接访问。
    // Cam::Ecam 表示使用 PCIe ECAM（每个设备函数 4KiB 配置空间）。
    let mut pci_root = unsafe {
        PciRoot::new(PCI_ECAM_BASE as *mut u8, Cam::Ecam)
    };

    // BAR 地址分配器
    let mut allocator = BarAllocator::new();

    // 第一遍：收集所有设备信息并分配 BAR 地址
    //
    // 🟡 微内核适配：裸机模式下没有固件分配 BAR 地址，
    // 需要在创建 PciTransport 前手动分配。
    let mut devices = alloc::vec::Vec::new();

    for (device_function, info) in pci_root.enumerate_bus(PCI_BUS_0) {
        let dev_type = virtio_device_type(&info);
        crate::println!(
            "[virtio-pci] PCI {:?}: vendor={:#06x} device={:#06x} type={:?}",
            device_function, info.vendor_id, info.device_id, dev_type
        );

        if dev_type.is_some() {
            // 为 VirtIO 设备分配 BAR 地址并启用
            assign_bars_and_enable(&mut pci_root, device_function, &mut allocator);
            devices.push(device_function);
        }
    }

    // 第二遍：为已分配 BAR 的设备创建 PciTransport
    let mut blk_count = 0usize;
    let mut net_count = 0usize;

    for device_function in devices {
        // 🟢 直接复用 PciTransport::new()
        // PciTransport 会自动解析 VirtIO PCI capabilities、配置 BAR 等
        match PciTransport::new::<VirtioHalImpl>(&mut pci_root, device_function) {
            Ok(transport) => {
                let actual_type = transport.device_type();
                crate::println!(
                    "[virtio-pci] PciTransport initialized for {:?}",
                    actual_type
                );

                match actual_type {
                    DeviceType::Block => {
                        super::blk::init_pci_device(transport);
                        blk_count += 1;
                    }
                    DeviceType::Network => {
                        super::net::init_pci_device(transport);
                        net_count += 1;
                    }
                    _ => {
                        crate::println!(
                            "[virtio-pci] Skipping unsupported VirtIO device type: {:?}",
                            actual_type
                        );
                    }
                }
            }
            Err(e) => {
                crate::println!(
                    "[virtio-pci] ERROR: Failed to init PciTransport at {:?}: {:?}",
                    device_function, e
                );
            }
        }
    }

    crate::println!(
        "[virtio-pci] PCI enumeration complete: {} block device(s), {} network device(s)",
        blk_count, net_count
    );
}

/// PCI VirtIO 子系统自测试
pub fn self_test() {
    crate::println!("[virtio-pci] Running self-test...");
    crate::println!("[virtio-pci] PCI_ECAM_BASE = {:#x}", PCI_ECAM_BASE);
    crate::println!("[virtio-pci] PCI_MMIO_BASE = {:#x}", PCI_MMIO_BASE);
    crate::println!("[virtio-pci] PCI_MMIO_END  = {:#x}", PCI_MMIO_END);
    crate::println!("[virtio-pci] VIRTIO_VENDOR_ID = {:#06x}", VIRTIO_VENDOR_ID);
    crate::println!("[virtio-pci] Self-test passed!");
}
