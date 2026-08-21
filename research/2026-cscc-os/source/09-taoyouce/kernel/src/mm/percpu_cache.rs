//! Per-CPU 内存缓存
//!
//! 为每个 CPU 维护独立的内存缓存，减少多核系统的锁竞争。
//!
//! # 特性
//! - 本地缓存优先分配
//! - 批量从全局获取/归还
//! - 水位控制
//! - 统计信息

#![allow(dead_code)]

use core::sync::atomic::{AtomicUsize, AtomicU64, Ordering};
use crate::sync::SpinLock;

/// 最大 CPU 数量（统一 per-CPU 底座，历史模块继续 re-export）。
pub use crate::percpu::MAX_CPUS;

/// 大小类数量
pub const NUM_SIZE_CLASSES: usize = 12;

/// 页缓存大小
pub const PAGE_CACHE_SIZE: usize = 16;

/// 批量获取大小
pub const BATCH_SIZE: usize = 32;

/// 大小类定义（字节）
pub const SIZE_CLASSES: [usize; NUM_SIZE_CLASSES] = [
    16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768
];

/// 空闲对象链表节点
#[repr(C)]
struct FreeObject {
    next: *mut FreeObject,
}

/// 大小类缓存
pub struct SizeClassCache {
    /// 对象大小
    object_size: usize,
    /// 空闲对象链表头
    free_list: *mut FreeObject,
    /// 缓存中的对象数
    count: usize,
    /// 最大缓存数
    max_count: usize,
    /// 低水位（触发填充）
    low_watermark: usize,
    /// 高水位（触发归还）
    high_watermark: usize,
}

// SAFETY: SizeClassCache 只在持有锁时访问，且 free_list 指向的内存由分配器管理
unsafe impl Send for SizeClassCache {}

impl SizeClassCache {
    /// 创建新的大小类缓存
    pub const fn new(object_size: usize) -> Self {
        let max_count = if object_size <= 256 { 64 } else { 32 };
        Self {
            object_size,
            free_list: core::ptr::null_mut(),
            count: 0,
            max_count,
            low_watermark: max_count / 4,
            high_watermark: max_count * 3 / 4,
        }
    }

    /// 从缓存分配
    pub fn pop(&mut self) -> Option<*mut u8> {
        if self.free_list.is_null() {
            return None;
        }

        let obj = self.free_list;
        unsafe {
            self.free_list = (*obj).next;
        }
        self.count -= 1;

        Some(obj as *mut u8)
    }

    /// 归还到缓存
    pub fn push(&mut self, ptr: *mut u8) {
        let obj = ptr as *mut FreeObject;
        unsafe {
            (*obj).next = self.free_list;
        }
        self.free_list = obj;
        self.count += 1;
    }

    /// 检查是否需要填充
    pub fn needs_refill(&self) -> bool {
        self.count < self.low_watermark
    }

    /// 检查是否需要归还
    pub fn needs_flush(&self) -> bool {
        self.count > self.high_watermark
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 是否已满
    pub fn is_full(&self) -> bool {
        self.count >= self.max_count
    }

    /// 获取对象大小
    pub fn object_size(&self) -> usize {
        self.object_size
    }

    /// 获取当前数量
    pub fn count(&self) -> usize {
        self.count
    }
}

/// 页缓存
pub struct PageCache {
    /// 缓存的页列表
    pages: [usize; PAGE_CACHE_SIZE],
    /// 缓存数量
    count: usize,
}

impl PageCache {
    /// 创建新的页缓存
    pub const fn new() -> Self {
        Self {
            pages: [0; PAGE_CACHE_SIZE],
            count: 0,
        }
    }

    /// 分配页
    pub fn pop(&mut self) -> Option<usize> {
        if self.count == 0 {
            return None;
        }
        self.count -= 1;
        Some(self.pages[self.count])
    }

    /// 归还页
    pub fn push(&mut self, page: usize) -> bool {
        if self.count >= PAGE_CACHE_SIZE {
            return false;
        }
        self.pages[self.count] = page;
        self.count += 1;
        true
    }

    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// 是否已满
    pub fn is_full(&self) -> bool {
        self.count >= PAGE_CACHE_SIZE
    }
}

/// 缓存统计信息
#[derive(Default)]
pub struct CacheStats {
    /// 本地缓存命中次数
    pub local_hits: AtomicU64,
    /// 本地缓存未命中次数
    pub local_misses: AtomicU64,
    /// 从全局获取次数
    pub global_refills: AtomicU64,
    /// 归还到全局次数
    pub global_flushes: AtomicU64,
    /// 分配总数
    pub allocations: AtomicU64,
    /// 释放总数
    pub deallocations: AtomicU64,
}

impl CacheStats {
    pub const fn new() -> Self {
        Self {
            local_hits: AtomicU64::new(0),
            local_misses: AtomicU64::new(0),
            global_refills: AtomicU64::new(0),
            global_flushes: AtomicU64::new(0),
            allocations: AtomicU64::new(0),
            deallocations: AtomicU64::new(0),
        }
    }

    /// 记录本地命中
    pub fn record_local_hit(&self) {
        self.local_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录本地未命中
    pub fn record_local_miss(&self) {
        self.local_misses.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录全局填充
    pub fn record_global_refill(&self) {
        self.global_refills.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录全局归还
    pub fn record_global_flush(&self) {
        self.global_flushes.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录分配
    pub fn record_allocation(&self) {
        self.allocations.fetch_add(1, Ordering::Relaxed);
    }

    /// 记录释放
    pub fn record_deallocation(&self) {
        self.deallocations.fetch_add(1, Ordering::Relaxed);
    }

    /// 获取命中率
    pub fn hit_rate(&self) -> f64 {
        let hits = self.local_hits.load(Ordering::Relaxed);
        let misses = self.local_misses.load(Ordering::Relaxed);
        let total = hits + misses;
        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }
}

/// 单个 CPU 的本地缓存
pub struct CpuLocalCache {
    /// 小对象缓存（按大小类分）
    size_classes: [SizeClassCache; NUM_SIZE_CLASSES],
    /// 页缓存
    page_cache: PageCache,
    /// 统计信息
    stats: CacheStats,
    /// CPU ID
    cpu_id: usize,
}

impl CpuLocalCache {
    /// 创建新的 CPU 本地缓存
    pub const fn new(cpu_id: usize) -> Self {
        Self {
            size_classes: [
                SizeClassCache::new(SIZE_CLASSES[0]),
                SizeClassCache::new(SIZE_CLASSES[1]),
                SizeClassCache::new(SIZE_CLASSES[2]),
                SizeClassCache::new(SIZE_CLASSES[3]),
                SizeClassCache::new(SIZE_CLASSES[4]),
                SizeClassCache::new(SIZE_CLASSES[5]),
                SizeClassCache::new(SIZE_CLASSES[6]),
                SizeClassCache::new(SIZE_CLASSES[7]),
                SizeClassCache::new(SIZE_CLASSES[8]),
                SizeClassCache::new(SIZE_CLASSES[9]),
                SizeClassCache::new(SIZE_CLASSES[10]),
                SizeClassCache::new(SIZE_CLASSES[11]),
            ],
            page_cache: PageCache::new(),
            stats: CacheStats::new(),
            cpu_id,
        }
    }

    /// 分配对象
    pub fn alloc(&mut self, size: usize) -> Option<*mut u8> {
        let class = size_to_class(size)?;
        
        // 尝试从本地缓存分配
        if let Some(ptr) = self.size_classes[class].pop() {
            self.stats.record_local_hit();
            self.stats.record_allocation();
            return Some(ptr);
        }

        // 本地缓存为空
        self.stats.record_local_miss();
        None
    }

    /// 释放对象
    pub fn dealloc(&mut self, ptr: *mut u8, size: usize) -> bool {
        let class = match size_to_class(size) {
            Some(c) => c,
            None => return false,
        };

        // 检查缓存是否已满
        if self.size_classes[class].is_full() {
            return false;
        }

        self.size_classes[class].push(ptr);
        self.stats.record_deallocation();
        true
    }

    /// 填充大小类缓存
    pub fn refill(&mut self, class: usize, objects: &[*mut u8]) {
        for &ptr in objects {
            if self.size_classes[class].is_full() {
                break;
            }
            self.size_classes[class].push(ptr);
        }
        self.stats.record_global_refill();
    }

    /// 归还部分缓存到全局
    pub fn flush(&mut self, class: usize) -> alloc::vec::Vec<*mut u8> {
        let mut flushed = alloc::vec::Vec::new();
        let cache = &mut self.size_classes[class];
        
        // 归还一半
        let to_flush = cache.count / 2;
        for _ in 0..to_flush {
            if let Some(ptr) = cache.pop() {
                flushed.push(ptr);
            }
        }
        
        if !flushed.is_empty() {
            self.stats.record_global_flush();
        }
        
        flushed
    }

    /// 归还部分缓存到调用方提供的数组（零堆分配，用于 SLUB 批量回收）
    ///
    /// 弹出 count/2 个对象到 `buf`，返回实际弹出数量。
    pub fn flush_to_array(&mut self, class: usize, buf: &mut [*mut u8]) -> usize {
        let cache = &mut self.size_classes[class];
        let to_flush = (cache.count / 2).min(buf.len());
        let mut n = 0;
        for i in 0..to_flush {
            if let Some(ptr) = cache.pop() {
                buf[i] = ptr;
                n += 1;
            } else {
                break;
            }
        }
        if n > 0 {
            self.stats.record_global_flush();
        }
        n
    }

    /// 分配页
    pub fn alloc_page(&mut self) -> Option<usize> {
        if let Some(page) = self.page_cache.pop() {
            self.stats.record_local_hit();
            Some(page)
        } else {
            self.stats.record_local_miss();
            None
        }
    }

    /// 释放页
    pub fn dealloc_page(&mut self, page: usize) -> bool {
        if self.page_cache.push(page) {
            true
        } else {
            false
        }
    }

    /// 获取统计信息
    pub fn stats(&self) -> &CacheStats {
        &self.stats
    }

    /// 获取 CPU ID
    pub fn cpu_id(&self) -> usize {
        self.cpu_id
    }
}

/// Per-CPU 内存缓存管理器
pub struct PerCpuCache {
    /// 每个 CPU 的缓存
    caches: [SpinLock<CpuLocalCache>; MAX_CPUS],
    /// 当前 CPU 数量
    num_cpus: AtomicUsize,
}

impl PerCpuCache {
    /// 创建新的 Per-CPU 缓存
    pub const fn new() -> Self {
        Self {
            caches: [
                SpinLock::new(CpuLocalCache::new(0)),
                SpinLock::new(CpuLocalCache::new(1)),
                SpinLock::new(CpuLocalCache::new(2)),
                SpinLock::new(CpuLocalCache::new(3)),
                SpinLock::new(CpuLocalCache::new(4)),
                SpinLock::new(CpuLocalCache::new(5)),
                SpinLock::new(CpuLocalCache::new(6)),
                SpinLock::new(CpuLocalCache::new(7)),
                SpinLock::new(CpuLocalCache::new(8)),
                SpinLock::new(CpuLocalCache::new(9)),
                SpinLock::new(CpuLocalCache::new(10)),
                SpinLock::new(CpuLocalCache::new(11)),
                SpinLock::new(CpuLocalCache::new(12)),
                SpinLock::new(CpuLocalCache::new(13)),
                SpinLock::new(CpuLocalCache::new(14)),
                SpinLock::new(CpuLocalCache::new(15)),
            ],
            num_cpus: AtomicUsize::new(1),
        }
    }

    /// 设置 CPU 数量
    pub fn set_num_cpus(&self, num: usize) {
        self.num_cpus.store(num.min(MAX_CPUS), Ordering::Release);
    }

    /// 获取当前 CPU 的缓存
    pub fn get_cache(&self, cpu_id: usize) -> Option<&SpinLock<CpuLocalCache>> {
        if cpu_id < MAX_CPUS {
            Some(&self.caches[cpu_id])
        } else {
            None
        }
    }

    /// 分配对象
    pub fn alloc(&self, cpu_id: usize, size: usize) -> Option<*mut u8> {
        let cache = self.get_cache(cpu_id)?;
        cache.lock().alloc(size)
    }

    /// 释放对象
    pub fn dealloc(&self, cpu_id: usize, ptr: *mut u8, size: usize) -> bool {
        if let Some(cache) = self.get_cache(cpu_id) {
            cache.lock().dealloc(ptr, size)
        } else {
            false
        }
    }

    /// 获取总统计信息
    pub fn total_stats(&self) -> CacheStats {
        let stats = CacheStats::new();
        let num_cpus = self.num_cpus.load(Ordering::Acquire);
        
        for i in 0..num_cpus {
            let cache = self.caches[i].lock();
            let s = cache.stats();
            stats.local_hits.fetch_add(s.local_hits.load(Ordering::Relaxed), Ordering::Relaxed);
            stats.local_misses.fetch_add(s.local_misses.load(Ordering::Relaxed), Ordering::Relaxed);
            stats.global_refills.fetch_add(s.global_refills.load(Ordering::Relaxed), Ordering::Relaxed);
            stats.global_flushes.fetch_add(s.global_flushes.load(Ordering::Relaxed), Ordering::Relaxed);
            stats.allocations.fetch_add(s.allocations.load(Ordering::Relaxed), Ordering::Relaxed);
            stats.deallocations.fetch_add(s.deallocations.load(Ordering::Relaxed), Ordering::Relaxed);
        }
        
        stats
    }
}

/// 将大小转换为大小类索引
pub fn size_to_class(size: usize) -> Option<usize> {
    for (i, &class_size) in SIZE_CLASSES.iter().enumerate() {
        if size <= class_size {
            return Some(i);
        }
    }
    None
}

/// 将大小类索引转换为实际大小
pub fn class_to_size(class: usize) -> Option<usize> {
    SIZE_CLASSES.get(class).copied()
}

/// 全局 Per-CPU 缓存
static PERCPU_CACHE: PerCpuCache = PerCpuCache::new();

/// 初始化 Per-CPU 缓存
pub fn init(num_cpus: usize) {
    PERCPU_CACHE.set_num_cpus(num_cpus);
    crate::println!("[percpu] Per-CPU cache initialized for {} CPUs", num_cpus);
}

/// 获取全局 Per-CPU 缓存
pub fn get_cache() -> &'static PerCpuCache {
    &PERCPU_CACHE
}

/// 获取当前 CPU ID（统一 per-CPU 底座）。
#[inline]
pub fn current_cpu_id() -> usize {
    crate::percpu::current_cpu_id()
}

/// 设置当前 CPU ID（统一 per-CPU 底座）。
#[inline]
pub fn set_cpu_id(cpu_id: usize) {
    crate::percpu::set_cpu_id(cpu_id);
}


/// Per-CPU 缓存自测试
pub fn self_test() {
    crate::println!("[percpu_cache] Running self-test...");
    
    // 测试大小类转换
    let class = size_to_class(64).expect("size_to_class failed");
    let size = class_to_size(class).expect("class_to_size failed");
    assert!(size >= 64);
    crate::println!("[percpu_cache] Size class for 64 bytes: {} (actual size: {})", class, size);
    
    // 测试 SizeClassCache
    let cache = SizeClassCache::new(64);
    assert!(cache.is_empty());
    assert!(!cache.is_full());
    assert_eq!(cache.object_size(), 64);
    assert_eq!(cache.count(), 0);
    assert!(cache.needs_refill());
    assert!(!cache.needs_flush());
    crate::println!("[percpu_cache] SizeClassCache basic tests passed");
    
    // 测试 PageCache
    let mut page_cache = PageCache::new();
    assert!(page_cache.is_empty());
    assert!(!page_cache.is_full());
    
    // 测试 push/pop
    assert!(page_cache.push(0x1000));
    assert!(!page_cache.is_empty());
    let page = page_cache.pop();
    assert_eq!(page, Some(0x1000));
    assert!(page_cache.is_empty());
    crate::println!("[percpu_cache] PageCache tests passed");
    
    // 测试 CacheStats
    let stats = CacheStats::new();
    stats.record_local_hit();
    stats.record_local_miss();
    stats.record_global_refill();
    stats.record_global_flush();
    stats.record_allocation();
    stats.record_deallocation();
    let hit_rate = stats.hit_rate();
    assert!(hit_rate >= 0.0 && hit_rate <= 1.0);
    crate::println!("[percpu_cache] CacheStats hit rate: {:.2}%", hit_rate * 100.0);
    
    // 测试 CpuLocalCache
    let cpu_id = current_cpu_id();
    let cache = get_cache();
    if let Some(local_cache) = cache.get_cache(cpu_id) {
        let mut guard = local_cache.lock();
        let local_cpu_id = guard.cpu_id();
        let local_stats = guard.stats();
        crate::println!("[percpu_cache] CPU {} local cache stats: {} allocations", 
            local_cpu_id, local_stats.allocations.load(core::sync::atomic::Ordering::Relaxed));
        
        // 测试页分配
        if let Some(page) = guard.alloc_page() {
            guard.dealloc_page(page);
        }
        
        // 测试 refill 和 flush
        let empty_objects: [*mut u8; 0] = [];
        guard.refill(0, &empty_objects);
        let _flushed = guard.flush(0);
    }
    
    // 测试 set_cpu_id
    let original_cpu = current_cpu_id();
    set_cpu_id(0);
    assert_eq!(current_cpu_id(), 0);
    set_cpu_id(original_cpu);
    
    crate::println!("[percpu_cache] Self-test passed!");
}
