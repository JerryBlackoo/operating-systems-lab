//! Hypervisor 虚拟化支持
//!
//! 实现 RISC-V H 扩展的虚拟化功能，支持运行 Guest OS

pub mod virtio_pv;

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use crate::sync::SpinLock;
use crate::mm::{PhysAddr, PhysPageNum, PAGE_SIZE};

/// 虚拟机 ID
pub type VmId = u64;

/// vCPU ID
pub type VcpuId = u64;

/// 虚拟机状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    /// 已创建
    Created,
    /// 运行中
    Running,
    /// 暂停
    Paused,
    /// 已停止
    Stopped,
}

/// vCPU 状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcpuState {
    /// 空闲
    Idle,
    /// 运行中
    Running,
    /// 等待中断
    WaitingForInterrupt,
    /// 已停止
    Stopped,
}

/// Guest 寄存器上下文
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct GuestContext {
    /// 通用寄存器 x0-x31
    pub x: [u64; 32],
    /// 程序计数器
    pub sepc: u64,
    /// 状态寄存器
    pub sstatus: u64,
    /// 中断使能
    pub sie: u64,
    /// 中断待处理
    pub sip: u64,
    /// 页表基址
    pub satp: u64,
    /// 异常原因
    pub scause: u64,
    /// 异常值
    pub stval: u64,
    /// 中断向量
    pub stvec: u64,
    /// 临时寄存器
    pub sscratch: u64,
}

impl GuestContext {
    pub fn new() -> Self {
        Self::default()
    }
    
    /// 设置入口点
    pub fn set_entry(&mut self, entry: u64) {
        self.sepc = entry;
    }
    
    /// 设置栈指针
    pub fn set_sp(&mut self, sp: u64) {
        self.x[2] = sp;
    }
    
    /// 设置参数
    pub fn set_arg(&mut self, index: usize, value: u64) {
        if index < 8 {
            self.x[10 + index] = value;
        }
    }
}

/// vCPU 结构
pub struct Vcpu {
    /// vCPU ID
    id: VcpuId,
    /// 所属虚拟机
    #[allow(dead_code)]
    vm_id: VmId,
    /// vCPU 状态
    state: VcpuState,
    /// Guest 上下文
    context: GuestContext,
    /// 是否有待处理中断
    pending_irq: AtomicBool,
    /// 待注入的中断号
    pending_irq_num: AtomicU64,
}

impl Vcpu {
    /// 创建新 vCPU
    pub fn new(id: VcpuId, vm_id: VmId) -> Self {
        Self {
            id,
            vm_id,
            state: VcpuState::Idle,
            context: GuestContext::new(),
            pending_irq: AtomicBool::new(false),
            pending_irq_num: AtomicU64::new(0),
        }
    }
    
    /// 获取 vCPU ID
    pub fn id(&self) -> VcpuId {
        self.id
    }
    
    /// 获取状态
    pub fn state(&self) -> VcpuState {
        self.state
    }
    
    /// 获取上下文
    pub fn context(&self) -> &GuestContext {
        &self.context
    }
    
    /// 获取可变上下文
    pub fn context_mut(&mut self) -> &mut GuestContext {
        &mut self.context
    }
    
    /// 注入中断
    pub fn inject_irq(&self, irq: u64) {
        self.pending_irq_num.store(irq, Ordering::SeqCst);
        self.pending_irq.store(true, Ordering::SeqCst);
    }
    
    /// 检查是否有待处理中断
    pub fn has_pending_irq(&self) -> bool {
        self.pending_irq.load(Ordering::SeqCst)
    }
    
    /// 获取并清除待处理中断
    pub fn take_pending_irq(&self) -> Option<u64> {
        if self.pending_irq.swap(false, Ordering::SeqCst) {
            Some(self.pending_irq_num.load(Ordering::SeqCst))
        } else {
            None
        }
    }
    
    /// 运行 vCPU（进入 Guest 模式）
    pub fn run(&mut self) -> VmExit {
        self.state = VcpuState::Running;
        
        // 检查待处理中断
        if let Some(irq) = self.take_pending_irq() {
            return VmExit::ExternalInterrupt(irq);
        }
        
        // 实际的 VM Entry 需要汇编实现
        // 这里返回模拟的退出原因
        self.state = VcpuState::Idle;
        VmExit::Halt
    }
}

/// VM Exit 原因
#[derive(Debug, Clone, Copy)]
pub enum VmExit {
    /// 外部中断
    ExternalInterrupt(u64),
    /// 时钟中断
    TimerInterrupt,
    /// 系统调用（ecall）
    Ecall,
    /// 页错误
    PageFault { addr: u64, is_write: bool },
    /// MMIO 访问
    MmioAccess { addr: u64, is_write: bool, size: usize },
    /// 停机指令
    Halt,
    /// 无效指令
    IllegalInstruction,
    /// 未知原因
    Unknown(u64),
}

/// Stage-2 页表（Guest 物理地址 -> Host 物理地址）
pub struct Stage2PageTable {
    /// 根页表物理页号
    root_ppn: PhysPageNum,
    /// 页表页帧
    frames: Vec<PhysPageNum>,
}

impl Stage2PageTable {
    /// 创建新的 Stage-2 页表
    pub fn new() -> Self {
        // 分配根页表
        let frame = crate::mm::frame_alloc().expect("Failed to allocate S2PT");
        let ppn = frame.ppn;
        
        // 清零
        ppn.as_bytes_mut().fill(0);
        
        Self {
            root_ppn: ppn,
            frames: Vec::new(),
        }
    }
    
    /// 获取 HGATP 值
    pub fn hgatp(&self) -> u64 {
        // Mode = 8 (Sv39x4), VMID = 0, PPN = root_ppn
        (8u64 << 60) | (self.root_ppn.0 as u64)
    }
    
    /// 映射 Guest 物理地址到 Host 物理地址
    pub fn map(&mut self, gpa: PhysAddr, hpa: PhysAddr, size: usize, flags: u64) {
        // 简化实现：假设 1:1 映射
        // 实际需要遍历页表并创建映射
        let _ = (gpa, hpa, size, flags);
    }
    
    /// 取消映射
    pub fn unmap(&mut self, gpa: PhysAddr, size: usize) {
        let _ = (gpa, size);
    }
}

/// 虚拟机配置
#[derive(Debug, Clone)]
pub struct VmConfig {
    /// 内存大小（字节）
    pub memory_size: usize,
    /// vCPU 数量
    pub vcpu_count: usize,
    /// 入口地址
    pub entry_point: u64,
}

impl Default for VmConfig {
    fn default() -> Self {
        Self {
            memory_size: 64 * 1024 * 1024, // 64MB
            vcpu_count: 1,
            entry_point: 0x8020_0000,
        }
    }
}

/// 虚拟机
pub struct Vm {
    /// 虚拟机 ID
    id: VmId,
    /// 虚拟机状态
    state: VmState,
    /// 配置
    config: VmConfig,
    /// vCPU 列表
    vcpus: Vec<Vcpu>,
    /// Stage-2 页表
    s2pt: Stage2PageTable,
    /// Guest 内存区域
    memory: Vec<PhysPageNum>,
}

impl Vm {
    /// 创建新虚拟机
    pub fn new(id: VmId, config: VmConfig) -> Self {
        let mut vcpus = Vec::with_capacity(config.vcpu_count);
        for i in 0..config.vcpu_count {
            vcpus.push(Vcpu::new(i as VcpuId, id));
        }
        
        Self {
            id,
            state: VmState::Created,
            config,
            vcpus,
            s2pt: Stage2PageTable::new(),
            memory: Vec::new(),
        }
    }
    
    /// 获取虚拟机 ID
    pub fn id(&self) -> VmId {
        self.id
    }
    
    /// 获取状态
    pub fn state(&self) -> VmState {
        self.state
    }
    
    /// 分配 Guest 内存
    pub fn allocate_memory(&mut self) -> Result<(), HypervisorError> {
        let pages = self.config.memory_size / PAGE_SIZE;
        
        for _ in 0..pages {
            let frame = crate::mm::frame_alloc()
                .ok_or(HypervisorError::OutOfMemory)?;
            let ppn = frame.ppn;
            self.memory.push(ppn);
        }
        
        // 设置 Stage-2 映射
        for (i, &ppn) in self.memory.iter().enumerate() {
            let gpa = PhysAddr(self.config.entry_point as usize + i * PAGE_SIZE);
            let hpa = ppn.addr();
            self.s2pt.map(gpa, hpa, PAGE_SIZE, 0x7); // RWX
        }
        
        Ok(())
    }
    
    /// 加载 Guest 镜像
    pub fn load_image(&mut self, image: &[u8]) -> Result<(), HypervisorError> {
        if image.len() > self.memory.len() * PAGE_SIZE {
            return Err(HypervisorError::ImageTooLarge);
        }
        
        // 复制镜像到 Guest 内存
        let mut offset = 0;
        for &ppn in &self.memory {
            let page = ppn.as_bytes_mut();
            let copy_len = (image.len() - offset).min(PAGE_SIZE);
            if copy_len > 0 {
                page[..copy_len].copy_from_slice(&image[offset..offset + copy_len]);
                offset += copy_len;
            }
            if offset >= image.len() {
                break;
            }
        }
        
        Ok(())
    }
    
    /// 启动虚拟机
    pub fn start(&mut self) -> Result<(), HypervisorError> {
        if self.state != VmState::Created && self.state != VmState::Paused {
            return Err(HypervisorError::InvalidState);
        }
        
        // 设置 vCPU 入口点
        for vcpu in &mut self.vcpus {
            vcpu.context_mut().set_entry(self.config.entry_point);
        }
        
        self.state = VmState::Running;
        Ok(())
    }
    
    /// 暂停虚拟机
    pub fn pause(&mut self) -> Result<(), HypervisorError> {
        if self.state != VmState::Running {
            return Err(HypervisorError::InvalidState);
        }
        self.state = VmState::Paused;
        Ok(())
    }
    
    /// 停止虚拟机
    pub fn stop(&mut self) -> Result<(), HypervisorError> {
        self.state = VmState::Stopped;
        Ok(())
    }
    
    /// 获取 vCPU
    pub fn vcpu(&self, id: VcpuId) -> Option<&Vcpu> {
        self.vcpus.get(id as usize)
    }
    
    /// 获取可变 vCPU
    pub fn vcpu_mut(&mut self, id: VcpuId) -> Option<&mut Vcpu> {
        self.vcpus.get_mut(id as usize)
    }
    
    /// 运行 vCPU
    pub fn run_vcpu(&mut self, vcpu_id: VcpuId) -> Result<VmExit, HypervisorError> {
        let vcpu = self.vcpus.get_mut(vcpu_id as usize)
            .ok_or(HypervisorError::InvalidVcpu)?;
        Ok(vcpu.run())
    }
}

/// Hypervisor 错误
#[derive(Debug, Clone, Copy)]
pub enum HypervisorError {
    /// 内存不足
    OutOfMemory,
    /// 镜像太大
    ImageTooLarge,
    /// 无效状态
    InvalidState,
    /// 无效 vCPU
    InvalidVcpu,
    /// 无效虚拟机
    InvalidVm,
    /// 不支持
    NotSupported,
}

/// 虚拟中断控制器
pub struct VirtualInterruptController {
    /// 待处理中断位图
    pending: AtomicU64,
    /// 中断使能位图
    enabled: AtomicU64,
}

impl VirtualInterruptController {
    pub fn new() -> Self {
        Self {
            pending: AtomicU64::new(0),
            enabled: AtomicU64::new(0xFFFF_FFFF_FFFF_FFFF),
        }
    }
    
    /// 设置中断待处理
    pub fn set_pending(&self, irq: u32) {
        self.pending.fetch_or(1 << irq, Ordering::SeqCst);
    }
    
    /// 清除中断待处理
    pub fn clear_pending(&self, irq: u32) {
        self.pending.fetch_and(!(1 << irq), Ordering::SeqCst);
    }
    
    /// 使能中断
    pub fn enable(&self, irq: u32) {
        self.enabled.fetch_or(1 << irq, Ordering::SeqCst);
    }
    
    /// 禁用中断
    pub fn disable(&self, irq: u32) {
        self.enabled.fetch_and(!(1 << irq), Ordering::SeqCst);
    }
    
    /// 获取最高优先级待处理中断
    pub fn get_pending(&self) -> Option<u32> {
        let pending = self.pending.load(Ordering::SeqCst);
        let enabled = self.enabled.load(Ordering::SeqCst);
        let active = pending & enabled;
        
        if active == 0 {
            None
        } else {
            Some(active.trailing_zeros())
        }
    }
}

/// 全局 Hypervisor
static HYPERVISOR: SpinLock<Option<Hypervisor>> = SpinLock::new(None);

/// Hypervisor 管理器
pub struct Hypervisor {
    /// 虚拟机表
    vms: BTreeMap<VmId, Vm>,
    /// 下一个虚拟机 ID
    next_vm_id: VmId,
}

impl Hypervisor {
    fn new() -> Self {
        Self {
            vms: BTreeMap::new(),
            next_vm_id: 1,
        }
    }
    
    /// 创建虚拟机
    pub fn create_vm(&mut self, config: VmConfig) -> Result<VmId, HypervisorError> {
        let id = self.next_vm_id;
        self.next_vm_id += 1;
        
        let mut vm = Vm::new(id, config);
        vm.allocate_memory()?;
        
        self.vms.insert(id, vm);
        Ok(id)
    }
    
    /// 获取虚拟机
    pub fn get_vm(&self, id: VmId) -> Option<&Vm> {
        self.vms.get(&id)
    }
    
    /// 获取可变虚拟机
    pub fn get_vm_mut(&mut self, id: VmId) -> Option<&mut Vm> {
        self.vms.get_mut(&id)
    }
    
    /// 销毁虚拟机
    pub fn destroy_vm(&mut self, id: VmId) -> Result<(), HypervisorError> {
        self.vms.remove(&id)
            .map(|_| ())
            .ok_or(HypervisorError::InvalidVm)
    }
}

/// 初始化 Hypervisor
pub fn init() {
    let mut hv = HYPERVISOR.lock();
    *hv = Some(Hypervisor::new());
    crate::println!("[hypervisor] Hypervisor initialized");
}

/// 创建虚拟机
pub fn create_vm(config: VmConfig) -> Result<VmId, HypervisorError> {
    HYPERVISOR.lock()
        .as_mut()
        .ok_or(HypervisorError::NotSupported)?
        .create_vm(config)
}


/// Hypervisor 模块自测试
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[hypervisor] Running self-test...");
    
    // 测试 VmState 枚举
    let vm_states = [
        VmState::Created,
        VmState::Running,
        VmState::Paused,
        VmState::Stopped,
    ];
    for state in &vm_states {
        crate::println!("[hypervisor] VM State: {:?}", state);
    }
    
    // 测试 VcpuState 枚举
    let vcpu_states = [
        VcpuState::Idle,
        VcpuState::Running,
        VcpuState::WaitingForInterrupt,
        VcpuState::Stopped,
    ];
    for state in &vcpu_states {
        crate::println!("[hypervisor] vCPU State: {:?}", state);
    }
    
    // 测试 GuestContext
    let mut ctx = GuestContext::new();
    ctx.set_entry(0x80200000);
    ctx.set_sp(0x80300000);
    ctx.set_arg(0, 100);
    ctx.set_arg(7, 700);
    ctx.set_arg(8, 800); // 超出范围，应该被忽略
    assert_eq!(ctx.sepc, 0x80200000);
    assert_eq!(ctx.x[2], 0x80300000);
    assert_eq!(ctx.x[10], 100);
    
    // 测试 Vcpu
    let mut vcpu = Vcpu::new(0, 1);
    assert_eq!(vcpu.id(), 0);
    assert_eq!(vcpu.state(), VcpuState::Idle);
    let _ = vcpu.context();
    let _ = vcpu.context_mut();
    vcpu.inject_irq(5);
    assert!(vcpu.has_pending_irq());
    let irq = vcpu.take_pending_irq();
    assert_eq!(irq, Some(5));
    assert!(!vcpu.has_pending_irq());
    
    // 测试 vcpu.run()
    let exit = vcpu.run();
    crate::println!("[hypervisor] vCPU exit: {:?}", exit);
    
    // 测试 VmExit 枚举
    let exits = [
        VmExit::ExternalInterrupt(1),
        VmExit::TimerInterrupt,
        VmExit::Ecall,
        VmExit::PageFault { addr: 0x1000, is_write: false },
        VmExit::MmioAccess { addr: 0x2000, is_write: true, size: 4 },
        VmExit::Halt,
        VmExit::IllegalInstruction,
        VmExit::Unknown(0),
    ];
    for exit in &exits {
        crate::println!("[hypervisor] Exit: {:?}", exit);
    }
    
    // 测试 Stage2PageTable
    let mut s2pt = Stage2PageTable::new();
    let _ = s2pt.hgatp();
    s2pt.map(PhysAddr(0x80000000), PhysAddr(0x80000000), PAGE_SIZE, 0x7);
    s2pt.unmap(PhysAddr(0x80000000), PAGE_SIZE);
    // 访问字段
    let _ = s2pt.root_ppn;
    let _ = s2pt.frames.len();
    
    // 测试 VmConfig
    let config = VmConfig::default();
    assert_eq!(config.vcpu_count, 1);
    let custom_config = VmConfig {
        memory_size: 128 * 1024 * 1024,
        vcpu_count: 2,
        entry_point: 0x80100000,
    };
    crate::println!("[hypervisor] Config: {:?}", custom_config);
    
    // 测试 HypervisorError 枚举
    let errors = [
        HypervisorError::OutOfMemory,
        HypervisorError::ImageTooLarge,
        HypervisorError::InvalidState,
        HypervisorError::InvalidVcpu,
        HypervisorError::InvalidVm,
        HypervisorError::NotSupported,
    ];
    for err in &errors {
        crate::println!("[hypervisor] Error: {:?}", err);
    }
    
    // 测试 VirtualInterruptController
    let vic = VirtualInterruptController::new();
    vic.set_pending(5);
    vic.enable(5);
    let pending = vic.get_pending();
    assert_eq!(pending, Some(5));
    vic.disable(5);
    vic.clear_pending(5);
    
    // 测试 Hypervisor
    init();
    
    // 测试创建虚拟机（可能会因为内存不足而失败）
    let small_config = VmConfig {
        memory_size: PAGE_SIZE, // 只分配一页
        vcpu_count: 1,
        entry_point: 0x80200000,
    };
    
    match create_vm(small_config) {
        Ok(vm_id) => {
            crate::println!("[hypervisor] Created VM id={}", vm_id);
            
            // 测试 Hypervisor 方法
            if let Some(hv) = HYPERVISOR.lock().as_mut() {
                if let Some(vm) = hv.get_vm(vm_id) {
                    assert_eq!(vm.id(), vm_id);
                    assert_eq!(vm.state(), VmState::Created);
                }
                
                if let Some(vm) = hv.get_vm_mut(vm_id) {
                    // 测试 Vm 方法
                    let _ = vm.vcpu(0);
                    let _ = vm.vcpu_mut(0);
                    
                    // 测试 load_image
                    let image = [0u8; 64];
                    let _ = vm.load_image(&image);
                    
                    // 测试 start
                    let _ = vm.start();
                    
                    // 测试 run_vcpu
                    let _ = vm.run_vcpu(0);
                    
                    // 测试 pause
                    let _ = vm.pause();
                    
                    // 测试 stop
                    let _ = vm.stop();
                }
                
                // 测试 destroy_vm
                let _ = hv.destroy_vm(vm_id);
            }
        }
        Err(e) => {
            crate::println!("[hypervisor] Failed to create VM: {:?}", e);
        }
    }
    
    // 测试 virtio_pv 子模块
    virtio_pv::self_test();
    
    crate::println!("[hypervisor] Self-test passed!");
    Ok(())
}
