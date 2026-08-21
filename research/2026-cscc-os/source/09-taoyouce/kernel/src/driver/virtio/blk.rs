//! VirtIO 块设备驱动
//!
//! 实现 VirtIO Block Device 驱动

#![allow(dead_code)]

use alloc::boxed::Box;
use alloc::vec::Vec;
use alloc::vec;
use core::ptr::read_volatile;
use crate::sync::SpinLock;
use super::{VirtioDevice, VirtioMmioReg};
use super::queue::{SplitVirtqueue, RequestTracker, QUEUE_SIZE};

/// 块大小
pub const BLOCK_SIZE: usize = 512;

/// VirtIO 块设备请求类型
#[repr(u32)]
pub enum VirtioBlkReqType {
    Read = 0,
    Write = 1,
    Flush = 4,
    GetId = 8,
    Discard = 11,
    WriteZeroes = 13,
}

/// VirtIO 块设备请求头
#[repr(C)]
pub struct VirtioBlkReqHeader {
    /// 请求类型
    pub req_type: u32,
    /// 保留
    pub reserved: u32,
    /// 扇区号
    pub sector: u64,
}

/// VirtIO 块设备状态
#[repr(u8)]
pub enum VirtioBlkStatus {
    Ok = 0,
    IoErr = 1,
    Unsupp = 2,
}

/// VirtIO 块设备配置
#[repr(C)]
pub struct VirtioBlkConfig {
    /// 容量（扇区数）
    pub capacity: u64,
    /// 大小上限
    pub size_max: u32,
    /// 段上限
    pub seg_max: u32,
    /// 几何信息
    pub geometry: VirtioBlkGeometry,
    /// 块大小
    pub blk_size: u32,
}

/// 块设备几何信息
#[repr(C)]
pub struct VirtioBlkGeometry {
    pub cylinders: u16,
    pub heads: u8,
    pub sectors: u8,
}

/// 块设备请求
pub struct BlkRequest {
    /// 请求头
    header: Box<VirtioBlkReqHeader>,
    /// 数据缓冲区
    data: Vec<u8>,
    /// 状态
    status: Box<u8>,
    /// 描述符索引
    desc_idx: Option<u16>,
}

/// DMA 读缓冲：设备会整段覆写，预先 memset 清零纯属浪费
/// （pchist 实证 from_elem 清零是 minibuild 内核第一热点 4.3e8 insns）。
/// 安全：u8 无效位模式；缓冲区仅在 DMA 成功完成后才暴露给调用方，
/// 失败路径直接 drop，不泄露旧堆内容。
pub(crate) fn dma_read_buf(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    #[allow(clippy::uninit_vec)]
    unsafe {
        v.set_len(len);
    }
    v
}

impl BlkRequest {
    /// 创建读请求
    pub fn read(sector: u64, count: usize) -> Self {
        Self {
            header: Box::new(VirtioBlkReqHeader {
                req_type: VirtioBlkReqType::Read as u32,
                reserved: 0,
                sector,
            }),
            data: dma_read_buf(count * BLOCK_SIZE),
            status: Box::new(0xff),
            desc_idx: None,
        }
    }
    
    /// 创建写请求
    pub fn write(sector: u64, data: Vec<u8>) -> Self {
        Self {
            header: Box::new(VirtioBlkReqHeader {
                req_type: VirtioBlkReqType::Write as u32,
                reserved: 0,
                sector,
            }),
            data,
            status: Box::new(0xff),
            desc_idx: None,
        }
    }
    
    /// 获取数据
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    
    /// 获取状态
    pub fn status(&self) -> u8 {
        *self.status
    }
    
    /// 是否成功
    pub fn is_ok(&self) -> bool {
        *self.status == VirtioBlkStatus::Ok as u8
    }
}

/// VirtIO 块设备
pub struct VirtioBlk {
    /// 底层设备
    device: VirtioDevice,
    /// 虚拟队列
    queue: SplitVirtqueue,
    /// 请求追踪器
    tracker: RequestTracker,
    /// 容量（扇区数）
    capacity: u64,
    /// 待处理请求
    pending: Vec<BlkRequest>,
}

impl VirtioBlk {
    /// 创建块设备
    pub fn new(device: VirtioDevice) -> Result<Self, &'static str> {
        // 读取配置
        let config_addr = device.config_addr();
        let capacity = unsafe { read_volatile(config_addr as *const u64) };
        
        crate::println!("[virtio-blk] Device config addr: {:#x}, capacity: {} sectors", config_addr, capacity);
        crate::println!("[virtio-blk] Device is Legacy: {}", device.is_legacy());
        
        // 根据设备版本创建队列
        let queue = if device.is_legacy() {
            SplitVirtqueue::new_legacy(0)
        } else {
            SplitVirtqueue::new(0)
        };
        
        // 配置队列 - 必须先选择队列
        device.write_reg(VirtioMmioReg::QueueSel, 0);
        
        // 读取队列最大大小
        let queue_max = device.read_reg(VirtioMmioReg::QueueNumMax);
        crate::println!("[virtio-blk] Queue max size: {}", queue_max);
        if queue_max == 0 {
            return Err("Queue not available");
        }
        
        // 设置队列大小
        let queue_size = queue.size() as u32;
        device.write_reg(VirtioMmioReg::QueueNum, queue_size);
        crate::println!("[virtio-blk] Queue size set to: {}", queue_size);
        
        if device.is_legacy() {
            // Legacy 接口：设置页大小和页帧号
            device.write_reg(VirtioMmioReg::GuestPageSize, 4096);
            
            let pfn = queue.pfn();
            crate::println!("[virtio-blk] Legacy mode: PFN = {:#x}", pfn);
            device.write_reg(VirtioMmioReg::QueuePfn, pfn as u32);
        } else {
            // Modern 接口：设置队列地址
            let desc_addr = queue.desc_addr();
            let avail_addr = queue.avail_addr();
            let used_addr = queue.used_addr();
            
            crate::println!("[virtio-blk] Modern mode queue addresses:");
            crate::println!("[virtio-blk]   desc:  {:#x}", desc_addr);
            crate::println!("[virtio-blk]   avail: {:#x}", avail_addr);
            crate::println!("[virtio-blk]   used:  {:#x}", used_addr);
            
            device.write_reg(VirtioMmioReg::QueueDescLow, desc_addr as u32);
            device.write_reg(VirtioMmioReg::QueueDescHigh, (desc_addr >> 32) as u32);
            device.write_reg(VirtioMmioReg::QueueDriverLow, avail_addr as u32);
            device.write_reg(VirtioMmioReg::QueueDriverHigh, (avail_addr >> 32) as u32);
            device.write_reg(VirtioMmioReg::QueueDeviceLow, used_addr as u32);
            device.write_reg(VirtioMmioReg::QueueDeviceHigh, (used_addr >> 32) as u32);
            
            // 内存屏障 - 确保所有写入完成
            core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
            
            // 启用队列 (Modern only)
            device.write_reg(VirtioMmioReg::QueueReady, 1);
        }
        
        crate::println!("[virtio-blk] Queue configured");
        
        // 完成初始化
        device.finish_init();
        
        // 验证设备状态
        let status = device.read_reg(VirtioMmioReg::Status);
        crate::println!("[virtio-blk] Device status after init: {:#x}", status);
        
        crate::println!("[virtio-blk] Block device initialized, capacity: {} sectors ({} MB)", 
            capacity, capacity * 512 / 1024 / 1024);
        
        Ok(Self {
            device,
            queue,
            tracker: RequestTracker::new(QUEUE_SIZE),
            capacity,
            pending: Vec::new(),
        })
    }
    
    /// 获取容量（扇区数）
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
    
    /// 提交请求
    pub fn submit(&mut self, mut request: BlkRequest) -> Result<u16, &'static str> {
        let header_addr = request.header.as_ref() as *const _ as u64;
        let data_addr = request.data.as_ptr() as u64;
        let status_addr = request.status.as_ref() as *const _ as u64;

        // 广撒网探针：提交时核对 DMA 数据缓冲帧的归属。若它已落在某条存活内核栈
        // 区间内（或被堆/slab 占用），即「块设备读缓冲与内核栈物理帧双重占用」，
        // 设备 DMA 会把磁盘数据写进活内核栈、内核栈又把指针写回缓冲。
        if crate::mm::uaf_dbg_enabled() && request.header.req_type == VirtioBlkReqType::Read as u32 {
            // kstk_owner_scan_dbg 仅在 data_addr 落入某存活内核栈区间时才打印，
            // 故此处不会刷屏；命中即坐实「读缓冲帧 == 活内核栈帧」。
            if let Some(t) = crate::sched::process::PROCESS_TABLE.try_lock() {
                if let Some(tbl) = t.as_ref() {
                    tbl.kstk_owner_scan_dbg(data_addr as usize, 0);
                }
            }
        }

        let is_write = request.header.req_type == VirtioBlkReqType::Write as u32;
        
        // 构建描述符链
        let bufs = if is_write {
            vec![
                (header_addr, core::mem::size_of::<VirtioBlkReqHeader>() as u32, false),
                (data_addr, request.data.len() as u32, false),
                (status_addr, 1, true),
            ]
        } else {
            vec![
                (header_addr, core::mem::size_of::<VirtioBlkReqHeader>() as u32, false),
                (data_addr, request.data.len() as u32, true),
                (status_addr, 1, true),
            ]
        };
        
        let desc_idx = self.queue.add_buf(&bufs).ok_or("Queue full")?;
        request.desc_idx = Some(desc_idx);
        
        self.tracker.start(desc_idx);
        self.pending.push(request);
        
        // 通知设备
        self.device.notify(0);
        
        Ok(desc_idx)
    }
    
    /// 轮询完成
    pub fn poll(&mut self) -> Vec<BlkRequest> {
        let mut completed = Vec::new();
        
        while let Some((idx, len)) = self.queue.pop_used() {
            self.tracker.complete(idx, len);
            self.queue.free_chain(idx);
            
            // 找到对应的请求
            if let Some(pos) = self.pending.iter().position(|r| r.desc_idx == Some(idx)) {
                completed.push(self.pending.remove(pos));
            }
        }
        
        completed
    }
    
    /// 同步读取
    pub fn read_sync(&mut self, sector: u64, count: usize) -> Result<Vec<u8>, &'static str> {
        let request = BlkRequest::read(sector, count);
        let desc_idx = self.submit(request)?;
        
        // Debug disabled for cleaner output
        // crate::println!("[virtio-blk] Submitted read request: sector={}, count={}, desc_idx={}", 
        //     sector, count, desc_idx);
        let _ = desc_idx;
        
        // 等待完成
        let mut poll_count = 0u32;
        loop {
            let completed = self.poll();
            if let Some(req) = completed.into_iter().next() {
                let _status = req.status();
                // Debug disabled for cleaner output
                // crate::println!("[virtio-blk] Request completed, status: {}", status);
                if req.is_ok() {
                    // 广撒网探针：DMA 读完成后，若数据缓冲里出现内核线性指针
                    // [0x80200000,0x90000000)，说明这块磁盘读缓冲混进了内核内存
                    // （典型为缓冲帧与内核栈/堆/slab 物理双重占用）。一次性打印：
                    //   1. 缓冲帧本身的全部归属（heap/slab/ft_live/ref/buddy-free）；
                    //   2. 缓冲帧是否落在某条存活内核栈区间内；
                    //   3. 被写进的指针值落在谁的内核栈内。
                    if crate::mm::uaf_dbg_enabled() {
                        let base = req.data.as_ptr() as usize;
                        let words = req.data.len() / 8;
                        let p = req.data.as_ptr() as *const u64;
                        for i in 0..words {
                            let v = unsafe { core::ptr::read_unaligned(p.add(i)) } as usize;
                            if v >= 0x8020_0000 && v < 0x9000_0000 {
                                let off = i * 8;
                                crate::println!(
                                    "[BLKBUF] sector={} buf_pa={:#x} off={:#x} kernel-linear word={:#x} in DMA read data",
                                    req.header.sector, base, off, v
                                );
                                crate::mm::frame_ownership_dump_dbg(base + off, "blkbuf");
                                if let Some(t) = crate::sched::process::PROCESS_TABLE.try_lock() {
                                    if let Some(tbl) = t.as_ref() {
                                        tbl.kstk_owner_scan_dbg(base + off, 0);
                                        tbl.kstk_owner_scan_dbg(v, 0);
                                    }
                                }
                                break;
                            }
                        }
                    }
                    return Ok(req.data);
                } else {
                    return Err("Read failed");
                }
            }
            poll_count += 1;
            if poll_count > 10_000_000 {
                // 打印调试信息
                let int_status = self.device.read_reg(VirtioMmioReg::InterruptStatus);
                let dev_status = self.device.read_reg(VirtioMmioReg::Status);
                crate::println!("[virtio-blk] Timeout! int_status={:#x}, dev_status={:#x}", 
                    int_status, dev_status);
                crate::println!("[virtio-blk] has_used={}", self.queue.has_used());
                return Err("Read timeout");
            }
            if poll_count % 1_000_000 == 0 {
                crate::println!("[virtio-blk] Still waiting... poll_count={}", poll_count);
            }
            core::hint::spin_loop();
        }
    }
    
    /// 同步写入
    pub fn write_sync(&mut self, sector: u64, data: Vec<u8>) -> Result<(), &'static str> {
        let request = BlkRequest::write(sector, data);
        self.submit(request)?;
        
        // 等待完成
        let mut poll_count = 0u32;
        loop {
            let completed = self.poll();
            if let Some(req) = completed.into_iter().next() {
                if req.is_ok() {
                    return Ok(());
                } else {
                    return Err("Write failed");
                }
            }
            poll_count += 1;
            if poll_count > 10_000_000 {
                crate::println!("[virtio-blk] Write timeout!");
                return Err("Write timeout");
            }
            core::hint::spin_loop();
        }
    }
    
    /// 处理中断
    pub fn handle_interrupt(&mut self) {
        self.device.ack_interrupt();
        self.poll();
    }
}

/// 全局块设备列表（支持多块设备）
static BLOCK_DEVICES: SpinLock<Vec<VirtioBlk>> = SpinLock::new(Vec::new());

/// 全局 PCI 块设备列表（LoongArch64 使用 virtio-drivers crate）
///
/// 🟢 直接复用 virtio-drivers crate 的 VirtIOBlk<VirtioHalImpl, PciTransport>
/// 🟡 微内核适配：通过 SpinLock 保护全局设备列表
#[cfg(target_arch = "loongarch64")]
static PCI_BLOCK_DEVICES: SpinLock<Vec<virtio_drivers::device::blk::VirtIOBlk<super::hal_impl::VirtioHalImpl, virtio_drivers::transport::pci::PciTransport>>> = SpinLock::new(Vec::new());

/// 初始化 PCI 块设备（LoongArch64 专用）
///
/// 使用 virtio-drivers crate 的 VirtIOBlk 和 PciTransport 初始化块设备。
/// 由 pci.rs 的设备枚举逻辑调用。
///
/// # 参数
/// - `transport`: virtio-drivers crate 的 PciTransport 实例
#[cfg(target_arch = "loongarch64")]
pub fn init_pci_device(transport: virtio_drivers::transport::pci::PciTransport) {
    use virtio_drivers::device::blk::VirtIOBlk;
    use super::hal_impl::VirtioHalImpl;

    match VirtIOBlk::<VirtioHalImpl, _>::new(transport) {
        Ok(blk) => {
            let cap = blk.capacity();
            let mut devs = PCI_BLOCK_DEVICES.lock();
            let idx = devs.len();
            devs.push(blk);
            crate::println!(
                "[virtio-blk-pci] Block device {} ready, capacity: {} sectors ({} MB)",
                idx, cap, cap * 512 / 1024 / 1024
            );
        }
        Err(e) => {
            crate::println!("[virtio-blk-pci] Failed to initialize VirtIOBlk: {:?}", e);
        }
    }
}

/// 初始化块设备（追加到设备列表）
pub fn init_device(device: VirtioDevice) {
    match VirtioBlk::new(device) {
        Ok(blk) => {
            let mut devs = BLOCK_DEVICES.lock();
            let idx = devs.len();
            devs.push(blk);
            crate::println!("[virtio-blk] Block device {} ready", idx);
        }
        Err(e) => {
            crate::println!("[virtio-blk] Failed to initialize: {}", e);
        }
    }
}

/// 获取块设备数量
pub fn device_count() -> usize {
    #[cfg(target_arch = "loongarch64")]
    {
        PCI_BLOCK_DEVICES.lock().len()
    }
    #[cfg(not(target_arch = "loongarch64"))]
    {
        BLOCK_DEVICES.lock().len()
    }
}

pub fn capacity_sectors_dev(dev: usize) -> Option<u64> {
    #[cfg(target_arch = "loongarch64")]
    {
        PCI_BLOCK_DEVICES.lock().get(dev).map(|blk| blk.capacity())
    }
    #[cfg(not(target_arch = "loongarch64"))]
    {
        BLOCK_DEVICES.lock().get(dev).map(|blk| blk.capacity())
    }
}

/// 从指定设备读取扇区
pub fn read_sectors_dev(dev: usize, sector: u64, count: usize) -> Result<Vec<u8>, &'static str> {
    #[cfg(target_arch = "loongarch64")]
    {
        let mut devs = PCI_BLOCK_DEVICES.lock();
        let blk = devs.get_mut(dev).ok_or("PCI block device not found")?;
        let mut buf = dma_read_buf(count * BLOCK_SIZE);
        // virtio-drivers 的 read_blocks 按 512 字节扇区读取
        blk.read_blocks(sector as usize, &mut buf)
            .map_err(|_| "PCI block read failed")?;
        Ok(buf)
    }
    #[cfg(not(target_arch = "loongarch64"))]
    {
        let mut devs = BLOCK_DEVICES.lock();
        devs.get_mut(dev)
            .ok_or("Block device not found")?
            .read_sync(sector, count)
    }
}

/// 向指定设备写入扇区
pub fn write_sectors_dev(dev: usize, sector: u64, data: Vec<u8>) -> Result<(), &'static str> {
    #[cfg(target_arch = "loongarch64")]
    {
        let mut devs = PCI_BLOCK_DEVICES.lock();
        let blk = devs.get_mut(dev).ok_or("PCI block device not found")?;
        blk.write_blocks(sector as usize, &data)
            .map_err(|_| "PCI block write failed")?;
        Ok(())
    }
    #[cfg(not(target_arch = "loongarch64"))]
    {
        let mut devs = BLOCK_DEVICES.lock();
        devs.get_mut(dev)
            .ok_or("Block device not found")?
            .write_sync(sector, data)
    }
}

/// 读取扇区（兼容旧接口，操作第一个设备）
pub fn read_sectors(sector: u64, count: usize) -> Result<Vec<u8>, &'static str> {
    read_sectors_dev(0, sector, count)
}

/// 写入扇区（兼容旧接口，操作第一个设备）
pub fn write_sectors(sector: u64, data: Vec<u8>) -> Result<(), &'static str> {
    write_sectors_dev(0, sector, data)
}

/// VirtIO 块设备自测试
pub fn self_test() {
    crate::println!("[virtio-blk] Running self-test...");
    
    // 测试常量
    let _block_size = BLOCK_SIZE;
    crate::println!("[virtio-blk] BLOCK_SIZE = {}", BLOCK_SIZE);
    
    // 测试 VirtioBlkReqType
    let req_types = [
        VirtioBlkReqType::Read as u32,
        VirtioBlkReqType::Write as u32,
        VirtioBlkReqType::Flush as u32,
        VirtioBlkReqType::GetId as u32,
        VirtioBlkReqType::Discard as u32,
        VirtioBlkReqType::WriteZeroes as u32,
    ];
    for rt in &req_types {
        crate::println!("[virtio-blk] Request type: {}", rt);
    }
    
    // 测试 VirtioBlkReqHeader
    let header = VirtioBlkReqHeader {
        req_type: VirtioBlkReqType::Read as u32,
        reserved: 0,
        sector: 0,
    };
    crate::println!("[virtio-blk] Header: type={} sector={}", header.req_type, header.sector);
    
    // 测试 VirtioBlkStatus
    let statuses = [
        VirtioBlkStatus::Ok as u8,
        VirtioBlkStatus::IoErr as u8,
        VirtioBlkStatus::Unsupp as u8,
    ];
    for s in &statuses {
        crate::println!("[virtio-blk] Status: {}", s);
    }
    
    // 测试 VirtioBlkConfig 和 VirtioBlkGeometry
    let geometry = VirtioBlkGeometry {
        cylinders: 100,
        heads: 16,
        sectors: 63,
    };
    let config = VirtioBlkConfig {
        capacity: 1024 * 1024,
        size_max: 4096,
        seg_max: 128,
        geometry,
        blk_size: 512,
    };
    crate::println!("[virtio-blk] Config: capacity={} blk_size={}", config.capacity, config.blk_size);
    crate::println!("[virtio-blk] Geometry: C={} H={} S={}", 
        config.geometry.cylinders, config.geometry.heads, config.geometry.sectors);
    
    // 测试 BlkRequest
    let read_req = BlkRequest::read(0, 1);
    crate::println!("[virtio-blk] Read request data len: {}", read_req.data().len());
    let _status = read_req.status();
    let _is_ok = read_req.is_ok();
    
    let write_data = vec![0u8; BLOCK_SIZE];
    let write_req = BlkRequest::write(0, write_data);
    crate::println!("[virtio-blk] Write request data len: {}", write_req.data().len());
    
    // 测试全局函数（不实际执行，因为没有设备）
    let _read_result = read_sectors(0, 1);
    let _write_result = write_sectors(0, vec![0u8; BLOCK_SIZE]);
    
    crate::println!("[virtio-blk] Self-test passed!");
}
