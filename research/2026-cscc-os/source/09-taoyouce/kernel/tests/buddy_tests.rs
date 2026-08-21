//! Buddy 分配器属性测试
//!
//! 验证 Buddy System 内存分配器的以下属性：
//! 1. 分配-释放不变量：分配后释放，内存状态恢复
//! 2. 对齐属性：分配的内存块正确对齐
//! 3. 合并属性：相邻空闲块正确合并
//! 4. 无重叠属性：分配的内存块不重叠
//! 5. 边界属性：不会分配超出范围的内存

use crate::{SimpleRng, println};
use alloc::vec::Vec;
use alloc::collections::BTreeSet;

/// 模拟的物理页号
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MockPPN(usize);

/// 测试用 Buddy 分配器
struct TestBuddyAllocator {
    free_lists: [Vec<MockPPN>; 11],
    start: usize,
    end: usize,
    allocated: usize,
    allocated_blocks: BTreeSet<(usize, usize)>, // (ppn, order)
}

impl TestBuddyAllocator {
    const MAX_ORDER: usize = 11;
    
    fn new() -> Self {
        Self {
            free_lists: Default::default(),
            start: 0,
            end: 0,
            allocated: 0,
            allocated_blocks: BTreeSet::new(),
        }
    }
    
    fn init(&mut self, start: usize, end: usize) {
        self.start = start;
        self.end = end;
        
        let mut current = start;
        while current < end {
            let mut order = 0;
            while order < Self::MAX_ORDER - 1 {
                let size = 1 << (order + 1);
                if current % size != 0 || current + size > end {
                    break;
                }
                order += 1;
            }
            self.free_lists[order].push(MockPPN(current));
            current += 1 << order;
        }
    }
    
    fn alloc(&mut self, order: usize) -> Option<MockPPN> {
        if order >= Self::MAX_ORDER {
            return None;
        }
        
        for o in order..Self::MAX_ORDER {
            if let Some(frame) = self.free_lists[o].pop() {
                for split_order in (order..o).rev() {
                    let buddy = MockPPN(frame.0 + (1 << split_order));
                    self.free_lists[split_order].push(buddy);
                }
                self.allocated += 1 << order;
                self.allocated_blocks.insert((frame.0, order));
                return Some(frame);
            }
        }
        None
    }
    
    fn dealloc(&mut self, frame: MockPPN, order: usize) {
        if order >= Self::MAX_ORDER {
            return;
        }
        
        self.allocated_blocks.remove(&(frame.0, order));
        
        let mut current = frame;
        let mut current_order = order;
        
        while current_order < Self::MAX_ORDER - 1 {
            let buddy_ppn = current.0 ^ (1 << current_order);
            let buddy = MockPPN(buddy_ppn);
            
            if let Some(pos) = self.free_lists[current_order]
                .iter()
                .position(|&p| p == buddy)
            {
                self.free_lists[current_order].swap_remove(pos);
                current = MockPPN(current.0.min(buddy_ppn));
                current_order += 1;
            } else {
                break;
            }
        }
        
        self.free_lists[current_order].push(current);
        self.allocated -= 1 << order;
    }
    
    fn total_free(&self) -> usize {
        let mut total = 0;
        for (order, list) in self.free_lists.iter().enumerate() {
            total += list.len() * (1 << order);
        }
        total
    }
}

/// 属性1：分配-释放不变量
/// 分配后释放，总空闲内存应该恢复
#[test_case]
fn test_alloc_dealloc_invariant() -> bool {
    let mut allocator = TestBuddyAllocator::new();
    allocator.init(0, 1024);
    
    let initial_free = allocator.total_free();
    let mut rng = SimpleRng::new(12345);
    let mut allocations = Vec::new();
    
    // 随机分配
    for _ in 0..100 {
        let order = rng.next_range(0, 5);
        if let Some(ppn) = allocator.alloc(order) {
            allocations.push((ppn, order));
        }
    }
    
    // 全部释放
    for (ppn, order) in allocations {
        allocator.dealloc(ppn, order);
    }
    
    let final_free = allocator.total_free();
    
    if initial_free != final_free {
        println!("  错误: 初始空闲={}, 最终空闲={}", initial_free, final_free);
        return false;
    }
    true
}

/// 属性2：对齐属性
/// 分配的 2^order 块应该对齐到 2^order 边界
#[test_case]
fn test_alignment_property() -> bool {
    let mut allocator = TestBuddyAllocator::new();
    allocator.init(0, 4096);
    
    let mut rng = SimpleRng::new(54321);
    
    for _ in 0..50 {
        let order = rng.next_range(0, 8);
        if let Some(ppn) = allocator.alloc(order) {
            let alignment = 1 << order;
            if ppn.0 % alignment != 0 {
                println!("  错误: PPN {} 未对齐到 2^{}", ppn.0, order);
                return false;
            }
            allocator.dealloc(ppn, order);
        }
    }
    true
}

/// 属性3：无重叠属性
/// 同时分配的内存块不应重叠
#[test_case]
fn test_no_overlap_property() -> bool {
    let mut allocator = TestBuddyAllocator::new();
    allocator.init(0, 2048);
    
    let mut rng = SimpleRng::new(11111);
    let mut allocations: Vec<(usize, usize, usize)> = Vec::new(); // (start, end, order)
    
    // 分配多个块
    for _ in 0..30 {
        let order = rng.next_range(0, 6);
        if let Some(ppn) = allocator.alloc(order) {
            let size = 1 << order;
            allocations.push((ppn.0, ppn.0 + size, order));
        }
    }
    
    // 检查重叠
    for i in 0..allocations.len() {
        for j in (i + 1)..allocations.len() {
            let (s1, e1, _) = allocations[i];
            let (s2, e2, _) = allocations[j];
            
            if s1 < e2 && s2 < e1 {
                println!("  错误: 块 [{}, {}) 与 [{}, {}) 重叠", s1, e1, s2, e2);
                return false;
            }
        }
    }
    
    // 清理
    for (ppn, _, order) in allocations {
        allocator.dealloc(MockPPN(ppn), order);
    }
    true
}

/// 属性4：边界属性
/// 分配的内存不应超出初始化范围
#[test_case]
fn test_boundary_property() -> bool {
    let start = 100;
    let end = 500;
    let mut allocator = TestBuddyAllocator::new();
    allocator.init(start, end);
    
    let mut rng = SimpleRng::new(99999);
    
    for _ in 0..50 {
        let order = rng.next_range(0, 5);
        if let Some(ppn) = allocator.alloc(order) {
            let size = 1 << order;
            if ppn.0 < start || ppn.0 + size > end {
                println!("  错误: 分配 [{}, {}) 超出范围 [{}, {})", 
                         ppn.0, ppn.0 + size, start, end);
                return false;
            }
            allocator.dealloc(ppn, order);
        }
    }
    true
}

/// 属性5：合并属性
/// 释放相邻块后应该合并成更大的块
#[test_case]
fn test_merge_property() -> bool {
    let mut allocator = TestBuddyAllocator::new();
    allocator.init(0, 16); // 16 页，可以合并成一个 order=4 的块
    
    // 分配 16 个 order=0 的块
    let mut blocks = Vec::new();
    for _ in 0..16 {
        if let Some(ppn) = allocator.alloc(0) {
            blocks.push(ppn);
        }
    }
    
    if blocks.len() != 16 {
        println!("  错误: 只分配了 {} 个块", blocks.len());
        return false;
    }
    
    // 全部释放
    for ppn in blocks {
        allocator.dealloc(ppn, 0);
    }
    
    // 检查是否合并成一个大块
    // 应该只有 order=4 的空闲列表有一个块
    let mut found_merged = false;
    for (order, list) in allocator.free_lists.iter().enumerate() {
        if !list.is_empty() {
            if order == 4 && list.len() == 1 {
                found_merged = true;
            } else {
                println!("  警告: order={} 有 {} 个块", order, list.len());
            }
        }
    }
    
    found_merged
}

/// 属性6：分配失败属性
/// 当内存不足时应该返回 None
#[test_case]
fn test_allocation_failure() -> bool {
    let mut allocator = TestBuddyAllocator::new();
    allocator.init(0, 8); // 只有 8 页
    
    // 尝试分配超过可用内存的块
    let result = allocator.alloc(4); // 需要 16 页
    if result.is_some() {
        println!("  错误: 不应该能分配 16 页（只有 8 页可用）");
        return false;
    }
    
    // 分配所有内存后再分配应该失败
    let block = allocator.alloc(3); // 8 页
    if block.is_none() {
        println!("  错误: 应该能分配 8 页");
        return false;
    }
    
    let extra = allocator.alloc(0);
    if extra.is_some() {
        println!("  错误: 内存已满，不应该能再分配");
        return false;
    }
    
    true
}

/// 属性7：随机操作序列测试
/// 随机执行分配和释放操作，验证不变量
#[test_case]
fn test_random_operations() -> bool {
    let mut allocator = TestBuddyAllocator::new();
    allocator.init(0, 1024);
    
    let initial_total = allocator.end - allocator.start;
    let mut rng = SimpleRng::new(77777);
    let mut allocations = Vec::new();
    
    for _ in 0..200 {
        let op = rng.next_range(0, 3);
        
        match op {
            0 | 1 => {
                // 分配
                let order = rng.next_range(0, 6);
                if let Some(ppn) = allocator.alloc(order) {
                    allocations.push((ppn, order));
                }
            }
            _ => {
                // 释放
                if !allocations.is_empty() {
                    let idx = rng.next_range(0, allocations.len());
                    let (ppn, order) = allocations.swap_remove(idx);
                    allocator.dealloc(ppn, order);
                }
            }
        }
        
        // 验证不变量：已分配 + 空闲 = 总量
        let free = allocator.total_free();
        let allocated = allocator.allocated;
        if free + allocated != initial_total {
            println!("  错误: 空闲({}) + 已分配({}) != 总量({})", 
                     free, allocated, initial_total);
            return false;
        }
    }
    
    true
}
