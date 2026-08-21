//! VirtIO 半虚拟化支持
//!
//! 实现 Guest 与 Host 之间的 VirtIO 半虚拟化通信

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use crate::sync::SpinLock;

/// VirtIO PV 设备类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VirtioPvDeviceType {
    /// 控制台
    Console = 0,
    /// 块设备
    Block = 1,
    /// 网络
    Network = 2,
    /// 文件系统
    Filesystem = 3,
    /// 9P 文件系统
    P9 = 4,
    /// VSOCK
    Vsock = 5,
}

/// VirtIO PV 请求类型
#[derive(Debug, Clone, Copy)]
#[repr(u32)]
pub enum VirtioPvReqType {
    /// 读取
    Read = 0,
    /// 写入
    Write = 1,
    /// 刷新
    Flush = 2,
    /// 获取 ID
    GetId = 3,
    /// 丢弃
    Discard = 4,
}

/// VirtIO PV 请求头
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioPvReqHeader {
    /// 请求类型
    pub req_type: u32,
    /// 设备 ID
    pub device_id: u32,
    /// 请求 ID
    pub request_id: u64,
    /// 偏移
    pub offset: u64,
    /// 长度
    pub length: u32,
    /// 标志
    pub flags: u32,
}

/// VirtIO PV 响应
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VirtioPvResponse {
    /// 请求 ID
    pub request_id: u64,
    /// 状态
    pub status: i32,
    /// 传输的字节数
    pub bytes_transferred: u32,
}

/// VirtIO PV 设备
pub struct VirtioPvDevice {
    /// 设备 ID
    pub device_id: u32,
    /// 设备类型
    pub device_type: VirtioPvDeviceType,
    /// 是否就绪
    pub ready: AtomicBool,
    /// 下一个请求 ID
    pub next_request_id: AtomicU32,
    /// 待处理请求
    pub pending_requests: SpinLock<BTreeMap<u64, VirtioPvReqHeader>>,
}

impl VirtioPvDevice {
    pub fn new(device_id: u32, device_type: VirtioPvDeviceType) -> Self {
        Self {
            device_id,
            device_type,
            ready: AtomicBool::new(false),
            next_request_id: AtomicU32::new(1),
            pending_requests: SpinLock::new(BTreeMap::new()),
        }
    }
    
    /// 分配请求 ID
    pub fn alloc_request_id(&self) -> u64 {
        self.next_request_id.fetch_add(1, Ordering::SeqCst) as u64
    }
    
    /// 提交请求
    pub fn submit_request(&self, header: VirtioPvReqHeader) {
        self.pending_requests.lock().insert(header.request_id, header);
    }
    
    /// 完成请求
    pub fn complete_request(&self, request_id: u64) -> Option<VirtioPvReqHeader> {
        self.pending_requests.lock().remove(&request_id)
    }
}

/// VirtIO PV 控制器
pub struct VirtioPvController {
    /// 设备列表
    devices: BTreeMap<u32, VirtioPvDevice>,
    /// 下一个设备 ID
    next_device_id: AtomicU32,
    /// 共享内存区域
    shared_memory: Option<SharedMemoryRegion>,
}

/// 共享内存区域
pub struct SharedMemoryRegion {
    /// 基地址
    pub base: usize,
    /// 大小
    pub size: usize,
    /// 请求队列偏移
    pub req_queue_offset: usize,
    /// 响应队列偏移
    pub resp_queue_offset: usize,
    /// 数据区偏移
    pub data_offset: usize,
}

impl VirtioPvController {
    pub fn new() -> Self {
        Self {
            devices: BTreeMap::new(),
            next_device_id: AtomicU32::new(1),
            shared_memory: None,
        }
    }
    
    /// 初始化共享内存
    pub fn init_shared_memory(&mut self, base: usize, size: usize) {
        let req_queue_offset = 0;
        let resp_queue_offset = size / 4;
        let data_offset = size / 2;
        
        self.shared_memory = Some(SharedMemoryRegion {
            base,
            size,
            req_queue_offset,
            resp_queue_offset,
            data_offset,
        });
    }
    
    /// 注册设备
    pub fn register_device(&mut self, device_type: VirtioPvDeviceType) -> u32 {
        let device_id = self.next_device_id.fetch_add(1, Ordering::SeqCst);
        let device = VirtioPvDevice::new(device_id, device_type);
        self.devices.insert(device_id, device);
        
        crate::println!("[virtio-pv] Registered {:?} device with ID {}", device_type, device_id);
        device_id
    }
    
    /// 获取设备
    pub fn get_device(&self, device_id: u32) -> Option<&VirtioPvDevice> {
        self.devices.get(&device_id)
    }
    
    /// 提交请求到 Host
    #[allow(dead_code)]
    pub fn submit_to_host(&self, device_id: u32, req_type: VirtioPvReqType, offset: u64, length: u32, data: Option<&[u8]>) -> Result<u64, &'static str> {
        let device = self.devices.get(&device_id).ok_or("Device not found")?;
        let shm = self.shared_memory.as_ref().ok_or("Shared memory not initialized")?;
        
        let request_id = device.alloc_request_id();
        
        let header = VirtioPvReqHeader {
            req_type: req_type as u32,
            device_id,
            request_id,
            offset,
            length,
            flags: 0,
        };
        
        // 写入请求到共享内存
        unsafe {
            let req_ptr = (shm.base + shm.req_queue_offset) as *mut VirtioPvReqHeader;
            core::ptr::write_volatile(req_ptr, header);
            
            // 如果有数据，写入数据区
            if let Some(data) = data {
                let data_ptr = (shm.base + shm.data_offset) as *mut u8;
                core::ptr::copy_nonoverlapping(data.as_ptr(), data_ptr, data.len());
            }
        }
        
        device.submit_request(header);
        
        // 触发 Hypercall 通知 Host
        self.notify_host(device_id);
        
        Ok(request_id)
    }
    
    /// 通知 Host
    #[allow(dead_code)]
    fn notify_host(&self, device_id: u32) {
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!(
                "li a7, 0x4B564D",
                "li a0, 0",
                "mv a1, {device_id}",
                "ecall",
                device_id = in(reg) device_id,
                out("a0") _,
                out("a7") _,
            );
        }
        #[cfg(not(target_arch = "riscv64"))]
        { let _ = device_id; }
    }
    
    /// 轮询响应
    pub fn poll_response(&self, device_id: u32) -> Option<VirtioPvResponse> {
        let shm = self.shared_memory.as_ref()?;
        
        unsafe {
            let resp_ptr = (shm.base + shm.resp_queue_offset) as *const VirtioPvResponse;
            let response = core::ptr::read_volatile(resp_ptr);
            
            if response.request_id != 0 {
                // 清除响应
                core::ptr::write_volatile(
                    (shm.base + shm.resp_queue_offset) as *mut u64,
                    0
                );
                
                // 完成请求
                if let Some(device) = self.devices.get(&device_id) {
                    device.complete_request(response.request_id);
                }
                
                return Some(response);
            }
        }
        
        None
    }
    
    /// 读取响应数据
    pub fn read_response_data(&self, buf: &mut [u8]) -> usize {
        let shm = match &self.shared_memory {
            Some(s) => s,
            None => return 0,
        };
        
        unsafe {
            let data_ptr = (shm.base + shm.data_offset) as *const u8;
            let len = buf.len();
            core::ptr::copy_nonoverlapping(data_ptr, buf.as_mut_ptr(), len);
            len
        }
    }
}

/// VirtIO PV 块设备接口
#[allow(dead_code)]
pub struct VirtioPvBlock {
    /// 控制器引用
    controller: *const VirtioPvController,
    /// 设备 ID
    device_id: u32,
    /// 扇区大小
    sector_size: u32,
    /// 容量（扇区数）
    capacity: u64,
}

impl VirtioPvBlock {
    #[allow(dead_code)]
    pub fn new(controller: &VirtioPvController, device_id: u32) -> Self {
        Self {
            controller: controller as *const _,
            device_id,
            sector_size: 512,
            capacity: 0,
        }
    }
    
    /// 读取扇区
    #[allow(dead_code)]
    pub fn read_sector(&self, sector: u64, buf: &mut [u8]) -> Result<(), &'static str> {
        let controller = unsafe { &*self.controller };
        
        let request_id = controller.submit_to_host(
            self.device_id,
            VirtioPvReqType::Read,
            sector * self.sector_size as u64,
            buf.len() as u32,
            None,
        )?;
        
        // 等待响应
        loop {
            if let Some(response) = controller.poll_response(self.device_id) {
                if response.request_id == request_id {
                    if response.status == 0 {
                        controller.read_response_data(buf);
                        return Ok(());
                    } else {
                        return Err("Read failed");
                    }
                }
            }
            core::hint::spin_loop();
        }
    }
    
    /// 写入扇区
    #[allow(dead_code)]
    pub fn write_sector(&self, sector: u64, buf: &[u8]) -> Result<(), &'static str> {
        let controller = unsafe { &*self.controller };
        
        let request_id = controller.submit_to_host(
            self.device_id,
            VirtioPvReqType::Write,
            sector * self.sector_size as u64,
            buf.len() as u32,
            Some(buf),
        )?;
        
        // 等待响应
        loop {
            if let Some(response) = controller.poll_response(self.device_id) {
                if response.request_id == request_id {
                    if response.status == 0 {
                        return Ok(());
                    } else {
                        return Err("Write failed");
                    }
                }
            }
            core::hint::spin_loop();
        }
    }
}

/// 全局 VirtIO PV 控制器
static VIRTIO_PV_CONTROLLER: SpinLock<Option<VirtioPvController>> = SpinLock::new(None);

/// 初始化 VirtIO 半虚拟化
pub fn init() {
    let controller = VirtioPvController::new();
    
    // 初始化共享内存（需要与 Host 协商）
    // controller.init_shared_memory(SHARED_MEM_BASE, SHARED_MEM_SIZE);
    
    *VIRTIO_PV_CONTROLLER.lock() = Some(controller);
    crate::println!("[virtio-pv] VirtIO paravirtualization initialized");
}

/// 注册 PV 设备
pub fn register_pv_device(device_type: VirtioPvDeviceType) -> Option<u32> {
    VIRTIO_PV_CONTROLLER.lock()
        .as_mut()
        .map(|c| c.register_device(device_type))
}


/// VirtIO PV 模块自测试
pub fn self_test() {
    crate::println!("[virtio-pv] Running self-test...");
    
    // 测试 VirtioPvDeviceType 枚举
    let device_types = [
        VirtioPvDeviceType::Console,
        VirtioPvDeviceType::Block,
        VirtioPvDeviceType::Network,
        VirtioPvDeviceType::Filesystem,
        VirtioPvDeviceType::P9,
        VirtioPvDeviceType::Vsock,
    ];
    for dt in &device_types {
        crate::println!("[virtio-pv] Device type: {:?} = {}", dt, *dt as u32);
    }
    
    // 测试 VirtioPvReqType 枚举
    let req_types = [
        VirtioPvReqType::Read,
        VirtioPvReqType::Write,
        VirtioPvReqType::Flush,
        VirtioPvReqType::GetId,
        VirtioPvReqType::Discard,
    ];
    for rt in &req_types {
        crate::println!("[virtio-pv] Request type: {:?} = {}", rt, *rt as u32);
    }
    
    // 测试 VirtioPvReqHeader
    let header = VirtioPvReqHeader {
        req_type: VirtioPvReqType::Read as u32,
        device_id: 1,
        request_id: 100,
        offset: 0,
        length: 512,
        flags: 0,
    };
    crate::println!("[virtio-pv] Header: {:?}", header);
    
    // 测试 VirtioPvResponse
    let response = VirtioPvResponse {
        request_id: 100,
        status: 0,
        bytes_transferred: 512,
    };
    crate::println!("[virtio-pv] Response: {:?}", response);
    
    // 测试 VirtioPvDevice
    let device = VirtioPvDevice::new(1, VirtioPvDeviceType::Block);
    assert_eq!(device.device_id, 1);
    assert_eq!(device.device_type, VirtioPvDeviceType::Block);
    let req_id = device.alloc_request_id();
    crate::println!("[virtio-pv] Allocated request ID: {}", req_id);
    device.submit_request(header);
    let _ = device.complete_request(header.request_id);
    
    // 测试 SharedMemoryRegion（使用栈上分配的内存进行测试）
    // 分配一个足够大的缓冲区来模拟共享内存
    let mut test_shared_mem = alloc::vec![0u8; 0x1000]; // 4KB 测试缓冲区
    let test_shm_base = test_shared_mem.as_mut_ptr() as usize;
    
    let shm = SharedMemoryRegion {
        base: test_shm_base,
        size: 0x1000,
        req_queue_offset: 0,
        resp_queue_offset: 0x400,  // 1KB 偏移
        data_offset: 0x800,        // 2KB 偏移
    };
    crate::println!("[virtio-pv] Shared memory: base={:#x}, size={:#x}", shm.base, shm.size);
    
    // 测试 VirtioPvController（使用真正分配的内存）
    let mut controller = VirtioPvController::new();
    controller.init_shared_memory(test_shm_base, 0x1000);
    let dev_id = controller.register_device(VirtioPvDeviceType::Console);
    crate::println!("[virtio-pv] Registered device ID: {}", dev_id);
    let _ = controller.get_device(dev_id);
    
    // 测试 poll_response（使用真正分配的内存，不会崩溃）
    let poll_result = controller.poll_response(dev_id);
    crate::println!("[virtio-pv] poll_response result: {:?}", poll_result.is_some());
    
    // 测试 read_response_data（使用真正分配的内存）
    let mut buf = [0u8; 64];
    let bytes_read = controller.read_response_data(&mut buf);
    crate::println!("[virtio-pv] read_response_data bytes: {}", bytes_read);
    
    // 测试 VirtioPvBlock（不实际执行 I/O）
    let block = VirtioPvBlock::new(&controller, dev_id);
    assert_eq!(block.device_id, dev_id);
    assert_eq!(block.sector_size, 512);
    let _ = block.capacity;
    // 注意：read_sector 和 write_sector 会阻塞等待响应，所以不测试
    
    // 测试全局函数
    init();
    let _ = register_pv_device(VirtioPvDeviceType::Network);
    
    crate::println!("[virtio-pv] Self-test passed!");
}
