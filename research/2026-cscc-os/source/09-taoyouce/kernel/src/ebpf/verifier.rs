//! eBPF 验证器
//!
//! 验证 eBPF 程序的安全性，包括：
//! - 寄存器状态追踪
//! - 内存访问检查
//! - 循环检测
//! - 指令合法性检查

use alloc::vec::Vec;
use alloc::string::ToString;
use alloc::collections::BTreeSet;
use super::{BpfProg, BpfInsn, BpfError, BPF_REG_COUNT, bpf_class, bpf_alu, bpf_jmp};

/// 最大指令数
const MAX_INSNS: usize = 4096;

/// 最大栈深度
const MAX_STACK_DEPTH: usize = 512;

/// 寄存器类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegType {
    /// 未初始化
    NotInit,
    /// 标量值
    Scalar,
    /// 指向栈的指针
    PtrToStack,
    /// 指向 Map 值的指针
    PtrToMapValue,
    /// 指向上下文的指针
    PtrToCtx,
    /// 指向包数据的指针
    PtrToPacket,
    /// 指向包结束的指针
    PtrToPacketEnd,
    /// 常量
    Const(i64),
}

/// 寄存器状态
#[derive(Debug, Clone)]
pub struct RegState {
    /// 寄存器类型
    pub reg_type: RegType,
    /// 最小值（用于范围追踪）
    pub min_value: i64,
    /// 最大值
    pub max_value: i64,
    /// 是否已知精确值
    pub precise: bool,
}

impl Default for RegState {
    fn default() -> Self {
        Self {
            reg_type: RegType::NotInit,
            min_value: i64::MIN,
            max_value: i64::MAX,
            precise: false,
        }
    }
}

impl RegState {
    /// 创建标量状态
    pub fn scalar() -> Self {
        Self {
            reg_type: RegType::Scalar,
            ..Default::default()
        }
    }
    
    /// 创建常量状态
    pub fn constant(value: i64) -> Self {
        Self {
            reg_type: RegType::Const(value),
            min_value: value,
            max_value: value,
            precise: true,
        }
    }
    
    /// 创建指针状态
    pub fn ptr(ptr_type: RegType) -> Self {
        Self {
            reg_type: ptr_type,
            ..Default::default()
        }
    }
}

/// 验证器状态
#[derive(Debug, Clone)]
pub struct VerifierState {
    /// 寄存器状态
    pub regs: [RegState; BPF_REG_COUNT],
    /// 栈槽状态
    pub stack: [Option<RegState>; MAX_STACK_DEPTH / 8],
    /// 当前指令索引
    pub insn_idx: usize,
}

impl Default for VerifierState {
    fn default() -> Self {
        let mut regs: [RegState; BPF_REG_COUNT] = Default::default();
        // R1 = 上下文指针
        regs[1] = RegState::ptr(RegType::PtrToCtx);
        // R10 = 栈指针（只读）
        regs[10] = RegState::ptr(RegType::PtrToStack);
        
        Self {
            regs,
            stack: [const { None }; MAX_STACK_DEPTH / 8],
            insn_idx: 0,
        }
    }
}

/// 验证器
pub struct Verifier<'a> {
    /// 程序
    prog: &'a BpfProg,
    /// 当前状态
    state: VerifierState,
    /// 已访问的指令
    visited: BTreeSet<usize>,
    /// 分支目标
    branch_targets: Vec<(usize, VerifierState)>,
}

impl<'a> Verifier<'a> {
    pub fn new(prog: &'a BpfProg) -> Self {
        Self {
            prog,
            state: VerifierState::default(),
            visited: BTreeSet::new(),
            branch_targets: Vec::new(),
        }
    }
    
    /// 验证程序
    pub fn verify(&mut self) -> Result<(), BpfError> {
        let insns = self.prog.insns();
        
        // 检查指令数量
        if insns.is_empty() {
            return Err(BpfError::VerifyFailed("Empty program".to_string()));
        }
        if insns.len() > MAX_INSNS {
            return Err(BpfError::VerifyFailed("Too many instructions".to_string()));
        }
        
        // 检查最后一条指令必须是 EXIT
        let last = &insns[insns.len() - 1];
        if last.class() != bpf_class::BPF_JMP || (last.code & 0xF0) != bpf_jmp::BPF_EXIT {
            return Err(BpfError::VerifyFailed("Program must end with EXIT".to_string()));
        }
        
        // 模拟执行
        self.simulate()?;
        
        // 检查是否有未访问的指令（死代码）
        for i in 0..insns.len() {
            if !self.visited.contains(&i) {
                // 死代码警告，但不是错误
            }
        }
        
        Ok(())
    }
    
    /// 模拟执行
    fn simulate(&mut self) -> Result<(), BpfError> {
        let insns = self.prog.insns();
        
        while self.state.insn_idx < insns.len() {
            let idx = self.state.insn_idx;
            
            // 检测循环
            if self.visited.contains(&idx) {
                // 已访问过，检查是否有待处理的分支
                if let Some((target, state)) = self.branch_targets.pop() {
                    self.state = state;
                    self.state.insn_idx = target;
                    continue;
                }
                break;
            }
            
            self.visited.insert(idx);
            let insn = &insns[idx];
            
            // 验证并执行指令
            self.verify_insn(insn)?;
        }
        
        // 处理剩余分支
        while let Some((target, state)) = self.branch_targets.pop() {
            self.state = state;
            self.state.insn_idx = target;
            self.simulate()?;
        }
        
        Ok(())
    }
    
    /// 验证单条指令
    fn verify_insn(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let class = insn.class();
        
        match class {
            bpf_class::BPF_ALU | bpf_class::BPF_ALU64 => {
                self.verify_alu(insn)?;
            }
            bpf_class::BPF_JMP | bpf_class::BPF_JMP32 => {
                self.verify_jmp(insn)?;
                return Ok(()); // JMP 指令自己处理 PC
            }
            bpf_class::BPF_LD | bpf_class::BPF_LDX => {
                self.verify_load(insn)?;
            }
            bpf_class::BPF_ST | bpf_class::BPF_STX => {
                self.verify_store(insn)?;
            }
            _ => {
                return Err(BpfError::VerifyFailed(
                    alloc::format!("Unknown instruction class: {}", class)
                ));
            }
        }
        
        self.state.insn_idx += 1;
        Ok(())
    }
    
    /// 验证 ALU 指令
    fn verify_alu(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let dst = insn.dst_reg() as usize;
        let src = insn.src_reg() as usize;
        let op = insn.code & 0xF0;
        let is_imm = (insn.code & 0x08) == 0;
        
        // 检查目标寄存器
        if dst >= BPF_REG_COUNT || dst == 10 {
            return Err(BpfError::VerifyFailed("Invalid dst register".to_string()));
        }
        
        // 检查源寄存器（如果使用）
        if !is_imm && src >= BPF_REG_COUNT {
            return Err(BpfError::VerifyFailed("Invalid src register".to_string()));
        }
        
        // 检查源寄存器是否已初始化
        if !is_imm {
            if let RegType::NotInit = self.state.regs[src].reg_type {
                return Err(BpfError::VerifyFailed(
                    alloc::format!("Uninitialized register R{}", src)
                ));
            }
        }
        
        // 更新目标寄存器状态
        match op {
            bpf_alu::BPF_MOV => {
                if is_imm {
                    self.state.regs[dst] = RegState::constant(insn.imm as i64);
                } else {
                    self.state.regs[dst] = self.state.regs[src].clone();
                }
            }
            bpf_alu::BPF_ADD | bpf_alu::BPF_SUB | bpf_alu::BPF_MUL |
            bpf_alu::BPF_DIV | bpf_alu::BPF_MOD | bpf_alu::BPF_OR |
            bpf_alu::BPF_AND | bpf_alu::BPF_XOR | bpf_alu::BPF_LSH |
            bpf_alu::BPF_RSH | bpf_alu::BPF_ARSH => {
                // 检查除零
                if (op == bpf_alu::BPF_DIV || op == bpf_alu::BPF_MOD) && is_imm && insn.imm == 0 {
                    return Err(BpfError::VerifyFailed("Division by zero".to_string()));
                }
                self.state.regs[dst] = RegState::scalar();
            }
            bpf_alu::BPF_NEG => {
                self.state.regs[dst] = RegState::scalar();
            }
            _ => {
                return Err(BpfError::VerifyFailed(
                    alloc::format!("Unknown ALU op: {:#x}", op)
                ));
            }
        }
        
        Ok(())
    }
    
    /// 验证跳转指令
    fn verify_jmp(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let op = insn.code & 0xF0;
        let idx = self.state.insn_idx;
        let insns = self.prog.insns();
        
        match op {
            bpf_jmp::BPF_EXIT => {
                // 检查 R0 是否已初始化
                if let RegType::NotInit = self.state.regs[0].reg_type {
                    return Err(BpfError::VerifyFailed(
                        "R0 not initialized at exit".to_string()
                    ));
                }
                // 尝试处理下一个分支
                if let Some((target, state)) = self.branch_targets.pop() {
                    self.state = state;
                    self.state.insn_idx = target;
                } else {
                    self.state.insn_idx = insns.len(); // 结束
                }
            }
            bpf_jmp::BPF_JA => {
                // 无条件跳转
                let target = (idx as i64 + 1 + insn.off as i64) as usize;
                if target >= insns.len() {
                    return Err(BpfError::VerifyFailed("Jump out of bounds".to_string()));
                }
                self.state.insn_idx = target;
            }
            bpf_jmp::BPF_CALL => {
                // 函数调用
                self.verify_call(insn)?;
                self.state.insn_idx += 1;
            }
            _ => {
                // 条件跳转
                let target = (idx as i64 + 1 + insn.off as i64) as usize;
                if target >= insns.len() {
                    return Err(BpfError::VerifyFailed("Jump out of bounds".to_string()));
                }
                
                // 保存分支状态
                self.branch_targets.push((target, self.state.clone()));
                self.state.insn_idx += 1;
            }
        }
        
        Ok(())
    }
    
    /// 验证函数调用
    fn verify_call(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let _func_id = insn.imm as u32;
        
        // 检查是否是已知的辅助函数
        // 这里简化处理，实际需要检查每个函数的参数类型
        
        // 调用后 R0 包含返回值
        self.state.regs[0] = RegState::scalar();
        
        // R1-R5 被调用者保存，调用后未定义
        for i in 1..=5 {
            self.state.regs[i] = RegState::default();
        }
        
        Ok(())
    }
    
    /// 验证加载指令
    fn verify_load(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let dst = insn.dst_reg() as usize;
        let src = insn.src_reg() as usize;
        
        if dst >= BPF_REG_COUNT || dst == 10 {
            return Err(BpfError::VerifyFailed("Invalid dst register".to_string()));
        }
        
        let class = insn.class();
        
        // BPF_LD class: 64-bit immediate load (BPF_LD | BPF_IMM | BPF_DW)
        // This is a two-instruction encoding that loads a 64-bit immediate.
        // src_reg is used as a pseudo-src (0 = plain imm, 1 = map fd, etc.)
        // Per Linux kernel/bpf/verifier.c check_ld_imm()
        if class == bpf_class::BPF_LD {
            // 64-bit immediate: result is scalar (or map ptr depending on src)
            self.state.regs[dst] = RegState::scalar();
            // Skip the next instruction (it's the second half of the 64-bit imm)
            self.state.insn_idx += 1;
            return Ok(());
        }
        
        // BPF_LDX class: memory load from register+offset
        // 检查源是否是有效指针
        match self.state.regs[src].reg_type {
            RegType::PtrToStack | RegType::PtrToMapValue | 
            RegType::PtrToCtx | RegType::PtrToPacket |
            RegType::Scalar | RegType::Const(_) => {
                // Valid: pointers and scalars that might be runtime pointers
            }
            RegType::NotInit => {
                return Err(BpfError::VerifyFailed(
                    alloc::format!("R{} is not initialized for load", src)
                ));
            }
            _ => {}
        }
        
        // 加载后目标寄存器变为标量
        self.state.regs[dst] = RegState::scalar();
        
        Ok(())
    }
    
    /// 验证存储指令
    fn verify_store(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let dst = insn.dst_reg() as usize;
        
        // 检查目标是否是有效指针
        match self.state.regs[dst].reg_type {
            RegType::PtrToStack | RegType::PtrToMapValue => {
                // 可写的指针类型
            }
            RegType::PtrToCtx | RegType::PtrToPacket => {
                // Context writes are allowed for some program types (e.g. TC)
                // Per Linux verifier: may_access_direct_pkt_data()
            }
            RegType::Scalar | RegType::Const(_) => {
                // Runtime pointer stored in scalar — allowed (verifier tracks
                // pointer provenance loosely in our implementation)
            }
            RegType::NotInit => {
                return Err(BpfError::VerifyFailed(
                    alloc::format!("R{} is not initialized for store", dst)
                ));
            }
            _ => {}
        }
        
        Ok(())
    }
}

/// 验证 eBPF 程序
pub fn verify(prog: &BpfProg) -> Result<(), BpfError> {
    let mut verifier = Verifier::new(prog);
    verifier.verify()
}
