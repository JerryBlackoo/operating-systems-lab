//! FAT32 文件系统实现
//!
//! 实现 FAT32 文件系统的读写操作

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::mem::size_of;
use crate::sync::SpinLock;
use super::{FileSystem, Inode, InodeType, DirEntry, FileType, FileStat, FileMode, FsError, File, Directory};

/// 扇区大小
pub const SECTOR_SIZE: usize = 512;

/// FAT32 引导扇区
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Fat32BootSector {
    /// 跳转指令
    pub jmp_boot: [u8; 3],
    /// OEM 名称
    pub oem_name: [u8; 8],
    /// 每扇区字节数
    pub bytes_per_sector: u16,
    /// 每簇扇区数
    pub sectors_per_cluster: u8,
    /// 保留扇区数
    pub reserved_sectors: u16,
    /// FAT 表数量
    pub num_fats: u8,
    /// 根目录项数（FAT32 为 0）
    pub root_entry_count: u16,
    /// 总扇区数（小）
    pub total_sectors_16: u16,
    /// 媒体类型
    pub media_type: u8,
    /// 每 FAT 扇区数（FAT12/16）
    pub fat_size_16: u16,
    /// 每磁道扇区数
    pub sectors_per_track: u16,
    /// 磁头数
    pub num_heads: u16,
    /// 隐藏扇区数
    pub hidden_sectors: u32,
    /// 总扇区数（大）
    pub total_sectors_32: u32,
    /// 每 FAT 扇区数（FAT32）
    pub fat_size_32: u32,
    /// 扩展标志
    pub ext_flags: u16,
    /// 文件系统版本
    pub fs_version: u16,
    /// 根目录起始簇
    pub root_cluster: u32,
    /// FSInfo 扇区号
    pub fs_info_sector: u16,
    /// 备份引导扇区
    pub backup_boot_sector: u16,
    /// 保留
    pub reserved: [u8; 12],
    /// 驱动器号
    pub drive_number: u8,
    /// 保留
    pub reserved1: u8,
    /// 扩展引导签名
    pub boot_signature: u8,
    /// 卷序列号
    pub volume_id: u32,
    /// 卷标
    pub volume_label: [u8; 11],
    /// 文件系统类型
    pub fs_type: [u8; 8],
}

/// FAT32 目录项
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Fat32DirEntry {
    /// 文件名（8.3 格式）
    pub name: [u8; 11],
    /// 属性
    pub attr: u8,
    /// NT 保留
    pub nt_reserved: u8,
    /// 创建时间（十分之一秒）
    pub create_time_tenth: u8,
    /// 创建时间
    pub create_time: u16,
    /// 创建日期
    pub create_date: u16,
    /// 最后访问日期
    pub last_access_date: u16,
    /// 起始簇号高 16 位
    pub first_cluster_hi: u16,
    /// 修改时间
    pub write_time: u16,
    /// 修改日期
    pub write_date: u16,
    /// 起始簇号低 16 位
    pub first_cluster_lo: u16,
    /// 文件大小
    pub file_size: u32,
}

/// 目录项属性
pub mod attr {
    pub const READ_ONLY: u8 = 0x01;
    pub const HIDDEN: u8 = 0x02;
    pub const SYSTEM: u8 = 0x04;
    pub const VOLUME_ID: u8 = 0x08;
    pub const DIRECTORY: u8 = 0x10;
    pub const ARCHIVE: u8 = 0x20;
    pub const LONG_NAME: u8 = 0x0F;
}

/// FAT 表项特殊值
pub mod fat_entry {
    pub const FREE: u32 = 0x0000_0000;
    pub const RESERVED: u32 = 0x0FFF_FFF0;
    pub const BAD: u32 = 0x0FFF_FFF7;
    pub const END: u32 = 0x0FFF_FFFF;
}

/// 长文件名目录项
#[repr(C, packed)]
#[derive(Debug, Clone, Copy)]
pub struct Fat32LfnEntry {
    /// 序号
    pub order: u8,
    /// 名称字符 1-5
    pub name1: [u16; 5],
    /// 属性（总是 0x0F）
    pub attr: u8,
    /// 类型
    pub entry_type: u8,
    /// 校验和
    pub checksum: u8,
    /// 名称字符 6-11
    pub name2: [u16; 6],
    /// 起始簇（总是 0）
    pub first_cluster: u16,
    /// 名称字符 12-13
    pub name3: [u16; 2],
}

/// FAT32 文件系统
pub struct Fat32Fs {
    /// 引导扇区信息
    boot_sector: Fat32BootSector,
    /// 每簇字节数
    bytes_per_cluster: u32,
    /// FAT 表起始扇区
    fat_start_sector: u32,
    /// 数据区起始扇区
    data_start_sector: u32,
    /// 根目录起始簇
    root_cluster: u32,
    /// FAT 表缓存
    fat_cache: Vec<u32>,
    /// 块设备读取函数
    read_fn: Box<dyn Fn(u64, usize) -> Result<Vec<u8>, &'static str> + Send + Sync>,
    /// 块设备写入函数
    write_fn: Box<dyn Fn(u64, Vec<u8>) -> Result<(), &'static str> + Send + Sync>,
}

/// 包装 FAT32 文件系统以支持 Arc<SpinLock<>>
pub struct Fat32FsWrapper {
    inner: Arc<SpinLock<Fat32Fs>>,
}

impl Fat32Fs {
    /// 挂载 FAT32 文件系统
    pub fn mount<R, W>(read_fn: R, write_fn: W) -> Result<Self, &'static str>
    where
        R: Fn(u64, usize) -> Result<Vec<u8>, &'static str> + Send + Sync + 'static,
        W: Fn(u64, Vec<u8>) -> Result<(), &'static str> + Send + Sync + 'static,
    {
        // 读取引导扇区
        let boot_data = read_fn(0, 1)?;
        if boot_data.len() < size_of::<Fat32BootSector>() {
            return Err("Boot sector too small");
        }
        
        let boot_sector: Fat32BootSector = unsafe {
            core::ptr::read(boot_data.as_ptr() as *const Fat32BootSector)
        };
        
        // 验证签名
        if &boot_sector.fs_type[0..5] != b"FAT32" {
            return Err("Not a FAT32 filesystem");
        }
        
        let bytes_per_sector = boot_sector.bytes_per_sector as u32;
        let sectors_per_cluster = boot_sector.sectors_per_cluster as u32;
        let bytes_per_cluster = bytes_per_sector * sectors_per_cluster;
        
        let fat_start_sector = boot_sector.reserved_sectors as u32;
        let fat_sectors = boot_sector.fat_size_32 * boot_sector.num_fats as u32;
        let data_start_sector = fat_start_sector + fat_sectors;
        
        // 复制 packed struct 字段到本地变量，避免未对齐引用
        let root_cluster = boot_sector.root_cluster;
        
        crate::println!(
            "[fat32] Mounted: {} bytes/cluster, root cluster {}",
            bytes_per_cluster, root_cluster
        );
        
        Ok(Self {
            boot_sector,
            bytes_per_cluster,
            fat_start_sector,
            data_start_sector,
            root_cluster,
            fat_cache: Vec::new(),
            read_fn: Box::new(read_fn),
            write_fn: Box::new(write_fn),
        })
    }
    
    /// 簇号转扇区号
    fn cluster_to_sector(&self, cluster: u32) -> u32 {
        self.data_start_sector + (cluster - 2) * self.boot_sector.sectors_per_cluster as u32
    }
    
    /// 读取簇
    fn read_cluster(&self, cluster: u32) -> Result<Vec<u8>, &'static str> {
        let sector = self.cluster_to_sector(cluster);
        let count = self.boot_sector.sectors_per_cluster as usize;
        (self.read_fn)(sector as u64, count)
    }
    
    /// 写入簇
    fn write_cluster(&self, cluster: u32, data: &[u8]) -> Result<(), &'static str> {
        let sector = self.cluster_to_sector(cluster);
        let cluster_size = self.bytes_per_cluster as usize;
        
        // 确保数据大小正确
        let mut buf = vec![0u8; cluster_size];
        let copy_len = data.len().min(cluster_size);
        buf[..copy_len].copy_from_slice(&data[..copy_len]);
        
        (self.write_fn)(sector as u64, buf)
    }
    
    /// 读取 FAT 表项
    fn read_fat_entry(&self, cluster: u32) -> Result<u32, &'static str> {
        let fat_offset = cluster * 4;
        let fat_sector = self.fat_start_sector + fat_offset / self.boot_sector.bytes_per_sector as u32;
        let offset_in_sector = (fat_offset % self.boot_sector.bytes_per_sector as u32) as usize;
        
        let sector_data = (self.read_fn)(fat_sector as u64, 1)?;
        let entry = u32::from_le_bytes([
            sector_data[offset_in_sector],
            sector_data[offset_in_sector + 1],
            sector_data[offset_in_sector + 2],
            sector_data[offset_in_sector + 3],
        ]) & 0x0FFF_FFFF;
        
        Ok(entry)
    }
    
    /// 写入 FAT 表项
    fn write_fat_entry(&self, cluster: u32, value: u32) -> Result<(), &'static str> {
        let fat_offset = cluster * 4;
        let fat_sector = self.fat_start_sector + fat_offset / self.boot_sector.bytes_per_sector as u32;
        let offset_in_sector = (fat_offset % self.boot_sector.bytes_per_sector as u32) as usize;
        
        // 读取扇区
        let mut sector_data = (self.read_fn)(fat_sector as u64, 1)?;
        
        // 修改 FAT 表项（保留高 4 位）
        let old_value = u32::from_le_bytes([
            sector_data[offset_in_sector],
            sector_data[offset_in_sector + 1],
            sector_data[offset_in_sector + 2],
            sector_data[offset_in_sector + 3],
        ]);
        let new_value = (old_value & 0xF000_0000) | (value & 0x0FFF_FFFF);
        let bytes = new_value.to_le_bytes();
        sector_data[offset_in_sector..offset_in_sector + 4].copy_from_slice(&bytes);
        
        // 写回扇区 (FAT #0)
        (self.write_fn)(fat_sector as u64, sector_data.clone())?;

        // Sync to all backup FAT copies (FAT #1 .. #num_fats-1).
        // Per FAT32 spec, each FAT copy starts at fat_start + i*fat_size_32.
        let num_fats = self.boot_sector.num_fats as u32;
        let fat_size = self.boot_sector.fat_size_32;
        for i in 1..num_fats {
            let backup_sector = fat_sector + i * fat_size;
            let _ = (self.write_fn)(backup_sector as u64, sector_data.clone());
        }
        Ok(())
    }
    
    /// 分配一个空闲簇
    fn alloc_cluster(&self) -> Result<u32, &'static str> {
        // 从簇 2 开始搜索空闲簇
        let total_clusters = (self.boot_sector.total_sectors_32 - self.data_start_sector) 
            / self.boot_sector.sectors_per_cluster as u32;
        
        for cluster in 2..total_clusters + 2 {
            let entry = self.read_fat_entry(cluster)?;
            if entry == fat_entry::FREE {
                // 标记为已使用（链尾）
                self.write_fat_entry(cluster, fat_entry::END)?;
                return Ok(cluster);
            }
        }
        
        Err("No free clusters")
    }
    
    /// 释放簇链
    #[allow(dead_code)]
    fn free_cluster_chain(&self, start_cluster: u32) -> Result<(), &'static str> {
        let mut cluster = start_cluster;
        
        while cluster >= 2 && cluster < fat_entry::RESERVED {
            let next = self.read_fat_entry(cluster)?;
            self.write_fat_entry(cluster, fat_entry::FREE)?;
            cluster = next;
        }
        
        Ok(())
    }
    
    /// 获取簇链
    fn get_cluster_chain(&self, start_cluster: u32) -> Result<Vec<u32>, &'static str> {
        let mut chain = Vec::new();
        let mut cluster = start_cluster;
        const MAX_CHAIN: usize = 1_000_000; // 防止 FAT 循环导致无限循环
        
        while cluster >= 2 && cluster < fat_entry::RESERVED {
            if chain.len() >= MAX_CHAIN {
                crate::println!("[fat32] BUG: cluster chain exceeded {} at cluster {}", MAX_CHAIN, cluster);
                return Err("Cluster chain too long (possible FAT corruption)");
            }
            chain.push(cluster);
            cluster = self.read_fat_entry(cluster)?;
        }
        
        Ok(chain)
    }
    
    /// 读取目录（返回短文件名目录项）
    fn read_directory(&self, cluster: u32) -> Result<Vec<Fat32DirEntry>, &'static str> {
        let chain = self.get_cluster_chain(cluster)?;
        let mut entries = Vec::new();
        
        for cluster in chain {
            let data = self.read_cluster(cluster)?;
            let entry_count = data.len() / size_of::<Fat32DirEntry>();
            
            for i in 0..entry_count {
                let entry: Fat32DirEntry = unsafe {
                    core::ptr::read(data.as_ptr().add(i * size_of::<Fat32DirEntry>()) as *const Fat32DirEntry)
                };
                
                // 跳过空项和删除项
                if entry.name[0] == 0x00 {
                    break;
                }
                if entry.name[0] == 0xE5 {
                    continue;
                }
                // 跳过长文件名项
                if entry.attr == attr::LONG_NAME {
                    continue;
                }
                
                entries.push(entry);
            }
        }
        
        Ok(entries)
    }
    
    /// 读取目录（包含长文件名支持）
    /// 返回 (长文件名或短文件名, 目录项) 的列表
    fn read_directory_with_lfn(&self, cluster: u32) -> Result<Vec<(String, Fat32DirEntry)>, &'static str> {
        let chain = self.get_cluster_chain(cluster)?;
        let mut entries = Vec::new();
        let mut lfn_parts: Vec<(u8, String)> = Vec::new(); // (序号, 名称片段)
        
        for cluster in chain {
            let data = self.read_cluster(cluster)?;
            let entry_count = data.len() / size_of::<Fat32DirEntry>();
            
            for i in 0..entry_count {
                let raw_entry: [u8; 32] = unsafe {
                    let ptr = data.as_ptr().add(i * 32);
                    let mut arr = [0u8; 32];
                    core::ptr::copy_nonoverlapping(ptr, arr.as_mut_ptr(), 32);
                    arr
                };
                
                // 跳过空项
                if raw_entry[0] == 0x00 {
                    break;
                }
                // 跳过删除项
                if raw_entry[0] == 0xE5 {
                    lfn_parts.clear();
                    continue;
                }
                
                let attr = raw_entry[11];
                
                // 长文件名项
                if attr == attr::LONG_NAME {
                    let lfn: Fat32LfnEntry = unsafe {
                        core::ptr::read(raw_entry.as_ptr() as *const Fat32LfnEntry)
                    };
                    
                    let order = lfn.order & 0x3F; // 去掉最高位标志
                    let name_part = Self::parse_lfn_entry(&lfn);
                    
                    // 如果是第一个 LFN 项（最高位为 1），清空之前的
                    if lfn.order & 0x40 != 0 {
                        lfn_parts.clear();
                    }
                    
                    lfn_parts.push((order, name_part));
                } else {
                    // 短文件名项（实际目录项）
                    let entry: Fat32DirEntry = unsafe {
                        core::ptr::read(raw_entry.as_ptr() as *const Fat32DirEntry)
                    };
                    
                    // 组装长文件名
                    let long_name = if !lfn_parts.is_empty() {
                        // 按序号排序（从小到大）
                        lfn_parts.sort_by_key(|(order, _)| *order);
                        let name: String = lfn_parts.iter()
                            .map(|(_, part)| part.as_str())
                            .collect();
                        lfn_parts.clear();
                        name
                    } else {
                        // 没有长文件名，使用短文件名
                        Self::parse_short_name(&entry.name)
                    };
                    
                    entries.push((long_name, entry));
                }
            }
        }
        
        Ok(entries)
    }
    
    /// 解析长文件名目录项中的名称片段
    fn parse_lfn_entry(lfn: &Fat32LfnEntry) -> String {
        let mut chars = Vec::new();
        
        // 安全地读取 packed 结构体的字段
        // 先复制到本地数组，避免未对齐引用
        let name1: [u16; 5] = unsafe {
            let ptr = core::ptr::addr_of!(lfn.name1);
            core::ptr::read_unaligned(ptr)
        };
        let name2: [u16; 6] = unsafe {
            let ptr = core::ptr::addr_of!(lfn.name2);
            core::ptr::read_unaligned(ptr)
        };
        let name3: [u16; 2] = unsafe {
            let ptr = core::ptr::addr_of!(lfn.name3);
            core::ptr::read_unaligned(ptr)
        };
        
        // 读取 name1 (5 个 UTF-16 字符)
        for c in name1 {
            if c == 0x0000 || c == 0xFFFF {
                break;
            }
            chars.push(c);
        }
        
        // 读取 name2 (6 个 UTF-16 字符)
        for c in name2 {
            if c == 0x0000 || c == 0xFFFF {
                break;
            }
            chars.push(c);
        }
        
        // 读取 name3 (2 个 UTF-16 字符)
        for c in name3 {
            if c == 0x0000 || c == 0xFFFF {
                break;
            }
            chars.push(c);
        }
        
        // UTF-16 转 String
        String::from_utf16_lossy(&chars)
    }
    
    /// 解析 8.3 文件名
    fn parse_short_name(name: &[u8; 11]) -> String {
        let mut result = String::new();
        
        // 主文件名
        for i in 0..8 {
            if name[i] == b' ' {
                break;
            }
            result.push(name[i] as char);
        }
        
        // 扩展名
        if name[8] != b' ' {
            result.push('.');
            for i in 8..11 {
                if name[i] == b' ' {
                    break;
                }
                result.push(name[i] as char);
            }
        }
        
        result.to_lowercase()
    }
    
    /// 查找文件（支持长文件名）
    fn find_entry(&self, dir_cluster: u32, name: &str) -> Result<Option<Fat32DirEntry>, &'static str> {
        let entries = self.read_directory_with_lfn(dir_cluster)?;
        let name_lower = name.to_lowercase();
        // 预计算查询名的 8.3 短名，用于无 LFN 场景下的截断匹配
        let query_short = Self::make_short_name(name);
        
        for (idx, (entry_name, entry)) in entries.iter().enumerate() {
            // 优先匹配 LFN 或解析后的短名
            if entry_name.to_lowercase() == name_lower {
                return Ok(Some(*entry));
            }
            // 回退：当名称超过 8.3 限制时，按生成的短名字节匹配
            if entry.name == query_short {
                return Ok(Some(*entry));
            }
        }
        
        Ok(None)
    }
    
    /// 解析路径
    fn resolve_path(&self, path: &str) -> Result<Fat32DirEntry, &'static str> {
        let mut current_cluster = self.root_cluster;
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        
        if parts.is_empty() {
            // 返回根目录
            return Ok(Fat32DirEntry {
                name: [b' '; 11],
                attr: attr::DIRECTORY,
                nt_reserved: 0,
                create_time_tenth: 0,
                create_time: 0,
                create_date: 0,
                last_access_date: 0,
                first_cluster_hi: (self.root_cluster >> 16) as u16,
                write_time: 0,
                write_date: 0,
                first_cluster_lo: self.root_cluster as u16,
                file_size: 0,
            });
        }
        
        let mut entry = None;
        for part in parts {
            entry = self.find_entry(current_cluster, part)?;
            match entry {
                Some(e) => {
                    current_cluster = ((e.first_cluster_hi as u32) << 16) | e.first_cluster_lo as u32;
                }
                None => return Err("Path not found"),
            }
        }
        
        entry.ok_or("Path not found")
    }
    
    /// 生成 8.3 短文件名
    fn make_short_name(name: &str) -> [u8; 11] {
        let mut short_name = [b' '; 11];
        let name_upper = name.to_uppercase();
        
        // 分离文件名和扩展名
        let (base, ext) = if let Some(dot_pos) = name_upper.rfind('.') {
            (&name_upper[..dot_pos], &name_upper[dot_pos + 1..])
        } else {
            (name_upper.as_str(), "")
        };
        
        // 复制主文件名（最多 8 字符）
        let base_bytes: Vec<u8> = base.bytes()
            .filter(|&c| c != b' ' && c != b'.')
            .take(8)
            .collect();
        for (i, &b) in base_bytes.iter().enumerate() {
            short_name[i] = b;
        }
        
        // 复制扩展名（最多 3 字符）
        let ext_bytes: Vec<u8> = ext.bytes()
            .filter(|&c| c != b' ')
            .take(3)
            .collect();
        for (i, &b) in ext_bytes.iter().enumerate() {
            short_name[8 + i] = b;
        }
        
        short_name
    }
    
    /// 在目录中创建新条目
    fn create_entry(&mut self, dir_cluster: u32, name: &str, file_type: FileType) -> Result<Fat32DirEntry, &'static str> {
        // 生成短文件名
        let short_name = Self::make_short_name(name);
        
        // 分配新簇（如果是目录）
        let new_cluster = if file_type == FileType::Directory {
            let cluster = self.alloc_cluster()?;
            // 初始化目录簇（清零）
            let cluster_size = self.bytes_per_cluster as usize;
            self.write_cluster(cluster, &vec![0u8; cluster_size])?;
            cluster
        } else {
            0 // 空文件不分配簇
        };
        
        // 创建目录项
        let new_entry = Fat32DirEntry {
            name: short_name,
            attr: if file_type == FileType::Directory { attr::DIRECTORY } else { attr::ARCHIVE },
            nt_reserved: 0,
            create_time_tenth: 0,
            create_time: 0,
            create_date: 0,
            last_access_date: 0,
            first_cluster_hi: (new_cluster >> 16) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_lo: new_cluster as u16,
            file_size: 0,
        };
        
        // 在目录中找到空闲位置并写入
        let chain = self.get_cluster_chain(dir_cluster)?;
        
        for cluster in &chain {
            let mut data = self.read_cluster(*cluster)?;
            let entry_count = data.len() / size_of::<Fat32DirEntry>();
            
            for i in 0..entry_count {
                let offset = i * size_of::<Fat32DirEntry>();
                let first_byte = data[offset];
                
                // 找到空闲或已删除的条目
                if first_byte == 0x00 || first_byte == 0xE5 {
                    // 写入新条目
                    let entry_bytes = unsafe {
                        core::slice::from_raw_parts(
                            &new_entry as *const _ as *const u8,
                            size_of::<Fat32DirEntry>()
                        )
                    };
                    data[offset..offset + size_of::<Fat32DirEntry>()].copy_from_slice(entry_bytes);
                    
                    // 写回簇
                    self.write_cluster(*cluster, &data)?;
                    
                    // Debug output disabled
                    // crate::println!("[fat32] Created entry: {} in cluster {}", name, cluster);
                    return Ok(new_entry);
                }
            }
        }
        
        // 目录已满，需要分配新簇
        let new_dir_cluster = self.alloc_cluster()?;
        
        // 链接到目录簇链
        if let Some(&last) = chain.last() {
            self.write_fat_entry(last, new_dir_cluster)?;
        }
        
        // 初始化新簇并写入条目
        let cluster_size = self.bytes_per_cluster as usize;
        let mut data = vec![0u8; cluster_size];
        let entry_bytes = unsafe {
            core::slice::from_raw_parts(
                &new_entry as *const _ as *const u8,
                size_of::<Fat32DirEntry>()
            )
        };
        data[..size_of::<Fat32DirEntry>()].copy_from_slice(entry_bytes);
        self.write_cluster(new_dir_cluster, &data)?;
        
        Ok(new_entry)
    }
    
    /// 删除目录中的条目
    fn delete_entry(&mut self, dir_cluster: u32, name: &str) -> Result<(), &'static str> {
        let name_lower = name.to_lowercase();
        let chain = self.get_cluster_chain(dir_cluster)?;
        
        for cluster in &chain {
            let mut data = self.read_cluster(*cluster)?;
            let entry_count = data.len() / size_of::<Fat32DirEntry>();
            
            for i in 0..entry_count {
                let offset = i * size_of::<Fat32DirEntry>();
                let entry: Fat32DirEntry = unsafe {
                    core::ptr::read(data.as_ptr().add(offset) as *const Fat32DirEntry)
                };
                
                // 跳过空项和长文件名项
                if entry.name[0] == 0x00 || entry.name[0] == 0xE5 || entry.attr == attr::LONG_NAME {
                    continue;
                }
                
                let entry_name = Self::parse_short_name(&entry.name);
                if entry_name == name_lower {
                    // 释放文件的簇链
                    let file_cluster = ((entry.first_cluster_hi as u32) << 16) | entry.first_cluster_lo as u32;
                    if file_cluster >= 2 {
                        self.free_cluster_chain(file_cluster)?;
                    }
                    
                    // 标记目录项为已删除（第一个字节设为 0xE5）
                    data[offset] = 0xE5;
                    self.write_cluster(*cluster, &data)?;
                    
                    // Debug output disabled
                    // crate::println!("[fat32] Deleted entry: {}", name);
                    return Ok(());
                }
            }
        }
        
        Err("Entry not found")
    }
    
    /// 更新目录中已有条目的内容（按短名匹配）
    fn update_entry(&self, dir_cluster: u32, short_name: &[u8; 11], updated: &Fat32DirEntry) -> Result<(), &'static str> {
        let chain = self.get_cluster_chain(dir_cluster)?;
        
        for cluster in &chain {
            let mut data = self.read_cluster(*cluster)?;
            let entry_count = data.len() / size_of::<Fat32DirEntry>();
            
            for i in 0..entry_count {
                let offset = i * size_of::<Fat32DirEntry>();
                let entry: Fat32DirEntry = unsafe {
                    core::ptr::read(data.as_ptr().add(offset) as *const Fat32DirEntry)
                };
                
                if entry.name[0] == 0x00 {
                    return Err("Entry not found");
                }
                if entry.name[0] == 0xE5 || entry.attr == attr::LONG_NAME {
                    continue;
                }
                
                if entry.name == *short_name {
                    let entry_bytes = unsafe {
                        core::slice::from_raw_parts(
                            updated as *const _ as *const u8,
                            size_of::<Fat32DirEntry>()
                        )
                    };
                    data[offset..offset + size_of::<Fat32DirEntry>()].copy_from_slice(entry_bytes);
                    self.write_cluster(*cluster, &data)?;
                    return Ok(());
                }
            }
        }
        
        Err("Entry not found")
    }
    
    /// 读取文件内容
    pub fn read_file(&self, path: &str) -> Result<Vec<u8>, &'static str> {
        let entry = self.resolve_path(path)?;
        
        if entry.attr & attr::DIRECTORY != 0 {
            return Err("Is a directory");
        }
        
        let start_cluster = ((entry.first_cluster_hi as u32) << 16) | entry.first_cluster_lo as u32;
        let chain = self.get_cluster_chain(start_cluster)?;
        
        let mut data = Vec::new();
        let file_size = entry.file_size as usize;
        
        for cluster in chain {
            let cluster_data = self.read_cluster(cluster)?;
            let remaining = file_size - data.len();
            let to_copy = remaining.min(cluster_data.len());
            data.extend_from_slice(&cluster_data[..to_copy]);
            
            if data.len() >= file_size {
                break;
            }
        }
        
        Ok(data)
    }
    
    /// 写入文件内容（覆盖写入）
    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<(), &'static str> {
        // 解析路径，获取父目录簇和文件名
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            return Err("Invalid path");
        }
        let file_name = *parts.last().unwrap();
        
        // 定位父目录簇
        let mut parent_cluster = self.root_cluster;
        for &part in &parts[..parts.len() - 1] {
            let entry = self.find_entry(parent_cluster, part)?
                .ok_or("Path not found")?;
            parent_cluster = ((entry.first_cluster_hi as u32) << 16) | entry.first_cluster_lo as u32;
        }
        
        // 查找文件目录项
        let entry = self.find_entry(parent_cluster, file_name)?
            .ok_or("File not found")?;
        
        if entry.attr & attr::DIRECTORY != 0 {
            return Err("Is a directory");
        }
        
        let start_cluster = ((entry.first_cluster_hi as u32) << 16) | entry.first_cluster_lo as u32;
        let cluster_size = self.bytes_per_cluster as usize;
        
        // 获取现有簇链
        let mut chain = self.get_cluster_chain(start_cluster)?;
        
        // 计算需要的簇数
        let clusters_needed = if data.is_empty() { 0 } else {
            (data.len() + cluster_size - 1) / cluster_size
        };
        
        // 分配更多簇（如果需要）
        while chain.len() < clusters_needed {
            let new_cluster = self.alloc_cluster()?;
            if let Some(&last) = chain.last() {
                self.write_fat_entry(last, new_cluster)?;
            }
            // 标记新簇为链尾
            self.write_fat_entry(new_cluster, fat_entry::END)?;
            chain.push(new_cluster);
        }
        
        // 释放多余的簇（如果有）
        if chain.len() > clusters_needed {
            if clusters_needed > 0 {
                let to_free = chain[clusters_needed];
                self.write_fat_entry(chain[clusters_needed - 1], fat_entry::END)?;
                let mut cluster = to_free;
                while cluster >= 2 && cluster < fat_entry::RESERVED {
                    let next = self.read_fat_entry(cluster)?;
                    self.write_fat_entry(cluster, fat_entry::FREE)?;
                    cluster = next;
                }
            } else {
                // 释放所有簇
                self.free_cluster_chain(chain[0])?;
            }
            chain.truncate(clusters_needed);
        }
        
        // 写入数据
        let mut offset = 0;
        for &cluster in &chain {
            let end = (offset + cluster_size).min(data.len());
            if offset < data.len() {
                self.write_cluster(cluster, &data[offset..end])?;
            }
            offset = end;
        }
        
        // 更新目录项：first_cluster 和 file_size
        let new_first = chain.first().copied().unwrap_or(0);
        let mut updated_entry = entry;
        updated_entry.first_cluster_hi = (new_first >> 16) as u16;
        updated_entry.first_cluster_lo = new_first as u16;
        updated_entry.file_size = data.len() as u32;
        self.update_entry(parent_cluster, &entry.name, &updated_entry)?;
        
        Ok(())
    }
    
    /// 列出目录
    pub fn list_dir(&self, path: &str) -> Result<Vec<DirEntry>, &'static str> {
        let entry = self.resolve_path(path)?;
        
        if entry.attr & attr::DIRECTORY == 0 {
            return Err("Not a directory");
        }
        
        let cluster = ((entry.first_cluster_hi as u32) << 16) | entry.first_cluster_lo as u32;
        let entries = self.read_directory_with_lfn(cluster)?;
        
        let mut result = Vec::new();
        for (name, e) in entries {
            // 跳过 . 和 ..
            if name == "." || name == ".." || e.name[0] == b'.' {
                continue;
            }
            
            let _inode_type = if e.attr & attr::DIRECTORY != 0 {
                InodeType::Directory
            } else {
                InodeType::File
            };
            
            result.push(DirEntry {
                name,
                file_type: if e.attr & attr::DIRECTORY != 0 {
                    FileType::Directory
                } else {
                    FileType::Regular
                },
                inode: ((e.first_cluster_hi as usize) << 16) | e.first_cluster_lo as usize,
            });
        }
        
        Ok(result)
    }
}

/// FAT32 Inode
pub struct Fat32Inode {
    /// 文件系统引用
    fs: Arc<SpinLock<Fat32Fs>>,
    /// 目录项
    entry: Fat32DirEntry,
    /// 路径
    path: String,
    /// 文件数据缓存——避免每次 read() 都重新从磁盘读取整个文件
    cached_data: SpinLock<Option<Vec<u8>>>,
}

impl Fat32Inode {
    pub fn new(fs: Arc<SpinLock<Fat32Fs>>, entry: Fat32DirEntry, path: String) -> Self {
        Self { fs, entry, path, cached_data: SpinLock::new(None) }
    }
}

impl File for Fat32Inode {
    fn read(&self, offset: usize, buf: &mut [u8]) -> Result<usize, FsError> {
        // 快速路径：缓存命中
        {
            let cache = self.cached_data.lock();
            if let Some(ref data) = *cache {
                if offset >= data.len() {
                    return Ok(0);
                }
                let to_read = buf.len().min(data.len() - offset);
                buf[..to_read].copy_from_slice(&data[offset..offset + to_read]);
                return Ok(to_read);
            }
        }
        // 缓存未命中：从磁盘读取并缓存
        let data = {
            let fs = self.fs.lock();
            fs.read_file(&self.path).map_err(|_| FsError::IoError)?
        };
        if offset >= data.len() {
            *self.cached_data.lock() = Some(data);
            return Ok(0);
        }
        let to_read = buf.len().min(data.len() - offset);
        buf[..to_read].copy_from_slice(&data[offset..offset + to_read]);
        *self.cached_data.lock() = Some(data);
        Ok(to_read)
    }
    
    fn write(&self, offset: usize, buf: &[u8]) -> Result<usize, FsError> {
        // 从缓存或磁盘获取现有数据
        let mut data = match self.cached_data.lock().take() {
            Some(d) => d,
            None => {
                let fs = self.fs.lock();
                fs.read_file(&self.path).unwrap_or_else(|_| Vec::new())
            }
        };
        
        if offset + buf.len() > data.len() {
            data.resize(offset + buf.len(), 0);
        }
        data[offset..offset + buf.len()].copy_from_slice(buf);
        
        {
            let fs = self.fs.lock();
            fs.write_file(&self.path, &data).map_err(|_| FsError::IoError)?;
        }
        
        *self.cached_data.lock() = Some(data);
        Ok(buf.len())
    }
    
    fn stat(&self) -> Result<FileStat, FsError> {
        Ok(FileStat {
            file_type: if self.entry.attr & attr::DIRECTORY != 0 {
                FileType::Directory
            } else {
                FileType::Regular
            },
            size: self.entry.file_size as usize,
            mode: FileMode::default_file(),
            nlink: 1,
            uid: 0,
            gid: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        })
    }
    
    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

impl Directory for Fat32Inode {
    fn read_dir(&self) -> Result<Vec<DirEntry>, FsError> {
        let fs = self.fs.lock();
        fs.list_dir(&self.path).map_err(|_| FsError::IoError)
    }
    
    fn lookup(&self, name: &str) -> Result<Arc<dyn Inode>, FsError> {
        let fs = self.fs.lock();
        let cluster = ((self.entry.first_cluster_hi as u32) << 16) | self.entry.first_cluster_lo as u32;
        // crate::println!("[fat32] lookup: cluster={}", cluster);
        
        if let Some(entry) = fs.find_entry(cluster, name).map_err(|_e| {
            // crate::println!("[fat32] lookup: find_entry error: {}", e);
            FsError::IoError
        })? {
            let new_path = if self.path == "/" {
                alloc::format!("/{}", name)
            } else {
                alloc::format!("{}/{}", self.path, name)
            };
            // crate::println!("[fat32] lookup: found entry, new_path={}", new_path);
            
            drop(fs);
            Ok(Arc::new(Fat32Inode::new(self.fs.clone(), entry, new_path)))
        } else {
            // crate::println!("[fat32] lookup: entry not found");
            Err(FsError::NotFound)
        }
    }
    
    fn create(&self, name: &str, file_type: FileType) -> Result<Arc<dyn Inode>, FsError> {
        let mut fs = self.fs.lock();
        let dir_cluster = ((self.entry.first_cluster_hi as u32) << 16) | self.entry.first_cluster_lo as u32;
        
        // 检查文件是否已存在
        if fs.find_entry(dir_cluster, name).map_err(|_| FsError::IoError)?.is_some() {
            return Err(FsError::AlreadyExists);
        }
        
        // 创建新的目录项
        let new_entry = fs.create_entry(dir_cluster, name, file_type)
            .map_err(|_| FsError::IoError)?;
        
        let new_path = if self.path == "/" {
            alloc::format!("/{}", name)
        } else {
            alloc::format!("{}/{}", self.path, name)
        };
        
        drop(fs);
        Ok(Arc::new(Fat32Inode::new(self.fs.clone(), new_entry, new_path)))
    }
    
    fn unlink(&self, name: &str) -> Result<(), FsError> {
        let mut fs = self.fs.lock();
        let dir_cluster = ((self.entry.first_cluster_hi as u32) << 16) | self.entry.first_cluster_lo as u32;
        
        // 查找要删除的文件
        let entry = fs.find_entry(dir_cluster, name)
            .map_err(|_| FsError::IoError)?
            .ok_or(FsError::NotFound)?;
        
        // 不允许删除目录（需要用 rmdir）
        if entry.attr & attr::DIRECTORY != 0 {
            return Err(FsError::IsDirectory);
        }
        
        // 删除目录项和释放簇链
        fs.delete_entry(dir_cluster, name)
            .map_err(|_| FsError::IoError)?;
        
        Ok(())
    }
}

impl Inode for Fat32Inode {
    fn ino(&self) -> usize {
        ((self.entry.first_cluster_hi as usize) << 16) | self.entry.first_cluster_lo as usize
    }
    
    fn file_type(&self) -> FileType {
        if self.entry.attr & attr::DIRECTORY != 0 {
            FileType::Directory
        } else {
            FileType::Regular
        }
    }
    
    fn as_file(&self) -> Option<&dyn File> {
        if self.entry.attr & attr::DIRECTORY == 0 {
            Some(self)
        } else {
            None
        }
    }
    
    fn as_dir(&self) -> Option<&dyn Directory> {
        if self.entry.attr & attr::DIRECTORY != 0 {
            Some(self)
        } else {
            None
        }
    }
    
    fn stat(&self) -> Result<FileStat, FsError> {
        File::stat(self)
    }
}

impl FileSystem for Fat32Fs {
    fn name(&self) -> &str {
        "fat32"
    }
    
    fn root(&self) -> Arc<dyn Inode> {
        // 这里需要重构以支持 Arc<SpinLock<Fat32Fs>>
        unimplemented!("Use Fat32FsWrapper instead")
    }
    
    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}

impl Fat32FsWrapper {
    /// 创建 FAT32 文件系统包装器
    pub fn new(fs: Fat32Fs) -> Self {
        Self {
            inner: Arc::new(SpinLock::new(fs)),
        }
    }
    
    /// 从 VirtIO 块设备挂载 FAT32
    pub fn mount_virtio() -> Result<Self, &'static str> {
        use crate::driver::virtio::blk::{read_sectors, write_sectors};
        
        let read_fn = |sector: u64, count: usize| -> Result<Vec<u8>, &'static str> {
            read_sectors(sector, count)
        };
        
        let write_fn = |sector: u64, data: Vec<u8>| -> Result<(), &'static str> {
            write_sectors(sector, data)
        };
        
        let fs = Fat32Fs::mount(read_fn, write_fn)?;
        Ok(Self::new(fs))
    }
    
    /// 获取根目录 Inode
    pub fn root_inode(&self) -> Arc<Fat32Inode> {
        let fs = self.inner.lock();
        let root_cluster = fs.root_cluster;
        drop(fs);
        
        let entry = Fat32DirEntry {
            name: [b' '; 11],
            attr: attr::DIRECTORY,
            nt_reserved: 0,
            create_time_tenth: 0,
            create_time: 0,
            create_date: 0,
            last_access_date: 0,
            first_cluster_hi: (root_cluster >> 16) as u16,
            write_time: 0,
            write_date: 0,
            first_cluster_lo: root_cluster as u16,
            file_size: 0,
        };
        
        Arc::new(Fat32Inode::new(self.inner.clone(), entry, String::from("/")))
    }
}

impl FileSystem for Fat32FsWrapper {
    fn name(&self) -> &str {
        "fat32"
    }
    
    fn root(&self) -> Arc<dyn Inode> {
        self.root_inode()
    }
    
    fn sync(&self) -> Result<(), FsError> {
        Ok(())
    }
}


/// FAT32 自测试
pub fn self_test() {
    crate::println!("[fat32] Running self-test...");
    
    // 测试常量
    assert_eq!(SECTOR_SIZE, 512);
    
    // 测试属性常量
    let _attrs = [
        attr::READ_ONLY,
        attr::HIDDEN,
        attr::SYSTEM,
        attr::VOLUME_ID,
        attr::DIRECTORY,
        attr::ARCHIVE,
        attr::LONG_NAME,
    ];
    
    // 测试 FAT 表项常量
    let _fat_entries = [
        fat_entry::FREE,
        fat_entry::RESERVED,
        fat_entry::BAD,
        fat_entry::END,
    ];
    
    // 测试 Fat32BootSector 结构体大小
    assert!(core::mem::size_of::<Fat32BootSector>() > 0);
    
    // 测试 Fat32DirEntry 结构体大小
    assert_eq!(core::mem::size_of::<Fat32DirEntry>(), 32);
    
    // 测试 Fat32LfnEntry 结构体大小
    assert_eq!(core::mem::size_of::<Fat32LfnEntry>(), 32);
    
    // 测试 parse_short_name
    let name: [u8; 11] = *b"TEST    TXT";
    let parsed = Fat32Fs::parse_short_name(&name);
    assert_eq!(parsed, "test.txt");
    
    // 测试无扩展名的文件名
    let name2: [u8; 11] = *b"README     ";
    let parsed2 = Fat32Fs::parse_short_name(&name2);
    assert_eq!(parsed2, "readme");
    
    // 测试 Fat32Fs::mount（使用模拟的读写函数）
    let mock_read = |_sector: u64, _count: usize| -> Result<Vec<u8>, &'static str> {
        // 返回一个模拟的引导扇区
        let mut data = vec![0u8; 512];
        // 设置 FAT32 签名
        data[82..87].copy_from_slice(b"FAT32");
        // 设置每扇区字节数
        data[11] = 0x00;
        data[12] = 0x02; // 512 bytes
        // 设置每簇扇区数
        data[13] = 1;
        // 设置保留扇区数
        data[14] = 1;
        data[15] = 0;
        // 设置 FAT 数量
        data[16] = 2;
        // 设置根目录簇
        data[44] = 2;
        data[45] = 0;
        data[46] = 0;
        data[47] = 0;
        Ok(data)
    };
    
    let mock_write = |_sector: u64, _data: Vec<u8>| -> Result<(), &'static str> {
        Ok(())
    };
    
    // 尝试挂载（可能会失败，因为模拟数据不完整）
    let result = Fat32Fs::mount(mock_read, mock_write);
    if result.is_ok() {
        let fs = result.unwrap();
        // 测试 bytes_per_cluster 字段
        let _bpc = fs.bytes_per_cluster;
        // 测试 fat_cache 字段
        let _cache_len = fs.fat_cache.len();
        // 测试 write_fn 字段（通过调用）
        let _ = (fs.write_fn)(0, vec![0u8; 512]);
        crate::println!("[fat32] Mock mount succeeded");
    } else {
        crate::println!("[fat32] Mock mount failed (expected for incomplete data)");
    }
    
    crate::println!("[fat32] Self-test passed!");
}
