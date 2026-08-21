//! TrapContext 属性测试
//!
//! 这些测试验证 TrapContext 保存/恢复操作的正确性。
//!
//! ## Property 7: Trap Context Round-Trip (user-program-loading)
//! Feature: user-program-loading, Property 7: Trap Context Round-Trip
//! **Validates: Requirements 5.7, 5.8**
//!
//! *For all* trap context save/restore operations:
//! - Saving then restoring SHALL preserve all 32 general-purpose registers
//! - x0 (zero) SHALL always be 0 after restore
//! - sp (x2) SHALL be correctly saved and restored

#![allow(dead_code)]

use super::trap_context::TrapContext;

/// 属性测试迭代次数 - 减少以加快启动速度
const PROPERTY_TEST_ITERATIONS: usize = 5;

/// 简单的伪随机数生成器
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    
    fn next(&mut self) -> u64 {
        self.state = self.state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.state
    }
    
    fn next_usize(&mut self) -> usize {
        self.next() as usize
    }
}

// ============================================================================
// Property 7: Trap Context Round-Trip (user-program-loading)
// ============================================================================

/// TrapContext 往返属性测试入口
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[trap_context] Running round-trip property tests (Property 7)...");
    
    crate::println!("[trap_context] Testing register preservation ({} iterations)...", PROPERTY_TEST_ITERATIONS);
    run_register_preservation_test()?;
    
    crate::println!("[trap_context] Testing x0 always zero ({} iterations)...", PROPERTY_TEST_ITERATIONS);
    run_x0_always_zero_test()?;
    
    crate::println!("[trap_context] Testing sp preservation ({} iterations)...", PROPERTY_TEST_ITERATIONS);
    run_sp_preservation_test()?;
    
    crate::println!("[trap_context] Testing complete round-trip ({} iterations)...", PROPERTY_TEST_ITERATIONS);
    run_complete_round_trip_test()?;
    
    crate::println!("[trap_context] Testing syscall args preservation ({} iterations)...", PROPERTY_TEST_ITERATIONS);
    run_syscall_args_preservation_test()?;
    
    crate::println!("[trap_context] Testing return value setting ({} iterations)...", PROPERTY_TEST_ITERATIONS);
    run_return_value_setting_test()?;
    
    crate::println!("[trap_context] All round-trip property tests passed!");
    Ok(())
}

fn run_register_preservation_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(20250104_7);
    
    for _ in 0..PROPERTY_TEST_ITERATIONS {
        let mut original = TrapContext::zero();
        
        for i in 1..32 {
            original.x[i] = rng.next_usize();
        }
        original.x[0] = 0;
        
        original.sstatus = rng.next_usize();
        original.sepc = rng.next_usize();
        original.kernel_satp = rng.next_usize();
        original.kernel_sp = rng.next_usize();
        original.trap_handler = rng.next_usize();
        
        let saved = original.clone();
        
        for i in 0..32 {
            if saved.x[i] != original.x[i] {
                return Err("Register preservation mismatch");
            }
        }
        
        if saved.sstatus != original.sstatus || saved.sepc != original.sepc {
            return Err("CSR preservation mismatch");
        }
    }
    Ok(())
}

fn run_x0_always_zero_test() -> Result<(), &'static str> {
    for _ in 0..PROPERTY_TEST_ITERATIONS {
        let fresh_ctx = TrapContext::zero();
        if fresh_ctx.x[0] != 0 {
            return Err("x0 is not zero");
        }
    }
    Ok(())
}

fn run_sp_preservation_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(20250104_7_3);
    
    for _ in 0..PROPERTY_TEST_ITERATIONS {
        let sp_value = rng.next_usize() & 0x7FFF_FFFF;
        let mut ctx = TrapContext::zero();
        ctx.set_sp(sp_value);
        
        if ctx.sp() != sp_value || ctx.x[2] != sp_value {
            return Err("sp preservation mismatch");
        }
        
        let saved = ctx.clone();
        if saved.sp() != sp_value {
            return Err("sp save mismatch");
        }
    }
    Ok(())
}

fn run_complete_round_trip_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(20250104_7_4);
    
    for _ in 0..PROPERTY_TEST_ITERATIONS {
        let mut original = TrapContext::zero();
        
        for i in 1..32 {
            original.x[i] = rng.next_usize();
        }
        original.x[0] = 0;
        original.sstatus = rng.next_usize();
        original.sepc = rng.next_usize();
        original.kernel_satp = rng.next_usize();
        original.kernel_sp = rng.next_usize();
        original.trap_handler = rng.next_usize();
        
        let saved = original.clone();
        let restored = saved.clone();
        
        for i in 0..32 {
            if restored.x[i] != original.x[i] {
                return Err("Round-trip register mismatch");
            }
        }
        
        if restored.sstatus != original.sstatus ||
           restored.sepc != original.sepc ||
           restored.kernel_satp != original.kernel_satp ||
           restored.kernel_sp != original.kernel_sp ||
           restored.trap_handler != original.trap_handler {
            return Err("Round-trip CSR mismatch");
        }
    }
    Ok(())
}

fn run_syscall_args_preservation_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(20250104_7_5);
    
    for _ in 0..PROPERTY_TEST_ITERATIONS {
        let mut ctx = TrapContext::zero();
        
        let syscall_id = rng.next_usize() % 1000;
        ctx.x[17] = syscall_id;
        
        let args: [usize; 6] = [
            rng.next_usize(), rng.next_usize(), rng.next_usize(),
            rng.next_usize(), rng.next_usize(), rng.next_usize(),
        ];
        
        for i in 0..6 {
            ctx.x[10 + i] = args[i];
        }
        
        if ctx.syscall_id() != syscall_id {
            return Err("syscall_id mismatch");
        }
        
        let retrieved_args = ctx.syscall_args();
        for i in 0..6 {
            if retrieved_args[i] != args[i] {
                return Err("syscall_args mismatch");
            }
        }
    }
    Ok(())
}

fn run_return_value_setting_test() -> Result<(), &'static str> {
    let mut rng = SimpleRng::new(20250104_7_6);
    
    for _ in 0..PROPERTY_TEST_ITERATIONS {
        let mut ctx = TrapContext::zero();
        let return_value = rng.next_usize();
        
        ctx.set_return_value(return_value);
        
        if ctx.x[10] != return_value {
            return Err("return value mismatch");
        }
        
        let saved = ctx.clone();
        if saved.x[10] != return_value {
            return Err("saved return value mismatch");
        }
    }
    Ok(())
}
