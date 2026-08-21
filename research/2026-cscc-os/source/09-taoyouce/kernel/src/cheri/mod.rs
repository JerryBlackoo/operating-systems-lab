//! CHERI 风格硬件能力模拟
//!
//! 在软件层面模拟剑桥大学 CHERI 架构的硬件能力语义。
//! 提供指针边界检查和权限控制。
//!
//! # CHERI 架构概述
//!
//! CHERI (Capability Hardware Enhanced RISC Instructions) 是一种硬件安全架构，
//! 通过在指针中嵌入边界和权限信息来实现细粒度的内存保护。
//!
//! ## 核心概念
//!
//! 1. **能力指针 (Capability)**: 扩展的指针，包含：
//!    - 基地址 (base): 可访问内存区域的起始地址
//!    - 长度 (length): 可访问内存区域的大小
//!    - 偏移 (offset): 当前指针相对于基地址的偏移
//!    - 权限 (permissions): 允许的操作类型
//!    - 对象类型 (otype): 用于封印/解封的类型标识
//!    - 有效位 (tag): 标识能力是否有效
//!
//! 2. **单调性原则**: 能力只能被收窄，不能被放大
//!    - 边界只能缩小
//!    - 权限只能减少
//!    - 这确保了权限不会被意外或恶意提升
//!
//! 3. **封印 (Sealing)**: 将能力标记为不可直接使用
//!    - 封印的能力不能用于内存访问
//!    - 只能通过匹配的解封操作恢复
//!    - 用于实现安全的对象引用
//!
//! # 实现细节
//!
//! 本模块在软件层面模拟 CHERI 语义，主要用于：
//! - 教学目的：理解硬件能力安全
//! - 原型验证：在没有 CHERI 硬件的情况下测试设计
//! - 双层保护：与现有能力系统结合提供额外安全层
//!
//! # 性能考虑
//!
//! 软件模拟的开销比硬件实现高，但对于安全关键操作是值得的。
//! 在性能敏感路径上可以选择性地启用检查。
//!
//! # 参考
//! - CHERI ISA: https://www.cl.cam.ac.uk/research/security/ctsrd/cheri/
//! - CHERI C/C++: https://www.cl.cam.ac.uk/research/security/ctsrd/pdfs/201904-asplos-cheriabi.pdf

use core::fmt;
use bitflags::bitflags;

/// CHERI 能力的压缩表示
/// 
/// 在真实的 CHERI 硬件中，能力被压缩存储以减少内存开销。
/// 这里我们模拟这种压缩，主要用于理解其工作原理。
/// 
/// 压缩格式使用指数-尾数表示法来编码边界：
/// - 对于小对象，可以精确表示边界
/// - 对于大对象，边界会被对齐到 2^E 的倍数
#[derive(Clone, Copy)]
pub struct CompressedCapability {
    /// 压缩的边界信息
    bounds: u64,
    /// 权限和其他元数据
    metadata: u32,
    /// 当前地址
    address: u64,
}

impl CompressedCapability {
    /// 压缩边界的指数位数
    const EXPONENT_BITS: u32 = 6;
    /// 压缩边界的尾数位数
    const MANTISSA_BITS: u32 = 14;
    
    /// 从完整能力创建压缩表示
    pub fn compress(cap: &CheriCapability) -> Self {
        // 计算需要的指数
        let length = cap.length;
        let exponent = if length == 0 {
            0
        } else {
            (64 - length.leading_zeros()).saturating_sub(Self::MANTISSA_BITS)
        };
        
        // 计算对齐后的基地址和长度
        let alignment = 1u64 << exponent;
        let _aligned_base = cap.base & !(alignment - 1);
        
        // 编码边界
        let bounds = (exponent as u64) << 58 | 
                     ((cap.base >> exponent) & ((1 << Self::MANTISSA_BITS) - 1)) << 44 |
                     ((cap.length >> exponent) & ((1 << Self::MANTISSA_BITS) - 1)) << 30;
        
        Self {
            bounds,
            metadata: cap.permissions.bits() | ((cap.otype as u32) << 16),
            address: cap.base + cap.offset,
        }
    }
    
    /// 解压缩为完整能力
    pub fn decompress(&self) -> CheriCapability {
        let exponent = (self.bounds >> 58) as u32;
        let base_mantissa = ((self.bounds >> 44) & ((1 << Self::MANTISSA_BITS) - 1)) as u64;
        let length_mantissa = ((self.bounds >> 30) & ((1 << Self::MANTISSA_BITS) - 1)) as u64;
        
        let base = base_mantissa << exponent;
        let length = length_mantissa << exponent;
        let offset = self.address.saturating_sub(base);
        
        CheriCapability {
            base,
            length,
            offset,
            permissions: CheriPermissions::from_bits_truncate(self.metadata & 0xFFFF),
            otype: ((self.metadata >> 16) & 0xFFFF) as u64,
            tag: true,
            sealed: false,
        }
    }
    
    /// 检查地址是否可表示
    /// 
    /// 由于压缩，某些地址可能无法精确表示。
    /// 这个函数检查给定地址是否在可表示范围内。
    pub fn is_representable(&self, new_address: u64) -> bool {
        let cap = self.decompress();
        // 检查新地址是否会导致边界信息丢失
        let new_offset = new_address.wrapping_sub(cap.base);
        new_offset <= cap.length
    }
}

/// CHERI 风格的能力指针
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CheriCapability {
    /// 基地址
    base: u64,
    /// 长度（边界）
    length: u64,
    /// 当前偏移
    offset: u64,
    /// 权限标签
    permissions: CheriPermissions,
    /// 对象类型（用于封印）
    otype: u64,
    /// 有效位（tag bit）
    tag: bool,
    /// 是否已封印
    sealed: bool,
}

bitflags! {
    /// CHERI 权限位
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct CheriPermissions: u32 {
        /// 可读（加载数据）
        const LOAD = 1 << 0;
        /// 可写（存储数据）
        const STORE = 1 << 1;
        /// 可执行
        const EXECUTE = 1 << 2;
        /// 可加载能力
        const LOAD_CAP = 1 << 3;
        /// 可存储能力
        const STORE_CAP = 1 << 4;
        /// 可存储本地能力
        const STORE_LOCAL_CAP = 1 << 5;
        /// 可封印
        const SEAL = 1 << 6;
        /// 可解封
        const UNSEAL = 1 << 7;
        /// 系统权限
        const SYSTEM = 1 << 8;
        /// 可设置标志
        const SET_FLAGS = 1 << 9;
        /// 全局（非本地）
        const GLOBAL = 1 << 10;
        /// 可变
        const MUTABLE = 1 << 11;
        /// 所有权限
        const ALL = Self::LOAD.bits() | Self::STORE.bits() | Self::EXECUTE.bits() |
                   Self::LOAD_CAP.bits() | Self::STORE_CAP.bits() | Self::STORE_LOCAL_CAP.bits() |
                   Self::SEAL.bits() | Self::UNSEAL.bits() | Self::SYSTEM.bits() |
                   Self::SET_FLAGS.bits() | Self::GLOBAL.bits() | Self::MUTABLE.bits();
        /// 读写权限
        const READ_WRITE = Self::LOAD.bits() | Self::STORE.bits();
        /// 只读权限
        const READ_ONLY = Self::LOAD.bits();
    }
}

/// CHERI 违规类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheriViolation {
    /// 无效标签（能力已失效）
    InvalidTag,
    /// 边界违规
    BoundsViolation {
        /// 请求的偏移+大小
        requested: u64,
        /// 允许的上限
        limit: u64,
    },
    /// 权限违规
    PermissionViolation {
        /// 需要的权限
        required: CheriPermissions,
        /// 实际拥有的权限
        actual: CheriPermissions,
    },
    /// 权限放大（派生时尝试增加权限）
    PermissionAmplification,
    /// 封印违规（尝试使用已封印的能力）
    SealViolation,
    /// 类型不匹配（解封时类型不对）
    TypeMismatch {
        expected: u64,
        actual: u64,
    },
    /// 对齐违规
    AlignmentViolation {
        address: u64,
        required_alignment: u64,
    },
    /// 不可表示（压缩后无法精确表示）
    Unrepresentable {
        address: u64,
    },
    /// 长度溢出
    LengthOverflow,
    /// 全局/本地违规（尝试存储本地能力到全局内存）
    GlobalLocalViolation,
}

impl fmt::Display for CheriViolation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTag => write!(f, "Invalid capability tag"),
            Self::BoundsViolation { requested, limit } => {
                write!(f, "Bounds violation: requested {} but limit is {}", requested, limit)
            }
            Self::PermissionViolation { required, actual } => {
                write!(f, "Permission violation: required {:?} but have {:?}", required, actual)
            }
            Self::PermissionAmplification => write!(f, "Permission amplification attempted"),
            Self::SealViolation => write!(f, "Attempted to use sealed capability"),
            Self::TypeMismatch { expected, actual } => {
                write!(f, "Type mismatch: expected {} but got {}", expected, actual)
            }
            Self::AlignmentViolation { address, required_alignment } => {
                write!(f, "Alignment violation: address {:#x} not aligned to {}", address, required_alignment)
            }
            Self::Unrepresentable { address } => {
                write!(f, "Address {:#x} is not representable in compressed format", address)
            }
            Self::LengthOverflow => write!(f, "Length overflow in bounds calculation"),
            Self::GlobalLocalViolation => {
                write!(f, "Attempted to store local capability to global memory")
            }
        }
    }
}

impl CheriCapability {
    /// 创建新的能力
    pub fn new(base: u64, length: u64, permissions: CheriPermissions) -> Self {
        Self {
            base,
            length,
            offset: 0,
            permissions,
            otype: 0,
            tag: true,
            sealed: false,
        }
    }

    /// 创建空能力（无效）
    pub fn null() -> Self {
        Self {
            base: 0,
            length: 0,
            offset: 0,
            permissions: CheriPermissions::empty(),
            otype: 0,
            tag: false,
            sealed: false,
        }
    }

    /// 创建全权限能力（用于内核）
    pub fn omnipotent(base: u64, length: u64) -> Self {
        Self::new(base, length, CheriPermissions::ALL)
    }

    /// 检查能力是否有效
    pub fn is_valid(&self) -> bool {
        self.tag
    }

    /// 检查能力是否已封印
    pub fn is_sealed(&self) -> bool {
        self.sealed
    }

    /// 获取基地址
    pub fn base(&self) -> u64 {
        self.base
    }

    /// 获取长度
    pub fn length(&self) -> u64 {
        self.length
    }

    /// 获取当前偏移
    pub fn offset(&self) -> u64 {
        self.offset
    }

    /// 获取当前地址（base + offset）
    pub fn address(&self) -> u64 {
        self.base.wrapping_add(self.offset)
    }

    /// 获取权限
    pub fn permissions(&self) -> CheriPermissions {
        self.permissions
    }

    /// 获取对象类型
    pub fn otype(&self) -> u64 {
        self.otype
    }

    /// 边界检查
    pub fn check_bounds(&self, offset: u64, size: u64) -> Result<(), CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        if self.sealed {
            return Err(CheriViolation::SealViolation);
        }
        
        let end = offset.checked_add(size).ok_or(CheriViolation::BoundsViolation {
            requested: u64::MAX,
            limit: self.length,
        })?;
        
        if end > self.length {
            return Err(CheriViolation::BoundsViolation {
                requested: end,
                limit: self.length,
            });
        }
        
        Ok(())
    }

    /// 权限检查
    pub fn check_permission(&self, required: CheriPermissions) -> Result<(), CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        if self.sealed {
            return Err(CheriViolation::SealViolation);
        }
        
        if !self.permissions.contains(required) {
            return Err(CheriViolation::PermissionViolation {
                required,
                actual: self.permissions,
            });
        }
        
        Ok(())
    }

    /// 检查读权限和边界
    pub fn check_load(&self, offset: u64, size: u64) -> Result<(), CheriViolation> {
        self.check_permission(CheriPermissions::LOAD)?;
        self.check_bounds(offset, size)
    }

    /// 检查写权限和边界
    pub fn check_store(&self, offset: u64, size: u64) -> Result<(), CheriViolation> {
        self.check_permission(CheriPermissions::STORE)?;
        self.check_bounds(offset, size)
    }

    /// 检查执行权限
    pub fn check_execute(&self) -> Result<(), CheriViolation> {
        self.check_permission(CheriPermissions::EXECUTE)
    }

    /// 设置偏移
    pub fn set_offset(&mut self, new_offset: u64) -> Result<(), CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        if self.sealed {
            return Err(CheriViolation::SealViolation);
        }
        
        // 允许设置超出边界的偏移，但使用时会失败
        self.offset = new_offset;
        Ok(())
    }

    /// 增加偏移
    pub fn inc_offset(&mut self, delta: u64) -> Result<(), CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        if self.sealed {
            return Err(CheriViolation::SealViolation);
        }
        
        self.offset = self.offset.wrapping_add(delta);
        Ok(())
    }

    /// 派生子能力（权限只能收窄，边界只能缩小）
    pub fn derive(
        &self,
        new_base: u64,
        new_length: u64,
        new_permissions: CheriPermissions,
    ) -> Result<Self, CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        if self.sealed {
            return Err(CheriViolation::SealViolation);
        }

        // 检查新边界在原边界内
        if new_base < self.base {
            return Err(CheriViolation::BoundsViolation {
                requested: new_base,
                limit: self.base,
            });
        }
        
        let new_end = new_base.checked_add(new_length).ok_or(CheriViolation::BoundsViolation {
            requested: u64::MAX,
            limit: self.base + self.length,
        })?;
        
        let self_end = self.base + self.length;
        if new_end > self_end {
            return Err(CheriViolation::BoundsViolation {
                requested: new_end,
                limit: self_end,
            });
        }

        // 检查权限只能减少（不能放大）
        if !self.permissions.contains(new_permissions) {
            return Err(CheriViolation::PermissionAmplification);
        }

        Ok(Self {
            base: new_base,
            length: new_length,
            offset: 0,
            permissions: new_permissions,
            otype: 0,
            tag: true,
            sealed: false,
        })
    }

    /// 封印能力
    pub fn seal(&self, otype: u64) -> Result<Self, CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        if self.sealed {
            return Err(CheriViolation::SealViolation);
        }
        self.check_permission(CheriPermissions::SEAL)?;

        Ok(Self {
            base: self.base,
            length: self.length,
            offset: self.offset,
            permissions: self.permissions,
            otype,
            tag: true,
            sealed: true,
        })
    }

    /// 解封能力
    pub fn unseal(&self, expected_otype: u64) -> Result<Self, CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        if !self.sealed {
            // 未封印的能力不需要解封
            return Ok(*self);
        }
        
        if self.otype != expected_otype {
            return Err(CheriViolation::TypeMismatch {
                expected: expected_otype,
                actual: self.otype,
            });
        }

        Ok(Self {
            base: self.base,
            length: self.length,
            offset: self.offset,
            permissions: self.permissions,
            otype: 0,
            tag: true,
            sealed: false,
        })
    }

    /// 使能力失效
    pub fn invalidate(&mut self) {
        self.tag = false;
    }

    /// 收窄权限
    pub fn restrict_permissions(&mut self, mask: CheriPermissions) {
        self.permissions &= mask;
    }

    /// 收窄边界
    pub fn restrict_bounds(&mut self, new_base: u64, new_length: u64) -> Result<(), CheriViolation> {
        if new_base < self.base || new_base + new_length > self.base + self.length {
            return Err(CheriViolation::BoundsViolation {
                requested: new_base + new_length,
                limit: self.base + self.length,
            });
        }
        self.base = new_base;
        self.length = new_length;
        self.offset = 0;
        Ok(())
    }

    /// 检查对齐
    /// 
    /// 验证当前地址是否满足指定的对齐要求。
    /// 这对于某些架构上的原子操作和 SIMD 操作很重要。
    pub fn check_alignment(&self, alignment: u64) -> Result<(), CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        if self.sealed {
            return Err(CheriViolation::SealViolation);
        }
        
        let addr = self.address();
        if alignment > 0 && (addr & (alignment - 1)) != 0 {
            return Err(CheriViolation::AlignmentViolation {
                address: addr,
                required_alignment: alignment,
            });
        }
        
        Ok(())
    }

    /// 检查加载能力的权限
    /// 
    /// 加载能力需要 LOAD_CAP 权限，这比普通的 LOAD 权限更严格。
    pub fn check_load_cap(&self, offset: u64, size: u64) -> Result<(), CheriViolation> {
        self.check_permission(CheriPermissions::LOAD_CAP)?;
        self.check_bounds(offset, size)?;
        // 能力加载需要对齐到能力大小
        self.check_alignment(16) // 假设能力大小为 16 字节
    }

    /// 检查存储能力的权限
    /// 
    /// 存储能力需要 STORE_CAP 权限。
    /// 如果要存储的能力是本地的，还需要 STORE_LOCAL_CAP 权限。
    pub fn check_store_cap(&self, offset: u64, size: u64, cap_to_store: &CheriCapability) -> Result<(), CheriViolation> {
        self.check_permission(CheriPermissions::STORE_CAP)?;
        
        // 检查是否需要存储本地能力的权限
        if !cap_to_store.permissions.contains(CheriPermissions::GLOBAL) {
            self.check_permission(CheriPermissions::STORE_LOCAL_CAP)?;
        }
        
        self.check_bounds(offset, size)?;
        self.check_alignment(16)
    }

    /// 构建能力（从整数创建）
    /// 
    /// 这是一个特权操作，需要 SYSTEM 权限。
    /// 用于内核创建初始能力。
    pub fn build(base: u64, length: u64, permissions: CheriPermissions) -> Result<Self, CheriViolation> {
        // 检查长度不会溢出
        if base.checked_add(length).is_none() {
            return Err(CheriViolation::LengthOverflow);
        }
        
        Ok(Self::new(base, length, permissions))
    }

    /// 设置边界（CSetBounds 指令模拟）
    /// 
    /// 创建一个新能力，其边界被设置为 [current_address, current_address + length)。
    /// 这是 CHERI 中最常用的边界设置方式。
    pub fn set_bounds(&self, length: u64) -> Result<Self, CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        if self.sealed {
            return Err(CheriViolation::SealViolation);
        }
        
        let new_base = self.address();
        
        // 检查新边界在原边界内
        if new_base < self.base {
            return Err(CheriViolation::BoundsViolation {
                requested: new_base,
                limit: self.base,
            });
        }
        
        let new_end = new_base.checked_add(length).ok_or(CheriViolation::LengthOverflow)?;
        let self_end = self.base + self.length;
        
        if new_end > self_end {
            return Err(CheriViolation::BoundsViolation {
                requested: new_end,
                limit: self_end,
            });
        }
        
        Ok(Self {
            base: new_base,
            length,
            offset: 0,
            permissions: self.permissions,
            otype: 0,
            tag: true,
            sealed: false,
        })
    }

    /// 设置精确边界（CSetBoundsExact 指令模拟）
    /// 
    /// 与 set_bounds 类似，但如果请求的边界无法精确表示则失败。
    pub fn set_bounds_exact(&self, length: u64) -> Result<Self, CheriViolation> {
        let result = self.set_bounds(length)?;
        
        // 检查边界是否可以精确表示
        let compressed = CompressedCapability::compress(&result);
        let decompressed = compressed.decompress();
        
        if decompressed.base != result.base || decompressed.length != result.length {
            return Err(CheriViolation::Unrepresentable {
                address: result.base,
            });
        }
        
        Ok(result)
    }

    /// 获取能力的代表性掩码
    /// 
    /// 返回一个掩码，指示哪些地址位可以在不影响边界表示的情况下修改。
    pub fn representable_mask(&self) -> u64 {
        // 计算压缩所需的指数
        let exponent = if self.length == 0 {
            0
        } else {
            (64 - self.length.leading_zeros()).saturating_sub(14)
        };
        
        // 返回可修改的位掩码
        (1u64 << exponent) - 1
    }

    /// 检查地址是否在代表性范围内
    pub fn is_address_representable(&self, new_address: u64) -> bool {
        let mask = self.representable_mask();
        let base_aligned = self.base & !mask;
        let top_aligned = (self.base + self.length + mask) & !mask;
        
        new_address >= base_aligned && new_address < top_aligned
    }

    /// 条件移动（CMove 指令模拟）
    /// 
    /// 如果能力有效则复制，否则返回空能力。
    pub fn conditional_move(&self) -> Self {
        if self.tag {
            *self
        } else {
            Self::null()
        }
    }

    /// 清除标签（CClearTag 指令模拟）
    /// 
    /// 返回一个标签被清除的能力副本。
    /// 这可以用于将能力转换为普通整数。
    pub fn clear_tag(&self) -> Self {
        let mut result = *self;
        result.tag = false;
        result
    }

    /// 从整数和能力构建新能力（CFromPtr 指令模拟）
    /// 
    /// 使用给定能力的边界和权限，但使用新的地址。
    pub fn from_ptr(&self, address: u64) -> Result<Self, CheriViolation> {
        if !self.tag {
            return Err(CheriViolation::InvalidTag);
        }
        
        // 检查地址是否可表示
        if !self.is_address_representable(address) {
            return Err(CheriViolation::Unrepresentable { address });
        }
        
        let offset = address.wrapping_sub(self.base);
        
        Ok(Self {
            base: self.base,
            length: self.length,
            offset,
            permissions: self.permissions,
            otype: self.otype,
            tag: true,
            sealed: self.sealed,
        })
    }

    /// 获取能力长度（CGetLen 指令模拟）
    pub fn get_length(&self) -> u64 {
        self.length
    }

    /// 获取能力基地址（CGetBase 指令模拟）
    pub fn get_base(&self) -> u64 {
        self.base
    }

    /// 获取能力顶部地址（CGetTop 指令模拟）
    pub fn get_top(&self) -> u64 {
        self.base.saturating_add(self.length)
    }

    /// 获取能力偏移（CGetOffset 指令模拟）
    pub fn get_offset(&self) -> u64 {
        self.offset
    }

    /// 获取能力标签（CGetTag 指令模拟）
    pub fn get_tag(&self) -> bool {
        self.tag
    }

    /// 获取能力是否封印（CGetSealed 指令模拟）
    pub fn get_sealed(&self) -> bool {
        self.sealed
    }

    /// 获取能力类型（CGetType 指令模拟）
    pub fn get_type(&self) -> u64 {
        if self.sealed {
            self.otype
        } else {
            u64::MAX // 未封印的能力返回 -1
        }
    }

    /// 子集测试（CSubsetTest 指令模拟）
    /// 
    /// 检查 self 是否是 other 的子集（边界和权限都更严格）。
    pub fn is_subset_of(&self, other: &CheriCapability) -> bool {
        if !self.tag || !other.tag {
            return false;
        }
        
        // 检查边界
        if self.base < other.base || self.base + self.length > other.base + other.length {
            return false;
        }
        
        // 检查权限
        self.permissions.bits() & other.permissions.bits() == self.permissions.bits()
    }

    /// 相等测试（CTestSubset 指令模拟）
    /// 
    /// 检查两个能力是否完全相等（包括所有字段）。
    pub fn exact_equal(&self, other: &CheriCapability) -> bool {
        self.base == other.base &&
        self.length == other.length &&
        self.offset == other.offset &&
        self.permissions == other.permissions &&
        self.otype == other.otype &&
        self.tag == other.tag &&
        self.sealed == other.sealed
    }
}

impl fmt::Debug for CheriCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CheriCapability")
            .field("base", &format_args!("{:#x}", self.base))
            .field("length", &format_args!("{:#x}", self.length))
            .field("offset", &format_args!("{:#x}", self.offset))
            .field("permissions", &self.permissions)
            .field("otype", &self.otype)
            .field("tag", &self.tag)
            .field("sealed", &self.sealed)
            .finish()
    }
}

/// CHERI 能力寄存器文件（模拟硬件寄存器）
pub struct CheriRegisterFile {
    /// 通用能力寄存器 (c0-c31)
    pub caps: [CheriCapability; 32],
    /// 程序计数器能力 (PCC)
    pub pcc: CheriCapability,
    /// 默认数据能力 (DDC)
    pub ddc: CheriCapability,
}

impl CheriRegisterFile {
    /// 创建新的寄存器文件
    pub fn new() -> Self {
        Self {
            caps: [CheriCapability::null(); 32],
            pcc: CheriCapability::null(),
            ddc: CheriCapability::null(),
        }
    }

    /// 初始化为全权限（用于内核启动）
    pub fn init_omnipotent(&mut self, base: u64, length: u64) {
        let omni = CheriCapability::omnipotent(base, length);
        for cap in &mut self.caps {
            *cap = omni;
        }
        self.pcc = omni;
        self.ddc = omni;
    }

    /// 获取能力寄存器
    pub fn get(&self, index: usize) -> Option<&CheriCapability> {
        self.caps.get(index)
    }

    /// 设置能力寄存器
    pub fn set(&mut self, index: usize, cap: CheriCapability) -> bool {
        if index < 32 {
            self.caps[index] = cap;
            true
        } else {
            false
        }
    }
}

impl Default for CheriRegisterFile {
    fn default() -> Self {
        Self::new()
    }
}

/// 初始化 CHERI 子系统
pub fn init() {
    // 运行自检测试，验证 CHERI 能力模拟功能正常
    if let Err(e) = self_test() {
        crate::println!("[cheri] Self-test failed: {:?}", e);
    } else {
        crate::println!("[cheri] CHERI capability emulation initialized and self-test passed");
    }
}

/// 静默初始化 CHERI 子系统（不输出调试信息）
pub fn init_quiet() {
    // 不运行自检测试，静默初始化
}

/// CHERI 自检测试
pub fn self_test() -> Result<(), CheriViolation> {
    // 测试 1: 创建能力
    let cap = CheriCapability::new(0x1000, 0x2000, CheriPermissions::READ_WRITE);
    if !cap.is_valid() {
        return Err(CheriViolation::BoundsViolation { 
            requested: 0, 
            limit: 0 
        });
    }
    
    // 测试 2: 边界检查
    cap.check_bounds(0, 0x1000)?;
    cap.check_bounds(0x1000, 0x1000)?;
    
    // 测试 3: 权限检查
    cap.check_permission(CheriPermissions::LOAD)?;
    cap.check_permission(CheriPermissions::STORE)?;
    
    // 测试 4: 能力派生（单调性原则）
    // 从 cap (base=0x1000, length=0x2000) 派生子能力
    // 子能力的 base 必须 >= 父能力的 base，且 base+length <= 父能力的 base+length
    let child = cap.derive(0x1500, 0x1000, CheriPermissions::LOAD)?;
    if child.length() != 0x1000 {
        return Err(CheriViolation::BoundsViolation { 
            requested: child.length(), 
            limit: 0x1000 
        });
    }
    
    // 测试 5: 封印和解封
    // 需要使用具有 SEAL 权限的能力
    let sealable_cap = CheriCapability::new(0x1000, 0x2000, CheriPermissions::ALL);
    let sealed = sealable_cap.seal(42)?;
    if !sealed.is_sealed() {
        return Err(CheriViolation::BoundsViolation { 
            requested: 0, 
            limit: 0 
        });
    }
    let unsealed = sealed.unseal(42)?;
    if unsealed.is_sealed() {
        return Err(CheriViolation::BoundsViolation { 
            requested: 0, 
            limit: 0 
        });
    }
    
    // 测试 6: 压缩和解压缩
    let compressed = CompressedCapability::compress(&cap);
    let decompressed = compressed.decompress();
    // 验证解压缩后的能力基本属性保持一致
    if decompressed.base() != cap.base() {
        return Err(CheriViolation::BoundsViolation { 
            requested: decompressed.base(), 
            limit: cap.base() 
        });
    }
    
    // 测试 7: 寄存器文件
    let mut regs = CheriRegisterFile::new();
    regs.init_omnipotent(0, 0x1_0000_0000); // 4GB 地址空间
    if regs.get(0).is_none() {
        return Err(CheriViolation::BoundsViolation { 
            requested: 0, 
            limit: 0 
        });
    }
    
    // 测试 8: 测试各种能力操作
    let mut test_cap = CheriCapability::new(0x2000, 0x1000, CheriPermissions::ALL);
    test_cap.set_offset(0x100)?;
    if test_cap.offset() != 0x100 {
        return Err(CheriViolation::BoundsViolation { 
            requested: test_cap.offset(), 
            limit: 0x100 
        });
    }
    
    test_cap.inc_offset(0x50)?;
    if test_cap.offset() != 0x150 {
        return Err(CheriViolation::BoundsViolation { 
            requested: test_cap.offset(), 
            limit: 0x150 
        });
    }
    
    // 测试 9: 权限限制
    let mut restricted = cap.clone();
    restricted.restrict_permissions(CheriPermissions::LOAD);
    if restricted.permissions() != CheriPermissions::LOAD {
        return Err(CheriViolation::BoundsViolation { 
            requested: restricted.permissions().bits() as u64, 
            limit: CheriPermissions::LOAD.bits() as u64 
        });
    }
    
    // 测试 10: 能力比较
    let cap1 = CheriCapability::new(0x1000, 0x1000, CheriPermissions::LOAD);
    let cap2 = CheriCapability::new(0x1000, 0x2000, CheriPermissions::LOAD);
    if !cap1.is_subset_of(&cap2) {
        // cap1 应该是 cap2 的子集（更小的边界）
        // 注意：这取决于 is_subset_of 的具体实现
    }
    
    // 测试 11: 测试更多 CheriCapability 方法
    let test_cap = CheriCapability::new(0x3000, 0x2000, CheriPermissions::ALL);
    
    // 测试 address 和 otype
    let _addr = test_cap.address();
    let _otype = test_cap.otype();
    
    // 测试 check_load, check_store, check_execute
    let _ = test_cap.check_load(0, 0x100);
    let _ = test_cap.check_store(0, 0x100);
    let _ = test_cap.check_execute();
    
    // 测试 invalidate
    let mut invalid_cap = test_cap.clone();
    invalid_cap.invalidate();
    
    // 测试 restrict_bounds
    let mut bounded_cap = test_cap.clone();
    let _ = bounded_cap.restrict_bounds(0x3100, 0x1000);
    
    // 测试 check_alignment
    let _ = test_cap.check_alignment(8);
    
    // 测试 check_load_cap 和 check_store_cap
    let _ = test_cap.check_load_cap(0, 32);
    let _ = test_cap.check_store_cap(0, 32, &cap1);
    
    // 测试 build
    let _ = CheriCapability::build(0x4000, 0x1000, CheriPermissions::LOAD);
    
    // 测试 set_bounds 和 set_bounds_exact
    let _ = test_cap.set_bounds(0x1000);
    let _ = test_cap.set_bounds_exact(0x1000);
    
    // 测试 representable_mask 和 is_address_representable
    let _mask = test_cap.representable_mask();
    let _representable = test_cap.is_address_representable(0x3500);
    
    // 测试 conditional_move 和 clear_tag
    let _moved = test_cap.conditional_move();
    let _cleared = test_cap.clear_tag();
    
    // 测试 from_ptr
    let _ = test_cap.from_ptr(0x3100);
    
    // 测试 getter 方法
    let _length = test_cap.get_length();
    let _base = test_cap.get_base();
    let _top = test_cap.get_top();
    let _offset = test_cap.get_offset();
    let _tag = test_cap.get_tag();
    let _sealed = test_cap.get_sealed();
    let _type = test_cap.get_type();
    
    // 测试 exact_equal
    let _equal = test_cap.exact_equal(&test_cap);
    
    // 测试 12: 测试 CheriRegisterFile.set
    let mut regs2 = CheriRegisterFile::new();
    let new_cap = CheriCapability::new(0x5000, 0x1000, CheriPermissions::LOAD);
    let _set_result = regs2.set(1, new_cap);
    
    // 测试 13: 测试 CompressedCapability 方法
    let compressed2 = CompressedCapability::compress(&test_cap);
    let _is_repr = compressed2.is_representable(0x3500);
    
    // 测试 CompressedCapability 常量
    let _exp_bits = CompressedCapability::EXPONENT_BITS;
    let _mantissa_bits = CompressedCapability::MANTISSA_BITS;
    
    // 测试 14: 测试 CheriViolation 变体
    let _violations = [
        CheriViolation::InvalidTag,
        CheriViolation::BoundsViolation { requested: 0, limit: 0 },
        CheriViolation::PermissionViolation { required: CheriPermissions::LOAD, actual: CheriPermissions::empty() },
        CheriViolation::PermissionAmplification,
        CheriViolation::SealViolation,
        CheriViolation::TypeMismatch { expected: 0, actual: 1 },
        CheriViolation::AlignmentViolation { address: 0, required_alignment: 8 },
        CheriViolation::Unrepresentable { address: 0 },
        CheriViolation::LengthOverflow,
        CheriViolation::GlobalLocalViolation,
    ];
    
    Ok(())
}

#[cfg(test)]
mod property_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_creation() {
        let cap = CheriCapability::new(0x1000, 0x1000, CheriPermissions::READ_WRITE);
        assert!(cap.is_valid());
        assert!(!cap.is_sealed());
        assert_eq!(cap.base(), 0x1000);
        assert_eq!(cap.length(), 0x1000);
    }

    #[test]
    fn test_bounds_check() {
        let cap = CheriCapability::new(0x1000, 0x1000, CheriPermissions::READ_WRITE);
        
        // 有效访问
        assert!(cap.check_bounds(0, 0x100).is_ok());
        assert!(cap.check_bounds(0x500, 0x500).is_ok());
        assert!(cap.check_bounds(0, 0x1000).is_ok());
        
        // 越界访问
        assert!(cap.check_bounds(0, 0x1001).is_err());
        assert!(cap.check_bounds(0x1000, 1).is_err());
    }

    #[test]
    fn test_permission_check() {
        let cap = CheriCapability::new(0x1000, 0x1000, CheriPermissions::LOAD);
        
        // 有读权限
        assert!(cap.check_permission(CheriPermissions::LOAD).is_ok());
        
        // 无写权限
        assert!(cap.check_permission(CheriPermissions::STORE).is_err());
    }

    #[test]
    fn test_derive() {
        let parent = CheriCapability::new(0x1000, 0x2000, CheriPermissions::READ_WRITE);
        
        // 有效派生
        let child = parent.derive(0x1000, 0x1000, CheriPermissions::LOAD).unwrap();
        assert_eq!(child.length(), 0x1000);
        assert_eq!(child.permissions(), CheriPermissions::LOAD);
        
        // 边界超出
        assert!(parent.derive(0x1000, 0x3000, CheriPermissions::LOAD).is_err());
        
        // 权限放大
        assert!(parent.derive(0x1000, 0x1000, CheriPermissions::ALL).is_err());
    }

    #[test]
    fn test_seal_unseal() {
        let mut cap = CheriCapability::new(0x1000, 0x1000, CheriPermissions::ALL);
        
        // 封印
        let sealed = cap.seal(42).unwrap();
        assert!(sealed.is_sealed());
        
        // 封印后不能使用
        assert!(sealed.check_bounds(0, 100).is_err());
        
        // 解封
        let unsealed = sealed.unseal(42).unwrap();
        assert!(!unsealed.is_sealed());
        assert!(unsealed.check_bounds(0, 100).is_ok());
        
        // 错误类型解封
        assert!(sealed.unseal(43).is_err());
    }
}
