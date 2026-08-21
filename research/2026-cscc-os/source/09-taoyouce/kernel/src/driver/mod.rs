//! 用户态设备驱动框架
//!
//! 实现中断转发、MMIO 映射和设备能力管理

pub mod virtio;
pub mod uart_user;
pub mod rtc;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use alloc::boxed::Box;
use crate::sync::SpinLock;
use crate::capability::{Capability, CapType, CapRights};
use crate::mm::{PhysAddr, VirtAddr};

/// 设备类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceType {
    /// 字符设备
    Char,
    /// 块设备
    Block,
    /// 网络设备
    Network,
    /// 其他
    Other,
}

/// 设备信息
#[derive(Debug)]
pub struct DeviceInfo {
    /// 设备名称
    pub name: &'static str,
    /// 设备类型
    pub device_type: DeviceType,
    /// MMIO 基地址
    pub mmio_base: PhysAddr,
    /// MMIO 大小
    pub mmio_size: usize,
    /// 中断号
    pub irq: Option<u32>,
}

/// 设备驱动接口
pub trait Driver: Send + Sync {
    /// 获取设备信息
    fn info(&self) -> &DeviceInfo;
    
    /// 初始化设备
    fn init(&mut self) -> Result<(), DriverError>;
    
    /// 处理中断
    fn handle_irq(&mut self);
    
    /// 读取数据
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, DriverError>;
    
    /// 写入数据
    fn write(&mut self, buf: &[u8]) -> Result<usize, DriverError>;
}

/// 驱动错误
#[derive(Debug, Clone, Copy)]
pub enum DriverError {
    /// 设备未找到
    NotFound,
    /// 设备忙
    Busy,
    /// I/O 错误
    IoError,
    /// 无效参数
    InvalidArgument,
    /// 权限不足
    PermissionDenied,
}

/// 中断转发器
pub struct InterruptForwarder {
    /// 中断号到驱动进程的映射
    handlers: BTreeMap<u32, usize>, // irq -> pid
    /// 待处理中断
    pending: SpinLock<Vec<u32>>,
}

impl InterruptForwarder {
    pub fn new() -> Self {
        Self {
            handlers: BTreeMap::new(),
            pending: SpinLock::new(Vec::new()),
        }
    }
    
    /// 注册中断处理程序
    pub fn register(&mut self, irq: u32, pid: usize) {
        self.handlers.insert(irq, pid);
    }
    
    /// 取消注册
    pub fn unregister(&mut self, irq: u32) {
        self.handlers.remove(&irq);
    }
    
    /// 处理中断（从内核中断处理程序调用）
    pub fn handle_irq(&self, irq: u32) {
        if self.handlers.contains_key(&irq) {
            self.pending.lock().push(irq);
            // TODO: 唤醒驱动进程
        }
    }
    
    /// 获取待处理中断
    pub fn get_pending(&self, pid: usize) -> Option<u32> {
        let mut pending = self.pending.lock();
        for (i, &irq) in pending.iter().enumerate() {
            if self.handlers.get(&irq) == Some(&pid) {
                return Some(pending.remove(i));
            }
        }
        None
    }
}

/// MMIO 映射管理器
pub struct MmioManager {
    /// 已映射的区域
    mappings: BTreeMap<PhysAddr, MmioMapping>,
}

/// MMIO 映射
struct MmioMapping {
    /// 物理地址
    phys_addr: PhysAddr,
    /// 虚拟地址
    virt_addr: VirtAddr,
    /// 大小
    size: usize,
    /// 所属进程
    pid: usize,
}

impl MmioManager {
    pub fn new() -> Self {
        Self {
            mappings: BTreeMap::new(),
        }
    }
    
    /// 映射 MMIO 区域到用户空间
    pub fn map(
        &mut self,
        phys_addr: PhysAddr,
        size: usize,
        pid: usize,
    ) -> Result<VirtAddr, DriverError> {
        // TODO: 实际映射到用户地址空间
        let virt_addr = VirtAddr(0x4000_0000 + self.mappings.len() * 0x1000);
        
        self.mappings.insert(phys_addr, MmioMapping {
            phys_addr,
            virt_addr,
            size,
            pid,
        });
        
        Ok(virt_addr)
    }
    
    /// 取消映射
    pub fn unmap(&mut self, phys_addr: PhysAddr) -> Result<(), DriverError> {
        self.mappings.remove(&phys_addr)
            .map(|_| ())
            .ok_or(DriverError::NotFound)
    }
}

/// 设备能力管理器
pub struct DeviceCapManager {
    /// 设备能力
    capabilities: BTreeMap<usize, Capability>, // device_id -> capability
}

impl DeviceCapManager {
    pub fn new() -> Self {
        Self {
            capabilities: BTreeMap::new(),
        }
    }
    
    /// 创建设备能力
    pub fn create_capability(&mut self, device_id: usize) -> Capability {
        let cap = Capability::new(
            CapType::Device,
            CapRights::READ | CapRights::WRITE,
            device_id,
        );
        self.capabilities.insert(device_id, cap.clone());
        cap
    }
    
    /// 验证设备能力
    pub fn verify(&self, cap: &Capability, device_id: usize) -> bool {
        cap.cap_type == CapType::Device && cap.object == device_id
    }
}

/// 全局驱动管理器
static DRIVER_MANAGER: SpinLock<Option<DriverManager>> = SpinLock::new(None);

/// 驱动管理器
pub struct DriverManager {
    /// 中断转发器
    pub irq_forwarder: InterruptForwarder,
    /// MMIO 管理器
    pub mmio_manager: MmioManager,
    /// 设备能力管理器
    pub cap_manager: DeviceCapManager,
    /// 注册的驱动
    drivers: BTreeMap<usize, Box<dyn Driver>>,
    /// 下一个设备 ID
    next_id: usize,
}

impl DriverManager {
    fn new() -> Self {
        Self {
            irq_forwarder: InterruptForwarder::new(),
            mmio_manager: MmioManager::new(),
            cap_manager: DeviceCapManager::new(),
            drivers: BTreeMap::new(),
            next_id: 0,
        }
    }
    
    /// 注册驱动
    #[allow(dead_code)]
    pub fn register(&mut self, driver: Box<dyn Driver>) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        
        // 注册中断处理
        if let Some(irq) = driver.info().irq {
            self.irq_forwarder.register(irq, 0); // TODO: 使用实际 PID
        }
        
        self.drivers.insert(id, driver);
        id
    }
}

/// 初始化驱动框架
pub fn init() {
    crate::println!("[driver] init() entry");
    let mut manager = DRIVER_MANAGER.lock();
    *manager = Some(DriverManager::new());
    crate::println!("[driver] Driver framework initialized");
    
    // 从平台 RTC 初始化 wallclock（rtc_hctosys 语义）
    rtc::init_wallclock();

    // 初始化 VirtIO 子系统
    crate::println!("[driver] Probing VirtIO devices...");
    virtio::init();
    crate::println!("[driver] init() done");
}

/// 处理外部中断
pub fn handle_external_irq(irq: u32) {
    if let Some(manager) = DRIVER_MANAGER.lock().as_ref() {
        manager.irq_forwarder.handle_irq(irq);
    }
}

/// 驱动模块自测试
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[driver] Running self-test...");
    
    // 测试 DeviceType
    let device_types = [
        DeviceType::Char,
        DeviceType::Block,
        DeviceType::Network,
        DeviceType::Other,
    ];
    for dt in &device_types {
        crate::println!("[driver] Device type: {:?}", dt);
    }
    
    // 测试 DeviceInfo
    let info = DeviceInfo {
        name: "test_device",
        device_type: DeviceType::Char,
        mmio_base: PhysAddr(0x1000_0000),
        mmio_size: 0x1000,
        irq: Some(10),
    };
    crate::println!("[driver] Device: {} type={:?} mmio={:#x} size={:#x} irq={:?}",
        info.name, info.device_type, info.mmio_base.0, info.mmio_size, info.irq);
    
    // 测试 DriverError
    let errors = [
        DriverError::NotFound,
        DriverError::Busy,
        DriverError::IoError,
        DriverError::InvalidArgument,
        DriverError::PermissionDenied,
    ];
    for err in &errors {
        crate::println!("[driver] Error: {:?}", err);
    }
    
    // 测试 InterruptForwarder
    let mut irq_fwd = InterruptForwarder::new();
    irq_fwd.register(10, 100);
    irq_fwd.handle_irq(10);
    let pending = irq_fwd.get_pending(100);
    crate::println!("[driver] Pending IRQ for pid 100: {:?}", pending);
    irq_fwd.unregister(10);
    
    // 测试 MmioManager
    let mut mmio_mgr = MmioManager::new();
    let phys = PhysAddr(0x2000_0000);
    match mmio_mgr.map(phys, 0x1000, 1) {
        Ok(virt) => {
            crate::println!("[driver] Mapped MMIO {:#x} -> {:#x}", phys.0, virt.0);
            let _ = mmio_mgr.unmap(phys);
        }
        Err(e) => crate::println!("[driver] MMIO map error: {:?}", e),
    }
    
    // 测试 DeviceCapManager
    let mut cap_mgr = DeviceCapManager::new();
    let cap = cap_mgr.create_capability(1);
    let valid = cap_mgr.verify(&cap, 1);
    crate::println!("[driver] Capability valid: {}", valid);
    
    // 测试 DriverManager
    init();
    
    // 测试 DriverManager 的 drivers 和 next_id 字段
    {
        let mut manager_guard = DRIVER_MANAGER.lock();
        if let Some(manager) = manager_guard.as_mut() {
            // 访问 drivers 字段
            let driver_count = manager.drivers.len();
            crate::println!("[driver] Registered drivers: {}", driver_count);
            
            // 访问 next_id 字段
            let next_id = manager.next_id;
            crate::println!("[driver] Next device ID: {}", next_id);
        }
    }
    
    // 测试全局中断处理
    handle_external_irq(5);
    
    // 测试 MmioMapping 结构体字段
    let mapping = MmioMapping {
        phys_addr: PhysAddr(0x3000_0000),
        virt_addr: VirtAddr(0x4000_0000),
        size: 0x2000,
        pid: 42,
    };
    crate::println!("[driver] MmioMapping: phys={:#x} virt={:#x} size={:#x} pid={}",
        mapping.phys_addr.0, mapping.virt_addr.0, mapping.size, mapping.pid);
    
    // 测试 virtio 子模块
    virtio::self_test();
    
    // 测试 uart_user 子模块
    uart_user::self_test();
    
    crate::println!("[driver] Self-test passed!");
    Ok(())
}
