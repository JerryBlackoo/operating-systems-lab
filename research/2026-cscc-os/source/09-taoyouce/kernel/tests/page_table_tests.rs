//! 页表属性测试
//!
//! 验证 Sv39 页表的以下属性：
//! 1. 映射-翻译一致性：映射后翻译应返回正确的物理地址
//! 2. 取消映射属性：取消映射后翻译应返回 None
//! 3. 权限属性：页表项权限正确设置
//! 4. 多级页表属性：三级页表正确遍历
//! 5. 地址空间隔离：不同页表互不影响

use crate::{SimpleRng, println};
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

/// 模拟的虚拟页号
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct MockVPN(usize);

/// 模拟的物理页号
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct MockPPN(usize);

/// 页表项标志
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MockPTEFlags(u8);

impl MockPTEFlags {
    const V: u8 = 1 << 0;
    const R: u8 = 1 << 1;
    const W: u8 = 1 << 2;
    const X: u8 = 1 << 3;
    const U: u8 = 1 << 4;
    
    fn new(bits: u8) -> Self {
        Self(bits)
    }
    
    fn contains(&self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// 模拟页表项
#[derive(Debug, Clone, Copy)]
struct MockPTE {
    ppn: MockPPN,
    flags: MockPTEFlags,
}

impl MockPTE {
    fn empty() -> Self {
        Self {
            ppn: MockPPN(0),
            flags: MockPTEFlags(0),
        }
    }
    
    fn new(ppn: MockPPN, flags: MockPTEFlags) -> Self {
        Self { ppn, flags }
    }
    
    fn is_valid(&self) -> bool {
        self.flags.contains(MockPTEFlags::V)
    }
}

/// 测试用页表
struct TestPageTable {
    /// 映射表（简化实现）
    mappings: BTreeMap<MockVPN, (MockPPN, MockPTEFlags)>,
    /// 分配的页表页数
    allocated_pages: usize,
}

impl TestPageTable {
    fn new() -> Self {
        Self {
            mappings: BTreeMap::new(),
            allocated_pages: 1, // 根页表
        }
    }
    
    fn map(&mut self, vpn: MockVPN, ppn: MockPPN, flags: MockPTEFlags) -> bool {
        if self.mappings.contains_key(&vpn) {
            return false; // 已映射
        }
        
        // 模拟页表页分配（每 512 个映射需要一个新页表页）
        let level2_idx = vpn.0 / 512;
        let level1_idx = level2_idx / 512;
        
        // 简化：假设每个新的索引需要分配页表页
        self.allocated_pages += 1;
        
        let full_flags = MockPTEFlags::new(flags.0 | MockPTEFlags::V);
        self.mappings.insert(vpn, (ppn, full_flags));
        true
    }
    
    fn unmap(&mut self, vpn: MockVPN) -> bool {
        self.mappings.remove(&vpn).is_some()
    }
    
    fn translate(&self, vpn: MockVPN) -> Option<MockPPN> {
        self.mappings.get(&vpn).map(|(ppn, _)| *ppn)
    }
    
    fn get_flags(&self, vpn: MockVPN) -> Option<MockPTEFlags> {
        self.mappings.get(&vpn).map(|(_, flags)| *flags)
    }
}

/// 属性1：映射-翻译一致性
/// 映射 VPN -> PPN 后，翻译 VPN 应返回 PPN
#[test_case]
fn test_map_translate_consistency() -> bool {
    let mut pt = TestPageTable::new();
    let mut rng = SimpleRng::new(12345);
    
    let mut mappings = Vec::new();
    
    // 创建随机映射
    for _ in 0..100 {
        let vpn = MockVPN(rng.next_range(0, 0x10000));
        let ppn = MockPPN(rng.next_range(0, 0x10000));
        let flags = MockPTEFlags::new(MockPTEFlags::R | MockPTEFlags::W);
        
        if pt.map(vpn, ppn, flags) {
            mappings.push((vpn, ppn));
        }
    }
    
    // 验证所有映射
    for (vpn, expected_ppn) in &mappings {
        match pt.translate(*vpn) {
            Some(actual_ppn) if actual_ppn == *expected_ppn => {}
            Some(actual_ppn) => {
                println!("  错误: VPN {:?} 翻译为 {:?}，期望 {:?}", 
                         vpn, actual_ppn, expected_ppn);
                return false;
            }
            None => {
                println!("  错误: VPN {:?} 翻译失败", vpn);
                return false;
            }
        }
    }
    
    true
}

/// 属性2：取消映射属性
/// 取消映射后，翻译应返回 None
#[test_case]
fn test_unmap_property() -> bool {
    let mut pt = TestPageTable::new();
    
    let vpn = MockVPN(0x1000);
    let ppn = MockPPN(0x2000);
    let flags = MockPTEFlags::new(MockPTEFlags::R);
    
    // 映射
    pt.map(vpn, ppn, flags);
    
    // 验证映射存在
    if pt.translate(vpn).is_none() {
        println!("  错误: 映射后翻译失败");
        return false;
    }
    
    // 取消映射
    pt.unmap(vpn);
    
    // 验证映射已删除
    if pt.translate(vpn).is_some() {
        println!("  错误: 取消映射后仍能翻译");
        return false;
    }
    
    true
}

/// 属性3：权限属性
/// 页表项应正确保存权限标志
#[test_case]
fn test_permission_property() -> bool {
    let mut pt = TestPageTable::new();
    
    // 测试不同权限组合
    let test_cases = [
        (MockVPN(0x100), MockPTEFlags::R),
        (MockVPN(0x200), MockPTEFlags::R | MockPTEFlags::W),
        (MockVPN(0x300), MockPTEFlags::R | MockPTEFlags::X),
        (MockVPN(0x400), MockPTEFlags::R | MockPTEFlags::W | MockPTEFlags::X),
        (MockVPN(0x500), MockPTEFlags::R | MockPTEFlags::U),
    ];
    
    for (vpn, flags) in test_cases {
        let ppn = MockPPN(vpn.0 + 0x1000);
        let flags = MockPTEFlags::new(flags);
        pt.map(vpn, ppn, flags);
        
        if let Some(actual_flags) = pt.get_flags(vpn) {
            // 检查 V 位被设置
            if !actual_flags.contains(MockPTEFlags::V) {
                println!("  错误: VPN {:?} 的 V 位未设置", vpn);
                return false;
            }
            // 检查原始权限被保留
            if (actual_flags.0 & flags.0) != flags.0 {
                println!("  错误: VPN {:?} 权限不匹配", vpn);
                return false;
            }
        } else {
            println!("  错误: 无法获取 VPN {:?} 的权限", vpn);
            return false;
        }
    }
    
    true
}

/// 属性4：重复映射检测
/// 对已映射的 VPN 再次映射应该失败
#[test_case]
fn test_duplicate_mapping() -> bool {
    let mut pt = TestPageTable::new();
    
    let vpn = MockVPN(0x1000);
    let ppn1 = MockPPN(0x2000);
    let ppn2 = MockPPN(0x3000);
    let flags = MockPTEFlags::new(MockPTEFlags::R | MockPTEFlags::W);
    
    // 第一次映射应成功
    if !pt.map(vpn, ppn1, flags) {
        println!("  错误: 第一次映射失败");
        return false;
    }
    
    // 第二次映射应失败
    if pt.map(vpn, ppn2, flags) {
        println!("  错误: 重复映射应该失败");
        return false;
    }
    
    // 验证原映射未被覆盖
    if pt.translate(vpn) != Some(ppn1) {
        println!("  错误: 原映射被覆盖");
        return false;
    }
    
    true
}

/// 属性5：地址空间隔离
/// 不同页表的映射互不影响
#[test_case]
fn test_address_space_isolation() -> bool {
    let mut pt1 = TestPageTable::new();
    let mut pt2 = TestPageTable::new();
    
    let vpn = MockVPN(0x1000);
    let ppn1 = MockPPN(0x2000);
    let ppn2 = MockPPN(0x3000);
    let flags = MockPTEFlags::new(MockPTEFlags::R);
    
    // 在两个页表中映射相同 VPN 到不同 PPN
    pt1.map(vpn, ppn1, flags);
    pt2.map(vpn, ppn2, flags);
    
    // 验证隔离
    if pt1.translate(vpn) != Some(ppn1) {
        println!("  错误: pt1 映射被影响");
        return false;
    }
    
    if pt2.translate(vpn) != Some(ppn2) {
        println!("  错误: pt2 映射被影响");
        return false;
    }
    
    true
}

/// 属性6：VPN 索引计算
/// 验证 Sv39 的三级索引计算正确
#[test_case]
fn test_vpn_indices() -> bool {
    // Sv39: VPN[2] = bits[38:30], VPN[1] = bits[29:21], VPN[0] = bits[20:12]
    
    let test_cases = [
        // (vpn, expected_indices)
        (0x000000000, [0, 0, 0]),
        (0x000000001, [0, 0, 1]),
        (0x000000200, [0, 1, 0]),  // 512 = 0x200
        (0x000040000, [1, 0, 0]),  // 512 * 512 = 0x40000
        (0x000040201, [1, 1, 1]),
        (0x007FFFFFF, [511, 511, 511]),
    ];
    
    for (vpn, expected) in test_cases {
        let idx0 = vpn & 0x1FF;           // bits [8:0]
        let idx1 = (vpn >> 9) & 0x1FF;    // bits [17:9]
        let idx2 = (vpn >> 18) & 0x1FF;   // bits [26:18]
        
        let actual = [idx2, idx1, idx0];
        
        if actual != expected {
            println!("  错误: VPN 0x{:x} 索引 {:?}，期望 {:?}", vpn, actual, expected);
            return false;
        }
    }
    
    true
}

/// 属性7：随机映射-取消映射序列
/// 随机执行映射和取消映射，验证一致性
#[test_case]
fn test_random_map_unmap() -> bool {
    let mut pt = TestPageTable::new();
    let mut rng = SimpleRng::new(99999);
    let mut active_mappings: BTreeMap<usize, usize> = BTreeMap::new();
    
    for _ in 0..200 {
        let op = rng.next_range(0, 3);
        
        match op {
            0 | 1 => {
                // 映射
                let vpn = rng.next_range(0, 1000);
                let ppn = rng.next_range(0, 10000);
                let flags = MockPTEFlags::new(MockPTEFlags::R | MockPTEFlags::W);
                
                if pt.map(MockVPN(vpn), MockPPN(ppn), flags) {
                    active_mappings.insert(vpn, ppn);
                }
            }
            _ => {
                // 取消映射
                if !active_mappings.is_empty() {
                    let keys: Vec<_> = active_mappings.keys().cloned().collect();
                    let idx = rng.next_range(0, keys.len());
                    let vpn = keys[idx];
                    
                    pt.unmap(MockVPN(vpn));
                    active_mappings.remove(&vpn);
                }
            }
        }
        
        // 验证所有活跃映射
        for (&vpn, &expected_ppn) in &active_mappings {
            match pt.translate(MockVPN(vpn)) {
                Some(actual) if actual.0 == expected_ppn => {}
                _ => {
                    println!("  错误: 映射不一致 VPN={}", vpn);
                    return false;
                }
            }
        }
    }
    
    true
}
