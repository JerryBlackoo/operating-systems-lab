//! eBPF Maps
//!
//! 实现 eBPF 程序使用的数据结构

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use alloc::vec;
use core::any::Any;

/// Map 类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BpfMapType {
    /// 未指定
    Unspec,
    /// 哈希表
    Hash,
    /// 数组
    Array,
    /// 程序数组
    ProgArray,
    /// Perf 事件数组
    PerfEventArray,
    /// Per-CPU 哈希表
    PercpuHash,
    /// Per-CPU 数组
    PercpuArray,
    /// 栈追踪
    StackTrace,
    /// LRU 哈希表
    LruHash,
}

/// Map 操作 trait
pub trait BpfMap: Send + Sync {
    /// 获取 Map 类型
    fn map_type(&self) -> BpfMapType;
    
    /// 查找元素
    fn lookup(&self, key: &[u8]) -> Option<&[u8]>;
    
    /// 更新元素
    fn update(&mut self, key: &[u8], value: &[u8], flags: u64) -> Result<(), MapError>;
    
    /// 删除元素
    fn delete(&mut self, key: &[u8]) -> Result<(), MapError>;
    
    /// 获取下一个 key
    fn get_next_key(&self, key: Option<&[u8]>) -> Option<Vec<u8>>;
    
    /// 转换为 Any（用于向下转型）
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Map 错误
#[derive(Debug, Clone, Copy)]
pub enum MapError {
    /// Key 不存在
    KeyNotFound,
    /// Key 已存在
    KeyExists,
    /// Map 已满
    MapFull,
    /// 无效参数
    InvalidArg,
}

/// 哈希 Map
pub struct BpfHashMap {
    /// 数据存储
    data: BTreeMap<Vec<u8>, Vec<u8>>,
    /// Key 大小
    key_size: u32,
    /// Value 大小
    value_size: u32,
    /// 最大条目数
    max_entries: u32,
}

impl BpfHashMap {
    pub fn new(key_size: u32, value_size: u32, max_entries: u32) -> Self {
        Self {
            data: BTreeMap::new(),
            key_size,
            value_size,
            max_entries,
        }
    }
}

impl BpfMap for BpfHashMap {
    fn map_type(&self) -> BpfMapType {
        BpfMapType::Hash
    }
    
    fn lookup(&self, key: &[u8]) -> Option<&[u8]> {
        self.data.get(key).map(|v| v.as_slice())
    }
    
    fn update(&mut self, key: &[u8], value: &[u8], flags: u64) -> Result<(), MapError> {
        if key.len() != self.key_size as usize {
            return Err(MapError::InvalidArg);
        }
        if value.len() != self.value_size as usize {
            return Err(MapError::InvalidArg);
        }
        
        let exists = self.data.contains_key(key);
        
        // BPF_NOEXIST = 1, BPF_EXIST = 2
        if flags == 1 && exists {
            return Err(MapError::KeyExists);
        }
        if flags == 2 && !exists {
            return Err(MapError::KeyNotFound);
        }
        
        if !exists && self.data.len() >= self.max_entries as usize {
            return Err(MapError::MapFull);
        }
        
        self.data.insert(key.to_vec(), value.to_vec());
        Ok(())
    }
    
    fn delete(&mut self, key: &[u8]) -> Result<(), MapError> {
        self.data.remove(key)
            .map(|_| ())
            .ok_or(MapError::KeyNotFound)
    }
    
    fn get_next_key(&self, key: Option<&[u8]>) -> Option<Vec<u8>> {
        match key {
            None => self.data.keys().next().cloned(),
            Some(k) => {
                let mut iter = self.data.range(k.to_vec()..);
                iter.next(); // 跳过当前 key
                iter.next().map(|(k, _)| k.clone())
            }
        }
    }
    
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// 数组 Map
pub struct BpfArray {
    /// 数据存储
    data: Vec<Vec<u8>>,
    /// Value 大小
    value_size: u32,
    /// 最大条目数
    max_entries: u32,
}

impl BpfArray {
    pub fn new(value_size: u32, max_entries: u32) -> Self {
        let mut data = Vec::with_capacity(max_entries as usize);
        for _ in 0..max_entries {
            data.push(vec![0u8; value_size as usize]);
        }
        Self {
            data,
            value_size,
            max_entries,
        }
    }
    
    fn key_to_index(&self, key: &[u8]) -> Option<usize> {
        if key.len() != 4 {
            return None;
        }
        let index = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]) as usize;
        if index < self.max_entries as usize {
            Some(index)
        } else {
            None
        }
    }
}

impl BpfMap for BpfArray {
    fn map_type(&self) -> BpfMapType {
        BpfMapType::Array
    }
    
    fn lookup(&self, key: &[u8]) -> Option<&[u8]> {
        let index = self.key_to_index(key)?;
        Some(&self.data[index])
    }
    
    fn update(&mut self, key: &[u8], value: &[u8], _flags: u64) -> Result<(), MapError> {
        let index = self.key_to_index(key).ok_or(MapError::InvalidArg)?;
        if value.len() != self.value_size as usize {
            return Err(MapError::InvalidArg);
        }
        self.data[index].copy_from_slice(value);
        Ok(())
    }
    
    fn delete(&mut self, key: &[u8]) -> Result<(), MapError> {
        let index = self.key_to_index(key).ok_or(MapError::InvalidArg)?;
        self.data[index].fill(0);
        Ok(())
    }
    
    fn get_next_key(&self, key: Option<&[u8]>) -> Option<Vec<u8>> {
        let next_index = match key {
            None => 0,
            Some(k) => {
                let index = self.key_to_index(k)?;
                index + 1
            }
        };
        
        if next_index < self.max_entries as usize {
            Some((next_index as u32).to_ne_bytes().to_vec())
        } else {
            None
        }
    }
    
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}

/// Per-CPU 数组
pub struct BpfPercpuArray {
    /// 每个 CPU 的数据
    data: Vec<Vec<Vec<u8>>>,
    /// Value 大小
    value_size: u32,
    /// 最大条目数
    max_entries: u32,
    /// CPU 数量
    num_cpus: usize,
}

impl BpfPercpuArray {
    pub fn new(value_size: u32, max_entries: u32, num_cpus: usize) -> Self {
        let mut data = Vec::with_capacity(num_cpus);
        for _ in 0..num_cpus {
            let mut cpu_data = Vec::with_capacity(max_entries as usize);
            for _ in 0..max_entries {
                cpu_data.push(vec![0u8; value_size as usize]);
            }
            data.push(cpu_data);
        }
        Self {
            data,
            value_size,
            max_entries,
            num_cpus,
        }
    }
    
    /// 获取当前 CPU 的数据
    pub fn lookup_percpu(&self, key: &[u8], cpu: usize) -> Option<&[u8]> {
        if key.len() != 4 || cpu >= self.num_cpus {
            return None;
        }
        let index = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]) as usize;
        if index < self.max_entries as usize {
            Some(&self.data[cpu][index])
        } else {
            None
        }
    }
    
    /// 更新当前 CPU 的数据
    pub fn update_percpu(&mut self, key: &[u8], value: &[u8], cpu: usize) -> Result<(), MapError> {
        if key.len() != 4 || cpu >= self.num_cpus {
            return Err(MapError::InvalidArg);
        }
        let index = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]) as usize;
        if index >= self.max_entries as usize {
            return Err(MapError::InvalidArg);
        }
        if value.len() != self.value_size as usize {
            return Err(MapError::InvalidArg);
        }
        self.data[cpu][index].copy_from_slice(value);
        Ok(())
    }
}

impl BpfMap for BpfPercpuArray {
    fn map_type(&self) -> BpfMapType {
        BpfMapType::PercpuArray
    }
    
    fn lookup(&self, key: &[u8]) -> Option<&[u8]> {
        // 返回 CPU 0 的数据
        self.lookup_percpu(key, 0)
    }
    
    fn update(&mut self, key: &[u8], value: &[u8], _flags: u64) -> Result<(), MapError> {
        // 更新 CPU 0 的数据
        self.update_percpu(key, value, 0)
    }
    
    fn delete(&mut self, key: &[u8]) -> Result<(), MapError> {
        if key.len() != 4 {
            return Err(MapError::InvalidArg);
        }
        let index = u32::from_ne_bytes([key[0], key[1], key[2], key[3]]) as usize;
        if index >= self.max_entries as usize {
            return Err(MapError::InvalidArg);
        }
        for cpu in 0..self.num_cpus {
            self.data[cpu][index].fill(0);
        }
        Ok(())
    }
    
    fn get_next_key(&self, key: Option<&[u8]>) -> Option<Vec<u8>> {
        let next_index = match key {
            None => 0,
            Some(k) => {
                if k.len() != 4 {
                    return None;
                }
                let index = u32::from_ne_bytes([k[0], k[1], k[2], k[3]]) as usize;
                index + 1
            }
        };
        
        if next_index < self.max_entries as usize {
            Some((next_index as u32).to_ne_bytes().to_vec())
        } else {
            None
        }
    }
    
    fn as_any(&self) -> &dyn Any { self }
    fn as_any_mut(&mut self) -> &mut dyn Any { self }
}
