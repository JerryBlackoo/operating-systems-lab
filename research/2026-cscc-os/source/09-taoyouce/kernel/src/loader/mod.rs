//! ELF 加载器模块
//!
//! 本模块实现 ELF 可执行文件的解析和加载功能，支持将用户态程序加载到进程地址空间。
//!
//! # 功能
//! - ELF 文件格式解析（ELF64 for RISC-V）
//! - 程序段加载（PT_LOAD）
//! - BSS 段清零
//! - 地址空间映射
//!
//! # 使用示例
//! ```rust
//! use crate::loader::{ElfLoader, ElfInfo};
//! use crate::mm::AddressSpace;
//!
//! let elf_data: &[u8] = /* ELF 文件数据 */;
//! let mut address_space = AddressSpace::new();
//! let info = ElfLoader::load(elf_data, &mut address_space)?;
//! println!("Entry point: {:#x}", info.entry);
//! ```

pub mod elf;

mod property_tests;

pub use elf::{
    ElfHeader, ProgramHeader, ElfLoader, ElfInfo, LoaderError,
    // ELF 常量
    ELF_MAGIC, ET_EXEC, ET_DYN, EM_RISCV,
    PT_NULL, PT_LOAD, PT_DYNAMIC, PT_INTERP, PT_NOTE, PT_PHDR,
    PF_X, PF_W, PF_R,
    // ELF 标识常量
    ELFCLASS64, ELFDATA2LSB, EV_CURRENT, ELFOSABI_NONE,
};

/// ELF 加载器属性测试
/// 
/// 运行所有 ELF 解析属性测试：
/// - Property 2: ELF Parsing Correctness (user-program-loading)
/// - Property 6: ELF 解析正确性 (rustmicroos-comprehensive-fix)
/// 
/// Feature: user-program-loading, Property 2: ELF Parsing Correctness
/// **Validates: Requirements 3.1, 3.3, 3.6**
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
pub fn self_test() -> Result<(), &'static str> {
    property_tests::self_test()
}

/// Checkpoint 5: 验证 ELF 加载
/// 
/// 任务 5 的验证点，包含以下测试：
/// - 加载有效 ELF 并验证 entry point 正确
/// - 加载无效数据并验证返回 LoaderError
/// - 加载错误架构 ELF 并验证返回 UnsupportedArch
/// - 打印加载后的段信息（vaddr, memsz, flags）
/// 
/// **Validates: Requirements 3.1, 3.3, 3.6**
pub fn checkpoint_5_verify_elf_loading() -> Result<(), &'static str> {
    property_tests::checkpoint_5_verify_elf_loading()
}
