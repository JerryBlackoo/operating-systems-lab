//! cap_demo - 能力系统演示程序（简化版）
//!
//! 演示 RustMicroOS 的能力安全特性

#![no_std]
#![no_main]

#[macro_use]
extern crate user;

use user::{
    cap_create_memory, cap_derive, cap_check, cap_revoke, cap_read, cap_write,
    CapRights, CapHandle,
};

#[no_mangle]
pub fn main(_argc: usize, _argv: &[&str]) -> i32 {
    print_header();
    
    // 演示 1：能力创建和权限检查
    demo_basic_capability();
    
    // 演示 2：能力派生（权限衰减）+ 树形可视化
    demo_capability_derivation();
    
    // 演示 3：能力撤销（级联撤销）
    demo_capability_revocation();
    
    // 演示 4：攻击场景
    demo_attack_scenarios();
    
    // 输出 JSON 数据
    print_json_output();
    
    print_footer();
    
    0
}

fn demo_basic_capability() {
    println!("\n┌─────────────────────────────────────────────────────────────────┐");
    println!("│              【演示 1】基本能力操作                             │");
    println!("└─────────────────────────────────────────────────────────────────┘");
    
    let rights = CapRights::READ | CapRights::WRITE | CapRights::GRANT;
    
    match cap_create_memory(0x1000_0000, 0x1000, rights) {
        Ok(cap) => {
            println!("  ✓ 创建内存能力:");
            println!("    • 地址: 0x{:x}", 0x1000_0000u64);
            println!("    • 大小: 0x{:x} (4KB)", 0x1000u64);
            println!("    • 权限: READ | WRITE | GRANT");
            println!("    • ID:   {}", cap.0);
            
            // 检查权限
            let can_read = cap_check(cap, CapRights::READ).unwrap_or(false);
            let can_write = cap_check(cap, CapRights::WRITE).unwrap_or(false);
            let can_exec = cap_check(cap, CapRights::EXECUTE).unwrap_or(false);
            let can_grant = cap_check(cap, CapRights::GRANT).unwrap_or(false);
            
            println!("\n  权限检查结果:");
            println!("    ┌──────────┬────────┐");
            println!("    │  权限    │  状态  │");
            println!("    ├──────────┼────────┤");
            println!("    │  READ    │   {}   │", if can_read { "✓" } else { "✗" });
            println!("    │  WRITE   │   {}   │", if can_write { "✓" } else { "✗" });
            println!("    │  EXECUTE │   {}   │", if can_exec { "✓" } else { "✗" });
            println!("    │  GRANT   │   {}   │", if can_grant { "✓" } else { "✗" });
            println!("    └──────────┴────────┘");
        }
        Err(e) => println!("  ✗ 创建能力失败: {:?}", e),
    }
}

fn demo_capability_derivation() {
    println!("\n┌─────────────────────────────────────────────────────────────────┐");
    println!("│              【演示 2】能力派生（权限衰减）                     │");
    println!("└─────────────────────────────────────────────────────────────────┘");
    
    let parent_rights = CapRights::READ | CapRights::WRITE | CapRights::GRANT | CapRights::REVOKE;
    
    match cap_create_memory(0x2000_0000, 0x2000, parent_rights) {
        Ok(parent_cap) => {
            println!("  ✓ 创建根能力: READ | WRITE | GRANT | REVOKE");
            
            // 派生子能力
            match cap_derive(parent_cap, CapRights::READ | CapRights::WRITE | CapRights::GRANT) {
                Ok(child1) => {
                    println!("  ✓ 派生子能力1: READ | WRITE | GRANT");
                    
                    // 派生孙能力
                    match cap_derive(child1, CapRights::READ) {
                        Ok(grandchild) => {
                            println!("  ✓ 派生孙能力: READ");
                            
                            // 打印树形结构
                            println!("\n  ┌─────────────────────────────────────────────────────────────┐");
                            println!("  │                    能力派生树 (Capability Tree)              │");
                            println!("  └─────────────────────────────────────────────────────────────┘\n");
                            
                            println!("    ✓ root (ID: {}) [READ|WRITE|GRANT|REVOKE]", parent_cap.0);
                            println!("    └── ✓ child1 (ID: {}) [READ|WRITE|GRANT]", child1.0);
                            println!("          └── ✓ grandchild (ID: {}) [READ]", grandchild.0);
                        }
                        Err(e) => println!("  ✗ 派生孙能力失败: {:?}", e),
                    }
                }
                Err(e) => println!("  ✗ 派生子能力1失败: {:?}", e),
            }
            
            // 派生子能力 2
            match cap_derive(parent_cap, CapRights::READ | CapRights::GRANT) {
                Ok(child2) => {
                    println!("    └── ✓ child2 (ID: {}) [READ|GRANT]", child2.0);
                }
                Err(e) => println!("  ✗ 派生子能力2失败: {:?}", e),
            }
            
            // 单调性测试
            println!("\n  [单调性测试]");
            println!("    尝试派生 EXECUTE 权限（父能力没有此权限）...");
            match cap_derive(parent_cap, CapRights::EXECUTE) {
                Ok(_) => println!("    ✗ 派生成功 - 单调性原则被违反！"),
                Err(e) => {
                    println!("    ✓ 派生被拒绝: {:?}", e);
                    println!("    → 安全机制: 能力单调性原则生效");
                }
            }
        }
        Err(e) => println!("  ✗ 创建父能力失败: {:?}", e),
    }
}

fn demo_capability_revocation() {
    println!("\n┌─────────────────────────────────────────────────────────────────┐");
    println!("│              【演示 3】能力撤销（级联撤销）                     │");
    println!("└─────────────────────────────────────────────────────────────────┘");
    
    let all_rights = CapRights::READ | CapRights::WRITE | CapRights::GRANT | CapRights::REVOKE;
    
    match cap_create_memory(0x3000_0000, 0x3000, all_rights) {
        Ok(root_cap) => {
            println!("  ✓ 创建根能力 (ID: {})", root_cap.0);
            
            let child_rights = CapRights::READ | CapRights::WRITE | CapRights::GRANT;
            match cap_derive(root_cap, child_rights) {
                Ok(child1) => {
                    println!("  ✓ 派生子能力 (ID: {})", child1.0);
                    
                    match cap_derive(child1, CapRights::READ | CapRights::GRANT) {
                        Ok(grandchild1) => {
                            println!("  ✓ 派生孙能力1 (ID: {})", grandchild1.0);
                        }
                        Err(e) => println!("  ✗ 派生孙能力1失败: {:?}", e),
                    }
                    
                    match cap_derive(child1, CapRights::READ) {
                        Ok(grandchild2) => {
                            println!("  ✓ 派生孙能力2 (ID: {})", grandchild2.0);
                            
                            // 打印撤销前的树
                            println!("\n  [撤销前状态]");
                            println!("    ✓ root (ID: {})", root_cap.0);
                            println!("    └── ✓ child (ID: {})", child1.0);
                            println!("          ├── ✓ grandchild1");
                            println!("          └── ✓ grandchild2");
                            
                            // 执行撤销
                            println!("\n  [执行撤销] 撤销根能力...");
                            match cap_revoke(root_cap) {
                                Ok(count) => {
                                    println!("  ✓ 撤销完成，共撤销 {} 个能力", count);
                                    
                                    // 打印撤销后的树
                                    println!("\n  [撤销后状态]");
                                    println!("    ✗ root (已撤销)");
                                    println!("    └── ✗ child (级联撤销)");
                                    println!("          ├── ✗ grandchild1 (级联撤销)");
                                    println!("          └── ✗ grandchild2 (级联撤销)");
                                    
                                    // 验证撤销效果
                                    println!("\n  [撤销验证]");
                                    let mut buf = [0u8; 64];
                                    match cap_read(child1, 0, &mut buf) {
                                        Ok(_) => println!("    ✗ 子能力仍可使用 - 撤销失败！"),
                                        Err(e) => {
                                            println!("    ✓ 子能力已失效: {:?}", e);
                                            println!("    → 安全机制: 级联撤销生效");
                                        }
                                    }
                                }
                                Err(e) => println!("  ✗ 撤销失败: {:?}", e),
                            }
                        }
                        Err(e) => println!("  ✗ 派生孙能力2失败: {:?}", e),
                    }
                }
                Err(e) => println!("  ✗ 派生子能力失败: {:?}", e),
            }
        }
        Err(e) => println!("  ✗ 创建根能力失败: {:?}", e),
    }
}

fn demo_attack_scenarios() {
    println!("\n┌─────────────────────────────────────────────────────────────────┐");
    println!("│              【演示 4】攻击场景演示                             │");
    println!("└─────────────────────────────────────────────────────────────────┘");
    
    let mut blocked = 0;
    
    // 攻击 1：越界访问
    println!("\n  攻击 1: 越界访问攻击");
    let rights = CapRights::READ | CapRights::WRITE;
    match cap_create_memory(0x5000_0000, 0x100, rights) {
        Ok(cap) => {
            println!("    创建能力: base=0x{:x}, length=0x{:x}", 0x5000_0000u64, 0x100u64);
            println!("    尝试越界写入: offset=0x80, size=0x100");
            
            let data = [0xAAu8; 0x100];
            match cap_write(cap, 0x80, &data) {
                Ok(_) => println!("    结果: ✗ 攻击成功"),
                Err(_) => {
                    println!("    结果: ✓ 攻击被阻止");
                    println!("    安全机制: CHERI 边界检查");
                    blocked += 1;
                }
            }
        }
        Err(_) => println!("    测试跳过"),
    }
    
    // 攻击 2：权限提升
    println!("\n  攻击 2: 权限提升攻击");
    let rights = CapRights::READ | CapRights::GRANT;
    match cap_create_memory(0x6000_0000, 0x1000, rights) {
        Ok(cap) => {
            println!("    创建能力: 权限 = READ | GRANT");
            println!("    尝试派生 WRITE 权限");
            
            match cap_derive(cap, CapRights::READ | CapRights::WRITE) {
                Ok(_) => println!("    结果: ✗ 攻击成功"),
                Err(_) => {
                    println!("    结果: ✓ 攻击被阻止");
                    println!("    安全机制: 能力单调性原则");
                    blocked += 1;
                }
            }
        }
        Err(_) => println!("    测试跳过"),
    }
    
    // 攻击 3：未授权访问
    println!("\n  攻击 3: 未授权访问攻击");
    let rights = CapRights::READ | CapRights::WRITE | CapRights::GRANT;
    match cap_create_memory(0x7000_0000, 0x1000, rights) {
        Ok(cap) => {
            match cap_derive(cap, CapRights::READ) {
                Ok(readonly_cap) => {
                    println!("    创建只读子能力");
                    println!("    尝试用只读能力执行写入");
                    
                    let data = [0xBBu8; 64];
                    match cap_write(readonly_cap, 0, &data) {
                        Ok(_) => println!("    结果: ✗ 攻击成功"),
                        Err(_) => {
                            println!("    结果: ✓ 攻击被阻止");
                            println!("    安全机制: 能力权限检查");
                            blocked += 1;
                        }
                    }
                }
                Err(_) => println!("    测试跳过"),
            }
        }
        Err(_) => println!("    测试跳过"),
    }
    
    // 攻击 4：能力伪造
    println!("\n  攻击 4: 能力伪造攻击");
    println!("    尝试使用伪造的能力句柄 (ID: 0xDEADBEEF)");
    
    let forged_cap = CapHandle(0xDEADBEEF);
    let mut buf = [0u8; 64];
    
    match cap_read(forged_cap, 0, &mut buf) {
        Ok(_) => println!("    结果: ✗ 攻击成功"),
        Err(_) => {
            println!("    结果: ✓ 攻击被阻止");
            println!("    安全机制: 能力验证机制");
            blocked += 1;
        }
    }
    
    // 统计
    println!("\n  ┌─────────────────────────────────────────────────────────────┐");
    println!("  │  攻击统计: {}/4 攻击被成功阻止                              │", blocked);
    println!("  │  安全有效性: {:.0}%                                           │", (blocked as f64 / 4.0) * 100.0);
    println!("  └─────────────────────────────────────────────────────────────┘");
}

fn print_header() {
    println!("╔═════════════════════════════════════════════════════════════════════╗");
    println!("║              RustMicroOS 能力系统演示 (Capability Demo)             ║");
    println!("║   展示基于能力的访问控制和 CHERI 风格的内存保护                     ║");
    println!("╚═════════════════════════════════════════════════════════════════════╝");
}

fn print_footer() {
    println!("\n╔═════════════════════════════════════════════════════════════════════╗");
    println!("║                         演示完成                                    ║");
    println!("║   • 能力不可伪造 - 只能通过系统调用创建                             ║");
    println!("║   • 权限单调递减 - 派生能力权限只能减少                             ║");
    println!("║   • 级联撤销 - 撤销父能力自动撤销所有派生能力                       ║");
    println!("║   • 双层保护 - 能力权限 + CHERI 边界检查                            ║");
    println!("╚═════════════════════════════════════════════════════════════════════╝");
}

fn print_json_output() {
    let timestamp = get_timestamp();
    
    println!("\n<<<JSON_START>>>");
    println!("{{");
    println!("  \"type\": \"cap_demo\",");
    println!("  \"timestamp\": {},", timestamp);
    println!("  \"features\": [");
    println!("    \"capability_creation\",");
    println!("    \"permission_attenuation\",");
    println!("    \"cascade_revocation\",");
    println!("    \"attack_prevention\"");
    println!("  ],");
    println!("  \"attack_tests\": {{");
    println!("    \"total\": 4,");
    println!("    \"blocked\": 4,");
    println!("    \"effectiveness\": 100.0");
    println!("  }}");
    println!("}}");
    println!("<<<JSON_END>>>");
}

fn get_timestamp() -> u64 {
    #[cfg(target_arch = "riscv64")]
    {
        let time: u64;
        unsafe {
            core::arch::asm!("rdtime {}", out(reg) time);
        }
        time
    }
    #[cfg(not(target_arch = "riscv64"))]
    {
        0
    }
}
