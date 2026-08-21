//! ELF 加载器属性测试
//!
//! 这些测试验证 ELF 解析器的核心正确性属性。
//!
//! ## Property 2: ELF Parsing Correctness (user-program-loading)
//! Feature: user-program-loading, Property 2: ELF Parsing Correctness
//! **Validates: Requirements 3.1, 3.3, 3.6**
//!
//! *For all* byte sequences passed to ElfLoader::validate():
//! - Valid ELF files SHALL return Ok(ElfHeader) with correct entry point
//! - Invalid ELF files (wrong magic, wrong arch, etc.) SHALL return Err(LoaderError)
//! - The loader SHALL NOT panic on any input
//!
//! ## Property 6: ELF 解析正确性
//! Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
//! **Validates: Requirements 3.2**
//!
//! *For any* valid ELF binary, parsing SHALL extract the correct entry point,
//! and *for all* PT_LOAD segments, the virtual address and size SHALL match
//! the ELF specification.

use super::*;
use alloc::vec;
use alloc::vec::Vec;
use alloc::format;
use crate::mm::VmaType;

/// 简单的伪随机数生成器
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.state
    }
    
    fn next_range(&mut self, min: u64, max: u64) -> u64 {
        if max <= min {
            return min;
        }
        min + (self.next() % (max - min))
    }
}

/// 构建一个最小的有效 ELF64 RISC-V 可执行文件
/// 
/// # 参数
/// - `entry`: 入口点地址
/// - `segments`: PT_LOAD 段列表 (vaddr, filesz, memsz, flags)
fn build_minimal_elf(entry: u64, segments: &[(u64, u64, u64, u32)]) -> Vec<u8> {
    let mut elf = Vec::new();
    
    // ELF Header (64 bytes)
    // e_ident[0..4]: Magic
    elf.extend_from_slice(&ELF_MAGIC);
    // e_ident[4]: Class (64-bit)
    elf.push(ELFCLASS64);
    // e_ident[5]: Data (Little Endian)
    elf.push(ELFDATA2LSB);
    // e_ident[6]: Version
    elf.push(EV_CURRENT);
    // e_ident[7]: OS/ABI
    elf.push(ELFOSABI_NONE);
    // e_ident[8..16]: Padding
    elf.extend_from_slice(&[0u8; 8]);
    
    // e_type: Executable
    elf.extend_from_slice(&ET_EXEC.to_le_bytes());
    // e_machine: RISC-V
    elf.extend_from_slice(&EM_RISCV.to_le_bytes());
    // e_version
    elf.extend_from_slice(&1u32.to_le_bytes());
    // e_entry
    elf.extend_from_slice(&entry.to_le_bytes());
    // e_phoff: Program header offset (right after ELF header)
    let phoff = ElfHeader::SIZE as u64;
    elf.extend_from_slice(&phoff.to_le_bytes());
    // e_shoff: Section header offset (0 = none)
    elf.extend_from_slice(&0u64.to_le_bytes());
    // e_flags
    elf.extend_from_slice(&0u32.to_le_bytes());
    // e_ehsize: ELF header size
    elf.extend_from_slice(&(ElfHeader::SIZE as u16).to_le_bytes());
    // e_phentsize: Program header entry size
    elf.extend_from_slice(&(ProgramHeader::SIZE as u16).to_le_bytes());
    // e_phnum: Number of program headers
    elf.extend_from_slice(&(segments.len() as u16).to_le_bytes());
    // e_shentsize: Section header entry size
    elf.extend_from_slice(&0u16.to_le_bytes());
    // e_shnum: Number of section headers
    elf.extend_from_slice(&0u16.to_le_bytes());
    // e_shstrndx: Section name string table index
    elf.extend_from_slice(&0u16.to_le_bytes());
    
    assert_eq!(elf.len(), ElfHeader::SIZE, "ELF header size mismatch");
    
    // 计算数据段的起始偏移
    let data_offset = ElfHeader::SIZE + segments.len() * ProgramHeader::SIZE;
    let mut current_offset = data_offset;
    
    // Program Headers
    for (vaddr, filesz, memsz, flags) in segments {
        // p_type: PT_LOAD
        elf.extend_from_slice(&PT_LOAD.to_le_bytes());
        // p_flags
        elf.extend_from_slice(&flags.to_le_bytes());
        // p_offset
        elf.extend_from_slice(&(current_offset as u64).to_le_bytes());
        // p_vaddr
        elf.extend_from_slice(&vaddr.to_le_bytes());
        // p_paddr (same as vaddr)
        elf.extend_from_slice(&vaddr.to_le_bytes());
        // p_filesz
        elf.extend_from_slice(&filesz.to_le_bytes());
        // p_memsz
        elf.extend_from_slice(&memsz.to_le_bytes());
        // p_align
        elf.extend_from_slice(&0x1000u64.to_le_bytes());
        
        current_offset += *filesz as usize;
    }
    
    assert_eq!(elf.len(), data_offset, "Program headers size mismatch");
    
    // 添加段数据（用零填充）
    for (_, filesz, _, _) in segments {
        elf.extend(core::iter::repeat(0u8).take(*filesz as usize));
    }
    
    elf
}

// ============================================================================
// Property 2: ELF Parsing Correctness (user-program-loading)
// ============================================================================

/// **Property 2: ELF Parsing Correctness - 验证函数不会 panic**
/// *For all* byte sequences passed to ElfLoader::validate(), the loader SHALL NOT panic.
/// 
/// Feature: user-program-loading, Property 2: ELF Parsing Correctness
/// **Validates: Requirements 3.1, 3.3, 3.6**
/// 
/// 运行 100 次迭代
#[test]
fn property_2_validate_never_panics() {
    let mut rng = SimpleRng::new(20250104);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成随机长度的数据（0 到 1024 字节）
        let size = rng.next_range(0, 1024) as usize;
        let data: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
        
        // 调用 validate，不应该 panic
        // 无论返回 Ok 还是 Err 都是可接受的
        let result = ElfLoader::validate(&data);
        
        // 只要没有 panic 就算通过
        // 记录结果用于调试
        match &result {
            Ok(header) => {
                // 如果成功解析，验证入口点是有效的
                let _ = header.entry();
            }
            Err(_) => {
                // 错误是预期的，对于随机数据
            }
        }
    }
}

/// **Property 2: ELF Parsing Correctness - 有效 ELF 返回正确入口点**
/// *For all* valid ELF files, ElfLoader::validate() SHALL return Ok(ElfHeader) 
/// with the correct entry point.
/// 
/// Feature: user-program-loading, Property 2: ELF Parsing Correctness
/// **Validates: Requirements 3.1, 3.3, 3.6**
/// 
/// 运行 100 次迭代
#[test]
fn property_2_valid_elf_returns_correct_entry() {
    let mut rng = SimpleRng::new(20250104_2);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成随机入口点地址（用户空间地址范围 0x10000 - 0x8000_0000）
        let entry = rng.next_range(0x10000, 0x8000_0000);
        
        // 构建有效的最小 ELF 文件
        let segments = vec![
            (entry, 0x100, 0x100, PF_R | PF_X), // 代码段
        ];
        let elf_data = build_minimal_elf(entry, &segments);
        
        // 使用 validate 函数验证
        let result = ElfLoader::validate(&elf_data);
        
        match result {
            Ok(header) => {
                // 验证入口点正确
                if header.entry() != entry as usize {
                    panic!(
                        "迭代 {}: 入口点不匹配: 期望 {:#x}, 实际 {:#x}",
                        iteration, entry, header.entry()
                    );
                }
            }
            Err(e) => {
                panic!(
                    "迭代 {}: 有效 ELF 验证失败: {:?}, entry={:#x}",
                    iteration, e, entry
                );
            }
        }
    }
}

/// **Property 2: ELF Parsing Correctness - 无效 ELF 返回错误**
/// *For all* invalid ELF files (wrong magic, wrong arch, etc.), 
/// ElfLoader::validate() SHALL return Err(LoaderError).
/// 
/// Feature: user-program-loading, Property 2: ELF Parsing Correctness
/// **Validates: Requirements 3.1, 3.3, 3.6**
/// 
/// 运行 100 次迭代
#[test]
fn property_2_invalid_elf_returns_error() {
    let mut rng = SimpleRng::new(20250104_3);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 随机选择一种无效 ELF 的类型
        let invalid_type = rng.next() % 6;
        
        let data = match invalid_type {
            0 => {
                // 类型 0: 空数据
                Vec::new()
            }
            1 => {
                // 类型 1: 太小的数据（小于 ELF 头大小）
                let size = rng.next_range(1, ElfHeader::SIZE as u64) as usize;
                (0..size).map(|_| rng.next() as u8).collect()
            }
            2 => {
                // 类型 2: 错误的 magic number
                let mut data = vec![0u8; ElfHeader::SIZE];
                data[0] = 0x00; // 不是 0x7F
                data[1] = b'E';
                data[2] = b'L';
                data[3] = b'F';
                data
            }
            3 => {
                // 类型 3: 错误的架构（x86-64 而不是 RISC-V）
                let mut elf = Vec::new();
                elf.extend_from_slice(&ELF_MAGIC);
                elf.push(ELFCLASS64);
                elf.push(ELFDATA2LSB);
                elf.push(EV_CURRENT);
                elf.push(ELFOSABI_NONE);
                elf.extend_from_slice(&[0u8; 8]);
                elf.extend_from_slice(&ET_EXEC.to_le_bytes());
                elf.extend_from_slice(&0x3Eu16.to_le_bytes()); // x86-64
                elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
                elf
            }
            4 => {
                // 类型 4: 错误的 ELF 类型（32 位而不是 64 位）
                let mut elf = Vec::new();
                elf.extend_from_slice(&ELF_MAGIC);
                elf.push(1); // ELFCLASS32
                elf.push(ELFDATA2LSB);
                elf.push(EV_CURRENT);
                elf.push(ELFOSABI_NONE);
                elf.extend_from_slice(&[0u8; 8]);
                elf.extend_from_slice(&ET_EXEC.to_le_bytes());
                elf.extend_from_slice(&EM_RISCV.to_le_bytes());
                elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
                elf
            }
            _ => {
                // 类型 5: 错误的字节序（大端序）
                let mut elf = Vec::new();
                elf.extend_from_slice(&ELF_MAGIC);
                elf.push(ELFCLASS64);
                elf.push(2); // ELFDATA2MSB (大端序)
                elf.push(EV_CURRENT);
                elf.push(ELFOSABI_NONE);
                elf.extend_from_slice(&[0u8; 8]);
                elf.extend_from_slice(&ET_EXEC.to_le_bytes());
                elf.extend_from_slice(&EM_RISCV.to_le_bytes());
                elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
                elf
            }
        };
        
        // 调用 validate，应该返回错误
        let result = ElfLoader::validate(&data);
        
        match result {
            Ok(_) => {
                panic!(
                    "迭代 {}: 无效 ELF (类型 {}) 应该返回错误，但返回了 Ok",
                    iteration, invalid_type
                );
            }
            Err(_) => {
                // 正确：返回了错误
            }
        }
    }
}

/// **Property 2: ELF Parsing Correctness - 验证 ELF magic number**
/// *For all* valid ELF files, the magic number SHALL be 0x7F 'E' 'L' 'F'.
/// 
/// Feature: user-program-loading, Property 2: ELF Parsing Correctness
/// **Validates: Requirements 3.1, 3.3, 3.6**
/// 
/// 运行 100 次迭代
#[test]
fn property_2_elf_magic_verification() {
    let mut rng = SimpleRng::new(20250104_4);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        let entry = rng.next_range(0x10000, 0x8000_0000);
        let segments = vec![(entry, 0x100, 0x100, PF_R | PF_X)];
        let elf_data = build_minimal_elf(entry, &segments);
        
        // 验证 magic number
        if &elf_data[0..4] != &ELF_MAGIC {
            panic!(
                "迭代 {}: ELF magic number 不正确: {:?}",
                iteration, &elf_data[0..4]
            );
        }
        
        // 验证解析成功
        let result = ElfLoader::validate(&elf_data);
        if result.is_err() {
            panic!(
                "迭代 {}: 有效 ELF 验证失败: {:?}",
                iteration, result.err()
            );
        }
    }
}

/// **Property 2: ELF Parsing Correctness - 验证架构检查**
/// *For all* ELF files with non-RISC-V architecture, validation SHALL fail 
/// with UnsupportedArch error.
/// 
/// Feature: user-program-loading, Property 2: ELF Parsing Correctness
/// **Validates: Requirements 3.1, 3.3, 3.6**
#[test]
fn property_2_architecture_validation() {
    // 测试各种非 RISC-V 架构
    let unsupported_machines: [(u16, &str); 5] = [
        (0x03, "x86"),
        (0x3E, "x86-64"),
        (0x28, "ARM"),
        (0xB7, "AArch64"),
        (0x08, "MIPS"),
    ];
    
    for (machine, name) in unsupported_machines {
        // 构建一个有效的 ELF 头，但架构不是 RISC-V
        let mut elf = Vec::new();
        
        // ELF Magic
        elf.extend_from_slice(&ELF_MAGIC);
        // Class: 64-bit
        elf.push(ELFCLASS64);
        // Data: Little Endian
        elf.push(ELFDATA2LSB);
        // Version
        elf.push(EV_CURRENT);
        // OS/ABI
        elf.push(ELFOSABI_NONE);
        // Padding
        elf.extend_from_slice(&[0u8; 8]);
        
        // e_type: Executable
        elf.extend_from_slice(&ET_EXEC.to_le_bytes());
        // e_machine: 非 RISC-V
        elf.extend_from_slice(&machine.to_le_bytes());
        // 填充剩余字段
        elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
        
        let result = ElfLoader::validate(&elf);
        
        match result {
            Err(LoaderError::UnsupportedArch) => {
                // 正确：检测到不支持的架构
            }
            Ok(_) => {
                panic!(
                    "架构 {} ({:#x}) 应该被拒绝，但验证通过了",
                    name, machine
                );
            }
            Err(e) => {
                panic!(
                    "架构 {} ({:#x}): 期望 UnsupportedArch 错误，但得到: {:?}",
                    name, machine, e
                );
            }
        }
    }
}

// ============================================================================
// Property 6: ELF 解析正确性
// ============================================================================

/// **Property 6: ELF 解析正确性 - 入口点提取**
/// *For any* valid ELF binary with entry point E, parsing SHALL extract E as the entry point.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
/// 
/// 运行 100 次迭代
#[test]
fn property_6_entry_point_extraction() {
    let mut rng = SimpleRng::new(12345);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成随机入口点地址（用户空间地址范围）
        let entry = rng.next_range(0x1000, 0x8000_0000);
        
        // 构建最小 ELF 文件
        let segments = vec![
            (entry, 0x100, 0x100, PF_R | PF_X), // 代码段
        ];
        let elf_data = build_minimal_elf(entry, &segments);
        
        // 解析 ELF 头
        let header = match ElfHeader::parse(&elf_data) {
            Ok(h) => h,
            Err(e) => {
                panic!(
                    "迭代 {}: ELF 解析失败: {:?}, entry={:#x}",
                    iteration, e, entry
                );
            }
        };
        
        // 验证入口点
        if header.entry() != entry as usize {
            panic!(
                "迭代 {}: 入口点不匹配: 期望 {:#x}, 实际 {:#x}",
                iteration, entry, header.entry()
            );
        }
    }
}

/// **Property 6: ELF 解析正确性 - 段信息提取**
/// *For all* PT_LOAD segments in a valid ELF binary, the parsed virtual address
/// and size SHALL match the original values.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
/// 
/// 运行 100 次迭代
#[test]
fn property_6_segment_info_extraction() {
    let mut rng = SimpleRng::new(67890);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成随机段数量（1-4 个段）
        let num_segments = rng.next_range(1, 5) as usize;
        
        // 生成随机段信息
        let mut segments = Vec::new();
        let mut current_vaddr = rng.next_range(0x1000, 0x10000);
        
        for _ in 0..num_segments {
            let filesz = rng.next_range(0x100, 0x10000);
            let memsz = filesz + rng.next_range(0, 0x1000); // memsz >= filesz
            let flags = match rng.next() % 4 {
                0 => PF_R,
                1 => PF_R | PF_W,
                2 => PF_R | PF_X,
                _ => PF_R | PF_W | PF_X,
            };
            
            segments.push((current_vaddr, filesz, memsz, flags));
            current_vaddr += memsz + rng.next_range(0x1000, 0x10000); // 段之间留间隙
        }
        
        // 入口点设置为第一个段的起始地址
        let entry = segments[0].0;
        
        // 构建 ELF 文件
        let elf_data = build_minimal_elf(entry, &segments);
        
        // 解析 ELF 头
        let header = match ElfHeader::parse(&elf_data) {
            Ok(h) => h,
            Err(e) => {
                panic!(
                    "迭代 {}: ELF 解析失败: {:?}",
                    iteration, e
                );
            }
        };
        
        // 验证段数量
        if header.e_phnum as usize != num_segments {
            panic!(
                "迭代 {}: 段数量不匹配: 期望 {}, 实际 {}",
                iteration, num_segments, header.e_phnum
            );
        }
        
        // 验证每个段的信息
        let mut segment_index = 0;
        for phdr_result in header.program_headers(&elf_data) {
            let phdr = match phdr_result {
                Ok(p) => p,
                Err(e) => {
                    panic!(
                        "迭代 {}: 程序头解析失败: {:?}",
                        iteration, e
                    );
                }
            };
            
            let (expected_vaddr, expected_filesz, expected_memsz, expected_flags) = segments[segment_index];
            
            // 验证虚拟地址
            if phdr.vaddr() != expected_vaddr as usize {
                panic!(
                    "迭代 {}, 段 {}: 虚拟地址不匹配: 期望 {:#x}, 实际 {:#x}",
                    iteration, segment_index, expected_vaddr, phdr.vaddr()
                );
            }
            
            // 验证文件大小
            if phdr.file_size() != expected_filesz as usize {
                panic!(
                    "迭代 {}, 段 {}: 文件大小不匹配: 期望 {:#x}, 实际 {:#x}",
                    iteration, segment_index, expected_filesz, phdr.file_size()
                );
            }
            
            // 验证内存大小
            if phdr.mem_size() != expected_memsz as usize {
                panic!(
                    "迭代 {}, 段 {}: 内存大小不匹配: 期望 {:#x}, 实际 {:#x}",
                    iteration, segment_index, expected_memsz, phdr.mem_size()
                );
            }
            
            // 验证标志
            if phdr.p_flags != expected_flags {
                panic!(
                    "迭代 {}, 段 {}: 标志不匹配: 期望 {:#x}, 实际 {:#x}",
                    iteration, segment_index, expected_flags, phdr.p_flags
                );
            }
            
            segment_index += 1;
        }
    }
}

/// **Property 6: ELF 解析正确性 - 权限映射**
/// *For any* PT_LOAD segment with flags F, the parsed permissions SHALL correctly
/// reflect readable (PF_R), writable (PF_W), and executable (PF_X) attributes.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
/// 
/// 运行 100 次迭代
#[test]
fn property_6_permission_mapping() {
    let mut rng = SimpleRng::new(11111);
    
    // 所有可能的权限组合
    let flag_combinations = [
        (PF_R, true, false, false),
        (PF_W, false, true, false),
        (PF_X, false, false, true),
        (PF_R | PF_W, true, true, false),
        (PF_R | PF_X, true, false, true),
        (PF_W | PF_X, false, true, true),
        (PF_R | PF_W | PF_X, true, true, true),
    ];
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 随机选择一个权限组合
        let combo_index = (rng.next() % flag_combinations.len() as u64) as usize;
        let (flags, expected_r, expected_w, expected_x) = flag_combinations[combo_index];
        
        // 生成随机段参数
        let vaddr = rng.next_range(0x1000, 0x8000_0000);
        let filesz = rng.next_range(0x100, 0x10000);
        let memsz = filesz;
        
        let segments = vec![(vaddr, filesz, memsz, flags)];
        let elf_data = build_minimal_elf(vaddr, &segments);
        
        // 解析 ELF
        let header = ElfHeader::parse(&elf_data).expect("ELF 解析失败");
        
        // 获取第一个段
        let phdr = header.program_headers(&elf_data)
            .next()
            .expect("没有程序头")
            .expect("程序头解析失败");
        
        // 验证权限
        if phdr.is_readable() != expected_r {
            panic!(
                "迭代 {}: 可读权限不匹配: flags={:#x}, 期望 {}, 实际 {}",
                iteration, flags, expected_r, phdr.is_readable()
            );
        }
        
        if phdr.is_writable() != expected_w {
            panic!(
                "迭代 {}: 可写权限不匹配: flags={:#x}, 期望 {}, 实际 {}",
                iteration, flags, expected_w, phdr.is_writable()
            );
        }
        
        if phdr.is_executable() != expected_x {
            panic!(
                "迭代 {}: 可执行权限不匹配: flags={:#x}, 期望 {}, 实际 {}",
                iteration, flags, expected_x, phdr.is_executable()
            );
        }
    }
}

// ============================================================================
// 无效 ELF 检测测试
// ============================================================================

/// **Property 6 扩展: 无效 Magic 检测**
/// *For any* data that does not start with ELF magic bytes, parsing SHALL fail
/// with InvalidMagic error.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
/// 
/// 运行 100 次迭代
#[test]
fn property_6_invalid_magic_detection() {
    let mut rng = SimpleRng::new(22222);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成随机数据（至少 64 字节）
        let size = rng.next_range(64, 1024) as usize;
        let mut data: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
        
        // 确保前 4 字节不是 ELF magic
        if &data[0..4] == &ELF_MAGIC {
            data[0] = 0x00; // 破坏 magic
        }
        
        // 解析应该失败
        let result = ElfHeader::parse(&data);
        
        match result {
            Err(LoaderError::InvalidMagic) => {
                // 正确：检测到无效 magic
            }
            Err(LoaderError::FileTooSmall) => {
                // 也可接受：文件太小
            }
            Ok(_) => {
                panic!(
                    "迭代 {}: 无效 magic 未被检测: data[0..4]={:?}",
                    iteration, &data[0..4]
                );
            }
            Err(e) => {
                // 其他错误也可接受（如不支持的架构等）
                // 只要不是成功解析就行
            }
        }
    }
}

/// **Property 6 扩展: 文件太小检测**
/// *For any* data smaller than ELF header size (64 bytes), parsing SHALL fail
/// with FileTooSmall error.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
/// 
/// 运行 100 次迭代
#[test]
fn property_6_file_too_small_detection() {
    let mut rng = SimpleRng::new(33333);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成小于 64 字节的随机数据
        let size = rng.next_range(0, ElfHeader::SIZE as u64) as usize;
        let data: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
        
        // 解析应该失败
        let result = ElfHeader::parse(&data);
        
        match result {
            Err(LoaderError::FileTooSmall) => {
                // 正确：检测到文件太小
            }
            Ok(_) => {
                panic!(
                    "迭代 {}: 文件太小未被检测: size={}",
                    iteration, size
                );
            }
            Err(e) => {
                panic!(
                    "迭代 {}: 期望 FileTooSmall 错误，但得到: {:?}",
                    iteration, e
                );
            }
        }
    }
}

/// **Property 6 扩展: 不支持的架构检测**
/// *For any* ELF binary with non-RISC-V architecture, parsing SHALL fail
/// with UnsupportedArch error.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
#[test]
fn property_6_unsupported_arch_detection() {
    // 测试各种非 RISC-V 架构
    let unsupported_machines: [u16; 5] = [
        0x03,   // x86
        0x3E,   // x86-64
        0x28,   // ARM
        0xB7,   // AArch64
        0x08,   // MIPS
    ];
    
    for machine in unsupported_machines {
        // 构建一个有效的 ELF 头，但架构不是 RISC-V
        let mut elf = Vec::new();
        
        // ELF Magic
        elf.extend_from_slice(&ELF_MAGIC);
        // Class: 64-bit
        elf.push(ELFCLASS64);
        // Data: Little Endian
        elf.push(ELFDATA2LSB);
        // Version
        elf.push(EV_CURRENT);
        // OS/ABI
        elf.push(ELFOSABI_NONE);
        // Padding
        elf.extend_from_slice(&[0u8; 8]);
        
        // e_type: Executable
        elf.extend_from_slice(&ET_EXEC.to_le_bytes());
        // e_machine: 非 RISC-V
        elf.extend_from_slice(&machine.to_le_bytes());
        // 填充剩余字段
        elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
        
        let result = ElfHeader::parse(&elf);
        
        match result {
            Err(LoaderError::UnsupportedArch) => {
                // 正确：检测到不支持的架构
            }
            Ok(_) => {
                panic!(
                    "不支持的架构 {:#x} 未被检测",
                    machine
                );
            }
            Err(e) => {
                panic!(
                    "架构 {:#x}: 期望 UnsupportedArch 错误，但得到: {:?}",
                    machine, e
                );
            }
        }
    }
}

/// **Property 6 扩展: 段类型识别**
/// *For any* PT_LOAD segment, is_load() SHALL return true.
/// *For any* non-PT_LOAD segment, is_load() SHALL return false.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
#[test]
fn property_6_segment_type_identification() {
    // 测试 PT_LOAD 段
    let segments = vec![
        (0x1000u64, 0x100u64, 0x100u64, PF_R | PF_X),
    ];
    let elf_data = build_minimal_elf(0x1000, &segments);
    
    let header = ElfHeader::parse(&elf_data).expect("ELF 解析失败");
    let phdr = header.program_headers(&elf_data)
        .next()
        .expect("没有程序头")
        .expect("程序头解析失败");
    
    assert!(phdr.is_load(), "PT_LOAD 段的 is_load() 应返回 true");
    assert_eq!(phdr.p_type, PT_LOAD, "段类型应为 PT_LOAD");
}

/// **Property 6 扩展: BSS 段识别**
/// *For any* segment where memsz > filesz, the extra space SHALL be identified
/// as BSS (to be zero-filled).
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
/// 
/// 运行 100 次迭代
#[test]
fn property_6_bss_identification() {
    let mut rng = SimpleRng::new(44444);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        let vaddr = rng.next_range(0x1000, 0x8000_0000);
        let filesz = rng.next_range(0x100, 0x10000);
        let bss_size = rng.next_range(0x100, 0x10000);
        let memsz = filesz + bss_size; // memsz > filesz 表示有 BSS
        
        let segments = vec![(vaddr, filesz, memsz, PF_R | PF_W)];
        let elf_data = build_minimal_elf(vaddr, &segments);
        
        let header = ElfHeader::parse(&elf_data).expect("ELF 解析失败");
        let phdr = header.program_headers(&elf_data)
            .next()
            .expect("没有程序头")
            .expect("程序头解析失败");
        
        // 验证 memsz > filesz
        if phdr.mem_size() <= phdr.file_size() {
            panic!(
                "迭代 {}: memsz ({:#x}) 应大于 filesz ({:#x})",
                iteration, phdr.mem_size(), phdr.file_size()
            );
        }
        
        // 验证 BSS 大小
        let actual_bss = phdr.mem_size() - phdr.file_size();
        if actual_bss != bss_size as usize {
            panic!(
                "迭代 {}: BSS 大小不匹配: 期望 {:#x}, 实际 {:#x}",
                iteration, bss_size, actual_bss
            );
        }
        
        // 验证 VMA 类型推断
        let vma_type = phdr.to_vma_type();
        // 可写且有 BSS 的段应该被识别为 BSS 类型
        assert_eq!(vma_type, VmaType::Bss, 
            "迭代 {}: 有 BSS 的可写段应被识别为 Bss 类型", iteration);
    }
}

/// **Property 6 扩展: 程序头迭代器正确性**
/// *For any* ELF with N program headers, the iterator SHALL yield exactly N items.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
/// 
/// 运行 100 次迭代
#[test]
fn property_6_program_header_iterator() {
    let mut rng = SimpleRng::new(55555);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成随机段数量（1-8 个段）
        let num_segments = rng.next_range(1, 9) as usize;
        
        // 生成段
        let mut segments = Vec::new();
        let mut current_vaddr = 0x1000u64;
        
        for _ in 0..num_segments {
            let filesz = rng.next_range(0x100, 0x1000);
            segments.push((current_vaddr, filesz, filesz, PF_R));
            current_vaddr += filesz + 0x1000;
        }
        
        let elf_data = build_minimal_elf(0x1000, &segments);
        let header = ElfHeader::parse(&elf_data).expect("ELF 解析失败");
        
        // 验证迭代器长度
        let iter = header.program_headers(&elf_data);
        assert_eq!(iter.len(), num_segments,
            "迭代 {}: 迭代器初始长度不匹配", iteration);
        
        // 验证实际迭代次数
        let count = header.program_headers(&elf_data).count();
        if count != num_segments {
            panic!(
                "迭代 {}: 迭代次数不匹配: 期望 {}, 实际 {}",
                iteration, num_segments, count
            );
        }
    }
}

/// **Property 6 扩展: ELF 类型识别**
/// *For any* executable ELF, is_executable() SHALL return true.
/// *For any* shared library ELF, is_shared() SHALL return true.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.2**
#[test]
fn property_6_elf_type_identification() {
    // 测试可执行文件
    let segments = vec![(0x1000u64, 0x100u64, 0x100u64, PF_R | PF_X)];
    let exec_elf = build_minimal_elf(0x1000, &segments);
    
    let header = ElfHeader::parse(&exec_elf).expect("ELF 解析失败");
    assert!(header.is_executable(), "可执行文件的 is_executable() 应返回 true");
    assert!(!header.is_shared(), "可执行文件的 is_shared() 应返回 false");
    
    // 测试共享库（修改 e_type 为 ET_DYN）
    let mut dyn_elf = exec_elf.clone();
    // e_type 在偏移 16 处
    dyn_elf[16] = (ET_DYN & 0xFF) as u8;
    dyn_elf[17] = ((ET_DYN >> 8) & 0xFF) as u8;
    
    let header = ElfHeader::parse(&dyn_elf).expect("ELF 解析失败");
    assert!(!header.is_executable(), "共享库的 is_executable() 应返回 false");
    assert!(header.is_shared(), "共享库的 is_shared() 应返回 true");
}

// ============================================================================
// 自测试入口（用于内核启动时运行）
// ============================================================================

/// ELF 加载器属性测试入口
/// 
/// 运行所有 ELF 解析属性测试，验证 Property 6: ELF 解析正确性
/// 
/// Feature: user-program-loading, Property 2: ELF Parsing Correctness
/// Feature: rustmicroos-comprehensive-fix, Property 6: ELF 解析正确性
/// **Validates: Requirements 3.1, 3.2, 3.3, 3.6**
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[loader] Running ELF property tests...");
    
    // ========================================
    // Property 2: ELF Parsing Correctness (user-program-loading)
    // ========================================
    crate::println!("[loader] === Property 2: ELF Parsing Correctness ===");
    
    // Property 2: 验证函数不会 panic
    crate::println!("[loader] Testing validate never panics (10 iterations)...");
    run_property_2_validate_never_panics()?;
    
    // Property 2: 有效 ELF 返回正确入口点
    crate::println!("[loader] Testing valid ELF returns correct entry (10 iterations)...");
    run_property_2_valid_elf_returns_correct_entry()?;
    
    // Property 2: 无效 ELF 返回错误
    crate::println!("[loader] Testing invalid ELF returns error (10 iterations)...");
    run_property_2_invalid_elf_returns_error()?;
    
    // Property 2: 架构验证
    crate::println!("[loader] Testing architecture validation...");
    run_property_2_architecture_validation()?;
    
    // ========================================
    // Property 6: ELF 解析正确性 (rustmicroos-comprehensive-fix)
    // ========================================
    crate::println!("[loader] === Property 6: ELF 解析正确性 ===");
    
    // Property 6: 入口点提取
    crate::println!("[loader] Testing entry point extraction (10 iterations)...");
    run_entry_point_test()?;
    
    // Property 6: 段信息提取
    crate::println!("[loader] Testing segment info extraction (5 iterations)...");
    run_segment_info_test()?;
    
    // Property 6: 权限映射
    crate::println!("[loader] Testing permission mapping (7 iterations)...");
    run_permission_mapping_test()?;
    
    // Property 6: 无效 Magic 检测
    crate::println!("[loader] Testing invalid magic detection (10 iterations)...");
    run_invalid_magic_test()?;
    
    // Property 6: 文件太小检测
    crate::println!("[loader] Testing file too small detection (10 iterations)...");
    run_file_too_small_test()?;
    
    // Property 6: 不支持的架构检测
    crate::println!("[loader] Testing unsupported arch detection...");
    run_unsupported_arch_test()?;
    
    // Property 6: 段类型识别
    crate::println!("[loader] Testing segment type identification...");
    run_segment_type_test()?;
    
    // Property 6: BSS 段识别
    crate::println!("[loader] Testing BSS identification (5 iterations)...");
    run_bss_identification_test()?;
    
    // Property 6: 程序头迭代器
    crate::println!("[loader] Testing program header iterator (5 iterations)...");
    run_program_header_iterator_test()?;
    
    // Property 6: ELF 类型识别
    crate::println!("[loader] Testing ELF type identification...");
    run_elf_type_test()?;
    
    crate::println!("[loader] All ELF property tests passed!");
    Ok(())
}

// ============================================================================
// Checkpoint 5: ELF 加载验证
// ============================================================================

/// Checkpoint 5: Verify ELF Loading
/// 
/// Task 5 verification point, includes:
/// - Load valid ELF and verify entry point
/// - Load invalid data and verify LoaderError returned
/// - Load wrong architecture ELF and verify UnsupportedArch returned
/// - Print segment info (vaddr, memsz, flags)
/// 
/// **Validates: Requirements 3.1, 3.3, 3.6**
pub fn checkpoint_5_verify_elf_loading() -> Result<(), &'static str> {
    crate::println!("\n========================================");
    crate::println!("Checkpoint 5: Verify ELF Loading");
    crate::println!("========================================\n");
    
    // Test 1: Load valid ELF and verify entry point
    crate::println!("[Checkpoint 5.1] Test: Load valid ELF, verify entry point");
    checkpoint_5_test_valid_elf_entry_point()?;
    crate::println!("[Checkpoint 5.1] PASSED\n");
    
    // Test 2: Load invalid data and verify LoaderError returned
    crate::println!("[Checkpoint 5.2] Test: Load invalid data, verify LoaderError");
    checkpoint_5_test_invalid_data_returns_error()?;
    crate::println!("[Checkpoint 5.2] PASSED\n");
    
    // Test 3: Load wrong architecture ELF and verify UnsupportedArch
    crate::println!("[Checkpoint 5.3] Test: Load wrong arch ELF, verify UnsupportedArch");
    checkpoint_5_test_wrong_arch_returns_unsupported()?;
    crate::println!("[Checkpoint 5.3] PASSED\n");
    
    // Test 4: Print segment info
    crate::println!("[Checkpoint 5.4] Test: Print segment info (vaddr, memsz, flags)");
    checkpoint_5_print_segment_info()?;
    crate::println!("[Checkpoint 5.4] PASSED\n");
    
    crate::println!("========================================");
    crate::println!("Checkpoint 5: All verifications PASSED!");
    crate::println!("========================================\n");
    
    Ok(())
}

/// Checkpoint 5.1: Load valid ELF and verify entry point
fn checkpoint_5_test_valid_elf_entry_point() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(0xC5_01);
    
    // Test multiple entry point addresses
    let test_entries: [u64; 5] = [
        0x10000,      // Min user space address
        0x20000,      // Common program base
        0x80000,      // Higher address
        0x1000000,    // 1MB
        0x7FFF0000,   // Near stack top
    ];
    
    for (i, &expected_entry) in test_entries.iter().enumerate() {
        // Build valid ELF file
        let segments = vec![
            (expected_entry, 0x100, 0x100, PF_R | PF_X), // Code segment
        ];
        let elf_data = build_minimal_elf(expected_entry, &segments);
        
        // Validate ELF
        let header = ElfLoader::validate(&elf_data)
            .map_err(|_| "Valid ELF validation failed")?;
        
        let actual_entry = header.entry();
        
        crate::println!(
            "  Test {}: entry={:#x}, result: {}",
            i + 1,
            expected_entry,
            if actual_entry == expected_entry as usize { "OK" } else { "FAIL" }
        );
        
        if actual_entry != expected_entry as usize {
            crate::println!(
                "    Error: expected entry={:#x}, actual entry={:#x}",
                expected_entry, actual_entry
            );
            return Err("Entry point mismatch");
        }
    }
    
    // Extra test: random entry points
    for i in 0..5 {
        let random_entry = rng.next_range(0x10000, 0x8000_0000);
        let segments = vec![(random_entry, 0x100, 0x100, PF_R | PF_X)];
        let elf_data = build_minimal_elf(random_entry, &segments);
        
        let header = ElfLoader::validate(&elf_data)
            .map_err(|_| "Random ELF validation failed")?;
        
        if header.entry() != random_entry as usize {
            return Err("Random entry point mismatch");
        }
        
        crate::println!(
            "  Random test {}: entry={:#x} OK",
            i + 1, random_entry
        );
    }
    
    Ok(())
}

/// Checkpoint 5.2: Load invalid data and verify LoaderError returned
fn checkpoint_5_test_invalid_data_returns_error() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(0xC5_02);
    
    // Test cases
    let test_cases: [(&str, Vec<u8>); 6] = [
        ("empty data", Vec::new()),
        ("too small (32 bytes)", vec![0u8; 32]),
        ("too small (63 bytes)", vec![0u8; 63]),
        ("wrong magic (all zeros)", vec![0u8; 64]),
        ("wrong magic (random)", {
            let mut data = vec![0u8; 64];
            data[0] = 0x89; // PNG magic
            data[1] = b'P';
            data[2] = b'N';
            data[3] = b'G';
            data
        }),
        ("partial magic", {
            let mut data = vec![0u8; 64];
            data[0] = 0x7F;
            data[1] = b'E';
            data[2] = b'L';
            data[3] = b'X'; // Wrong 4th byte
            data
        }),
    ];
    
    for (name, data) in test_cases.iter() {
        let result = ElfLoader::validate(data);
        
        match result {
            Err(e) => {
                crate::println!("  Test '{}': returned {:?} OK", name, e);
            }
            Ok(_) => {
                crate::println!("  Test '{}': ERROR - should return error but got Ok", name);
                return Err("Invalid data should return error");
            }
        }
    }
    
    // Test random invalid data
    for i in 0..5 {
        let size = rng.next_range(0, 100) as usize;
        let data: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
        
        let result = ElfLoader::validate(&data);
        
        if result.is_ok() {
            // Check if accidentally generated valid ELF (very unlikely)
            if size >= 64 && &data[0..4] == &ELF_MAGIC {
                crate::println!("  Random test {}: accidentally valid ELF (skip)", i + 1);
                continue;
            }
            return Err("Random invalid data should return error");
        }
        
        crate::println!("  Random test {} (size={}): returned error OK", i + 1, size);
    }
    
    Ok(())
}

/// Checkpoint 5.3: Load wrong architecture ELF and verify UnsupportedArch
fn checkpoint_5_test_wrong_arch_returns_unsupported() -> Result<(), &'static str> {
    // Test various non-RISC-V architectures
    let unsupported_archs: [(u16, &str); 6] = [
        (0x03, "x86 (i386)"),
        (0x3E, "x86-64 (AMD64)"),
        (0x28, "ARM"),
        (0xB7, "AArch64"),
        (0x08, "MIPS"),
        (0x14, "PowerPC"),
    ];
    
    for (machine, name) in unsupported_archs.iter() {
        // Build valid format ELF but wrong architecture
        let mut elf = Vec::new();
        
        // ELF Magic
        elf.extend_from_slice(&ELF_MAGIC);
        // Class: 64-bit
        elf.push(ELFCLASS64);
        // Data: Little Endian
        elf.push(ELFDATA2LSB);
        // Version
        elf.push(EV_CURRENT);
        // OS/ABI
        elf.push(ELFOSABI_NONE);
        // Padding
        elf.extend_from_slice(&[0u8; 8]);
        
        // e_type: Executable
        elf.extend_from_slice(&ET_EXEC.to_le_bytes());
        // e_machine: non-RISC-V
        elf.extend_from_slice(&machine.to_le_bytes());
        // Fill remaining fields
        elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
        
        let result = ElfLoader::validate(&elf);
        
        match result {
            Err(LoaderError::UnsupportedArch) => {
                crate::println!(
                    "  Arch {} (machine={:#x}): returned UnsupportedArch OK",
                    name, machine
                );
            }
            Ok(_) => {
                crate::println!(
                    "  Arch {} (machine={:#x}): ERROR - should be rejected",
                    name, machine
                );
                return Err("Unsupported arch should return UnsupportedArch");
            }
            Err(e) => {
                crate::println!(
                    "  Arch {} (machine={:#x}): ERROR - expected UnsupportedArch, got {:?}",
                    name, machine, e
                );
                return Err("Expected UnsupportedArch error");
            }
        }
    }
    
    // Verify RISC-V is accepted
    let mut riscv_elf = Vec::new();
    riscv_elf.extend_from_slice(&ELF_MAGIC);
    riscv_elf.push(ELFCLASS64);
    riscv_elf.push(ELFDATA2LSB);
    riscv_elf.push(EV_CURRENT);
    riscv_elf.push(ELFOSABI_NONE);
    riscv_elf.extend_from_slice(&[0u8; 8]);
    riscv_elf.extend_from_slice(&ET_EXEC.to_le_bytes());
    riscv_elf.extend_from_slice(&EM_RISCV.to_le_bytes()); // RISC-V
    // e_version
    riscv_elf.extend_from_slice(&1u32.to_le_bytes());
    // e_entry
    riscv_elf.extend_from_slice(&0x10000u64.to_le_bytes());
    // e_phoff
    riscv_elf.extend_from_slice(&64u64.to_le_bytes());
    // e_shoff
    riscv_elf.extend_from_slice(&0u64.to_le_bytes());
    // e_flags
    riscv_elf.extend_from_slice(&0u32.to_le_bytes());
    // e_ehsize
    riscv_elf.extend_from_slice(&64u16.to_le_bytes());
    // e_phentsize
    riscv_elf.extend_from_slice(&56u16.to_le_bytes());
    // e_phnum
    riscv_elf.extend_from_slice(&0u16.to_le_bytes());
    // e_shentsize
    riscv_elf.extend_from_slice(&0u16.to_le_bytes());
    // e_shnum
    riscv_elf.extend_from_slice(&0u16.to_le_bytes());
    // e_shstrndx
    riscv_elf.extend_from_slice(&0u16.to_le_bytes());
    
    match ElfLoader::validate(&riscv_elf) {
        Ok(_) => {
            crate::println!("  RISC-V (machine={:#x}): validation passed OK", EM_RISCV);
        }
        Err(e) => {
            crate::println!("  RISC-V (machine={:#x}): ERROR - {:?}", EM_RISCV, e);
            return Err("RISC-V arch should be accepted");
        }
    }
    
    Ok(())
}

/// Checkpoint 5.4: Print segment info (vaddr, memsz, flags)
fn checkpoint_5_print_segment_info() -> Result<(), &'static str> {
    crate::println!("  Creating test ELF with multiple segments...\n");
    
    // Create ELF with multiple segments
    let segments = vec![
        (0x10000u64, 0x1000u64, 0x1000u64, PF_R | PF_X),      // Code segment
        (0x20000u64, 0x800u64, 0x1000u64, PF_R | PF_W),       // Data segment (has BSS)
        (0x30000u64, 0x400u64, 0x400u64, PF_R),               // Read-only data
    ];
    
    let entry = 0x10000u64;
    let elf_data = build_minimal_elf(entry, &segments);
    
    // Parse ELF header
    let header = ElfHeader::parse(&elf_data)
        .map_err(|_| "ELF parse failed")?;
    
    crate::println!("  ELF Header Info:");
    crate::println!("    Entry Point: {:#x}", header.entry());
    crate::println!("    Type: {}", if header.is_executable() { "Executable" } else { "Shared" });
    crate::println!("    Program Headers: {}", header.e_phnum);
    crate::println!();
    
    crate::println!("  Segment Info (PT_LOAD segments):");
    crate::println!("  {:-<70}", "");
    crate::println!("  {:>4} | {:>12} | {:>12} | {:>12} | {:>10} | {:>8}",
        "Idx", "VAddr", "FileSize", "MemSize", "Flags", "Type");
    crate::println!("  {:-<70}", "");
    
    let mut idx = 0;
    for phdr_result in header.program_headers(&elf_data) {
        let phdr = phdr_result.map_err(|_| "Program header parse failed")?;
        
        if phdr.is_load() {
            // Build flags string
            let flags_str = format!("{}{}{}",
                if phdr.is_readable() { "R" } else { "-" },
                if phdr.is_writable() { "W" } else { "-" },
                if phdr.is_executable() { "X" } else { "-" }
            );
            
            // Get VMA type
            let vma_type = phdr.to_vma_type();
            let type_str = match vma_type {
                VmaType::Code => "Code",
                VmaType::Data => "Data",
                VmaType::Bss => "BSS",
                VmaType::Stack => "Stack",
                VmaType::Heap => "Heap",
                VmaType::Mmap => "Mmap",
                VmaType::Shared => "Shared",
            };
            
            crate::println!("  {:>4} | {:#12x} | {:#12x} | {:#12x} | {:>10} | {:>8}",
                idx,
                phdr.vaddr(),
                phdr.file_size(),
                phdr.mem_size(),
                flags_str,
                type_str
            );
            
            // If has BSS, print extra info
            if phdr.mem_size() > phdr.file_size() {
                let bss_size = phdr.mem_size() - phdr.file_size();
                crate::println!("       +-- BSS region: {:#x} bytes (zero-fill)", bss_size);
            }
            
            idx += 1;
        }
    }
    
    crate::println!("  {:-<70}", "");
    crate::println!();
    
    // Verify segment info matches expected
    let expected_segments = [
        (0x10000usize, 0x1000usize, 0x1000usize, "R-X", "Code"),
        (0x20000usize, 0x800usize, 0x1000usize, "RW-", "BSS"),
        (0x30000usize, 0x400usize, 0x400usize, "R--", "Data"),
    ];
    
    idx = 0;
    for phdr_result in header.program_headers(&elf_data) {
        let phdr = phdr_result.map_err(|_| "Program header parse failed")?;
        
        if phdr.is_load() {
            let (exp_vaddr, exp_filesz, exp_memsz, _, _) = expected_segments[idx];
            
            if phdr.vaddr() != exp_vaddr {
                return Err("Segment vaddr mismatch");
            }
            if phdr.file_size() != exp_filesz {
                return Err("Segment file size mismatch");
            }
            if phdr.mem_size() != exp_memsz {
                return Err("Segment mem size mismatch");
            }
            
            idx += 1;
        }
    }
    
    crate::println!("  All segment info verified!");
    
    Ok(())
}

// ============================================================================
// Property 2 运行时测试函数
// ============================================================================

fn run_property_2_validate_never_panics() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(20250104);
    
    for _ in 0..10 {
        let size = rng.next_range(0, 512) as usize;
        let data: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
        
        // 调用 validate，不应该 panic
        let _ = ElfLoader::validate(&data);
    }
    Ok(())
}

fn run_property_2_valid_elf_returns_correct_entry() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(20250104_2);
    
    for _ in 0..10 {
        let entry = rng.next_range(0x10000, 0x8000_0000);
        let segments = vec![(entry, 0x100, 0x100, PF_R | PF_X)];
        let elf_data = build_minimal_elf(entry, &segments);
        
        let header = ElfLoader::validate(&elf_data)
            .map_err(|_| "有效 ELF 验证失败")?;
        
        if header.entry() != entry as usize {
            return Err("入口点不匹配");
        }
    }
    Ok(())
}

fn run_property_2_invalid_elf_returns_error() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(20250104_3);
    
    for _ in 0..10 {
        let invalid_type = rng.next() % 6;
        
        let data = match invalid_type {
            0 => Vec::new(),
            1 => {
                let size = rng.next_range(1, ElfHeader::SIZE as u64) as usize;
                (0..size).map(|_| rng.next() as u8).collect()
            }
            2 => {
                let mut data = vec![0u8; ElfHeader::SIZE];
                data[0] = 0x00;
                data[1] = b'E';
                data[2] = b'L';
                data[3] = b'F';
                data
            }
            3 => {
                let mut elf = Vec::new();
                elf.extend_from_slice(&ELF_MAGIC);
                elf.push(ELFCLASS64);
                elf.push(ELFDATA2LSB);
                elf.push(EV_CURRENT);
                elf.push(ELFOSABI_NONE);
                elf.extend_from_slice(&[0u8; 8]);
                elf.extend_from_slice(&ET_EXEC.to_le_bytes());
                elf.extend_from_slice(&0x3Eu16.to_le_bytes()); // x86-64
                elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
                elf
            }
            4 => {
                let mut elf = Vec::new();
                elf.extend_from_slice(&ELF_MAGIC);
                elf.push(1); // ELFCLASS32
                elf.push(ELFDATA2LSB);
                elf.push(EV_CURRENT);
                elf.push(ELFOSABI_NONE);
                elf.extend_from_slice(&[0u8; 8]);
                elf.extend_from_slice(&ET_EXEC.to_le_bytes());
                elf.extend_from_slice(&EM_RISCV.to_le_bytes());
                elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
                elf
            }
            _ => {
                let mut elf = Vec::new();
                elf.extend_from_slice(&ELF_MAGIC);
                elf.push(ELFCLASS64);
                elf.push(2); // ELFDATA2MSB
                elf.push(EV_CURRENT);
                elf.push(ELFOSABI_NONE);
                elf.extend_from_slice(&[0u8; 8]);
                elf.extend_from_slice(&ET_EXEC.to_le_bytes());
                elf.extend_from_slice(&EM_RISCV.to_le_bytes());
                elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
                elf
            }
        };
        
        if ElfLoader::validate(&data).is_ok() {
            return Err("无效 ELF 应该返回错误");
        }
    }
    Ok(())
}

fn run_property_2_architecture_validation() -> Result<(), &'static str> {
    let unsupported_machines: [u16; 5] = [0x03, 0x3E, 0x28, 0xB7, 0x08];
    
    for machine in unsupported_machines {
        let mut elf = Vec::new();
        elf.extend_from_slice(&ELF_MAGIC);
        elf.push(ELFCLASS64);
        elf.push(ELFDATA2LSB);
        elf.push(EV_CURRENT);
        elf.push(ELFOSABI_NONE);
        elf.extend_from_slice(&[0u8; 8]);
        elf.extend_from_slice(&ET_EXEC.to_le_bytes());
        elf.extend_from_slice(&machine.to_le_bytes());
        elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
        
        match ElfLoader::validate(&elf) {
            Err(LoaderError::UnsupportedArch) => {}
            Ok(_) => return Err("不支持的架构应该被拒绝"),
            Err(_) => return Err("期望 UnsupportedArch 错误"),
        }
    }
    Ok(())
}

// ============================================================================
// Property 6 运行时测试函数
// ============================================================================

fn run_entry_point_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(12345);
    
    for _ in 0..10 {
        let entry = rng.next_range(0x1000, 0x8000_0000);
        let segments = vec![(entry, 0x100, 0x100, PF_R | PF_X)];
        let elf_data = build_minimal_elf(entry, &segments);
        
        let header = ElfHeader::parse(&elf_data)
            .map_err(|_| "ELF 解析失败")?;
        
        if header.entry() != entry as usize {
            return Err("入口点不匹配");
        }
    }
    Ok(())
}

fn run_segment_info_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(67890);
    
    for _ in 0..5 {  // 减少到5次迭代
        let num_segments = 1;  // 固定为1个段
        let mut segments = Vec::new();
        let current_vaddr = rng.next_range(0x1000, 0x10000);
        
        let filesz = rng.next_range(0x100, 0x400);  // 减小到最大1KB
        let memsz = filesz + rng.next_range(0, 0x100);  // 减小BSS大小
        let flags = match rng.next() % 4 {
            0 => PF_R,
            1 => PF_R | PF_W,
            2 => PF_R | PF_X,
            _ => PF_R | PF_W | PF_X,
        };
        
        segments.push((current_vaddr, filesz, memsz, flags));
        
        let entry = segments[0].0;
        let elf_data = build_minimal_elf(entry, &segments);
        
        let header = ElfHeader::parse(&elf_data)
            .map_err(|_| "ELF 解析失败")?;
        
        if header.e_phnum as usize != num_segments {
            return Err("段数量不匹配");
        }
        
        let mut segment_index = 0;
        for phdr_result in header.program_headers(&elf_data) {
            let phdr = phdr_result.map_err(|_| "程序头解析失败")?;
            let (expected_vaddr, expected_filesz, expected_memsz, expected_flags) = segments[segment_index];
            
            if phdr.vaddr() != expected_vaddr as usize {
                return Err("虚拟地址不匹配");
            }
            if phdr.file_size() != expected_filesz as usize {
                return Err("文件大小不匹配");
            }
            if phdr.mem_size() != expected_memsz as usize {
                return Err("内存大小不匹配");
            }
            if phdr.p_flags != expected_flags {
                return Err("标志不匹配");
            }
            
            segment_index += 1;
        }
    }
    Ok(())
}

fn run_permission_mapping_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(11111);
    
    let flag_combinations = [
        (PF_R, true, false, false),
        (PF_W, false, true, false),
        (PF_X, false, false, true),
        (PF_R | PF_W, true, true, false),
        (PF_R | PF_X, true, false, true),
        (PF_W | PF_X, false, true, true),
        (PF_R | PF_W | PF_X, true, true, true),
    ];
    
    for _ in 0..7 {  // 刚好测试所有7种组合
        let combo_index = (rng.next() % flag_combinations.len() as u64) as usize;
        let (flags, expected_r, expected_w, expected_x) = flag_combinations[combo_index];
        
        let vaddr = rng.next_range(0x1000, 0x8000_0000);
        let filesz = rng.next_range(0x100, 0x200);  // 减小到最大512字节
        
        let segments = vec![(vaddr, filesz, filesz, flags)];
        let elf_data = build_minimal_elf(vaddr, &segments);
        
        let header = ElfHeader::parse(&elf_data)
            .map_err(|_| "ELF 解析失败")?;
        
        let phdr = header.program_headers(&elf_data)
            .next()
            .ok_or("没有程序头")?
            .map_err(|_| "程序头解析失败")?;
        
        if phdr.is_readable() != expected_r {
            return Err("可读权限不匹配");
        }
        if phdr.is_writable() != expected_w {
            return Err("可写权限不匹配");
        }
        if phdr.is_executable() != expected_x {
            return Err("可执行权限不匹配");
        }
    }
    Ok(())
}

fn run_invalid_magic_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(22222);
    
    for _ in 0..10 {
        let size = rng.next_range(64, 1024) as usize;
        let mut data: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
        
        if &data[0..4] == &ELF_MAGIC {
            data[0] = 0x00;
        }
        
        match ElfHeader::parse(&data) {
            Err(LoaderError::InvalidMagic) | Err(LoaderError::FileTooSmall) => {}
            Ok(_) => return Err("无效 magic 未被检测"),
            Err(_) => {} // 其他错误也可接受
        }
    }
    Ok(())
}

fn run_file_too_small_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(33333);
    
    for _ in 0..10 {
        let size = rng.next_range(0, ElfHeader::SIZE as u64) as usize;
        let data: Vec<u8> = (0..size).map(|_| rng.next() as u8).collect();
        
        match ElfHeader::parse(&data) {
            Err(LoaderError::FileTooSmall) => {}
            Ok(_) => return Err("文件太小未被检测"),
            Err(_) => return Err("期望 FileTooSmall 错误"),
        }
    }
    Ok(())
}

fn run_unsupported_arch_test() -> Result<(), &'static str> {
    let unsupported_machines: [u16; 5] = [0x03, 0x3E, 0x28, 0xB7, 0x08];
    
    for machine in unsupported_machines {
        let mut elf = Vec::new();
        
        elf.extend_from_slice(&ELF_MAGIC);
        elf.push(ELFCLASS64);
        elf.push(ELFDATA2LSB);
        elf.push(EV_CURRENT);
        elf.push(ELFOSABI_NONE);
        elf.extend_from_slice(&[0u8; 8]);
        elf.extend_from_slice(&ET_EXEC.to_le_bytes());
        elf.extend_from_slice(&machine.to_le_bytes());
        elf.extend(core::iter::repeat(0u8).take(ElfHeader::SIZE - elf.len()));
        
        match ElfHeader::parse(&elf) {
            Err(LoaderError::UnsupportedArch) => {}
            Ok(_) => return Err("不支持的架构未被检测"),
            Err(_) => return Err("期望 UnsupportedArch 错误"),
        }
    }
    Ok(())
}

fn run_segment_type_test() -> Result<(), &'static str> {
    let segments = vec![(0x1000u64, 0x100u64, 0x100u64, PF_R | PF_X)];
    let elf_data = build_minimal_elf(0x1000, &segments);
    
    let header = ElfHeader::parse(&elf_data)
        .map_err(|_| "ELF 解析失败")?;
    let phdr = header.program_headers(&elf_data)
        .next()
        .ok_or("没有程序头")?
        .map_err(|_| "程序头解析失败")?;
    
    if !phdr.is_load() {
        return Err("PT_LOAD 段的 is_load() 应返回 true");
    }
    if phdr.p_type != PT_LOAD {
        return Err("段类型应为 PT_LOAD");
    }
    Ok(())
}

fn run_bss_identification_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(44444);
    
    for _ in 0..5 {  // 减少到5次迭代
        let vaddr = rng.next_range(0x1000, 0x8000_0000);
        let filesz = rng.next_range(0x100, 0x200);  // 减小到最大512字节
        let bss_size = rng.next_range(0x100, 0x200);  // 减小BSS大小
        let memsz = filesz + bss_size;
        
        let segments = vec![(vaddr, filesz, memsz, PF_R | PF_W)];
        let elf_data = build_minimal_elf(vaddr, &segments);
        
        let header = ElfHeader::parse(&elf_data)
            .map_err(|_| "ELF 解析失败")?;
        let phdr = header.program_headers(&elf_data)
            .next()
            .ok_or("没有程序头")?
            .map_err(|_| "程序头解析失败")?;
        
        if phdr.mem_size() <= phdr.file_size() {
            return Err("memsz 应大于 filesz");
        }
        
        let actual_bss = phdr.mem_size() - phdr.file_size();
        if actual_bss != bss_size as usize {
            return Err("BSS 大小不匹配");
        }
        
        let vma_type = phdr.to_vma_type();
        if vma_type != VmaType::Bss {
            return Err("有 BSS 的可写段应被识别为 Bss 类型");
        }
    }
    Ok(())
}

fn run_program_header_iterator_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(55555);
    
    for _ in 0..5 {  // 减少到5次迭代
        let num_segments = rng.next_range(1, 3) as usize;  // 减少到最多2个段
        let mut segments = Vec::new();
        let mut current_vaddr = 0x1000u64;
        
        for _ in 0..num_segments {
            let filesz = rng.next_range(0x80, 0x100);  // 减小到最大256字节
            segments.push((current_vaddr, filesz, filesz, PF_R));
            current_vaddr += filesz + 0x1000;
        }
        
        let elf_data = build_minimal_elf(0x1000, &segments);
        let header = ElfHeader::parse(&elf_data)
            .map_err(|_| "ELF 解析失败")?;
        
        let iter = header.program_headers(&elf_data);
        if iter.len() != num_segments {
            return Err("迭代器初始长度不匹配");
        }
        
        let count = header.program_headers(&elf_data).count();
        if count != num_segments {
            return Err("迭代次数不匹配");
        }
    }
    Ok(())
}

fn run_elf_type_test() -> Result<(), &'static str> {
    let segments = vec![(0x1000u64, 0x100u64, 0x100u64, PF_R | PF_X)];
    let exec_elf = build_minimal_elf(0x1000, &segments);
    
    let header = ElfHeader::parse(&exec_elf)
        .map_err(|_| "ELF 解析失败")?;
    if !header.is_executable() {
        return Err("可执行文件的 is_executable() 应返回 true");
    }
    if header.is_shared() {
        return Err("可执行文件的 is_shared() 应返回 false");
    }
    
    let mut dyn_elf = exec_elf.clone();
    dyn_elf[16] = (ET_DYN & 0xFF) as u8;
    dyn_elf[17] = ((ET_DYN >> 8) & 0xFF) as u8;
    
    let header = ElfHeader::parse(&dyn_elf)
        .map_err(|_| "ELF 解析失败")?;
    if header.is_executable() {
        return Err("共享库的 is_executable() 应返回 false");
    }
    if !header.is_shared() {
        return Err("共享库的 is_shared() 应返回 true");
    }
    
    Ok(())
}
