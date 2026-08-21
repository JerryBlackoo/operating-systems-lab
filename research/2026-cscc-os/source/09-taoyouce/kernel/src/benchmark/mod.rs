//! 内核性能基准测试框架
//!
//! 提供内核级别的性能测量和基准测试功能。

#![allow(dead_code)]

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::sync::atomic::{AtomicU64, Ordering};

/// 基准测试结果
#[derive(Debug, Clone)]
pub struct BenchmarkResult {
    /// 测试名称
    pub name: String,
    /// 迭代次数
    pub iterations: u64,
    /// 总耗时（纳秒）
    pub total_ns: u64,
    /// 最小耗时（纳秒）
    pub min_ns: u64,
    /// 最大耗时（纳秒）
    pub max_ns: u64,
    /// 平均耗时（纳秒）
    pub avg_ns: u64,
    /// 标准差（纳秒）
    pub std_dev_ns: u64,
    /// 吞吐量（操作/秒）
    pub throughput: u64,
}

impl BenchmarkResult {
    /// 创建新的基准测试结果
    pub fn new(name: &str) -> Self {
        Self {
            name: String::from(name),
            iterations: 0,
            total_ns: 0,
            min_ns: u64::MAX,
            max_ns: 0,
            avg_ns: 0,
            std_dev_ns: 0,
            throughput: 0,
        }
    }
    
    /// 记录一次测量
    pub fn record(&mut self, duration_ns: u64) {
        self.iterations += 1;
        self.total_ns += duration_ns;
        self.min_ns = self.min_ns.min(duration_ns);
        self.max_ns = self.max_ns.max(duration_ns);
        self.avg_ns = self.total_ns / self.iterations;
        
        // 计算吞吐量（操作/秒）
        if self.avg_ns > 0 {
            self.throughput = 1_000_000_000 / self.avg_ns;
        }
    }
    
    /// 转换为 JSON 格式
    pub fn to_json(&self) -> String {
        format!(
            r#"{{"name":"{}","iterations":{},"total_ns":{},"min_ns":{},"max_ns":{},"avg_ns":{},"throughput":{}}}"#,
            self.name, self.iterations, self.total_ns, self.min_ns, self.max_ns, self.avg_ns, self.throughput
        )
    }
}

/// 基准测试运行器
pub struct BenchmarkRunner {
    /// 测试结果列表
    results: Vec<BenchmarkResult>,
    /// 当前测试
    current: Option<BenchmarkResult>,
    /// 开始时间
    start_time: u64,
}

impl BenchmarkRunner {
    /// 创建新的基准测试运行器
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
            current: None,
            start_time: 0,
        }
    }
    
    /// 开始一个新的基准测试
    pub fn begin(&mut self, name: &str) {
        self.current = Some(BenchmarkResult::new(name));
        self.start_time = get_time_ns();
    }
    
    /// 记录一次迭代
    pub fn iteration(&mut self) {
        let now = get_time_ns();
        let duration = now - self.start_time;
        
        if let Some(ref mut result) = self.current {
            result.record(duration);
        }
        
        self.start_time = now;
    }
    
    /// 结束当前基准测试
    pub fn end(&mut self) {
        if let Some(result) = self.current.take() {
            self.results.push(result);
        }
    }
    
    /// 运行基准测试函数
    pub fn run<F>(&mut self, name: &str, iterations: u64, mut f: F)
    where
        F: FnMut(),
    {
        self.begin(name);
        
        for _ in 0..iterations {
            f();
            self.iteration();
        }
        
        self.end();
    }
    
    /// 获取所有结果
    pub fn results(&self) -> &[BenchmarkResult] {
        &self.results
    }
    
    /// 打印结果
    pub fn print_results(&self) {
        crate::println!("\n╔═══════════════════════════════════════════════════════════╗");
        crate::println!("║                    基准测试结果                            ║");
        crate::println!("╚═══════════════════════════════════════════════════════════╝");
        
        for result in &self.results {
            crate::println!("\n┌─ {} ─────────────────────────────", result.name);
            crate::println!("  迭代次数: {}", result.iterations);
            crate::println!("  总耗时: {} ns", result.total_ns);
            crate::println!("  最小: {} ns", result.min_ns);
            crate::println!("  最大: {} ns", result.max_ns);
            crate::println!("  平均: {} ns", result.avg_ns);
            crate::println!("  吞吐量: {} ops/s", result.throughput);
        }
    }
    
    /// 生成 JSON 报告
    pub fn to_json(&self) -> String {
        let results: Vec<String> = self.results.iter().map(|r| r.to_json()).collect();
        format!("{{\"benchmarks\":[{}]}}", results.join(","))
    }
}

impl Default for BenchmarkRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// 获取当前时间（纳秒）
/// 
/// 使用 RISC-V 的 time CSR 获取高精度时间戳
pub fn get_time_ns() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        let time: u64;
        time = crate::arch::asm_helpers::rdtime();
        // 假设时钟频率为 10MHz（QEMU virt 默认）
        // 转换为纳秒：time * 100
        time * 100
    }
    
    #[cfg(not(target_arch = "riscv64"))]
    {
        0
    }
}

/// 全局基准测试计数器
static BENCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// 增加基准测试计数器
pub fn increment_counter() {
    BENCH_COUNTER.fetch_add(1, Ordering::Relaxed);
}

/// 获取基准测试计数器值
pub fn get_counter() -> u64 {
    BENCH_COUNTER.load(Ordering::Relaxed)
}

/// 重置基准测试计数器
pub fn reset_counter() {
    BENCH_COUNTER.store(0, Ordering::Relaxed);
}

/// 上下文切换基准测试
pub fn bench_context_switch() -> BenchmarkResult {
    let mut result = BenchmarkResult::new("context_switch");
    
    for _ in 0..1000 {
        let start = get_time_ns();
        
        // 模拟上下文切换（保存/恢复寄存器）
        #[cfg(target_arch = "riscv64")]
        unsafe {
            core::arch::asm!(
                "mv t0, sp",
                "mv t1, ra",
                "mv sp, t0",
                "mv ra, t1",
                out("t0") _,
                out("t1") _,
            );
        }
        #[cfg(target_arch = "loongarch64")]
        unsafe {
            core::arch::asm!(
                "move $t0, $sp",
                "move $t1, $ra",
                "move $sp, $t0",
                "move $ra, $t1",
                out("$t0") _,
                out("$t1") _,
            );
        }
        
        let duration = get_time_ns() - start;
        result.record(duration);
    }
    
    result
}

/// 内存分配基准测试
pub fn bench_memory_alloc() -> BenchmarkResult {
    use alloc::vec;
    
    let mut result = BenchmarkResult::new("memory_alloc");
    
    for _ in 0..1000 {
        let start = get_time_ns();
        
        // 分配和释放 4KB
        let _v: Vec<u8> = vec![0u8; 4096];
        
        let duration = get_time_ns() - start;
        result.record(duration);
    }
    
    result
}

/// 自旋锁基准测试
pub fn bench_spinlock() -> BenchmarkResult {
    use crate::sync::SpinLock;
    
    let mut result = BenchmarkResult::new("spinlock");
    let lock = SpinLock::new(0u64);
    
    for _ in 0..10000 {
        let start = get_time_ns();
        
        {
            let mut guard = lock.lock();
            *guard += 1;
        }
        
        let duration = get_time_ns() - start;
        result.record(duration);
    }
    
    result
}

/// 运行所有基准测试
pub fn run_all() -> Vec<BenchmarkResult> {
    crate::println!("[benchmark] Running all benchmarks...");
    
    let mut results = Vec::new();
    
    results.push(bench_context_switch());
    results.push(bench_memory_alloc());
    results.push(bench_spinlock());
    
    results
}

/// 基准测试自测试
pub fn self_test() {
    crate::println!("[benchmark] Running self-test...");
    
    // 测试 BenchmarkResult
    let mut result = BenchmarkResult::new("test");
    result.record(100);
    result.record(200);
    result.record(150);
    assert_eq!(result.iterations, 3);
    assert_eq!(result.min_ns, 100);
    assert_eq!(result.max_ns, 200);
    
    // 测试 JSON 输出
    let json = result.to_json();
    assert!(json.contains("\"name\":\"test\""));
    
    // 测试 BenchmarkRunner
    let mut runner = BenchmarkRunner::new();
    runner.run("simple", 10, || {
        // 空操作
    });
    assert_eq!(runner.results().len(), 1);
    
    // 测试计数器
    reset_counter();
    increment_counter();
    increment_counter();
    assert_eq!(get_counter(), 2);
    
    // 测试时间获取
    let t1 = get_time_ns();
    let t2 = get_time_ns();
    assert!(t2 >= t1);
    
    crate::println!("[benchmark] Self-test passed!");
}
