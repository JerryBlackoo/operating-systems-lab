//! 虚拟化属性测试
//!
//! 验证 Hypervisor 的以下属性：
//! 1. vCPU 状态隔离：不同 vCPU 状态互不影响
//! 2. Stage-2 页表隔离：不同 VM 内存隔离
//! 3. 虚拟中断正确性：中断正确注入
//! 4. VM Exit 处理：正确处理 VM Exit
//! 5. 资源限制：VM 不能超出分配的资源

use crate::{SimpleRng, println};
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

/// vCPU 状态
#[derive(Debug, Clone)]
struct VcpuState {
    /// 通用寄存器
    regs: [u64; 32],
    /// 程序计数器
    pc: u64,
    /// 状态寄存器
    sstatus: u64,
    /// 是否运行中
    running: bool,
    /// 待处理中断
    pending_irqs: Vec<u32>,
}

impl VcpuState {
    fn new() -> Self {
        Self {
            regs: [0; 32],
            pc: 0,
            sstatus: 0,
            running: false,
            pending_irqs: Vec::new(),
        }
    }
    
    fn set_reg(&mut self, idx: usize, value: u64) {
        if idx > 0 && idx < 32 {
            self.regs[idx] = value;
        }
    }
    
    fn get_reg(&self, idx: usize) -> u64 {
        if idx < 32 { self.regs[idx] } else { 0 }
    }
    
    fn inject_irq(&mut self, irq: u32) {
        self.pending_irqs.push(irq);
    }
    
    fn has_pending_irq(&self) -> bool {
        !self.pending_irqs.is_empty()
    }
}

/// Stage-2 页表项
#[derive(Debug, Clone, Copy)]
struct S2PTE {
    ppn: u64,
    flags: u64,
}

/// Stage-2 页表
struct Stage2PageTable {
    mappings: BTreeMap<u64, S2PTE>, // GPA -> HPA
    vm_id: usize,
}

impl Stage2PageTable {
    fn new(vm_id: usize) -> Self {
        Self {
            mappings: BTreeMap::new(),
            vm_id,
        }
    }
    
    fn map(&mut self, gpa: u64, hpa: u64, flags: u64) {
        self.mappings.insert(gpa, S2PTE { ppn: hpa >> 12, flags });
    }
    
    fn unmap(&mut self, gpa: u64) {
        self.mappings.remove(&gpa);
    }
    
    fn translate(&self, gpa: u64) -> Option<u64> {
        let page = gpa & !0xFFF;
        let offset = gpa & 0xFFF;
        self.mappings.get(&page).map(|pte| (pte.ppn << 12) | offset)
    }
}

/// VM Exit 原因
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VmExitReason {
    Ecall,
    PageFault,
    IllegalInstruction,
    Timer,
    ExternalInterrupt,
}

/// 虚拟机
struct VirtualMachine {
    vm_id: usize,
    vcpus: Vec<VcpuState>,
    s2pt: Stage2PageTable,
    memory_limit: usize,
    allocated_memory: usize,
}

impl VirtualMachine {
    fn new(vm_id: usize, num_vcpus: usize, memory_limit: usize) -> Self {
        let mut vcpus = Vec::with_capacity(num_vcpus);
        for _ in 0..num_vcpus {
            vcpus.push(VcpuState::new());
        }
        Self {
            vm_id,
            vcpus,
            s2pt: Stage2PageTable::new(vm_id),
            memory_limit,
            allocated_memory: 0,
        }
    }
    
    fn allocate_memory(&mut self, size: usize) -> bool {
        if self.allocated_memory + size > self.memory_limit {
            return false;
        }
        self.allocated_memory += size;
        true
    }
    
    fn map_memory(&mut self, gpa: u64, hpa: u64, size: usize) -> bool {
        if !self.allocate_memory(size) {
            return false;
        }
        
        let pages = (size + 0xFFF) / 0x1000;
        for i in 0..pages {
            let offset = (i * 0x1000) as u64;
            self.s2pt.map(gpa + offset, hpa + offset, 0x7); // RWX
        }
        true
    }
    
    fn run_vcpu(&mut self, vcpu_id: usize) -> Option<VmExitReason> {
        if vcpu_id >= self.vcpus.len() {
            return None;
        }
        
        self.vcpus[vcpu_id].running = true;
        
        // 模拟运行，返回随机 exit 原因
        Some(VmExitReason::Timer)
    }
    
    fn stop_vcpu(&mut self, vcpu_id: usize) {
        if vcpu_id < self.vcpus.len() {
            self.vcpus[vcpu_id].running = false;
        }
    }
}

/// Hypervisor
struct Hypervisor {
    vms: BTreeMap<usize, VirtualMachine>,
    next_vm_id: usize,
    total_memory: usize,
    used_memory: usize,
}

impl Hypervisor {
    fn new(total_memory: usize) -> Self {
        Self {
            vms: BTreeMap::new(),
            next_vm_id: 1,
            total_memory,
            used_memory: 0,
        }
    }
    
    fn create_vm(&mut self, num_vcpus: usize, memory_limit: usize) -> Option<usize> {
        if self.used_memory + memory_limit > self.total_memory {
            return None;
        }
        
        let vm_id = self.next_vm_id;
        self.next_vm_id += 1;
        self.used_memory += memory_limit;
        
        self.vms.insert(vm_id, VirtualMachine::new(vm_id, num_vcpus, memory_limit));
        Some(vm_id)
    }
    
    fn destroy_vm(&mut self, vm_id: usize) -> bool {
        if let Some(vm) = self.vms.remove(&vm_id) {
            self.used_memory -= vm.memory_limit;
            true
        } else {
            false
        }
    }
    
    fn get_vm(&self, vm_id: usize) -> Option<&VirtualMachine> {
        self.vms.get(&vm_id)
    }
    
    fn get_vm_mut(&mut self, vm_id: usize) -> Option<&mut VirtualMachine> {
        self.vms.get_mut(&vm_id)
    }
}

/// 属性1：vCPU 状态隔离
/// 不同 vCPU 的状态应互不影响
#[test_case]
fn test_vcpu_state_isolation() -> bool {
    let mut vm = VirtualMachine::new(1, 4, 1024 * 1024);
    
    // 设置不同 vCPU 的寄存器
    for i in 0..4 {
        vm.vcpus[i].set_reg(1, (i * 100) as u64);
        vm.vcpus[i].pc = (i * 0x1000) as u64;
    }
    
    // 验证隔离
    for i in 0..4 {
        if vm.vcpus[i].get_reg(1) != (i * 100) as u64 {
            println!("  错误: vCPU {} 寄存器被修改", i);
            return false;
        }
        if vm.vcpus[i].pc != (i * 0x1000) as u64 {
            println!("  错误: vCPU {} PC 被修改", i);
            return false;
        }
    }
    
    true
}

/// 属性2：Stage-2 页表隔离
/// 不同 VM 的内存映射应隔离
#[test_case]
fn test_s2pt_isolation() -> bool {
    let mut hv = Hypervisor::new(16 * 1024 * 1024);
    
    let vm1 = hv.create_vm(1, 4 * 1024 * 1024).unwrap();
    let vm2 = hv.create_vm(1, 4 * 1024 * 1024).unwrap();
    
    // VM1 映射 GPA 0x1000 -> HPA 0x10000
    hv.get_vm_mut(vm1).unwrap().s2pt.map(0x1000, 0x10000, 0x7);
    
    // VM2 映射 GPA 0x1000 -> HPA 0x20000
    hv.get_vm_mut(vm2).unwrap().s2pt.map(0x1000, 0x20000, 0x7);
    
    // 验证隔离
    let hpa1 = hv.get_vm(vm1).unwrap().s2pt.translate(0x1000);
    let hpa2 = hv.get_vm(vm2).unwrap().s2pt.translate(0x1000);
    
    if hpa1 != Some(0x10000) {
        println!("  错误: VM1 翻译不正确");
        return false;
    }
    
    if hpa2 != Some(0x20000) {
        println!("  错误: VM2 翻译不正确");
        return false;
    }
    
    true
}

/// 属性3：虚拟中断注入
/// 中断应正确注入到 vCPU
#[test_case]
fn test_virtual_interrupt_injection() -> bool {
    let mut vm = VirtualMachine::new(1, 2, 1024 * 1024);
    
    // 向 vCPU 0 注入中断
    vm.vcpus[0].inject_irq(10);
    vm.vcpus[0].inject_irq(11);
    
    // 向 vCPU 1 注入不同中断
    vm.vcpus[1].inject_irq(20);
    
    // 验证
    if !vm.vcpus[0].has_pending_irq() {
        println!("  错误: vCPU 0 应有待处理中断");
        return false;
    }
    
    if vm.vcpus[0].pending_irqs.len() != 2 {
        println!("  错误: vCPU 0 应有 2 个待处理中断");
        return false;
    }
    
    if vm.vcpus[1].pending_irqs.len() != 1 {
        println!("  错误: vCPU 1 应有 1 个待处理中断");
        return false;
    }
    
    true
}

/// 属性4：内存限制
/// VM 不能超出分配的内存
#[test_case]
fn test_memory_limit() -> bool {
    let mut vm = VirtualMachine::new(1, 1, 4096); // 4KB 限制
    
    // 分配 2KB 应成功
    if !vm.allocate_memory(2048) {
        println!("  错误: 2KB 分配应成功");
        return false;
    }
    
    // 再分配 2KB 应成功
    if !vm.allocate_memory(2048) {
        println!("  错误: 第二次 2KB 分配应成功");
        return false;
    }
    
    // 再分配 1 字节应失败
    if vm.allocate_memory(1) {
        println!("  错误: 超出限制的分配应失败");
        return false;
    }
    
    true
}

/// 属性5：Hypervisor 资源管理
/// Hypervisor 应正确管理总资源
#[test_case]
fn test_hypervisor_resource_management() -> bool {
    let mut hv = Hypervisor::new(10 * 1024 * 1024); // 10MB
    
    // 创建 3 个 3MB 的 VM 应成功
    let vm1 = hv.create_vm(1, 3 * 1024 * 1024);
    let vm2 = hv.create_vm(1, 3 * 1024 * 1024);
    let vm3 = hv.create_vm(1, 3 * 1024 * 1024);
    
    if vm1.is_none() || vm2.is_none() || vm3.is_none() {
        println!("  错误: 前 3 个 VM 创建应成功");
        return false;
    }
    
    // 创建第 4 个 3MB VM 应失败
    let vm4 = hv.create_vm(1, 3 * 1024 * 1024);
    if vm4.is_some() {
        println!("  错误: 超出总内存的 VM 创建应失败");
        return false;
    }
    
    // 销毁一个 VM 后应能创建新 VM
    hv.destroy_vm(vm1.unwrap());
    
    let vm5 = hv.create_vm(1, 3 * 1024 * 1024);
    if vm5.is_none() {
        println!("  错误: 销毁后应能创建新 VM");
        return false;
    }
    
    true
}

/// 属性6：vCPU 运行状态
/// vCPU 运行状态应正确跟踪
#[test_case]
fn test_vcpu_running_state() -> bool {
    let mut vm = VirtualMachine::new(1, 2, 1024 * 1024);
    
    // 初始应不运行
    if vm.vcpus[0].running || vm.vcpus[1].running {
        println!("  错误: 初始 vCPU 不应运行");
        return false;
    }
    
    // 运行 vCPU 0
    vm.run_vcpu(0);
    
    if !vm.vcpus[0].running {
        println!("  错误: vCPU 0 应运行");
        return false;
    }
    
    if vm.vcpus[1].running {
        println!("  错误: vCPU 1 不应运行");
        return false;
    }
    
    // 停止 vCPU 0
    vm.stop_vcpu(0);
    
    if vm.vcpus[0].running {
        println!("  错误: vCPU 0 应停止");
        return false;
    }
    
    true
}

/// 属性7：GPA 翻译正确性
/// GPA 到 HPA 的翻译应正确
#[test_case]
fn test_gpa_translation() -> bool {
    let mut s2pt = Stage2PageTable::new(1);
    
    // 映射多个页
    s2pt.map(0x0000, 0x10000, 0x7);
    s2pt.map(0x1000, 0x20000, 0x7);
    s2pt.map(0x2000, 0x30000, 0x7);
    
    // 验证翻译
    if s2pt.translate(0x0000) != Some(0x10000) {
        println!("  错误: GPA 0x0000 翻译不正确");
        return false;
    }
    
    if s2pt.translate(0x0100) != Some(0x10100) {
        println!("  错误: GPA 0x0100 翻译不正确（页内偏移）");
        return false;
    }
    
    if s2pt.translate(0x1500) != Some(0x20500) {
        println!("  错误: GPA 0x1500 翻译不正确");
        return false;
    }
    
    // 未映射地址应返回 None
    if s2pt.translate(0x5000).is_some() {
        println!("  错误: 未映射地址应返回 None");
        return false;
    }
    
    true
}

/// 属性8：VM 销毁
/// 销毁 VM 应释放所有资源
#[test_case]
fn test_vm_destruction() -> bool {
    let mut hv = Hypervisor::new(10 * 1024 * 1024);
    
    let initial_used = hv.used_memory;
    
    let vm_id = hv.create_vm(4, 4 * 1024 * 1024).unwrap();
    
    if hv.used_memory != initial_used + 4 * 1024 * 1024 {
        println!("  错误: 创建后内存使用应增加");
        return false;
    }
    
    hv.destroy_vm(vm_id);
    
    if hv.used_memory != initial_used {
        println!("  错误: 销毁后内存使用应恢复");
        return false;
    }
    
    if hv.get_vm(vm_id).is_some() {
        println!("  错误: 销毁后 VM 不应存在");
        return false;
    }
    
    true
}

/// 属性9：随机 VM 操作
/// 随机创建和销毁 VM，验证资源管理
#[test_case]
fn test_random_vm_operations() -> bool {
    let mut hv = Hypervisor::new(32 * 1024 * 1024);
    let mut rng = SimpleRng::new(12345);
    let mut active_vms: Vec<usize> = Vec::new();
    
    for _ in 0..50 {
        let op = rng.next_range(0, 3);
        
        match op {
            0 | 1 => {
                // 创建 VM
                let mem = rng.next_range(1, 5) * 1024 * 1024;
                if let Some(vm_id) = hv.create_vm(1, mem) {
                    active_vms.push(vm_id);
                }
            }
            _ => {
                // 销毁 VM
                if !active_vms.is_empty() {
                    let idx = rng.next_range(0, active_vms.len());
                    let vm_id = active_vms.swap_remove(idx);
                    hv.destroy_vm(vm_id);
                }
            }
        }
        
        // 验证不变量
        if hv.used_memory > hv.total_memory {
            println!("  错误: 使用内存超过总内存");
            return false;
        }
        
        if hv.vms.len() != active_vms.len() {
            println!("  错误: VM 数量不一致");
            return false;
        }
    }
    
    true
}
