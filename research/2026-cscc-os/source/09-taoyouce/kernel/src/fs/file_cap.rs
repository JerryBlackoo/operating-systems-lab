//! 文件能力集成
//!
//! 将能力系统与文件系统集成，实现基于能力的文件访问控制

#![allow(dead_code)]

use alloc::string::String;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::sync::SpinLock;

/// 文件能力权限
#[derive(Debug, Clone, Copy)]
pub struct FileCapRights {
    /// 可读
    pub read: bool,
    /// 可写
    pub write: bool,
    /// 可执行
    pub execute: bool,
    /// 可追加
    pub append: bool,
    /// 可截断
    pub truncate: bool,
    /// 可创建
    pub create: bool,
    /// 可删除
    pub delete: bool,
    /// 可获取属性
    pub stat: bool,
    /// 可修改属性
    pub chmod: bool,
    /// 可授权
    pub grant: bool,
}

impl FileCapRights {
    /// 只读权限
    pub const READ_ONLY: Self = Self {
        read: true,
        write: false,
        execute: false,
        append: false,
        truncate: false,
        create: false,
        delete: false,
        stat: true,
        chmod: false,
        grant: false,
    };
    
    /// 读写权限
    pub const READ_WRITE: Self = Self {
        read: true,
        write: true,
        execute: false,
        append: true,
        truncate: true,
        create: false,
        delete: false,
        stat: true,
        chmod: false,
        grant: false,
    };
    
    /// 完全权限
    pub const FULL: Self = Self {
        read: true,
        write: true,
        execute: true,
        append: true,
        truncate: true,
        create: true,
        delete: true,
        stat: true,
        chmod: true,
        grant: true,
    };
    
    /// 执行权限
    pub const EXECUTE: Self = Self {
        read: true,
        write: false,
        execute: true,
        append: false,
        truncate: false,
        create: false,
        delete: false,
        stat: true,
        chmod: false,
        grant: false,
    };
    
    /// 检查是否包含指定权限
    pub fn contains(&self, other: &Self) -> bool {
        (!other.read || self.read) &&
        (!other.write || self.write) &&
        (!other.execute || self.execute) &&
        (!other.append || self.append) &&
        (!other.truncate || self.truncate) &&
        (!other.create || self.create) &&
        (!other.delete || self.delete) &&
        (!other.stat || self.stat) &&
        (!other.chmod || self.chmod) &&
        (!other.grant || self.grant)
    }
    
    /// 权限交集
    pub fn intersect(&self, other: &Self) -> Self {
        Self {
            read: self.read && other.read,
            write: self.write && other.write,
            execute: self.execute && other.execute,
            append: self.append && other.append,
            truncate: self.truncate && other.truncate,
            create: self.create && other.create,
            delete: self.delete && other.delete,
            stat: self.stat && other.stat,
            chmod: self.chmod && other.chmod,
            grant: self.grant && other.grant,
        }
    }
}

/// 目录能力权限
#[derive(Debug, Clone, Copy)]
pub struct DirCapRights {
    /// 可列出
    pub list: bool,
    /// 可查找
    pub lookup: bool,
    /// 可创建文件
    pub create_file: bool,
    /// 可创建目录
    pub create_dir: bool,
    /// 可删除
    pub delete: bool,
    /// 可重命名
    pub rename: bool,
    /// 可获取属性
    pub stat: bool,
    /// 可授权
    pub grant: bool,
}

impl DirCapRights {
    /// 只读权限
    pub const READ_ONLY: Self = Self {
        list: true,
        lookup: true,
        create_file: false,
        create_dir: false,
        delete: false,
        rename: false,
        stat: true,
        grant: false,
    };
    
    /// 完全权限
    pub const FULL: Self = Self {
        list: true,
        lookup: true,
        create_file: true,
        create_dir: true,
        delete: true,
        rename: true,
        stat: true,
        grant: true,
    };
}

/// 文件能力
#[derive(Debug, Clone)]
pub struct FileCapability {
    /// 能力 ID
    pub cap_id: u64,
    /// 文件路径（或 inode ID）
    pub path: String,
    /// Inode ID
    pub inode_id: u64,
    /// 权限
    pub rights: FileCapRights,
    /// 所有者进程
    pub owner_pid: u64,
    /// 是否有效
    pub valid: bool,
}

impl FileCapability {
    pub fn new(cap_id: u64, path: String, inode_id: u64, rights: FileCapRights, owner_pid: u64) -> Self {
        Self {
            cap_id,
            path,
            inode_id,
            rights,
            owner_pid,
            valid: true,
        }
    }
    
    /// 派生新能力（权限缩减）
    pub fn derive(&self, new_cap_id: u64, new_rights: FileCapRights, new_owner: u64) -> Option<Self> {
        if !self.valid || !self.rights.grant {
            return None;
        }
        
        // 新权限不能超过原权限
        let derived_rights = self.rights.intersect(&new_rights);
        
        Some(Self {
            cap_id: new_cap_id,
            path: self.path.clone(),
            inode_id: self.inode_id,
            rights: derived_rights,
            owner_pid: new_owner,
            valid: true,
        })
    }
    
    /// 撤销
    pub fn revoke(&mut self) {
        self.valid = false;
    }
}

/// 目录能力
#[derive(Debug, Clone)]
pub struct DirCapability {
    /// 能力 ID
    pub cap_id: u64,
    /// 目录路径
    pub path: String,
    /// Inode ID
    pub inode_id: u64,
    /// 权限
    pub rights: DirCapRights,
    /// 所有者进程
    pub owner_pid: u64,
    /// 是否有效
    pub valid: bool,
}

impl DirCapability {
    pub fn new(cap_id: u64, path: String, inode_id: u64, rights: DirCapRights, owner_pid: u64) -> Self {
        Self {
            cap_id,
            path,
            inode_id,
            rights,
            owner_pid,
            valid: true,
        }
    }
}

/// 文件能力空间
pub struct FileCSpace {
    /// 文件能力
    file_caps: BTreeMap<u64, FileCapability>,
    /// 目录能力
    dir_caps: BTreeMap<u64, DirCapability>,
    /// 下一个能力 ID
    next_cap_id: AtomicU64,
    /// 所有者进程
    owner_pid: u64,
}

impl FileCSpace {
    pub fn new(owner_pid: u64) -> Self {
        Self {
            file_caps: BTreeMap::new(),
            dir_caps: BTreeMap::new(),
            next_cap_id: AtomicU64::new(1),
            owner_pid,
        }
    }
    
    /// 分配能力 ID
    fn alloc_cap_id(&self) -> u64 {
        self.next_cap_id.fetch_add(1, Ordering::SeqCst)
    }
    
    /// 创建文件能力
    pub fn create_file_cap(&mut self, path: &str, inode_id: u64, rights: FileCapRights) -> u64 {
        let cap_id = self.alloc_cap_id();
        let cap = FileCapability::new(cap_id, String::from(path), inode_id, rights, self.owner_pid);
        self.file_caps.insert(cap_id, cap);
        cap_id
    }
    
    /// 创建目录能力
    pub fn create_dir_cap(&mut self, path: &str, inode_id: u64, rights: DirCapRights) -> u64 {
        let cap_id = self.alloc_cap_id();
        let cap = DirCapability::new(cap_id, String::from(path), inode_id, rights, self.owner_pid);
        self.dir_caps.insert(cap_id, cap);
        cap_id
    }
    
    /// 查找文件能力
    pub fn lookup_file_cap(&self, cap_id: u64) -> Option<&FileCapability> {
        self.file_caps.get(&cap_id).filter(|c| c.valid)
    }
    
    /// 查找目录能力
    pub fn lookup_dir_cap(&self, cap_id: u64) -> Option<&DirCapability> {
        self.dir_caps.get(&cap_id).filter(|c| c.valid)
    }
    
    /// 撤销文件能力
    pub fn revoke_file_cap(&mut self, cap_id: u64) -> Result<(), &'static str> {
        let cap = self.file_caps.get_mut(&cap_id).ok_or("Capability not found")?;
        cap.revoke();
        Ok(())
    }
    
    /// 派生文件能力
    pub fn derive_file_cap(&self, cap_id: u64, new_rights: FileCapRights, target: &mut FileCSpace) -> Result<u64, &'static str> {
        let cap = self.lookup_file_cap(cap_id).ok_or("Capability not found")?;
        
        let new_cap_id = target.alloc_cap_id();
        let derived = cap.derive(new_cap_id, new_rights, target.owner_pid)
            .ok_or("Cannot derive capability")?;
        
        target.file_caps.insert(new_cap_id, derived);
        Ok(new_cap_id)
    }
}

/// 文件能力验证器
pub struct FileCapVerifier;

impl FileCapVerifier {
    /// 验证读取权限
    pub fn verify_read(cspace: &FileCSpace, cap_id: u64) -> Result<u64, &'static str> {
        let cap = cspace.lookup_file_cap(cap_id).ok_or("Invalid capability")?;
        if !cap.rights.read {
            return Err("No read permission");
        }
        Ok(cap.inode_id)
    }
    
    /// 验证写入权限
    pub fn verify_write(cspace: &FileCSpace, cap_id: u64) -> Result<u64, &'static str> {
        let cap = cspace.lookup_file_cap(cap_id).ok_or("Invalid capability")?;
        if !cap.rights.write {
            return Err("No write permission");
        }
        Ok(cap.inode_id)
    }
    
    /// 验证执行权限
    pub fn verify_execute(cspace: &FileCSpace, cap_id: u64) -> Result<u64, &'static str> {
        let cap = cspace.lookup_file_cap(cap_id).ok_or("Invalid capability")?;
        if !cap.rights.execute {
            return Err("No execute permission");
        }
        Ok(cap.inode_id)
    }
    
    /// 验证目录列出权限
    pub fn verify_list(cspace: &FileCSpace, cap_id: u64) -> Result<u64, &'static str> {
        let cap = cspace.lookup_dir_cap(cap_id).ok_or("Invalid capability")?;
        if !cap.rights.list {
            return Err("No list permission");
        }
        Ok(cap.inode_id)
    }
    
    /// 验证目录创建文件权限
    pub fn verify_create_file(cspace: &FileCSpace, cap_id: u64) -> Result<u64, &'static str> {
        let cap = cspace.lookup_dir_cap(cap_id).ok_or("Invalid capability")?;
        if !cap.rights.create_file {
            return Err("No create file permission");
        }
        Ok(cap.inode_id)
    }
}

/// 全局文件能力空间表
static FILE_CSPACES: SpinLock<BTreeMap<u64, FileCSpace>> = SpinLock::new(BTreeMap::new());

/// 初始化进程的文件能力空间
pub fn init_process_file_cspace(pid: u64) {
    let mut cspace = FileCSpace::new(pid);
    
    // 授予根目录能力
    cspace.create_dir_cap("/", 0, DirCapRights::READ_ONLY);
    
    FILE_CSPACES.lock().insert(pid, cspace);
}

/// 销毁进程的文件能力空间
pub fn destroy_process_file_cspace(pid: u64) {
    FILE_CSPACES.lock().remove(&pid);
}

/// 打开文件（基于能力）
pub fn cap_open(pid: u64, dir_cap_id: u64, name: &str, rights: FileCapRights) -> Result<u64, &'static str> {
    let mut cspaces = FILE_CSPACES.lock();
    let cspace = cspaces.get_mut(&pid).ok_or("Process CSpace not found")?;
    
    // 验证目录能力
    let dir_cap = cspace.lookup_dir_cap(dir_cap_id).ok_or("Invalid directory capability")?;
    if !dir_cap.rights.lookup {
        return Err("No lookup permission");
    }
    
    // 构建完整路径
    let path = if dir_cap.path == "/" {
        alloc::format!("/{}", name)
    } else {
        alloc::format!("{}/{}", dir_cap.path, name)
    };
    
    // Resolve path through VFS to get the real inode ID
    let inode_id = match super::lookup(&path) {
        Ok(inode) => inode.ino() as u64,
        Err(_) => return Err("File not found"),
    };
    
    // 创建文件能力
    let cap_id = cspace.create_file_cap(&path, inode_id, rights);
    
    Ok(cap_id)
}

/// 读取文件（基于能力）
pub fn cap_read(pid: u64, cap_id: u64, buf: &mut [u8]) -> Result<usize, &'static str> {
    let cspaces = FILE_CSPACES.lock();
    let cspace = cspaces.get(&pid).ok_or("Process CSpace not found")?;
    
    let inode_id = FileCapVerifier::verify_read(cspace, cap_id)?;
    
    // 调用 VFS 读取
    // vfs::read(inode_id, buf)
    let _ = (inode_id, buf);
    Ok(0)
}

/// 写入文件（基于能力）
pub fn cap_write(pid: u64, cap_id: u64, buf: &[u8]) -> Result<usize, &'static str> {
    let cspaces = FILE_CSPACES.lock();
    let cspace = cspaces.get(&pid).ok_or("Process CSpace not found")?;
    
    let inode_id = FileCapVerifier::verify_write(cspace, cap_id)?;
    
    // 调用 VFS 写入
    // vfs::write(inode_id, buf)
    let _ = (inode_id, buf);
    Ok(buf.len())
}

/// 初始化
pub fn init() {
    crate::println!("[file_cap] File capability integration initialized");
}
