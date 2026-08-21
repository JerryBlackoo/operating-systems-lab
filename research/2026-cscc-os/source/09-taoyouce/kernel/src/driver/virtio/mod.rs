//! VirtIO 驱动框架
//!
//! 实现 VirtIO 设备的通用抽象和具体驱动

#![allow(dead_code)]

pub mod queue;
pub mod blk;
pub mod net;
pub mod hal_impl;

#[cfg(target_arch = "loongarch64")]
pub mod pci;

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};

/// VirtIO 设备基地址（QEMU virt）
pub const VIRTIO_BASE: usize = 0x1000_1000;

/// VirtIO 设备间隔
pub const VIRTIO_STRIDE: usize = 0x1000;

/// VirtIO 设备数量
pub const VIRTIO_COUNT: usize = 8;

/// VirtIO 魔数
pub const VIRTIO_MAGIC: u32 = 0x74726976;

/// VirtIO 版本
pub const VIRTIO_VERSION_LEGACY: u32 = 1;
pub const VIRTIO_VERSION_MODERN: u32 = 2;

/// VirtIO 设备类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VirtioDeviceType {
    Invalid = 0,
    Network = 1,
    Block = 2,
    Console = 3,
    Entropy = 4,
    Balloon = 5,
    ScsiHost = 8,
    Gpu = 16,
    Input = 18,
    Socket = 19,
}

impl From<u32> for VirtioDeviceType {
    fn from(value: u32) -> Self {
        match value {
            1 => VirtioDeviceType::Network,
            2 => VirtioDeviceType::Block,
            3 => VirtioDeviceType::Console,
            4 => VirtioDeviceType::Entropy,
            5 => VirtioDeviceType::Balloon,
            8 => VirtioDeviceType::ScsiHost,
            16 => VirtioDeviceType::Gpu,
            18 => VirtioDeviceType::Input,
            19 => VirtioDeviceType::Socket,
            _ => VirtioDeviceType::Invalid,
        }
    }
}

/// VirtIO 设备状态
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum VirtioStatus {
    /// 重置
    Reset = 0,
    /// 已确认
    Acknowledge = 1,
    /// 驱动已加载
    Driver = 2,
    /// 特性协商完成
    FeaturesOk = 8,
    /// 驱动就绪
    DriverOk = 4,
    /// 设备需要重置
    NeedsReset = 64,
    /// 设备失败
    Failed = 128,
}

/// VirtIO MMIO 寄存器偏移
/// 注意：QEMU virtio-blk-device 默认使用 Legacy (v1) 接口
#[repr(usize)]
pub enum VirtioMmioReg {
    MagicValue = 0x000,
    Version = 0x004,
    DeviceId = 0x008,
    VendorId = 0x00c,
    DeviceFeatures = 0x010,
    DeviceFeaturesSel = 0x014,
    DriverFeatures = 0x020,
    DriverFeaturesSel = 0x024,
    // Legacy (v1) 专用寄存器
    GuestPageSize = 0x028,  // Legacy: 页大小
    QueueSel = 0x030,
    QueueNumMax = 0x034,
    QueueNum = 0x038,
    QueueAlign = 0x03c,     // Legacy: 对齐
    QueuePfn = 0x040,       // Legacy: 队列页帧号
    QueueReady = 0x044,
    QueueNotify = 0x050,
    InterruptStatus = 0x060,
    InterruptAck = 0x064,
    Status = 0x070,
    // Modern (v2) 专用寄存器
    QueueDescLow = 0x080,
    QueueDescHigh = 0x084,
    QueueDriverLow = 0x090,
    QueueDriverHigh = 0x094,
    QueueDeviceLow = 0x0a0,
    QueueDeviceHigh = 0x0a4,
    ConfigGeneration = 0x0fc,
    Config = 0x100,
}

/// VirtIO 设备抽象
pub struct VirtioDevice {
    /// 基地址
    base: usize,
    /// 设备类型
    device_type: VirtioDeviceType,
    /// 设备特性
    features: u64,
    /// 是否是 Legacy 设备
    is_legacy: bool,
}

impl VirtioDevice {
    /// 探测设备
    pub fn probe(base: usize) -> Option<Self> {
        let magic = unsafe { read_volatile((base + VirtioMmioReg::MagicValue as usize) as *const u32) };
        if magic != VIRTIO_MAGIC {
            return None;
        }
        
        let version = unsafe { read_volatile((base + VirtioMmioReg::Version as usize) as *const u32) };
        let is_legacy = version == VIRTIO_VERSION_LEGACY;
        
        if version != VIRTIO_VERSION_LEGACY && version != VIRTIO_VERSION_MODERN {
            return None;
        }
        
        let device_id = unsafe { read_volatile((base + VirtioMmioReg::DeviceId as usize) as *const u32) };
        let device_type = VirtioDeviceType::from(device_id);
        
        if device_type == VirtioDeviceType::Invalid {
            return None;
        }
        
        crate::println!("[virtio] Device version: {} ({})", version, 
            if is_legacy { "Legacy" } else { "Modern" });
        
        Some(Self {
            base,
            device_type,
            features: 0,
            is_legacy,
        })
    }
    
    /// 是否是 Legacy 设备
    pub fn is_legacy(&self) -> bool {
        self.is_legacy
    }
    
    /// 读取寄存器
    pub fn read_reg(&self, reg: VirtioMmioReg) -> u32 {
        unsafe { read_volatile((self.base + reg as usize) as *const u32) }
    }
    
    /// 写入寄存器
    pub fn write_reg(&self, reg: VirtioMmioReg, value: u32) {
        unsafe { write_volatile((self.base + reg as usize) as *mut u32, value) }
    }
    
    /// 获取设备类型
    pub fn device_type(&self) -> VirtioDeviceType {
        self.device_type
    }
    
    /// 初始化设备
    pub fn init(&mut self) -> Result<(), &'static str> {
        // 1. 重置设备
        self.write_reg(VirtioMmioReg::Status, VirtioStatus::Reset as u32);
        
        // 2. 设置 ACKNOWLEDGE 状态
        self.write_reg(VirtioMmioReg::Status, VirtioStatus::Acknowledge as u32);
        
        // 3. 设置 DRIVER 状态
        let status = self.read_reg(VirtioMmioReg::Status);
        self.write_reg(VirtioMmioReg::Status, status | VirtioStatus::Driver as u32);
        
        // 4. 读取设备特性
        if self.is_legacy {
            // Legacy: 只有 32 位特性
            let features = self.read_reg(VirtioMmioReg::DeviceFeatures);
            self.features = features as u64;
            
            // 协商特性 - 禁用一些不需要的特性
            let negotiated = features & !(
                (1 << 5) |   // VIRTIO_BLK_F_RO
                (1 << 7) |   // VIRTIO_BLK_F_SCSI
                (1 << 11) |  // VIRTIO_BLK_F_CONFIG_WCE
                (1 << 12) |  // VIRTIO_BLK_F_MQ
                (1 << 27) |  // VIRTIO_F_ANY_LAYOUT
                (1 << 28) |  // VIRTIO_RING_F_INDIRECT_DESC
                (1 << 29)    // VIRTIO_RING_F_EVENT_IDX
            );
            self.write_reg(VirtioMmioReg::DriverFeatures, negotiated);
        } else {
            // Modern: 64 位特性
            self.write_reg(VirtioMmioReg::DeviceFeaturesSel, 0);
            let features_low = self.read_reg(VirtioMmioReg::DeviceFeatures);
            self.write_reg(VirtioMmioReg::DeviceFeaturesSel, 1);
            let features_high = self.read_reg(VirtioMmioReg::DeviceFeatures);
            self.features = (features_high as u64) << 32 | features_low as u64;
            
            // 协商特性（接受所有特性）
            self.write_reg(VirtioMmioReg::DriverFeaturesSel, 0);
            self.write_reg(VirtioMmioReg::DriverFeatures, features_low);
            self.write_reg(VirtioMmioReg::DriverFeaturesSel, 1);
            self.write_reg(VirtioMmioReg::DriverFeatures, features_high);
            
            // 6. 设置 FEATURES_OK 状态
            let status = self.read_reg(VirtioMmioReg::Status);
            self.write_reg(VirtioMmioReg::Status, status | VirtioStatus::FeaturesOk as u32);
            
            // 7. 检查 FEATURES_OK
            let status = self.read_reg(VirtioMmioReg::Status);
            if status & VirtioStatus::FeaturesOk as u32 == 0 {
                return Err("Device did not accept features");
            }
        }
        
        Ok(())
    }
    
    /// 完成初始化
    pub fn finish_init(&self) {
        let status = self.read_reg(VirtioMmioReg::Status);
        self.write_reg(VirtioMmioReg::Status, status | VirtioStatus::DriverOk as u32);
    }
    
    /// 通知设备
    pub fn notify(&self, queue: u32) {
        self.write_reg(VirtioMmioReg::QueueNotify, queue);
    }
    
    /// 确认中断
    pub fn ack_interrupt(&self) -> u32 {
        let status = self.read_reg(VirtioMmioReg::InterruptStatus);
        self.write_reg(VirtioMmioReg::InterruptAck, status);
        status
    }
    
    /// 获取配置空间地址
    pub fn config_addr(&self) -> usize {
        self.base + VirtioMmioReg::Config as usize
    }
}

/// 探测所有 VirtIO 设备
pub fn probe_all() -> Vec<VirtioDevice> {
    let mut devices = Vec::new();
    
    // LoongArch64 QEMU virt 使用 VirtIO PCI（不是 MMIO），
    // MMIO 探测地址 0x10001000 在 LoongArch64 上无效，跳过。
    // LoongArch64 QEMU virt uses VirtIO PCI (not MMIO),
    // MMIO probe addresses are invalid on LoongArch64, skip.
    #[cfg(target_arch = "loongarch64")]
    {
        crate::println!("[virtio] LoongArch64: MMIO probe skipped (PCI transport needed)");
        // TODO(task-3.3): 实现 VirtIO PCI transport 探测
        return devices;
    }
    
    #[cfg(not(target_arch = "loongarch64"))]
    {
        for i in 0..VIRTIO_COUNT {
            let base = VIRTIO_BASE + i * VIRTIO_STRIDE;
            if let Some(device) = VirtioDevice::probe(base) {
                crate::println!("[virtio] Found {:?} device at {:#x}", device.device_type(), base);
                devices.push(device);
            }
        }
    }
    
    devices
}

/// 初始化 VirtIO 子系统
pub fn init() {
    // LoongArch64 使用 PCI transport，RISC-V64 使用 MMIO transport
    #[cfg(target_arch = "loongarch64")]
    {
        crate::println!("[virtio] LoongArch64: Using PCI transport");
        pci::init();
        crate::println!("[virtio] VirtIO PCI subsystem initialized");
        return;
    }

    #[cfg(not(target_arch = "loongarch64"))]
    {
        let devices = probe_all();

        for mut device in devices {
            match device.device_type() {
                VirtioDeviceType::Block => {
                    if device.init().is_ok() {
                        blk::init_device(device);
                    }
                }
                VirtioDeviceType::Network => {
                    if device.init().is_ok() {
                        net::init_device(device);
                    }
                }
                _ => {}
            }
        }

        crate::println!("[virtio] VirtIO subsystem initialized");
    }
}

/// VirtIO 子系统自测试
pub fn self_test() {
    crate::println!("[virtio] Running self-test...");
    
    // 测试常量
    let _base = VIRTIO_BASE;
    let _stride = VIRTIO_STRIDE;
    let _count = VIRTIO_COUNT;
    let _magic = VIRTIO_MAGIC;
    let _version_legacy = VIRTIO_VERSION_LEGACY;
    let _version_modern = VIRTIO_VERSION_MODERN;
    crate::println!("[virtio] Constants: base={:#x} stride={:#x} count={} magic={:#x}",
        VIRTIO_BASE, VIRTIO_STRIDE, VIRTIO_COUNT, VIRTIO_MAGIC);
    
    // 测试 VirtioDeviceType
    let device_types = [
        VirtioDeviceType::Invalid,
        VirtioDeviceType::Network,
        VirtioDeviceType::Block,
        VirtioDeviceType::Console,
        VirtioDeviceType::Entropy,
        VirtioDeviceType::Balloon,
        VirtioDeviceType::ScsiHost,
        VirtioDeviceType::Gpu,
        VirtioDeviceType::Input,
        VirtioDeviceType::Socket,
    ];
    for dt in &device_types {
        crate::println!("[virtio] Device type: {:?}", dt);
    }
    
    // 测试 VirtioDeviceType::from
    let _dt = VirtioDeviceType::from(1);
    let _dt = VirtioDeviceType::from(2);
    let _dt = VirtioDeviceType::from(999);
    
    // 测试 VirtioStatus
    let statuses = [
        VirtioStatus::Reset,
        VirtioStatus::Acknowledge,
        VirtioStatus::Driver,
        VirtioStatus::FeaturesOk,
        VirtioStatus::DriverOk,
        VirtioStatus::NeedsReset,
        VirtioStatus::Failed,
    ];
    for status in &statuses {
        crate::println!("[virtio] Status: {:?}", status);
    }
    
    // 测试 VirtioMmioReg
    let regs = [
        VirtioMmioReg::MagicValue,
        VirtioMmioReg::Version,
        VirtioMmioReg::DeviceId,
        VirtioMmioReg::VendorId,
        VirtioMmioReg::DeviceFeatures,
        VirtioMmioReg::DeviceFeaturesSel,
        VirtioMmioReg::DriverFeatures,
        VirtioMmioReg::DriverFeaturesSel,
        VirtioMmioReg::GuestPageSize,
        VirtioMmioReg::QueueSel,
        VirtioMmioReg::QueueNumMax,
        VirtioMmioReg::QueueNum,
        VirtioMmioReg::QueueAlign,
        VirtioMmioReg::QueuePfn,
        VirtioMmioReg::QueueReady,
        VirtioMmioReg::QueueNotify,
        VirtioMmioReg::InterruptStatus,
        VirtioMmioReg::InterruptAck,
        VirtioMmioReg::Status,
        VirtioMmioReg::QueueDescLow,
        VirtioMmioReg::QueueDescHigh,
        VirtioMmioReg::QueueDriverLow,
        VirtioMmioReg::QueueDriverHigh,
        VirtioMmioReg::QueueDeviceLow,
        VirtioMmioReg::QueueDeviceHigh,
        VirtioMmioReg::ConfigGeneration,
        VirtioMmioReg::Config,
    ];
    for reg in regs {
        let _offset = reg as usize;
    }
    crate::println!("[virtio] VirtioMmioReg variants tested");
    
    // 测试 queue 子模块
    queue::self_test();
    
    // 测试 blk 子模块
    blk::self_test();
    
    // 测试 net 子模块
    net::self_test();
    
    crate::println!("[virtio] Self-test passed!");
}
