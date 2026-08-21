//! 内核配置
//!
//! 存储内核运行时配置参数，包括模块启用/禁用选项。
//! 
//! # 使用方式
//! 
//! 1. 使用全局静态配置（编译时确定）：
//! ```rust
//! use crate::config::KERNEL_CONFIG;
//! if KERNEL_CONFIG.power_enabled {
//!     power::init();
//! }
//! ```
//! 
//! 2. 使用运行时配置检查函数：
//! ```rust
//! use crate::config;
//! if config::is_power_enabled() {
//!     power::set_policy(power::PowerPolicy::Balanced);
//! }
//! ```
//! 
//! 3. 动态修改配置：
//! ```rust
//! use crate::config::KernelConfig;
//! let mut config = KernelConfig::load();
//! config.power_enabled = false;
//! config.store();
//! ```

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// CPU 数量
pub static NUM_CPUS: AtomicUsize = AtomicUsize::new(1);

/// 设置 CPU 数量
pub fn set_num_cpus(num: usize) {
    NUM_CPUS.store(num, Ordering::Release);
}

/// 获取 CPU 数量
pub fn get_num_cpus() -> usize {
    NUM_CPUS.load(Ordering::Acquire)
}

/// 内存大小（字节）
pub static MEMORY_SIZE: AtomicUsize = AtomicUsize::new(128 * 1024 * 1024); // 默认 128MB

/// 设置内存大小
pub fn set_memory_size(size: usize) {
    MEMORY_SIZE.store(size, Ordering::Release);
}

/// 获取内存大小
pub fn get_memory_size() -> usize {
    MEMORY_SIZE.load(Ordering::Acquire)
}

// ============================================================================
// 模块启用/禁用配置
// ============================================================================

/// 电源管理模块启用状态
pub static POWER_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// 安全模块启用状态
pub static SECURE_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// GDB Stub 模块启用状态
pub static GDB_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// 形式化验证模块启用状态
pub static FORMAL_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// CHERI 能力模拟模块启用状态
pub static CHERI_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// Hypervisor 模块启用状态
pub static HYPERVISOR_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// eBPF 模块启用状态
pub static EBPF_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// 热补丁模块启用状态
pub static LIVEPATCH_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// io_uring 模块启用状态
pub static IO_URING_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// 网络模块启用状态
pub static NETWORK_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// 文件系统模块启用状态
pub static FILESYSTEM_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// SMP 多核支持启用状态
pub static SMP_MODULE_ENABLED: AtomicBool = AtomicBool::new(true);

/// 调试输出级别
pub static DEBUG_LEVEL: AtomicUsize = AtomicUsize::new(3);

/// 内核配置结构体
/// 
/// 提供编译时和运行时的模块配置选项。
/// 
/// # 示例
/// 
/// ```rust
/// use crate::config::{KernelConfig, KERNEL_CONFIG};
/// 
/// // 使用默认配置
/// let config = KernelConfig::default();
/// 
/// // 使用全局静态配置
/// if KERNEL_CONFIG.enable_power_management {
///     // 初始化电源管理
/// }
/// ```
#[derive(Debug, Clone, Copy)]
pub struct KernelConfig {
    /// CPU 数量
    pub num_cpus: usize,
    /// 内存大小（字节）
    pub memory_size: usize,
    /// 电源管理模块
    pub power_enabled: bool,
    /// 安全模块
    pub secure_enabled: bool,
    /// GDB Stub 模块（调试模式下默认启用）
    pub gdb_enabled: bool,
    /// 形式化验证模块（调试模式下默认启用）
    pub formal_enabled: bool,
    /// CHERI 能力模拟
    pub cheri_enabled: bool,
    /// Hypervisor 模块
    pub hypervisor_enabled: bool,
    /// eBPF 模块
    pub ebpf_enabled: bool,
    /// 热补丁模块
    pub livepatch_enabled: bool,
    /// io_uring 模块
    pub io_uring_enabled: bool,
    /// 网络模块
    pub network_enabled: bool,
    /// 文件系统模块
    pub filesystem_enabled: bool,
    /// SMP 多核支持
    pub smp_enabled: bool,
    /// 调试输出详细级别 (0=关闭, 1=错误, 2=警告, 3=信息, 4=调试)
    pub debug_level: usize,
}

/// 全局内核配置常量
/// 
/// 这是编译时确定的默认配置。可以通过 `KernelConfig::load()` 
/// 获取运行时配置，或通过 `KernelConfig::store()` 修改运行时配置。
/// 
/// # 设计原则
/// 
/// - 生产环境：GDB 和形式化验证默认禁用以提高性能
/// - 调试环境：所有调试功能默认启用
/// - 所有功能模块默认启用，可在运行时禁用
pub const KERNEL_CONFIG: KernelConfig = KernelConfig {
    num_cpus: 1,
    memory_size: 128 * 1024 * 1024, // 128MB
    power_enabled: true,
    secure_enabled: true,
    // GDB 和形式化验证在调试模式下启用
    #[cfg(debug_assertions)]
    gdb_enabled: true,
    #[cfg(not(debug_assertions))]
    gdb_enabled: false,
    #[cfg(debug_assertions)]
    formal_enabled: true,
    #[cfg(not(debug_assertions))]
    formal_enabled: false,
    cheri_enabled: true,
    hypervisor_enabled: true,
    ebpf_enabled: true,
    livepatch_enabled: true,
    io_uring_enabled: true,
    network_enabled: true,
    filesystem_enabled: true,
    smp_enabled: true,
    debug_level: 3, // 默认信息级别
};

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            num_cpus: 1,
            memory_size: 128 * 1024 * 1024,
            power_enabled: true,
            secure_enabled: true,
            gdb_enabled: cfg!(debug_assertions),
            formal_enabled: cfg!(debug_assertions),
            cheri_enabled: true,
            hypervisor_enabled: true,
            ebpf_enabled: true,
            livepatch_enabled: true,
            io_uring_enabled: true,
            network_enabled: true,
            filesystem_enabled: true,
            smp_enabled: true,
            debug_level: 3,
        }
    }
}

impl KernelConfig {
    /// 创建新的配置（const 函数，可用于静态初始化）
    pub const fn new() -> Self {
        Self {
            num_cpus: 1,
            memory_size: 128 * 1024 * 1024,
            power_enabled: true,
            secure_enabled: true,
            gdb_enabled: true,
            formal_enabled: true,
            cheri_enabled: true,
            hypervisor_enabled: true,
            ebpf_enabled: true,
            livepatch_enabled: true,
            io_uring_enabled: true,
            network_enabled: true,
            filesystem_enabled: true,
            smp_enabled: true,
            debug_level: 3,
        }
    }
    
    /// 创建最小配置（仅启用核心功能）
    pub const fn minimal() -> Self {
        Self {
            num_cpus: 1,
            memory_size: 64 * 1024 * 1024, // 64MB
            power_enabled: false,
            secure_enabled: false,
            gdb_enabled: false,
            formal_enabled: false,
            cheri_enabled: false,
            hypervisor_enabled: false,
            ebpf_enabled: false,
            livepatch_enabled: false,
            io_uring_enabled: false,
            network_enabled: false,
            filesystem_enabled: true, // 文件系统是核心功能
            smp_enabled: false,
            debug_level: 1, // 仅错误
        }
    }
    
    /// 创建调试配置（启用所有调试功能）
    pub const fn debug() -> Self {
        Self {
            num_cpus: 1,
            memory_size: 256 * 1024 * 1024, // 256MB
            power_enabled: true,
            secure_enabled: true,
            gdb_enabled: true,
            formal_enabled: true,
            cheri_enabled: true,
            hypervisor_enabled: true,
            ebpf_enabled: true,
            livepatch_enabled: true,
            io_uring_enabled: true,
            network_enabled: true,
            filesystem_enabled: true,
            smp_enabled: true,
            debug_level: 4, // 完整调试输出
        }
    }

    /// 从全局原子变量加载当前配置
    pub fn load() -> Self {
        Self {
            num_cpus: NUM_CPUS.load(Ordering::Acquire),
            memory_size: MEMORY_SIZE.load(Ordering::Acquire),
            power_enabled: POWER_MODULE_ENABLED.load(Ordering::Acquire),
            secure_enabled: SECURE_MODULE_ENABLED.load(Ordering::Acquire),
            gdb_enabled: GDB_MODULE_ENABLED.load(Ordering::Acquire),
            formal_enabled: FORMAL_MODULE_ENABLED.load(Ordering::Acquire),
            cheri_enabled: CHERI_MODULE_ENABLED.load(Ordering::Acquire),
            hypervisor_enabled: HYPERVISOR_MODULE_ENABLED.load(Ordering::Acquire),
            ebpf_enabled: EBPF_MODULE_ENABLED.load(Ordering::Acquire),
            livepatch_enabled: LIVEPATCH_MODULE_ENABLED.load(Ordering::Acquire),
            io_uring_enabled: IO_URING_MODULE_ENABLED.load(Ordering::Acquire),
            network_enabled: NETWORK_MODULE_ENABLED.load(Ordering::Acquire),
            filesystem_enabled: FILESYSTEM_MODULE_ENABLED.load(Ordering::Acquire),
            smp_enabled: SMP_MODULE_ENABLED.load(Ordering::Acquire),
            debug_level: DEBUG_LEVEL.load(Ordering::Acquire),
        }
    }

    /// 将配置保存到全局原子变量
    pub fn store(&self) {
        NUM_CPUS.store(self.num_cpus, Ordering::Release);
        MEMORY_SIZE.store(self.memory_size, Ordering::Release);
        POWER_MODULE_ENABLED.store(self.power_enabled, Ordering::Release);
        SECURE_MODULE_ENABLED.store(self.secure_enabled, Ordering::Release);
        GDB_MODULE_ENABLED.store(self.gdb_enabled, Ordering::Release);
        FORMAL_MODULE_ENABLED.store(self.formal_enabled, Ordering::Release);
        CHERI_MODULE_ENABLED.store(self.cheri_enabled, Ordering::Release);
        HYPERVISOR_MODULE_ENABLED.store(self.hypervisor_enabled, Ordering::Release);
        EBPF_MODULE_ENABLED.store(self.ebpf_enabled, Ordering::Release);
        LIVEPATCH_MODULE_ENABLED.store(self.livepatch_enabled, Ordering::Release);
        IO_URING_MODULE_ENABLED.store(self.io_uring_enabled, Ordering::Release);
        NETWORK_MODULE_ENABLED.store(self.network_enabled, Ordering::Release);
        FILESYSTEM_MODULE_ENABLED.store(self.filesystem_enabled, Ordering::Release);
        SMP_MODULE_ENABLED.store(self.smp_enabled, Ordering::Release);
        DEBUG_LEVEL.store(self.debug_level, Ordering::Release);
    }
}

// ============================================================================
// 模块状态检查函数
// ============================================================================

/// 检查电源管理模块是否启用
pub fn is_power_enabled() -> bool {
    POWER_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查安全模块是否启用
pub fn is_secure_enabled() -> bool {
    SECURE_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查 GDB Stub 模块是否启用
pub fn is_gdb_enabled() -> bool {
    GDB_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查形式化验证模块是否启用
pub fn is_formal_enabled() -> bool {
    FORMAL_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查 CHERI 模块是否启用
pub fn is_cheri_enabled() -> bool {
    CHERI_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查 Hypervisor 模块是否启用
pub fn is_hypervisor_enabled() -> bool {
    HYPERVISOR_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查 eBPF 模块是否启用
pub fn is_ebpf_enabled() -> bool {
    EBPF_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查热补丁模块是否启用
pub fn is_livepatch_enabled() -> bool {
    LIVEPATCH_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查 io_uring 模块是否启用
pub fn is_io_uring_enabled() -> bool {
    IO_URING_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查网络模块是否启用
pub fn is_network_enabled() -> bool {
    NETWORK_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查文件系统模块是否启用
pub fn is_filesystem_enabled() -> bool {
    FILESYSTEM_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 检查 SMP 多核支持是否启用
pub fn is_smp_enabled() -> bool {
    SMP_MODULE_ENABLED.load(Ordering::Acquire)
}

/// 自测试启用状态
/// 临时禁用以加快启动速度进行调试
pub static SELF_TEST_ENABLED: AtomicBool = AtomicBool::new(false);

/// 检查自测试是否启用
pub fn is_self_test_enabled() -> bool {
    SELF_TEST_ENABLED.load(Ordering::Acquire)
}

/// 设置自测试启用状态
pub fn set_self_test_enabled(enabled: bool) {
    SELF_TEST_ENABLED.store(enabled, Ordering::Release);
}

/// 获取调试输出级别
pub fn get_debug_level() -> usize {
    DEBUG_LEVEL.load(Ordering::Acquire)
}

/// 设置调试输出级别
pub fn set_debug_level(level: usize) {
    DEBUG_LEVEL.store(level, Ordering::Release);
}

// ============================================================================
// 模块启用/禁用设置函数
// ============================================================================

/// 设置电源管理模块启用状态
pub fn set_power_enabled(enabled: bool) {
    POWER_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置安全模块启用状态
pub fn set_secure_enabled(enabled: bool) {
    SECURE_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置 GDB Stub 模块启用状态
pub fn set_gdb_enabled(enabled: bool) {
    GDB_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置形式化验证模块启用状态
pub fn set_formal_enabled(enabled: bool) {
    FORMAL_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置 CHERI 模块启用状态
pub fn set_cheri_enabled(enabled: bool) {
    CHERI_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置 Hypervisor 模块启用状态
pub fn set_hypervisor_enabled(enabled: bool) {
    HYPERVISOR_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置 eBPF 模块启用状态
pub fn set_ebpf_enabled(enabled: bool) {
    EBPF_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置热补丁模块启用状态
pub fn set_livepatch_enabled(enabled: bool) {
    LIVEPATCH_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置 io_uring 模块启用状态
pub fn set_io_uring_enabled(enabled: bool) {
    IO_URING_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置网络模块启用状态
pub fn set_network_enabled(enabled: bool) {
    NETWORK_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置文件系统模块启用状态
pub fn set_filesystem_enabled(enabled: bool) {
    FILESYSTEM_MODULE_ENABLED.store(enabled, Ordering::Release);
}

/// 设置 SMP 多核支持启用状态
pub fn set_smp_enabled(enabled: bool) {
    SMP_MODULE_ENABLED.store(enabled, Ordering::Release);
}

// ============================================================================
// 配置初始化
// ============================================================================

/// 从全局常量初始化运行时配置
/// 
/// 在内核启动时调用，将编译时配置复制到运行时原子变量
pub fn init() {
    KERNEL_CONFIG.store_to_atomics();
}

impl KernelConfig {
    /// 将配置存储到全局原子变量（内部使用）
    fn store_to_atomics(&self) {
        NUM_CPUS.store(self.num_cpus, Ordering::Release);
        MEMORY_SIZE.store(self.memory_size, Ordering::Release);
        POWER_MODULE_ENABLED.store(self.power_enabled, Ordering::Release);
        SECURE_MODULE_ENABLED.store(self.secure_enabled, Ordering::Release);
        GDB_MODULE_ENABLED.store(self.gdb_enabled, Ordering::Release);
        FORMAL_MODULE_ENABLED.store(self.formal_enabled, Ordering::Release);
        CHERI_MODULE_ENABLED.store(self.cheri_enabled, Ordering::Release);
        HYPERVISOR_MODULE_ENABLED.store(self.hypervisor_enabled, Ordering::Release);
        EBPF_MODULE_ENABLED.store(self.ebpf_enabled, Ordering::Release);
        LIVEPATCH_MODULE_ENABLED.store(self.livepatch_enabled, Ordering::Release);
        IO_URING_MODULE_ENABLED.store(self.io_uring_enabled, Ordering::Release);
        NETWORK_MODULE_ENABLED.store(self.network_enabled, Ordering::Release);
        FILESYSTEM_MODULE_ENABLED.store(self.filesystem_enabled, Ordering::Release);
        SMP_MODULE_ENABLED.store(self.smp_enabled, Ordering::Release);
        DEBUG_LEVEL.store(self.debug_level, Ordering::Release);
    }
    
    /// 打印当前配置信息
    #[cfg(feature = "std")]
    pub fn print_config(&self) {
        println!("Kernel Configuration:");
        println!("  CPUs: {}", self.num_cpus);
        println!("  Memory: {} MB", self.memory_size / (1024 * 1024));
        println!("  Power Management: {}", self.power_enabled);
        println!("  Secure Boot: {}", self.secure_enabled);
        println!("  GDB Stub: {}", self.gdb_enabled);
        println!("  Formal Verification: {}", self.formal_enabled);
        println!("  CHERI: {}", self.cheri_enabled);
        println!("  Hypervisor: {}", self.hypervisor_enabled);
        println!("  eBPF: {}", self.ebpf_enabled);
        println!("  Live Patch: {}", self.livepatch_enabled);
        println!("  io_uring: {}", self.io_uring_enabled);
        println!("  Network: {}", self.network_enabled);
        println!("  Filesystem: {}", self.filesystem_enabled);
        println!("  SMP: {}", self.smp_enabled);
        println!("  Debug Level: {}", self.debug_level);
    }
}
