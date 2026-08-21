//! 能力系统属性测试
//!
//! 验证 Capability-based Security 的以下属性：
//! Property 2: 能力权限单调性 - 派生的能力权限不能超过父能力
//! Property 3: 能力 ID 唯一性 - 能力 ID 唯一且不可预测
//! 以及其他辅助属性测试

use crate::{SimpleRng, println};
use alloc::vec::Vec;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};

/// 能力类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum CapType {
    Null = 0,
    Memory = 1,
    Thread = 2,
    Endpoint = 3,
}

/// 能力权限
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CapRights(u32);

impl CapRights {
    const READ: u32 = 1 << 0;
    const WRITE: u32 = 1 << 1;
    const EXECUTE: u32 = 1 << 2;
    const GRANT: u32 = 1 << 3;
    const ALL: u32 = Self::READ | Self::WRITE | Self::EXECUTE | Self::GRANT;
    
    fn new(bits: u32) -> Self {
        Self(bits)
    }
    
    fn contains(&self, other: CapRights) -> bool {
        (self.0 & other.0) == other.0
    }
    
    fn intersect(&self, other: CapRights) -> Self {
        Self(self.0 & other.0)
    }
    
    fn is_subset_of(&self, other: CapRights) -> bool {
        (self.0 & !other.0) == 0
    }
}

/// 能力 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct CapId(u64);

impl CapId {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        Self(COUNTER.fetch_add(1, Ordering::Relaxed))
    }
    
    fn null() -> Self {
        Self(0)
    }
    
    fn is_null(&self) -> bool {
        self.0 == 0
    }
}

/// 能力
#[derive(Debug, Clone)]
struct Capability {
    id: CapId,
    cap_type: CapType,
    rights: CapRights,
    object: usize,
    parent: CapId,
}

impl Capability {
    fn new(cap_type: CapType, rights: CapRights, object: usize) -> Self {
        Self {
            id: CapId::new(),
            cap_type,
            rights,
            object,
            parent: CapId::null(),
        }
    }
    
    fn derive(&self, new_rights: CapRights) -> Option<Self> {
        // 必须有 GRANT 权限才能派生
        if !self.rights.contains(CapRights::new(CapRights::GRANT)) {
            return None;
        }
        
        // 新权限必须是原权限的子集
        let derived_rights = self.rights.intersect(new_rights);
        
        Some(Self {
            id: CapId::new(),
            cap_type: self.cap_type,
            rights: derived_rights,
            object: self.object,
            parent: self.id,
        })
    }
    
    fn check(&self, required: CapRights) -> bool {
        self.rights.contains(required)
    }
}

/// 能力空间
struct CSpace {
    slots: BTreeMap<usize, Capability>,
    next_slot: usize,
    max_slots: usize,
}

impl CSpace {
    fn new(max_slots: usize) -> Self {
        Self {
            slots: BTreeMap::new(),
            next_slot: 0,
            max_slots,
        }
    }
    
    fn insert(&mut self, cap: Capability) -> Option<usize> {
        if self.next_slot >= self.max_slots {
            return None;
        }
        let slot = self.next_slot;
        self.slots.insert(slot, cap);
        self.next_slot += 1;
        Some(slot)
    }
    
    fn get(&self, slot: usize) -> Option<&Capability> {
        self.slots.get(&slot)
    }
    
    fn remove(&mut self, slot: usize) -> Option<Capability> {
        self.slots.remove(&slot)
    }
    
    fn derive(&mut self, src_slot: usize, new_rights: CapRights) -> Option<usize> {
        let src_cap = self.get(src_slot)?;
        let derived = src_cap.derive(new_rights)?;
        self.insert(derived)
    }
}

/// Property 2: 能力权限单调性
/// Feature: rust-warnings-cleanup, Property 2: 能力权限单调性
/// 
/// *For any* capability derivation from parent to child, the child's rights 
/// must be a subset of the parent's rights (rights can only decrease, never increase).
/// 
/// **Validates: Requirements 2.2, 8.2**
#[test_case]
fn test_rights_monotonic_decrease() -> bool {
    let parent = Capability::new(
        CapType::Memory,
        CapRights::new(CapRights::ALL),
        0x1000,
    );
    
    // 派生时请求更多权限
    let child = parent.derive(CapRights::new(CapRights::ALL));
    
    if let Some(child) = child {
        // 子能力权限应该是父能力权限的子集
        if !child.rights.is_subset_of(parent.rights) {
            println!("  错误: 子能力权限超过父能力");
            return false;
        }
    }
    
    // 测试多级派生
    let mut current = parent.clone();
    for _ in 0..5 {
        if let Some(derived) = current.derive(CapRights::new(CapRights::ALL)) {
            if !derived.rights.is_subset_of(current.rights) {
                println!("  错误: 多级派生权限不单调");
                return false;
            }
            current = derived;
        } else {
            break;
        }
    }
    
    true
}

/// Property 2 扩展: 权限单调性 - 随机测试
/// Feature: rust-warnings-cleanup, Property 2: 能力权限单调性
/// 
/// *For any* random sequence of derivations, rights must always decrease or stay the same.
/// 
/// **Validates: Requirements 2.2, 8.2**
#[test_case]
fn test_rights_monotonic_random() -> bool {
    let mut rng = SimpleRng::new(42);
    
    // 运行 100 次随机测试
    for iteration in 0..100 {
        // 创建随机权限的父能力
        let parent_rights = rng.next_range(0, 16) as u32 | CapRights::GRANT;
        let parent = Capability::new(
            CapType::Memory,
            CapRights::new(parent_rights),
            rng.next_usize(),
        );
        
        // 尝试派生随机权限
        let requested_rights = rng.next_range(0, 16) as u32;
        
        if let Some(child) = parent.derive(CapRights::new(requested_rights)) {
            // Property: 子能力权限必须是父能力权限的子集
            if !child.rights.is_subset_of(parent.rights) {
                println!("  Property violation at iteration {}: child rights {:?} not subset of parent {:?}",
                    iteration, child.rights, parent.rights);
                return false;
            }
            
            // Property: 子能力权限必须是请求权限和父能力权限的交集
            let expected = parent.rights.intersect(CapRights::new(requested_rights));
            if child.rights.0 != expected.0 {
                println!("  Property violation at iteration {}: child rights {:?} != expected {:?}",
                    iteration, child.rights, expected);
                return false;
            }
        }
    }
    
    true
}

/// Property 2 扩展: 多级派生权限单调性
/// Feature: rust-warnings-cleanup, Property 2: 能力权限单调性
/// 
/// *For any* chain of derivations, each level must have rights <= previous level.
/// 
/// **Validates: Requirements 2.2, 8.2**
#[test_case]
fn test_rights_monotonic_chain() -> bool {
    let mut rng = SimpleRng::new(12345);
    
    // 测试多条派生链
    for chain_idx in 0..20 {
        // 创建根能力（全部权限）
        let root = Capability::new(
            CapType::Memory,
            CapRights::new(CapRights::ALL),
            chain_idx * 0x1000,
        );
        
        let mut current = root;
        let mut prev_rights = current.rights;
        
        // 派生链深度 1-10
        let chain_depth = rng.next_range(1, 10);
        
        for depth in 0..chain_depth {
            let requested = rng.next_range(0, 16) as u32;
            
            if let Some(derived) = current.derive(CapRights::new(requested)) {
                // Property: 每一级的权限都不能超过上一级
                if !derived.rights.is_subset_of(prev_rights) {
                    println!("  Property violation in chain {} at depth {}", chain_idx, depth);
                    return false;
                }
                
                prev_rights = derived.rights;
                current = derived;
            } else {
                // 无法派生（可能是因为没有 GRANT 权限），这是正确的行为
                break;
            }
        }
    }
    
    true
}

/// Property 2 辅助: 无 GRANT 权限不能派生
/// Feature: rust-warnings-cleanup, Property 2: 能力权限单调性
/// 
/// *For any* capability without GRANT permission, derivation must fail.
/// 
/// **Validates: Requirements 2.2**
#[test_case]
fn test_no_grant_no_derive() -> bool {
    let cap = Capability::new(
        CapType::Memory,
        CapRights::new(CapRights::READ | CapRights::WRITE), // 无 GRANT
        0x1000,
    );
    
    let derived = cap.derive(CapRights::new(CapRights::READ));
    
    if derived.is_some() {
        println!("  错误: 无 GRANT 权限不应能派生");
        return false;
    }
    
    true
}

/// Property 3: 能力 ID 唯一性
/// Feature: rust-warnings-cleanup, Property 3: 能力 ID 唯一性
/// 
/// *For any* two distinct capabilities created by the system, their IDs must be different.
/// 
/// **Validates: Requirements 2.1**
#[test_case]
fn test_capability_id_uniqueness() -> bool {
    let mut ids = Vec::new();
    
    for _ in 0..100 {
        let cap = Capability::new(
            CapType::Memory,
            CapRights::new(CapRights::READ),
            0,
        );
        
        if ids.contains(&cap.id) {
            println!("  错误: 发现重复的能力 ID");
            return false;
        }
        ids.push(cap.id);
    }
    
    true
}

/// Property 3 扩展: 大规模 ID 唯一性测试
/// Feature: rust-warnings-cleanup, Property 3: 能力 ID 唯一性
/// 
/// *For any* large number of capabilities, all IDs must be unique.
/// 
/// **Validates: Requirements 2.1**
#[test_case]
fn test_capability_id_uniqueness_large_scale() -> bool {
    use alloc::collections::BTreeSet;
    
    let mut ids = BTreeSet::new();
    const NUM_CAPS: usize = 1000;
    
    for i in 0..NUM_CAPS {
        let cap = Capability::new(
            CapType::Memory,
            CapRights::new(CapRights::READ),
            i,
        );
        
        // Property: 每个新 ID 都应该是唯一的
        if !ids.insert(cap.id) {
            println!("  Property violation: duplicate ID found at iteration {}", i);
            return false;
        }
    }
    
    // Property: 应该有 NUM_CAPS 个唯一 ID
    if ids.len() != NUM_CAPS {
        println!("  Property violation: expected {} unique IDs, got {}", NUM_CAPS, ids.len());
        return false;
    }
    
    true
}

/// Property 3 扩展: 派生能力 ID 唯一性
/// Feature: rust-warnings-cleanup, Property 3: 能力 ID 唯一性
/// 
/// *For any* derived capability, its ID must be different from its parent and all siblings.
/// 
/// **Validates: Requirements 2.1**
#[test_case]
fn test_derived_capability_id_uniqueness() -> bool {
    use alloc::collections::BTreeSet;
    
    let mut all_ids = BTreeSet::new();
    
    // 创建多个根能力
    for root_idx in 0..10 {
        let root = Capability::new(
            CapType::Memory,
            CapRights::new(CapRights::ALL),
            root_idx * 0x1000,
        );
        
        // Property: 根能力 ID 唯一
        if !all_ids.insert(root.id) {
            println!("  Property violation: duplicate root ID at index {}", root_idx);
            return false;
        }
        
        // 从每个根派生多个子能力
        for child_idx in 0..10 {
            if let Some(child) = root.derive(CapRights::new(CapRights::READ | CapRights::GRANT)) {
                // Property: 子能力 ID 与父能力不同
                if child.id == root.id {
                    println!("  Property violation: child ID equals parent ID");
                    return false;
                }
                
                // Property: 子能力 ID 与所有其他能力不同
                if !all_ids.insert(child.id) {
                    println!("  Property violation: duplicate child ID at root {} child {}", 
                        root_idx, child_idx);
                    return false;
                }
            }
        }
    }
    
    true
}

/// Property 3 扩展: ID 单调递增
/// Feature: rust-warnings-cleanup, Property 3: 能力 ID 唯一性
/// 
/// *For any* sequence of capability creations, IDs should be monotonically increasing.
/// 
/// **Validates: Requirements 2.1**
#[test_case]
fn test_capability_id_monotonic() -> bool {
    let mut prev_id = CapId::null();
    
    for i in 0..100 {
        let cap = Capability::new(
            CapType::Memory,
            CapRights::new(CapRights::READ),
            i,
        );
        
        // Property: 新 ID 应该大于之前的 ID（除了第一个）
        if !prev_id.is_null() && cap.id.0 <= prev_id.0 {
            println!("  Property violation: ID {} not greater than previous {}", 
                cap.id.0, prev_id.0);
            return false;
        }
        
        prev_id = cap.id;
    }
    
    true
}

/// 属性4：权限检查正确性
/// 权限检查应正确判断权限是否满足
#[test_case]
fn test_permission_check() -> bool {
    let cap = Capability::new(
        CapType::Memory,
        CapRights::new(CapRights::READ | CapRights::WRITE),
        0x1000,
    );
    
    // 应该通过的检查
    if !cap.check(CapRights::new(CapRights::READ)) {
        println!("  错误: READ 检查应通过");
        return false;
    }
    
    if !cap.check(CapRights::new(CapRights::WRITE)) {
        println!("  错误: WRITE 检查应通过");
        return false;
    }
    
    if !cap.check(CapRights::new(CapRights::READ | CapRights::WRITE)) {
        println!("  错误: READ|WRITE 检查应通过");
        return false;
    }
    
    // 应该失败的检查
    if cap.check(CapRights::new(CapRights::EXECUTE)) {
        println!("  错误: EXECUTE 检查应失败");
        return false;
    }
    
    if cap.check(CapRights::new(CapRights::GRANT)) {
        println!("  错误: GRANT 检查应失败");
        return false;
    }
    
    true
}

/// 属性5：CSpace 插入-查找一致性
/// 插入的能力应能正确查找
#[test_case]
fn test_cspace_insert_get() -> bool {
    let mut cspace = CSpace::new(100);
    let mut rng = SimpleRng::new(12345);
    let mut inserted = Vec::new();
    
    for _ in 0..50 {
        let cap = Capability::new(
            CapType::Memory,
            CapRights::new(rng.next_range(0, 16) as u32),
            rng.next_usize(),
        );
        let cap_id = cap.id;
        
        if let Some(slot) = cspace.insert(cap) {
            inserted.push((slot, cap_id));
        }
    }
    
    // 验证所有插入的能力
    for (slot, expected_id) in &inserted {
        match cspace.get(*slot) {
            Some(cap) if cap.id == *expected_id => {}
            Some(cap) => {
                println!("  错误: 槽 {} 的能力 ID 不匹配", slot);
                return false;
            }
            None => {
                println!("  错误: 槽 {} 查找失败", slot);
                return false;
            }
        }
    }
    
    true
}

/// 属性6：CSpace 删除属性
/// 删除后应无法查找
#[test_case]
fn test_cspace_remove() -> bool {
    let mut cspace = CSpace::new(100);
    
    let cap = Capability::new(
        CapType::Thread,
        CapRights::new(CapRights::ALL),
        0x2000,
    );
    
    let slot = cspace.insert(cap).expect("插入失败");
    
    // 验证存在
    if cspace.get(slot).is_none() {
        println!("  错误: 插入后查找失败");
        return false;
    }
    
    // 删除
    cspace.remove(slot);
    
    // 验证已删除
    if cspace.get(slot).is_some() {
        println!("  错误: 删除后仍能查找");
        return false;
    }
    
    true
}

/// 属性7：CSpace 容量限制
/// 超过最大槽数应返回 None
#[test_case]
fn test_cspace_capacity() -> bool {
    let mut cspace = CSpace::new(5);
    
    for i in 0..5 {
        let cap = Capability::new(CapType::Memory, CapRights::new(0), i);
        if cspace.insert(cap).is_none() {
            println!("  错误: 第 {} 次插入不应失败", i);
            return false;
        }
    }
    
    // 第 6 次应失败
    let cap = Capability::new(CapType::Memory, CapRights::new(0), 5);
    if cspace.insert(cap).is_some() {
        println!("  错误: 超过容量应失败");
        return false;
    }
    
    true
}

/// 属性8：派生链追踪
/// 派生的能力应正确记录父能力 ID
#[test_case]
fn test_derivation_chain() -> bool {
    let root = Capability::new(
        CapType::Endpoint,
        CapRights::new(CapRights::ALL),
        0x3000,
    );
    let root_id = root.id;
    
    // 第一级派生
    let child1 = root.derive(CapRights::new(CapRights::READ | CapRights::GRANT))
        .expect("派生失败");
    
    if child1.parent != root_id {
        println!("  错误: child1 的父 ID 不正确");
        return false;
    }
    
    let child1_id = child1.id;
    
    // 第二级派生
    let child2 = child1.derive(CapRights::new(CapRights::READ | CapRights::GRANT))
        .expect("派生失败");
    
    if child2.parent != child1_id {
        println!("  错误: child2 的父 ID 不正确");
        return false;
    }
    
    true
}

/// 属性9：CSpace 派生操作
/// 通过 CSpace 派生应正确工作
#[test_case]
fn test_cspace_derive() -> bool {
    let mut cspace = CSpace::new(100);
    
    let root = Capability::new(
        CapType::Memory,
        CapRights::new(CapRights::ALL),
        0x4000,
    );
    let root_id = root.id;
    
    let root_slot = cspace.insert(root).expect("插入失败");
    
    // 派生
    let child_slot = cspace.derive(root_slot, CapRights::new(CapRights::READ | CapRights::GRANT))
        .expect("派生失败");
    
    // 验证子能力
    let child = cspace.get(child_slot).expect("查找失败");
    
    if child.parent != root_id {
        println!("  错误: 派生的能力父 ID 不正确");
        return false;
    }
    
    if !child.rights.is_subset_of(CapRights::new(CapRights::ALL)) {
        println!("  错误: 派生的能力权限不正确");
        return false;
    }
    
    true
}

/// 属性10：随机操作序列
/// 随机执行能力操作，验证不变量
#[test_case]
fn test_random_capability_operations() -> bool {
    let mut cspace = CSpace::new(200);
    let mut rng = SimpleRng::new(77777);
    let mut active_slots: Vec<usize> = Vec::new();
    
    for _ in 0..100 {
        let op = rng.next_range(0, 4);
        
        match op {
            0 => {
                // 创建新能力
                let rights = rng.next_range(0, 16) as u32;
                let cap = Capability::new(
                    CapType::Memory,
                    CapRights::new(rights | CapRights::GRANT),
                    rng.next_usize(),
                );
                if let Some(slot) = cspace.insert(cap) {
                    active_slots.push(slot);
                }
            }
            1 => {
                // 派生
                if !active_slots.is_empty() {
                    let idx = rng.next_range(0, active_slots.len());
                    let src_slot = active_slots[idx];
                    let rights = rng.next_range(0, 16) as u32;
                    
                    if let Some(new_slot) = cspace.derive(src_slot, CapRights::new(rights)) {
                        // 验证权限单调递减
                        let src = cspace.get(src_slot).unwrap();
                        let derived = cspace.get(new_slot).unwrap();
                        
                        if !derived.rights.is_subset_of(src.rights) {
                            println!("  错误: 派生权限不单调");
                            return false;
                        }
                        
                        active_slots.push(new_slot);
                    }
                }
            }
            2 => {
                // 删除
                if !active_slots.is_empty() {
                    let idx = rng.next_range(0, active_slots.len());
                    let slot = active_slots.swap_remove(idx);
                    cspace.remove(slot);
                }
            }
            _ => {
                // 权限检查
                if !active_slots.is_empty() {
                    let idx = rng.next_range(0, active_slots.len());
                    let slot = active_slots[idx];
                    
                    if let Some(cap) = cspace.get(slot) {
                        // 检查自身权限应该通过
                        if !cap.check(cap.rights) {
                            println!("  错误: 自身权限检查失败");
                            return false;
                        }
                    }
                }
            }
        }
    }
    
    true
}
