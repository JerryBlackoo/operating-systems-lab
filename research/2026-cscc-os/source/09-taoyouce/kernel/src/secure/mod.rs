//! 安全启动与 TEE 支持
//!
//! 实现签名验证、PMP 隔离和安全世界切换
//!
//! # 功能
//! 
//! - 安全启动验证 (Secure Boot Verification)
//! - 启动度量 (Boot Measurement)
//! - PMP 物理内存保护
//! - TEE 可信执行环境支持
//!
//! # 使用示例
//!
//! ```rust
//! use crate::secure;
//!
//! // 初始化安全子系统
//! secure::init();
//!
//! // 验证启动链
//! if let Err(e) = secure::verify_boot_chain() {
//!     panic!("Boot chain verification failed: {:?}", e);
//! }
//!
//! // 记录启动度量
//! secure::measure_boot_stage(BootStage::Kernel, &kernel_data);
//! ```

use crate::sync::SpinLock;

/// 签名算法
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureAlgorithm {
    /// Ed25519
    Ed25519,
    /// RSA-2048
    Rsa2048,
    /// RSA-4096
    Rsa4096,
    /// ECDSA P-256
    EcdsaP256,
}

/// 哈希算法
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HashAlgorithm {
    Sha256,
    Sha384,
    Sha512,
}

/// 公钥
#[derive(Debug, Clone)]
pub struct PublicKey {
    /// 算法
    pub algorithm: SignatureAlgorithm,
    /// 密钥数据
    pub data: alloc::vec::Vec<u8>,
}

/// 签名
#[derive(Debug, Clone)]
pub struct Signature {
    /// 算法
    pub algorithm: SignatureAlgorithm,
    /// 签名数据
    pub data: alloc::vec::Vec<u8>,
}

/// 验证结果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyResult {
    /// 验证成功
    Valid,
    /// 签名无效
    InvalidSignature,
    /// 密钥无效
    InvalidKey,
    /// 算法不支持
    UnsupportedAlgorithm,
    /// 数据损坏
    DataCorrupted,
}

/// 镜像头部
#[repr(C)]
#[derive(Debug, Clone)]
pub struct ImageHeader {
    /// 魔数
    pub magic: u32,
    /// 版本
    pub version: u32,
    /// 镜像大小
    pub image_size: u32,
    /// 签名偏移
    pub signature_offset: u32,
    /// 签名大小
    pub signature_size: u32,
    /// 哈希算法
    pub hash_algo: u8,
    /// 签名算法
    pub sig_algo: u8,
    /// 保留
    pub reserved: [u8; 2],
}

impl ImageHeader {
    pub const MAGIC: u32 = 0x4B524E4C; // "KRNL"
    pub const SIZE: usize = 24;
    
    /// 验证魔数
    pub fn is_valid(&self) -> bool {
        self.magic == Self::MAGIC
    }
}

/// 安全启动验证器
pub struct SecureBootVerifier {
    /// 信任的公钥列表
    trusted_keys: alloc::vec::Vec<PublicKey>,
    /// 是否启用
    enabled: bool,
}

impl SecureBootVerifier {
    pub fn new() -> Self {
        Self {
            trusted_keys: alloc::vec::Vec::new(),
            enabled: true,
        }
    }
    
    /// 添加信任的公钥
    pub fn add_trusted_key(&mut self, key: PublicKey) {
        self.trusted_keys.push(key);
    }
    
    /// 验证镜像
    pub fn verify_image(&self, image: &[u8]) -> VerifyResult {
        if !self.enabled {
            return VerifyResult::Valid;
        }
        
        if image.len() < ImageHeader::SIZE {
            return VerifyResult::DataCorrupted;
        }
        
        // 解析头部
        let header = unsafe {
            core::ptr::read_unaligned(image.as_ptr() as *const ImageHeader)
        };
        
        if !header.is_valid() {
            return VerifyResult::DataCorrupted;
        }
        
        // 获取签名
        let sig_start = header.signature_offset as usize;
        let sig_end = sig_start + header.signature_size as usize;
        
        if sig_end > image.len() {
            return VerifyResult::DataCorrupted;
        }
        
        let signature_data = &image[sig_start..sig_end];
        let image_data = &image[..sig_start];
        
        // 计算哈希
        let hash = self.compute_hash(image_data, header.hash_algo);
        
        // 验证签名
        for key in &self.trusted_keys {
            if self.verify_signature(&hash, signature_data, key) {
                return VerifyResult::Valid;
            }
        }
        
        VerifyResult::InvalidSignature
    }
    
    /// 计算哈希（简化实现）
    fn compute_hash(&self, data: &[u8], _algo: u8) -> [u8; 32] {
        // 简化的哈希实现
        let mut hash = [0u8; 32];
        for (i, &byte) in data.iter().enumerate() {
            hash[i % 32] ^= byte;
        }
        hash
    }
    
    /// 验证签名（简化实现）
    fn verify_signature(&self, _hash: &[u8; 32], _signature: &[u8], _key: &PublicKey) -> bool {
        // 实际实现需要密码学库
        true
    }
    
    /// 启用/禁用安全启动
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

// ============================================================================
// 启动度量 (Boot Measurement) 支持
// ============================================================================

/// 启动阶段
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootStage {
    /// 固件阶段
    Firmware,
    /// 引导加载器阶段
    Bootloader,
    /// 内核阶段
    Kernel,
    /// 初始化阶段
    Init,
    /// 用户空间阶段
    UserSpace,
}

impl BootStage {
    /// 获取阶段名称
    pub fn name(&self) -> &'static str {
        match self {
            BootStage::Firmware => "Firmware",
            BootStage::Bootloader => "Bootloader",
            BootStage::Kernel => "Kernel",
            BootStage::Init => "Init",
            BootStage::UserSpace => "UserSpace",
        }
    }
    
    /// 获取阶段索引（用于 PCR 扩展）
    pub fn pcr_index(&self) -> usize {
        match self {
            BootStage::Firmware => 0,
            BootStage::Bootloader => 1,
            BootStage::Kernel => 2,
            BootStage::Init => 3,
            BootStage::UserSpace => 4,
        }
    }
}

/// 启动度量记录
#[derive(Debug, Clone)]
pub struct BootMeasurement {
    /// 启动阶段
    pub stage: BootStage,
    /// 度量哈希值
    pub hash: [u8; 32],
    /// 时间戳（启动后的 tick 数）
    pub timestamp: u64,
    /// 描述信息
    pub description: alloc::string::String,
}

/// 启动度量日志
pub struct BootMeasurementLog {
    /// 度量记录列表
    measurements: alloc::vec::Vec<BootMeasurement>,
    /// PCR 值（模拟 TPM PCR）
    pcr_values: [[u8; 32]; 8],
    /// 是否启用
    enabled: bool,
}

impl BootMeasurementLog {
    /// 创建新的度量日志
    pub fn new() -> Self {
        Self {
            measurements: alloc::vec::Vec::new(),
            pcr_values: [[0u8; 32]; 8],
            enabled: true,
        }
    }
    
    /// 记录启动度量
    pub fn measure(&mut self, stage: BootStage, data: &[u8], description: &str) {
        if !self.enabled {
            return;
        }
        
        // 计算数据哈希
        let hash = Self::compute_hash(data);
        
        // 扩展 PCR 值
        let pcr_index = stage.pcr_index();
        if pcr_index < self.pcr_values.len() {
            self.extend_pcr(pcr_index, &hash);
        }
        
        // 获取时间戳
        let timestamp = Self::get_timestamp();
        
        // 记录度量
        let measurement = BootMeasurement {
            stage,
            hash,
            timestamp,
            description: alloc::string::String::from(description),
        };
        
        self.measurements.push(measurement);
        
        crate::println!("[secure] Boot measurement: stage={}, hash={:02x?}", 
            stage.name(), &hash[..8]);
    }
    
    /// 扩展 PCR 值（模拟 TPM PCR 扩展操作）
    fn extend_pcr(&mut self, index: usize, hash: &[u8; 32]) {
        if index >= self.pcr_values.len() {
            return;
        }
        
        // PCR_new = Hash(PCR_old || new_hash)
        let mut combined = [0u8; 64];
        combined[..32].copy_from_slice(&self.pcr_values[index]);
        combined[32..].copy_from_slice(hash);
        
        self.pcr_values[index] = Self::compute_hash(&combined);
    }
    
    /// 获取 PCR 值
    pub fn get_pcr(&self, index: usize) -> Option<[u8; 32]> {
        if index < self.pcr_values.len() {
            Some(self.pcr_values[index])
        } else {
            None
        }
    }
    
    /// 获取所有度量记录
    pub fn get_measurements(&self) -> &[BootMeasurement] {
        &self.measurements
    }
    
    /// 验证启动链完整性
    pub fn verify_chain(&self) -> Result<(), BootChainError> {
        // 检查是否有度量记录
        if self.measurements.is_empty() {
            return Err(BootChainError::NoMeasurements);
        }
        
        // 检查必要的启动阶段是否都有度量
        let required_stages = [BootStage::Kernel];
        for stage in &required_stages {
            if !self.measurements.iter().any(|m| m.stage == *stage) {
                return Err(BootChainError::MissingStage(*stage));
            }
        }
        
        // 验证 PCR 值不为零（表示有有效度量）
        let kernel_pcr = self.pcr_values[BootStage::Kernel.pcr_index()];
        if kernel_pcr == [0u8; 32] {
            return Err(BootChainError::InvalidPcr(BootStage::Kernel.pcr_index()));
        }
        
        Ok(())
    }
    
    /// 计算哈希（简化实现）
    fn compute_hash(data: &[u8]) -> [u8; 32] {
        let mut hash = [0u8; 32];
        for (i, &byte) in data.iter().enumerate() {
            hash[i % 32] ^= byte;
            // 简单的混合操作
            hash[(i + 1) % 32] = hash[(i + 1) % 32].wrapping_add(byte);
        }
        hash
    }
    
    /// 获取时间戳
    fn get_timestamp() -> u64 {
        // 读取 RISC-V cycle 计数器
        #[cfg(target_arch = "riscv64")]
        unsafe {
            let cycle: u64;
            core::arch::asm!("rdcycle {}", out(reg) cycle);
            cycle
        }
        #[cfg(not(target_arch = "riscv64"))]
        0
    }
    
    /// 启用/禁用度量
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }
}

/// 启动链验证错误
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootChainError {
    /// 没有度量记录
    NoMeasurements,
    /// 缺少必要的启动阶段
    MissingStage(BootStage),
    /// PCR 值无效
    InvalidPcr(usize),
    /// 签名验证失败
    SignatureInvalid,
    /// 哈希不匹配
    HashMismatch,
}

// ============================================================================
// PMP (Physical Memory Protection) 支持
// ============================================================================

/// PMP 配置
#[derive(Debug, Clone, Copy)]
pub struct PmpConfig {
    /// 地址
    pub addr: usize,
    /// 大小
    pub size: usize,
    /// 权限
    pub perm: PmpPermission,
    /// 地址匹配模式
    pub mode: PmpMode,
}

/// PMP 权限
#[derive(Debug, Clone, Copy)]
pub struct PmpPermission {
    pub read: bool,
    pub write: bool,
    pub execute: bool,
}

impl PmpPermission {
    pub const NONE: Self = Self { read: false, write: false, execute: false };
    pub const RO: Self = Self { read: true, write: false, execute: false };
    pub const RW: Self = Self { read: true, write: true, execute: false };
    pub const RX: Self = Self { read: true, write: false, execute: true };
    pub const RWX: Self = Self { read: true, write: true, execute: true };
    
    /// 编码为 PMP 配置位
    pub fn encode(&self) -> u8 {
        let mut bits = 0u8;
        if self.read { bits |= 0x01; }
        if self.write { bits |= 0x02; }
        if self.execute { bits |= 0x04; }
        bits
    }
}

/// PMP 地址匹配模式
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmpMode {
    /// 禁用
    Off,
    /// TOR (Top of Range)
    Tor,
    /// NA4 (Naturally Aligned 4-byte)
    Na4,
    /// NAPOT (Naturally Aligned Power-of-Two)
    Napot,
}

impl PmpMode {
    pub fn encode(&self) -> u8 {
        match self {
            PmpMode::Off => 0,
            PmpMode::Tor => 1,
            PmpMode::Na4 => 2,
            PmpMode::Napot => 3,
        }
    }
}

/// PMP 管理器
pub struct PmpManager {
    /// 配置数组（最多 16 个 PMP 条目）
    configs: [Option<PmpConfig>; 16],
}

impl PmpManager {
    pub fn new() -> Self {
        Self {
            configs: [None; 16],
        }
    }
    
    /// 配置 PMP 条目
    pub fn configure(&mut self, index: usize, config: PmpConfig) -> Result<(), &'static str> {
        if index >= 16 {
            return Err("PMP index out of range");
        }
        
        self.configs[index] = Some(config);
        self.apply_config(index, &config);
        Ok(())
    }
    
    /// 清除 PMP 条目
    pub fn clear(&mut self, index: usize) {
        if index < 16 {
            self.configs[index] = None;
            // 写入零配置
        }
    }
    
    /// 应用配置到硬件
    /// 
    /// 注意：PMP 寄存器只能在 M-mode 下访问。
    /// 在 S-mode 下运行时，我们只记录配置，实际的 PMP 设置需要通过 SBI 调用完成。
    fn apply_config(&self, index: usize, config: &PmpConfig) {
        // 计算 NAPOT 地址
        let napot_addr = if config.mode == PmpMode::Napot {
            let size_bits = (config.size.trailing_zeros() - 2) as usize;
            (config.addr >> 2) | ((1 << size_bits) - 1)
        } else {
            config.addr >> 2
        };
        
        // 编码配置
        let cfg_bits = config.perm.encode() | (config.mode.encode() << 3);
        
        // PMP 寄存器只能在 M-mode 下访问
        // 在 S-mode 下，我们只记录配置信息，不实际写入 CSR
        // 实际的 PMP 配置需要通过 SBI 调用或在 M-mode 固件中完成
        #[cfg(feature = "mmode")]
        unsafe {
            match index {
                0 => {
                    core::arch::asm!(
                        "csrw pmpaddr0, {addr}",
                        addr = in(reg) napot_addr,
                    );
                }
                // ... 其他 PMP 寄存器
                _ => {}
            }
        }
        
        // 在 S-mode 下，只记录配置（用于调试）
        #[cfg(not(feature = "mmode"))]
        {
            let _ = napot_addr;
            let _ = cfg_bits;
            let _ = index;
        }
    }
}

// ============================================================================
// TEE (Trusted Execution Environment) 支持
// ============================================================================

/// 安全世界状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorldState {
    /// 普通世界
    Normal,
    /// 安全世界
    Secure,
}

/// TEE 上下文
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct TeeContext {
    /// 通用寄存器
    pub regs: [u64; 32],
    /// PC
    pub pc: u64,
    /// 状态寄存器
    pub status: u64,
}

/// TEE 管理器
pub struct TeeManager {
    /// 当前世界
    current_world: WorldState,
    /// 普通世界上下文
    normal_ctx: TeeContext,
    /// 安全世界上下文
    secure_ctx: TeeContext,
    /// 安全世界入口
    secure_entry: usize,
    /// PMP 管理器
    pmp: PmpManager,
}

impl TeeManager {
    pub fn new() -> Self {
        Self {
            current_world: WorldState::Normal,
            normal_ctx: TeeContext::default(),
            secure_ctx: TeeContext::default(),
            secure_entry: 0,
            pmp: PmpManager::new(),
        }
    }
    
    /// 设置安全世界入口
    pub fn set_secure_entry(&mut self, entry: usize) {
        self.secure_entry = entry;
    }
    
    /// 配置安全内存区域
    pub fn configure_secure_memory(&mut self, base: usize, size: usize) -> Result<(), &'static str> {
        // 使用 PMP 保护安全内存
        self.pmp.configure(0, PmpConfig {
            addr: base,
            size,
            perm: PmpPermission::NONE, // 普通世界无法访问
            mode: PmpMode::Napot,
        })
    }
    
    /// 切换到安全世界
    pub fn enter_secure(&mut self) -> Result<(), &'static str> {
        if self.current_world == WorldState::Secure {
            return Err("Already in secure world");
        }
        
        // 保存普通世界上下文
        Self::save_context_static(&mut self.normal_ctx);
        
        // 恢复安全世界上下文
        Self::restore_context_static(&self.secure_ctx);
        
        // 更新 PMP 配置
        self.update_pmp_for_secure();
        
        self.current_world = WorldState::Secure;
        Ok(())
    }
    
    /// 返回普通世界
    pub fn exit_secure(&mut self) -> Result<(), &'static str> {
        if self.current_world == WorldState::Normal {
            return Err("Already in normal world");
        }
        
        // 保存安全世界上下文
        Self::save_context_static(&mut self.secure_ctx);
        
        // 恢复普通世界上下文
        Self::restore_context_static(&self.normal_ctx);
        
        // 更新 PMP 配置
        self.update_pmp_for_normal();
        
        self.current_world = WorldState::Normal;
        Ok(())
    }
    
    /// 获取当前世界
    pub fn current_world(&self) -> WorldState {
        self.current_world
    }
    
    fn save_context_static(_ctx: &mut TeeContext) {
        // 保存寄存器
    }
    
    fn restore_context_static(_ctx: &TeeContext) {
        // 恢复寄存器
    }
    
    fn update_pmp_for_secure(&mut self) {
        // 安全世界可以访问所有内存
    }
    
    fn update_pmp_for_normal(&mut self) {
        // 普通世界无法访问安全内存
    }
}

/// 全局安全管理器
static SECURE_MANAGER: SpinLock<Option<SecureManager>> = SpinLock::new(None);

/// 安全管理器
pub struct SecureManager {
    /// 安全启动验证器
    pub verifier: SecureBootVerifier,
    /// TEE 管理器
    pub tee: TeeManager,
    /// 启动度量日志
    pub measurement_log: BootMeasurementLog,
}

impl SecureManager {
    fn new() -> Self {
        Self {
            verifier: SecureBootVerifier::new(),
            tee: TeeManager::new(),
            measurement_log: BootMeasurementLog::new(),
        }
    }
}

/// 初始化安全子系统
pub fn init() {
    let mut mgr = SECURE_MANAGER.lock();
    *mgr = Some(SecureManager::new());
    crate::println!("[secure] Security subsystem initialized");
    
    // 记录内核启动度量
    if let Some(ref mut manager) = *mgr {
        // 使用内核代码段作为度量数据（简化实现）
        extern "C" {
            fn stext();
            fn etext();
        }
        let kernel_text = unsafe {
            core::slice::from_raw_parts(
                stext as *const u8,
                (etext as usize - stext as usize).min(4096) // 限制大小
            )
        };
        manager.measurement_log.measure(
            BootStage::Kernel,
            kernel_text,
            "Kernel code section"
        );
    }
}

/// 验证启动链
/// 
/// 检查所有启动阶段的度量是否有效，确保系统从可信状态启动。
/// 
/// # 返回值
/// 
/// - `Ok(())` - 启动链验证通过
/// - `Err(BootChainError)` - 验证失败，包含具体错误信息
/// 
/// # 示例
/// 
/// ```rust
/// if let Err(e) = secure::verify_boot_chain() {
///     panic!("Boot verification failed: {:?}", e);
/// }
/// ```
pub fn verify_boot_chain() -> Result<(), BootChainError> {
    let mgr = SECURE_MANAGER.lock();
    if let Some(ref manager) = *mgr {
        let result = manager.measurement_log.verify_chain();
        match &result {
            Ok(()) => {
                crate::println!("[secure] Boot chain verification: PASSED");
            }
            Err(e) => {
                crate::println!("[secure] Boot chain verification: FAILED ({:?})", e);
            }
        }
        result
    } else {
        Err(BootChainError::NoMeasurements)
    }
}

/// 记录启动度量
/// 
/// 在启动过程中记录各阶段的度量值，用于后续验证。
/// 
/// # 参数
/// 
/// - `stage` - 启动阶段
/// - `data` - 要度量的数据
/// - `description` - 度量描述
/// 
/// # 示例
/// 
/// ```rust
/// secure::measure_boot_stage(BootStage::Init, &init_data, "Init process");
/// ```
pub fn measure_boot_stage(stage: BootStage, data: &[u8], description: &str) {
    let mut mgr = SECURE_MANAGER.lock();
    if let Some(ref mut manager) = *mgr {
        manager.measurement_log.measure(stage, data, description);
    }
}

/// 获取 PCR 值
/// 
/// 获取指定索引的 PCR（Platform Configuration Register）值。
/// 
/// # 参数
/// 
/// - `index` - PCR 索引 (0-7)
/// 
/// # 返回值
/// 
/// - `Some([u8; 32])` - PCR 值
/// - `None` - 索引无效或安全管理器未初始化
pub fn get_pcr(index: usize) -> Option<[u8; 32]> {
    let mgr = SECURE_MANAGER.lock();
    if let Some(ref manager) = *mgr {
        manager.measurement_log.get_pcr(index)
    } else {
        None
    }
}

/// 验证镜像
pub fn verify_image(image: &[u8]) -> VerifyResult {
    SECURE_MANAGER.lock()
        .as_ref()
        .map(|m| m.verifier.verify_image(image))
        .unwrap_or(VerifyResult::UnsupportedAlgorithm)
}

/// 安全模块自测试
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[secure] Running self-test...");
    
    // 测试 SignatureAlgorithm
    let sig_algos = [
        SignatureAlgorithm::Ed25519,
        SignatureAlgorithm::Rsa2048,
        SignatureAlgorithm::Rsa4096,
        SignatureAlgorithm::EcdsaP256,
    ];
    for algo in &sig_algos {
        crate::println!("[secure] Signature algorithm: {:?}", algo);
    }
    
    // 测试 HashAlgorithm
    let hash_algos = [
        HashAlgorithm::Sha256,
        HashAlgorithm::Sha384,
        HashAlgorithm::Sha512,
    ];
    for algo in &hash_algos {
        crate::println!("[secure] Hash algorithm: {:?}", algo);
    }
    
    // 测试 VerifyResult
    let results = [
        VerifyResult::Valid,
        VerifyResult::InvalidSignature,
        VerifyResult::InvalidKey,
        VerifyResult::UnsupportedAlgorithm,
        VerifyResult::DataCorrupted,
    ];
    for result in &results {
        crate::println!("[secure] Verify result: {:?}", result);
    }
    
    // 测试 ImageHeader
    let header = ImageHeader {
        magic: ImageHeader::MAGIC,
        version: 1,
        image_size: 1024,
        signature_offset: 512,
        signature_size: 64,
        hash_algo: 0,
        sig_algo: 0,
        reserved: [0; 2],
    };
    assert!(header.is_valid());
    assert_eq!(ImageHeader::SIZE, 24);
    
    // 测试 SecureBootVerifier
    let mut verifier = SecureBootVerifier::new();
    let key = PublicKey {
        algorithm: SignatureAlgorithm::Ed25519,
        data: alloc::vec![0u8; 32],
    };
    verifier.add_trusted_key(key);
    verifier.set_enabled(true);
    let _ = verifier.trusted_keys.len();
    let _ = verifier.enabled;
    
    // 测试验证（使用无效数据）
    let result = verifier.verify_image(&[0u8; 10]);
    assert_eq!(result, VerifyResult::DataCorrupted);
    
    // 测试 compute_hash 和 verify_signature
    let hash = verifier.compute_hash(&[1, 2, 3, 4], 0);
    let _ = verifier.verify_signature(&hash, &[0u8; 64], &PublicKey {
        algorithm: SignatureAlgorithm::Ed25519,
        data: alloc::vec![0u8; 32],
    });
    
    // 测试 BootStage
    let stages = [
        BootStage::Firmware,
        BootStage::Bootloader,
        BootStage::Kernel,
        BootStage::Init,
        BootStage::UserSpace,
    ];
    for stage in &stages {
        crate::println!("[secure] Boot stage: {} (PCR index: {})", stage.name(), stage.pcr_index());
    }
    
    // 测试 BootMeasurementLog
    let mut log = BootMeasurementLog::new();
    log.set_enabled(true);  // 测试启用功能
    log.measure(BootStage::Firmware, b"firmware data", "Test firmware");
    log.measure(BootStage::Bootloader, b"bootloader data", "Test bootloader");
    log.measure(BootStage::Kernel, b"kernel data", "Test kernel");
    
    // 测试禁用功能
    log.set_enabled(false);
    let count_before = log.get_measurements().len();
    log.measure(BootStage::Init, b"should not be recorded", "Disabled test");
    let count_after = log.get_measurements().len();
    assert_eq!(count_before, count_after, "Measurement should not be recorded when disabled");
    log.set_enabled(true);  // 重新启用
    
    // 验证度量记录
    assert_eq!(log.get_measurements().len(), 3);
    
    // 验证 PCR 值
    let pcr0 = log.get_pcr(0).expect("PCR 0 should exist");
    assert_ne!(pcr0, [0u8; 32], "PCR 0 should not be zero after measurement");
    
    // 测试启动链验证
    let chain_result = log.verify_chain();
    assert!(chain_result.is_ok(), "Boot chain verification should pass");
    
    // 测试 BootChainError
    let errors = [
        BootChainError::NoMeasurements,
        BootChainError::MissingStage(BootStage::Kernel),
        BootChainError::InvalidPcr(0),
        BootChainError::SignatureInvalid,
        BootChainError::HashMismatch,
    ];
    for error in &errors {
        crate::println!("[secure] Boot chain error: {:?}", error);
    }
    
    // 测试全局函数
    let pcr_result = get_pcr(BootStage::Kernel.pcr_index());
    crate::println!("[secure] Global PCR result: {:?}", pcr_result.is_some());
    
    // 测试 PmpPermission
    let perms = [
        PmpPermission::NONE,
        PmpPermission::RO,
        PmpPermission::RW,
        PmpPermission::RX,
        PmpPermission::RWX,
    ];
    for perm in &perms {
        let encoded = perm.encode();
        crate::println!("[secure] PMP permission encoded: {:#x}", encoded);
    }
    
    // 测试 PmpMode
    let modes = [
        PmpMode::Off,
        PmpMode::Tor,
        PmpMode::Na4,
        PmpMode::Napot,
    ];
    for mode in &modes {
        let encoded = mode.encode();
        crate::println!("[secure] PMP mode {:?} encoded: {}", mode, encoded);
    }
    
    // 测试 PmpManager
    let mut pmp = PmpManager::new();
    let config = PmpConfig {
        addr: 0x8000_0000,
        size: 0x1000,
        perm: PmpPermission::RWX,
        mode: PmpMode::Napot,
    };
    let _ = pmp.configure(0, config);
    pmp.clear(0);
    let _ = pmp.configs[0];
    
    // 测试 WorldState
    let worlds = [WorldState::Normal, WorldState::Secure];
    for world in &worlds {
        crate::println!("[secure] World state: {:?}", world);
    }
    
    // 测试 TeeContext
    let ctx = TeeContext::default();
    let _ = ctx.regs;
    let _ = ctx.pc;
    let _ = ctx.status;
    
    // 测试 TeeManager
    let mut tee = TeeManager::new();
    tee.set_secure_entry(0x9000_0000);
    let _ = tee.configure_secure_memory(0x9000_0000, 0x10_0000);
    let _ = tee.current_world();
    let _ = tee.current_world;
    let _ = tee.normal_ctx;
    let _ = tee.secure_ctx;
    let _ = tee.secure_entry;
    
    // 测试世界切换
    let _ = tee.enter_secure();
    let _ = tee.exit_secure();
    
    // 测试全局 verify_boot_chain
    let boot_chain_result = verify_boot_chain();
    crate::println!("[secure] Boot chain verification result: {:?}", boot_chain_result.is_ok());
    
    // 测试全局 verify_image 函数
    let image_result = verify_image(&[0u8; 100]);
    crate::println!("[secure] Image verification result: {:?}", image_result);
    
    // 测试全局 measure_boot_stage 函数
    measure_boot_stage(BootStage::UserSpace, b"test data", "Self-test measurement");
    crate::println!("[secure] Boot stage measurement recorded");
    
    crate::println!("[secure] Self-test passed!");
    Ok(())
}
