//! ELF 文件格式解析和加载
//!
//! 实现 ELF64 格式的解析，专门针对 RISC-V 64 位架构。
//!
//! # ELF 文件结构
//! ```text
//! +------------------+
//! |    ELF Header    |  <- 文件头，包含基本信息
//! +------------------+
//! | Program Headers  |  <- 程序头表，描述段信息
//! +------------------+
//! |    Segment 1     |  <- 可加载段（代码、数据等）
//! +------------------+
//! |    Segment 2     |
//! +------------------+
//! |       ...        |
//! +------------------+
//! | Section Headers  |  <- 节头表（可选，用于调试）
//! +------------------+
//! ```

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use crate::mm::{
    AddressSpace, FrameTracker, VirtAddr, VmaType, VmaPermission, PAGE_SIZE,
};
use crate::sync::SpinLock;
use crate::error::{KernelError, MemoryError};
use crate::fs::File;

// ============================================================================
// exec 只读段（text/rodata）全局帧缓存（第 15 批）
// ============================================================================
//
// 不可变内容的只读段首次加载后，把承载文件数据的物理帧按 Arc 缓存起来；
// 后续再 exec 同一文件时，只读段直接 map_region_shared_frames 共享映射——
// 免读文件、免整页 memcpy、免清零，直接命中已就绪帧。
//
// key = (ino, size, mtime, ctime, seg_vaddr)：四元文件身份 + 段虚址。
// 文件一旦被写（ext4 会刷新 mtime/ctime、大小变化亦然）key 即变，旧帧自然
// 失效、绝不误命中；映射时 VMA 标记 cow=true，mprotect 加写后写入触发 COW，
// 不会污染全局缓存帧。仅对**非可写段**（write==false）且 **bias==0**（非 PIE，
// 无重定位改写段内容）启用。总页数超上限时按 FIFO 淘汰最早条目。

/// (ino, size, mtime, ctime, seg_vaddr)
type SegCacheKey = (u64, u64, u64, u64, usize);

struct SegCacheEntry {
    key: SegCacheKey,
    frames: Vec<crate::mm::SharedFrame>,
}

struct SegCache {
    entries: Vec<SegCacheEntry>,
    pages: usize,
}

/// 缓存总页数上限（4KiB/页 → 约 8MiB 常驻），超限 FIFO 淘汰。
const SEG_CACHE_MAX_PAGES: usize = 2048;

impl SegCache {
    const fn new() -> Self {
        Self { entries: Vec::new(), pages: 0 }
    }

    /// clone = 每帧 MetaSlot +1：缓存与本次 exec 的 VMA 各自持有。
    fn get(&self, key: &SegCacheKey) -> Option<Vec<crate::mm::SharedFrame>> {
        self.entries
            .iter()
            .find(|e| &e.key == key)
            .map(|e| e.frames.clone())
    }

    fn insert(&mut self, key: SegCacheKey, frames: Vec<crate::mm::SharedFrame>) {
        let n = frames.len();
        if n == 0 || n > SEG_CACHE_MAX_PAGES {
            return;
        }
        if self.entries.iter().any(|e| e.key == key) {
            return;
        }
        while self.pages + n > SEG_CACHE_MAX_PAGES && !self.entries.is_empty() {
            let ev = self.entries.remove(0);
            self.pages -= ev.frames.len();
        }
        self.pages += n;
        self.entries.push(SegCacheEntry { key, frames });
    }
}

static SEG_CACHE: SpinLock<SegCache> = SpinLock::new(SegCache::new());

// ============================================================================
// ELF 常量定义
// ============================================================================

/// ELF Magic Number: 0x7F 'E' 'L' 'F'
pub const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// ELF 类型：64 位
pub const ELFCLASS64: u8 = 2;

/// 数据编码：小端序
pub const ELFDATA2LSB: u8 = 1;

/// ELF 版本
pub const EV_CURRENT: u8 = 1;

/// OS/ABI: UNIX System V
pub const ELFOSABI_NONE: u8 = 0;

// ELF 文件类型
/// 无类型
pub const ET_NONE: u16 = 0;
/// 可重定位文件
pub const ET_REL: u16 = 1;
/// 可执行文件
pub const ET_EXEC: u16 = 2;
/// 共享目标文件
pub const ET_DYN: u16 = 3;
/// Core 文件
pub const ET_CORE: u16 = 4;

// 目标架构
/// RISC-V
pub const EM_RISCV: u16 = 243;
/// LoongArch
pub const EM_LOONGARCH: u16 = 258;

// 程序头类型
/// 空段
pub const PT_NULL: u32 = 0;
/// 可加载段
pub const PT_LOAD: u32 = 1;
/// 动态链接信息
pub const PT_DYNAMIC: u32 = 2;
/// 解释器路径
pub const PT_INTERP: u32 = 3;
/// 辅助信息
pub const PT_NOTE: u32 = 4;
/// 保留
pub const PT_SHLIB: u32 = 5;
/// 程序头表
pub const PT_PHDR: u32 = 6;
/// 线程本地存储
pub const PT_TLS: u32 = 7;

// 段标志
/// 可执行
pub const PF_X: u32 = 0x1;
/// 可写
pub const PF_W: u32 = 0x2;
/// 可读
pub const PF_R: u32 = 0x4;

// 动态段标签 (d_tag)
const DT_RELA: u64 = 7;
const DT_RELASZ: u64 = 8;
const DT_RELAENT: u64 = 9;

/// R_RISCV_RELATIVE 重定位类型
const R_RISCV_RELATIVE: u32 = 3;

/// PIE 默认加载基址
const PIE_LOAD_BASE: usize = 0x4000_0000;

/// 动态链接器（interpreter）加载基址
const INTERP_LOAD_BASE: usize = 0x2000_0000;

// ============================================================================
// ELF 数据结构
// ============================================================================

/// ELF64 文件头
///
/// 位于 ELF 文件的开头，包含文件的基本信息。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ElfHeader {
    /// ELF 标识（Magic + Class + Data + Version + OS/ABI + Padding）
    pub e_ident: [u8; 16],
    /// 文件类型（ET_EXEC, ET_DYN 等）
    pub e_type: u16,
    /// 目标架构（EM_RISCV）
    pub e_machine: u16,
    /// ELF 版本
    pub e_version: u32,
    /// 入口点虚拟地址
    pub e_entry: u64,
    /// 程序头表在文件中的偏移
    pub e_phoff: u64,
    /// 节头表在文件中的偏移
    pub e_shoff: u64,
    /// 处理器特定标志
    pub e_flags: u32,
    /// ELF 头大小
    pub e_ehsize: u16,
    /// 程序头表项大小
    pub e_phentsize: u16,
    /// 程序头表项数量
    pub e_phnum: u16,
    /// 节头表项大小
    pub e_shentsize: u16,
    /// 节头表项数量
    pub e_shnum: u16,
    /// 节名字符串表索引
    pub e_shstrndx: u16,
}

impl ElfHeader {
    /// ELF 头大小
    pub const SIZE: usize = 64;

    /// 从字节数组解析 ELF 头
    ///
    /// # 参数
    /// - `data`: ELF 文件数据
    ///
    /// # 返回
    /// - `Ok(ElfHeader)`: 解析成功
    /// - `Err(LoaderError)`: 解析失败
    pub fn parse(data: &[u8]) -> Result<Self, LoaderError> {
        // 检查数据长度
        if data.len() < Self::SIZE {
            return Err(LoaderError::FileTooSmall);
        }

        // 验证 Magic Number
        if &data[0..4] != &ELF_MAGIC {
            return Err(LoaderError::InvalidMagic);
        }

        // 验证 ELF 类型（必须是 64 位）
        if data[4] != ELFCLASS64 {
            return Err(LoaderError::UnsupportedClass);
        }

        // 验证字节序（必须是小端序）
        if data[5] != ELFDATA2LSB {
            return Err(LoaderError::UnsupportedEndian);
        }

        // 验证 ELF 版本
        if data[6] != EV_CURRENT {
            return Err(LoaderError::UnsupportedVersion);
        }

        // 安全地解析头部
        let header = unsafe {
            core::ptr::read_unaligned(data.as_ptr() as *const ElfHeader)
        };

        // 验证目标架构
        #[cfg(target_arch = "riscv64")]
        if header.e_machine != EM_RISCV {
            return Err(LoaderError::UnsupportedArch);
        }
        #[cfg(target_arch = "loongarch64")]
        if header.e_machine != EM_LOONGARCH {
            return Err(LoaderError::UnsupportedArch);
        }

        // 验证文件类型
        if header.e_type != ET_EXEC && header.e_type != ET_DYN {
            return Err(LoaderError::UnsupportedType);
        }

        // 验证程序头表项大小
        if header.e_phentsize as usize != ProgramHeader::SIZE {
            return Err(LoaderError::InvalidProgramHeaderSize);
        }

        Ok(header)
    }

    /// 获取程序头表迭代器
    ///
    /// # 参数
    /// - `data`: ELF 文件数据
    ///
    /// # 返回
    /// 程序头迭代器
    pub fn program_headers<'a>(&self, data: &'a [u8]) -> ProgramHeaderIter<'a> {
        ProgramHeaderIter {
            data,
            offset: self.e_phoff as usize,
            count: self.e_phnum as usize,
            index: 0,
        }
    }

    /// 获取入口点地址
    pub fn entry(&self) -> usize {
        self.e_entry as usize
    }

    /// 检查是否为可执行文件
    pub fn is_executable(&self) -> bool {
        self.e_type == ET_EXEC
    }

    /// 检查是否为共享库/PIE
    pub fn is_shared(&self) -> bool {
        self.e_type == ET_DYN
    }
}

/// ELF64 程序头
///
/// 描述一个程序段的信息，用于加载到内存。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ProgramHeader {
    /// 段类型（PT_LOAD, PT_DYNAMIC 等）
    pub p_type: u32,
    /// 段标志（PF_R, PF_W, PF_X）
    pub p_flags: u32,
    /// 段在文件中的偏移
    pub p_offset: u64,
    /// 段的虚拟地址
    pub p_vaddr: u64,
    /// 段的物理地址（通常与虚拟地址相同）
    pub p_paddr: u64,
    /// 段在文件中的大小
    pub p_filesz: u64,
    /// 段在内存中的大小
    pub p_memsz: u64,
    /// 段对齐要求
    pub p_align: u64,
}

impl ProgramHeader {
    /// 程序头大小
    pub const SIZE: usize = 56;

    /// 从字节数组解析程序头
    pub fn parse(data: &[u8], offset: usize) -> Result<Self, LoaderError> {
        if offset + Self::SIZE > data.len() {
            return Err(LoaderError::InvalidProgramHeader);
        }

        let header = unsafe {
            core::ptr::read_unaligned(data[offset..].as_ptr() as *const ProgramHeader)
        };

        Ok(header)
    }

    /// 检查是否为可加载段
    pub fn is_load(&self) -> bool {
        self.p_type == PT_LOAD
    }

    /// 获取段的虚拟地址
    pub fn vaddr(&self) -> usize {
        self.p_vaddr as usize
    }

    /// 获取段在文件中的大小
    pub fn file_size(&self) -> usize {
        self.p_filesz as usize
    }

    /// 获取段在内存中的大小
    pub fn mem_size(&self) -> usize {
        self.p_memsz as usize
    }

    /// 获取段在文件中的偏移
    pub fn offset(&self) -> usize {
        self.p_offset as usize
    }

    /// 检查是否可读
    pub fn is_readable(&self) -> bool {
        self.p_flags & PF_R != 0
    }

    /// 检查是否可写
    pub fn is_writable(&self) -> bool {
        self.p_flags & PF_W != 0
    }

    /// 检查是否可执行
    pub fn is_executable(&self) -> bool {
        self.p_flags & PF_X != 0
    }

    /// 转换为 VMA 权限
    pub fn to_vma_permission(&self) -> VmaPermission {
        VmaPermission {
            read: self.is_readable(),
            write: self.is_writable(),
            execute: self.is_executable(),
            user: true, // 用户态程序
        }
    }

    /// 推断 VMA 类型
    pub fn to_vma_type(&self) -> VmaType {
        if self.is_executable() {
            VmaType::Code
        } else if self.is_writable() {
            if self.p_filesz < self.p_memsz {
                VmaType::Bss
            } else {
                VmaType::Data
            }
        } else {
            VmaType::Data
        }
    }
}

/// 程序头迭代器
pub struct ProgramHeaderIter<'a> {
    data: &'a [u8],
    offset: usize,
    count: usize,
    index: usize,
}

impl<'a> Iterator for ProgramHeaderIter<'a> {
    type Item = Result<ProgramHeader, LoaderError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index >= self.count {
            return None;
        }

        let current_offset = self.offset + self.index * ProgramHeader::SIZE;
        self.index += 1;

        Some(ProgramHeader::parse(self.data, current_offset))
    }
}

impl<'a> ExactSizeIterator for ProgramHeaderIter<'a> {
    fn len(&self) -> usize {
        self.count - self.index
    }
}

// ============================================================================
// 重定位结构
// ============================================================================

/// ELF64 Rela 重定位条目
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Elf64Rela {
    r_offset: u64,
    r_info: u64,
    r_addend: i64,
}

impl Elf64Rela {
    const SIZE: usize = 24;

    fn r_type(&self) -> u32 {
        (self.r_info & 0xffff_ffff) as u32
    }
}

/// ELF64 Dynamic 段条目
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Elf64Dyn {
    d_tag: u64,
    d_val: u64,
}

impl Elf64Dyn {
    const SIZE: usize = 16;
}

// ============================================================================
// ELF 加载器
// ============================================================================

/// ELF 加载信息
///
/// 加载 ELF 文件后返回的信息，用于设置进程状态。
#[derive(Debug, Clone)]
pub struct ElfInfo {
    /// 程序入口点地址
    pub entry: usize,
    /// 程序最低虚拟地址
    pub base: usize,
    /// 程序最高虚拟地址
    pub end: usize,
    /// 建议的栈顶地址
    pub stack_top: usize,
    /// 建议的堆起始地址
    pub heap_start: usize,
    /// 程序 break 地址（堆的当前结束位置）
    pub program_break: usize,
    /// 是否为 PIE（位置无关可执行文件）
    pub is_pie: bool,
    /// 程序头表在内存中的虚拟地址（AT_PHDR）
    pub phdr_vaddr: usize,
    /// 程序头表项大小（AT_PHENT，通常 56）
    pub phent_size: usize,
    /// 程序头表项数量（AT_PHNUM）
    pub phnum: usize,
    /// 解释器路径（PT_INTERP）
    pub interp_path: Option<String>,
    /// 解释器加载基址（AT_BASE）
    pub interp_base: usize,
    /// 应用程序原始入口点（AT_ENTRY，用于动态加载时）
    pub app_entry: usize,
}

impl ElfInfo {
    /// 默认用户栈大小（8MB）
    pub const DEFAULT_STACK_SIZE: usize = 8 * 1024 * 1024;
    /// 默认用户栈顶地址
    /// LoongArch64: 用户程序链接在 0x120000000 附近，栈需要在更高地址
    #[cfg(target_arch = "loongarch64")]
    pub const DEFAULT_STACK_TOP: usize = 0x7F_FFFE_0000;
    #[cfg(not(target_arch = "loongarch64"))]
    pub const DEFAULT_STACK_TOP: usize = 0x8000_0000;
}

/// ELF 加载器
///
/// 负责解析 ELF 文件并将其加载到进程地址空间。
pub struct ElfLoader;

impl ElfLoader {
    /// brk 起点与 bss 末尾之间的固定间隙（Linux `arch_randomize_brk` 会把
    /// brk 基址随机上移 ≤32MB；这里取确定性的 16MB，保证 libc 启动期按
    /// bss 末尾 hint 的匿名映射不会压住 brk 区）。
    pub const BRK_GAP: usize = 16 * 1024 * 1024;

    /// 加载 ELF 文件到地址空间
    ///
    /// # 参数
    /// - `elf_data`: ELF 文件的原始字节数据
    /// - `address_space`: 目标地址空间
    ///
    /// # 返回
    /// - `Ok(ElfInfo)`: 加载成功，返回加载信息
    /// - `Err(LoaderError)`: 加载失败
    ///
    /// # 加载过程
    /// 1. 解析 ELF 头，验证格式
    /// 2. 遍历程序头表，找到 PT_LOAD 段
    /// 3. 为每个 PT_LOAD 段分配内存并映射
    /// 4. 复制段数据到内存
    /// 5. 清零 BSS 段（memsz > filesz 的部分）
    pub fn load(
        elf_data: &[u8],
        address_space: &mut AddressSpace,
    ) -> Result<ElfInfo, LoaderError> {
        // 1. 解析 ELF 头
        let header = ElfHeader::parse(elf_data)?;

        // PIE (ET_DYN) 需要加载偏移，ET_EXEC 偏移为 0
        let bias = if header.is_shared() { PIE_LOAD_BASE } else { 0 };

        // 2. 计算程序的地址范围（加上 bias）
        let mut base = usize::MAX;
        let mut end = 0usize;

        // 首先遍历一次，计算地址范围
        for phdr_result in header.program_headers(elf_data) {
            let phdr = phdr_result?;
            if phdr.is_load() {
                let seg_start = phdr.vaddr() + bias;
                let seg_end = seg_start + phdr.mem_size();
                base = base.min(seg_start);
                end = end.max(seg_end);
            }
        }

        if base == usize::MAX {
            return Err(LoaderError::NoLoadableSegments);
        }

        // 3. 查找 PT_PHDR 和 PT_DYNAMIC，同时加载 PT_LOAD 段
        let mut phdr_vaddr: Option<usize> = None;
        let mut dynamic_offset: Option<usize> = None;
        let mut dynamic_size: Option<usize> = None;
        let mut interp_path: Option<String> = None;
        for phdr_result in header.program_headers(elf_data) {
            let phdr = phdr_result?;
            if phdr.p_type == PT_PHDR {
                phdr_vaddr = Some(phdr.vaddr() + bias);
            }
            if phdr.p_type == PT_DYNAMIC {
                dynamic_offset = Some(phdr.offset());
                dynamic_size = Some(phdr.file_size());
            }
            if phdr.p_type == PT_INTERP {
                let off = phdr.offset();
                let sz = phdr.file_size();
                if off + sz <= elf_data.len() && sz > 0 {
                    let interp_bytes = &elf_data[off..off + sz];
                    let end_pos = interp_bytes.iter().position(|&b| b == 0).unwrap_or(interp_bytes.len());
                    if let Ok(s) = core::str::from_utf8(&interp_bytes[..end_pos]) {
                        interp_path = Some(String::from(s));
                    }
                }
            }
            if phdr.is_load() {
                Self::load_segment(elf_data, &phdr, address_space, bias)?;
            }
        }

        // 如果没有 PT_PHDR 段，通过第一个 PT_LOAD 段推算
        let phdr_vaddr = phdr_vaddr.unwrap_or(base + header.e_phoff as usize);

        // 4. 处理 PIE 重定位
        if bias != 0 {
            if let (Some(dyn_off), Some(dyn_sz)) = (dynamic_offset, dynamic_size) {
                Self::process_relocations(elf_data, &header, dyn_off, dyn_sz, bias, address_space)?;
            }
        }

        // 5. 计算堆和栈的位置。brk 起点在 bss 之上留一段间隙（Linux
        // arch_randomize_brk 语义：brk 基址随机上移最多 32MB），避免
        // libc 启动期按 bss 末尾 hint 的匿名映射（PROT_NONE 守护页等）
        // 恰好压住 brk 区导致后续 brk 扩展 ENOMEM（brk01/brk02）。
        let heap_start = ((end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)) + Self::BRK_GAP;
        let program_break = heap_start;
        let stack_top = ElfInfo::DEFAULT_STACK_TOP;

        let entry = header.entry() + bias;
        Ok(ElfInfo {
            entry,
            base,
            end,
            stack_top,
            heap_start,
            program_break,
            is_pie: header.is_shared(),
            phdr_vaddr,
            phent_size: header.e_phentsize as usize,
            phnum: header.e_phnum as usize,
            interp_path,
            interp_base: 0,
            app_entry: entry,
        })
    }

    /// 加载单个程序段
    fn load_segment(
        elf_data: &[u8],
        phdr: &ProgramHeader,
        address_space: &mut AddressSpace,
        bias: usize,
    ) -> Result<(), LoaderError> {
        let vaddr = phdr.vaddr() + bias;
        let filesz = phdr.file_size();
        let memsz = phdr.mem_size();
        let offset = phdr.offset();
        
        // 验证文件偏移和大小
        if offset + filesz > elf_data.len() {
            return Err(LoaderError::InvalidSegmentOffset);
        }

        // 计算页对齐的地址范围
        let start_va = VirtAddr(vaddr & !(PAGE_SIZE - 1));
        let end_va = VirtAddr((vaddr + memsz + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));

        // 获取权限和类型
        let permission = phdr.to_vma_permission();
        let vma_type = phdr.to_vma_type();

        // 映射内存区域
        address_space.map_region(start_va, end_va, vma_type, permission);

        // 复制段数据
        // 注意：这里需要通过页表写入数据
        // 由于我们的 AddressSpace 实现会自动分配物理页，
        // 我们需要通过页表来写入数据
        let page_table = address_space.page_table_mut();
        
        // 获取源数据
        let src_data = &elf_data[offset..offset + filesz];

        // 逐页复制数据
        let mut copied = 0;
        let mut current_va = vaddr;

        while copied < filesz {
            let vpn = VirtAddr(current_va).floor();
            let page_off = current_va & (PAGE_SIZE - 1);
            let copy_len = (PAGE_SIZE - page_off).min(filesz - copied);

            // 获取物理页
            if let Some(ppn) = page_table.translate(vpn) {
                let dst = ppn.as_bytes_mut();
                dst[page_off..page_off + copy_len]
                    .copy_from_slice(&src_data[copied..copied + copy_len]);
            } else {
                return Err(LoaderError::MappingFailed);
            }

            copied += copy_len;
            current_va += copy_len;
        }

        // BSS 段（memsz > filesz）无需在此显式清零：map_region 已 eager 映射
        // [start_va, align_up(vaddr+memsz)) 全段，每页经 frame_alloc 的 zero_frame
        // 快速清零；filesz 末页尾部同理已是零。原先这里对整段 BSS 逐字节清零是
        // 冗余的二次清零（大 BSS 二进制 exec 的主要指令开销之一）。

        Ok(())
    }

    /// 处理 PIE 重定位（.rela.dyn）
    ///
    /// 从 PT_DYNAMIC 段解析 DT_RELA/DT_RELASZ/DT_RELAENT，
    /// 对所有 R_RISCV_RELATIVE 条目执行：`*(bias + offset) = bias + addend`
    fn process_relocations(
        elf_data: &[u8],
        header: &ElfHeader,
        dyn_offset: usize,
        dyn_size: usize,
        bias: usize,
        address_space: &mut AddressSpace,
    ) -> Result<(), LoaderError> {
        // 解析 PT_DYNAMIC 段，提取 DT_RELA 信息
        let mut rela_vaddr: Option<u64> = None;
        let mut rela_size: Option<u64> = None;
        let mut rela_ent: Option<u64> = None;

        let dyn_end = dyn_offset + dyn_size;
        let mut off = dyn_offset;
        while off + Elf64Dyn::SIZE <= dyn_end && off + Elf64Dyn::SIZE <= elf_data.len() {
            let dyn_entry = unsafe {
                core::ptr::read_unaligned(elf_data[off..].as_ptr() as *const Elf64Dyn)
            };
            match dyn_entry.d_tag {
                0 => break, // DT_NULL
                DT_RELA => rela_vaddr = Some(dyn_entry.d_val),
                DT_RELASZ => rela_size = Some(dyn_entry.d_val),
                DT_RELAENT => rela_ent = Some(dyn_entry.d_val),
                _ => {}
            }
            off += Elf64Dyn::SIZE;
        }

        let rela_va = match rela_vaddr {
            Some(v) => v as usize,
            None => return Ok(()), // 无重定位表
        };
        let rela_total = rela_size.unwrap_or(0) as usize;
        let rela_entry_size = rela_ent.unwrap_or(Elf64Rela::SIZE as u64) as usize;

        if rela_total == 0 || rela_entry_size == 0 {
            return Ok(());
        }

        // 将 rela 虚拟地址转换为文件偏移
        let rela_file_offset = Self::vaddr_to_file_offset(elf_data, header, rela_va)?;

        let page_table = address_space.page_table_mut();
        let mut off = rela_file_offset;
        let rela_end = rela_file_offset + rela_total;

        while off + rela_entry_size <= rela_end && off + Elf64Rela::SIZE <= elf_data.len() {
            let rela = unsafe {
                core::ptr::read_unaligned(elf_data[off..].as_ptr() as *const Elf64Rela)
            };

            if rela.r_type() == R_RISCV_RELATIVE {
                let target_va = bias + rela.r_offset as usize;
                let value = (bias as i64 + rela.r_addend) as u64;

                let vpn = VirtAddr(target_va & !(PAGE_SIZE - 1)).floor();
                let page_off = target_va & (PAGE_SIZE - 1);

                if let Some(ppn) = page_table.translate(vpn) {
                    let dst = ppn.as_bytes_mut();
                    if page_off + 8 <= PAGE_SIZE {
                        dst[page_off..page_off + 8].copy_from_slice(&value.to_le_bytes());
                    } else {
                        // 跨页写入
                        let first_part = PAGE_SIZE - page_off;
                        let bytes = value.to_le_bytes();
                        dst[page_off..].copy_from_slice(&bytes[..first_part]);
                        let next_vpn = VirtAddr(target_va + first_part).floor();
                        if let Some(next_ppn) = page_table.translate(next_vpn) {
                            let next_dst = next_ppn.as_bytes_mut();
                            next_dst[..8 - first_part].copy_from_slice(&bytes[first_part..]);
                        }
                    }
                }
            }
            off += rela_entry_size;
        }

        Ok(())
    }

    /// 将 ELF 虚拟地址转换为文件偏移
    ///
    /// 遍历 PT_LOAD 段，找到包含 vaddr 的段并计算文件偏移
    fn vaddr_to_file_offset(
        elf_data: &[u8],
        header: &ElfHeader,
        vaddr: usize,
    ) -> Result<usize, LoaderError> {
        for phdr_result in header.program_headers(elf_data) {
            let phdr = phdr_result?;
            if phdr.is_load() {
                let seg_start = phdr.vaddr();
                let seg_end = seg_start + phdr.file_size();
                if vaddr >= seg_start && vaddr < seg_end {
                    return Ok(phdr.offset() + (vaddr - seg_start));
                }
            }
        }
        // 如果没找到匹配的 LOAD 段，尝试直接用 vaddr 作为 offset（适用于 vaddr=file_offset 的情况）
        if vaddr < elf_data.len() {
            Ok(vaddr)
        } else {
            Err(LoaderError::InvalidSegmentOffset)
        }
    }

    /// 验证 ELF 文件格式（不加载）
    ///
    /// 用于快速检查文件是否为有效的 ELF 文件。
    pub fn validate(elf_data: &[u8]) -> Result<ElfHeader, LoaderError> {
        ElfHeader::parse(elf_data)
    }

    /// 获取 ELF 文件的入口点（不加载）
    pub fn get_entry(elf_data: &[u8]) -> Result<usize, LoaderError> {
        let header = ElfHeader::parse(elf_data)?;
        Ok(header.entry())
    }

    fn read_exact_from_file(
        file: &dyn File,
        offset: usize,
        buf: &mut [u8],
    ) -> Result<(), LoaderError> {
        if buf.is_empty() {
            return Ok(());
        }

        let mut total_read = 0usize;
        while total_read < buf.len() {
            let bytes_read = file
                .read(offset + total_read, &mut buf[total_read..])
                .map_err(|_| LoaderError::IoError)?;
            if bytes_read == 0 {
                return Err(LoaderError::InvalidSegmentOffset);
            }
            total_read += bytes_read;
        }

        Ok(())
    }

    fn read_program_headers_from_file(
        file: &dyn File,
        file_size: usize,
        header: &ElfHeader,
    ) -> Result<Vec<ProgramHeader>, LoaderError> {
        let phoff = header.e_phoff as usize;
        let phentsize = header.e_phentsize as usize;
        let phnum = header.e_phnum as usize;
        let total = phentsize
            .checked_mul(phnum)
            .ok_or(LoaderError::InvalidProgramHeader)?;
        let end = phoff
            .checked_add(total)
            .ok_or(LoaderError::InvalidProgramHeader)?;

        if end > file_size {
            return Err(LoaderError::InvalidProgramHeader);
        }

        let mut buf = Vec::new();
        buf.resize(total, 0u8);
        Self::read_exact_from_file(file, phoff, &mut buf)?;

        let mut phdrs = Vec::with_capacity(phnum);
        for idx in 0..phnum {
            phdrs.push(ProgramHeader::parse(&buf, idx * phentsize)?);
        }
        Ok(phdrs)
    }

    fn load_segment_from_file(
        file: &dyn File,
        file_size: usize,
        phdr: &ProgramHeader,
        address_space: &mut AddressSpace,
        bias: usize,
        cache_id: Option<(u64, u64, u64, u64)>,
    ) -> Result<(), LoaderError> {
        let vaddr = phdr.vaddr() + bias;
        let filesz = phdr.file_size();
        let memsz = phdr.mem_size();
        let offset = phdr.offset();
        let end = offset
            .checked_add(filesz)
            .ok_or(LoaderError::InvalidSegmentOffset)?;

        if end > file_size {
            return Err(LoaderError::InvalidSegmentOffset);
        }

        let start_va = VirtAddr(vaddr & !(PAGE_SIZE - 1));
        let end_va = VirtAddr((vaddr + memsz + PAGE_SIZE - 1) & !(PAGE_SIZE - 1));
        let permission = phdr.to_vma_permission();
        let vma_type = phdr.to_vma_type();
        let npages = (end_va.0 - start_va.0) / PAGE_SIZE;

        // 只读段（含数据）在 bias==0 时可参与全局帧缓存：key 加上段虚址。
        let seg_key: Option<SegCacheKey> = if !permission.write {
            cache_id.map(|(ino, size, mtime, ctime)| (ino, size, mtime, ctime, phdr.vaddr()))
        } else {
            None
        };

        // 命中：直接用缓存的共享帧建立只读映射，跳过分配/读文件/清零。
        // 先取出帧克隆并立刻释放缓存锁，再做映射（映射期间不持锁）。
        if let Some(key) = seg_key {
            let cached = SEG_CACHE.lock().get(&key);
            if let Some(frames) = cached {
                if frames.len() == npages
                    && address_space.map_region_shared_frames(
                        start_va, end_va, vma_type, permission, &frames,
                    )
                {
                    return Ok(());
                }
                // 页数不符或映射失败（理应不发生）→ 落回下方普通装载路径。
            }
        }

        address_space.map_region(start_va, end_va, vma_type, permission);

        let page_table = address_space.page_table_mut();
        let mut copied = 0usize;
        let mut current_va = vaddr;

        while copied < filesz {
            let vpn = VirtAddr(current_va).floor();
            let page_off = current_va & (PAGE_SIZE - 1);
            let copy_len = (PAGE_SIZE - page_off).min(filesz - copied);

            // 直接读进目标物理帧，免去 chunk 中转缓冲的整页二次 memcpy。
            let Some(ppn) = page_table.translate(vpn) else {
                return Err(LoaderError::MappingFailed);
            };
            let dst_page = ppn.as_bytes_mut();
            let src = &mut dst_page[page_off..page_off + copy_len];

            Self::read_exact_from_file(file, offset + copied, src)?;

            // UAF 探针：刚读出的文件数据里若出现内核线性指针 [0x80200000,0x90000000)，
            // 说明缓存该 ELF 块的 page-cache 帧被内核写过（典型为被复用为内核栈后
            // 仍被 page-cache 索引 = page-cache UAF / 帧双重占用）。打印文件偏移 + 值，
            // 并扫描所有存活进程的内核栈区间，确认该值是否落在某条 live kstack 内、属谁。
            if crate::mm::uaf_dbg_enabled() {
                let buf_base = src.as_ptr() as usize;
                let words = copy_len / 8;
                let p = src.as_ptr() as *const u64;
                for i in 0..words {
                    let v = unsafe { core::ptr::read_unaligned(p.add(i)) } as usize;
                    // 判定标准（区分真污染 vs 合法 ELF 数据常量）：值落在 **活跃帧带**
                    // [0x8600_0000,0x8800_0000)。iperf 期间帧分配集中在 0x86xx_xxxx，
                    // 崩溃污染值（0x86aed800 / 0x8687d358 / 0x86dfd358）全在此带内；
                    // 合法 ELF 常量（0x9e00_0000 / 0xb504_f32d）> 0x8800_0000，被挡掉。
                    if v >= 0x8600_0000 && v < 0x8800_0000 {
                        let vppn = crate::mm::PhysPageNum(v >> 12);
                        let vref = crate::mm::get_frame_ref(vppn);
                        let buf_addr = buf_base + i * 8;
                        crate::println!(
                            "[ELFSRC] ###### REAL CORRUPTION ###### file_off={:#x} chunk_off={:#x} word={:#x} (points to LIVE frame ppn={:#x} ref={}) buf_addr={:#x}",
                            offset + copied + i * 8, i * 8, v, vppn.0, vref, buf_addr
                        );
                        // 装这块数据的 chunk 缓冲帧自身的归属。
                        crate::mm::frame_ownership_dump_dbg(buf_addr, "chunk");
                        // 该污染值指向的帧的归属（它是谁——活内核栈？slab？用户页？）。
                        crate::mm::frame_ownership_dump_dbg(v, "target");
                        if let Some(t) = crate::sched::process::PROCESS_TABLE.try_lock() {
                            if let Some(tbl) = t.as_ref() {
                                tbl.kstk_owner_scan_dbg(buf_addr, 0);
                                tbl.kstk_owner_scan_dbg(v, 0);
                            }
                        }
                        // PROV 环回放该污染值所指帧的完整易主链（alloc/free/slab/kstack）。
                        crate::mm::prov_dump_dbg(vppn.0);
                        // 同一 file_off 在多轮运行中重复出现同一常量，可能是合法
                        // ELF 数据；冻结会把“真构建”误判成挂死。告警后继续执行，
                        // 让后续真实的缺页/执行错误说话。
                        crate::println!("[ELFSRC] suspicious constant; continuing execution");
                        continue;
                    }
                }
            }

            copied += copy_len;
            current_va += copy_len;
        }

        if memsz > filesz {
            let bss_start = vaddr + filesz;
            let bss_end = vaddr + memsz;

            // 只需清零 bss 的首个"半页"（与文件数据共享的页，可能残留同页
            // 前一次写入）；其后的整页由 map_region → frame_alloc 分配时已全零，
            // 重复清零纯属浪费（exec 热路径）。
            let page_off = bss_start & (PAGE_SIZE - 1);
            if page_off != 0 {
                let vpn = VirtAddr(bss_start).floor();
                let zero_len = (PAGE_SIZE - page_off).min(bss_end - bss_start);
                if let Some(ppn) = page_table.translate(vpn) {
                    let dst = ppn.as_bytes_mut();
                    // LoongArch64: 避免 memset，逐字节清零
                    for i in page_off..page_off + zero_len {
                        dst[i] = 0;
                    }
                }
            }
        }

        // 回填：只读段首次装载完成，把整段物理帧登记进全局缓存供后续 exec 共享。
        // 仅在恰好映满 npages 页时回填，避免缓存到不完整/带洞的段。
        if let Some(key) = seg_key {
            if let Some(frames) = address_space.vma_frames_if_full(start_va, npages) {
                SEG_CACHE.lock().insert(key, frames);
            }
        }

        Ok(())
    }

    fn vaddr_to_file_offset_in_headers(
        phdrs: &[ProgramHeader],
        file_size: usize,
        vaddr: usize,
    ) -> Result<usize, LoaderError> {
        for phdr in phdrs {
            if phdr.is_load() {
                let seg_start = phdr.vaddr();
                let seg_end = seg_start + phdr.file_size();
                if vaddr >= seg_start && vaddr < seg_end {
                    return Ok(phdr.offset() + (vaddr - seg_start));
                }
            }
        }

        if vaddr < file_size {
            Ok(vaddr)
        } else {
            Err(LoaderError::InvalidSegmentOffset)
        }
    }

    /// 解析 PT_DYNAMIC 里的 DT_RELA 表位置，返回 (文件内偏移, 总字节, 每条字节)。
    fn rela_table_location(
        file: &dyn File,
        file_size: usize,
        phdrs: &[ProgramHeader],
        dyn_offset: usize,
        dyn_size: usize,
    ) -> Result<Option<(usize, usize, usize)>, LoaderError> {
        if dyn_size == 0 {
            return Ok(None);
        }

        let dyn_end = dyn_offset
            .checked_add(dyn_size)
            .ok_or(LoaderError::InvalidSegmentOffset)?;
        if dyn_end > file_size {
            return Err(LoaderError::InvalidSegmentOffset);
        }

        let mut dyn_data = Vec::new();
        dyn_data.resize(dyn_size, 0u8);
        Self::read_exact_from_file(file, dyn_offset, &mut dyn_data)?;

        let mut rela_vaddr: Option<u64> = None;
        let mut rela_size: Option<u64> = None;
        let mut rela_ent: Option<u64> = None;
        let mut off = 0usize;

        while off + Elf64Dyn::SIZE <= dyn_data.len() {
            let dyn_entry = unsafe {
                core::ptr::read_unaligned(dyn_data[off..].as_ptr() as *const Elf64Dyn)
            };
            match dyn_entry.d_tag {
                0 => break,
                DT_RELA => rela_vaddr = Some(dyn_entry.d_val),
                DT_RELASZ => rela_size = Some(dyn_entry.d_val),
                DT_RELAENT => rela_ent = Some(dyn_entry.d_val),
                _ => {}
            }
            off += Elf64Dyn::SIZE;
        }

        let rela_va = match rela_vaddr {
            Some(v) => v as usize,
            None => return Ok(None),
        };
        let rela_total = rela_size.unwrap_or(0) as usize;
        let rela_entry_size = rela_ent.unwrap_or(Elf64Rela::SIZE as u64) as usize;

        if rela_total == 0 || rela_entry_size == 0 {
            return Ok(None);
        }
        if rela_entry_size < Elf64Rela::SIZE {
            return Err(LoaderError::InvalidProgramHeader);
        }

        let rela_file_offset = Self::vaddr_to_file_offset_in_headers(phdrs, file_size, rela_va)?;
        let rela_end = rela_file_offset
            .checked_add(rela_total)
            .ok_or(LoaderError::InvalidSegmentOffset)?;
        if rela_end > file_size {
            return Err(LoaderError::InvalidSegmentOffset);
        }

        Ok(Some((rela_file_offset, rela_total, rela_entry_size)))
    }

    /// 预扫描 R_RISCV_RELATIVE 重定位目标（未加 bias 的 vaddr）区间 [lo, hi)。
    /// 用于判定只读段是否会被内核重定位写入（会则禁用共享帧缓存，
    /// 防止直写物理帧污染全局缓存）。无重定位时返回 None。
    fn rela_target_span(
        file: &dyn File,
        file_size: usize,
        phdrs: &[ProgramHeader],
        dyn_offset: usize,
        dyn_size: usize,
    ) -> Result<Option<(usize, usize)>, LoaderError> {
        let Some((rela_off, rela_total, rela_entry_size)) =
            Self::rela_table_location(file, file_size, phdrs, dyn_offset, dyn_size)?
        else {
            return Ok(None);
        };

        let mut table = Vec::new();
        table.resize(rela_total, 0u8);
        Self::read_exact_from_file(file, rela_off, &mut table)?;

        let mut lo = usize::MAX;
        let mut hi = 0usize;
        let mut off = 0usize;
        while off + rela_entry_size <= rela_total {
            let rela =
                unsafe { core::ptr::read_unaligned(table[off..].as_ptr() as *const Elf64Rela) };
            if rela.r_type() == R_RISCV_RELATIVE {
                let t = rela.r_offset as usize;
                lo = lo.min(t);
                hi = hi.max(t + 8);
            }
            off += rela_entry_size;
        }
        Ok(if lo < hi { Some((lo, hi)) } else { None })
    }

    fn process_relocations_from_file(
        file: &dyn File,
        file_size: usize,
        phdrs: &[ProgramHeader],
        dyn_offset: usize,
        dyn_size: usize,
        bias: usize,
        address_space: &mut AddressSpace,
    ) -> Result<(), LoaderError> {
        let Some((rela_file_offset, rela_total, rela_entry_size)) =
            Self::rela_table_location(file, file_size, phdrs, dyn_offset, dyn_size)?
        else {
            return Ok(());
        };

        let page_table = address_space.page_table_mut();
        // 一次性读入整张 rela 表，取代逐条 24B 文件读（每次 exec 都要走，
        // ld.so 动辄上千条，逐条读是数千次 VFS 往返）。
        let mut table = Vec::new();
        table.resize(rela_total, 0u8);
        Self::read_exact_from_file(file, rela_file_offset, &mut table)?;

        let mut off = 0usize;
        while off + rela_entry_size <= rela_total {
            let rela =
                unsafe { core::ptr::read_unaligned(table[off..].as_ptr() as *const Elf64Rela) };

            if rela.r_type() == R_RISCV_RELATIVE {
                let target_va = bias + rela.r_offset as usize;
                let value = (bias as i64 + rela.r_addend) as u64;
                let vpn = VirtAddr(target_va & !(PAGE_SIZE - 1)).floor();
                let page_off = target_va & (PAGE_SIZE - 1);

                if let Some(ppn) = page_table.translate(vpn) {
                    let dst = ppn.as_bytes_mut();
                    if page_off + 8 <= PAGE_SIZE {
                        dst[page_off..page_off + 8].copy_from_slice(&value.to_le_bytes());
                    } else {
                        let first_part = PAGE_SIZE - page_off;
                        let bytes = value.to_le_bytes();
                        dst[page_off..].copy_from_slice(&bytes[..first_part]);
                        let next_vpn = VirtAddr(target_va + first_part).floor();
                        if let Some(next_ppn) = page_table.translate(next_vpn) {
                            let next_dst = next_ppn.as_bytes_mut();
                            next_dst[..8 - first_part].copy_from_slice(&bytes[first_part..]);
                        }
                    }
                }
            }

            off += rela_entry_size;
        }

        Ok(())
    }

    fn load_from_file(
        file: &dyn File,
        file_size: usize,
        address_space: &mut AddressSpace,
        cache_id: Option<(u64, u64, u64, u64)>,
    ) -> Result<ElfInfo, LoaderError> {
        let header_size = core::mem::size_of::<ElfHeader>();
        if file_size < header_size {
            return Err(LoaderError::FileTooSmall);
        }

        let mut header_buf = Vec::new();
        header_buf.resize(header_size, 0u8);
        Self::read_exact_from_file(file, 0, &mut header_buf)?;

        let header = ElfHeader::parse(&header_buf)?;
        let phdrs = Self::read_program_headers_from_file(file, file_size, &header)?;
        let bias = if header.is_shared() { PIE_LOAD_BASE } else { 0 };

        let mut base = usize::MAX;
        let mut end = 0usize;
        let mut phdr_vaddr: Option<usize> = None;
        let mut dynamic_offset: Option<usize> = None;
        let mut dynamic_size: Option<usize> = None;
        let mut interp_path: Option<String> = None;

        for phdr in &phdrs {
            if phdr.is_load() {
                let seg_start = phdr.vaddr() + bias;
                let seg_end = seg_start + phdr.mem_size();
                base = base.min(seg_start);
                end = end.max(seg_end);
            }

            if phdr.p_type == PT_PHDR {
                phdr_vaddr = Some(phdr.vaddr() + bias);
            }
            if phdr.p_type == PT_DYNAMIC {
                dynamic_offset = Some(phdr.offset());
                dynamic_size = Some(phdr.file_size());
            }
            if phdr.p_type == PT_INTERP {
                let off = phdr.offset();
                let sz = phdr.file_size();
                if sz > 0 && sz < 256 {
                    let mut buf = alloc::vec![0u8; sz];
                    if Self::read_exact_from_file(file, off, &mut buf).is_ok() {
                        let end_pos = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                        if let Ok(s) = core::str::from_utf8(&buf[..end_pos]) {
                            interp_path = Some(String::from(s));
                        }
                    }
                }
            }
        }

        if base == usize::MAX {
            return Err(LoaderError::NoLoadableSegments);
        }

        // 段内容 = 原始文件字节（内核不改写）时才允许共享帧缓存。
        // PIE（bias!=0）下内核会对 RELATIVE 重定位目标直写物理帧，
        // 预扫目标区间，与之相交的段禁用缓存；其余只读段（text 等）
        // 内容与 bias 无关，可安全共享（bias 固定为 PIE_LOAD_BASE）。
        let reloc_span = if bias != 0 && cache_id.is_some() {
            if let (Some(dyn_off), Some(dyn_sz)) = (dynamic_offset, dynamic_size) {
                Self::rela_target_span(file, file_size, &phdrs, dyn_off, dyn_sz)?
            } else {
                None
            }
        } else {
            None
        };
        for phdr in &phdrs {
            if phdr.is_load() {
                let seg_cache_id = match reloc_span {
                    Some((lo, hi))
                        if phdr.vaddr() < hi && phdr.vaddr() + phdr.mem_size() > lo =>
                    {
                        None
                    }
                    _ => cache_id,
                };
                Self::load_segment_from_file(
                    file, file_size, phdr, address_space, bias, seg_cache_id,
                )?;
            }
        }

        let phdr_vaddr = phdr_vaddr.unwrap_or(base + header.e_phoff as usize);

        if bias != 0 {
            if let (Some(dyn_off), Some(dyn_sz)) = (dynamic_offset, dynamic_size) {
                Self::process_relocations_from_file(
                    file,
                    file_size,
                    &phdrs,
                    dyn_off,
                    dyn_sz,
                    bias,
                    address_space,
                )?;
            }
        }

        // brk 间隙同上（Linux arch_randomize_brk 语义）。
        let heap_start = ((end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1)) + Self::BRK_GAP;
        let program_break = heap_start;
        let stack_top = ElfInfo::DEFAULT_STACK_TOP;

        let entry = header.entry() + bias;
        Ok(ElfInfo {
            entry,
            base,
            end,
            stack_top,
            heap_start,
            program_break,
            is_pie: header.is_shared(),
            phdr_vaddr,
            phent_size: header.e_phentsize as usize,
            phnum: header.e_phnum as usize,
            interp_path,
            interp_base: 0,
            app_entry: entry,
        })
    }

    /// 从文件系统加载 ELF 文件到地址空间
    ///
    /// # 参数
    /// - `path`: 文件系统中的 ELF 文件路径
    /// - `address_space`: 目标地址空间
    ///
    /// # 返回
    /// - `Ok(ElfInfo)`: 加载成功，返回加载信息
    /// - `Err(LoaderError)`: 加载失败
    ///
    /// # 加载过程
    /// 1. 通过 VFS 接口查找文件
    /// 2. 读取文件内容到缓冲区
    /// 3. 调用 load 函数解析并加载 ELF
    ///
    /// # Requirements
    /// - Requirements 3.4: THE ELF_Loader SHALL read ELF data from the FAT32 file system through the VFS interface
    pub fn load_from_fs(
        path: &str,
        address_space: &mut AddressSpace,
    ) -> Result<ElfInfo, LoaderError> {
        use crate::fs::{lookup, FsError};

        // 1. 通过 VFS 查找文件
        let inode = lookup(path).map_err(|e| match e {
            FsError::NotFound => LoaderError::FileNotFound,
            FsError::NotDirectory => LoaderError::FileNotFound,
            _ => LoaderError::IoError,
        })?;

        // 2. 获取文件状态，检查是否为普通文件
        let stat = inode.stat().map_err(|_| LoaderError::IoError)?;
        
        // 检查是否为目录
        if stat.file_type == crate::fs::FileType::Directory {
            return Err(LoaderError::NotAFile);
        }

        // 3. 获取文件接口
        let file = inode.as_file().ok_or(LoaderError::NotAFile)?;

        let file_size = stat.size;
        if file_size == 0 {
            return Err(LoaderError::FileTooSmall);
        }

        // 段缓存身份键：普通文件按 (ino,size,mtime,ctime) 唯一标定内容版本。
        // 文件被写会刷新 mtime/ctime（或改变 size），键随之失效，绝不误命中。
        let cache_id = Some((
            inode.ino() as u64,
            file_size as u64,
            stat.mtime,
            stat.ctime,
        ));

        Self::load_from_file(file, file_size, address_space, cache_id)
    }

    /// 从文件系统加载动态链接器（interpreter）到地址空间
    ///
    /// 与 load_from_fs 不同，interpreter 始终加载到 INTERP_LOAD_BASE。
    /// 返回 (entry_point, load_base)。
    pub fn load_interp_from_fs(
        path: &str,
        address_space: &mut AddressSpace,
    ) -> Result<(usize, usize), LoaderError> {
        use crate::fs::{lookup, FsError};

        let inode = lookup(path).map_err(|e| match e {
            FsError::NotFound => LoaderError::FileNotFound,
            FsError::NotDirectory => LoaderError::FileNotFound,
            _ => LoaderError::IoError,
        })?;

        let stat = inode.stat().map_err(|_| LoaderError::IoError)?;
        if stat.file_type == crate::fs::FileType::Directory {
            return Err(LoaderError::NotAFile);
        }

        let file = inode.as_file().ok_or(LoaderError::NotAFile)?;
        let file_size = stat.size;
        if file_size == 0 {
            return Err(LoaderError::FileTooSmall);
        }

        // 段缓存身份键：与 load_from_fs 同一套内容版本标定。
        let cache_id = Some((
            inode.ino() as u64,
            file_size as u64,
            stat.mtime,
            stat.ctime,
        ));

        let header_size = core::mem::size_of::<ElfHeader>();
        let mut header_buf = alloc::vec![0u8; header_size];
        Self::read_exact_from_file(file, 0, &mut header_buf)?;
        let header = ElfHeader::parse(&header_buf)?;
        let phdrs = Self::read_program_headers_from_file(file, file_size, &header)?;

        let bias = INTERP_LOAD_BASE;

        let mut base = usize::MAX;
        let mut end = 0usize;
        let mut dynamic_offset: Option<usize> = None;
        let mut dynamic_size: Option<usize> = None;

        for phdr in &phdrs {
            if phdr.is_load() {
                let seg_start = phdr.vaddr() + bias;
                let seg_end = seg_start + phdr.mem_size();
                base = base.min(seg_start);
                end = end.max(seg_end);
            }
            if phdr.p_type == PT_DYNAMIC {
                dynamic_offset = Some(phdr.offset());
                dynamic_size = Some(phdr.file_size());
            }
        }

        if base == usize::MAX {
            return Err(LoaderError::NoLoadableSegments);
        }

        // interpreter（ld.so）只读段也参与共享帧缓存：bias 固定为
        // INTERP_LOAD_BASE，段内容 = 原始文件字节；仅排除会被内核
        // RELATIVE 重定位直写的段（预扫目标区间判定）。
        let reloc_span = if let (Some(dyn_off), Some(dyn_sz)) = (dynamic_offset, dynamic_size) {
            Self::rela_target_span(file, file_size, &phdrs, dyn_off, dyn_sz)?
        } else {
            None
        };
        for phdr in &phdrs {
            if phdr.is_load() {
                let seg_cache_id = match reloc_span {
                    Some((lo, hi))
                        if phdr.vaddr() < hi && phdr.vaddr() + phdr.mem_size() > lo =>
                    {
                        None
                    }
                    _ => cache_id,
                };
                Self::load_segment_from_file(
                    file, file_size, phdr, address_space, bias, seg_cache_id,
                )?;
            }
        }

        if let (Some(dyn_off), Some(dyn_sz)) = (dynamic_offset, dynamic_size) {
            Self::process_relocations_from_file(
                file, file_size, &phdrs, dyn_off, dyn_sz, bias, address_space,
            )?;
        }

        Ok((header.entry() + bias, base))
    }
}

// ============================================================================
// 错误类型
// ============================================================================

/// ELF 加载器错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoaderError {
    /// 文件太小，无法包含 ELF 头
    FileTooSmall,
    /// 无效的 ELF Magic Number
    InvalidMagic,
    /// 不支持的 ELF 类型（非 64 位）
    UnsupportedClass,
    /// 不支持的字节序（非小端序）
    UnsupportedEndian,
    /// 不支持的 ELF 版本
    UnsupportedVersion,
    /// 不支持的目标架构（非 RISC-V）
    UnsupportedArch,
    /// 不支持的文件类型（非可执行文件或共享库）
    UnsupportedType,
    /// 无效的程序头大小
    InvalidProgramHeaderSize,
    /// 无效的程序头
    InvalidProgramHeader,
    /// 无效的段偏移
    InvalidSegmentOffset,
    /// 没有可加载的段
    NoLoadableSegments,
    /// 内存映射失败
    MappingFailed,
    /// 内存不足
    OutOfMemory,
    /// 文件未找到
    FileNotFound,
    /// 不是文件（是目录）
    NotAFile,
    /// I/O 错误
    IoError,
}

impl fmt::Display for LoaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileTooSmall => write!(f, "File too small to contain ELF header"),
            Self::InvalidMagic => write!(f, "Invalid ELF magic number"),
            Self::UnsupportedClass => write!(f, "Unsupported ELF class (not 64-bit)"),
            Self::UnsupportedEndian => write!(f, "Unsupported byte order (not little-endian)"),
            Self::UnsupportedVersion => write!(f, "Unsupported ELF version"),
            Self::UnsupportedArch => write!(f, "Unsupported architecture (not RISC-V)"),
            Self::UnsupportedType => write!(f, "Unsupported file type (not executable or shared)"),
            Self::InvalidProgramHeaderSize => write!(f, "Invalid program header size"),
            Self::InvalidProgramHeader => write!(f, "Invalid program header"),
            Self::InvalidSegmentOffset => write!(f, "Invalid segment offset"),
            Self::NoLoadableSegments => write!(f, "No loadable segments found"),
            Self::MappingFailed => write!(f, "Memory mapping failed"),
            Self::OutOfMemory => write!(f, "Out of memory"),
            Self::FileNotFound => write!(f, "File not found"),
            Self::NotAFile => write!(f, "Not a file (is a directory)"),
            Self::IoError => write!(f, "I/O error"),
        }
    }
}

impl From<LoaderError> for KernelError {
    fn from(e: LoaderError) -> Self {
        match e {
            LoaderError::OutOfMemory => KernelError::Memory(MemoryError::OutOfMemory),
            LoaderError::MappingFailed => KernelError::Memory(MemoryError::MappingFailed),
            LoaderError::FileNotFound => KernelError::FileSystem(crate::fs::FsError::NotFound),
            LoaderError::NotAFile => KernelError::FileSystem(crate::fs::FsError::IsDirectory),
            LoaderError::IoError => KernelError::FileSystem(crate::fs::FsError::IoError),
            LoaderError::FileTooSmall => KernelError::Other("ELF file too small"),
            LoaderError::InvalidMagic => KernelError::Other("Invalid ELF magic number"),
            LoaderError::UnsupportedClass => KernelError::Other("Unsupported ELF class"),
            LoaderError::UnsupportedEndian => KernelError::Other("Unsupported ELF endianness"),
            LoaderError::UnsupportedVersion => KernelError::Other("Unsupported ELF version"),
            LoaderError::UnsupportedArch => KernelError::Other("Unsupported ELF architecture"),
            LoaderError::UnsupportedType => KernelError::Other("Unsupported ELF type"),
            LoaderError::InvalidProgramHeaderSize => KernelError::Other("Invalid ELF program header size"),
            LoaderError::InvalidProgramHeader => KernelError::Other("Invalid ELF program header"),
            LoaderError::InvalidSegmentOffset => KernelError::Other("Invalid ELF segment offset"),
            LoaderError::NoLoadableSegments => KernelError::Other("No loadable ELF segments"),
        }
    }
}
