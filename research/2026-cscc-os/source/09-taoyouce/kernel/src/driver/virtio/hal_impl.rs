//! VirtIO HAL 适配层 — 桥接 virtio-drivers crate 与 RustMicroOS 微内核
//!
//! 🟡 微内核适配：实现 virtio-drivers 的 `Hal` trait，将 DMA 内存分配、
//! 物理地址转换等操作委托给微内核的 Buddy 分配器和地址管理模块。
//!
//! 来源: 适配层代码为 RustMicroOS 原创
//! 复用: virtio-drivers crate (MIT License, https://github.com/rcore-os/virtio-drivers)
//!
//! # 设计说明
//!
//! virtio-drivers crate 通过 `Hal` trait 回调内核的物理内存管理，
//! 这是一种典型的依赖反转模式。适配层需要提供：
//!
//! - `dma_alloc`: 分配物理连续的 DMA 缓冲区（用于 VirtQueue 描述符表等）
//! - `dma_dealloc`: 释放 DMA 缓冲区
//! - `mmio_phys_to_virt`: 物理地址 → 虚拟地址转换（PCI BAR 地址映射）
//! - `share`/`unshare`: 共享内存管理（用于 DMA 数据传输）
//!
//! # 架构兼容性
//!
//! 当前实现假设恒等映射（identity mapping），即物理地址 == 虚拟地址。
//! 这在 RISC-V64 QEMU virt 平台的 S-mode 下成立（内核使用直接映射）。
//! LoongArch64 平台需要通过 DMW（Direct Mapping Window）实现类似效果。

use core::ptr::NonNull;
use virtio_drivers::{BufferDirection, Hal, PhysAddr, PAGE_SIZE};
use crate::mm::{PhysPageNum, alloc_frames, dealloc_frames};

/// VirtIO HAL 适配层实现
///
/// 空结构体，作为 `Hal` trait 的类型参数传递给 virtio-drivers 的设备驱动。
/// 例如: `VirtIOBlk<VirtioHalImpl, MmioTransport>`
pub struct VirtioHalImpl;

unsafe impl Hal for VirtioHalImpl {
    /// 分配 DMA 缓冲区（物理连续内存）
    ///
    /// 调用微内核的 Buddy 分配器分配 `pages` 个物理连续页帧。
    /// 返回 (物理地址, 虚拟地址指针) 元组。
    ///
    /// # 实现细节
    ///
    /// - 使用 Buddy 分配器的 `alloc(order)` 接口，其中 order = log2(pages)
    /// - 分配后清零页帧内容（virtio-drivers 要求）
    /// - 当前假设恒等映射，物理地址直接作为虚拟地址使用
    fn dma_alloc(
        pages: usize,
        _direction: BufferDirection,
    ) -> (PhysAddr, NonNull<u8>) {
        // 计算 Buddy 分配器所需的 order
        // order = ceil(log2(pages))，确保分配 >= pages 个连续页
        let order = pages.next_power_of_two().trailing_zeros() as usize;

        let ppn = alloc_frames(order)
            .expect("[virtio-hal] DMA alloc failed: out of physical memory");

        // 物理地址 = 页帧号 << 12
        let paddr = (ppn.0 << 12) as PhysAddr;

        // RISC-V64: 恒等映射 vaddr == paddr
        // LoongArch64 PG=1: 通过 DMW0 窗口访问物理内存
        let vaddr = crate::mm::phys_to_kernel_vaddr(paddr as usize);

        // 清零分配的页帧（virtio-drivers 要求 DMA 缓冲区已清零）
        // LoongArch64: 避免 compiler_builtins memset bug，逐 8 字节清零
        let actual_pages = 1 << order;
        let total_bytes = actual_pages * PAGE_SIZE;
        unsafe {
            let mut ptr = vaddr as *mut u64;
            let end = (vaddr + total_bytes) as *mut u64;
            while ptr < end {
                core::ptr::write_volatile(ptr, 0u64);
                ptr = ptr.add(1);
            }
        }

        let ptr = NonNull::new(vaddr as *mut u8)
            .expect("[virtio-hal] DMA alloc returned null virtual address");

        (paddr, ptr)
    }

    /// 释放 DMA 缓冲区
    ///
    /// 将之前通过 `dma_alloc` 分配的物理连续页帧归还给 Buddy 分配器。
    ///
    /// # Safety
    ///
    /// - `paddr` 和 `vaddr` 必须是之前 `dma_alloc` 返回的值
    /// - `pages` 必须与分配时相同
    /// - 内存不能已被释放
    unsafe fn dma_dealloc(
        paddr: PhysAddr,
        _vaddr: NonNull<u8>,
        pages: usize,
    ) -> i32 {
        let ppn = PhysPageNum(paddr as usize >> 12);
        let order = pages.next_power_of_two().trailing_zeros() as usize;
        dealloc_frames(ppn, order);
        0 // 成功返回 0
    }

    /// 物理地址 → 虚拟地址转换（MMIO 区域）
    ///
    /// 用于 PCI transport 将 BAR 中读取的物理地址转换为驱动可访问的虚拟地址。
    ///
    /// - RISC-V64: 恒等映射（S-mode 下物理地址可直接访问）
    /// - LoongArch64 PG=1 模式: 通过 DMW1 (VSEG=0x8000, SUC) 访问 MMIO 设备
    ///
    /// # Safety
    ///
    /// - `paddr` 和 `size` 必须描述一个有效的 MMIO 区域
    unsafe fn mmio_phys_to_virt(paddr: PhysAddr, _size: usize) -> NonNull<u8> {
        #[cfg(target_arch = "riscv64")]
        {
            // RISC-V64: 恒等映射 vaddr == paddr
            NonNull::new(paddr as usize as *mut u8)
                .expect("[virtio-hal] mmio_phys_to_virt: null address")
        }
        #[cfg(target_arch = "loongarch64")]
        {
            // LoongArch64 PG=1: 通过 DMW1 窗口 (VSEG=0x8000, SUC) 访问 MMIO
            // DMW1: 0x8000_0000_0000_0000 | paddr
            let vaddr = 0x8000_0000_0000_0000 | (paddr as usize);
            NonNull::new(vaddr as *mut u8)
                .expect("[virtio-hal] mmio_phys_to_virt: null address")
        }
    }

    /// 共享内存 — 将缓冲区地址转换为设备可访问的物理地址
    ///
    /// 在没有 IOMMU 的环境下（QEMU virt 平台），直接返回虚拟地址对应的物理地址。
    /// 当前使用恒等映射，虚拟地址 == 物理地址。
    ///
    /// # Safety
    ///
    /// - `buffer` 必须是有效的非空内存区域
    /// - 在此调用期间不能被其他线程访问
    unsafe fn share(
        buffer: NonNull<[u8]>,
        _direction: BufferDirection,
    ) -> PhysAddr {
        // 将虚拟地址转换为物理地址
        // RISC-V64: 恒等映射 paddr == vaddr
        // LoongArch64 PG=1: DMW0 窗口地址需要去掉高位前缀
        let vaddr = buffer.as_ptr() as *const u8 as usize;
        #[cfg(target_arch = "riscv64")]
        { vaddr as PhysAddr }
        #[cfg(target_arch = "loongarch64")]
        {
            // 去掉 DMW 窗口前缀，恢复物理地址
            let paddr = vaddr & 0x0000_FFFF_FFFF_FFFF;
            paddr as PhysAddr
        }
    }

    /// 取消共享内存
    ///
    /// 在没有 IOMMU 的环境下，无需额外操作。
    /// 如果使用了 bounce buffer（IOMMU/swiotlb 场景），需要在此处将数据拷贝回原缓冲区。
    ///
    /// # Safety
    ///
    /// - `buffer` 必须是有效的非空内存区域
    /// - `paddr` 必须是之前 `share` 返回的值
    unsafe fn unshare(
        _paddr: PhysAddr,
        _buffer: NonNull<[u8]>,
        _direction: BufferDirection,
    ) {
        // 恒等映射下无需操作
        // 未来如果引入 IOMMU 或 swiotlb，需要在此处拷贝数据
    }
}
