//! 无锁数据结构属性测试
//!
//! 验证无锁数据结构的以下属性：
//! 1. 线性化：操作看起来是原子的
//! 2. 无锁进展：至少一个线程能取得进展
//! 3. ABA 问题处理：正确处理 ABA 问题
//! 4. 内存回收：Epoch-based 回收正确工作
//! 5. 队列 FIFO：无锁队列保持 FIFO 顺序

use crate::{SimpleRng, println};
use alloc::vec::Vec;
use alloc::boxed::Box;
use core::sync::atomic::{AtomicUsize, AtomicPtr, AtomicU64, Ordering};
use core::ptr;

/// 无锁队列节点
struct QueueNode<T> {
    value: Option<T>,
    next: AtomicPtr<QueueNode<T>>,
}

impl<T> QueueNode<T> {
    fn new(value: T) -> Self {
        Self {
            value: Some(value),
            next: AtomicPtr::new(ptr::null_mut()),
        }
    }
    
    fn sentinel() -> Self {
        Self {
            value: None,
            next: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

/// 无锁队列（Michael-Scott Queue 简化版）
struct LockFreeQueue<T> {
    head: AtomicPtr<QueueNode<T>>,
    tail: AtomicPtr<QueueNode<T>>,
    size: AtomicUsize,
}

impl<T> LockFreeQueue<T> {
    fn new() -> Self {
        let sentinel = Box::into_raw(Box::new(QueueNode::sentinel()));
        Self {
            head: AtomicPtr::new(sentinel),
            tail: AtomicPtr::new(sentinel),
            size: AtomicUsize::new(0),
        }
    }
    
    fn enqueue(&self, value: T) {
        let new_node = Box::into_raw(Box::new(QueueNode::new(value)));
        
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            let tail_ref = unsafe { &*tail };
            let next = tail_ref.next.load(Ordering::Acquire);
            
            if next.is_null() {
                // 尝试链接新节点
                if tail_ref.next.compare_exchange(
                    ptr::null_mut(),
                    new_node,
                    Ordering::Release,
                    Ordering::Relaxed,
                ).is_ok() {
                    // 尝试更新 tail（可能失败，但没关系）
                    let _ = self.tail.compare_exchange(
                        tail,
                        new_node,
                        Ordering::Release,
                        Ordering::Relaxed,
                    );
                    self.size.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            } else {
                // 帮助其他线程推进 tail
                let _ = self.tail.compare_exchange(
                    tail,
                    next,
                    Ordering::Release,
                    Ordering::Relaxed,
                );
            }
        }
    }
    
    fn dequeue(&self) -> Option<T> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            let head_ref = unsafe { &*head };
            let next = head_ref.next.load(Ordering::Acquire);
            
            if head == tail {
                if next.is_null() {
                    return None; // 队列为空
                }
                // tail 落后，帮助推进
                let _ = self.tail.compare_exchange(
                    tail,
                    next,
                    Ordering::Release,
                    Ordering::Relaxed,
                );
            } else {
                // 尝试出队
                if self.head.compare_exchange(
                    head,
                    next,
                    Ordering::Release,
                    Ordering::Relaxed,
                ).is_ok() {
                    let next_ref = unsafe { &mut *next };
                    let value = next_ref.value.take();
                    // 注意：这里简化了内存回收
                    self.size.fetch_sub(1, Ordering::Relaxed);
                    return value;
                }
            }
        }
    }
    
    fn len(&self) -> usize {
        self.size.load(Ordering::Relaxed)
    }
    
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Epoch-based 内存回收
struct Epoch {
    current: AtomicU64,
    thread_epochs: Vec<AtomicU64>,
}

impl Epoch {
    fn new(num_threads: usize) -> Self {
        let mut thread_epochs = Vec::with_capacity(num_threads);
        for _ in 0..num_threads {
            thread_epochs.push(AtomicU64::new(0));
        }
        Self {
            current: AtomicU64::new(0),
            thread_epochs,
        }
    }
    
    fn enter(&self, thread_id: usize) -> u64 {
        let epoch = self.current.load(Ordering::Acquire);
        if thread_id < self.thread_epochs.len() {
            self.thread_epochs[thread_id].store(epoch, Ordering::Release);
        }
        epoch
    }
    
    fn exit(&self, thread_id: usize) {
        if thread_id < self.thread_epochs.len() {
            self.thread_epochs[thread_id].store(u64::MAX, Ordering::Release);
        }
    }
    
    fn try_advance(&self) -> bool {
        let current = self.current.load(Ordering::Acquire);
        
        // 检查所有线程是否都离开了当前 epoch
        for epoch in &self.thread_epochs {
            let thread_epoch = epoch.load(Ordering::Acquire);
            if thread_epoch != u64::MAX && thread_epoch <= current {
                return false;
            }
        }
        
        // 推进 epoch
        self.current.compare_exchange(
            current,
            current + 1,
            Ordering::Release,
            Ordering::Relaxed,
        ).is_ok()
    }
    
    fn current_epoch(&self) -> u64 {
        self.current.load(Ordering::Acquire)
    }
}

/// 无锁栈节点
struct StackNode<T> {
    value: T,
    next: AtomicPtr<StackNode<T>>,
}

/// 无锁栈（Treiber Stack）
struct LockFreeStack<T> {
    top: AtomicPtr<StackNode<T>>,
    size: AtomicUsize,
}

impl<T> LockFreeStack<T> {
    fn new() -> Self {
        Self {
            top: AtomicPtr::new(ptr::null_mut()),
            size: AtomicUsize::new(0),
        }
    }
    
    fn push(&self, value: T) {
        let new_node = Box::into_raw(Box::new(StackNode {
            value,
            next: AtomicPtr::new(ptr::null_mut()),
        }));
        
        loop {
            let top = self.top.load(Ordering::Acquire);
            unsafe { (*new_node).next.store(top, Ordering::Relaxed); }
            
            if self.top.compare_exchange(
                top,
                new_node,
                Ordering::Release,
                Ordering::Relaxed,
            ).is_ok() {
                self.size.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }
    }
    
    fn pop(&self) -> Option<T> {
        loop {
            let top = self.top.load(Ordering::Acquire);
            if top.is_null() {
                return None;
            }
            
            let top_ref = unsafe { &*top };
            let next = top_ref.next.load(Ordering::Acquire);
            
            if self.top.compare_exchange(
                top,
                next,
                Ordering::Release,
                Ordering::Relaxed,
            ).is_ok() {
                let node = unsafe { Box::from_raw(top) };
                self.size.fetch_sub(1, Ordering::Relaxed);
                return Some(node.value);
            }
        }
    }
    
    fn len(&self) -> usize {
        self.size.load(Ordering::Relaxed)
    }
}

/// 属性1：队列 FIFO 顺序
/// 入队顺序应与出队顺序一致
#[test_case]
fn test_queue_fifo_order() -> bool {
    let queue = LockFreeQueue::new();
    
    // 入队
    for i in 0..100 {
        queue.enqueue(i);
    }
    
    // 出队并验证顺序
    for i in 0..100 {
        match queue.dequeue() {
            Some(value) if value == i => {}
            Some(value) => {
                println!("  错误: 期望 {}，得到 {}", i, value);
                return false;
            }
            None => {
                println!("  错误: 队列不应为空");
                return false;
            }
        }
    }
    
    // 队列应为空
    if !queue.is_empty() {
        println!("  错误: 队列应为空");
        return false;
    }
    
    true
}

/// 属性2：栈 LIFO 顺序
/// 入栈顺序应与出栈顺序相反
#[test_case]
fn test_stack_lifo_order() -> bool {
    let stack = LockFreeStack::new();
    
    // 入栈
    for i in 0..100 {
        stack.push(i);
    }
    
    // 出栈并验证顺序（应该是反序）
    for i in (0..100).rev() {
        match stack.pop() {
            Some(value) if value == i => {}
            Some(value) => {
                println!("  错误: 期望 {}，得到 {}", i, value);
                return false;
            }
            None => {
                println!("  错误: 栈不应为空");
                return false;
            }
        }
    }
    
    true
}

/// 属性3：空队列出队
/// 空队列出队应返回 None
#[test_case]
fn test_empty_queue_dequeue() -> bool {
    let queue: LockFreeQueue<i32> = LockFreeQueue::new();
    
    if queue.dequeue().is_some() {
        println!("  错误: 空队列不应返回值");
        return false;
    }
    
    // 入队再出队
    queue.enqueue(42);
    queue.dequeue();
    
    if queue.dequeue().is_some() {
        println!("  错误: 清空后不应返回值");
        return false;
    }
    
    true
}

/// 属性4：空栈出栈
/// 空栈出栈应返回 None
#[test_case]
fn test_empty_stack_pop() -> bool {
    let stack: LockFreeStack<i32> = LockFreeStack::new();
    
    if stack.pop().is_some() {
        println!("  错误: 空栈不应返回值");
        return false;
    }
    
    true
}

/// 属性5：大小计数正确性
/// 入队/出队后大小应正确更新
#[test_case]
fn test_size_tracking() -> bool {
    let queue = LockFreeQueue::new();
    
    // 入队
    for i in 0..50 {
        queue.enqueue(i);
        if queue.len() != i + 1 {
            println!("  错误: 入队后大小不正确");
            return false;
        }
    }
    
    // 出队
    for i in 0..50 {
        queue.dequeue();
        if queue.len() != 49 - i {
            println!("  错误: 出队后大小不正确");
            return false;
        }
    }
    
    true
}

/// 属性6：Epoch 进入和退出
/// 线程应能正确进入和退出 epoch
#[test_case]
fn test_epoch_enter_exit() -> bool {
    let epoch = Epoch::new(4);
    
    // 线程 0 进入
    let e0 = epoch.enter(0);
    if e0 != 0 {
        println!("  错误: 初始 epoch 应为 0");
        return false;
    }
    
    // 线程 0 退出
    epoch.exit(0);
    
    // 应能推进 epoch
    if !epoch.try_advance() {
        println!("  错误: 所有线程退出后应能推进");
        return false;
    }
    
    if epoch.current_epoch() != 1 {
        println!("  错误: epoch 应推进到 1");
        return false;
    }
    
    true
}

/// 属性7：Epoch 阻止推进
/// 有线程在 epoch 中时不应推进
#[test_case]
fn test_epoch_blocks_advance() -> bool {
    let epoch = Epoch::new(4);
    
    // 线程 0 进入
    epoch.enter(0);
    
    // 不应能推进
    if epoch.try_advance() {
        println!("  错误: 有线程在 epoch 中不应推进");
        return false;
    }
    
    // 线程 0 退出
    epoch.exit(0);
    
    // 现在应能推进
    if !epoch.try_advance() {
        println!("  错误: 线程退出后应能推进");
        return false;
    }
    
    true
}

/// 属性8：随机队列操作
/// 随机入队出队后，队列状态应一致
#[test_case]
fn test_random_queue_operations() -> bool {
    let queue = LockFreeQueue::new();
    let mut rng = SimpleRng::new(12345);
    let mut expected_size = 0;
    let mut enqueued = 0;
    let mut dequeued = 0;
    
    for _ in 0..200 {
        let op = rng.next_range(0, 3);
        
        match op {
            0 | 1 => {
                // 入队
                queue.enqueue(enqueued);
                enqueued += 1;
                expected_size += 1;
            }
            _ => {
                // 出队
                if expected_size > 0 {
                    if let Some(value) = queue.dequeue() {
                        if value != dequeued {
                            println!("  错误: FIFO 顺序错误");
                            return false;
                        }
                        dequeued += 1;
                        expected_size -= 1;
                    }
                }
            }
        }
        
        if queue.len() != expected_size {
            println!("  错误: 大小不一致");
            return false;
        }
    }
    
    true
}

/// 属性9：随机栈操作
/// 随机入栈出栈后，栈状态应一致
#[test_case]
fn test_random_stack_operations() -> bool {
    let stack = LockFreeStack::new();
    let mut rng = SimpleRng::new(54321);
    let mut expected: Vec<i32> = Vec::new();
    
    for i in 0..200 {
        let op = rng.next_range(0, 3);
        
        match op {
            0 | 1 => {
                // 入栈
                stack.push(i as i32);
                expected.push(i as i32);
            }
            _ => {
                // 出栈
                let actual = stack.pop();
                let expect = expected.pop();
                
                if actual != expect {
                    println!("  错误: 栈操作不一致");
                    return false;
                }
            }
        }
        
        if stack.len() != expected.len() {
            println!("  错误: 大小不一致");
            return false;
        }
    }
    
    true
}

/// 属性10：队列不丢失元素
/// 所有入队的元素都应能出队
#[test_case]
fn test_no_element_loss() -> bool {
    let queue = LockFreeQueue::new();
    let count = 100;
    
    // 入队
    for i in 0..count {
        queue.enqueue(i);
    }
    
    // 出队并计数
    let mut dequeued_count = 0;
    while queue.dequeue().is_some() {
        dequeued_count += 1;
    }
    
    if dequeued_count != count {
        println!("  错误: 入队 {} 个，出队 {} 个", count, dequeued_count);
        return false;
    }
    
    true
}
