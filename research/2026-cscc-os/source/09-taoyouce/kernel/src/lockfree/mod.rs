//! 无锁并发数据结构
//!
//! 实现无锁队列、无锁哈希表等高性能并发数据结构

#![allow(dead_code)]

use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use core::ptr;
use alloc::boxed::Box;

/// 无锁队列节点
struct QueueNode<T> {
    value: Option<T>,
    next: AtomicPtr<QueueNode<T>>,
}

impl<T> QueueNode<T> {
    fn new(value: Option<T>) -> *mut Self {
        Box::into_raw(Box::new(Self {
            value,
            next: AtomicPtr::new(ptr::null_mut()),
        }))
    }
}

/// Michael-Scott 无锁队列
pub struct LockFreeQueue<T> {
    head: AtomicPtr<QueueNode<T>>,
    tail: AtomicPtr<QueueNode<T>>,
    len: AtomicUsize,
}

impl<T> LockFreeQueue<T> {
    /// 创建新队列
    pub fn new() -> Self {
        let sentinel = QueueNode::new(None);
        Self {
            head: AtomicPtr::new(sentinel),
            tail: AtomicPtr::new(sentinel),
            len: AtomicUsize::new(0),
        }
    }
    
    /// 入队
    pub fn push(&self, value: T) {
        let node = QueueNode::new(Some(value));
        
        loop {
            let tail = self.tail.load(Ordering::Acquire);
            let tail_ref = unsafe { &*tail };
            let next = tail_ref.next.load(Ordering::Acquire);
            
            if next.is_null() {
                // 尝试将新节点链接到尾部
                if tail_ref.next.compare_exchange(
                    ptr::null_mut(),
                    node,
                    Ordering::Release,
                    Ordering::Relaxed,
                ).is_ok() {
                    // 尝试更新尾指针
                    let _ = self.tail.compare_exchange(
                        tail,
                        node,
                        Ordering::Release,
                        Ordering::Relaxed,
                    );
                    self.len.fetch_add(1, Ordering::Relaxed);
                    return;
                }
            } else {
                // 帮助其他线程推进尾指针
                let _ = self.tail.compare_exchange(
                    tail,
                    next,
                    Ordering::Release,
                    Ordering::Relaxed,
                );
            }
        }
    }
    
    /// 出队
    pub fn pop(&self) -> Option<T> {
        loop {
            let head = self.head.load(Ordering::Acquire);
            let tail = self.tail.load(Ordering::Acquire);
            let head_ref = unsafe { &*head };
            let next = head_ref.next.load(Ordering::Acquire);
            
            if head == tail {
                if next.is_null() {
                    // 队列为空
                    return None;
                }
                // 尾指针落后，帮助推进
                let _ = self.tail.compare_exchange(
                    tail,
                    next,
                    Ordering::Release,
                    Ordering::Relaxed,
                );
            } else {
                // 读取值
                let next_ref = unsafe { &*next };
                if let Some(_value) = next_ref.value.as_ref() {
                    // 尝试移动头指针
                    if self.head.compare_exchange(
                        head,
                        next,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ).is_ok() {
                        self.len.fetch_sub(1, Ordering::Relaxed);
                        // 释放旧头节点
                        unsafe { drop(Box::from_raw(head)); }
                        // 返回值（需要克隆或移动）
                        return unsafe { ptr::read(&next_ref.value) };
                    }
                }
            }
        }
    }
    
    /// 获取长度
    pub fn len(&self) -> usize {
        self.len.load(Ordering::Relaxed)
    }
    
    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<T> Drop for LockFreeQueue<T> {
    fn drop(&mut self) {
        while self.pop().is_some() {}
        // 释放哨兵节点
        let head = self.head.load(Ordering::Relaxed);
        unsafe { drop(Box::from_raw(head)); }
    }
}

unsafe impl<T: Send> Send for LockFreeQueue<T> {}
unsafe impl<T: Send> Sync for LockFreeQueue<T> {}

/// Epoch-based 内存回收
pub mod epoch {
    use core::sync::atomic::{AtomicUsize, Ordering};
    use alloc::vec::Vec;
    use crate::sync::SpinLock;
    
    /// 全局 Epoch
    static GLOBAL_EPOCH: AtomicUsize = AtomicUsize::new(0);
    
    /// 延迟释放列表
    static GARBAGE: SpinLock<[Vec<DeferredFree>; 3]> = SpinLock::new([
        Vec::new(),
        Vec::new(),
        Vec::new(),
    ]);
    
    /// 延迟释放项
    struct DeferredFree {
        ptr: *mut u8,
        drop_fn: unsafe fn(*mut u8),
    }
    
    unsafe impl Send for DeferredFree {}
    
    /// Epoch 守卫
    pub struct Guard {
        epoch: usize,
    }
    
    impl Guard {
        /// 进入临界区
        pub fn pin() -> Self {
            let epoch = GLOBAL_EPOCH.load(Ordering::SeqCst);
            Self { epoch }
        }
    }
    
    impl Drop for Guard {
        fn drop(&mut self) {
            // 尝试推进 epoch 并回收垃圾
            try_advance();
        }
    }
    
    /// 延迟释放
    pub fn defer_free<T>(ptr: *mut T) {
        let epoch = GLOBAL_EPOCH.load(Ordering::SeqCst) % 3;
        let deferred = DeferredFree {
            ptr: ptr as *mut u8,
            drop_fn: |p| unsafe { drop(alloc::boxed::Box::from_raw(p as *mut T)) },
        };
        GARBAGE.lock()[epoch].push(deferred);
    }
    
    /// 尝试推进 epoch
    fn try_advance() {
        let current = GLOBAL_EPOCH.load(Ordering::SeqCst);
        let next = current.wrapping_add(1);
        
        if GLOBAL_EPOCH.compare_exchange(
            current,
            next,
            Ordering::SeqCst,
            Ordering::Relaxed,
        ).is_ok() {
            // 回收两个 epoch 前的垃圾
            let old_epoch = next.wrapping_sub(2) % 3;
            let mut garbage = GARBAGE.lock();
            for deferred in garbage[old_epoch].drain(..) {
                unsafe { (deferred.drop_fn)(deferred.ptr); }
            }
        }
    }
}

/// 无锁哈希表（简化实现）
pub struct LockFreeHashMap<K, V> {
    buckets: alloc::vec::Vec<AtomicPtr<HashNode<K, V>>>,
    size: AtomicUsize,
}

struct HashNode<K, V> {
    key: K,
    value: V,
    next: AtomicPtr<HashNode<K, V>>,
}

impl<K: Eq + core::hash::Hash, V> LockFreeHashMap<K, V> {
    /// 创建新哈希表
    pub fn new(capacity: usize) -> Self {
        let mut buckets = alloc::vec::Vec::with_capacity(capacity);
        for _ in 0..capacity {
            buckets.push(AtomicPtr::new(ptr::null_mut()));
        }
        Self {
            buckets,
            size: AtomicUsize::new(0),
        }
    }
    
    /// 计算哈希值
    fn hash(&self, key: &K) -> usize {
        use core::hash::Hasher;
        struct SimpleHasher(u64);
        impl Hasher for SimpleHasher {
            fn finish(&self) -> u64 { self.0 }
            fn write(&mut self, bytes: &[u8]) {
                for &b in bytes {
                    self.0 = self.0.wrapping_mul(31).wrapping_add(b as u64);
                }
            }
        }
        let mut hasher = SimpleHasher(0);
        core::hash::Hash::hash(key, &mut hasher);
        hasher.finish() as usize % self.buckets.len()
    }
    
    /// 插入
    pub fn insert(&self, key: K, value: V) -> Option<V> {
        let bucket = self.hash(&key);
        let node = Box::into_raw(Box::new(HashNode {
            key,
            value,
            next: AtomicPtr::new(ptr::null_mut()),
        }));
        
        loop {
            let head = self.buckets[bucket].load(Ordering::Acquire);
            unsafe { (*node).next.store(head, Ordering::Relaxed); }
            
            if self.buckets[bucket].compare_exchange(
                head,
                node,
                Ordering::Release,
                Ordering::Relaxed,
            ).is_ok() {
                self.size.fetch_add(1, Ordering::Relaxed);
                return None;
            }
        }
    }
    
    /// 查找
    pub fn get(&self, key: &K) -> Option<&V> {
        let bucket = self.hash(key);
        let mut current = self.buckets[bucket].load(Ordering::Acquire);
        
        while !current.is_null() {
            let node = unsafe { &*current };
            if &node.key == key {
                return Some(&node.value);
            }
            current = node.next.load(Ordering::Acquire);
        }
        
        None
    }
    
    /// 获取大小
    pub fn len(&self) -> usize {
        self.size.load(Ordering::Relaxed)
    }
}

unsafe impl<K: Send, V: Send> Send for LockFreeHashMap<K, V> {}
unsafe impl<K: Send + Sync, V: Send + Sync> Sync for LockFreeHashMap<K, V> {}
