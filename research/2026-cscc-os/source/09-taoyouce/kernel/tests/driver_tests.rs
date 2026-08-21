//! 驱动隔离测试
//!
//! 验证用户态驱动框架的以下属性：
//! 1. 地址空间隔离：驱动不能访问内核内存
//! 2. 设备访问控制：只能访问授权的设备
//! 3. 中断处理：中断正确路由到驱动
//! 4. DMA 安全：DMA 缓冲区正确隔离
//! 5. 驱动崩溃恢复：驱动崩溃不影响系统

use crate::{SimpleRng, println};
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

/// 设备类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeviceType {
    Block,
    Network,
    Serial,
    Display,
}

/// 设备能力
#[derive(Debug, Clone)]
struct DeviceCap {
    device_type: DeviceType,
    mmio_base: usize,
    mmio_size: usize,
    irq: usize,
    dma_allowed: bool,
}

/// 驱动进程
struct DriverProcess {
    pid: usize,
    name: &'static str,
    caps: Vec<DeviceCap>,
    mapped_regions: Vec<(usize, usize)>, // (vaddr, size)
    pending_irqs: Vec<usize>,
    crashed: bool,
}

impl DriverProcess {
    fn new(pid: usize, name: &'static str) -> Self {
        Self {
            pid,
            name,
            caps: Vec::new(),
            mapped_regions: Vec::new(),
            pending_irqs: Vec::new(),
            crashed: false,
        }
    }
    
    fn grant_device(&mut self, cap: DeviceCap) {
        self.caps.push(cap);
    }
    
    fn has_device_access(&self, device_type: DeviceType) -> bool {
        self.caps.iter().any(|c| c.device_type == device_type)
    }
    
    fn map_mmio(&mut self, device_type: DeviceType, vaddr: usize) -> Result<(), &'static str> {
        if let Some(cap) = self.caps.iter().find(|c| c.device_type == device_type) {
            self.mapped_regions.push((vaddr, cap.mmio_size));
            Ok(())
        } else {
            Err("No capability for device")
        }
    }
    
    fn handle_irq(&mut self, irq: usize) -> bool {
        if self.caps.iter().any(|c| c.irq == irq) {
            self.pending_irqs.push(irq);
            true
        } else {
            false
        }
    }
    
    fn crash(&mut self) {
        self.crashed = true;
    }
    
    fn restart(&mut self) {
        self.crashed = false;
        self.pending_irqs.clear();
    }
}

/// 驱动管理器
struct DriverManager {
    drivers: BTreeMap<usize, DriverProcess>,
    next_pid: usize,
    kernel_memory: (usize, usize), // (start, end)
}

impl DriverManager {
    fn new() -> Self {
        Self {
            drivers: BTreeMap::new(),
            next_pid: 1,
            kernel_memory: (0xFFFF_0000_0000_0000, 0xFFFF_FFFF_FFFF_FFFF),
        }
    }
    
    fn spawn_driver(&mut self, name: &'static str) -> usize {
        let pid = self.next_pid;
        self.next_pid += 1;
        self.drivers.insert(pid, DriverProcess::new(pid, name));
        pid
    }
    
    fn grant_device(&mut self, pid: usize, cap: DeviceCap) -> bool {
        if let Some(driver) = self.drivers.get_mut(&pid) {
            driver.grant_device(cap);
            true
        } else {
            false
        }
    }
    
    fn is_kernel_address(&self, addr: usize) -> bool {
        addr >= self.kernel_memory.0 && addr < self.kernel_memory.1
    }
    
    fn validate_access(&self, pid: usize, addr: usize) -> bool {
        // 驱动不能访问内核内存
        if self.is_kernel_address(addr) {
            return false;
        }
        
        // 检查是否在映射区域内
        if let Some(driver) = self.drivers.get(&pid) {
            driver.mapped_regions.iter().any(|&(start, size)| {
                addr >= start && addr < start + size
            })
        } else {
            false
        }
    }
    
    fn dispatch_irq(&mut self, irq: usize) -> Vec<usize> {
        let mut handled_by = Vec::new();
        for (pid, driver) in &mut self.drivers {
            if !driver.crashed && driver.handle_irq(irq) {
                handled_by.push(*pid);
            }
        }
        handled_by
    }
    
    fn crash_driver(&mut self, pid: usize) {
        if let Some(driver) = self.drivers.get_mut(&pid) {
            driver.crash();
        }
    }
    
    fn restart_driver(&mut self, pid: usize) {
        if let Some(driver) = self.drivers.get_mut(&pid) {
            driver.restart();
        }
    }
    
    fn is_driver_healthy(&self, pid: usize) -> bool {
        self.drivers.get(&pid).map(|d| !d.crashed).unwrap_or(false)
    }
}

/// DMA 缓冲区管理器
struct DmaManager {
    buffers: BTreeMap<usize, DmaBuffer>,
    next_id: usize,
}

#[derive(Debug)]
struct DmaBuffer {
    id: usize,
    phys_addr: usize,
    size: usize,
    owner_pid: usize,
}

impl DmaManager {
    fn new() -> Self {
        Self {
            buffers: BTreeMap::new(),
            next_id: 0,
        }
    }
    
    fn allocate(&mut self, pid: usize, size: usize, phys_addr: usize) -> usize {
        let id = self.next_id;
        self.next_id += 1;
        self.buffers.insert(id, DmaBuffer {
            id,
            phys_addr,
            size,
            owner_pid: pid,
        });
        id
    }
    
    fn free(&mut self, id: usize, pid: usize) -> bool {
        if let Some(buf) = self.buffers.get(&id) {
            if buf.owner_pid == pid {
                self.buffers.remove(&id);
                return true;
            }
        }
        false
    }
    
    fn can_access(&self, id: usize, pid: usize) -> bool {
        self.buffers.get(&id).map(|b| b.owner_pid == pid).unwrap_or(false)
    }
}

/// 属性1：地址空间隔离
/// 驱动不能访问内核内存
#[test_case]
fn test_address_space_isolation() -> bool {
    let mut mgr = DriverManager::new();
    
    let pid = mgr.spawn_driver("test_driver");
    
    // 尝试访问内核地址
    let kernel_addr = 0xFFFF_8000_0000_0000;
    
    if mgr.validate_access(pid, kernel_addr) {
        println!("  错误: 驱动不应能访问内核内存");
        return false;
    }
    
    // 用户空间地址（未映射）也应失败
    let user_addr = 0x1000;
    if mgr.validate_access(pid, user_addr) {
        println!("  错误: 未映射地址不应可访问");
        return false;
    }
    
    true
}

/// 属性2：设备访问控制
/// 只能访问授权的设备
#[test_case]
fn test_device_access_control() -> bool {
    let mut mgr = DriverManager::new();
    
    let pid = mgr.spawn_driver("block_driver");
    
    // 授权块设备
    mgr.grant_device(pid, DeviceCap {
        device_type: DeviceType::Block,
        mmio_base: 0x1000_0000,
        mmio_size: 0x1000,
        irq: 10,
        dma_allowed: true,
    });
    
    let driver = mgr.drivers.get(&pid).unwrap();
    
    // 应有块设备访问权
    if !driver.has_device_access(DeviceType::Block) {
        println!("  错误: 应有块设备访问权");
        return false;
    }
    
    // 不应有网络设备访问权
    if driver.has_device_access(DeviceType::Network) {
        println!("  错误: 不应有网络设备访问权");
        return false;
    }
    
    true
}

/// 属性3：MMIO 映射
/// 只能映射授权设备的 MMIO
#[test_case]
fn test_mmio_mapping() -> bool {
    let mut mgr = DriverManager::new();
    
    let pid = mgr.spawn_driver("serial_driver");
    
    // 授权串口设备
    mgr.grant_device(pid, DeviceCap {
        device_type: DeviceType::Serial,
        mmio_base: 0x1000_0000,
        mmio_size: 0x100,
        irq: 4,
        dma_allowed: false,
    });
    
    let driver = mgr.drivers.get_mut(&pid).unwrap();
    
    // 映射授权设备应成功
    if driver.map_mmio(DeviceType::Serial, 0x2000_0000).is_err() {
        println!("  错误: 授权设备映射应成功");
        return false;
    }
    
    // 映射未授权设备应失败
    if driver.map_mmio(DeviceType::Network, 0x3000_0000).is_ok() {
        println!("  错误: 未授权设备映射应失败");
        return false;
    }
    
    true
}

/// 属性4：中断路由
/// 中断应正确路由到对应驱动
#[test_case]
fn test_irq_routing() -> bool {
    let mut mgr = DriverManager::new();
    
    let pid1 = mgr.spawn_driver("driver1");
    let pid2 = mgr.spawn_driver("driver2");
    
    // 授权不同中断
    mgr.grant_device(pid1, DeviceCap {
        device_type: DeviceType::Block,
        mmio_base: 0x1000_0000,
        mmio_size: 0x1000,
        irq: 10,
        dma_allowed: true,
    });
    
    mgr.grant_device(pid2, DeviceCap {
        device_type: DeviceType::Network,
        mmio_base: 0x2000_0000,
        mmio_size: 0x1000,
        irq: 11,
        dma_allowed: true,
    });
    
    // 分发 IRQ 10
    let handlers = mgr.dispatch_irq(10);
    
    if !handlers.contains(&pid1) {
        println!("  错误: driver1 应处理 IRQ 10");
        return false;
    }
    
    if handlers.contains(&pid2) {
        println!("  错误: driver2 不应处理 IRQ 10");
        return false;
    }
    
    // 分发 IRQ 11
    let handlers = mgr.dispatch_irq(11);
    
    if handlers.contains(&pid1) {
        println!("  错误: driver1 不应处理 IRQ 11");
        return false;
    }
    
    if !handlers.contains(&pid2) {
        println!("  错误: driver2 应处理 IRQ 11");
        return false;
    }
    
    true
}

/// 属性5：DMA 缓冲区隔离
/// DMA 缓冲区只能被所有者访问
#[test_case]
fn test_dma_isolation() -> bool {
    let mut dma = DmaManager::new();
    
    // 进程 1 分配 DMA 缓冲区
    let buf1 = dma.allocate(1, 4096, 0x1000_0000);
    
    // 进程 2 分配 DMA 缓冲区
    let buf2 = dma.allocate(2, 4096, 0x2000_0000);
    
    // 进程 1 应能访问 buf1
    if !dma.can_access(buf1, 1) {
        println!("  错误: 进程 1 应能访问 buf1");
        return false;
    }
    
    // 进程 1 不应能访问 buf2
    if dma.can_access(buf2, 1) {
        println!("  错误: 进程 1 不应能访问 buf2");
        return false;
    }
    
    // 进程 2 不应能释放 buf1
    if dma.free(buf1, 2) {
        println!("  错误: 进程 2 不应能释放 buf1");
        return false;
    }
    
    // 进程 1 应能释放 buf1
    if !dma.free(buf1, 1) {
        println!("  错误: 进程 1 应能释放 buf1");
        return false;
    }
    
    true
}

/// 属性6：驱动崩溃隔离
/// 驱动崩溃不应影响其他驱动
#[test_case]
fn test_driver_crash_isolation() -> bool {
    let mut mgr = DriverManager::new();
    
    let pid1 = mgr.spawn_driver("driver1");
    let pid2 = mgr.spawn_driver("driver2");
    
    // 授权设备
    mgr.grant_device(pid1, DeviceCap {
        device_type: DeviceType::Block,
        mmio_base: 0x1000_0000,
        mmio_size: 0x1000,
        irq: 10,
        dma_allowed: true,
    });
    
    mgr.grant_device(pid2, DeviceCap {
        device_type: DeviceType::Network,
        mmio_base: 0x2000_0000,
        mmio_size: 0x1000,
        irq: 11,
        dma_allowed: true,
    });
    
    // driver1 崩溃
    mgr.crash_driver(pid1);
    
    // driver1 应不健康
    if mgr.is_driver_healthy(pid1) {
        println!("  错误: driver1 应不健康");
        return false;
    }
    
    // driver2 应仍健康
    if !mgr.is_driver_healthy(pid2) {
        println!("  错误: driver2 应仍健康");
        return false;
    }
    
    // 崩溃的驱动不应处理中断
    let handlers = mgr.dispatch_irq(10);
    if handlers.contains(&pid1) {
        println!("  错误: 崩溃的驱动不应处理中断");
        return false;
    }
    
    true
}

/// 属性7：驱动重启
/// 崩溃的驱动应能重启
#[test_case]
fn test_driver_restart() -> bool {
    let mut mgr = DriverManager::new();
    
    let pid = mgr.spawn_driver("driver");
    
    mgr.grant_device(pid, DeviceCap {
        device_type: DeviceType::Block,
        mmio_base: 0x1000_0000,
        mmio_size: 0x1000,
        irq: 10,
        dma_allowed: true,
    });
    
    // 崩溃
    mgr.crash_driver(pid);
    
    if mgr.is_driver_healthy(pid) {
        println!("  错误: 崩溃后应不健康");
        return false;
    }
    
    // 重启
    mgr.restart_driver(pid);
    
    if !mgr.is_driver_healthy(pid) {
        println!("  错误: 重启后应健康");
        return false;
    }
    
    // 应能再次处理中断
    let handlers = mgr.dispatch_irq(10);
    if !handlers.contains(&pid) {
        println!("  错误: 重启后应能处理中断");
        return false;
    }
    
    true
}

/// 属性8：多驱动共存
/// 多个驱动应能同时运行
#[test_case]
fn test_multiple_drivers() -> bool {
    let mut mgr = DriverManager::new();
    
    let drivers: Vec<usize> = (0..5)
        .map(|i| {
            let pid = mgr.spawn_driver("driver");
            mgr.grant_device(pid, DeviceCap {
                device_type: DeviceType::Block,
                mmio_base: 0x1000_0000 + i * 0x1000,
                mmio_size: 0x1000,
                irq: 10 + i,
                dma_allowed: true,
            });
            pid
        })
        .collect();
    
    // 所有驱动应健康
    for &pid in &drivers {
        if !mgr.is_driver_healthy(pid) {
            println!("  错误: 驱动 {} 应健康", pid);
            return false;
        }
    }
    
    // 每个驱动应只处理自己的中断
    for (i, &pid) in drivers.iter().enumerate() {
        let handlers = mgr.dispatch_irq(10 + i);
        if handlers.len() != 1 || handlers[0] != pid {
            println!("  错误: IRQ {} 应只由驱动 {} 处理", 10 + i, pid);
            return false;
        }
    }
    
    true
}
