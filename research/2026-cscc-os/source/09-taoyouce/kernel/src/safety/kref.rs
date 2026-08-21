//! 内核引用计数智能指针 (KRef)
//!
//! 类似于 Arc，但针对内核环境优化

use core::ops::Deref;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};
use core::marker::PhantomData;
use alloc::boxed::Box;

/// 引用计数内部结构
struct KRefInner<T> {
    /// 强引用计数
    strong: AtomicUsize,
    /// 弱引用计数
    weak: AtomicUsize,
    /// 数据
    data: T,
}

/// 内核引用计数指针
/// 
/// 线程安全的引用计数智能指针，用于内核对象生命周期管理
pub struct KRef<T> {
    ptr: NonNull<KRefInner<T>>,
    _marker: PhantomData<KRefInner<T>>,
}

unsafe impl<T: Send + Sync> Send for KRef<T> {}
unsafe impl<T: Send + Sync> Sync for KRef<T> {}

impl<T> KRef<T> {
    /// 创建新的 KRef
    pub fn new(data: T) -> Self {
        let inner = Box::new(KRefInner {
            strong: AtomicUsize::new(1),
            weak: AtomicUsize::new(1), // 一个隐式弱引用
            data,
        });
        
        Self {
            ptr: NonNull::new(Box::into_raw(inner)).unwrap(),
            _marker: PhantomData,
        }
    }
    
    /// 获取强引用计数
    pub fn strong_count(&self) -> usize {
        self.inner().strong.load(Ordering::SeqCst)
    }
    
    /// 获取弱引用计数
    pub fn weak_count(&self) -> usize {
        self.inner().weak.load(Ordering::SeqCst) - 1
    }
    
    /// 创建弱引用
    pub fn downgrade(&self) -> KWeak<T> {
        self.inner().weak.fetch_add(1, Ordering::SeqCst);
        KWeak {
            ptr: self.ptr,
            _marker: PhantomData,
        }
    }
    
    /// 尝试获取独占访问
    /// 如果只有一个强引用，返回可变引用
    pub fn get_mut(&mut self) -> Option<&mut T> {
        if self.strong_count() == 1 {
            unsafe { Some(&mut (*self.ptr.as_ptr()).data) }
        } else {
            None
        }
    }
    
    /// 尝试解包
    /// 如果只有一个强引用，返回内部值
    pub fn try_unwrap(self) -> Result<T, Self> {
        if self.inner().strong.compare_exchange(
            1, 0, Ordering::SeqCst, Ordering::SeqCst
        ).is_ok() {
            // 成功获取所有权
            let inner = unsafe { Box::from_raw(self.ptr.as_ptr()) };
            core::mem::forget(self);
            
            // 减少弱引用计数
            if inner.weak.fetch_sub(1, Ordering::SeqCst) == 1 {
                // 最后一个引用，内存已被释放
            }
            
            Ok(inner.data)
        } else {
            Err(self)
        }
    }
    
    fn inner(&self) -> &KRefInner<T> {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> Deref for KRef<T> {
    type Target = T;
    
    fn deref(&self) -> &T {
        unsafe { &(*self.ptr.as_ptr()).data }
    }
}

impl<T> Clone for KRef<T> {
    fn clone(&self) -> Self {
        self.inner().strong.fetch_add(1, Ordering::SeqCst);
        Self {
            ptr: self.ptr,
            _marker: PhantomData,
        }
    }
}

impl<T> Drop for KRef<T> {
    fn drop(&mut self) {
        if self.inner().strong.fetch_sub(1, Ordering::SeqCst) == 1 {
            // 最后一个强引用
            // 先执行析构
            unsafe {
                core::ptr::drop_in_place(&mut (*self.ptr.as_ptr()).data);
            }
            
            // 减少弱引用计数
            if self.inner().weak.fetch_sub(1, Ordering::SeqCst) == 1 {
                // 最后一个引用，释放内存
                unsafe {
                    let _ = Box::from_raw(self.ptr.as_ptr());
                }
            }
        }
    }
}

/// 弱引用
pub struct KWeak<T> {
    ptr: NonNull<KRefInner<T>>,
    _marker: PhantomData<KRefInner<T>>,
}

unsafe impl<T: Send + Sync> Send for KWeak<T> {}
unsafe impl<T: Send + Sync> Sync for KWeak<T> {}

impl<T> KWeak<T> {
    /// 尝试升级为强引用
    pub fn upgrade(&self) -> Option<KRef<T>> {
        let inner = unsafe { self.ptr.as_ref() };
        
        loop {
            let count = inner.strong.load(Ordering::SeqCst);
            if count == 0 {
                return None;
            }
            
            if inner.strong.compare_exchange_weak(
                count, count + 1, Ordering::SeqCst, Ordering::SeqCst
            ).is_ok() {
                return Some(KRef {
                    ptr: self.ptr,
                    _marker: PhantomData,
                });
            }
        }
    }
    
    /// 获取强引用计数
    pub fn strong_count(&self) -> usize {
        unsafe { self.ptr.as_ref().strong.load(Ordering::SeqCst) }
    }
}

impl<T> Clone for KWeak<T> {
    fn clone(&self) -> Self {
        unsafe { self.ptr.as_ref().weak.fetch_add(1, Ordering::SeqCst) };
        Self {
            ptr: self.ptr,
            _marker: PhantomData,
        }
    }
}

impl<T> Drop for KWeak<T> {
    fn drop(&mut self) {
        let inner = unsafe { self.ptr.as_ref() };
        if inner.weak.fetch_sub(1, Ordering::SeqCst) == 1 {
            // 最后一个弱引用，释放内存
            unsafe {
                let _ = Box::from_raw(self.ptr.as_ptr());
            }
        }
    }
}
