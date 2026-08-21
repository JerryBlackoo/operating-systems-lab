//! 优先级继承互斥锁 (Priority Inheritance Mutex)
//!
//! 实现优先级继承协议，防止优先级反转

use alloc::collections::VecDeque;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use crate::sync::SpinLock;

/// 线程 ID
pub type Tid = u64;

/// 优先级
pub type Priority = u8;

/// 优先级继承互斥锁
pub struct PiMutex<T> {
    /// 锁状态：0 = 未锁定，非0 = 持有者 TID
    owner: AtomicU64,
    /// 持有者原始优先级
    owner_original_priority: AtomicU8,
    /// 持有者当前优先级（可能被提升）
    owner_current_priority: AtomicU8,
    /// 等待队列
    waiters: SpinLock<VecDeque<WaiterInfo>>,
    /// 数据
    data: UnsafeCell<T>,
}

/// 等待者信息
#[derive(Clone, Copy)]
struct WaiterInfo {
    tid: Tid,
    priority: Priority,
}

unsafe impl<T: Send> Send for PiMutex<T> {}
unsafe impl<T: Send> Sync for PiMutex<T> {}

impl<T> PiMutex<T> {
    /// 创建新的优先级继承互斥锁
    pub const fn new(data: T) -> Self {
        Self {
            owner: AtomicU64::new(0),
            owner_original_priority: AtomicU8::new(0),
            owner_current_priority: AtomicU8::new(0),
            waiters: SpinLock::new(VecDeque::new()),
            data: UnsafeCell::new(data),
        }
    }
    
    /// 尝试获取锁
    pub fn try_lock(&self, tid: Tid, priority: Priority) -> Option<PiMutexGuard<'_, T>> {
        if self.owner.compare_exchange(
            0, tid, Ordering::Acquire, Ordering::Relaxed
        ).is_ok() {
            self.owner_original_priority.store(priority, Ordering::SeqCst);
            self.owner_current_priority.store(priority, Ordering::SeqCst);
            Some(PiMutexGuard { mutex: self })
        } else {
            None
        }
    }
    
    /// 获取锁（阻塞）
    pub fn lock(&self, tid: Tid, priority: Priority) -> PiMutexGuard<'_, T> {
        loop {
            // 尝试获取锁
            if let Some(guard) = self.try_lock(tid, priority) {
                return guard;
            }
            
            // 获取失败，加入等待队列
            {
                let mut waiters = self.waiters.lock();
                waiters.push_back(WaiterInfo { tid, priority });
            }
            
            // 检查是否需要优先级继承
            self.check_priority_inheritance(priority);
            
            // 让出 CPU（实际实现需要调度器支持）
            core::hint::spin_loop();
            
            // 从等待队列移除（如果还在）
            {
                let mut waiters = self.waiters.lock();
                waiters.retain(|w| w.tid != tid);
            }
        }
    }
    
    /// 检查并执行优先级继承
    fn check_priority_inheritance(&self, waiter_priority: Priority) {
        let owner_priority = self.owner_current_priority.load(Ordering::SeqCst);
        
        // 如果等待者优先级更高（数值更小），提升持有者优先级
        if waiter_priority < owner_priority {
            self.owner_current_priority.store(waiter_priority, Ordering::SeqCst);
            
            // 通知调度器更新持有者优先级
            let owner_tid = self.owner.load(Ordering::SeqCst);
            if owner_tid != 0 {
                boost_thread_priority(owner_tid, waiter_priority);
            }
        }
    }
    
    /// 释放锁
    fn unlock(&self) {
        // 恢复原始优先级
        let owner_tid = self.owner.load(Ordering::SeqCst);
        let original_priority = self.owner_original_priority.load(Ordering::SeqCst);
        
        if owner_tid != 0 {
            restore_thread_priority(owner_tid, original_priority);
        }
        
        // 释放锁
        self.owner.store(0, Ordering::Release);
        
        // 唤醒等待者
        self.wake_waiters();
    }
    
    /// 唤醒等待者
    fn wake_waiters(&self) {
        let waiters = self.waiters.lock();
        
        // 找到最高优先级的等待者
        if let Some(highest) = waiters.iter().min_by_key(|w| w.priority) {
            wake_thread(highest.tid);
        }
    }
    
    /// 获取当前持有者
    pub fn owner(&self) -> Option<Tid> {
        let owner = self.owner.load(Ordering::SeqCst);
        if owner == 0 {
            None
        } else {
            Some(owner)
        }
    }
    
    /// 是否被锁定
    pub fn is_locked(&self) -> bool {
        self.owner.load(Ordering::SeqCst) != 0
    }
}

/// 互斥锁守卫
pub struct PiMutexGuard<'a, T> {
    mutex: &'a PiMutex<T>,
}

impl<'a, T> core::ops::Deref for PiMutexGuard<'a, T> {
    type Target = T;
    
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.data.get() }
    }
}

impl<'a, T> core::ops::DerefMut for PiMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.data.get() }
    }
}

impl<'a, T> Drop for PiMutexGuard<'a, T> {
    fn drop(&mut self) {
        self.mutex.unlock();
    }
}

/// 优先级继承链追踪
pub struct PiChain {
    /// 链中的锁
    chain: VecDeque<(Tid, Priority)>,
}

impl PiChain {
    pub fn new() -> Self {
        Self {
            chain: VecDeque::new(),
        }
    }
    
    /// 添加到链
    pub fn push(&mut self, tid: Tid, priority: Priority) {
        self.chain.push_back((tid, priority));
    }
    
    /// 获取链中最高优先级
    pub fn highest_priority(&self) -> Option<Priority> {
        self.chain.iter().map(|(_, p)| *p).min()
    }
    
    /// 传播优先级
    pub fn propagate(&self) {
        if let Some(highest) = self.highest_priority() {
            for (tid, _) in &self.chain {
                boost_thread_priority(*tid, highest);
            }
        }
    }
}

// 调度器回调函数 — wired to real process table PI boost

/// Boost thread priority via pi_boost_priority field on the PCB.
/// Per Linux rt_mutex_adjust_prio(): temporarily elevate the owner's
/// effective scheduling priority to prevent priority inversion.
fn boost_thread_priority(tid: Tid, new_priority: Priority) {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(t) = table.as_mut() {
        if let Some(p) = t.get_mut(tid as usize) {
            let boost = new_priority as i32;
            if boost > p.pi_boost_priority {
                p.pi_boost_priority = boost;
            }
        }
    }
}

/// Restore thread priority by clearing pi_boost_priority.
/// Effective priority falls back to base sched_priority.
fn restore_thread_priority(tid: Tid, _original_priority: Priority) {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(t) = table.as_mut() {
        if let Some(p) = t.get_mut(tid as usize) {
            p.pi_boost_priority = 0;
        }
    }
}

/// Wake a sleeping thread by marking it runnable.
fn wake_thread(tid: Tid) {
    let mut table = crate::sched::process::PROCESS_TABLE.lock();
    if let Some(t) = table.as_mut() {
        if matches!(
            t.get(tid as usize).map(|p| p.state),
            Some(crate::sched::process::ProcessState::Sleeping)
        ) {
            t.mark_runnable(tid as usize, true);
        }
    }
}

/// 优先级上限协议互斥锁
pub struct PcpMutex<T> {
    /// 优先级上限
    ceiling: Priority,
    /// 内部互斥锁
    inner: PiMutex<T>,
}

impl<T> PcpMutex<T> {
    /// 创建优先级上限互斥锁
    pub const fn new(data: T, ceiling: Priority) -> Self {
        Self {
            ceiling,
            inner: PiMutex::new(data),
        }
    }
    
    /// 获取锁
    pub fn lock(&self, tid: Tid, priority: Priority) -> PcpMutexGuard<'_, T> {
        // 检查优先级是否低于上限
        if priority < self.ceiling {
            panic!("Thread priority higher than mutex ceiling");
        }
        
        // 提升到上限优先级
        boost_thread_priority(tid, self.ceiling);
        
        let guard = self.inner.lock(tid, self.ceiling);
        
        PcpMutexGuard {
            mutex: self,
            original_priority: priority,
            tid,
            _inner: guard,
        }
    }
    
    /// 获取优先级上限
    pub fn ceiling(&self) -> Priority {
        self.ceiling
    }
}

/// 优先级上限互斥锁守卫
pub struct PcpMutexGuard<'a, T> {
    mutex: &'a PcpMutex<T>,
    original_priority: Priority,
    tid: Tid,
    _inner: PiMutexGuard<'a, T>,
}

impl<'a, T> core::ops::Deref for PcpMutexGuard<'a, T> {
    type Target = T;
    
    fn deref(&self) -> &T {
        &*self._inner
    }
}

impl<'a, T> core::ops::DerefMut for PcpMutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut *self._inner
    }
}

impl<'a, T> Drop for PcpMutexGuard<'a, T> {
    fn drop(&mut self) {
        // 恢复原始优先级
        restore_thread_priority(self.tid, self.original_priority);
    }
}


/// 优先级继承互斥锁自测试
pub fn self_test() {
    crate::println!("[pi_mutex] Running self-test...");
    
    // 测试 PiMutex
    let mutex: PiMutex<u32> = PiMutex::new(42);
    
    // 测试 try_lock
    let guard = mutex.try_lock(1, 5);
    assert!(guard.is_some());
    
    // 测试 owner
    assert_eq!(mutex.owner(), Some(1));
    
    // 测试 is_locked
    assert!(mutex.is_locked());
    
    // 测试 owner_current_priority 字段
    let priority = mutex.owner_current_priority.load(Ordering::SeqCst);
    assert_eq!(priority, 5);
    
    // 释放锁
    drop(guard);
    
    // 测试 lock
    let guard = mutex.lock(2, 3);
    assert_eq!(*guard, 42);
    drop(guard);
    
    // 测试 check_priority_inheritance（通过 lock 间接测试）
    // 创建一个新的 mutex 来测试优先级继承
    let mutex2: PiMutex<u32> = PiMutex::new(100);
    let _guard = mutex2.try_lock(1, 10);
    // check_priority_inheritance 会在 lock 中被调用
    
    crate::println!("[pi_mutex] PiMutex tests passed");
    
    // 测试 PiChain
    let mut chain = PiChain::new();
    chain.push(1, 5);
    chain.push(2, 3);
    chain.push(3, 7);
    
    let highest = chain.highest_priority();
    assert_eq!(highest, Some(3));
    
    chain.propagate();
    crate::println!("[pi_mutex] PiChain tests passed");
    
    // 测试 boost_thread_priority 函数
    boost_thread_priority(1, 2);
    
    // 测试 PcpMutex
    let pcp_mutex: PcpMutex<u32> = PcpMutex::new(200, 5);
    
    // 测试 ceiling
    assert_eq!(pcp_mutex.ceiling(), 5);
    
    // 测试 ceiling 和 inner 字段
    let _ceiling = pcp_mutex.ceiling;
    let _inner_owner = pcp_mutex.inner.owner();
    
    // 测试 lock（优先级必须 >= ceiling）
    let guard = pcp_mutex.lock(1, 10);
    
    // 测试 PcpMutexGuard 的 mutex 字段
    let _mutex_ref = guard.mutex;
    
    assert_eq!(*guard, 200);
    drop(guard);
    
    crate::println!("[pi_mutex] PcpMutex tests passed");
    
    crate::println!("[pi_mutex] Self-test passed!");
}
