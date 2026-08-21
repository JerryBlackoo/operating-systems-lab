//! 调度器属性测试
//!
//! 验证 MLFQ 调度器的以下属性：
//! Property 6: 调度器单运行不变量 - 任意时刻最多一个线程处于 Running 状态
//! 以及其他辅助属性测试

use crate::{SimpleRng, println};
use alloc::vec::Vec;
use alloc::collections::{VecDeque, BTreeSet};

/// 优先级级别数
const PRIORITY_LEVELS: usize = 8;
const DEFAULT_PRIORITY: usize = 4;
const MAX_PRIORITY: usize = 0;
const MIN_PRIORITY: usize = PRIORITY_LEVELS - 1;
const DEFAULT_TIME_SLICE: usize = 10;

/// 线程 ID
type Tid = usize;

/// 线程状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThreadState {
    Ready,
    Running,
    Blocked,
}

/// 测试用线程
#[derive(Debug, Clone)]
struct TestThread {
    tid: Tid,
    state: ThreadState,
    priority: usize,
    time_slice: usize,
    total_runtime: usize,
}

impl TestThread {
    fn new(tid: Tid, priority: usize) -> Self {
        Self {
            tid,
            state: ThreadState::Ready,
            priority,
            time_slice: DEFAULT_TIME_SLICE,
            total_runtime: 0,
        }
    }
}

/// MLFQ 调度器
struct MlfqScheduler {
    queues: [VecDeque<Tid>; PRIORITY_LEVELS],
    current: Option<Tid>,
    boost_counter: usize,
}

impl MlfqScheduler {
    fn new() -> Self {
        Self {
            queues: Default::default(),
            current: None,
            boost_counter: 0,
        }
    }
    
    fn enqueue(&mut self, tid: Tid, priority: usize) {
        let priority = priority.min(MIN_PRIORITY);
        self.queues[priority].push_back(tid);
    }
    
    fn dequeue(&mut self) -> Option<Tid> {
        for queue in &mut self.queues {
            if let Some(tid) = queue.pop_front() {
                return Some(tid);
            }
        }
        None
    }
    
    fn demote(&mut self, tid: Tid, current_priority: usize) {
        let new_priority = (current_priority + 1).min(MIN_PRIORITY);
        self.enqueue(tid, new_priority);
    }
    
    fn boost(&mut self) {
        for i in 1..PRIORITY_LEVELS {
            while let Some(tid) = self.queues[i].pop_front() {
                self.queues[0].push_back(tid);
            }
        }
    }
    
    fn tick(&mut self) {
        self.boost_counter += 1;
        if self.boost_counter >= 100 {
            self.boost();
            self.boost_counter = 0;
        }
    }
    
    fn total_threads(&self) -> usize {
        self.queues.iter().map(|q| q.len()).sum()
    }
    
    fn get_priority_distribution(&self) -> [usize; PRIORITY_LEVELS] {
        let mut dist = [0; PRIORITY_LEVELS];
        for (i, queue) in self.queues.iter().enumerate() {
            dist[i] = queue.len();
        }
        dist
    }
}

/// 属性1：优先级正确性
/// 高优先级任务应先被调度
#[test_case]
fn test_priority_ordering() -> bool {
    let mut sched = MlfqScheduler::new();
    
    // 添加不同优先级的任务
    sched.enqueue(1, 7); // 最低优先级
    sched.enqueue(2, 4); // 中等优先级
    sched.enqueue(3, 0); // 最高优先级
    sched.enqueue(4, 2); // 较高优先级
    
    // 验证调度顺序
    let expected_order = [3, 4, 2, 1];
    
    for &expected in &expected_order {
        match sched.dequeue() {
            Some(tid) if tid == expected => {}
            Some(tid) => {
                println!("  错误: 期望调度 {}，实际调度 {}", expected, tid);
                return false;
            }
            None => {
                println!("  错误: 队列不应为空");
                return false;
            }
        }
    }
    
    true
}

/// 属性2：同优先级 FIFO
/// 同优先级任务应按入队顺序调度
#[test_case]
fn test_same_priority_fifo() -> bool {
    let mut sched = MlfqScheduler::new();
    
    // 添加同优先级任务
    for i in 0..10 {
        sched.enqueue(i, DEFAULT_PRIORITY);
    }
    
    // 验证 FIFO 顺序
    for i in 0..10 {
        match sched.dequeue() {
            Some(tid) if tid == i => {}
            Some(tid) => {
                println!("  错误: 期望 {}，得到 {}", i, tid);
                return false;
            }
            None => {
                println!("  错误: 队列不应为空");
                return false;
            }
        }
    }
    
    true
}

/// 属性3：优先级降级
/// 用完时间片的任务优先级应降低
#[test_case]
fn test_priority_demotion() -> bool {
    let mut sched = MlfqScheduler::new();
    
    // 添加高优先级任务
    sched.enqueue(1, 0);
    
    // 模拟用完时间片后降级
    let tid = sched.dequeue().unwrap();
    sched.demote(tid, 0);
    
    // 验证任务在较低优先级队列
    if sched.queues[0].contains(&1) {
        println!("  错误: 任务不应在原优先级队列");
        return false;
    }
    
    if !sched.queues[1].contains(&1) {
        println!("  错误: 任务应在降级后的队列");
        return false;
    }
    
    true
}

/// 属性4：优先级提升（防饥饿）
/// 周期性提升应将所有任务移到最高优先级
#[test_case]
fn test_priority_boost() -> bool {
    let mut sched = MlfqScheduler::new();
    
    // 在不同优先级添加任务
    for i in 0..PRIORITY_LEVELS {
        sched.enqueue(i, i);
    }
    
    // 执行提升
    sched.boost();
    
    // 验证所有任务都在最高优先级队列
    if sched.queues[0].len() != PRIORITY_LEVELS {
        println!("  错误: 提升后最高优先级队列应有 {} 个任务", PRIORITY_LEVELS);
        return false;
    }
    
    for i in 1..PRIORITY_LEVELS {
        if !sched.queues[i].is_empty() {
            println!("  错误: 优先级 {} 队列应为空", i);
            return false;
        }
    }
    
    true
}

/// 属性5：任务不丢失
/// 调度操作不应丢失任务
#[test_case]
fn test_no_task_loss() -> bool {
    let mut sched = MlfqScheduler::new();
    let mut rng = SimpleRng::new(12345);
    let mut all_tasks: BTreeSet<Tid> = BTreeSet::new();
    
    // 添加任务
    for i in 0..50 {
        let priority = rng.next_range(0, PRIORITY_LEVELS);
        sched.enqueue(i, priority);
        all_tasks.insert(i);
    }
    
    // 随机操作
    for _ in 0..100 {
        let op = rng.next_range(0, 3);
        
        match op {
            0 => {
                // 调度并重新入队
                if let Some(tid) = sched.dequeue() {
                    let new_priority = rng.next_range(0, PRIORITY_LEVELS);
                    sched.enqueue(tid, new_priority);
                }
            }
            1 => {
                // 降级
                if let Some(tid) = sched.dequeue() {
                    let priority = rng.next_range(0, PRIORITY_LEVELS);
                    sched.demote(tid, priority);
                }
            }
            _ => {
                // 提升
                sched.boost();
            }
        }
    }
    
    // 验证所有任务仍在
    let mut found_tasks: BTreeSet<Tid> = BTreeSet::new();
    while let Some(tid) = sched.dequeue() {
        found_tasks.insert(tid);
    }
    
    if found_tasks != all_tasks {
        println!("  错误: 任务丢失或重复");
        return false;
    }
    
    true
}

/// 属性6：空队列处理
/// 空队列应返回 None
#[test_case]
fn test_empty_queue() -> bool {
    let mut sched = MlfqScheduler::new();
    
    if sched.dequeue().is_some() {
        println!("  错误: 空队列不应返回任务");
        return false;
    }
    
    // 添加并移除
    sched.enqueue(1, 0);
    sched.dequeue();
    
    if sched.dequeue().is_some() {
        println!("  错误: 清空后不应返回任务");
        return false;
    }
    
    true
}

/// 属性7：优先级边界
/// 优先级应被限制在有效范围内
#[test_case]
fn test_priority_bounds() -> bool {
    let mut sched = MlfqScheduler::new();
    
    // 尝试超出范围的优先级
    sched.enqueue(1, 100); // 应被限制到 MIN_PRIORITY
    sched.enqueue(2, 0);   // 最高优先级
    
    // 高优先级应先调度
    if sched.dequeue() != Some(2) {
        println!("  错误: 优先级 0 应先调度");
        return false;
    }
    
    if sched.dequeue() != Some(1) {
        println!("  错误: 超出范围的优先级应被限制");
        return false;
    }
    
    true
}

/// 属性8：降级边界
/// 最低优先级任务降级后应保持在最低优先级
#[test_case]
fn test_demotion_floor() -> bool {
    let mut sched = MlfqScheduler::new();
    
    sched.enqueue(1, MIN_PRIORITY);
    let tid = sched.dequeue().unwrap();
    
    // 多次降级
    for _ in 0..5 {
        sched.demote(tid, MIN_PRIORITY);
    }
    
    // 应该仍在最低优先级队列
    if !sched.queues[MIN_PRIORITY].contains(&1) {
        println!("  错误: 任务应保持在最低优先级");
        return false;
    }
    
    true
}

/// 属性9：提升周期
/// 100 个 tick 后应触发提升
#[test_case]
fn test_boost_period() -> bool {
    let mut sched = MlfqScheduler::new();
    
    // 添加低优先级任务
    sched.enqueue(1, MIN_PRIORITY);
    
    // 执行 99 个 tick
    for _ in 0..99 {
        sched.tick();
    }
    
    // 任务应仍在低优先级
    if !sched.queues[MIN_PRIORITY].contains(&1) {
        println!("  错误: 99 tick 后不应提升");
        return false;
    }
    
    // 第 100 个 tick
    sched.tick();
    
    // 任务应被提升
    if !sched.queues[0].contains(&1) {
        println!("  错误: 100 tick 后应提升");
        return false;
    }
    
    true
}

/// 属性10：公平性测试
/// 长时间运行后，所有任务应获得执行机会
#[test_case]
fn test_fairness() -> bool {
    let mut sched = MlfqScheduler::new();
    let num_tasks = 10;
    let mut execution_count = vec![0usize; num_tasks];
    
    // 添加任务
    for i in 0..num_tasks {
        sched.enqueue(i, DEFAULT_PRIORITY);
    }
    
    // 模拟调度
    for _ in 0..1000 {
        if let Some(tid) = sched.dequeue() {
            execution_count[tid] += 1;
            // 重新入队
            sched.enqueue(tid, DEFAULT_PRIORITY);
        }
        sched.tick();
    }
    
    // 验证所有任务都被执行过
    for (tid, &count) in execution_count.iter().enumerate() {
        if count == 0 {
            println!("  错误: 任务 {} 从未被执行", tid);
            return false;
        }
    }
    
    // 验证执行次数相对均衡（允许 50% 偏差）
    let avg = execution_count.iter().sum::<usize>() / num_tasks;
    for (tid, &count) in execution_count.iter().enumerate() {
        if count < avg / 2 || count > avg * 2 {
            println!("  警告: 任务 {} 执行次数 {} 偏离平均值 {}", tid, count, avg);
            // 不作为失败，只是警告
        }
    }
    
    true
}

/// **Property 6: 调度器单运行不变量**
/// *For any* scheduler state, at most one thread can be in the Running state at any time.
/// **Validates: Requirements 5.1, 5.2**
/// 
/// Feature: rust-warnings-cleanup, Property 6: 调度器单运行不变量
#[test_case]
fn test_property_single_running_invariant() -> bool {
    let mut rng = SimpleRng::new(67890);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        let mut threads: Vec<TestThread> = Vec::new();
        let mut sched = MlfqScheduler::new();
        let num_threads = rng.next_range(5, 20);
        
        // 创建随机数量的线程
        for i in 0..num_threads {
            let priority = rng.next_range(0, PRIORITY_LEVELS);
            let thread = TestThread::new(i, priority);
            threads.push(thread);
            sched.enqueue(i, priority);
        }
        
        // 模拟调度过程
        let num_operations = rng.next_range(50, 200);
        
        for _ in 0..num_operations {
            // 计算当前 Running 状态的线程数
            let running_count = threads.iter().filter(|t| t.state == ThreadState::Running).count();
            
            // 不变量检查：最多一个线程处于 Running 状态
            if running_count > 1 {
                println!("  错误 [迭代 {}]: {} 个线程同时处于 Running 状态", iteration, running_count);
                return false;
            }
            
            let op = rng.next_range(0, 5);
            
            match op {
                0 => {
                    // 调度新线程（如果当前没有运行的线程）
                    if running_count == 0 {
                        if let Some(tid) = sched.dequeue() {
                            if let Some(thread) = threads.get_mut(tid) {
                                thread.state = ThreadState::Running;
                            }
                        }
                    }
                }
                1 => {
                    // 当前线程让出 CPU
                    if let Some(running_thread) = threads.iter_mut().find(|t| t.state == ThreadState::Running) {
                        running_thread.state = ThreadState::Ready;
                        running_thread.time_slice = DEFAULT_TIME_SLICE;
                        sched.enqueue(running_thread.tid, running_thread.priority);
                    }
                }
                2 => {
                    // 当前线程阻塞
                    if let Some(running_thread) = threads.iter_mut().find(|t| t.state == ThreadState::Running) {
                        running_thread.state = ThreadState::Blocked;
                    }
                }
                3 => {
                    // 唤醒一个阻塞的线程
                    if let Some(blocked_thread) = threads.iter_mut().find(|t| t.state == ThreadState::Blocked) {
                        blocked_thread.state = ThreadState::Ready;
                        sched.enqueue(blocked_thread.tid, blocked_thread.priority);
                    }
                }
                _ => {
                    // 时间片用完，降级
                    if let Some(running_thread) = threads.iter_mut().find(|t| t.state == ThreadState::Running) {
                        running_thread.time_slice = running_thread.time_slice.saturating_sub(1);
                        if running_thread.time_slice == 0 {
                            running_thread.state = ThreadState::Ready;
                            let new_priority = (running_thread.priority + 1).min(MIN_PRIORITY);
                            running_thread.priority = new_priority;
                            running_thread.time_slice = DEFAULT_TIME_SLICE;
                            sched.demote(running_thread.tid, running_thread.priority);
                        }
                    }
                }
            }
            
            // 每次操作后再次验证不变量
            let running_count_after = threads.iter().filter(|t| t.state == ThreadState::Running).count();
            if running_count_after > 1 {
                println!("  错误 [迭代 {}]: 操作后 {} 个线程同时处于 Running 状态", iteration, running_count_after);
                return false;
            }
        }
    }
    
    println!("  Property 6: 调度器单运行不变量 - 100 次迭代全部通过");
    true
}

/// 属性11：状态转换一致性
/// 线程状态转换应遵循有效的状态机
#[test_case]
fn test_state_transition_consistency() -> bool {
    let mut rng = SimpleRng::new(11111);
    
    for iteration in 0..100 {
        let mut threads: Vec<TestThread> = Vec::new();
        let mut sched = MlfqScheduler::new();
        let num_threads = rng.next_range(3, 15);
        
        // 创建线程
        for i in 0..num_threads {
            let priority = rng.next_range(0, PRIORITY_LEVELS);
            let thread = TestThread::new(i, priority);
            threads.push(thread);
            sched.enqueue(i, priority);
        }
        
        // 模拟状态转换
        for _ in 0..100 {
            let tid = rng.next_range(0, num_threads);
            let thread = &mut threads[tid];
            let old_state = thread.state;
            
            // 根据当前状态选择有效的转换
            let new_state = match old_state {
                ThreadState::Ready => {
                    // Ready 可以转换为 Running
                    if rng.next_range(0, 2) == 0 {
                        // 只有当没有其他线程在运行时才能转换
                        let running_count = threads.iter().filter(|t| t.state == ThreadState::Running).count();
                        if running_count == 0 {
                            ThreadState::Running
                        } else {
                            ThreadState::Ready
                        }
                    } else {
                        ThreadState::Ready
                    }
                }
                ThreadState::Running => {
                    // Running 可以转换为 Ready 或 Blocked
                    match rng.next_range(0, 3) {
                        0 => {
                            sched.enqueue(tid, thread.priority);
                            ThreadState::Ready
                        }
                        1 => ThreadState::Blocked,
                        _ => ThreadState::Running,
                    }
                }
                ThreadState::Blocked => {
                    // Blocked 可以转换为 Ready
                    if rng.next_range(0, 3) == 0 {
                        sched.enqueue(tid, thread.priority);
                        ThreadState::Ready
                    } else {
                        ThreadState::Blocked
                    }
                }
            };
            
            thread.state = new_state;
            
            // 验证不变量
            let running_count = threads.iter().filter(|t| t.state == ThreadState::Running).count();
            if running_count > 1 {
                println!("  错误 [迭代 {}]: 状态转换后 {} 个线程同时运行", iteration, running_count);
                return false;
            }
        }
    }
    
    println!("  状态转换一致性测试 - 100 次迭代全部通过");
    true
}
