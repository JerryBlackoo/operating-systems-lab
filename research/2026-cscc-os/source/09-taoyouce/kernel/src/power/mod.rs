//! 电源管理
//!
//! 实现 DVFS、睡眠状态和 tickless 内核

use core::sync::atomic::{AtomicU32, AtomicBool, Ordering};
use crate::sync::SpinLock;

/// CPU 频率级别
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FreqLevel {
    /// 最低频率
    Lowest,
    /// 低频率
    Low,
    /// 中等频率
    Medium,
    /// 高频率
    High,
    /// 最高频率（性能模式）
    Highest,
}

impl FreqLevel {
    /// 获取频率比例（相对于最高频率）
    pub fn ratio(&self) -> u32 {
        match self {
            FreqLevel::Lowest => 25,
            FreqLevel::Low => 50,
            FreqLevel::Medium => 75,
            FreqLevel::High => 90,
            FreqLevel::Highest => 100,
        }
    }
}

/// C-state（CPU 空闲状态）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CState {
    /// C0: 运行状态
    C0,
    /// C1: 停止时钟
    C1,
    /// C2: 停止时钟 + 缓存刷新
    C2,
    /// C3: 深度睡眠
    C3,
}

impl CState {
    /// 获取退出延迟（微秒）
    pub fn exit_latency_us(&self) -> u32 {
        match self {
            CState::C0 => 0,
            CState::C1 => 1,
            CState::C2 => 10,
            CState::C3 => 100,
        }
    }
    
    /// 获取功耗比例
    pub fn power_ratio(&self) -> u32 {
        match self {
            CState::C0 => 100,
            CState::C1 => 50,
            CState::C2 => 20,
            CState::C3 => 5,
        }
    }
}

/// 电源策略
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerPolicy {
    /// 性能优先
    Performance,
    /// 平衡
    Balanced,
    /// 省电
    PowerSave,
}

/// CPU 电源状态
pub struct CpuPowerState {
    /// 当前频率级别
    freq_level: FreqLevel,
    /// 当前 C-state
    c_state: CState,
    /// 是否在线
    online: AtomicBool,
}

impl CpuPowerState {
    pub fn new() -> Self {
        Self {
            freq_level: FreqLevel::Medium,
            c_state: CState::C0,
            online: AtomicBool::new(true),
        }
    }
}

/// DVFS 控制器
pub struct DvfsController {
    /// 电源策略
    policy: PowerPolicy,
    /// 目标频率级别
    target_freq: FreqLevel,
    /// 负载历史
    load_history: [u32; 8],
    /// 历史索引
    history_idx: usize,
}

impl DvfsController {
    pub fn new() -> Self {
        Self {
            policy: PowerPolicy::Balanced,
            target_freq: FreqLevel::Medium,
            load_history: [50; 8],
            history_idx: 0,
        }
    }
    
    /// 设置电源策略
    pub fn set_policy(&mut self, policy: PowerPolicy) {
        self.policy = policy;
        self.update_target_freq();
    }
    
    /// 更新负载
    pub fn update_load(&mut self, load: u32) {
        self.load_history[self.history_idx] = load.min(100);
        self.history_idx = (self.history_idx + 1) % self.load_history.len();
        self.update_target_freq();
    }
    
    /// 获取平均负载
    fn avg_load(&self) -> u32 {
        let sum: u32 = self.load_history.iter().sum();
        sum / self.load_history.len() as u32
    }
    
    /// 更新目标频率
    fn update_target_freq(&mut self) {
        let load = self.avg_load();
        
        self.target_freq = match self.policy {
            PowerPolicy::Performance => FreqLevel::Highest,
            PowerPolicy::PowerSave => {
                if load < 20 {
                    FreqLevel::Lowest
                } else if load < 50 {
                    FreqLevel::Low
                } else {
                    FreqLevel::Medium
                }
            }
            PowerPolicy::Balanced => {
                if load < 20 {
                    FreqLevel::Low
                } else if load < 50 {
                    FreqLevel::Medium
                } else if load < 80 {
                    FreqLevel::High
                } else {
                    FreqLevel::Highest
                }
            }
        };
    }
    
    /// 获取目标频率
    pub fn target_freq(&self) -> FreqLevel {
        self.target_freq
    }
}

/// 空闲状态选择器
pub struct IdleSelector {
    /// 预测的空闲时间（微秒）
    predicted_idle_us: u32,
    /// 上次实际空闲时间
    last_idle_us: u32,
}

impl IdleSelector {
    pub fn new() -> Self {
        Self {
            predicted_idle_us: 100,
            last_idle_us: 100,
        }
    }
    
    /// 选择最佳 C-state
    pub fn select(&self) -> CState {
        let predicted = self.predicted_idle_us;
        
        // 选择退出延迟小于预测空闲时间的最深 C-state
        if predicted > CState::C3.exit_latency_us() * 3 {
            CState::C3
        } else if predicted > CState::C2.exit_latency_us() * 3 {
            CState::C2
        } else if predicted > CState::C1.exit_latency_us() * 3 {
            CState::C1
        } else {
            CState::C0
        }
    }
    
    /// 更新预测
    pub fn update(&mut self, actual_idle_us: u32) {
        self.last_idle_us = actual_idle_us;
        // 简单的指数移动平均
        self.predicted_idle_us = (self.predicted_idle_us * 3 + actual_idle_us) / 4;
    }
}

/// Tickless 内核支持
pub struct TicklessKernel {
    /// 是否启用
    enabled: AtomicBool,
    /// 下一个事件时间（tick）
    next_event: AtomicU32,
    /// 当前 tick
    current_tick: AtomicU32,
}

impl TicklessKernel {
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            next_event: AtomicU32::new(0),
            current_tick: AtomicU32::new(0),
        }
    }
    
    /// 启用/禁用 tickless
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }
    
    /// 是否启用
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }
    
    /// 设置下一个事件
    pub fn set_next_event(&self, tick: u32) {
        self.next_event.store(tick, Ordering::SeqCst);
    }
    
    /// 获取下一个事件
    pub fn next_event(&self) -> u32 {
        self.next_event.load(Ordering::SeqCst)
    }
    
    /// 计算可以跳过的 tick 数
    pub fn ticks_to_skip(&self) -> u32 {
        let current = self.current_tick.load(Ordering::SeqCst);
        let next = self.next_event.load(Ordering::SeqCst);
        
        if next > current {
            next - current
        } else {
            0
        }
    }
    
    /// 更新当前 tick
    pub fn advance(&self, ticks: u32) {
        self.current_tick.fetch_add(ticks, Ordering::SeqCst);
    }
}

/// 全局电源管理器
static POWER_MANAGER: SpinLock<Option<PowerManager>> = SpinLock::new(None);

/// 电源管理器
pub struct PowerManager {
    /// DVFS 控制器
    dvfs: DvfsController,
    /// 空闲选择器
    idle_selector: IdleSelector,
    /// Tickless 支持
    tickless: TicklessKernel,
    /// 每 CPU 状态
    cpu_states: alloc::vec::Vec<CpuPowerState>,
}

impl PowerManager {
    fn new(num_cpus: usize) -> Self {
        let mut cpu_states = alloc::vec::Vec::with_capacity(num_cpus);
        for _ in 0..num_cpus {
            cpu_states.push(CpuPowerState::new());
        }
        
        Self {
            dvfs: DvfsController::new(),
            idle_selector: IdleSelector::new(),
            tickless: TicklessKernel::new(),
            cpu_states,
        }
    }
    
    /// 进入空闲状态
    pub fn enter_idle(&mut self, cpu_id: usize) {
        let c_state = self.idle_selector.select();
        
        if cpu_id < self.cpu_states.len() {
            self.cpu_states[cpu_id].c_state = c_state;
        }
        
        match c_state {
            CState::C0 => {
                // 忙等待
            }
            CState::C1 | CState::C2 | CState::C3 => {
                // WFI 指令
                crate::arch::asm_helpers::wfi_once();
            }
        }
    }
    
    /// 退出空闲状态
    pub fn exit_idle(&mut self, cpu_id: usize, idle_us: u32) {
        if cpu_id < self.cpu_states.len() {
            self.cpu_states[cpu_id].c_state = CState::C0;
        }
        self.idle_selector.update(idle_us);
    }
    
    /// 更新 CPU 负载
    pub fn update_load(&mut self, load: u32) {
        self.dvfs.update_load(load);
    }
    
    /// 设置电源策略
    pub fn set_policy(&mut self, policy: PowerPolicy) {
        self.dvfs.set_policy(policy);
    }
}

/// 初始化电源管理
pub fn init() {
    // 真化〔去 TODO num_cpus=1 占位〕：从 `crate::config::get_num_cpus()` 真读
    // 启动时由 DTB 写入的 CPU 数；PowerManager 内部 per-CPU 数组随之扩容。
    let num_cpus = crate::config::get_num_cpus().max(1);
    let mut mgr = POWER_MANAGER.lock();
    *mgr = Some(PowerManager::new(num_cpus));
    crate::println!("[power] Power management initialized ({} CPUs)", num_cpus);
}

/// 静默初始化电源管理（不输出调试信息）
pub fn init_quiet() {
    let num_cpus = crate::config::get_num_cpus().max(1);
    let mut mgr = POWER_MANAGER.lock();
    *mgr = Some(PowerManager::new(num_cpus));
}

/// 进入空闲状态
pub fn enter_idle(cpu_id: usize) {
    if let Some(mgr) = POWER_MANAGER.lock().as_mut() {
        mgr.enter_idle(cpu_id);
    }
}

/// 设置电源策略
pub fn set_policy(policy: PowerPolicy) {
    if let Some(mgr) = POWER_MANAGER.lock().as_mut() {
        mgr.set_policy(policy);
    }
}

/// 电源管理模块自测试
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[power] Running self-test...");
    
    // 测试 FreqLevel
    let freq_levels = [
        FreqLevel::Lowest,
        FreqLevel::Low,
        FreqLevel::Medium,
        FreqLevel::High,
        FreqLevel::Highest,
    ];
    for level in &freq_levels {
        let ratio = level.ratio();
        assert!(ratio > 0 && ratio <= 100);
    }
    
    // 测试 CState
    let c_states = [CState::C0, CState::C1, CState::C2, CState::C3];
    for state in &c_states {
        let latency = state.exit_latency_us();
        let power = state.power_ratio();
        crate::println!("[power] {:?}: latency={}us, power={}%", state, latency, power);
    }
    
    // 测试 PowerPolicy
    let policies = [
        PowerPolicy::Performance,
        PowerPolicy::Balanced,
        PowerPolicy::PowerSave,
    ];
    
    // 测试 CpuPowerState
    let cpu_state = CpuPowerState::new();
    let _ = cpu_state.freq_level;
    let _ = cpu_state.c_state;
    let _ = cpu_state.online.load(Ordering::SeqCst);
    
    // 测试 DvfsController
    let mut dvfs = DvfsController::new();
    for policy in &policies {
        dvfs.set_policy(*policy);
    }
    dvfs.update_load(30);
    dvfs.update_load(60);
    dvfs.update_load(90);
    let _ = dvfs.avg_load();
    dvfs.update_target_freq();
    let target = dvfs.target_freq();
    crate::println!("[power] DVFS target freq: {:?}", target);
    
    // 测试 IdleSelector
    let mut idle_sel = IdleSelector::new();
    let selected = idle_sel.select();
    crate::println!("[power] Selected C-state: {:?}", selected);
    idle_sel.update(50);
    idle_sel.update(200);
    let _ = idle_sel.predicted_idle_us;
    let _ = idle_sel.last_idle_us;
    
    // 测试 TicklessKernel
    let tickless = TicklessKernel::new();
    tickless.set_enabled(true);
    assert!(tickless.is_enabled());
    tickless.set_next_event(100);
    assert_eq!(tickless.next_event(), 100);
    let skip = tickless.ticks_to_skip();
    crate::println!("[power] Ticks to skip: {}", skip);
    tickless.advance(10);
    
    // 测试 PowerManager
    let mut pm = PowerManager::new(2);
    pm.update_load(50);
    pm.set_policy(PowerPolicy::Balanced);
    // 测试 enter_idle（不实际执行 WFI，只测试状态更新）
    // 注意：enter_idle 内部会执行 WFI，这里通过直接设置状态来测试
    if pm.cpu_states.len() > 0 {
        pm.cpu_states[0].c_state = pm.idle_selector.select();
    }
    pm.exit_idle(0, 100);
    let _ = pm.dvfs.target_freq();
    let _ = pm.idle_selector.select();
    let _ = pm.tickless.is_enabled();
    let _ = pm.cpu_states.len();
    
    // 测试全局函数
    init();
    set_policy(PowerPolicy::Performance);
    // 测试 enter_idle 函数存在（不实际调用，因为会执行 WFI）
    let _ = enter_idle as fn(usize);
    
    crate::println!("[power] Self-test passed!");
    Ok(())
}
