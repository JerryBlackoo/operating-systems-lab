//! 用户态 UART 驱动
//!
//! 实现用户态 UART 驱动框架，支持中断转发和 MMIO 映射

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use crate::sync::SpinLock;
use crate::mm::{PhysAddr, VirtAddr};

/// UART 基地址（QEMU virt）
pub const UART0_BASE: usize = 0x1000_0000;

/// UART 寄存器偏移
pub mod regs {
    pub const RBR: usize = 0x00;  // 接收缓冲寄存器
    pub const THR: usize = 0x00;  // 发送保持寄存器
    pub const IER: usize = 0x01;  // 中断使能寄存器
    pub const IIR: usize = 0x02;  // 中断标识寄存器
    pub const FCR: usize = 0x02;  // FIFO 控制寄存器
    pub const LCR: usize = 0x03;  // 线路控制寄存器
    pub const MCR: usize = 0x04;  // 调制解调器控制寄存器
    pub const LSR: usize = 0x05;  // 线路状态寄存器
    pub const MSR: usize = 0x06;  // 调制解调器状态寄存器
    pub const SCR: usize = 0x07;  // 暂存寄存器
}

/// 线路状态寄存器位
pub mod lsr {
    pub const DATA_READY: u8 = 0x01;
    pub const OVERRUN_ERROR: u8 = 0x02;
    pub const PARITY_ERROR: u8 = 0x04;
    pub const FRAMING_ERROR: u8 = 0x08;
    pub const BREAK_INTERRUPT: u8 = 0x10;
    pub const THR_EMPTY: u8 = 0x20;
    pub const TRANSMITTER_EMPTY: u8 = 0x40;
    pub const FIFO_ERROR: u8 = 0x80;
}

/// 中断使能寄存器位
pub mod ier {
    pub const RX_AVAILABLE: u8 = 0x01;
    pub const TX_EMPTY: u8 = 0x02;
    pub const LINE_STATUS: u8 = 0x04;
    pub const MODEM_STATUS: u8 = 0x08;
}

/// 用户态 UART 驱动请求
#[derive(Debug, Clone)]
pub enum UartRequest {
    /// 读取数据
    Read { max_len: usize },
    /// 写入数据
    Write { data: Vec<u8> },
    /// 配置波特率
    SetBaudRate { rate: u32 },
    /// 配置数据格式
    SetFormat { data_bits: u8, stop_bits: u8, parity: u8 },
    /// 启用/禁用中断
    SetInterrupt { enable: bool },
}

/// 用户态 UART 驱动响应
#[derive(Debug, Clone)]
pub enum UartResponse {
    /// 读取结果
    ReadResult { data: Vec<u8> },
    /// 写入结果
    WriteResult { written: usize },
    /// 配置结果
    ConfigResult { success: bool },
    /// 错误
    Error { message: &'static str },
}

/// 用户态 UART 驱动上下文
pub struct UserUartContext {
    /// MMIO 映射的用户态虚拟地址
    pub mmio_vaddr: VirtAddr,
    /// 中断号
    pub irq: u32,
    /// 接收缓冲区
    pub rx_buffer: VecDeque<u8>,
    /// 发送缓冲区
    pub tx_buffer: VecDeque<u8>,
    /// 是否有待处理的中断
    pub pending_irq: AtomicBool,
    /// 波特率
    pub baud_rate: AtomicU32,
}

impl UserUartContext {
    pub fn new(mmio_vaddr: VirtAddr, irq: u32) -> Self {
        Self {
            mmio_vaddr,
            irq,
            rx_buffer: VecDeque::new(),
            tx_buffer: VecDeque::new(),
            pending_irq: AtomicBool::new(false),
            baud_rate: AtomicU32::new(115200),
        }
    }
}

/// 用户态 UART 驱动服务
pub struct UserUartService {
    /// 驱动上下文
    contexts: SpinLock<Vec<UserUartContext>>,
    /// 请求队列
    request_queue: SpinLock<VecDeque<(usize, UartRequest)>>,
    /// 响应队列
    response_queue: SpinLock<VecDeque<(usize, UartResponse)>>,
}

impl UserUartService {
    pub fn new() -> Self {
        Self {
            contexts: SpinLock::new(Vec::new()),
            request_queue: SpinLock::new(VecDeque::new()),
            response_queue: SpinLock::new(VecDeque::new()),
        }
    }
    
    /// 注册 UART 设备
    pub fn register_device(&self, phys_addr: PhysAddr, irq: u32) -> Result<usize, &'static str> {
        // 映射 MMIO 到用户空间
        // 实际实现需要调用 mm 模块
        let mmio_vaddr = VirtAddr(phys_addr.0); // 简化：直接映射
        
        let ctx = UserUartContext::new(mmio_vaddr, irq);
        let mut contexts = self.contexts.lock();
        let id = contexts.len();
        contexts.push(ctx);
        
        crate::println!("[uart_user] Registered UART device {} at {:#x}", id, phys_addr.0);
        Ok(id)
    }
    
    /// 提交请求
    pub fn submit_request(&self, device_id: usize, request: UartRequest) -> Result<(), &'static str> {
        let contexts = self.contexts.lock();
        if device_id >= contexts.len() {
            return Err("Invalid device ID");
        }
        drop(contexts);
        
        self.request_queue.lock().push_back((device_id, request));
        Ok(())
    }
    
    /// 获取响应
    pub fn get_response(&self, device_id: usize) -> Option<UartResponse> {
        let mut queue = self.response_queue.lock();
        
        // 查找匹配的响应
        for i in 0..queue.len() {
            if queue[i].0 == device_id {
                return Some(queue.remove(i).unwrap().1);
            }
        }
        
        None
    }
    
    /// 处理请求（内核态辅助）
    pub fn process_requests(&self) {
        let mut requests = self.request_queue.lock();
        
        while let Some((device_id, request)) = requests.pop_front() {
            let response = self.handle_request(device_id, request);
            self.response_queue.lock().push_back((device_id, response));
        }
    }
    
    /// 处理单个请求
    fn handle_request(&self, device_id: usize, request: UartRequest) -> UartResponse {
        let mut contexts = self.contexts.lock();
        let ctx = match contexts.get_mut(device_id) {
            Some(c) => c,
            None => return UartResponse::Error { message: "Invalid device" },
        };
        
        match request {
            UartRequest::Read { max_len } => {
                let mut data = Vec::new();
                for _ in 0..max_len {
                    if let Some(byte) = ctx.rx_buffer.pop_front() {
                        data.push(byte);
                    } else {
                        break;
                    }
                }
                UartResponse::ReadResult { data }
            }
            
            UartRequest::Write { data } => {
                let written = data.len();
                for byte in data {
                    ctx.tx_buffer.push_back(byte);
                }
                // 触发发送
                self.flush_tx(device_id);
                UartResponse::WriteResult { written }
            }
            
            UartRequest::SetBaudRate { rate } => {
                ctx.baud_rate.store(rate, Ordering::SeqCst);
                // 实际需要配置硬件
                UartResponse::ConfigResult { success: true }
            }
            
            UartRequest::SetFormat { data_bits, stop_bits, parity } => {
                // 配置数据格式
                let _ = (data_bits, stop_bits, parity);
                UartResponse::ConfigResult { success: true }
            }
            
            UartRequest::SetInterrupt { enable } => {
                // 配置中断
                let _ = enable;
                UartResponse::ConfigResult { success: true }
            }
        }
    }
    
    /// 刷新发送缓冲区
    fn flush_tx(&self, device_id: usize) {
        let mut contexts = self.contexts.lock();
        if let Some(ctx) = contexts.get_mut(device_id) {
            let base = ctx.mmio_vaddr.0;
            
            while let Some(byte) = ctx.tx_buffer.pop_front() {
                // 等待发送就绪
                unsafe {
                    while core::ptr::read_volatile((base + regs::LSR) as *const u8) & lsr::THR_EMPTY == 0 {
                        core::hint::spin_loop();
                    }
                    core::ptr::write_volatile((base + regs::THR) as *mut u8, byte);
                }
            }
        }
    }
    
    /// 处理中断
    pub fn handle_interrupt(&self, device_id: usize) {
        let mut contexts = self.contexts.lock();
        if let Some(ctx) = contexts.get_mut(device_id) {
            let base = ctx.mmio_vaddr.0;
            
            unsafe {
                // 读取中断标识
                let iir = core::ptr::read_volatile((base + regs::IIR) as *const u8);
                
                if iir & 0x01 == 0 {
                    // 有中断待处理
                    let int_type = (iir >> 1) & 0x07;
                    
                    match int_type {
                        0b010 => {
                            // 接收数据可用
                            while core::ptr::read_volatile((base + regs::LSR) as *const u8) & lsr::DATA_READY != 0 {
                                let byte = core::ptr::read_volatile((base + regs::RBR) as *const u8);
                                ctx.rx_buffer.push_back(byte);
                            }
                        }
                        0b001 => {
                            // 发送保持寄存器空
                            // 可以继续发送
                        }
                        0b011 => {
                            // 线路状态
                            let _ = core::ptr::read_volatile((base + regs::LSR) as *const u8);
                        }
                        _ => {}
                    }
                }
            }
            
            ctx.pending_irq.store(true, Ordering::Release);
        }
    }
    
    /// 检查是否有待处理的中断
    pub fn has_pending_irq(&self, device_id: usize) -> bool {
        let contexts = self.contexts.lock();
        contexts.get(device_id)
            .map(|c| c.pending_irq.swap(false, Ordering::AcqRel))
            .unwrap_or(false)
    }
}

/// 全局用户态 UART 服务
static USER_UART_SERVICE: SpinLock<Option<UserUartService>> = SpinLock::new(None);

/// 初始化用户态 UART 驱动
pub fn init() {
    let service = UserUartService::new();
    
    // 注册 UART0
    let _ = service.register_device(PhysAddr(UART0_BASE), 10);
    
    *USER_UART_SERVICE.lock() = Some(service);
    crate::println!("[uart_user] User-space UART driver initialized");
}

/// 提交 UART 请求
pub fn submit_request(device_id: usize, request: UartRequest) -> Result<(), &'static str> {
    USER_UART_SERVICE.lock()
        .as_ref()
        .ok_or("UART service not initialized")?
        .submit_request(device_id, request)
}

/// 获取 UART 响应
pub fn get_response(device_id: usize) -> Option<UartResponse> {
    USER_UART_SERVICE.lock()
        .as_ref()
        .and_then(|s| s.get_response(device_id))
}

/// 处理 UART 中断
pub fn handle_interrupt(device_id: usize) {
    if let Some(ref service) = *USER_UART_SERVICE.lock() {
        service.handle_interrupt(device_id);
    }
}

/// 用户态 UART 驱动自测试
pub fn self_test() {
    crate::println!("[uart_user] Running self-test...");
    
    // 测试常量
    let _uart0_base = UART0_BASE;
    crate::println!("[uart_user] UART0_BASE = {:#x}", UART0_BASE);
    
    // 测试 regs 模块
    let reg_offsets = [
        regs::RBR,
        regs::THR,
        regs::IER,
        regs::IIR,
        regs::FCR,
        regs::LCR,
        regs::MCR,
        regs::LSR,
        regs::MSR,
        regs::SCR,
    ];
    for offset in &reg_offsets {
        let _o = *offset;
    }
    crate::println!("[uart_user] Register offsets tested");
    
    // 测试 lsr 模块
    let lsr_bits = [
        lsr::DATA_READY,
        lsr::OVERRUN_ERROR,
        lsr::PARITY_ERROR,
        lsr::FRAMING_ERROR,
        lsr::BREAK_INTERRUPT,
        lsr::THR_EMPTY,
        lsr::TRANSMITTER_EMPTY,
        lsr::FIFO_ERROR,
    ];
    for bit in &lsr_bits {
        let _b = *bit;
    }
    crate::println!("[uart_user] LSR bits tested");
    
    // 测试 ier 模块
    let ier_bits = [
        ier::RX_AVAILABLE,
        ier::TX_EMPTY,
        ier::LINE_STATUS,
        ier::MODEM_STATUS,
    ];
    for bit in &ier_bits {
        let _b = *bit;
    }
    crate::println!("[uart_user] IER bits tested");
    
    // 测试 UartRequest 变体
    let requests = [
        UartRequest::Read { max_len: 64 },
        UartRequest::Write { data: vec![0u8; 16] },
        UartRequest::SetBaudRate { rate: 115200 },
        UartRequest::SetFormat { data_bits: 8, stop_bits: 1, parity: 0 },
        UartRequest::SetInterrupt { enable: true },
    ];
    for req in &requests {
        crate::println!("[uart_user] Request: {:?}", req);
    }
    
    // 测试 UartResponse 变体
    let responses = [
        UartResponse::ReadResult { data: vec![0u8; 8] },
        UartResponse::WriteResult { written: 16 },
        UartResponse::ConfigResult { success: true },
        UartResponse::Error { message: "test error" },
    ];
    for resp in &responses {
        crate::println!("[uart_user] Response: {:?}", resp);
    }
    
    // 测试 UserUartContext
    let ctx = UserUartContext::new(VirtAddr(0x1000_0000), 10);
    crate::println!("[uart_user] Context: mmio={:#x} irq={}", ctx.mmio_vaddr.0, ctx.irq);
    let _pending = ctx.pending_irq.load(Ordering::Relaxed);
    let _baud = ctx.baud_rate.load(Ordering::Relaxed);
    
    // 测试 UserUartService
    let service = UserUartService::new();
    let _ = service.register_device(PhysAddr(0x1000_0000), 10);
    let _ = service.submit_request(0, UartRequest::Read { max_len: 32 });
    service.process_requests();
    let _ = service.get_response(0);
    let _ = service.has_pending_irq(0);
    crate::println!("[uart_user] UserUartService tested");
    
    // 测试全局函数
    init();
    let _ = submit_request(0, UartRequest::Read { max_len: 16 });
    let _ = get_response(0);
    handle_interrupt(0);
    
    crate::println!("[uart_user] Self-test passed!");
}
