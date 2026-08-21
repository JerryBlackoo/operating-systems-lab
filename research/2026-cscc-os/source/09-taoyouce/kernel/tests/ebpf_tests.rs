//! eBPF 验证器属性测试
//!
//! 验证 eBPF 验证器的以下属性：
//! 1. 内存安全：拒绝越界访问
//! 2. 控制流安全：拒绝无限循环
//! 3. 类型安全：拒绝类型错误
//! 4. 指令合法性：拒绝非法指令
//! 5. 辅助函数调用：验证辅助函数参数

use crate::{SimpleRng, println};
use alloc::vec::Vec;

/// eBPF 指令操作码
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum BpfOpcode {
    // ALU 指令
    Add = 0x07,
    Sub = 0x17,
    Mul = 0x27,
    Div = 0x37,
    Mov = 0xB7,
    
    // 内存指令
    LdxW = 0x61,
    StxW = 0x63,
    
    // 跳转指令
    Ja = 0x05,
    Jeq = 0x15,
    Jgt = 0x25,
    Jge = 0x35,
    
    // 函数调用
    Call = 0x85,
    Exit = 0x95,
}

/// eBPF 指令
#[derive(Debug, Clone, Copy)]
struct BpfInsn {
    opcode: u8,
    dst_reg: u8,
    src_reg: u8,
    offset: i16,
    imm: i32,
}

impl BpfInsn {
    fn new(opcode: u8, dst: u8, src: u8, off: i16, imm: i32) -> Self {
        Self {
            opcode,
            dst_reg: dst,
            src_reg: src,
            offset: off,
            imm,
        }
    }
    
    fn mov_imm(dst: u8, imm: i32) -> Self {
        Self::new(BpfOpcode::Mov as u8, dst, 0, 0, imm)
    }
    
    fn add_imm(dst: u8, imm: i32) -> Self {
        Self::new(BpfOpcode::Add as u8, dst, 0, 0, imm)
    }
    
    fn ldx_w(dst: u8, src: u8, off: i16) -> Self {
        Self::new(BpfOpcode::LdxW as u8, dst, src, off, 0)
    }
    
    fn stx_w(dst: u8, src: u8, off: i16) -> Self {
        Self::new(BpfOpcode::StxW as u8, dst, src, off, 0)
    }
    
    fn ja(off: i16) -> Self {
        Self::new(BpfOpcode::Ja as u8, 0, 0, off, 0)
    }
    
    fn jeq_imm(dst: u8, imm: i32, off: i16) -> Self {
        Self::new(BpfOpcode::Jeq as u8, dst, 0, off, imm)
    }
    
    fn call(func_id: i32) -> Self {
        Self::new(BpfOpcode::Call as u8, 0, 0, 0, func_id)
    }
    
    fn exit() -> Self {
        Self::new(BpfOpcode::Exit as u8, 0, 0, 0, 0)
    }
}

/// 寄存器状态
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RegState {
    Unknown,
    Scalar(i64, i64),  // (min, max)
    Ptr { base: usize, offset: i64, size: usize },
}

/// 验证器状态
struct VerifierState {
    regs: [RegState; 11],
    stack_size: usize,
}

impl VerifierState {
    fn new(stack_size: usize) -> Self {
        let mut regs = [RegState::Unknown; 11];
        // R1 = 上下文指针
        regs[1] = RegState::Ptr { base: 0, offset: 0, size: 64 };
        // R10 = 栈指针
        regs[10] = RegState::Ptr { base: 1, offset: 0, size: stack_size };
        Self { regs, stack_size }
    }
}

/// 验证错误
#[derive(Debug, Clone, PartialEq, Eq)]
enum VerifyError {
    InvalidOpcode(u8),
    InvalidRegister(u8),
    OutOfBoundsAccess,
    UninitializedRegister,
    InfiniteLoop,
    InvalidJumpTarget,
    InvalidHelperCall,
    TypeMismatch,
    DivisionByZero,
}

/// eBPF 验证器
struct BpfVerifier {
    max_insns: usize,
    max_stack: usize,
    allowed_helpers: Vec<i32>,
}

impl BpfVerifier {
    fn new() -> Self {
        Self {
            max_insns: 4096,
            max_stack: 512,
            allowed_helpers: vec![1, 2, 3, 4, 5], // 允许的辅助函数 ID
        }
    }
    
    fn verify(&self, prog: &[BpfInsn]) -> Result<(), VerifyError> {
        if prog.is_empty() {
            return Err(VerifyError::InvalidJumpTarget);
        }
        
        if prog.len() > self.max_insns {
            return Err(VerifyError::InfiniteLoop);
        }
        
        let mut state = VerifierState::new(self.max_stack);
        let mut visited = vec![false; prog.len()];
        
        self.verify_path(prog, 0, &mut state, &mut visited, 0)
    }
    
    fn verify_path(
        &self,
        prog: &[BpfInsn],
        pc: usize,
        state: &mut VerifierState,
        visited: &mut [bool],
        depth: usize,
    ) -> Result<(), VerifyError> {
        if depth > prog.len() * 2 {
            return Err(VerifyError::InfiniteLoop);
        }
        
        if pc >= prog.len() {
            return Err(VerifyError::InvalidJumpTarget);
        }
        
        if visited[pc] {
            return Ok(()); // 已验证过
        }
        visited[pc] = true;
        
        let insn = &prog[pc];
        
        // 验证寄存器
        if insn.dst_reg > 10 || insn.src_reg > 10 {
            return Err(VerifyError::InvalidRegister(insn.dst_reg.max(insn.src_reg)));
        }
        
        match insn.opcode {
            op if op == BpfOpcode::Mov as u8 => {
                state.regs[insn.dst_reg as usize] = RegState::Scalar(insn.imm as i64, insn.imm as i64);
                self.verify_path(prog, pc + 1, state, visited, depth + 1)
            }
            
            op if op == BpfOpcode::Add as u8 => {
                match state.regs[insn.dst_reg as usize] {
                    RegState::Unknown => return Err(VerifyError::UninitializedRegister),
                    RegState::Scalar(min, max) => {
                        state.regs[insn.dst_reg as usize] = RegState::Scalar(
                            min.saturating_add(insn.imm as i64),
                            max.saturating_add(insn.imm as i64),
                        );
                    }
                    RegState::Ptr { base, offset, size } => {
                        state.regs[insn.dst_reg as usize] = RegState::Ptr {
                            base,
                            offset: offset + insn.imm as i64,
                            size,
                        };
                    }
                }
                self.verify_path(prog, pc + 1, state, visited, depth + 1)
            }
            
            op if op == BpfOpcode::Div as u8 => {
                if insn.imm == 0 {
                    return Err(VerifyError::DivisionByZero);
                }
                self.verify_path(prog, pc + 1, state, visited, depth + 1)
            }
            
            op if op == BpfOpcode::LdxW as u8 => {
                // 验证内存访问
                match state.regs[insn.src_reg as usize] {
                    RegState::Ptr { offset, size, .. } => {
                        let access_offset = offset + insn.offset as i64;
                        if access_offset < 0 || access_offset as usize + 4 > size {
                            return Err(VerifyError::OutOfBoundsAccess);
                        }
                    }
                    _ => return Err(VerifyError::TypeMismatch),
                }
                state.regs[insn.dst_reg as usize] = RegState::Scalar(i64::MIN, i64::MAX);
                self.verify_path(prog, pc + 1, state, visited, depth + 1)
            }
            
            op if op == BpfOpcode::StxW as u8 => {
                // 验证内存写入
                match state.regs[insn.dst_reg as usize] {
                    RegState::Ptr { offset, size, .. } => {
                        let access_offset = offset + insn.offset as i64;
                        if access_offset < 0 || access_offset as usize + 4 > size {
                            return Err(VerifyError::OutOfBoundsAccess);
                        }
                    }
                    _ => return Err(VerifyError::TypeMismatch),
                }
                self.verify_path(prog, pc + 1, state, visited, depth + 1)
            }
            
            op if op == BpfOpcode::Ja as u8 => {
                let target = (pc as i64 + 1 + insn.offset as i64) as usize;
                self.verify_path(prog, target, state, visited, depth + 1)
            }
            
            op if op == BpfOpcode::Jeq as u8 => {
                let target = (pc as i64 + 1 + insn.offset as i64) as usize;
                // 验证两个分支
                let mut state_clone = state.clone();
                self.verify_path(prog, pc + 1, state, visited, depth + 1)?;
                self.verify_path(prog, target, &mut state_clone, visited, depth + 1)
            }
            
            op if op == BpfOpcode::Call as u8 => {
                if !self.allowed_helpers.contains(&insn.imm) {
                    return Err(VerifyError::InvalidHelperCall);
                }
                state.regs[0] = RegState::Scalar(i64::MIN, i64::MAX);
                self.verify_path(prog, pc + 1, state, visited, depth + 1)
            }
            
            op if op == BpfOpcode::Exit as u8 => {
                Ok(())
            }
            
            _ => Err(VerifyError::InvalidOpcode(insn.opcode)),
        }
    }
}

/// 属性1：拒绝越界内存访问
/// 验证器应拒绝越界访问
#[test_case]
fn test_reject_out_of_bounds() -> bool {
    let verifier = BpfVerifier::new();
    
    // 越界读取
    let prog = vec![
        BpfInsn::mov_imm(2, 0),
        BpfInsn::ldx_w(3, 1, 1000), // 偏移 1000 超出上下文大小
        BpfInsn::exit(),
    ];
    
    match verifier.verify(&prog) {
        Err(VerifyError::OutOfBoundsAccess) => {}
        _ => {
            println!("  错误: 应拒绝越界访问");
            return false;
        }
    }
    
    true
}

/// 属性2：拒绝无限循环
/// 验证器应检测潜在的无限循环
#[test_case]
fn test_reject_infinite_loop() -> bool {
    let verifier = BpfVerifier::new();
    
    // 无限循环
    let prog = vec![
        BpfInsn::mov_imm(0, 0),
        BpfInsn::ja(-1), // 跳回自己
    ];
    
    match verifier.verify(&prog) {
        Err(VerifyError::InfiniteLoop) => {}
        _ => {
            println!("  错误: 应拒绝无限循环");
            return false;
        }
    }
    
    true
}

/// 属性3：拒绝未初始化寄存器
/// 验证器应拒绝使用未初始化的寄存器
#[test_case]
fn test_reject_uninitialized_register() -> bool {
    let verifier = BpfVerifier::new();
    
    // 使用未初始化的寄存器
    let prog = vec![
        BpfInsn::add_imm(5, 10), // R5 未初始化
        BpfInsn::exit(),
    ];
    
    match verifier.verify(&prog) {
        Err(VerifyError::UninitializedRegister) => {}
        _ => {
            println!("  错误: 应拒绝未初始化寄存器");
            return false;
        }
    }
    
    true
}

/// 属性4：拒绝无效跳转目标
/// 验证器应拒绝跳转到程序外
#[test_case]
fn test_reject_invalid_jump() -> bool {
    let verifier = BpfVerifier::new();
    
    // 跳转到程序外
    let prog = vec![
        BpfInsn::mov_imm(0, 0),
        BpfInsn::ja(100), // 跳转到程序外
        BpfInsn::exit(),
    ];
    
    match verifier.verify(&prog) {
        Err(VerifyError::InvalidJumpTarget) => {}
        _ => {
            println!("  错误: 应拒绝无效跳转");
            return false;
        }
    }
    
    true
}

/// 属性5：拒绝无效辅助函数调用
/// 验证器应拒绝调用未授权的辅助函数
#[test_case]
fn test_reject_invalid_helper() -> bool {
    let verifier = BpfVerifier::new();
    
    // 调用未授权的辅助函数
    let prog = vec![
        BpfInsn::call(999), // 未授权的函数 ID
        BpfInsn::exit(),
    ];
    
    match verifier.verify(&prog) {
        Err(VerifyError::InvalidHelperCall) => {}
        _ => {
            println!("  错误: 应拒绝无效辅助函数调用");
            return false;
        }
    }
    
    true
}

/// 属性6：拒绝除零
/// 验证器应拒绝除以零
#[test_case]
fn test_reject_division_by_zero() -> bool {
    let verifier = BpfVerifier::new();
    
    // 除以零
    let prog = vec![
        BpfInsn::new(BpfOpcode::Div as u8, 0, 0, 0, 0), // 除以 0
        BpfInsn::exit(),
    ];
    
    match verifier.verify(&prog) {
        Err(VerifyError::DivisionByZero) => {}
        _ => {
            println!("  错误: 应拒绝除零");
            return false;
        }
    }
    
    true
}

/// 属性7：接受有效程序
/// 验证器应接受有效的程序
#[test_case]
fn test_accept_valid_program() -> bool {
    let verifier = BpfVerifier::new();
    
    // 有效程序
    let prog = vec![
        BpfInsn::mov_imm(0, 0),
        BpfInsn::ldx_w(2, 1, 0), // 从上下文读取
        BpfInsn::add_imm(2, 1),
        BpfInsn::exit(),
    ];
    
    match verifier.verify(&prog) {
        Ok(()) => {}
        Err(e) => {
            println!("  错误: 有效程序应通过验证: {:?}", e);
            return false;
        }
    }
    
    true
}

/// 属性8：拒绝无效寄存器
/// 验证器应拒绝使用无效寄存器号
#[test_case]
fn test_reject_invalid_register() -> bool {
    let verifier = BpfVerifier::new();
    
    // 使用无效寄存器
    let prog = vec![
        BpfInsn::new(BpfOpcode::Mov as u8, 15, 0, 0, 0), // R15 不存在
        BpfInsn::exit(),
    ];
    
    match verifier.verify(&prog) {
        Err(VerifyError::InvalidRegister(15)) => {}
        _ => {
            println!("  错误: 应拒绝无效寄存器");
            return false;
        }
    }
    
    true
}

/// 属性9：类型检查
/// 验证器应检查指针和标量的类型
#[test_case]
fn test_type_checking() -> bool {
    let verifier = BpfVerifier::new();
    
    // 尝试从标量加载（类型错误）
    let prog = vec![
        BpfInsn::mov_imm(2, 100), // R2 = 标量
        BpfInsn::ldx_w(3, 2, 0),  // 尝试从标量加载
        BpfInsn::exit(),
    ];
    
    match verifier.verify(&prog) {
        Err(VerifyError::TypeMismatch) => {}
        _ => {
            println!("  错误: 应检测类型错误");
            return false;
        }
    }
    
    true
}

/// 属性10：条件分支验证
/// 验证器应验证条件分支的两个路径
#[test_case]
fn test_conditional_branch_verification() -> bool {
    let verifier = BpfVerifier::new();
    
    // 条件分支，一个路径有效，一个无效
    let prog = vec![
        BpfInsn::mov_imm(0, 0),
        BpfInsn::jeq_imm(0, 0, 2), // 如果 R0 == 0，跳过 2 条指令
        BpfInsn::exit(),           // 正常退出
        BpfInsn::ja(100),          // 无效跳转（会被执行如果条件为真）
        BpfInsn::exit(),
    ];
    
    // 由于一个分支无效，整个程序应该失败
    match verifier.verify(&prog) {
        Err(VerifyError::InvalidJumpTarget) => {}
        _ => {
            println!("  错误: 应验证所有分支");
            return false;
        }
    }
    
    true
}
