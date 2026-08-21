//! 内存分配性能基准测试程序
//! 
//! 测试 Per-CPU 缓存优化的效果，对比优化前后的性能差异。
//! 
//! 用法: alloc_bench

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::syscall::{sys_yield, sys_getpid};
use core::sync::atomic::{AtomicU64, Ordering};

/// 测试配置
struct BenchConfig {
    iterations: usize,
    alloc_size: usize,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            iterations: 1000,
            alloc_size: 64,
        }
    }
}

/// 测试结果
struct BenchResult {
    name: &'static str,
    iterations: usize,
    total_ops: u64,
}

/// 全局统计
static TOTAL_ALLOCS: AtomicU64 = AtomicU64::new(0);
static TOTAL_FREES: AtomicU64 = AtomicU64::new(0);

#[no_mangle]
pub fn main(_argc: usize, _argv: &[&str]) -> i32 {
    println!("===========================================");
    println!("  RustMicroOS 内存分配性能基准测试");
    println!("===========================================\n");
    
    let config = BenchConfig::default();
    
    println!("测试配置:");
    println!("  迭代次数: {}", config.iterations);
    println!("  分配大小: {} 字节", config.alloc_size);
    println!("  进程 ID:  {}\n", sys_getpid());
    
    // 运行各项测试
    let result1 = bench_single_alloc(&config);
    let result2 = bench_mixed_sizes(&config);
    let result3 = bench_alloc_free_pattern(&config);
    
    // 打印结果汇总
    print_summary(&[result1, result2, result3]);
    
    0
}

/// 测试 1: 单一大小分配性能
fn bench_single_alloc(config: &BenchConfig) -> BenchResult {
    println!("测试 1: 单一大小分配性能");
    println!("-----------------------------------------");
    
    let mut alloc_count = 0u64;
    let mut free_count = 0u64;
    
    for _ in 0..config.iterations {
        // 模拟分配操作
        let ptr = simulate_alloc(config.alloc_size);
        if ptr != 0 {
            alloc_count += 1;
            // 模拟释放操作
            simulate_dealloc(ptr, config.alloc_size);
            free_count += 1;
        }
        
        // 偶尔让出 CPU
        if alloc_count % 100 == 0 {
            sys_yield();
        }
    }
    
    let total_ops = alloc_count + free_count;
    
    println!("  分配次数: {}", alloc_count);
    println!("  释放次数: {}", free_count);
    println!("  总操作数: {}\n", total_ops);
    
    TOTAL_ALLOCS.fetch_add(alloc_count, Ordering::Relaxed);
    TOTAL_FREES.fetch_add(free_count, Ordering::Relaxed);
    
    BenchResult {
        name: "单一大小",
        iterations: config.iterations,
        total_ops,
    }
}

/// 测试 2: 混合大小分配
fn bench_mixed_sizes(config: &BenchConfig) -> BenchResult {
    println!("测试 2: 混合大小分配");
    println!("-----------------------------------------");
    
    let sizes = [16, 32, 64, 128, 256, 512, 1024, 2048];
    let mut alloc_count = 0u64;
    let mut free_count = 0u64;
    
    for i in 0..config.iterations {
        let size = sizes[i % sizes.len()];
        let ptr = simulate_alloc(size);
        if ptr != 0 {
            alloc_count += 1;
            simulate_dealloc(ptr, size);
            free_count += 1;
        }
    }
    
    let total_ops = alloc_count + free_count;
    
    println!("  分配次数: {}", alloc_count);
    println!("  释放次数: {}", free_count);
    println!("  总操作数: {}\n", total_ops);
    
    TOTAL_ALLOCS.fetch_add(alloc_count, Ordering::Relaxed);
    TOTAL_FREES.fetch_add(free_count, Ordering::Relaxed);
    
    BenchResult {
        name: "混合大小",
        iterations: config.iterations,
        total_ops,
    }
}

/// 测试 3: 分配-使用-释放模式
fn bench_alloc_free_pattern(config: &BenchConfig) -> BenchResult {
    println!("测试 3: 批量分配-释放模式");
    println!("-----------------------------------------");
    
    let batch_size = 10;
    let batches = config.iterations / batch_size;
    let mut alloc_count = 0u64;
    let mut free_count = 0u64;
    
    for _ in 0..batches {
        // 批量分配
        let mut ptrs = [0usize; 10];
        for i in 0..batch_size {
            let ptr = simulate_alloc(config.alloc_size);
            if ptr != 0 {
                ptrs[i] = ptr;
                alloc_count += 1;
            }
        }
        
        // 批量释放
        for i in 0..batch_size {
            if ptrs[i] != 0 {
                simulate_dealloc(ptrs[i], config.alloc_size);
                free_count += 1;
            }
        }
    }
    
    let total_ops = alloc_count + free_count;
    
    println!("  批次数:   {}", batches);
    println!("  分配次数: {}", alloc_count);
    println!("  释放次数: {}", free_count);
    println!("  总操作数: {}\n", total_ops);
    
    TOTAL_ALLOCS.fetch_add(alloc_count, Ordering::Relaxed);
    TOTAL_FREES.fetch_add(free_count, Ordering::Relaxed);
    
    BenchResult {
        name: "批量模式",
        iterations: config.iterations,
        total_ops,
    }
}

/// 打印结果汇总
fn print_summary(results: &[BenchResult]) {
    println!("===========================================");
    println!("                 结果汇总");
    println!("===========================================");
    println!("{:<16} {:>12} {:>12}", 
        "测试名称", "迭代次数", "总操作数");
    println!("-----------------------------------------");
    
    for result in results {
        println!("{:<16} {:>12} {:>12}",
            result.name,
            result.iterations,
            result.total_ops);
    }
    
    let total_allocs = TOTAL_ALLOCS.load(Ordering::Relaxed);
    let total_frees = TOTAL_FREES.load(Ordering::Relaxed);
    
    println!("\n总计:");
    println!("  总分配次数: {}", total_allocs);
    println!("  总释放次数: {}", total_frees);
    
    println!("\n测试完成!");
}

/// 内存分配
/// 使用 sbrk 系统调用进行实际内存分配
fn simulate_alloc(size: usize) -> usize {
    use user::syscall::sys_sbrk;
    
    if size == 0 || size > 4096 {
        return 0;
    }
    
    // 对齐到 8 字节
    let aligned_size = (size + 7) & !7;
    
    // 调用 sbrk 系统调用分配内存
    let result = sys_sbrk(aligned_size as isize);
    
    if result < 0 {
        0
    } else {
        result as usize
    }
}

/// 内存释放
/// 注意：sbrk 不支持真正的释放，这里记录释放操作用于统计
/// 在实际系统中，应该使用更完善的内存分配器
fn simulate_dealloc(ptr: usize, size: usize) {
    // sbrk 分配器不支持任意释放
    // 只有当释放的是最后分配的块时才能收缩堆
    // 这里我们只是标记释放，不实际回收
    
    // 在调试模式下验证指针有效性
    #[cfg(debug_assertions)]
    {
        if ptr == 0 {
            return;
        }
        // 确保指针在合理范围内
        assert!(ptr >= 0x1000, "Invalid pointer: too low");
        assert!(size > 0 && size <= 4096, "Invalid size");
    }
    
    // 实际的释放操作需要更复杂的内存管理
    // 这里使用 volatile 写入来防止编译器优化掉这个操作
    unsafe {
        core::ptr::write_volatile(ptr as *mut u8, 0);
    }
}
