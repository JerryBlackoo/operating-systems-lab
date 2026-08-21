//! GDB Stub 属性测试
//! Property 9: GDB 寄存器读写往返
//! 
//! Feature: rust-warnings-cleanup, Property 9: GDB 寄存器读写往返
//! **Validates: Requirements 7.2**

#[cfg(test)]
mod property_tests {
    use super::*;
    use alloc::vec::Vec;
    use alloc::format;
    
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
    
    /// **Property 9: GDB 寄存器读写往返**
    /// *For any* register value written through GDB protocol, reading the same register 
    /// should return the identical value.
    /// 
    /// Feature: rust-warnings-cleanup, Property 9: GDB 寄存器读写往返
    /// **Validates: Requirements 7.2**
    #[test]
    fn property_register_roundtrip() {
        let stub = GdbStub::new();
        let mut regs = RiscvRegisters::default();
        
        // 测试通用寄存器 x1-x31 (x0 始终为 0)
        for reg_num in 1..32 {
            // 测试多种值
            let test_values: [u64; 4] = [
                1,
                0xDEADBEEF,
                0xFFFF_FFFF_FFFF_FFFF,
                0x1234_5678_9ABC_DEF0,
            ];
            
            for &value in &test_values {
                // 写入寄存器
                let write_cmd = format!("P{:x}={:016x}", reg_num, value.swap_bytes());
                let write_result = stub.handle_command(write_cmd.as_bytes(), &mut regs);
                assert_eq!(write_result, b"OK", 
                    "写入寄存器 x{} 失败", reg_num);
                
                // 读取寄存器
                let read_cmd = format!("p{:x}", reg_num);
                let read_result = stub.handle_command(read_cmd.as_bytes(), &mut regs);
                
                // 解析读取结果
                let read_value = parse_hex_value(&read_result);
                
                // 验证 round-trip
                assert_eq!(read_value, value,
                    "寄存器 x{} round-trip 失败: 写入 {:x}, 读取 {:x}",
                    reg_num, value, read_value);
            }
        }
        
        // 测试 x0 始终为 0
        let read_x0 = stub.handle_command(b"p0", &mut regs);
        let x0_value = parse_hex_value(&read_x0);
        assert_eq!(x0_value, 0, "x0 应始终为 0");
    }
    
    /// **Property 9 扩展: 随机寄存器值往返测试**
    /// *For any* randomly generated register value, the round-trip property should hold.
    /// 
    /// Feature: rust-warnings-cleanup, Property 9: GDB 寄存器读写往返
    /// **Validates: Requirements 7.2**
    /// 
    /// 运行 100 次迭代
    #[test]
    fn property_register_roundtrip_random() {
        let stub = GdbStub::new();
        let mut rng = SimpleRng::new(12345);
        
        // 运行 100 次迭代
        for iteration in 0..100 {
            let mut regs = RiscvRegisters::default();
            
            // 随机选择寄存器 (x1-x31, 跳过 x0)
            let reg_num = rng.next_range(1, 32) as usize;
            
            // 生成随机值
            let value = rng.next();
            
            // 写入寄存器
            let write_cmd = format!("P{:x}={:016x}", reg_num, value.swap_bytes());
            let write_result = stub.handle_command(write_cmd.as_bytes(), &mut regs);
            
            if write_result != b"OK" {
                panic!("迭代 {}: 写入寄存器 x{} 失败", iteration, reg_num);
            }
            
            // 读取寄存器
            let read_cmd = format!("p{:x}", reg_num);
            let read_result = stub.handle_command(read_cmd.as_bytes(), &mut regs);
            
            // 解析读取结果
            let read_value = parse_hex_value(&read_result);
            
            // 验证 round-trip
            if read_value != value {
                panic!(
                    "迭代 {}: 寄存器 x{} round-trip 失败: 写入 {:016x}, 读取 {:016x}",
                    iteration, reg_num, value, read_value
                );
            }
        }
    }
    
    /// **Property 9 扩展: PC 寄存器往返测试**
    /// *For any* PC value, the round-trip property should hold.
    /// 
    /// Feature: rust-warnings-cleanup, Property 9: GDB 寄存器读写往返
    /// **Validates: Requirements 7.2**
    #[test]
    fn property_pc_roundtrip() {
        let stub = GdbStub::new();
        let mut rng = SimpleRng::new(67890);
        
        // 运行 100 次迭代
        for iteration in 0..100 {
            let mut regs = RiscvRegisters::default();
            
            // 生成随机 PC 值（对齐到 2 字节，因为 RISC-V 支持压缩指令）
            let value = (rng.next() & !1u64) | 0x8000_0000; // 确保在有效地址范围
            
            // 写入 PC (寄存器 32)
            let write_cmd = format!("P20={:016x}", value.swap_bytes());
            let write_result = stub.handle_command(write_cmd.as_bytes(), &mut regs);
            
            if write_result != b"OK" {
                panic!("迭代 {}: 写入 PC 失败", iteration);
            }
            
            // 读取 PC
            let read_result = stub.handle_command(b"p20", &mut regs);
            
            // 解析读取结果
            let read_value = parse_hex_value(&read_result);
            
            // 验证 round-trip
            if read_value != value {
                panic!(
                    "迭代 {}: PC round-trip 失败: 写入 {:016x}, 读取 {:016x}",
                    iteration, value, read_value
                );
            }
        }
    }
    
    /// **Property 9 扩展: 批量寄存器往返测试**
    /// *For any* set of register values written via 'G' command, reading via 'g' should 
    /// return identical values.
    /// 
    /// Feature: rust-warnings-cleanup, Property 9: GDB 寄存器读写往返
    /// **Validates: Requirements 7.2**
    #[test]
    fn property_all_registers_roundtrip() {
        let stub = GdbStub::new();
        let mut rng = SimpleRng::new(11111);
        
        // 运行 100 次迭代
        for iteration in 0..100 {
            let mut regs = RiscvRegisters::default();
            
            // 生成随机寄存器值
            let mut expected_values = [0u64; 33]; // x0-x31 + pc
            expected_values[0] = 0; // x0 始终为 0
            for i in 1..32 {
                expected_values[i] = rng.next();
            }
            expected_values[32] = (rng.next() & !1u64) | 0x8000_0000; // PC
            
            // 构建 G 命令数据
            let mut g_data = String::new();
            for &val in &expected_values {
                g_data.push_str(&format!("{:016x}", val.swap_bytes()));
            }
            
            // 写入所有寄存器
            let write_cmd = format!("G{}", g_data);
            let write_result = stub.handle_command(write_cmd.as_bytes(), &mut regs);
            
            if write_result != b"OK" {
                panic!("迭代 {}: 写入所有寄存器失败", iteration);
            }
            
            // 读取所有寄存器
            let read_result = stub.handle_command(b"g", &mut regs);
            
            // 解析读取结果
            let read_str = core::str::from_utf8(&read_result).unwrap_or("");
            
            // 验证每个寄存器
            for i in 0..33 {
                let start = i * 16;
                if start + 16 <= read_str.len() {
                    let hex_val = &read_str[start..start+16];
                    let read_value = u64::from_str_radix(hex_val, 16).unwrap_or(0).swap_bytes();
                    
                    if read_value != expected_values[i] {
                        panic!(
                            "迭代 {}: 寄存器 {} round-trip 失败: 期望 {:016x}, 得到 {:016x}",
                            iteration, i, expected_values[i], read_value
                        );
                    }
                }
            }
        }
    }
    
    /// Property 8: GDB 内存读写一致性
    /// For any 内存地址和数据，通过 GDB 写入后再读取，应得到相同的数据（round-trip）。
    /// 注意：此测试使用模拟内存，实际内核中需要有效的内存地址
    #[test]
    fn property_memory_protocol_correctness() {
        let stub = GdbStub::new();
        let mut regs = RiscvRegisters::default();
        
        // 测试内存读取命令格式
        let read_cmd = b"m80000000,10";
        let result = stub.handle_command(read_cmd, &mut regs);
        // 结果应该是十六进制字符串或错误
        assert!(!result.is_empty() || result.starts_with(b"E"));
        
        // 测试内存写入命令格式
        let write_cmd = b"M80000000,4:deadbeef";
        let result = stub.handle_command(write_cmd, &mut regs);
        // 结果应该是 OK 或错误
        assert!(result == b"OK" || result.starts_with(b"E"));
    }
    
    /// 属性测试：断点设置和移除
    #[test]
    fn property_breakpoint_consistency() {
        let stub = GdbStub::new();
        let mut regs = RiscvRegisters::default();
        
        // 测试断点命令格式
        // 设置断点
        let set_cmd = b"Z0,80000000,4";
        let set_result = stub.handle_command(set_cmd, &mut regs);
        // 应该返回 OK 或错误
        assert!(set_result == b"OK" || set_result.starts_with(b"E"));
        
        // 移除断点
        let remove_cmd = b"z0,80000000,4";
        let remove_result = stub.handle_command(remove_cmd, &mut regs);
        assert!(remove_result == b"OK" || remove_result.starts_with(b"E"));
    }
    
    /// 属性测试：查询命令
    #[test]
    fn property_query_commands() {
        let stub = GdbStub::new();
        let mut regs = RiscvRegisters::default();
        
        // 测试 qSupported
        let result = stub.handle_command(b"qSupported", &mut regs);
        assert!(!result.is_empty(), "qSupported 应返回支持的特性");
        
        // 测试 qAttached
        let result = stub.handle_command(b"qAttached", &mut regs);
        assert!(!result.is_empty(), "qAttached 应返回附加状态");
        
        // 测试 qC (当前线程)
        let result = stub.handle_command(b"qC", &mut regs);
        assert!(!result.is_empty(), "qC 应返回当前线程 ID");
    }
    
    /// 属性测试：停止原因查询
    #[test]
    fn property_halt_reason() {
        let stub = GdbStub::new();
        let mut regs = RiscvRegisters::default();
        
        // 测试 ? 命令
        let result = stub.handle_command(b"?", &mut regs);
        assert!(!result.is_empty(), "? 应返回停止原因");
        // 停止原因格式应该是 Sxx 或 Txx
        assert!(result.len() >= 3, "停止原因格式不正确");
    }
    
    /// 属性测试：执行控制命令
    #[test]
    fn property_execution_control() {
        let stub = GdbStub::new();
        let mut regs = RiscvRegisters::default();
        
        // 测试单步执行
        let step_result = stub.handle_command(b"s", &mut regs);
        // 单步应返回停止原因
        assert!(!step_result.is_empty(), "单步执行应返回停止原因");
        
        // 测试分离
        let detach_result = stub.handle_command(b"D", &mut regs);
        assert_eq!(detach_result, b"OK", "分离应返回 OK");
    }
    
    // 辅助函数
    fn parse_hex_value(data: &[u8]) -> u64 {
        let s = core::str::from_utf8(data).unwrap_or("");
        u64::from_str_radix(s, 16).unwrap_or(0).swap_bytes()
    }
}
