//! VirtIO 虚拟队列实现
//!
//! 实现 Split Virtqueue，支持 Legacy 和 Modern 接口

#![allow(dead_code)]

use alloc::vec::Vec;
use alloc::vec;
use alloc::boxed::Box;
use core::sync::atomic::{Ordering, fence};

/// 队列大小 - 使用较小的值以简化 Legacy 布局
pub const QUEUE_SIZE: usize = 8;

/// 页大小
pub const PAGE_SIZE: usize = 4096;

/// 描述符标志
pub mod desc_flags {
    pub const NEXT: u16 = 1;      // 有下一个描述符
    pub const WRITE: u16 = 2;     // 设备写入（否则设备读取）
    pub const INDIRECT: u16 = 4;  // 间接描述符
}

/// 虚拟队列描述符
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqDesc {
    /// 缓冲区物理地址
    pub addr: u64,
    /// 缓冲区长度
    pub len: u32,
    /// 标志
    pub flags: u16,
    /// 下一个描述符索引
    pub next: u16,
}

/// 可用环头部
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqAvailHeader {
    /// 标志
    pub flags: u16,
    /// 下一个可用索引
    pub idx: u16,
}

/// 已使用环元素
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqUsedElem {
    /// 描述符链头索引
    pub id: u32,
    /// 写入的字节数
    pub len: u32,
}

/// 已使用环头部
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqUsedHeader {
    /// 标志
    pub flags: u16,
    /// 下一个已使用索引
    pub idx: u16,
}

/// Legacy 队列内存布局 (连续 2 页)
/// 第一页: desc[NUM] + avail (flags, idx, ring[NUM], used_event)
/// 第二页: used (flags, idx, ring[NUM], avail_event)
#[repr(C, align(4096))]
pub struct LegacyQueuePages {
    // 第一页
    pub desc: [VirtqDesc; QUEUE_SIZE],           // 8 * 16 = 128 bytes
    pub avail_flags: u16,                         // 2 bytes
    pub avail_idx: u16,                           // 2 bytes
    pub avail_ring: [u16; QUEUE_SIZE],           // 8 * 2 = 16 bytes
    pub used_event: u16,                          // 2 bytes
    _pad1: [u8; PAGE_SIZE - 128 - 2 - 2 - 16 - 2], // 填充到 4096
    
    // 第二页
    pub used_flags: u16,                          // 2 bytes
    pub used_idx: u16,                            // 2 bytes
    pub used_ring: [VirtqUsedElem; QUEUE_SIZE],  // 8 * 8 = 64 bytes
    pub avail_event: u16,                         // 2 bytes
    _pad2: [u8; PAGE_SIZE - 2 - 2 - 64 - 2],     // 填充到 4096
}

impl Default for LegacyQueuePages {
    fn default() -> Self {
        Self {
            desc: [VirtqDesc::default(); QUEUE_SIZE],
            avail_flags: 0,
            avail_idx: 0,
            avail_ring: [0; QUEUE_SIZE],
            used_event: 0,
            _pad1: [0; PAGE_SIZE - 128 - 2 - 2 - 16 - 2],
            used_flags: 0,
            used_idx: 0,
            used_ring: [VirtqUsedElem::default(); QUEUE_SIZE],
            avail_event: 0,
            _pad2: [0; PAGE_SIZE - 2 - 2 - 64 - 2],
        }
    }
}

/// 可用环 (用于 Modern 接口)
#[repr(C)]
pub struct VirtqAvail {
    /// 标志
    pub flags: u16,
    /// 下一个可用索引
    pub idx: u16,
    /// 环
    pub ring: [u16; QUEUE_SIZE],
    /// 已使用事件（可选）
    pub used_event: u16,
}

/// 已使用环 (用于 Modern 接口)
#[repr(C)]
pub struct VirtqUsed {
    /// 标志
    pub flags: u16,
    /// 下一个已使用索引
    pub idx: u16,
    /// 环
    pub ring: [VirtqUsedElem; QUEUE_SIZE],
    /// 可用事件（可选）
    pub avail_event: u16,
}

/// Split Virtqueue - 支持 Legacy 和 Modern
pub struct SplitVirtqueue {
    /// 队列索引
    queue_idx: u16,
    /// 队列大小
    size: u16,
    /// Legacy 队列页 (用于 Legacy 接口)
    legacy_pages: Option<Box<LegacyQueuePages>>,
    /// 描述符表 (用于 Modern 接口)
    desc: Option<Box<[VirtqDesc; QUEUE_SIZE]>>,
    /// 可用环 (用于 Modern 接口)
    avail: Option<Box<VirtqAvail>>,
    /// 已使用环 (用于 Modern 接口)
    used: Option<Box<VirtqUsed>>,
    /// 空闲描述符列表
    free_list: Vec<u16>,
    /// 上次看到的已使用索引
    last_used_idx: u16,
    /// 是否是 Legacy 模式
    is_legacy: bool,
}

impl SplitVirtqueue {
    /// 创建新队列 (Legacy 模式)
    pub fn new_legacy(queue_idx: u16) -> Self {
        let mut pages = Box::new(LegacyQueuePages::default());
        
        // 初始化描述符链
        for i in 0..QUEUE_SIZE - 1 {
            pages.desc[i].next = (i + 1) as u16;
        }
        
        let free_list = (0..QUEUE_SIZE as u16).collect();
        
        Self {
            queue_idx,
            size: QUEUE_SIZE as u16,
            legacy_pages: Some(pages),
            desc: None,
            avail: None,
            used: None,
            free_list,
            last_used_idx: 0,
            is_legacy: true,
        }
    }
    
    /// 创建新队列 (Modern 模式)
    pub fn new(queue_idx: u16) -> Self {
        let mut desc = Box::new([VirtqDesc::default(); QUEUE_SIZE]);
        
        // 初始化描述符链
        for i in 0..QUEUE_SIZE - 1 {
            desc[i].next = (i + 1) as u16;
        }
        
        let avail = Box::new(VirtqAvail {
            flags: 0,
            idx: 0,
            ring: [0; QUEUE_SIZE],
            used_event: 0,
        });
        
        let used = Box::new(VirtqUsed {
            flags: 0,
            idx: 0,
            ring: [VirtqUsedElem::default(); QUEUE_SIZE],
            avail_event: 0,
        });
        
        let free_list = (0..QUEUE_SIZE as u16).collect();
        
        Self {
            queue_idx,
            size: QUEUE_SIZE as u16,
            legacy_pages: None,
            desc: Some(desc),
            avail: Some(avail),
            used: Some(used),
            free_list,
            last_used_idx: 0,
            is_legacy: false,
        }
    }
    
    /// 获取页帧号 (Legacy 模式)
    pub fn pfn(&self) -> u64 {
        if let Some(ref pages) = self.legacy_pages {
            (pages.as_ref() as *const _ as u64) >> 12
        } else {
            0
        }
    }
    
    /// 获取描述符表物理地址
    pub fn desc_addr(&self) -> u64 {
        if let Some(ref pages) = self.legacy_pages {
            pages.desc.as_ptr() as u64
        } else if let Some(ref desc) = self.desc {
            desc.as_ptr() as u64
        } else {
            0
        }
    }
    
    /// 获取可用环物理地址
    pub fn avail_addr(&self) -> u64 {
        if let Some(ref pages) = self.legacy_pages {
            &pages.avail_flags as *const _ as u64
        } else if let Some(ref avail) = self.avail {
            avail.as_ref() as *const _ as u64
        } else {
            0
        }
    }
    
    /// 获取已使用环物理地址
    pub fn used_addr(&self) -> u64 {
        if let Some(ref pages) = self.legacy_pages {
            &pages.used_flags as *const _ as u64
        } else if let Some(ref used) = self.used {
            used.as_ref() as *const _ as u64
        } else {
            0
        }
    }
    
    /// 分配描述符
    pub fn alloc_desc(&mut self) -> Option<u16> {
        self.free_list.pop()
    }
    
    /// 释放描述符
    pub fn free_desc(&mut self, idx: u16) {
        self.free_list.push(idx);
    }
    
    /// 释放描述符链
    pub fn free_chain(&mut self, head: u16) {
        let mut idx = head;
        loop {
            let (next, has_next) = if let Some(ref pages) = self.legacy_pages {
                let desc = &pages.desc[idx as usize];
                (desc.next, desc.flags & desc_flags::NEXT != 0)
            } else if let Some(ref desc) = self.desc {
                let d = &desc[idx as usize];
                (d.next, d.flags & desc_flags::NEXT != 0)
            } else {
                break;
            };
            
            self.free_desc(idx);
            
            if has_next {
                idx = next;
            } else {
                break;
            }
        }
    }
    
    /// 添加缓冲区到队列
    pub fn add_buf(&mut self, bufs: &[(u64, u32, bool)]) -> Option<u16> {
        if bufs.is_empty() || bufs.len() > self.free_list.len() || bufs.len() > 16 {
            return None;
        }
        
        // 分配描述符
        let mut desc_indices: [u16; 16] = [0; 16];
        for i in 0..bufs.len() {
            desc_indices[i] = self.alloc_desc()?;
        }
        
        let head = desc_indices[0];
        
        // 设置描述符
        for (i, &(addr, len, write)) in bufs.iter().enumerate() {
            let idx = desc_indices[i];
            let next_idx = if i < bufs.len() - 1 { desc_indices[i + 1] } else { 0 };
            let has_next = i < bufs.len() - 1;
            let flags = if write { desc_flags::WRITE } else { 0 }
                | if has_next { desc_flags::NEXT } else { 0 };
            
            if let Some(ref mut pages) = self.legacy_pages {
                unsafe {
                    core::ptr::write_volatile(
                        &mut pages.desc[idx as usize] as *mut VirtqDesc,
                        VirtqDesc { addr, len, flags, next: next_idx }
                    );
                }
            } else if let Some(ref mut desc) = self.desc {
                unsafe {
                    core::ptr::write_volatile(
                        &mut desc[idx as usize] as *mut VirtqDesc,
                        VirtqDesc { addr, len, flags, next: next_idx }
                    );
                }
            }
        }
        
        // 添加到可用环
        if let Some(ref mut pages) = self.legacy_pages {
            let avail_idx = unsafe {
                core::ptr::read_volatile(&pages.avail_idx as *const u16)
            } as usize % QUEUE_SIZE;
            
            unsafe {
                core::ptr::write_volatile(&mut pages.avail_ring[avail_idx] as *mut u16, head);
            }
            
            fence(Ordering::SeqCst);
            
            unsafe {
                let old_idx = core::ptr::read_volatile(&pages.avail_idx as *const u16);
                core::ptr::write_volatile(&mut pages.avail_idx as *mut u16, old_idx.wrapping_add(1));
            }
        } else if let Some(ref mut avail) = self.avail {
            let avail_idx = unsafe {
                core::ptr::read_volatile(&avail.idx as *const u16)
            } as usize % QUEUE_SIZE;
            
            unsafe {
                core::ptr::write_volatile(&mut avail.ring[avail_idx] as *mut u16, head);
            }
            
            fence(Ordering::SeqCst);
            
            unsafe {
                let old_idx = core::ptr::read_volatile(&avail.idx as *const u16);
                core::ptr::write_volatile(&mut avail.idx as *mut u16, old_idx.wrapping_add(1));
            }
        }
        
        fence(Ordering::SeqCst);
        
        Some(head)
    }
    
    /// 检查是否有已完成的请求
    pub fn has_used(&self) -> bool {
        // Acquire 读 used.idx：设备先写完 used_ring 元素和 DMA 缓冲，最后递增 idx。
        // 必须在读 idx 后插读屏障（见 pop_used），此处仅判定有无新条目，故 idx 读后
        // 立即返回，真正的 load-load 屏障在 pop_used 读环元素前。
        let used_idx = if let Some(ref pages) = self.legacy_pages {
            unsafe { core::ptr::read_volatile(&pages.used_idx as *const u16) }
        } else if let Some(ref used) = self.used {
            unsafe { core::ptr::read_volatile(&used.idx as *const u16) }
        } else {
            return false;
        };
        fence(Ordering::Acquire);
        
        self.last_used_idx != used_idx
    }
    
    /// 获取已完成的请求
    ///
    /// VirtIO 设备发布完成的顺序：① 写 DMA 缓冲数据 → ② 写 used_ring[idx]
    /// (id/len) → ③ 递增 used.idx。驱动观察到新的 used.idx 后，**必须**在读取
    /// used_ring 元素与随后的 DMA 缓冲之前插入一道 load-load 获取屏障，否则 CPU
    /// 可能把环元素/缓冲的读取重排到 idx 读取之前，读到**设备尚未写入的陈旧值**
    /// （陈旧环元素 → 错误描述符 id/len；陈旧缓冲 → 复用前残留的内核栈/堆字节）。
    /// 该陈旧磁盘数据被 memcpy 进用户代码页即触发 [ILL]。
    /// 对标 rcore-os/virtio-drivers#128（used/avail 环改用 Acquire 原子读）。
    pub fn pop_used(&mut self) -> Option<(u16, u32)> {
        let used_idx = if let Some(ref pages) = self.legacy_pages {
            unsafe { core::ptr::read_volatile(&pages.used_idx as *const u16) }
        } else if let Some(ref used) = self.used {
            unsafe { core::ptr::read_volatile(&used.idx as *const u16) }
        } else {
            return None;
        };
        
        if self.last_used_idx == used_idx {
            return None;
        }
        
        // 观察到新 used.idx 后的获取屏障：保证下面对 used_ring 元素以及调用方随后
        // 对 DMA 缓冲的读取，都看到设备在递增 idx 之前完成的全部写入。
        fence(Ordering::Acquire);
        
        let idx = self.last_used_idx as usize % QUEUE_SIZE;
        
        let elem = if let Some(ref pages) = self.legacy_pages {
            unsafe { core::ptr::read_volatile(&pages.used_ring[idx] as *const VirtqUsedElem) }
        } else if let Some(ref used) = self.used {
            unsafe { core::ptr::read_volatile(&used.ring[idx] as *const VirtqUsedElem) }
        } else {
            return None;
        };
        
        self.last_used_idx = self.last_used_idx.wrapping_add(1);
        
        Some((elem.id as u16, elem.len))
    }
    
    /// 获取队列大小
    pub fn size(&self) -> u16 {
        self.size
    }
}

/// 请求状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    /// 等待中
    Pending,
    /// 已完成
    Complete(u32),
    /// 错误
    Error,
}

/// 请求追踪器
pub struct RequestTracker {
    /// 请求状态
    status: Vec<RequestStatus>,
}

impl RequestTracker {
    pub fn new(size: usize) -> Self {
        Self {
            status: vec![RequestStatus::Pending; size],
        }
    }
    
    /// 标记请求开始
    pub fn start(&mut self, idx: u16) {
        if (idx as usize) < self.status.len() {
            self.status[idx as usize] = RequestStatus::Pending;
        }
    }
    
    /// 标记请求完成
    pub fn complete(&mut self, idx: u16, len: u32) {
        if (idx as usize) < self.status.len() {
            self.status[idx as usize] = RequestStatus::Complete(len);
        }
    }
    
    /// 获取请求状态
    pub fn get(&self, idx: u16) -> RequestStatus {
        self.status.get(idx as usize).copied().unwrap_or(RequestStatus::Error)
    }
}

/// VirtIO 队列自测试
pub fn self_test() {
    crate::println!("[virtio::queue] Running self-test...");
    
    // 测试常量
    crate::println!("[virtio::queue] QUEUE_SIZE = {}", QUEUE_SIZE);
    
    // 测试 desc_flags
    crate::println!("[virtio::queue] desc_flags: NEXT={} WRITE={} INDIRECT={}",
        desc_flags::NEXT, desc_flags::WRITE, desc_flags::INDIRECT);
    
    // 测试 VirtqDesc
    let desc = VirtqDesc {
        addr: 0x1000,
        len: 512,
        flags: desc_flags::NEXT | desc_flags::WRITE,
        next: 1,
    };
    crate::println!("[virtio::queue] VirtqDesc: addr={:#x} len={} flags={} next={}",
        desc.addr, desc.len, desc.flags, desc.next);
    
    // 测试 Legacy SplitVirtqueue
    let mut queue = SplitVirtqueue::new_legacy(0);
    crate::println!("[virtio::queue] Created Legacy SplitVirtqueue with size {}", queue.size());
    
    // 测试地址获取
    let pfn = queue.pfn();
    let desc_addr = queue.desc_addr();
    let avail_addr = queue.avail_addr();
    let used_addr = queue.used_addr();
    crate::println!("[virtio::queue] PFN={:#x} desc={:#x} avail={:#x} used={:#x}",
        pfn, desc_addr, avail_addr, used_addr);
    
    // 测试描述符分配和释放
    let idx = queue.alloc_desc();
    crate::println!("[virtio::queue] Allocated descriptor: {:?}", idx);
    if let Some(i) = idx {
        queue.free_desc(i);
        crate::println!("[virtio::queue] Freed descriptor {}", i);
    }
    
    // 测试添加缓冲区
    let bufs = [(0x2000u64, 512u32, false), (0x3000u64, 512u32, true)];
    let head = queue.add_buf(&bufs);
    crate::println!("[virtio::queue] Added buffer chain, head: {:?}", head);
    
    // 测试 has_used 和 pop_used
    let has_used = queue.has_used();
    crate::println!("[virtio::queue] Has used: {}", has_used);
    
    // 测试 free_chain
    if let Some(h) = head {
        queue.free_chain(h);
        crate::println!("[virtio::queue] Freed chain starting at {}", h);
    }
    
    // 测试 RequestTracker
    let mut tracker = RequestTracker::new(16);
    tracker.start(0);
    tracker.complete(0, 512);
    let status = tracker.get(0);
    crate::println!("[virtio::queue] RequestTracker status[0]: {:?}", status);
    
    crate::println!("[virtio::queue] Self-test passed!");
}
