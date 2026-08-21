//! IPC 能力验证集成
//!
//! 将能力系统与 IPC 子系统集成，实现基于能力的访问控制

use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::sync::SpinLock;

/// IPC 端点能力
#[derive(Debug, Clone, Copy)]
pub struct EndpointCapability {
    /// 端点 ID
    pub endpoint_id: u64,
    /// 权限
    pub rights: EndpointRights,
    /// 徽章（用于识别发送者）
    pub badge: u64,
}

/// 端点权限
#[derive(Debug, Clone, Copy)]
pub struct EndpointRights {
    /// 可发送
    pub can_send: bool,
    /// 可接收
    pub can_recv: bool,
    /// 可授权
    pub can_grant: bool,
    /// 可撤销
    pub can_revoke: bool,
}

impl EndpointRights {
    pub const SEND_ONLY: Self = Self {
        can_send: true,
        can_recv: false,
        can_grant: false,
        can_revoke: false,
    };
    
    pub const RECV_ONLY: Self = Self {
        can_send: false,
        can_recv: true,
        can_grant: false,
        can_revoke: false,
    };
    
    pub const FULL: Self = Self {
        can_send: true,
        can_recv: true,
        can_grant: true,
        can_revoke: true,
    };
}

/// 通知能力
#[derive(Debug, Clone, Copy)]
pub struct NotificationCapability {
    /// 通知对象 ID
    pub notification_id: u64,
    /// 权限
    pub rights: NotificationRights,
    /// 信号掩码
    pub signal_mask: u64,
}

/// 通知权限
#[derive(Debug, Clone, Copy)]
pub struct NotificationRights {
    /// 可发送信号
    pub can_signal: bool,
    /// 可等待信号
    pub can_wait: bool,
    /// 可轮询
    pub can_poll: bool,
}

impl NotificationRights {
    pub const SIGNAL_ONLY: Self = Self {
        can_signal: true,
        can_wait: false,
        can_poll: false,
    };
    
    pub const WAIT_ONLY: Self = Self {
        can_signal: false,
        can_wait: true,
        can_poll: true,
    };
    
    pub const FULL: Self = Self {
        can_signal: true,
        can_wait: true,
        can_poll: true,
    };
}

/// 共享内存能力
#[derive(Debug, Clone, Copy)]
pub struct SharedMemoryCapability {
    /// 共享内存区域 ID
    pub region_id: u64,
    /// 权限
    pub rights: SharedMemoryRights,
    /// 大小
    pub size: usize,
}

/// 共享内存权限
#[derive(Debug, Clone, Copy)]
pub struct SharedMemoryRights {
    /// 可读
    pub can_read: bool,
    /// 可写
    pub can_write: bool,
    /// 可执行
    pub can_exec: bool,
    /// 可映射
    pub can_map: bool,
}

impl SharedMemoryRights {
    pub const READ_ONLY: Self = Self {
        can_read: true,
        can_write: false,
        can_exec: false,
        can_map: true,
    };
    
    pub const READ_WRITE: Self = Self {
        can_read: true,
        can_write: true,
        can_exec: false,
        can_map: true,
    };
    
    pub const FULL: Self = Self {
        can_read: true,
        can_write: true,
        can_exec: true,
        can_map: true,
    };
}

/// IPC 能力类型
#[derive(Debug, Clone, Copy)]
pub enum IpcCapabilityType {
    /// 端点
    Endpoint(EndpointCapability),
    /// 通知
    Notification(NotificationCapability),
    /// 共享内存
    SharedMemory(SharedMemoryCapability),
}

/// IPC 能力
#[derive(Debug, Clone, Copy)]
pub struct IpcCapability {
    /// 能力 ID
    pub cap_id: u64,
    /// 能力类型
    pub cap_type: IpcCapabilityType,
    /// 所有者进程
    pub owner_pid: u64,
    /// 是否有效
    pub valid: bool,
}

impl IpcCapability {
    /// 创建端点能力
    pub fn new_endpoint(cap_id: u64, endpoint_id: u64, rights: EndpointRights, badge: u64, owner_pid: u64) -> Self {
        Self {
            cap_id,
            cap_type: IpcCapabilityType::Endpoint(EndpointCapability {
                endpoint_id,
                rights,
                badge,
            }),
            owner_pid,
            valid: true,
        }
    }
    
    /// 创建通知能力
    pub fn new_notification(cap_id: u64, notification_id: u64, rights: NotificationRights, signal_mask: u64, owner_pid: u64) -> Self {
        Self {
            cap_id,
            cap_type: IpcCapabilityType::Notification(NotificationCapability {
                notification_id,
                rights,
                signal_mask,
            }),
            owner_pid,
            valid: true,
        }
    }
    
    /// 创建共享内存能力
    pub fn new_shared_memory(cap_id: u64, region_id: u64, rights: SharedMemoryRights, size: usize, owner_pid: u64) -> Self {
        Self {
            cap_id,
            cap_type: IpcCapabilityType::SharedMemory(SharedMemoryCapability {
                region_id,
                rights,
                size,
            }),
            owner_pid,
            valid: true,
        }
    }
    
    /// 检查发送权限
    pub fn can_send(&self) -> bool {
        match &self.cap_type {
            IpcCapabilityType::Endpoint(ep) => ep.rights.can_send,
            IpcCapabilityType::Notification(n) => n.rights.can_signal,
            IpcCapabilityType::SharedMemory(_) => false,
        }
    }
    
    /// 检查接收权限
    pub fn can_recv(&self) -> bool {
        match &self.cap_type {
            IpcCapabilityType::Endpoint(ep) => ep.rights.can_recv,
            IpcCapabilityType::Notification(n) => n.rights.can_wait,
            IpcCapabilityType::SharedMemory(_) => false,
        }
    }
    
    /// 派生新能力（权限缩减）
    pub fn derive(&self, new_cap_id: u64, new_owner: u64) -> Option<Self> {
        if !self.valid {
            return None;
        }
        
        let mut derived = *self;
        derived.cap_id = new_cap_id;
        derived.owner_pid = new_owner;
        
        // 派生的能力不能有授权权限
        match &mut derived.cap_type {
            IpcCapabilityType::Endpoint(ep) => {
                ep.rights.can_grant = false;
                ep.rights.can_revoke = false;
            }
            _ => {}
        }
        
        Some(derived)
    }
    
    /// 撤销能力
    pub fn revoke(&mut self) {
        self.valid = false;
    }
}

/// IPC 能力空间
pub struct IpcCSpace {
    /// 能力表
    capabilities: BTreeMap<u64, IpcCapability>,
    /// 下一个能力 ID
    next_cap_id: AtomicU64,
    /// 所有者进程
    owner_pid: u64,
}

impl IpcCSpace {
    pub fn new(owner_pid: u64) -> Self {
        Self {
            capabilities: BTreeMap::new(),
            next_cap_id: AtomicU64::new(1),
            owner_pid,
        }
    }
    
    /// 分配能力 ID
    fn alloc_cap_id(&self) -> u64 {
        self.next_cap_id.fetch_add(1, Ordering::SeqCst)
    }
    
    /// 插入能力
    pub fn insert(&mut self, cap: IpcCapability) -> u64 {
        let cap_id = cap.cap_id;
        self.capabilities.insert(cap_id, cap);
        cap_id
    }
    
    /// 创建端点能力
    pub fn create_endpoint_cap(&mut self, endpoint_id: u64, rights: EndpointRights, badge: u64) -> u64 {
        let cap_id = self.alloc_cap_id();
        let cap = IpcCapability::new_endpoint(cap_id, endpoint_id, rights, badge, self.owner_pid);
        self.insert(cap)
    }
    
    /// 创建通知能力
    pub fn create_notification_cap(&mut self, notification_id: u64, rights: NotificationRights, signal_mask: u64) -> u64 {
        let cap_id = self.alloc_cap_id();
        let cap = IpcCapability::new_notification(cap_id, notification_id, rights, signal_mask, self.owner_pid);
        self.insert(cap)
    }
    
    /// 创建共享内存能力
    pub fn create_shared_memory_cap(&mut self, region_id: u64, rights: SharedMemoryRights, size: usize) -> u64 {
        let cap_id = self.alloc_cap_id();
        let cap = IpcCapability::new_shared_memory(cap_id, region_id, rights, size, self.owner_pid);
        self.insert(cap)
    }
    
    /// 查找能力
    pub fn lookup(&self, cap_id: u64) -> Option<&IpcCapability> {
        self.capabilities.get(&cap_id).filter(|c| c.valid)
    }
    
    /// 删除能力
    pub fn remove(&mut self, cap_id: u64) -> Option<IpcCapability> {
        self.capabilities.remove(&cap_id)
    }
    
    /// 撤销能力
    pub fn revoke(&mut self, cap_id: u64) -> Result<(), &'static str> {
        let cap = self.capabilities.get_mut(&cap_id).ok_or("Capability not found")?;
        cap.revoke();
        Ok(())
    }
    
    /// 派生能力到另一个进程
    pub fn derive_to(&self, cap_id: u64, target_cspace: &mut IpcCSpace) -> Result<u64, &'static str> {
        let cap = self.lookup(cap_id).ok_or("Capability not found")?;
        
        // 检查授权权限
        match &cap.cap_type {
            IpcCapabilityType::Endpoint(ep) if !ep.rights.can_grant => {
                return Err("No grant permission");
            }
            _ => {}
        }
        
        let new_cap_id = target_cspace.alloc_cap_id();
        let derived = cap.derive(new_cap_id, target_cspace.owner_pid)
            .ok_or("Cannot derive capability")?;
        
        target_cspace.insert(derived);
        Ok(new_cap_id)
    }
}

/// 能力验证器
pub struct CapabilityVerifier;

impl CapabilityVerifier {
    /// 验证发送权限
    pub fn verify_send(cspace: &IpcCSpace, cap_id: u64, endpoint_id: u64) -> Result<u64, &'static str> {
        let cap = cspace.lookup(cap_id).ok_or("Invalid capability")?;
        
        match &cap.cap_type {
            IpcCapabilityType::Endpoint(ep) => {
                if ep.endpoint_id != endpoint_id {
                    return Err("Endpoint mismatch");
                }
                if !ep.rights.can_send {
                    return Err("No send permission");
                }
                Ok(ep.badge)
            }
            _ => Err("Not an endpoint capability"),
        }
    }
    
    /// 验证接收权限
    pub fn verify_recv(cspace: &IpcCSpace, cap_id: u64, endpoint_id: u64) -> Result<(), &'static str> {
        let cap = cspace.lookup(cap_id).ok_or("Invalid capability")?;
        
        match &cap.cap_type {
            IpcCapabilityType::Endpoint(ep) => {
                if ep.endpoint_id != endpoint_id {
                    return Err("Endpoint mismatch");
                }
                if !ep.rights.can_recv {
                    return Err("No receive permission");
                }
                Ok(())
            }
            _ => Err("Not an endpoint capability"),
        }
    }
    
    /// 验证信号权限
    pub fn verify_signal(cspace: &IpcCSpace, cap_id: u64, notification_id: u64, signals: u64) -> Result<u64, &'static str> {
        let cap = cspace.lookup(cap_id).ok_or("Invalid capability")?;
        
        match &cap.cap_type {
            IpcCapabilityType::Notification(n) => {
                if n.notification_id != notification_id {
                    return Err("Notification mismatch");
                }
                if !n.rights.can_signal {
                    return Err("No signal permission");
                }
                // 应用信号掩码
                Ok(signals & n.signal_mask)
            }
            _ => Err("Not a notification capability"),
        }
    }
    
    /// 验证等待权限
    pub fn verify_wait(cspace: &IpcCSpace, cap_id: u64, notification_id: u64) -> Result<u64, &'static str> {
        let cap = cspace.lookup(cap_id).ok_or("Invalid capability")?;
        
        match &cap.cap_type {
            IpcCapabilityType::Notification(n) => {
                if n.notification_id != notification_id {
                    return Err("Notification mismatch");
                }
                if !n.rights.can_wait {
                    return Err("No wait permission");
                }
                Ok(n.signal_mask)
            }
            _ => Err("Not a notification capability"),
        }
    }
    
    /// 验证共享内存映射权限
    pub fn verify_map(cspace: &IpcCSpace, cap_id: u64, region_id: u64, write: bool) -> Result<usize, &'static str> {
        let cap = cspace.lookup(cap_id).ok_or("Invalid capability")?;
        
        match &cap.cap_type {
            IpcCapabilityType::SharedMemory(shm) => {
                if shm.region_id != region_id {
                    return Err("Region mismatch");
                }
                if !shm.rights.can_map {
                    return Err("No map permission");
                }
                if write && !shm.rights.can_write {
                    return Err("No write permission");
                }
                if !shm.rights.can_read {
                    return Err("No read permission");
                }
                Ok(shm.size)
            }
            _ => Err("Not a shared memory capability"),
        }
    }
}

/// 全局 IPC 能力空间表
static IPC_CSPACES: SpinLock<BTreeMap<u64, IpcCSpace>> = SpinLock::new(BTreeMap::new());

/// 初始化进程的 IPC 能力空间
pub fn init_process_cspace(pid: u64) {
    let cspace = IpcCSpace::new(pid);
    IPC_CSPACES.lock().insert(pid, cspace);
}

/// 销毁进程的 IPC 能力空间
pub fn destroy_process_cspace(pid: u64) {
    IPC_CSPACES.lock().remove(&pid);
}

/// 创建端点能力
pub fn create_endpoint_capability(pid: u64, endpoint_id: u64, rights: EndpointRights, badge: u64) -> Result<u64, &'static str> {
    let mut cspaces = IPC_CSPACES.lock();
    let cspace = cspaces.get_mut(&pid).ok_or("Process CSpace not found")?;
    Ok(cspace.create_endpoint_cap(endpoint_id, rights, badge))
}

/// 验证发送
pub fn verify_send_capability(pid: u64, cap_id: u64, endpoint_id: u64) -> Result<u64, &'static str> {
    let cspaces = IPC_CSPACES.lock();
    let cspace = cspaces.get(&pid).ok_or("Process CSpace not found")?;
    CapabilityVerifier::verify_send(cspace, cap_id, endpoint_id)
}

/// 验证接收
pub fn verify_recv_capability(pid: u64, cap_id: u64, endpoint_id: u64) -> Result<(), &'static str> {
    let cspaces = IPC_CSPACES.lock();
    let cspace = cspaces.get(&pid).ok_or("Process CSpace not found")?;
    CapabilityVerifier::verify_recv(cspace, cap_id, endpoint_id)
}

/// 初始化
pub fn init() {
    crate::println!("[cap_ipc] IPC capability integration initialized");
}

/// IPC 能力子系统自测试
pub fn self_test() {
    crate::println!("[cap_ipc] Running self-test...");
    
    // 测试 EndpointRights 常量
    let send_only = EndpointRights::SEND_ONLY;
    assert!(send_only.can_send);
    assert!(!send_only.can_recv);
    
    let recv_only = EndpointRights::RECV_ONLY;
    assert!(!recv_only.can_send);
    assert!(recv_only.can_recv);
    
    let full_ep = EndpointRights::FULL;
    assert!(full_ep.can_send);
    assert!(full_ep.can_recv);
    assert!(full_ep.can_grant);
    assert!(full_ep.can_revoke);
    crate::println!("[cap_ipc] EndpointRights tests passed");
    
    // 测试 NotificationRights 常量
    let signal_only = NotificationRights::SIGNAL_ONLY;
    assert!(signal_only.can_signal);
    assert!(!signal_only.can_wait);
    
    let wait_only = NotificationRights::WAIT_ONLY;
    assert!(!wait_only.can_signal);
    assert!(wait_only.can_wait);
    assert!(wait_only.can_poll);
    
    let full_notif = NotificationRights::FULL;
    assert!(full_notif.can_signal);
    assert!(full_notif.can_wait);
    assert!(full_notif.can_poll);
    crate::println!("[cap_ipc] NotificationRights tests passed");
    
    // 测试 SharedMemoryRights 常量
    let read_only = SharedMemoryRights::READ_ONLY;
    assert!(read_only.can_read);
    assert!(!read_only.can_write);
    assert!(read_only.can_map);
    
    let read_write = SharedMemoryRights::READ_WRITE;
    assert!(read_write.can_read);
    assert!(read_write.can_write);
    
    let full_shm = SharedMemoryRights::FULL;
    assert!(full_shm.can_read);
    assert!(full_shm.can_write);
    assert!(full_shm.can_exec);
    assert!(full_shm.can_map);
    crate::println!("[cap_ipc] SharedMemoryRights tests passed");
    
    // 测试 IpcCapability 创建方法
    let ep_cap = IpcCapability::new_endpoint(1, 100, EndpointRights::FULL, 42, 1000);
    assert_eq!(ep_cap.cap_id, 1);
    assert_eq!(ep_cap.owner_pid, 1000);
    assert!(ep_cap.valid);
    assert!(ep_cap.can_send());
    assert!(ep_cap.can_recv());
    
    let notif_cap = IpcCapability::new_notification(2, 200, NotificationRights::FULL, 0xFF, 1000);
    assert_eq!(notif_cap.cap_id, 2);
    assert!(notif_cap.can_send()); // signal
    assert!(notif_cap.can_recv()); // wait
    
    let shm_cap = IpcCapability::new_shared_memory(3, 300, SharedMemoryRights::FULL, 4096, 1000);
    assert_eq!(shm_cap.cap_id, 3);
    assert!(!shm_cap.can_send()); // 共享内存不支持 send
    assert!(!shm_cap.can_recv()); // 共享内存不支持 recv
    crate::println!("[cap_ipc] IpcCapability creation tests passed");
    
    // 测试 IpcCapabilityType 变体
    match ep_cap.cap_type {
        IpcCapabilityType::Endpoint(ep) => {
            assert_eq!(ep.endpoint_id, 100);
            assert_eq!(ep.badge, 42);
        }
        _ => panic!("Expected Endpoint"),
    }
    
    match notif_cap.cap_type {
        IpcCapabilityType::Notification(n) => {
            assert_eq!(n.notification_id, 200);
            assert_eq!(n.signal_mask, 0xFF);
        }
        _ => panic!("Expected Notification"),
    }
    
    match shm_cap.cap_type {
        IpcCapabilityType::SharedMemory(shm) => {
            assert_eq!(shm.region_id, 300);
            assert_eq!(shm.size, 4096);
        }
        _ => panic!("Expected SharedMemory"),
    }
    crate::println!("[cap_ipc] IpcCapabilityType tests passed");
    
    // 测试 IpcCapability derive 和 revoke
    let mut cap = IpcCapability::new_endpoint(10, 100, EndpointRights::FULL, 42, 1000);
    let derived = cap.derive(11, 2000);
    assert!(derived.is_some());
    let derived = derived.unwrap();
    assert_eq!(derived.cap_id, 11);
    assert_eq!(derived.owner_pid, 2000);
    // 派生的能力不能有授权权限
    match derived.cap_type {
        IpcCapabilityType::Endpoint(ep) => {
            assert!(!ep.rights.can_grant);
            assert!(!ep.rights.can_revoke);
        }
        _ => panic!("Expected Endpoint"),
    }
    
    cap.revoke();
    assert!(!cap.valid);
    let derived_invalid = cap.derive(12, 3000);
    assert!(derived_invalid.is_none());
    crate::println!("[cap_ipc] IpcCapability derive/revoke tests passed");
    
    // 测试 IpcCSpace
    let mut cspace = IpcCSpace::new(1000);
    
    // 创建各种能力
    let ep_cap_id = cspace.create_endpoint_cap(100, EndpointRights::FULL, 42);
    let notif_cap_id = cspace.create_notification_cap(200, NotificationRights::FULL, 0xFF);
    let shm_cap_id = cspace.create_shared_memory_cap(300, SharedMemoryRights::FULL, 4096);
    
    // 查找能力
    assert!(cspace.lookup(ep_cap_id).is_some());
    assert!(cspace.lookup(notif_cap_id).is_some());
    assert!(cspace.lookup(shm_cap_id).is_some());
    assert!(cspace.lookup(9999).is_none());
    
    // 插入能力
    let manual_cap = IpcCapability::new_endpoint(100, 500, EndpointRights::SEND_ONLY, 0, 1000);
    let inserted_id = cspace.insert(manual_cap);
    assert_eq!(inserted_id, 100);
    
    // 撤销能力
    let revoke_result = cspace.revoke(ep_cap_id);
    assert!(revoke_result.is_ok());
    assert!(cspace.lookup(ep_cap_id).is_none()); // 撤销后查找不到
    
    // 删除能力
    let removed = cspace.remove(notif_cap_id);
    assert!(removed.is_some());
    assert!(cspace.lookup(notif_cap_id).is_none());
    
    // 测试 derive_to
    let mut target_cspace = IpcCSpace::new(2000);
    // 创建一个有授权权限的能力
    let grant_cap_id = cspace.create_endpoint_cap(600, EndpointRights::FULL, 0);
    let derive_result = cspace.derive_to(grant_cap_id, &mut target_cspace);
    assert!(derive_result.is_ok());
    crate::println!("[cap_ipc] IpcCSpace tests passed");
    
    // 测试 CapabilityVerifier
    let mut verify_cspace = IpcCSpace::new(3000);
    
    // 创建用于验证的能力
    let send_cap_id = verify_cspace.create_endpoint_cap(700, EndpointRights::SEND_ONLY, 123);
    let recv_cap_id = verify_cspace.create_endpoint_cap(700, EndpointRights::RECV_ONLY, 0);
    let signal_cap_id = verify_cspace.create_notification_cap(800, NotificationRights::SIGNAL_ONLY, 0xF0);
    let wait_cap_id = verify_cspace.create_notification_cap(800, NotificationRights::WAIT_ONLY, 0xFF);
    let map_cap_id = verify_cspace.create_shared_memory_cap(900, SharedMemoryRights::READ_WRITE, 8192);
    
    // verify_send
    let send_result = CapabilityVerifier::verify_send(&verify_cspace, send_cap_id, 700);
    assert!(send_result.is_ok());
    assert_eq!(send_result.unwrap(), 123); // badge
    
    let send_fail = CapabilityVerifier::verify_send(&verify_cspace, recv_cap_id, 700);
    assert!(send_fail.is_err());
    
    // verify_recv
    let recv_result = CapabilityVerifier::verify_recv(&verify_cspace, recv_cap_id, 700);
    assert!(recv_result.is_ok());
    
    let recv_fail = CapabilityVerifier::verify_recv(&verify_cspace, send_cap_id, 700);
    assert!(recv_fail.is_err());
    
    // verify_signal
    let signal_result = CapabilityVerifier::verify_signal(&verify_cspace, signal_cap_id, 800, 0xFF);
    assert!(signal_result.is_ok());
    assert_eq!(signal_result.unwrap(), 0xF0); // masked
    
    // verify_wait
    let wait_result = CapabilityVerifier::verify_wait(&verify_cspace, wait_cap_id, 800);
    assert!(wait_result.is_ok());
    assert_eq!(wait_result.unwrap(), 0xFF); // signal_mask
    
    // verify_map
    let map_result = CapabilityVerifier::verify_map(&verify_cspace, map_cap_id, 900, true);
    assert!(map_result.is_ok());
    assert_eq!(map_result.unwrap(), 8192); // size
    
    let map_readonly = verify_cspace.create_shared_memory_cap(901, SharedMemoryRights::READ_ONLY, 1024);
    let map_write_fail = CapabilityVerifier::verify_map(&verify_cspace, map_readonly, 901, true);
    assert!(map_write_fail.is_err()); // 只读不能写
    crate::println!("[cap_ipc] CapabilityVerifier tests passed");
    
    // 测试全局函数
    init_process_cspace(5000);
    
    let create_result = create_endpoint_capability(5000, 1000, EndpointRights::FULL, 999);
    assert!(create_result.is_ok());
    let cap_id = create_result.unwrap();
    
    let verify_send_result = verify_send_capability(5000, cap_id, 1000);
    assert!(verify_send_result.is_ok());
    
    let verify_recv_result = verify_recv_capability(5000, cap_id, 1000);
    assert!(verify_recv_result.is_ok());
    
    destroy_process_cspace(5000);
    
    // 验证销毁后无法访问
    let after_destroy = verify_send_capability(5000, cap_id, 1000);
    assert!(after_destroy.is_err());
    
    // 测试 IPC_CSPACES 静态变量（通过全局函数间接使用）
    let _cspaces_lock = IPC_CSPACES.lock();
    drop(_cspaces_lock);
    crate::println!("[cap_ipc] Global functions tests passed");
    
    crate::println!("[cap_ipc] Self-test passed!");
}
