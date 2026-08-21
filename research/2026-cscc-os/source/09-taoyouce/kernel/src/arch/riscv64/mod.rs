//! RISC-V 64-bit 架构支持

pub mod trap;
pub mod trap_context;
pub mod context;
pub mod property_tests;

pub use trap_context::TrapContext;

/// 平台相关常量
pub mod consts {
    /// 内核起始地址
    #[cfg(feature = "qemu")]
    pub const KERNEL_BASE: usize = 0x8020_0000;
    
    #[cfg(feature = "k210")]
    pub const KERNEL_BASE: usize = 0x8002_0000;
    
    #[cfg(not(any(feature = "qemu", feature = "k210")))]
    pub const KERNEL_BASE: usize = 0x8020_0000;
    
    /// 物理内存结束地址（编译期上限 = 帧元数据数组容量；运行时按 FDT /memory
    /// 实测 clamp，见 `mm::init`）。决赛 BuildStorm 判题为 `-m 8G`（QEMU virt
    /// RAM = [0x8000_0000, 0x2_8000_0000)），故上限放到 8 GiB；`-m 1G` 时运行
    /// 时探测仍是 0xC000_0000，行为与旧版完全一致。
    #[cfg(feature = "qemu")]
    pub const MEMORY_END: usize = 0x2_8000_0000;

    #[cfg(feature = "k210")]
    pub const MEMORY_END: usize = 0x8060_0000; // 6MB
    
    #[cfg(not(any(feature = "qemu", feature = "k210")))]
    pub const MEMORY_END: usize = 0x8800_0000;
    
    /// 页大小
    pub const PAGE_SIZE: usize = 4096;
    pub const PAGE_SIZE_BITS: usize = 12;
    
    /// 时钟频率
    #[cfg(feature = "qemu")]
    pub const CLOCK_FREQ: usize = 10_000_000; // 10MHz
    
    #[cfg(feature = "k210")]
    pub const CLOCK_FREQ: usize = 390_000_000; // 390MHz
    
    #[cfg(not(any(feature = "qemu", feature = "k210")))]
    pub const CLOCK_FREQ: usize = 10_000_000;
}

/// 读取 CSR 寄存器
#[macro_export]
macro_rules! read_csr {
    ($csr:literal) => {{
        let value: usize;
        unsafe {
            core::arch::asm!(concat!("csrr {}, ", $csr), out(reg) value);
        }
        value
    }};
}

/// 写入 CSR 寄存器
#[macro_export]
macro_rules! write_csr {
    ($csr:literal, $value:expr) => {{
        unsafe {
            core::arch::asm!(concat!("csrw ", $csr, ", {}"), in(reg) $value);
        }
    }};
}

/// 设置 CSR 位
#[macro_export]
macro_rules! set_csr {
    ($csr:literal, $bits:expr) => {{
        unsafe {
            core::arch::asm!(concat!("csrs ", $csr, ", {}"), in(reg) $bits);
        }
    }};
}

/// 清除 CSR 位
#[macro_export]
macro_rules! clear_csr {
    ($csr:literal, $bits:expr) => {{
        unsafe {
            core::arch::asm!(concat!("csrc ", $csr, ", {}"), in(reg) $bits);
        }
    }};
}
