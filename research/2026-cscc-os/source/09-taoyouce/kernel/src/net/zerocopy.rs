//! 网络零拷贝传输
//!
//! 实现高性能零拷贝网络传输机制

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, AtomicBool, Ordering};
use crate::sync::SpinLock;
use crate::mm::{PhysPageNum, alloc_frame, dealloc_frame};

/// 零拷贝缓冲区大小
pub const ZC_BUFFER_SIZE: usize = 4096;

/// 最大 scatter-gather 条目数
pub const MAX_SG_ENTRIES: usize = 16;

/// Scatter-Gather 条目
#[derive(Debug, Clone, Copy)]
pub struct SgEntry {
    /// 物理页号
    pub phys_page: PhysPageNum,
    /// 长度
    pub len: usize,
    /// 偏移
    pub offset: usize,
}

impl SgEntry {
    pub fn new(phys_page: PhysPageNum, len: usize, offset: usize) -> Self {
        Self { phys_page, len, offset }
    }
}

/// Scatter-Gather 列表
pub struct SgList {
    /// 条目
    entries: Vec<SgEntry>,
    /// 总长度
    total_len: usize,
}

impl SgList {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            total_len: 0,
        }
    }
    
    /// 添加条目
    pub fn add(&mut self, entry: SgEntry) -> Result<(), &'static str> {
        if self.entries.len() >= MAX_SG_ENTRIES {
            return Err("SG list full");
        }
        self.total_len += entry.len;
        self.entries.push(entry);
        Ok(())
    }
    
    /// 获取条目
    pub fn entries(&self) -> &[SgEntry] {
        &self.entries
    }
    
    /// 获取总长度
    pub fn total_len(&self) -> usize {
        self.total_len
    }
    
    /// 是否为空
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 零拷贝缓冲区
pub struct ZeroCopyBuffer {
    /// 物理页面
    pages: Vec<PhysPageNum>,
    /// 引用计数
    ref_count: AtomicUsize,
    /// 是否已完成
    completed: AtomicBool,
    /// 数据长度
    data_len: usize,
    /// 头部偏移
    head_offset: usize,
}

impl ZeroCopyBuffer {
    /// 分配新缓冲区
    pub fn alloc(size: usize) -> Result<Self, &'static str> {
        let page_count = (size + ZC_BUFFER_SIZE - 1) / ZC_BUFFER_SIZE;
        let mut pages = Vec::with_capacity(page_count);
        
        for _ in 0..page_count {
            // 分配物理页面
            let page = alloc_frame().ok_or("Out of memory")?;
            pages.push(page);
        }
        
        Ok(Self {
            pages,
            ref_count: AtomicUsize::new(1),
            completed: AtomicBool::new(false),
            data_len: 0,
            head_offset: 0,
        })
    }
    
    /// 增加引用计数
    pub fn inc_ref(&self) {
        self.ref_count.fetch_add(1, Ordering::SeqCst);
    }
    
    /// 减少引用计数
    pub fn dec_ref(&self) -> bool {
        self.ref_count.fetch_sub(1, Ordering::SeqCst) == 1
    }
    
    /// 获取引用计数
    pub fn ref_count(&self) -> usize {
        self.ref_count.load(Ordering::SeqCst)
    }
    
    /// 标记完成
    pub fn complete(&self) {
        self.completed.store(true, Ordering::Release);
    }
    
    /// 是否完成
    pub fn is_completed(&self) -> bool {
        self.completed.load(Ordering::Acquire)
    }
    
    /// 获取 SG 列表
    pub fn to_sg_list(&self) -> SgList {
        let mut sg = SgList::new();
        let mut remaining = self.data_len;
        let mut first = true;
        
        for page in &self.pages {
            if remaining == 0 {
                break;
            }
            
            let offset = if first { self.head_offset } else { 0 };
            first = false;
            
            let len = (ZC_BUFFER_SIZE - offset).min(remaining);
            let _ = sg.add(SgEntry::new(*page, len, offset));
            remaining -= len;
        }
        
        sg
    }
    
    /// 设置数据长度
    pub fn set_data_len(&mut self, len: usize) {
        self.data_len = len;
    }
    
    /// 设置头部偏移
    pub fn set_head_offset(&mut self, offset: usize) {
        self.head_offset = offset;
    }
}

impl Drop for ZeroCopyBuffer {
    fn drop(&mut self) {
        // 释放物理页面
        for page in &self.pages {
            dealloc_frame(*page);
        }
    }
}

/// 零拷贝发送请求
pub struct ZcSendRequest {
    /// 缓冲区
    buffer: Arc<ZeroCopyBuffer>,
    /// 目标地址
    dest_addr: u32,
    /// 目标端口
    dest_port: u16,
    /// 完成回调
    completion: Option<Box<dyn FnOnce(Result<usize, &'static str>) + Send>>,
}

impl ZcSendRequest {
    pub fn new(buffer: Arc<ZeroCopyBuffer>, dest_addr: u32, dest_port: u16) -> Self {
        Self {
            buffer,
            dest_addr,
            dest_port,
            completion: None,
        }
    }
    
    /// 设置完成回调
    pub fn on_complete<F>(mut self, f: F) -> Self
    where
        F: FnOnce(Result<usize, &'static str>) + Send + 'static,
    {
        self.completion = Some(Box::new(f));
        self
    }
    
    /// 完成请求
    pub fn complete(self, result: Result<usize, &'static str>) {
        self.buffer.complete();
        if let Some(callback) = self.completion {
            callback(result);
        }
    }
}

/// 零拷贝接收请求
pub struct ZcRecvRequest {
    /// 缓冲区
    buffer: Arc<ZeroCopyBuffer>,
    /// 源地址
    src_addr: u32,
    /// 源端口
    src_port: u16,
    /// 接收长度
    recv_len: usize,
}

impl ZcRecvRequest {
    pub fn new(buffer: Arc<ZeroCopyBuffer>) -> Self {
        Self {
            buffer,
            src_addr: 0,
            src_port: 0,
            recv_len: 0,
        }
    }
    
    /// 设置接收信息
    pub fn set_recv_info(&mut self, src_addr: u32, src_port: u16, len: usize) {
        self.src_addr = src_addr;
        self.src_port = src_port;
        self.recv_len = len;
    }
    
    /// 获取缓冲区
    pub fn buffer(&self) -> &Arc<ZeroCopyBuffer> {
        &self.buffer
    }
}

/// 零拷贝 Socket
pub struct ZeroCopySocket {
    /// 本地端口
    local_port: u16,
    /// 发送队列
    send_queue: SpinLock<VecDeque<ZcSendRequest>>,
    /// 接收队列
    recv_queue: SpinLock<VecDeque<ZcRecvRequest>>,
    /// 预分配的接收缓冲区
    recv_buffers: SpinLock<Vec<Arc<ZeroCopyBuffer>>>,
    /// 是否启用零拷贝
    zc_enabled: AtomicBool,
}

impl ZeroCopySocket {
    pub fn new(local_port: u16) -> Self {
        Self {
            local_port,
            send_queue: SpinLock::new(VecDeque::new()),
            recv_queue: SpinLock::new(VecDeque::new()),
            recv_buffers: SpinLock::new(Vec::new()),
            zc_enabled: AtomicBool::new(true),
        }
    }
    
    /// 预分配接收缓冲区
    pub fn prealloc_recv_buffers(&self, count: usize) -> Result<(), &'static str> {
        let mut buffers = self.recv_buffers.lock();
        for _ in 0..count {
            let buffer = Arc::new(ZeroCopyBuffer::alloc(ZC_BUFFER_SIZE)?);
            buffers.push(buffer);
        }
        Ok(())
    }
    
    /// 零拷贝发送
    pub fn send_zc(&self, buffer: Arc<ZeroCopyBuffer>, dest_addr: u32, dest_port: u16) -> Result<(), &'static str> {
        if !self.zc_enabled.load(Ordering::Acquire) {
            return Err("Zero-copy disabled");
        }
        
        let request = ZcSendRequest::new(buffer, dest_addr, dest_port);
        self.send_queue.lock().push_back(request);
        
        // 触发发送
        self.process_send_queue();
        
        Ok(())
    }
    
    /// 零拷贝接收
    pub fn recv_zc(&self) -> Option<ZcRecvRequest> {
        self.recv_queue.lock().pop_front()
    }
    
    /// 处理发送队列
    fn process_send_queue(&self) {
        let mut queue = self.send_queue.lock();
        
        while let Some(request) = queue.pop_front() {
            let sg_list = request.buffer.to_sg_list();
            
            // 提交到网络设备
            // 实际实现需要调用网络驱动的 DMA 发送接口
            let result = self.submit_sg_send(&sg_list, request.dest_addr, request.dest_port);
            
            request.complete(result);
        }
    }
    
    /// 提交 SG 发送
    fn submit_sg_send(&self, sg_list: &SgList, dest_addr: u32, dest_port: u16) -> Result<usize, &'static str> {
        // 实际实现需要：
        // 1. 构建网络包头
        // 2. 设置 DMA 描述符
        // 3. 触发 DMA 传输
        let _ = (sg_list, dest_addr, dest_port);
        Ok(sg_list.total_len())
    }
    
    /// 处理接收完成
    pub fn handle_recv_complete(&self, phys_page: PhysPageNum, len: usize, src_addr: u32, src_port: u16) {
        // 查找对应的缓冲区
        let buffers = self.recv_buffers.lock();
        
        for buffer in buffers.iter() {
            if buffer.pages.first() == Some(&phys_page) {
                let mut request = ZcRecvRequest::new(buffer.clone());
                request.set_recv_info(src_addr, src_port, len);
                self.recv_queue.lock().push_back(request);
                return;
            }
        }
    }
    
    /// 启用/禁用零拷贝
    pub fn set_zc_enabled(&self, enabled: bool) {
        self.zc_enabled.store(enabled, Ordering::Release);
    }
}

/// 页面池（用于高效的缓冲区管理）
pub struct PagePool {
    /// 空闲页面
    free_pages: SpinLock<Vec<PhysPageNum>>,
    /// 池大小
    pool_size: usize,
    /// 已分配数量
    allocated: AtomicUsize,
}

impl PagePool {
    /// 创建页面池
    pub fn new(size: usize) -> Result<Self, &'static str> {
        let mut free_pages = Vec::with_capacity(size);
        
        for _ in 0..size {
            let page = alloc_frame().ok_or("Out of memory")?;
            free_pages.push(page);
        }
        
        Ok(Self {
            free_pages: SpinLock::new(free_pages),
            pool_size: size,
            allocated: AtomicUsize::new(0),
        })
    }
    
    /// 分配页面
    pub fn alloc(&self) -> Option<PhysPageNum> {
        let mut pages = self.free_pages.lock();
        if let Some(page) = pages.pop() {
            self.allocated.fetch_add(1, Ordering::SeqCst);
            Some(page)
        } else {
            // 池耗尽，尝试从系统分配
            alloc_frame()
        }
    }
    
    /// 释放页面
    pub fn free(&self, page: PhysPageNum) {
        let mut pages = self.free_pages.lock();
        if pages.len() < self.pool_size {
            pages.push(page);
            self.allocated.fetch_sub(1, Ordering::SeqCst);
        } else {
            // 池已满，释放到系统
            dealloc_frame(page);
        }
    }
    
    /// 获取统计信息
    pub fn stats(&self) -> (usize, usize) {
        let free = self.free_pages.lock().len();
        let allocated = self.allocated.load(Ordering::SeqCst);
        (free, allocated)
    }
}

/// 全局页面池
static PAGE_POOL: SpinLock<Option<PagePool>> = SpinLock::new(None);

/// 初始化零拷贝子系统
pub fn init() {
    // 创建页面池（256 个页面）
    match PagePool::new(256) {
        Ok(pool) => {
            *PAGE_POOL.lock() = Some(pool);
            crate::println!("[zerocopy] Page pool initialized with 256 pages");
        }
        Err(e) => {
            crate::println!("[zerocopy] Failed to initialize page pool: {}", e);
        }
    }
}

/// 从池分配页面
pub fn alloc_page() -> Option<PhysPageNum> {
    PAGE_POOL.lock()
        .as_ref()
        .and_then(|p| p.alloc())
}

/// 释放页面到池
pub fn free_page(page: PhysPageNum) {
    if let Some(ref pool) = *PAGE_POOL.lock() {
        pool.free(page);
    } else {
        dealloc_frame(page);
    }
}
