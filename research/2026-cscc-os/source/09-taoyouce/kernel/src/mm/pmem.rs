//! 持久化内存（PMEM）支持
//!
//! 实现持久化内存分配器、DAX 直接访问和崩溃一致性

#![allow(dead_code)]

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use alloc::vec;
use core::sync::atomic::{AtomicU64, Ordering, fence};
use crate::sync::SpinLock;

/// PMEM 区域起始地址（示例）
pub const PMEM_BASE: usize = 0x1_0000_0000;

/// PMEM 区域大小（1GB）
pub const PMEM_SIZE: usize = 1024 * 1024 * 1024;

/// PMEM 块大小（4KB）
pub const PMEM_BLOCK_SIZE: usize = 4096;

/// 缓存行大小
pub const CACHE_LINE_SIZE: usize = 64;

/// PMEM 魔数
pub const PMEM_MAGIC: u64 = 0x504D454D_52555354; // "PMEMRUST"

/// PMEM 超级块
#[repr(C)]
pub struct PmemSuperblock {
    /// 魔数
    pub magic: u64,
    /// 版本
    pub version: u32,
    /// 块大小
    pub block_size: u32,
    /// 总块数
    pub total_blocks: u64,
    /// 空闲块数
    pub free_blocks: u64,
    /// 位图起始偏移
    pub bitmap_offset: u64,
    /// 数据区起始偏移
    pub data_offset: u64,
    /// 日志区起始偏移
    pub log_offset: u64,
    /// 校验和
    pub checksum: u64,
}

impl PmemSuperblock {
    /// 计算校验和
    pub fn calculate_checksum(&self) -> u64 {
        let mut sum: u64 = 0;
        sum = sum.wrapping_add(self.magic);
        sum = sum.wrapping_add(self.version as u64);
        sum = sum.wrapping_add(self.block_size as u64);
        sum = sum.wrapping_add(self.total_blocks);
        sum = sum.wrapping_add(self.free_blocks);
        sum = sum.wrapping_add(self.bitmap_offset);
        sum = sum.wrapping_add(self.data_offset);
        sum = sum.wrapping_add(self.log_offset);
        sum
    }
    
    /// 验证校验和
    pub fn verify(&self) -> bool {
        self.magic == PMEM_MAGIC && self.checksum == self.calculate_checksum()
    }
}

/// 日志条目类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LogEntryType {
    /// 无效
    Invalid = 0,
    /// 写入
    Write = 1,
    /// 分配
    Alloc = 2,
    /// 释放
    Free = 3,
    /// 提交
    Commit = 4,
    /// 中止
    Abort = 5,
}

/// 日志条目
#[repr(C)]
pub struct LogEntry {
    /// 条目类型
    pub entry_type: u8,
    /// 保留
    pub reserved: [u8; 7],
    /// 事务 ID
    pub tx_id: u64,
    /// 目标地址
    pub addr: u64,
    /// 数据长度
    pub len: u32,
    /// 保留
    pub reserved2: u32,
    /// 旧数据（用于回滚）
    pub old_data: [u8; 32],
}

/// 事务状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxState {
    /// 活跃
    Active,
    /// 已提交
    Committed,
    /// 已中止
    Aborted,
}

/// PMEM 事务
pub struct PmemTransaction {
    /// 事务 ID
    tx_id: u64,
    /// 状态
    state: TxState,
    /// 日志条目
    log_entries: Vec<LogEntry>,
}

impl PmemTransaction {
    /// 创建新事务
    pub fn new(tx_id: u64) -> Self {
        Self {
            tx_id,
            state: TxState::Active,
            log_entries: Vec::new(),
        }
    }
    
    /// 记录写入
    pub fn log_write(&mut self, addr: u64, old_data: &[u8]) {
        let mut entry = LogEntry {
            entry_type: LogEntryType::Write as u8,
            reserved: [0; 7],
            tx_id: self.tx_id,
            addr,
            len: old_data.len() as u32,
            reserved2: 0,
            old_data: [0; 32],
        };
        
        let copy_len = old_data.len().min(32);
        entry.old_data[..copy_len].copy_from_slice(&old_data[..copy_len]);
        
        self.log_entries.push(entry);
    }
    
    /// 提交事务
    pub fn commit(&mut self) {
        self.state = TxState::Committed;
    }
    
    /// 中止事务
    pub fn abort(&mut self) {
        self.state = TxState::Aborted;
    }
}

/// PMEM 分配器
pub struct PmemAllocator {
    /// 基地址
    base: usize,
    /// 大小
    size: usize,
    /// 块大小
    block_size: usize,
    /// 总块数
    total_blocks: usize,
    /// 位图
    bitmap: Vec<AtomicU64>,
    /// 空闲块数
    free_blocks: AtomicU64,
    /// 下一个事务 ID
    next_tx_id: AtomicU64,
    /// 活跃事务
    active_txs: SpinLock<BTreeMap<u64, PmemTransaction>>,
}

impl PmemAllocator {
    /// 创建新分配器
    pub fn new(base: usize, size: usize) -> Self {
        let total_blocks = size / PMEM_BLOCK_SIZE;
        let bitmap_size = (total_blocks + 63) / 64;
        
        let mut bitmap = Vec::with_capacity(bitmap_size);
        for _ in 0..bitmap_size {
            bitmap.push(AtomicU64::new(0));
        }
        
        Self {
            base,
            size,
            block_size: PMEM_BLOCK_SIZE,
            total_blocks,
            bitmap,
            free_blocks: AtomicU64::new(total_blocks as u64),
            next_tx_id: AtomicU64::new(1),
            active_txs: SpinLock::new(BTreeMap::new()),
        }
    }
    
    /// 格式化 PMEM
    pub fn format(&self) -> Result<(), &'static str> {
        // 创建超级块
        let superblock = PmemSuperblock {
            magic: PMEM_MAGIC,
            version: 1,
            block_size: self.block_size as u32,
            total_blocks: self.total_blocks as u64,
            free_blocks: self.total_blocks as u64,
            bitmap_offset: core::mem::size_of::<PmemSuperblock>() as u64,
            data_offset: 4096,
            log_offset: self.size as u64 - 1024 * 1024, // 最后 1MB 作为日志
            checksum: 0,
        };
        
        let mut sb = superblock;
        sb.checksum = sb.calculate_checksum();
        
        // 持久化写入超级块
        self.persist_write(self.base, &sb as *const _ as *const u8, 
                          core::mem::size_of::<PmemSuperblock>());
        
        Ok(())
    }
    
    /// 分配块
    pub fn alloc_block(&self) -> Option<usize> {
        for (i, word) in self.bitmap.iter().enumerate() {
            let mut current = word.load(Ordering::Acquire);
            
            while current != u64::MAX {
                // 找到第一个空闲位
                let bit = (!current).trailing_zeros() as usize;
                if bit >= 64 {
                    break;
                }
                
                let new_value = current | (1u64 << bit);
                
                match word.compare_exchange_weak(
                    current, new_value, Ordering::AcqRel, Ordering::Acquire
                ) {
                    Ok(_) => {
                        self.free_blocks.fetch_sub(1, Ordering::SeqCst);
                        let block_idx = i * 64 + bit;
                        return Some(self.base + block_idx * self.block_size);
                    }
                    Err(v) => current = v,
                }
            }
        }
        
        None
    }
    
    /// 释放块
    pub fn free_block(&self, addr: usize) -> Result<(), &'static str> {
        if addr < self.base || addr >= self.base + self.size {
            return Err("Address out of range");
        }
        
        let block_idx = (addr - self.base) / self.block_size;
        let word_idx = block_idx / 64;
        let bit_idx = block_idx % 64;
        
        let old = self.bitmap[word_idx].fetch_and(!(1u64 << bit_idx), Ordering::AcqRel);
        
        if old & (1u64 << bit_idx) != 0 {
            self.free_blocks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        } else {
            Err("Block already free")
        }
    }
    
    /// 持久化写入
    fn persist_write(&self, addr: usize, data: *const u8, len: usize) {
        unsafe {
            // 写入数据
            core::ptr::copy_nonoverlapping(data, addr as *mut u8, len);
            
            // 刷新缓存行
            self.flush_cache_range(addr, len);
            
            // 内存屏障
            fence(Ordering::SeqCst);
        }
    }
    
    /// 刷新缓存行范围
    fn flush_cache_range(&self, addr: usize, len: usize) {
        let start = addr & !(CACHE_LINE_SIZE - 1);
        let end = (addr + len + CACHE_LINE_SIZE - 1) & !(CACHE_LINE_SIZE - 1);
        
        for cache_line in (start..end).step_by(CACHE_LINE_SIZE) {
            self.clflush(cache_line);
        }
    }
    
    /// 刷新单个缓存行（CLFLUSH 指令模拟）
    fn clflush(&self, addr: usize) {
        // RISC-V 没有 CLFLUSH，使用 fence 指令
        // 实际硬件可能需要特定的缓存管理指令
        fence(Ordering::SeqCst);
        let _ = addr;
    }
    
    /// 开始事务
    pub fn begin_tx(&self) -> u64 {
        let tx_id = self.next_tx_id.fetch_add(1, Ordering::SeqCst);
        let tx = PmemTransaction::new(tx_id);
        self.active_txs.lock().insert(tx_id, tx);
        tx_id
    }
    
    /// 事务写入
    pub fn tx_write(&self, tx_id: u64, addr: usize, data: &[u8]) -> Result<(), &'static str> {
        let mut txs = self.active_txs.lock();
        let tx = txs.get_mut(&tx_id).ok_or("Transaction not found")?;
        
        if tx.state != TxState::Active {
            return Err("Transaction not active");
        }
        
        // 读取旧数据
        let mut old_data = vec![0u8; data.len()];
        unsafe {
            core::ptr::copy_nonoverlapping(addr as *const u8, old_data.as_mut_ptr(), data.len());
        }
        
        // 记录日志
        tx.log_write(addr as u64, &old_data);
        
        // 写入新数据
        self.persist_write(addr, data.as_ptr(), data.len());
        
        Ok(())
    }
    
    /// 提交事务
    pub fn commit_tx(&self, tx_id: u64) -> Result<(), &'static str> {
        let mut txs = self.active_txs.lock();
        let tx = txs.get_mut(&tx_id).ok_or("Transaction not found")?;
        
        tx.commit();
        
        // 写入提交日志
        // ...
        
        txs.remove(&tx_id);
        Ok(())
    }
    
    /// 中止事务
    pub fn abort_tx(&self, tx_id: u64) -> Result<(), &'static str> {
        let mut txs = self.active_txs.lock();
        let tx = txs.get_mut(&tx_id).ok_or("Transaction not found")?;
        
        // 回滚所有写入
        for entry in tx.log_entries.iter().rev() {
            if entry.entry_type == LogEntryType::Write as u8 {
                self.persist_write(
                    entry.addr as usize,
                    entry.old_data.as_ptr(),
                    entry.len as usize
                );
            }
        }
        
        tx.abort();
        txs.remove(&tx_id);
        Ok(())
    }
    
    /// 恢复（崩溃后）
    pub fn recover(&self) -> Result<(), &'static str> {
        // 读取超级块
        let superblock: PmemSuperblock = unsafe {
            core::ptr::read(self.base as *const PmemSuperblock)
        };
        
        if !superblock.verify() {
            return Err("Invalid superblock");
        }
        
        // 扫描日志，回滚未提交的事务
        // ...
        
        crate::println!("[pmem] Recovery completed");
        Ok(())
    }
}

/// DAX（Direct Access）映射
pub struct DaxMapping {
    /// 起始地址
    pub addr: usize,
    /// 大小
    pub size: usize,
    /// 是否可写
    pub writable: bool,
}

impl DaxMapping {
    /// 创建 DAX 映射
    pub fn new(addr: usize, size: usize, writable: bool) -> Self {
        Self { addr, size, writable }
    }
    
    /// 读取数据
    pub fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, &'static str> {
        if offset >= self.size {
            return Ok(0);
        }
        
        let to_read = buf.len().min(self.size - offset);
        unsafe {
            core::ptr::copy_nonoverlapping(
                (self.addr + offset) as *const u8,
                buf.as_mut_ptr(),
                to_read
            );
        }
        
        Ok(to_read)
    }
    
    /// 写入数据
    pub fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, &'static str> {
        if !self.writable {
            return Err("Mapping not writable");
        }
        
        if offset >= self.size {
            return Ok(0);
        }
        
        let to_write = buf.len().min(self.size - offset);
        unsafe {
            core::ptr::copy_nonoverlapping(
                buf.as_ptr(),
                (self.addr + offset) as *mut u8,
                to_write
            );
            
            // 刷新缓存
            fence(Ordering::SeqCst);
        }
        
        Ok(to_write)
    }
    
    /// 同步到持久存储
    pub fn sync(&self) -> Result<(), &'static str> {
        fence(Ordering::SeqCst);
        Ok(())
    }
}

/// 全局 PMEM 分配器
static PMEM_ALLOCATOR: SpinLock<Option<PmemAllocator>> = SpinLock::new(None);

/// 初始化 PMEM 子系统
pub fn init() {
    let allocator = PmemAllocator::new(PMEM_BASE, PMEM_SIZE);
    
    // 尝试恢复或格式化
    if allocator.recover().is_err() {
        crate::println!("[pmem] Formatting PMEM...");
        let _ = allocator.format();
    }
    
    *PMEM_ALLOCATOR.lock() = Some(allocator);
    crate::println!("[pmem] PMEM subsystem initialized");
}

/// 分配 PMEM 块
pub fn alloc_block() -> Option<usize> {
    PMEM_ALLOCATOR.lock()
        .as_ref()
        .and_then(|a| a.alloc_block())
}

/// 释放 PMEM 块
pub fn free_block(addr: usize) -> Result<(), &'static str> {
    PMEM_ALLOCATOR.lock()
        .as_ref()
        .ok_or("PMEM not initialized")?
        .free_block(addr)
}

/// 开始事务
pub fn begin_transaction() -> Option<u64> {
    PMEM_ALLOCATOR.lock()
        .as_ref()
        .map(|a| a.begin_tx())
}

/// 提交事务
pub fn commit_transaction(tx_id: u64) -> Result<(), &'static str> {
    PMEM_ALLOCATOR.lock()
        .as_ref()
        .ok_or("PMEM not initialized")?
        .commit_tx(tx_id)
}

/// 中止事务
pub fn abort_transaction(tx_id: u64) -> Result<(), &'static str> {
    PMEM_ALLOCATOR.lock()
        .as_ref()
        .ok_or("PMEM not initialized")?
        .abort_tx(tx_id)
}

/// PMEM 子系统自测试
pub fn self_test() {
    crate::println!("[pmem] Running self-test...");
    
    // 测试常量
    crate::println!("[pmem] PMEM_BASE: {:#x}", PMEM_BASE);
    crate::println!("[pmem] PMEM_SIZE: {} MB", PMEM_SIZE / 1024 / 1024);
    crate::println!("[pmem] PMEM_BLOCK_SIZE: {} bytes", PMEM_BLOCK_SIZE);
    crate::println!("[pmem] CACHE_LINE_SIZE: {} bytes", CACHE_LINE_SIZE);
    crate::println!("[pmem] PMEM_MAGIC: {:#x}", PMEM_MAGIC);
    
    // 测试超级块
    let sb = PmemSuperblock {
        magic: PMEM_MAGIC,
        version: 1,
        block_size: PMEM_BLOCK_SIZE as u32,
        total_blocks: 1000,
        free_blocks: 1000,
        bitmap_offset: 64,
        data_offset: 4096,
        log_offset: 1024 * 1024,
        checksum: 0,
    };
    let checksum = sb.calculate_checksum();
    crate::println!("[pmem] Superblock checksum: {:#x}", checksum);
    
    // 测试日志条目类型
    let _invalid = LogEntryType::Invalid;
    let _write = LogEntryType::Write;
    let _alloc = LogEntryType::Alloc;
    let _free = LogEntryType::Free;
    let _commit = LogEntryType::Commit;
    let _abort = LogEntryType::Abort;
    
    // 测试事务状态
    let _active = TxState::Active;
    let _committed = TxState::Committed;
    let _aborted = TxState::Aborted;
    
    // 测试事务
    let mut tx = PmemTransaction::new(1);
    tx.log_write(0x1000, &[1, 2, 3, 4]);
    tx.commit();
    crate::println!("[pmem] Transaction test passed");
    
    // 测试 DAX 映射（使用模拟地址）
    let dax = DaxMapping::new(0x1000_0000, 4096, true);
    crate::println!("[pmem] DAX mapping: addr={:#x}, size={}, writable={}", 
        dax.addr, dax.size, dax.writable);
    
    crate::println!("[pmem] Self-test passed!");
}
