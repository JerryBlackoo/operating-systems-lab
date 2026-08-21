//! 地址类型定义

use crate::arch::consts::{PAGE_SIZE, PAGE_SIZE_BITS};

/// 物理地址
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PhysAddr(pub usize);

/// 虚拟地址
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtAddr(pub usize);

/// 物理页号
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct PhysPageNum(pub usize);

/// 虚拟页号
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtPageNum(pub usize);

impl PhysAddr {
    /// 获取页内偏移
    pub fn page_offset(&self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }
    
    /// 是否页对齐
    pub fn is_aligned(&self) -> bool {
        self.page_offset() == 0
    }
    
    /// 向下取整到页边界
    pub fn floor(&self) -> PhysPageNum {
        PhysPageNum(self.0 >> PAGE_SIZE_BITS)
    }
    
    /// 向上取整到页边界
    pub fn ceil(&self) -> PhysPageNum {
        PhysPageNum((self.0 + PAGE_SIZE - 1) >> PAGE_SIZE_BITS)
    }

    /// 将物理地址转换为内核可访问的虚拟地址
    ///
    /// RISC-V64: 恒等映射，物理地址 == 虚拟地址
    /// LoongArch64 PG=1 模式: 通过 DMW0 窗口访问 (0x9000_0000_0000_0000 | paddr)
    #[inline]
    pub fn to_kernel_vaddr(&self) -> usize {
        phys_to_kernel_vaddr(self.0)
    }
}

/// 将物理地址转换为内核可访问的虚拟地址（独立函数）
///
/// RISC-V64: 恒等映射，物理地址 == 虚拟地址
/// LoongArch64:
///   DA=1 模式: 物理地址直接可访问（仅 entry.S 早期启动阶段）
///   PG=1 模式: 通过 DMW0 窗口访问 (0x9000_0000_0000_0000 | paddr)
#[inline]
pub fn phys_to_kernel_vaddr(paddr: usize) -> usize {
    #[cfg(target_arch = "riscv64")]
    { paddr }
    #[cfg(target_arch = "loongarch64")]
    {
        // 如果地址已经在 DMW 窗口范围内，直接返回
        if paddr >= 0x8000_0000_0000_0000 {
            return paddr;
        }
        let vaddr = 0x9000_0000_0000_0000 | paddr;
        // 检测异常：如果物理地址为 0 或非常小，打印警告
        if paddr < 0x1000 {
            crate::println!("[WARN] phys_to_kernel_vaddr: suspicious paddr={:#x} -> vaddr={:#x}", paddr, vaddr);
        }
        vaddr
    }
}

impl VirtAddr {
    /// 获取页内偏移
    pub fn page_offset(&self) -> usize {
        self.0 & (PAGE_SIZE - 1)
    }
    
    /// 是否页对齐
    pub fn is_aligned(&self) -> bool {
        self.page_offset() == 0
    }
    
    /// 向下取整到页边界
    pub fn floor(&self) -> VirtPageNum {
        VirtPageNum(self.0 >> PAGE_SIZE_BITS)
    }
    
    /// 向上取整到页边界
    pub fn ceil(&self) -> VirtPageNum {
        VirtPageNum((self.0 + PAGE_SIZE - 1) >> PAGE_SIZE_BITS)
    }
    
    /// 获取 Sv39 页表索引
    pub fn indexes(&self) -> [usize; 3] {
        let vpn = self.0 >> PAGE_SIZE_BITS;
        [
            (vpn >> 18) & 0x1ff, // VPN[2]
            (vpn >> 9) & 0x1ff,  // VPN[1]
            vpn & 0x1ff,         // VPN[0]
        ]
    }
}

impl PhysPageNum {
    /// 转换为物理地址
    pub fn addr(&self) -> PhysAddr {
        PhysAddr(self.0 << PAGE_SIZE_BITS)
    }

    /// 获取内核可访问的虚拟地址（物理页起始地址）
    ///
    /// RISC-V64: 恒等映射，返回物理地址
    /// LoongArch64 PG=1: 返回 DMW0 窗口地址
    #[inline]
    pub fn kernel_addr(&self) -> usize {
        phys_to_kernel_vaddr(self.0 << PAGE_SIZE_BITS)
    }
    
    /// 将物理地址转换为内核可访问的虚拟地址
    ///
    /// RISC-V64: 恒等映射，物理地址 == 虚拟地址
    /// LoongArch64 PG=1 模式: 通过 DMW0 窗口访问 (0x9000_0000_0000_0000 | paddr)
    #[inline]
    pub fn to_kernel_vaddr(paddr: usize) -> usize {
        phys_to_kernel_vaddr(paddr)
    }

    /// 获取页帧的可变引用
    ///
    /// LoongArch64 PG=1 模式下通过 DMW0 窗口访问物理内存
    pub fn as_bytes_mut(&self) -> &'static mut [u8] {
        let vaddr = Self::to_kernel_vaddr(self.addr().0);
        unsafe {
            core::slice::from_raw_parts_mut(
                vaddr as *mut u8,
                PAGE_SIZE,
            )
        }
    }
    
    /// 获取页表项数组（旧接口，保持兼容）
    pub fn as_pte_array(&self) -> &'static mut [super::page_table::PageTableEntry] {
        self.get_pte_array()
    }
    
    /// 获取页表项数组
    ///
    /// LoongArch64 PG=1 模式下通过 DMW0 窗口访问物理内存
    pub fn get_pte_array(&self) -> &'static mut [super::page_table::PageTableEntry] {
        let vaddr = Self::to_kernel_vaddr(self.addr().0);
        unsafe {
            core::slice::from_raw_parts_mut(
                vaddr as *mut super::page_table::PageTableEntry,
                512,
            )
        }
    }
    
    /// 获取泛型数组引用
    pub fn get_mut<T>(&self) -> &'static mut T {
        let vaddr = Self::to_kernel_vaddr(self.addr().0);
        unsafe {
            &mut *(vaddr as *mut T)
        }
    }
}

impl VirtPageNum {
    /// 转换为虚拟地址
    pub fn addr(&self) -> VirtAddr {
        VirtAddr(self.0 << PAGE_SIZE_BITS)
    }
    
    /// 获取 Sv39 页表索引（旧接口）
    pub fn indexes(&self) -> [usize; 3] {
        self.indices()
    }
    
    /// 获取 Sv39 页表索引
    pub fn indices(&self) -> [usize; 3] {
        [
            (self.0 >> 18) & 0x1ff, // VPN[2] - 第一级
            (self.0 >> 9) & 0x1ff,  // VPN[1] - 第二级
            self.0 & 0x1ff,         // VPN[0] - 第三级
        ]
    }
}

impl From<usize> for PhysAddr {
    fn from(v: usize) -> Self {
        Self(v)
    }
}

impl From<usize> for VirtAddr {
    fn from(v: usize) -> Self {
        Self(v)
    }
}

impl From<PhysAddr> for usize {
    fn from(v: PhysAddr) -> Self {
        v.0
    }
}

impl From<VirtAddr> for usize {
    fn from(v: VirtAddr) -> Self {
        v.0
    }
}
