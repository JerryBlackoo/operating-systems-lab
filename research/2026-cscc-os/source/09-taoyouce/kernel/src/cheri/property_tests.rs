//! CHERI 能力属性测试
//! 
//! 这些测试验证 CHERI 能力模拟的核心安全属性。
//! 每个属性都对应 CHERI 架构规范中的关键不变量。
//!
//! ## Property 2: CHERI 边界检查正确性
//! Feature: rustmicroos-comprehensive-fix, Property 2: CHERI 边界检查正确性
//! **Validates: Requirements 2.2**
//!
//! *For any* CHERI capability with bounds [base, base+length) and *for any* access 
//! at offset with size, the bounds check SHALL pass if and only if offset+size <= length.

use super::*;

/// 简单的伪随机数生成器
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    
    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.state
    }
    
    fn next_range(&mut self, min: u64, max: u64) -> u64 {
        if max <= min {
            return min;
        }
        min + (self.next() % (max - min))
    }
}

/// **Property 2: CHERI 边界检查正确性**
/// *For any* CHERI capability with bounds [base, base+length) and *for any* access 
/// at offset with size, the bounds check SHALL pass if and only if offset+size <= length.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 2: CHERI 边界检查正确性
/// **Validates: Requirements 2.2**
#[test]
fn property_bounds_check_correctness() {
    // 测试多种边界组合
    let test_cases = [
        (0u64, 100u64),      // 从0开始
        (1000u64, 500u64),   // 中间位置
        (0x8000_0000u64, 0x1000u64), // 高地址
        (1u64, 1u64),        // 最小能力
        (0u64, u64::MAX / 2), // 大能力
    ];
    
    for (base, length) in test_cases {
        let cap = CheriCapability::new(base, length, CheriPermissions::ALL);
        
        // 测试有效访问
        for offset in [0, length / 2, length - 1] {
            if offset < length {
                let result = cap.check_bounds(offset, 1);
                assert!(result.is_ok(), 
                    "有效访问被拒绝: base={}, length={}, offset={}", 
                    base, length, offset);
            }
        }
        
        // 测试边界访问
        assert!(cap.check_bounds(0, length).is_ok(), 
            "完整范围访问被拒绝");
        
        // 测试越界访问
        let invalid_offsets = [length, length + 1];
        for offset in invalid_offsets {
            let result = cap.check_bounds(offset, 1);
            assert!(matches!(result, Err(CheriViolation::BoundsViolation { .. })),
                "越界访问未被检测: offset={}", offset);
        }
        
        // 测试跨边界访问
        if length > 0 {
            let result = cap.check_bounds(length - 1, 2);
            assert!(matches!(result, Err(CheriViolation::BoundsViolation { .. })),
                "跨边界访问未被检测");
        }
    }
}

/// **Property 2 扩展: 随机边界检查测试**
/// *For any* randomly generated capability and access, the bounds check should correctly
/// accept valid accesses (offset+size <= length) and reject invalid ones (offset+size > length).
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 2: CHERI 边界检查正确性
/// **Validates: Requirements 2.2**
/// 
/// 运行 100 次迭代
#[test]
fn property_2_bounds_check_random() {
    let mut rng = SimpleRng::new(12345);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成随机能力参数
        let base = rng.next_range(0, 0x1_0000_0000);
        let length = rng.next_range(1, 0x10000); // 至少 1 字节
        
        let cap = CheriCapability::new(base, length, CheriPermissions::ALL);
        
        // 测试有效访问（在边界内）
        let valid_offset = rng.next_range(0, length);
        let max_size = length - valid_offset;
        let valid_size = if max_size > 0 { rng.next_range(1, max_size.min(256) + 1) } else { 1 };
        
        if valid_offset + valid_size <= length {
            let result = cap.check_bounds(valid_offset, valid_size);
            if result.is_err() {
                panic!(
                    "迭代 {}: 有效访问被拒绝: base={}, length={}, offset={}, size={}",
                    iteration, base, length, valid_offset, valid_size
                );
            }
        }
        
        // 测试无效访问（超出边界）
        let invalid_offset = rng.next_range(length, length + 1000);
        let result = cap.check_bounds(invalid_offset, 1);
        
        if !matches!(result, Err(CheriViolation::BoundsViolation { .. })) {
            panic!(
                "迭代 {}: 越界访问未被检测: base={}, length={}, offset={}",
                iteration, base, length, invalid_offset
            );
        }
        
        // 测试跨边界访问
        if length > 1 {
            let cross_offset = length - 1;
            let cross_size = 2;
            let result = cap.check_bounds(cross_offset, cross_size);
            
            if !matches!(result, Err(CheriViolation::BoundsViolation { .. })) {
                panic!(
                    "迭代 {}: 跨边界访问未被检测: base={}, length={}, offset={}, size={}",
                    iteration, base, length, cross_offset, cross_size
                );
            }
        }
    }
}

/// **Property 2 扩展: 边界条件测试**
/// 测试边界条件：恰好在边界上的访问
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 2: CHERI 边界检查正确性
/// **Validates: Requirements 2.2**
/// 
/// 运行 100 次迭代
#[test]
fn property_2_bounds_edge_cases() {
    let mut rng = SimpleRng::new(67890);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        let base = rng.next_range(0, 0x1_0000_0000);
        let length = rng.next_range(10, 10000);
        
        let cap = CheriCapability::new(base, length, CheriPermissions::ALL);
        
        // 测试恰好在边界上的访问（应该成功）
        // offset = 0, size = length
        let result = cap.check_bounds(0, length);
        if result.is_err() {
            panic!(
                "迭代 {}: 完整范围访问被拒绝: base={}, length={}",
                iteration, base, length
            );
        }
        
        // 测试最后一个字节（应该成功）
        // offset = length - 1, size = 1
        let result = cap.check_bounds(length - 1, 1);
        if result.is_err() {
            panic!(
                "迭代 {}: 最后一个字节访问被拒绝: base={}, length={}",
                iteration, base, length
            );
        }
        
        // 测试恰好超出边界一个字节（应该失败）
        // offset = length, size = 1
        let result = cap.check_bounds(length, 1);
        if !matches!(result, Err(CheriViolation::BoundsViolation { .. })) {
            panic!(
                "迭代 {}: 恰好超出边界的访问未被检测: base={}, length={}",
                iteration, base, length
            );
        }
        
        // 测试 size = 0 的访问（应该成功，因为不访问任何内存）
        let result = cap.check_bounds(0, 0);
        if result.is_err() {
            panic!(
                "迭代 {}: 零大小访问被拒绝: base={}, length={}",
                iteration, base, length
            );
        }
    }
}

/// Property 4: CHERI 权限检查正确性
/// For any CheriCapability 和操作类型，如果操作所需权限是能力权限的子集则允许，
/// 否则触发 PermissionViolation 异常。
#[test]
fn property_permission_check_correctness() {
    let all_perms = [
        CheriPermissions::LOAD,
        CheriPermissions::STORE,
        CheriPermissions::EXECUTE,
        CheriPermissions::LOAD_CAP,
        CheriPermissions::STORE_CAP,
        CheriPermissions::SEAL,
        CheriPermissions::UNSEAL,
        CheriPermissions::SYSTEM,
    ];
    
    // 测试单一权限
    for &perm in &all_perms {
        let cap = CheriCapability::new(0, 100, perm);
        
        // 应该允许拥有的权限
        assert!(cap.check_permission(perm).is_ok(),
            "拥有的权限被拒绝: {:?}", perm);
        
        // 应该拒绝未拥有的权限
        for &other_perm in &all_perms {
            if other_perm != perm {
                let result = cap.check_permission(other_perm);
                assert!(matches!(result, Err(CheriViolation::PermissionViolation { .. })),
                    "未拥有的权限被允许: 拥有 {:?}, 请求 {:?}", perm, other_perm);
            }
        }
    }
    
    // 测试组合权限
    let read_write = CheriPermissions::LOAD | CheriPermissions::STORE;
    let cap = CheriCapability::new(0, 100, read_write);
    
    assert!(cap.check_permission(CheriPermissions::LOAD).is_ok());
    assert!(cap.check_permission(CheriPermissions::STORE).is_ok());
    assert!(cap.check_permission(read_write).is_ok());
    assert!(cap.check_permission(CheriPermissions::EXECUTE).is_err());
}

/// Property 5: CHERI 能力派生单调性（基础测试）
/// For any 能力派生操作，派生后的能力边界必须在父能力边界内，
/// 且权限必须是父能力权限的子集。
#[test]
fn property_derive_monotonicity() {
    let parent = CheriCapability::new(100, 1000, CheriPermissions::ALL);
    
    // 测试有效派生：边界收窄
    let valid_derives = [
        (100u64, 1000u64, CheriPermissions::ALL),      // 相同
        (100, 500, CheriPermissions::ALL),             // 长度减少
        (200, 800, CheriPermissions::ALL),             // 起始增加
        (100, 1000, CheriPermissions::LOAD),           // 权限减少
        (200, 500, CheriPermissions::LOAD | CheriPermissions::STORE), // 全部收窄
    ];
    
    for (new_base, new_length, new_perms) in valid_derives {
        let result = parent.derive(new_base, new_length, new_perms);
        assert!(result.is_ok(), 
            "有效派生被拒绝: base={}, length={}, perms={:?}",
            new_base, new_length, new_perms);
        
        let child = result.unwrap();
        
        // 验证边界单调性
        assert!(child.base() >= parent.base(), "子能力基地址小于父能力");
        assert!(child.base() + child.length() <= parent.base() + parent.length(),
            "子能力超出父能力边界");
        
        // 验证权限单调性
        assert!(child.permissions().bits() & parent.permissions().bits() 
                == child.permissions().bits(),
            "子能力权限不是父能力权限的子集");
    }
    
    // 测试无效派生：边界扩展
    let invalid_derives = [
        (50u64, 1000u64, CheriPermissions::ALL),   // 基地址减少
        (100, 1100, CheriPermissions::ALL),        // 长度增加
    ];
    
    for (new_base, new_length, new_perms) in invalid_derives {
        let result = parent.derive(new_base, new_length, new_perms);
        assert!(result.is_err(),
            "无效派生被允许: base={}, length={}", new_base, new_length);
    }
}

/// **Property 4: CHERI 派生单调性**
/// *For any* parent capability and *for any* derived child capability, the child's bounds 
/// SHALL be a subset of the parent's bounds AND the child's permissions SHALL be a subset 
/// of the parent's permissions.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 4: CHERI 派生单调性
/// **Validates: Requirements 2.4**
/// 
/// 运行 100 次迭代
#[test]
fn property_4_derive_monotonicity_random() {
    let mut rng = SimpleRng::new(54321);
    
    // 可用的权限列表
    let permission_options = [
        CheriPermissions::LOAD,
        CheriPermissions::STORE,
        CheriPermissions::EXECUTE,
        CheriPermissions::LOAD_CAP,
        CheriPermissions::STORE_CAP,
        CheriPermissions::SEAL,
        CheriPermissions::UNSEAL,
        CheriPermissions::READ_WRITE,
        CheriPermissions::ALL,
    ];
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成随机父能力参数
        let parent_base = rng.next_range(0x1000, 0x1_0000_0000);
        let parent_length = rng.next_range(0x1000, 0x10_0000); // 至少 4KB
        let parent_perms = CheriPermissions::ALL; // 父能力拥有所有权限
        
        let parent = CheriCapability::new(parent_base, parent_length, parent_perms);
        
        // 生成有效的子能力参数（在父能力范围内）
        let child_offset = rng.next_range(0, parent_length / 2);
        let child_base = parent_base + child_offset;
        let max_child_length = parent_length - child_offset;
        let child_length = rng.next_range(1, max_child_length.min(0x10000) + 1);
        
        // 随机选择一个权限子集
        let perm_index = (rng.next() % permission_options.len() as u64) as usize;
        let child_perms = permission_options[perm_index];
        
        // 验证派生参数在父能力范围内
        if child_base >= parent_base && child_base + child_length <= parent_base + parent_length {
            let result = parent.derive(child_base, child_length, child_perms);
            
            match result {
                Ok(child) => {
                    // 验证边界单调性：子能力边界必须在父能力边界内
                    if child.base() < parent.base() {
                        panic!(
                            "迭代 {}: 子能力基地址 ({:#x}) 小于父能力基地址 ({:#x})",
                            iteration, child.base(), parent.base()
                        );
                    }
                    
                    let child_end = child.base() + child.length();
                    let parent_end = parent.base() + parent.length();
                    if child_end > parent_end {
                        panic!(
                            "迭代 {}: 子能力结束地址 ({:#x}) 超出父能力结束地址 ({:#x})",
                            iteration, child_end, parent_end
                        );
                    }
                    
                    // 验证权限单调性：子能力权限必须是父能力权限的子集
                    let child_perm_bits = child.permissions().bits();
                    let parent_perm_bits = parent.permissions().bits();
                    if (child_perm_bits & parent_perm_bits) != child_perm_bits {
                        panic!(
                            "迭代 {}: 子能力权限 ({:?}) 不是父能力权限 ({:?}) 的子集",
                            iteration, child.permissions(), parent.permissions()
                        );
                    }
                    
                    // 验证子能力是父能力的子集
                    if !child.is_subset_of(&parent) {
                        panic!(
                            "迭代 {}: is_subset_of 返回 false，但子能力应该是父能力的子集",
                            iteration
                        );
                    }
                }
                Err(e) => {
                    // 如果派生失败，应该是因为权限放大（子权限不是父权限的子集）
                    // 或者边界超出（这里不应该发生，因为我们已经验证了边界）
                    if !matches!(e, CheriViolation::PermissionAmplification) {
                        panic!(
                            "迭代 {}: 有效派生失败，错误: {:?}, parent_base={:#x}, parent_length={:#x}, child_base={:#x}, child_length={:#x}",
                            iteration, e, parent_base, parent_length, child_base, child_length
                        );
                    }
                }
            }
        }
        
        // 测试无效派生：基地址小于父能力基地址
        if parent_base > 0 {
            let invalid_base = parent_base - 1;
            let result = parent.derive(invalid_base, child_length, child_perms);
            if result.is_ok() {
                panic!(
                    "迭代 {}: 基地址小于父能力的派生应该失败: invalid_base={:#x}, parent_base={:#x}",
                    iteration, invalid_base, parent_base
                );
            }
        }
        
        // 测试无效派生：结束地址超出父能力
        let invalid_length = parent_length + 1;
        let result = parent.derive(parent_base, invalid_length, child_perms);
        if result.is_ok() {
            panic!(
                "迭代 {}: 长度超出父能力的派生应该失败: invalid_length={:#x}, parent_length={:#x}",
                iteration, invalid_length, parent_length
            );
        }
    }
}

/// **Property 4 扩展: 权限放大检测**
/// *For any* parent capability with restricted permissions, attempting to derive a child 
/// with additional permissions SHALL fail with PermissionAmplification error.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 4: CHERI 派生单调性
/// **Validates: Requirements 2.4**
/// 
/// 运行 100 次迭代
#[test]
fn property_4_permission_amplification_detection() {
    let mut rng = SimpleRng::new(98765);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 生成随机父能力参数，但权限受限
        let parent_base = rng.next_range(0x1000, 0x1_0000_0000);
        let parent_length = rng.next_range(0x1000, 0x10_0000);
        
        // 父能力只有 LOAD 权限
        let parent_perms = CheriPermissions::LOAD;
        let parent = CheriCapability::new(parent_base, parent_length, parent_perms);
        
        // 尝试派生具有 STORE 权限的子能力（权限放大）
        let child_base = parent_base;
        let child_length = parent_length / 2;
        let child_perms = CheriPermissions::STORE; // 父能力没有这个权限
        
        let result = parent.derive(child_base, child_length, child_perms);
        
        match result {
            Ok(_) => {
                panic!(
                    "迭代 {}: 权限放大应该被拒绝: parent_perms={:?}, child_perms={:?}",
                    iteration, parent_perms, child_perms
                );
            }
            Err(CheriViolation::PermissionAmplification) => {
                // 正确：权限放大被检测到
            }
            Err(e) => {
                panic!(
                    "迭代 {}: 权限放大应该返回 PermissionAmplification 错误，但返回了: {:?}",
                    iteration, e
                );
            }
        }
        
        // 尝试派生具有 READ_WRITE 权限的子能力（权限放大，因为父能力只有 LOAD）
        let child_perms_rw = CheriPermissions::READ_WRITE;
        let result_rw = parent.derive(child_base, child_length, child_perms_rw);
        
        if result_rw.is_ok() {
            panic!(
                "迭代 {}: READ_WRITE 权限放大应该被拒绝",
                iteration
            );
        }
        
        // 有效派生：子能力权限是父能力权限的子集
        let valid_child_perms = CheriPermissions::LOAD;
        let valid_result = parent.derive(child_base, child_length, valid_child_perms);
        
        if valid_result.is_err() {
            panic!(
                "迭代 {}: 有效的权限子集派生应该成功",
                iteration
            );
        }
    }
}

/// **Property 4 扩展: 多级派生单调性**
/// *For any* chain of capability derivations (grandparent -> parent -> child), 
/// the final child's bounds and permissions SHALL be subsets of all ancestors.
/// 
/// Feature: rustmicroos-comprehensive-fix, Property 4: CHERI 派生单调性
/// **Validates: Requirements 2.4**
/// 
/// 运行 100 次迭代
#[test]
fn property_4_multi_level_derivation() {
    let mut rng = SimpleRng::new(11111);
    
    // 运行 100 次迭代
    for iteration in 0..100 {
        // 创建祖父能力
        let grandparent_base = rng.next_range(0x1000, 0x1_0000_0000);
        let grandparent_length = rng.next_range(0x10000, 0x100_0000); // 至少 64KB
        let grandparent = CheriCapability::new(grandparent_base, grandparent_length, CheriPermissions::ALL);
        
        // 派生父能力（收窄边界和权限）
        let parent_offset = rng.next_range(0, grandparent_length / 4);
        let parent_base = grandparent_base + parent_offset;
        let parent_length = rng.next_range(grandparent_length / 4, grandparent_length - parent_offset);
        let parent_perms = CheriPermissions::READ_WRITE | CheriPermissions::EXECUTE;
        
        let parent_result = grandparent.derive(parent_base, parent_length, parent_perms);
        if parent_result.is_err() {
            continue; // 跳过无效的随机参数
        }
        let parent = parent_result.unwrap();
        
        // 派生子能力（进一步收窄）
        let child_offset = rng.next_range(0, parent_length / 2);
        let child_base = parent_base + child_offset;
        let child_length = rng.next_range(1, (parent_length - child_offset).min(0x1000) + 1);
        let child_perms = CheriPermissions::LOAD;
        
        let child_result = parent.derive(child_base, child_length, child_perms);
        if child_result.is_err() {
            continue; // 跳过无效的随机参数
        }
        let child = child_result.unwrap();
        
        // 验证子能力是父能力的子集
        if !child.is_subset_of(&parent) {
            panic!(
                "迭代 {}: 子能力应该是父能力的子集",
                iteration
            );
        }
        
        // 验证子能力是祖父能力的子集
        if !child.is_subset_of(&grandparent) {
            panic!(
                "迭代 {}: 子能力应该是祖父能力的子集",
                iteration
            );
        }
        
        // 验证边界传递性
        if child.base() < grandparent.base() {
            panic!(
                "迭代 {}: 子能力基地址 ({:#x}) 小于祖父能力基地址 ({:#x})",
                iteration, child.base(), grandparent.base()
            );
        }
        
        let child_end = child.base() + child.length();
        let grandparent_end = grandparent.base() + grandparent.length();
        if child_end > grandparent_end {
            panic!(
                "迭代 {}: 子能力结束地址 ({:#x}) 超出祖父能力结束地址 ({:#x})",
                iteration, child_end, grandparent_end
            );
        }
        
        // 验证权限传递性
        let child_perm_bits = child.permissions().bits();
        let grandparent_perm_bits = grandparent.permissions().bits();
        if (child_perm_bits & grandparent_perm_bits) != child_perm_bits {
            panic!(
                "迭代 {}: 子能力权限不是祖父能力权限的子集",
                iteration
            );
        }
    }
}

/// 属性测试：无效标签检测
#[test]
fn property_invalid_tag_detection() {
    let mut cap = CheriCapability::new(0, 100, CheriPermissions::ALL);
    cap.invalidate(); // 使标签无效
    
    // 所有操作都应该失败
    assert!(matches!(cap.check_bounds(0, 1), Err(CheriViolation::InvalidTag)));
    assert!(matches!(cap.check_permission(CheriPermissions::LOAD), 
        Err(CheriViolation::InvalidTag)));
}

/// 属性测试：封印能力
#[test]
fn property_sealed_capability() {
    let cap = CheriCapability::new(0, 100, CheriPermissions::ALL);
    
    // 封印能力
    let sealed = cap.seal(42).expect("封印失败");
    assert!(sealed.is_sealed(), "封印后应标记为已封印");
    
    // 封印的能力不能直接使用
    assert!(sealed.check_bounds(0, 1).is_err());
    
    // 解封需要正确的 otype
    let unsealed = sealed.unseal(42);
    assert!(unsealed.is_ok(), "解封失败");
    assert!(!unsealed.unwrap().is_sealed(), "解封后应标记为未封印");
    
    // 错误类型解封
    assert!(sealed.unseal(43).is_err());
}

/// 属性测试：能力压缩和解压缩
#[test]
fn property_capability_compression() {
    let test_cases = [
        (0u64, 100u64),
        (0x1000u64, 0x1000u64),
        (0x8000_0000u64, 0x10000u64),
    ];
    
    for (base, length) in test_cases {
        let original = CheriCapability::new(base, length, CheriPermissions::READ_WRITE);
        
        // 压缩
        let compressed = CompressedCapability::compress(&original);
        
        // 解压缩
        let decompressed = compressed.decompress();
        
        // 验证基本属性保持
        // 注意：由于压缩，边界可能会有轻微变化
        assert!(decompressed.base() <= original.base(), 
            "解压缩后基地址增加");
        assert!(decompressed.base() + decompressed.length() >= original.base() + original.length(),
            "解压缩后边界缩小");
    }
}

/// 属性测试：set_bounds 操作
#[test]
fn property_set_bounds() {
    let cap = CheriCapability::new(0, 1000, CheriPermissions::ALL);
    
    // 设置偏移
    let mut cap_with_offset = cap;
    cap_with_offset.set_offset(100).unwrap();
    
    // 使用 set_bounds 创建新能力
    let bounded = cap_with_offset.set_bounds(500).unwrap();
    
    // 验证新能力的边界
    assert_eq!(bounded.base(), 100, "set_bounds 后基地址应为当前地址");
    assert_eq!(bounded.length(), 500, "set_bounds 后长度应为指定值");
    assert_eq!(bounded.offset(), 0, "set_bounds 后偏移应为 0");
}

/// 属性测试：子集测试
#[test]
fn property_subset_test() {
    let parent = CheriCapability::new(0, 1000, CheriPermissions::ALL);
    let child = parent.derive(100, 500, CheriPermissions::LOAD).unwrap();
    
    // 子能力应该是父能力的子集
    assert!(child.is_subset_of(&parent), "派生的能力应该是父能力的子集");
    
    // 父能力不是子能力的子集
    assert!(!parent.is_subset_of(&child), "父能力不应该是子能力的子集");
    
    // 相同能力是自己的子集
    assert!(parent.is_subset_of(&parent), "能力应该是自己的子集");
}

/// 属性测试：对齐检查
#[test]
fn property_alignment_check() {
    let cap = CheriCapability::new(0, 1000, CheriPermissions::ALL);
    
    // 设置到对齐地址
    let mut aligned_cap = cap;
    aligned_cap.set_offset(16).unwrap();
    assert!(aligned_cap.check_alignment(16).is_ok(), "16 字节对齐应该通过");
    assert!(aligned_cap.check_alignment(8).is_ok(), "8 字节对齐应该通过");
    
    // 设置到非对齐地址
    let mut unaligned_cap = cap;
    unaligned_cap.set_offset(17).unwrap();
    assert!(unaligned_cap.check_alignment(16).is_err(), "非 16 字节对齐应该失败");
}
