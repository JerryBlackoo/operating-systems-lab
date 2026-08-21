//! 能力系统（Capability-based Security）
//!
//! 实现基于能力的访问控制，所有资源访问都需要持有相应的能力令牌。
//! 能力不可伪造，只能通过派生获得，且权限单调递减。
//!
//! # 双层安全检查
//! 本系统集成了 CHERI 风格的硬件能力模拟，提供双层安全保护：
//! - 第一层：传统能力系统（对象级访问控制）
//! - 第二层：CHERI 能力（内存级边界和权限检查）

use alloc::vec::Vec;
use alloc::vec;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use crate::sync::SpinLock;
use crate::cheri::{CheriCapability, CheriPermissions, CheriViolation};

/// 能力类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CapType {
    /// 空能力
    Null = 0,
    /// 内存能力
    Memory = 1,
    /// 线程能力
    Thread = 2,
    /// 端点能力（IPC）
    Endpoint = 3,
    /// 通知能力
    Notification = 4,
    /// 页表能力
    PageTable = 5,
    /// 中断能力
    Interrupt = 6,
    /// 设备能力
    Device = 7,
    /// CNode 能力（能力空间节点）
    CNode = 8,
}

bitflags::bitflags! {
    /// 能力权限标志
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct CapRights: u32 {
        /// 读权限
        const READ = 1 << 0;
        /// 写权限
        const WRITE = 1 << 1;
        /// 执行权限
        const EXECUTE = 1 << 2;
        /// 授权权限（可派生）
        const GRANT = 1 << 3;
        /// 撤销权限
        const REVOKE = 1 << 4;
        /// 全部权限
        const ALL = Self::READ.bits() | Self::WRITE.bits() |
                    Self::EXECUTE.bits() | Self::GRANT.bits() | Self::REVOKE.bits();
    }
}

/// 能力标识符
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CapId(pub u64);

impl CapId {
    /// 生成新的能力 ID
    pub fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }

    /// 空能力 ID
    pub const fn null() -> Self {
        Self(0)
    }

    /// 是否为空
    pub fn is_null(&self) -> bool {
        self.0 == 0
    }

    /// 获取内部值
    pub fn value(&self) -> u64 {
        self.0
    }
}

/// 能力结构
#[derive(Debug, Clone)]
pub struct Capability {
    /// 能力 ID
    pub id: CapId,
    /// 能力类型
    pub cap_type: CapType,
    /// 权限
    pub rights: CapRights,
    /// 对象引用（内核对象指针或 ID）
    pub object: usize,
    /// 父能力 ID（用于派生链追踪）
    pub parent: CapId,
    /// 徽章（用于 IPC 识别）
    pub badge: u64,
    /// CHERI 能力（用于内存级保护）
    pub cheri_cap: Option<CheriCapability>,
}

impl Capability {
    /// 创建新能力
    pub fn new(cap_type: CapType, rights: CapRights, object: usize) -> Self {
        Self {
            id: CapId::new(),
            cap_type,
            rights,
            object,
            parent: CapId::null(),
            badge: 0,
            cheri_cap: None,
        }
    }

    /// 创建带 CHERI 保护的内存能力
    pub fn new_memory(base: usize, length: usize, rights: CapRights) -> Self {
        let cheri_perms = Self::rights_to_cheri_perms(rights);
        let cheri_cap = CheriCapability::new(base as u64, length as u64, cheri_perms);

        Self {
            id: CapId::new(),
            cap_type: CapType::Memory,
            rights,
            object: base,
            parent: CapId::null(),
            badge: 0,
            cheri_cap: Some(cheri_cap),
        }
    }

    /// 将 CapRights 转换为 CheriPermissions
    fn rights_to_cheri_perms(rights: CapRights) -> CheriPermissions {
        let mut perms = CheriPermissions::empty();
        if rights.contains(CapRights::READ) {
            perms |= CheriPermissions::LOAD;
        }
        if rights.contains(CapRights::WRITE) {
            perms |= CheriPermissions::STORE;
        }
        if rights.contains(CapRights::EXECUTE) {
            perms |= CheriPermissions::EXECUTE;
        }
        if rights.contains(CapRights::GRANT) {
            perms |= CheriPermissions::STORE_CAP;
        }
        perms
    }

    /// 派生能力（权限衰减）
    pub fn derive(&self, new_rights: CapRights) -> Option<Self> {
        // 检查是否有授权权限
        if !self.rights.contains(CapRights::GRANT) {
            return None;
        }

        // 检查请求的权限是否是父权限的子集（单调性原则）
        // 如果请求的权限包含父能力没有的权限，则拒绝派生
        if !self.rights.contains(new_rights) {
            return None;
        }

        // 新权限必须是原权限的子集
        let derived_rights = self.rights & new_rights;

        // 形式化验证检查：验证权限单调性
        #[cfg(debug_assertions)]
        {
            use crate::formal::capability_spec::Rights;
            let parent_spec_rights = Rights(self.rights.bits());
            let child_spec_rights = Rights(derived_rights.bits());
            debug_assert!(
                child_spec_rights.is_subset_of(&parent_spec_rights),
                "Formal verification failed: derived rights must be subset of parent rights"
            );
        }

        // 如果有 CHERI 能力，也需要派生
        let derived_cheri = if let Some(ref cheri) = self.cheri_cap {
            let new_perms = Self::rights_to_cheri_perms(derived_rights);
            cheri.derive(cheri.base(), cheri.length(), new_perms).ok()
        } else {
            None
        };

        Some(Self {
            id: CapId::new(),
            cap_type: self.cap_type,
            rights: derived_rights,
            object: self.object,
            parent: self.id,
            badge: self.badge,
            cheri_cap: derived_cheri,
        })
    }

    /// 派生子区域能力（用于内存能力）
    pub fn derive_subregion(&self, offset: usize, length: usize, new_rights: CapRights) -> Option<Self> {
        if !self.rights.contains(CapRights::GRANT) {
            return None;
        }

        // 检查请求的权限是否是父权限的子集（单调性原则）
        if !self.rights.contains(new_rights) {
            return None;
        }

        let derived_rights = self.rights & new_rights;

        // 形式化验证检查：验证权限单调性
        #[cfg(debug_assertions)]
        {
            use crate::formal::capability_spec::Rights;
            let parent_spec_rights = Rights(self.rights.bits());
            let child_spec_rights = Rights(derived_rights.bits());
            debug_assert!(
                child_spec_rights.is_subset_of(&parent_spec_rights),
                "Formal verification failed: derived rights must be subset of parent rights"
            );
        }

        // 派生 CHERI 子能力
        let derived_cheri = if let Some(ref cheri) = self.cheri_cap {
            // 形式化验证检查：验证边界单调性
            #[cfg(debug_assertions)]
            {
                use crate::formal::memory_spec::MemoryRegionSpec;
                let parent_region = MemoryRegionSpec {
                    base: cheri.base(),
                    size: cheri.length(),
                    allocated: true,
                    owner: 0,
                };
                debug_assert!(
                    parent_region.contains(cheri.base() + offset as u64, length as u64),
                    "Formal verification failed: subregion must be within parent bounds"
                );
            }

            let new_base = cheri.base() + offset as u64;
            let new_perms = Self::rights_to_cheri_perms(derived_rights);
            cheri.derive(new_base, length as u64, new_perms).ok()
        } else {
            None
        };

        Some(Self {
            id: CapId::new(),
            cap_type: self.cap_type,
            rights: derived_rights,
            object: self.object + offset,
            parent: self.id,
            badge: self.badge,
            cheri_cap: derived_cheri,
        })
    }

    /// 检查权限
    pub fn check(&self, required: CapRights) -> bool {
        self.rights.contains(required)
    }

    /// 双层安全检查：检查内存访问
    pub fn check_memory_access(&self, offset: usize, size: usize, write: bool) -> Result<(), CapError> {
        // 第一层：传统能力权限检查
        let required = if write { CapRights::WRITE } else { CapRights::READ };
        if !self.rights.contains(required) {
            return Err(CapError::PermissionDenied);
        }

        // 第二层：CHERI 边界和权限检查
        if let Some(ref cheri) = self.cheri_cap {
            // 形式化验证检查：验证内存访问边界
            #[cfg(debug_assertions)]
            {
                let access_valid = crate::formal::verify_memory_access(
                    cheri.base(),
                    cheri.length(),
                    cheri.base() + offset as u64,
                    size as u64,
                );
                debug_assert!(
                    access_valid,
                    "Formal verification failed: memory access out of bounds"
                );
            }

            if write {
                cheri.check_store(offset as u64, size as u64)
                    .map_err(|v| CapError::CheriViolation(v))?;
            } else {
                cheri.check_load(offset as u64, size as u64)
                    .map_err(|v| CapError::CheriViolation(v))?;
            }
        }

        Ok(())
    }

    /// 获取 CHERI 能力
    pub fn cheri(&self) -> Option<&CheriCapability> {
        self.cheri_cap.as_ref()
    }

    /// 设置 CHERI 能力
    pub fn set_cheri(&mut self, cheri: CheriCapability) {
        self.cheri_cap = Some(cheri);
    }
}

/// 能力错误
#[derive(Debug, Clone)]
pub enum CapError {
    /// 权限不足
    PermissionDenied,
    /// 无效能力
    InvalidCapability,
    /// 槽已满
    SlotFull,
    /// 无效槽
    InvalidSlot,
    /// CHERI 违规
    CheriViolation(CheriViolation),
}

/// 能力槽
#[derive(Debug, Clone)]
pub struct CapSlot {
    /// 槽中的能力
    pub cap: Option<Capability>,
}

impl CapSlot {
    pub const fn empty() -> Self {
        Self { cap: None }
    }

    pub fn new(cap: Capability) -> Self {
        Self { cap: Some(cap) }
    }
}

/// 能力空间（CSpace）
/// 使用 Radix Tree 结构管理能力槽
pub struct CSpace {
    /// 能力槽映射
    slots: BTreeMap<usize, CapSlot>,
    /// 下一个可用槽索引
    next_slot: usize,
    /// 最大槽数
    max_slots: usize,
}

impl CSpace {
    /// 创建新的能力空间
    pub fn new(max_slots: usize) -> Self {
        Self {
            slots: BTreeMap::new(),
            next_slot: 0,
            max_slots,
        }
    }

    /// 分配槽并插入能力
    pub fn insert(&mut self, cap: Capability) -> Option<usize> {
        if self.next_slot >= self.max_slots {
            return None;
        }
        let slot_idx = self.next_slot;
        self.slots.insert(slot_idx, CapSlot::new(cap));
        self.next_slot += 1;
        Some(slot_idx)
    }

    /// 在指定槽插入能力
    pub fn insert_at(&mut self, slot: usize, cap: Capability) -> bool {
        if slot >= self.max_slots {
            return false;
        }
        self.slots.insert(slot, CapSlot::new(cap));
        true
    }

    /// 获取能力
    pub fn get(&self, slot: usize) -> Option<&Capability> {
        self.slots.get(&slot).and_then(|s| s.cap.as_ref())
    }

    /// 获取可变能力
    #[allow(dead_code)]
    pub fn get_mut(&mut self, slot: usize) -> Option<&mut Capability> {
        self.slots.get_mut(&slot).and_then(|s| s.cap.as_mut())
    }

    /// 删除能力
    pub fn remove(&mut self, slot: usize) -> Option<Capability> {
        self.slots.remove(&slot).and_then(|s| s.cap)
    }

    /// 派生能力到新槽
    pub fn derive(&mut self, src_slot: usize, new_rights: CapRights) -> Option<usize> {
        let src_cap = self.get(src_slot)?;
        let derived = src_cap.derive(new_rights)?;
        self.insert(derived)
    }
}

/// 全局能力管理器
static CAP_MANAGER: SpinLock<Option<CapManager>> = SpinLock::new(None);

/// 能力管理器
pub struct CapManager {
    /// 所有能力的派生关系（用于撤销）
    derivation_tree: BTreeMap<CapId, Vec<CapId>>,
}

impl CapManager {
    fn new() -> Self {
        Self {
            derivation_tree: BTreeMap::new(),
        }
    }

    /// 记录派生关系
    pub fn record_derivation(&mut self, parent: CapId, child: CapId) {
        self.derivation_tree
            .entry(parent)
            .or_insert_with(Vec::new)
            .push(child);
    }

    /// 获取所有派生能力
    pub fn get_descendants(&self, cap_id: CapId) -> Vec<CapId> {
        let mut result = Vec::new();
        let mut stack = vec![cap_id];

        while let Some(id) = stack.pop() {
            if let Some(children) = self.derivation_tree.get(&id) {
                for &child in children {
                    result.push(child);
                    stack.push(child);
                }
            }
        }
        result
    }

    /// 直接子能力数（供 `sys_cap_info().child_count` 真实化使用）。
    pub fn child_count(&self, cap_id: CapId) -> usize {
        self.derivation_tree.get(&cap_id).map(|v| v.len()).unwrap_or(0)
    }
}

/// 公开 helper：查指定 cap 的直接子能力数。若 CAP_MANAGER 未初始化返 0。
pub fn cap_child_count(cap_id: CapId) -> u32 {
    let guard = CAP_MANAGER.lock();
    guard.as_ref().map(|m| m.child_count(cap_id) as u32).unwrap_or(0)
}

/// 初始化能力系统
pub fn init() {
    let mut manager = CAP_MANAGER.lock();
    *manager = Some(CapManager::new());
}

/// 创建能力
pub fn cap_create(cap_type: CapType, rights: CapRights, object: usize) -> Capability {
    Capability::new(cap_type, rights, object)
}

/// 验证能力权限
pub fn cap_check(cap: &Capability, required: CapRights) -> bool {
    cap.check(required)
}

/// 能力统计信息
#[derive(Default)]
pub struct CapStats {
    /// 总能力数
    pub total: usize,
    /// 活跃能力数
    pub active: usize,
    /// 已撤销能力数
    pub revoked: usize,
    /// 派生次数
    pub delegations: usize,
}

/// 全局能力统计
static CAP_STATS: SpinLock<CapStats> = SpinLock::new(CapStats {
    total: 0,
    active: 0,
    revoked: 0,
    delegations: 0,
});

/// 获取能力统计
pub fn get_stats() -> CapStats {
    let stats = CAP_STATS.lock();
    CapStats {
        total: stats.total,
        active: stats.active,
        revoked: stats.revoked,
        delegations: stats.delegations,
    }
}

/// 记录能力创建
pub fn record_cap_create() {
    let mut stats = CAP_STATS.lock();
    stats.total += 1;
    stats.active += 1;
}

/// 记录能力撤销
pub fn record_cap_revoke() {
    let mut stats = CAP_STATS.lock();
    stats.active = stats.active.saturating_sub(1);
    stats.revoked += 1;
}

/// 记录能力派生
pub fn record_cap_delegate() {
    let mut stats = CAP_STATS.lock();
    stats.delegations += 1;
}

/// 能力信息（供 ProcFS 使用）
#[derive(Clone)]
pub struct CapInfo {
    pub id: u64,
    pub cap_type: CapType,
    pub rights: CapRights,
    pub generation: u64,
    pub ref_count: usize,
}

impl CapType {
    /// 获取类型名称
    pub fn name(&self) -> &'static str {
        match self {
            CapType::Null => "Null",
            CapType::Memory => "Memory",
            CapType::Thread => "Thread",
            CapType::Endpoint => "Endpoint",
            CapType::Notification => "Notification",
            CapType::PageTable => "PageTable",
            CapType::Interrupt => "IRQ",
            CapType::Device => "Device",
            CapType::CNode => "CNode",
        }
    }
}

impl CapRights {
    /// 获取权限字符串
    pub fn as_str(&self) -> &'static str {
        match (self.contains(CapRights::READ),
               self.contains(CapRights::WRITE),
               self.contains(CapRights::EXECUTE)) {
            (true, true, true) => "RWX",
            (true, true, false) => "RW",
            (true, false, true) => "RX",
            (true, false, false) => "R",
            (false, true, true) => "WX",
            (false, true, false) => "W",
            (false, false, true) => "X",
            (false, false, false) => "-",
        }
    }
}

/// 全局能力列表（供 ProcFS 迭代）
static GLOBAL_CAPS: SpinLock<Vec<CapInfo>> = SpinLock::new(Vec::new());

/// 注册能力到全局列表
pub fn register_cap(cap: &Capability) {
    let mut caps = GLOBAL_CAPS.lock();
    caps.push(CapInfo {
        id: cap.id.0,
        cap_type: cap.cap_type,
        rights: cap.rights,
        generation: 0,
        ref_count: 1,
    });
    record_cap_create();
}

/// 迭代所有能力
pub fn iter_capabilities() -> Vec<CapInfo> {
    GLOBAL_CAPS.lock().clone()
}


/// 能力系统自测试
pub fn self_test() {
    crate::println!("[capability] Running self-test...");

    // 测试 CapId
    let id1 = CapId::new();
    let id2 = CapId::new();
    assert!(!id1.is_null());
    assert!(!id2.is_null());
    assert!(id1 != id2);

    let null_id = CapId::null();
    assert!(null_id.is_null());
    crate::println!("[capability] CapId tests passed");

    // 测试 CapType
    let types = [
        CapType::Null, CapType::Memory, CapType::Thread, CapType::Endpoint,
        CapType::Notification, CapType::PageTable, CapType::Interrupt,
        CapType::Device, CapType::CNode,
    ];
    for cap_type in types {
        let name = cap_type.name();
        crate::println!("[capability] CapType::{:?} = {}", cap_type, name);
    }

    // 测试 CapRights
    let rights = CapRights::READ | CapRights::WRITE;
    assert!(rights.contains(CapRights::READ));
    assert!(rights.contains(CapRights::WRITE));
    assert!(!rights.contains(CapRights::EXECUTE));
    let rights_str = rights.as_str();
    crate::println!("[capability] Rights: {}", rights_str);

    // 测试 Capability 创建
    let cap = Capability::new(CapType::Memory, CapRights::ALL, 0x1000);
    assert!(cap.check(CapRights::READ));
    assert!(cap.check(CapRights::WRITE));
    assert!(cap.check(CapRights::EXECUTE));
    crate::println!("[capability] Capability creation passed");

    // 测试带 CHERI 的内存能力
    let mem_cap = Capability::new_memory(0x2000, 0x1000, CapRights::READ | CapRights::WRITE);
    assert!(mem_cap.cheri().is_some());

    // 测试 check_memory_access
    let result = mem_cap.check_memory_access(0, 0x100, false);
    assert!(result.is_ok());
    let result = mem_cap.check_memory_access(0, 0x100, true);
    assert!(result.is_ok());
    crate::println!("[capability] Memory access check passed");

    // 测试能力派生
    let parent_cap = Capability::new(CapType::Thread, CapRights::ALL, 0x3000);
    if let Some(child_cap) = parent_cap.derive(CapRights::READ | CapRights::WRITE) {
        assert!(child_cap.check(CapRights::READ));
        assert!(child_cap.check(CapRights::WRITE));
        assert!(!child_cap.check(CapRights::EXECUTE));
        assert!(child_cap.parent == parent_cap.id);
        crate::println!("[capability] Capability derivation passed");
    }

    // 测试子区域派生
    let mem_parent = Capability::new_memory(0x4000, 0x2000, CapRights::ALL);
    if let Some(sub_cap) = mem_parent.derive_subregion(0x100, 0x500, CapRights::READ) {
        assert!(sub_cap.check(CapRights::READ));
        assert!(!sub_cap.check(CapRights::WRITE));
        crate::println!("[capability] Subregion derivation passed");
    }

    // 测试 set_cheri
    let mut cap_with_cheri = Capability::new(CapType::Memory, CapRights::READ, 0x5000);
    let cheri = crate::cheri::CheriCapability::new(0x5000, 0x1000, crate::cheri::CheriPermissions::LOAD);
    cap_with_cheri.set_cheri(cheri);
    assert!(cap_with_cheri.cheri().is_some());

    // 测试 CapSlot
    let slot = CapSlot::empty();
    assert!(slot.cap.is_none());
    let slot2 = CapSlot::new(cap.clone());
    assert!(slot2.cap.is_some());

    // 测试 CSpace
    let mut cspace = CSpace::new(16);
    let slot_idx = cspace.insert(cap.clone());
    assert!(slot_idx.is_some());

    let retrieved = cspace.get(slot_idx.unwrap());
    assert!(retrieved.is_some());

    let inserted = cspace.insert_at(5, parent_cap.clone());
    assert!(inserted);

    let derived_slot = cspace.derive(5, CapRights::READ);
    assert!(derived_slot.is_some());

    let removed = cspace.remove(slot_idx.unwrap());
    assert!(removed.is_some());
    crate::println!("[capability] CSpace tests passed");

    // 测试 CapManager
    {
        let mut manager = CAP_MANAGER.lock();
        if let Some(ref mut mgr) = *manager {
            let parent_id = CapId::new();
            let child_id = CapId::new();
            mgr.record_derivation(parent_id, child_id);
            let descendants = mgr.get_descendants(parent_id);
            assert!(descendants.contains(&child_id));
            crate::println!("[capability] CapManager tests passed");
        }
    }

    // 测试全局函数
    let new_cap = cap_create(CapType::Endpoint, CapRights::READ | CapRights::WRITE, 0x6000);
    assert!(cap_check(&new_cap, CapRights::READ));

    // 测试统计
    register_cap(&new_cap);
    record_cap_delegate();
    record_cap_revoke();

    let stats = get_stats();
    crate::println!("[capability] Stats: total={}, active={}, revoked={}, delegations={}",
        stats.total, stats.active, stats.revoked, stats.delegations);

    // 测试迭代
    let caps = iter_capabilities();
    crate::println!("[capability] Registered capabilities: {}", caps.len());

    // 测试 CapError 变体
    let _errors = [
        CapError::PermissionDenied,
        CapError::InvalidCapability,
        CapError::SlotFull,
        CapError::InvalidSlot,
        CapError::CheriViolation(crate::cheri::CheriViolation::InvalidTag),
    ];

    crate::println!("[capability] Self-test passed!");
}
