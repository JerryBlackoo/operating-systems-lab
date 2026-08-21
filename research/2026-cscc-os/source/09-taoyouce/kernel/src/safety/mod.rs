//! Rust 内存安全增强
//!
//! 提供类型状态模式、安全智能指针等内存安全机制

use core::marker::PhantomData;

pub mod typestate;
pub mod kref;

// ============================================================================
// 类型状态模式 (Typestate Pattern)
// ============================================================================

/// 资源状态标记 trait
pub trait ResourceState {}

/// 未初始化状态
pub struct Uninitialized;
impl ResourceState for Uninitialized {}

/// 已初始化状态
pub struct Initialized;
impl ResourceState for Initialized {}

/// 已激活状态
pub struct Active;
impl ResourceState for Active {}

/// 已关闭状态
pub struct Closed;
impl ResourceState for Closed {}

/// 带状态的资源包装器
/// 使用类型系统在编译期保证资源使用顺序
pub struct StatefulResource<T, S: ResourceState> {
    inner: T,
    _state: PhantomData<S>,
}

impl<T> StatefulResource<T, Uninitialized> {
    /// 创建未初始化的资源
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            _state: PhantomData,
        }
    }
    
    /// 初始化资源，转换到已初始化状态
    pub fn initialize(self) -> StatefulResource<T, Initialized> {
        StatefulResource {
            inner: self.inner,
            _state: PhantomData,
        }
    }
}

impl<T> StatefulResource<T, Initialized> {
    /// 激活资源
    pub fn activate(self) -> StatefulResource<T, Active> {
        StatefulResource {
            inner: self.inner,
            _state: PhantomData,
        }
    }
}

impl<T> StatefulResource<T, Active> {
    /// 获取资源引用（只有激活状态才能访问）
    pub fn get(&self) -> &T {
        &self.inner
    }
    
    /// 获取可变引用
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }
    
    /// 关闭资源
    pub fn close(self) -> StatefulResource<T, Closed> {
        StatefulResource {
            inner: self.inner,
            _state: PhantomData,
        }
    }
}

impl<T> StatefulResource<T, Closed> {
    /// 重新初始化
    pub fn reinitialize(self) -> StatefulResource<T, Initialized> {
        StatefulResource {
            inner: self.inner,
            _state: PhantomData,
        }
    }
}


/// 初始化安全子系统
pub fn init() {
    // 运行类型状态模式自检
    self_test();
    crate::println!("[safety] Rust safety subsystem initialized");
}

/// 静默初始化安全子系统（不输出调试信息）
pub fn init_quiet() {
    // 运行自检但不输出
    self_test_quiet();
}

/// 安全子系统自检
pub fn self_test() {
    crate::println!("[safety] Running self-test...");
    self_test_quiet();
    crate::println!("[safety] Self-test passed!");
}

/// 静默自检（不输出调试信息）
fn self_test_quiet() {
    // 测试类型状态模式
    let resource: StatefulResource<u32, Uninitialized> = StatefulResource::new(42);
    let resource = resource.initialize();
    let mut resource = resource.activate();
    
    // 只有在 Active 状态才能访问
    assert!(*resource.get() == 42);
    *resource.get_mut() = 100;
    assert!(*resource.get() == 100);
    
    let resource = resource.close();
    let _resource = resource.reinitialize();
    
    // 测试 kref 引用计数
    use kref::{KRef, KWeak};
    let kref1 = KRef::new(123);
    let kref2 = kref1.clone();
    assert!(*kref1 == 123);
    assert!(*kref2 == 123);
    
    // 测试 strong_count 和 weak_count
    let _strong = kref1.strong_count();
    let _weak = kref1.weak_count();
    
    // 测试 downgrade 和 upgrade
    let weak_ref: KWeak<i32> = kref1.downgrade();
    let _weak_strong = weak_ref.strong_count();
    
    if let Some(upgraded) = weak_ref.upgrade() {
        assert!(*upgraded == 123);
    }
    
    // 测试 get_mut
    drop(kref2);
    let mut kref_single = KRef::new(456);
    if let Some(val) = kref_single.get_mut() {
        *val = 789;
    }
    
    // 测试 try_unwrap
    let kref_unwrap = KRef::new(999);
    match kref_unwrap.try_unwrap() {
        Ok(val) => crate::println!("[safety] try_unwrap succeeded: {}", val),
        Err(_) => crate::println!("[safety] try_unwrap failed (multiple refs)"),
    }
    
    drop(kref1);
    
    // 测试 typestate 模块
    use typestate::{
        TypedFile, FileClosed,
        TypedLock, Unlocked,
        TypedConnection, Disconnected,
        BoundedArray,
    };
    
    // 测试 TypedFile
    let file = TypedFile::<FileClosed>::new();
    if let Ok(readonly_file) = file.open_readonly("/test") {
        let mut buf = [0u8; 10];
        let _ = readonly_file.read(&mut buf);
        let fd = readonly_file.fd();
        crate::println!("[safety] TypedFile readonly fd: {}", fd);
        let _closed = readonly_file.close();
    }
    
    let file2 = TypedFile::<FileClosed>::new();
    if let Ok(rw_file) = file2.open_readwrite("/test2") {
        let mut buf = [0u8; 10];
        let _ = rw_file.read(&mut buf);
        let _ = rw_file.write(&[1, 2, 3]);
        let fd = rw_file.fd();
        crate::println!("[safety] TypedFile readwrite fd: {}", fd);
        let _closed = rw_file.close();
    }
    
    // 测试 TypedLock
    let lock = TypedLock::<i32, Unlocked>::new(42);
    let locked = lock.lock();
    let val = locked.get();
    crate::println!("[safety] TypedLock value: {}", val);
    let mut locked_mut = TypedLock::<i32, Unlocked>::new(100).lock();
    *locked_mut.get_mut() = 200;
    let _unlocked = locked_mut.unlock();
    
    // 测试 TypedConnection
    let conn = TypedConnection::<Disconnected>::new();
    let connecting = conn.connect("127.0.0.1:8080");
    match connecting.wait() {
        Ok(connected) => {
            let _ = connected.send(&[1, 2, 3]);
            let mut buf = [0u8; 10];
            let _ = connected.recv(&mut buf);
            let _disconnected = connected.disconnect();
        }
        Err(_disconnected) => {
            crate::println!("[safety] Connection failed");
        }
    }
    
    // 测试 BoundedArray
    let mut arr = BoundedArray::<i32, 10>::new();
    arr.set::<0>(42);
    arr.set::<5>(100);
    let _val0 = arr.get::<0>();
    let _val5 = arr.get::<5>();
}
