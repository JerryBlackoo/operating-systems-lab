//! 类型状态模式示例
//!
//! 展示如何使用类型系统在编译期保证资源使用顺序

use core::marker::PhantomData;
use alloc::boxed::Box;

// ============================================================================
// 文件句柄类型状态示例
// ============================================================================

/// 文件状态 trait
pub trait FileState {}

/// 已关闭状态
pub struct FileClosed;
impl FileState for FileClosed {}

/// 已打开状态
pub struct FileOpen;
impl FileState for FileOpen {}

/// 只读模式
pub struct ReadOnly;
impl FileState for ReadOnly {}

/// 读写模式
pub struct ReadWrite;
impl FileState for ReadWrite {}

/// 类型安全的文件句柄
pub struct TypedFile<S: FileState> {
    fd: i32,
    _state: PhantomData<S>,
}

impl TypedFile<FileClosed> {
    /// 创建关闭状态的文件句柄
    pub fn new() -> Self {
        Self {
            fd: -1,
            _state: PhantomData,
        }
    }
    
    /// 以只读模式打开
    pub fn open_readonly(self, _path: &str) -> Result<TypedFile<ReadOnly>, i32> {
        // 实际实现会调用系统调用
        Ok(TypedFile {
            fd: 3, // 模拟 fd
            _state: PhantomData,
        })
    }
    
    /// 以读写模式打开
    pub fn open_readwrite(self, _path: &str) -> Result<TypedFile<ReadWrite>, i32> {
        Ok(TypedFile {
            fd: 3,
            _state: PhantomData,
        })
    }
}

impl TypedFile<ReadOnly> {
    /// 获取文件描述符
    pub fn fd(&self) -> i32 {
        self.fd
    }
    
    /// 读取数据（只读模式可用）
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, i32> {
        // 实际实现会使用 self.fd
        let _ = (self.fd, buf);
        Ok(0)
    }
    
    /// 关闭文件
    pub fn close(self) -> TypedFile<FileClosed> {
        TypedFile {
            fd: -1,
            _state: PhantomData,
        }
    }
}

impl TypedFile<ReadWrite> {
    /// 获取文件描述符
    pub fn fd(&self) -> i32 {
        self.fd
    }
    
    /// 读取数据
    pub fn read(&self, buf: &mut [u8]) -> Result<usize, i32> {
        let _ = (self.fd, buf);
        Ok(0)
    }
    
    /// 写入数据（只有读写模式可用）
    pub fn write(&self, buf: &[u8]) -> Result<usize, i32> {
        let _ = (self.fd, buf);
        Ok(buf.len())
    }
    
    /// 关闭文件
    pub fn close(self) -> TypedFile<FileClosed> {
        TypedFile {
            fd: -1,
            _state: PhantomData,
        }
    }
}

// ============================================================================
// 锁类型状态示例
// ============================================================================

/// 锁状态
pub trait LockState {}

/// 未锁定
pub struct Unlocked;
impl LockState for Unlocked {}

/// 已锁定
pub struct Locked;
impl LockState for Locked {}

/// 类型安全的锁守卫
pub struct TypedLock<T, S: LockState> {
    data: *mut T,
    _state: PhantomData<S>,
}

impl<T> TypedLock<T, Unlocked> {
    /// 创建未锁定的锁
    pub fn new(data: T) -> Self {
        Self {
            data: Box::into_raw(Box::new(data)),
            _state: PhantomData,
        }
    }
    
    /// 获取锁
    pub fn lock(self) -> TypedLock<T, Locked> {
        // 实际实现会进行原子操作
        TypedLock {
            data: self.data,
            _state: PhantomData,
        }
    }
}

impl<T> TypedLock<T, Locked> {
    /// 访问数据（只有锁定状态可用）
    pub fn get(&self) -> &T {
        unsafe { &*self.data }
    }
    
    /// 可变访问
    pub fn get_mut(&mut self) -> &mut T {
        unsafe { &mut *self.data }
    }
    
    /// 释放锁
    pub fn unlock(self) -> TypedLock<T, Unlocked> {
        TypedLock {
            data: self.data,
            _state: PhantomData,
        }
    }
}

// ============================================================================
// 网络连接类型状态示例
// ============================================================================

/// 连接状态
pub trait ConnectionState {}

/// 已断开
pub struct Disconnected;
impl ConnectionState for Disconnected {}

/// 正在连接
pub struct Connecting;
impl ConnectionState for Connecting {}

/// 已连接
pub struct Connected;
impl ConnectionState for Connected {}

/// 类型安全的网络连接
pub struct TypedConnection<S: ConnectionState> {
    socket_fd: i32,
    _state: PhantomData<S>,
}

impl TypedConnection<Disconnected> {
    pub fn new() -> Self {
        Self {
            socket_fd: -1,
            _state: PhantomData,
        }
    }
    
    /// 开始连接
    pub fn connect(self, _addr: &str) -> TypedConnection<Connecting> {
        TypedConnection {
            socket_fd: 4,
            _state: PhantomData,
        }
    }
}

impl TypedConnection<Connecting> {
    /// 等待连接完成
    pub fn wait(self) -> Result<TypedConnection<Connected>, TypedConnection<Disconnected>> {
        // 模拟连接成功
        Ok(TypedConnection {
            socket_fd: self.socket_fd,
            _state: PhantomData,
        })
    }
}

impl TypedConnection<Connected> {
    /// 发送数据（只有已连接状态可用）
    pub fn send(&self, data: &[u8]) -> Result<usize, i32> {
        let _ = data;
        Ok(data.len())
    }
    
    /// 接收数据
    pub fn recv(&self, buf: &mut [u8]) -> Result<usize, i32> {
        let _ = buf;
        Ok(0)
    }
    
    /// 断开连接
    pub fn disconnect(self) -> TypedConnection<Disconnected> {
        TypedConnection {
            socket_fd: -1,
            _state: PhantomData,
        }
    }
}

// ============================================================================
// 编译期边界检查示例
// ============================================================================

/// 带编译期边界检查的数组
pub struct BoundedArray<T, const N: usize> {
    data: [T; N],
}

impl<T: Default + Copy, const N: usize> BoundedArray<T, N> {
    pub fn new() -> Self {
        Self {
            data: [T::default(); N],
        }
    }
    
    /// 编译期检查的索引访问
    pub fn get<const I: usize>(&self) -> &T 
    where
        Assert<{ I < N }>: IsTrue,
    {
        &self.data[I]
    }
    
    /// 编译期检查的索引设置
    pub fn set<const I: usize>(&mut self, value: T)
    where
        Assert<{ I < N }>: IsTrue,
    {
        self.data[I] = value;
    }
}

/// 编译期断言辅助
pub struct Assert<const COND: bool>;

/// 断言为真的标记 trait
pub trait IsTrue {}

impl IsTrue for Assert<true> {}
