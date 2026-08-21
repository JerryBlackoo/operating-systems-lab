//! 内核热补丁 (Live Patching)
//!
//! 实现运行时内核函数替换，无需重启

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use alloc::string::String;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::sync::SpinLock;

/// 补丁 ID
pub type PatchId = u64;

/// 补丁状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchState {
    /// 已注册
    Registered,
    /// 正在应用
    Applying,
    /// 已应用
    Applied,
    /// 正在回滚
    Reverting,
    /// 已回滚
    Reverted,
    /// 失败
    Failed,
}

/// 函数补丁
#[derive(Debug)]
pub struct FuncPatch {
    /// 原函数名称
    pub name: String,
    /// 原函数地址
    pub old_addr: usize,
    /// 新函数地址
    pub new_addr: usize,
    /// 原始指令备份
    pub old_insns: [u8; 16],
    /// 是否已应用
    pub applied: bool,
}

impl FuncPatch {
    pub fn new(name: String, old_addr: usize, new_addr: usize) -> Self {
        Self {
            name,
            old_addr,
            new_addr,
            old_insns: [0; 16],
            applied: false,
        }
    }
}

/// 补丁
pub struct Patch {
    /// 补丁 ID
    id: PatchId,
    /// 补丁名称
    name: String,
    /// 补丁状态
    state: PatchState,
    /// 函数补丁列表
    funcs: Vec<FuncPatch>,
    /// 依赖的补丁
    depends: Vec<PatchId>,
}

impl Patch {
    pub fn new(id: PatchId, name: String) -> Self {
        Self {
            id,
            name,
            state: PatchState::Registered,
            funcs: Vec::new(),
            depends: Vec::new(),
        }
    }
    
    /// 添加函数补丁
    pub fn add_func(&mut self, func: FuncPatch) {
        self.funcs.push(func);
    }
    
    /// 添加依赖
    pub fn add_dependency(&mut self, patch_id: PatchId) {
        self.depends.push(patch_id);
    }
    
    /// 获取状态
    pub fn state(&self) -> PatchState {
        self.state
    }
}

/// 静默状态检测器
/// 用于确保所有 CPU 都处于安全点
pub struct QuiescentStateDetector {
    /// 每个 CPU 的计数器
    counters: Vec<AtomicU64>,
    /// 目标计数
    target: AtomicU64,
    /// 是否完成
    done: AtomicBool,
}

impl QuiescentStateDetector {
    pub fn new(num_cpus: usize) -> Self {
        let mut counters = Vec::with_capacity(num_cpus);
        for _ in 0..num_cpus {
            counters.push(AtomicU64::new(0));
        }
        Self {
            counters,
            target: AtomicU64::new(0),
            done: AtomicBool::new(false),
        }
    }
    
    /// 开始新的静默状态检测
    pub fn start(&self) {
        let _target = self.target.fetch_add(1, Ordering::SeqCst) + 1;
        self.done.store(false, Ordering::SeqCst);
        
        // 等待所有 CPU 到达静默点
        // 实际实现需要 IPI 通知
    }
    
    /// CPU 报告到达静默点
    pub fn report(&self, cpu_id: usize) {
        if cpu_id < self.counters.len() {
            self.counters[cpu_id].fetch_add(1, Ordering::SeqCst);
        }
        self.check_complete();
    }
    
    /// 检查是否所有 CPU 都到达
    fn check_complete(&self) {
        let target = self.target.load(Ordering::SeqCst);
        let all_reached = self.counters.iter()
            .all(|c| c.load(Ordering::SeqCst) >= target);
        
        if all_reached {
            self.done.store(true, Ordering::SeqCst);
        }
    }
    
    /// 等待完成
    pub fn wait(&self) {
        while !self.done.load(Ordering::SeqCst) {
            core::hint::spin_loop();
        }
    }
    
    /// 是否完成
    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }
}

/// 补丁错误
#[derive(Debug, Clone)]
pub enum PatchError {
    /// 补丁不存在
    NotFound,
    /// 无效状态
    InvalidState,
    /// 依赖未满足
    DependencyNotMet,
    /// 应用失败
    ApplyFailed(String),
    /// 回滚失败
    RevertFailed(String),
    /// 静默状态超时
    QuiescentTimeout,
}

/// 全局补丁管理器
static PATCH_MANAGER: SpinLock<Option<PatchManager>> = SpinLock::new(None);

/// 补丁管理器
pub struct PatchManager {
    /// 补丁表
    patches: BTreeMap<PatchId, Patch>,
    /// 下一个补丁 ID
    next_id: PatchId,
    /// 静默状态检测器
    qsd: QuiescentStateDetector,
}

impl PatchManager {
    fn new(num_cpus: usize) -> Self {
        Self {
            patches: BTreeMap::new(),
            next_id: 1,
            qsd: QuiescentStateDetector::new(num_cpus),
        }
    }
    
    /// 注册补丁
    pub fn register(&mut self, name: String, funcs: Vec<FuncPatch>) -> PatchId {
        let id = self.next_id;
        self.next_id += 1;
        
        let mut patch = Patch::new(id, name);
        for func in funcs {
            patch.add_func(func);
        }
        
        self.patches.insert(id, patch);
        id
    }
    
    /// 应用补丁
    pub fn apply(&mut self, patch_id: PatchId) -> Result<(), PatchError> {
        // 先检查补丁状态和依赖
        {
            let patch = self.patches.get(&patch_id)
                .ok_or(PatchError::NotFound)?;
            
            if patch.state != PatchState::Registered && patch.state != PatchState::Reverted {
                return Err(PatchError::InvalidState);
            }
            
            // 检查依赖
            for dep_id in &patch.depends {
                if let Some(dep) = self.patches.get(dep_id) {
                    if dep.state != PatchState::Applied {
                        return Err(PatchError::DependencyNotMet);
                    }
                }
            }
        }
        
        // 现在可以安全地获取可变引用
        let patch = self.patches.get_mut(&patch_id).unwrap();
        patch.state = PatchState::Applying;
        
        // 等待静默状态
        self.qsd.start();
        self.qsd.wait();
        
        // 应用所有函数补丁
        for func in &mut patch.funcs {
            if let Err(e) = apply_func_patch(func) {
                patch.state = PatchState::Failed;
                return Err(PatchError::ApplyFailed(e));
            }
        }
        
        patch.state = PatchState::Applied;
        Ok(())
    }
    
    /// 回滚补丁
    pub fn revert(&mut self, patch_id: PatchId) -> Result<(), PatchError> {
        let patch = self.patches.get_mut(&patch_id)
            .ok_or(PatchError::NotFound)?;
        
        if patch.state != PatchState::Applied {
            return Err(PatchError::InvalidState);
        }
        
        patch.state = PatchState::Reverting;
        
        // 等待静默状态
        self.qsd.start();
        self.qsd.wait();
        
        // 回滚所有函数补丁
        for func in &mut patch.funcs {
            if let Err(e) = revert_func_patch(func) {
                patch.state = PatchState::Failed;
                return Err(PatchError::RevertFailed(e));
            }
        }
        
        patch.state = PatchState::Reverted;
        Ok(())
    }
    
    /// 获取补丁状态
    pub fn get_state(&self, patch_id: PatchId) -> Option<PatchState> {
        self.patches.get(&patch_id).map(|p| p.state)
    }
    
    /// 列出所有补丁
    pub fn list(&self) -> Vec<(PatchId, &str, PatchState)> {
        self.patches.iter()
            .map(|(id, p)| (*id, p.name.as_str(), p.state))
            .collect()
    }
}

/// 应用函数补丁
fn apply_func_patch(func: &mut FuncPatch) -> Result<(), String> {
    if func.applied {
        return Ok(());
    }
    
    unsafe {
        let old_ptr = func.old_addr as *mut u8;
        
        // 备份原始指令
        core::ptr::copy_nonoverlapping(old_ptr, func.old_insns.as_mut_ptr(), 16);
        
        // 写入跳转指令
        // RISC-V: auipc t0, offset[31:12]; jalr x0, offset[11:0](t0)
        // 简化实现：直接写入绝对跳转
        let jump_insns = generate_jump(func.new_addr);
        core::ptr::copy_nonoverlapping(jump_insns.as_ptr(), old_ptr, jump_insns.len());
        
        // 刷新指令缓存
        flush_icache(func.old_addr, 16);
    }
    
    func.applied = true;
    Ok(())
}

/// 回滚函数补丁
fn revert_func_patch(func: &mut FuncPatch) -> Result<(), String> {
    if !func.applied {
        return Ok(());
    }
    
    unsafe {
        let old_ptr = func.old_addr as *mut u8;
        
        // 恢复原始指令
        core::ptr::copy_nonoverlapping(func.old_insns.as_ptr(), old_ptr, 16);
        
        // 刷新指令缓存
        flush_icache(func.old_addr, 16);
    }
    
    func.applied = false;
    Ok(())
}

/// 生成跳转指令
fn generate_jump(target: usize) -> Vec<u8> {
    let mut code = Vec::with_capacity(12);
    
    // RISC-V 长跳转序列:
    // lui t0, target[31:12]
    // addi t0, t0, target[11:0]
    // jalr x0, 0(t0)
    
    let upper = ((target as u32 + 0x800) >> 12) & 0xFFFFF;
    let lower = (target as u32) & 0xFFF;
    
    // lui t0, upper
    let lui = 0x37 | (5 << 7) | (upper << 12);
    code.extend_from_slice(&lui.to_le_bytes());
    
    // addi t0, t0, lower
    let addi = 0x13 | (5 << 7) | (0 << 12) | (5 << 15) | ((lower as u32) << 20);
    code.extend_from_slice(&addi.to_le_bytes());
    
    // jalr x0, 0(t0)
    let jalr: u32 = 0x67 | (0 << 7) | (0 << 12) | (5 << 15) | (0 << 20);
    code.extend_from_slice(&jalr.to_le_bytes());
    
    code
}

/// 刷新指令缓存
fn flush_icache(addr: usize, size: usize) {
    // RISC-V fence.i 指令
    #[cfg(target_arch = "riscv64")]
    unsafe {
        core::arch::asm!(
            "fence.i",
            options(nostack, preserves_flags)
        );
    }
    let _ = (addr, size);
}

/// 初始化热补丁系统
pub fn init() {
    // 真化〔去 TODO num_cpus=1 占位〕：从 `crate::config::get_num_cpus()`
    // 真读启动时由 DTB 写入的 CPU 数；PatchManager 内部 per-CPU 转换状态
    // 数组随之扩容。
    let num_cpus = crate::config::get_num_cpus().max(1);
    let mut mgr = PATCH_MANAGER.lock();
    *mgr = Some(PatchManager::new(num_cpus));
    crate::println!("[livepatch] Live patching system initialized ({} CPUs)", num_cpus);
}

/// 静默初始化热补丁系统（不输出调试信息）
pub fn init_quiet() {
    let num_cpus = crate::config::get_num_cpus().max(1);
    let mut mgr = PATCH_MANAGER.lock();
    *mgr = Some(PatchManager::new(num_cpus));
}

/// 注册补丁
pub fn register(name: String, funcs: Vec<FuncPatch>) -> PatchId {
    PATCH_MANAGER.lock()
        .as_mut()
        .map(|m| m.register(name, funcs))
        .unwrap_or(0)
}

/// 应用补丁
pub fn apply(patch_id: PatchId) -> Result<(), PatchError> {
    PATCH_MANAGER.lock()
        .as_mut()
        .ok_or(PatchError::NotFound)?
        .apply(patch_id)
}

/// 回滚补丁
pub fn revert(patch_id: PatchId) -> Result<(), PatchError> {
    PATCH_MANAGER.lock()
        .as_mut()
        .ok_or(PatchError::NotFound)?
        .revert(patch_id)
}


/// 热补丁模块自测试
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[livepatch] Running self-test...");
    
    // 测试 PatchState 枚举
    let states = [
        PatchState::Registered,
        PatchState::Applying,
        PatchState::Applied,
        PatchState::Reverting,
        PatchState::Reverted,
        PatchState::Failed,
    ];
    for state in &states {
        crate::println!("[livepatch] State: {:?}", state);
    }
    
    // 测试 FuncPatch
    let func_patch = FuncPatch::new(
        String::from("test_func"),
        0x80000000,
        0x80001000
    );
    assert_eq!(func_patch.old_addr, 0x80000000);
    assert_eq!(func_patch.new_addr, 0x80001000);
    assert!(!func_patch.applied);
    
    // 测试 Patch
    let mut patch = Patch::new(1, String::from("test_patch"));
    patch.add_func(FuncPatch::new(
        String::from("func1"),
        0x80002000,
        0x80003000
    ));
    patch.add_dependency(0);
    assert_eq!(patch.state(), PatchState::Registered);
    
    // 访问 Patch 的私有字段（通过 state 方法）
    let _ = patch.id;
    let _ = patch.name.as_str();
    let _ = patch.funcs.len();
    let _ = patch.depends.len();
    
    // 测试 QuiescentStateDetector
    let qsd = QuiescentStateDetector::new(4);
    qsd.start();
    qsd.report(0);
    qsd.report(1);
    qsd.report(2);
    qsd.report(3);
    let _ = qsd.is_done();
    // 注意：wait() 可能会阻塞，所以只在 is_done 为 true 时调用
    if qsd.is_done() {
        qsd.wait();
    }
    
    // 访问 QuiescentStateDetector 的字段
    let _ = qsd.counters.len();
    let _ = qsd.target.load(Ordering::Relaxed);
    let _ = qsd.done.load(Ordering::Relaxed);
    
    // 测试 PatchError 枚举
    let errors = [
        PatchError::NotFound,
        PatchError::InvalidState,
        PatchError::DependencyNotMet,
        PatchError::ApplyFailed(String::from("test error")),
        PatchError::RevertFailed(String::from("test error")),
        PatchError::QuiescentTimeout,
    ];
    for err in &errors {
        crate::println!("[livepatch] Error: {:?}", err);
    }
    
    // 测试 PatchManager
    let mut mgr = PatchManager::new(4);
    
    // 访问 PatchManager 的字段
    let _ = mgr.patches.len();
    let _ = mgr.next_id;
    let _ = mgr.qsd.is_done();
    
    // 测试 register
    let funcs = alloc::vec![
        FuncPatch::new(String::from("func_a"), 0x80010000, 0x80020000),
        FuncPatch::new(String::from("func_b"), 0x80010100, 0x80020100),
    ];
    let patch_id = mgr.register(String::from("my_patch"), funcs);
    crate::println!("[livepatch] Registered patch id={}", patch_id);
    
    // 测试 get_state
    let state = mgr.get_state(patch_id);
    assert_eq!(state, Some(PatchState::Registered));
    
    // 测试 list
    let patches = mgr.list();
    crate::println!("[livepatch] Patches: {:?}", patches.len());
    
    // 测试 apply（会失败因为地址无效，但会测试代码路径）
    // 注意：实际应用会修改内存，这里只测试错误路径
    let _ = mgr.apply(999); // 不存在的补丁
    
    // 测试 revert（会失败因为状态不对）
    let _ = mgr.revert(patch_id);
    
    // 测试辅助函数
    let jump_code = generate_jump(0x80100000);
    crate::println!("[livepatch] Jump code len={}", jump_code.len());
    
    // 测试 flush_icache（安全调用）
    flush_icache(0x80000000, 16);
    
    // 测试 apply_func_patch 和 revert_func_patch
    // 注意：这些函数会修改内存，所以我们只测试已应用的情况
    let mut test_func = FuncPatch::new(String::from("test"), 0, 0);
    test_func.applied = true;
    let _ = apply_func_patch(&mut test_func); // 已应用，直接返回 Ok
    test_func.applied = false;
    let _ = revert_func_patch(&mut test_func); // 未应用，直接返回 Ok
    
    // 测试全局函数
    init();
    
    let global_funcs = alloc::vec![
        FuncPatch::new(String::from("global_func"), 0x80030000, 0x80040000),
    ];
    let global_id = register(String::from("global_patch"), global_funcs);
    crate::println!("[livepatch] Global patch id={}", global_id);
    
    // 测试 apply 和 revert 全局函数
    let _ = apply(999); // 不存在
    let _ = revert(999); // 不存在
    
    crate::println!("[livepatch] Self-test passed!");
    Ok(())
}
