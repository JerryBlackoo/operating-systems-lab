//! eBPF JIT 编译器
//!
//! 将 eBPF 字节码编译为 RISC-V 机器码

use alloc::vec::Vec;
use alloc::vec;
use alloc::string::ToString;
use super::{BpfProg, BpfInsn, BpfError, bpf_class, bpf_alu, bpf_jmp};

/// RISC-V 寄存器映射
/// eBPF R0-R10 -> RISC-V a0-a7, s0-s2
const REG_MAP: [u8; 11] = [
    10, // R0 -> a0
    11, // R1 -> a1
    12, // R2 -> a2
    13, // R3 -> a3
    14, // R4 -> a4
    15, // R5 -> a5
    16, // R6 -> a6
    17, // R7 -> a7
    8,  // R8 -> s0
    9,  // R9 -> s1
    2,  // R10 -> sp (栈指针)
];

/// JIT 编译器
pub struct JitCompiler<'a> {
    prog: &'a BpfProg,
    code: Vec<u8>,
    /// 指令偏移表（eBPF 指令索引 -> 机器码偏移）
    offsets: Vec<usize>,
}

impl<'a> JitCompiler<'a> {
    pub fn new(prog: &'a BpfProg) -> Self {
        Self {
            prog,
            code: Vec::new(),
            offsets: Vec::new(),
        }
    }
    
    /// 编译程序
    pub fn compile(&mut self) -> Result<Vec<u8>, BpfError> {
        let insns = self.prog.insns();
        
        // 生成序言
        self.emit_prologue();
        
        // 记录每条指令的偏移
        self.offsets = vec![0; insns.len()];
        
        // 编译每条指令
        for (i, insn) in insns.iter().enumerate() {
            self.offsets[i] = self.code.len();
            self.compile_insn(insn, i)?;
        }
        
        // 生成尾声
        self.emit_epilogue();
        
        Ok(self.code.clone())
    }
    
    /// 生成序言
    fn emit_prologue(&mut self) {
        // 保存 callee-saved 寄存器
        // addi sp, sp, -64
        self.emit32(0xfc010113);
        // sd ra, 56(sp)
        self.emit32(0x02113c23);
        // sd s0, 48(sp)
        self.emit32(0x02813823);
        // sd s1, 40(sp)
        self.emit32(0x02913423);
        
        // 设置帧指针
        // addi s0, sp, 64
        self.emit32(0x04010413);
    }
    
    /// 生成尾声
    fn emit_epilogue(&mut self) {
        // 恢复 callee-saved 寄存器
        // ld ra, 56(sp)
        self.emit32(0x03813083);
        // ld s0, 48(sp)
        self.emit32(0x03013403);
        // ld s1, 40(sp)
        self.emit32(0x02813483);
        // addi sp, sp, 64
        self.emit32(0x04010113);
        // ret
        self.emit32(0x00008067);
    }
    
    /// 编译单条指令
    fn compile_insn(&mut self, insn: &BpfInsn, _idx: usize) -> Result<(), BpfError> {
        let class = insn.class();
        
        match class {
            bpf_class::BPF_ALU64 => self.compile_alu64(insn),
            bpf_class::BPF_ALU => self.compile_alu32(insn),
            bpf_class::BPF_JMP => self.compile_jmp(insn),
            bpf_class::BPF_LDX => self.compile_ldx(insn),
            bpf_class::BPF_STX => self.compile_stx(insn),
            bpf_class::BPF_ST => self.compile_st(insn),
            _ => Err(BpfError::JitFailed("Unsupported instruction".to_string())),
        }
    }
    
    /// 编译 64 位 ALU 指令
    fn compile_alu64(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let dst = REG_MAP[insn.dst_reg() as usize];
        let src = REG_MAP[insn.src_reg() as usize];
        let op = insn.code & 0xF0;
        let is_imm = (insn.code & 0x08) == 0;
        
        match op {
            bpf_alu::BPF_MOV => {
                if is_imm {
                    self.emit_li(dst, insn.imm as i64);
                } else {
                    self.emit_mv(dst, src);
                }
            }
            bpf_alu::BPF_ADD => {
                if is_imm {
                    self.emit_addi(dst, dst, insn.imm as i64);
                } else {
                    self.emit_add(dst, dst, src);
                }
            }
            bpf_alu::BPF_SUB => {
                if is_imm {
                    self.emit_addi(dst, dst, -(insn.imm as i64));
                } else {
                    self.emit_sub(dst, dst, src);
                }
            }
            bpf_alu::BPF_AND => {
                if is_imm {
                    self.emit_andi(dst, dst, insn.imm as i64);
                } else {
                    self.emit_and(dst, dst, src);
                }
            }
            bpf_alu::BPF_OR => {
                if is_imm {
                    self.emit_ori(dst, dst, insn.imm as i64);
                } else {
                    self.emit_or(dst, dst, src);
                }
            }
            bpf_alu::BPF_XOR => {
                if is_imm {
                    self.emit_xori(dst, dst, insn.imm as i64);
                } else {
                    self.emit_xor(dst, dst, src);
                }
            }
            bpf_alu::BPF_LSH => {
                if is_imm {
                    self.emit_slli(dst, dst, insn.imm as u32);
                } else {
                    self.emit_sll(dst, dst, src);
                }
            }
            bpf_alu::BPF_RSH => {
                if is_imm {
                    self.emit_srli(dst, dst, insn.imm as u32);
                } else {
                    self.emit_srl(dst, dst, src);
                }
            }
            _ => {
                return Err(BpfError::JitFailed(
                    alloc::format!("Unsupported ALU64 op: {:#x}", op)
                ));
            }
        }
        Ok(())
    }
    
    /// 编译 32 位 ALU 指令
    fn compile_alu32(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        // 32 位操作需要符号扩展
        self.compile_alu64(insn)?;
        let dst = REG_MAP[insn.dst_reg() as usize];
        // sext.w dst, dst
        self.emit32(self.encode_itype(0x1B, dst, 0, dst, 0));
        Ok(())
    }
    
    /// 编译跳转指令
    fn compile_jmp(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let op = insn.code & 0xF0;
        
        match op {
            bpf_jmp::BPF_EXIT => {
                // 返回值已在 a0 中
                // 跳转到尾声
                // j epilogue (这里简化处理)
            }
            bpf_jmp::BPF_CALL => {
                // 调用辅助函数
                let func_id = insn.imm;
                self.emit_li(5, func_id as i64); // t0 = func_id
                // 实际调用需要查表
            }
            bpf_jmp::BPF_JA => {
                // 无条件跳转
                let offset = insn.off as i32 * 4; // 简化：假设每条指令 4 字节
                self.emit_jal(0, offset);
            }
            _ => {
                // 条件跳转
                let dst = REG_MAP[insn.dst_reg() as usize];
                let src = REG_MAP[insn.src_reg() as usize];
                let offset = insn.off as i32 * 4;
                
                match op {
                    bpf_jmp::BPF_JEQ => self.emit_beq(dst, src, offset),
                    bpf_jmp::BPF_JNE => self.emit_bne(dst, src, offset),
                    bpf_jmp::BPF_JGT => self.emit_bltu(src, dst, offset),
                    bpf_jmp::BPF_JGE => self.emit_bgeu(dst, src, offset),
                    bpf_jmp::BPF_JLT => self.emit_bltu(dst, src, offset),
                    bpf_jmp::BPF_JLE => self.emit_bgeu(src, dst, offset),
                    _ => return Err(BpfError::JitFailed("Unsupported JMP".to_string())),
                }
            }
        }
        Ok(())
    }
    
    /// 编译加载指令
    fn compile_ldx(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let dst = REG_MAP[insn.dst_reg() as usize];
        let src = REG_MAP[insn.src_reg() as usize];
        let off = insn.off as i32;
        let size = (insn.code >> 3) & 0x03;
        
        match size {
            0 => self.emit_lw(dst, src, off),  // 32-bit
            1 => self.emit_lh(dst, src, off),  // 16-bit
            2 => self.emit_lb(dst, src, off),  // 8-bit
            3 => self.emit_ld(dst, src, off),  // 64-bit
            _ => return Err(BpfError::JitFailed("Invalid load size".to_string())),
        }
        Ok(())
    }
    
    /// 编译存储指令 (寄存器)
    fn compile_stx(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let dst = REG_MAP[insn.dst_reg() as usize];
        let src = REG_MAP[insn.src_reg() as usize];
        let off = insn.off as i32;
        let size = (insn.code >> 3) & 0x03;
        
        match size {
            0 => self.emit_sw(dst, src, off),
            1 => self.emit_sh(dst, src, off),
            2 => self.emit_sb(dst, src, off),
            3 => self.emit_sd(dst, src, off),
            _ => return Err(BpfError::JitFailed("Invalid store size".to_string())),
        }
        Ok(())
    }
    
    /// 编译存储指令 (立即数)
    fn compile_st(&mut self, insn: &BpfInsn) -> Result<(), BpfError> {
        let dst = REG_MAP[insn.dst_reg() as usize];
        let off = insn.off as i32;
        let imm = insn.imm;
        
        // 先将立即数加载到临时寄存器
        self.emit_li(5, imm as i64); // t0
        
        let size = (insn.code >> 3) & 0x03;
        match size {
            0 => self.emit_sw(dst, 5, off),
            1 => self.emit_sh(dst, 5, off),
            2 => self.emit_sb(dst, 5, off),
            3 => self.emit_sd(dst, 5, off),
            _ => return Err(BpfError::JitFailed("Invalid store size".to_string())),
        }
        Ok(())
    }

    // === RISC-V 指令编码辅助函数 ===
    
    fn emit32(&mut self, insn: u32) {
        self.code.extend_from_slice(&insn.to_le_bytes());
    }
    
    fn encode_rtype(&self, opcode: u32, rd: u8, funct3: u32, rs1: u8, rs2: u8, funct7: u32) -> u32 {
        opcode | ((rd as u32) << 7) | (funct3 << 12) | ((rs1 as u32) << 15) 
            | ((rs2 as u32) << 20) | (funct7 << 25)
    }
    
    fn encode_itype(&self, opcode: u32, rd: u8, funct3: u32, rs1: u8, imm: i32) -> u32 {
        opcode | ((rd as u32) << 7) | (funct3 << 12) | ((rs1 as u32) << 15) 
            | (((imm as u32) & 0xFFF) << 20)
    }
    
    fn encode_stype(&self, opcode: u32, funct3: u32, rs1: u8, rs2: u8, imm: i32) -> u32 {
        let imm = imm as u32;
        opcode | ((imm & 0x1F) << 7) | (funct3 << 12) | ((rs1 as u32) << 15) 
            | ((rs2 as u32) << 20) | (((imm >> 5) & 0x7F) << 25)
    }
    
    fn encode_btype(&self, opcode: u32, funct3: u32, rs1: u8, rs2: u8, imm: i32) -> u32 {
        let imm = imm as u32;
        opcode | (((imm >> 11) & 0x1) << 7) | (((imm >> 1) & 0xF) << 8) 
            | (funct3 << 12) | ((rs1 as u32) << 15) | ((rs2 as u32) << 20)
            | (((imm >> 5) & 0x3F) << 25) | (((imm >> 12) & 0x1) << 31)
    }
    
    // === 指令发射函数 ===
    
    fn emit_li(&mut self, rd: u8, imm: i64) {
        if imm >= -2048 && imm < 2048 {
            // addi rd, x0, imm
            self.emit32(self.encode_itype(0x13, rd, 0, 0, imm as i32));
        } else {
            // lui + addi
            let upper = ((imm + 0x800) >> 12) as i32;
            let lower = (imm as i32) & 0xFFF;
            self.emit32(0x37 | ((rd as u32) << 7) | (((upper as u32) & 0xFFFFF) << 12));
            if lower != 0 {
                self.emit32(self.encode_itype(0x13, rd, 0, rd, lower));
            }
        }
    }
    
    fn emit_mv(&mut self, rd: u8, rs: u8) {
        // addi rd, rs, 0
        self.emit32(self.encode_itype(0x13, rd, 0, rs, 0));
    }
    
    fn emit_add(&mut self, rd: u8, rs1: u8, rs2: u8) {
        self.emit32(self.encode_rtype(0x33, rd, 0, rs1, rs2, 0));
    }
    
    fn emit_sub(&mut self, rd: u8, rs1: u8, rs2: u8) {
        self.emit32(self.encode_rtype(0x33, rd, 0, rs1, rs2, 0x20));
    }
    
    fn emit_and(&mut self, rd: u8, rs1: u8, rs2: u8) {
        self.emit32(self.encode_rtype(0x33, rd, 7, rs1, rs2, 0));
    }
    
    fn emit_or(&mut self, rd: u8, rs1: u8, rs2: u8) {
        self.emit32(self.encode_rtype(0x33, rd, 6, rs1, rs2, 0));
    }
    
    fn emit_xor(&mut self, rd: u8, rs1: u8, rs2: u8) {
        self.emit32(self.encode_rtype(0x33, rd, 4, rs1, rs2, 0));
    }
    
    fn emit_sll(&mut self, rd: u8, rs1: u8, rs2: u8) {
        self.emit32(self.encode_rtype(0x33, rd, 1, rs1, rs2, 0));
    }
    
    fn emit_srl(&mut self, rd: u8, rs1: u8, rs2: u8) {
        self.emit32(self.encode_rtype(0x33, rd, 5, rs1, rs2, 0));
    }
    
    fn emit_addi(&mut self, rd: u8, rs: u8, imm: i64) {
        self.emit32(self.encode_itype(0x13, rd, 0, rs, imm as i32));
    }
    
    fn emit_andi(&mut self, rd: u8, rs: u8, imm: i64) {
        self.emit32(self.encode_itype(0x13, rd, 7, rs, imm as i32));
    }
    
    fn emit_ori(&mut self, rd: u8, rs: u8, imm: i64) {
        self.emit32(self.encode_itype(0x13, rd, 6, rs, imm as i32));
    }
    
    fn emit_xori(&mut self, rd: u8, rs: u8, imm: i64) {
        self.emit32(self.encode_itype(0x13, rd, 4, rs, imm as i32));
    }
    
    fn emit_slli(&mut self, rd: u8, rs: u8, shamt: u32) {
        self.emit32(self.encode_itype(0x13, rd, 1, rs, shamt as i32));
    }
    
    fn emit_srli(&mut self, rd: u8, rs: u8, shamt: u32) {
        self.emit32(self.encode_itype(0x13, rd, 5, rs, shamt as i32));
    }
    
    fn emit_ld(&mut self, rd: u8, rs: u8, off: i32) {
        self.emit32(self.encode_itype(0x03, rd, 3, rs, off));
    }
    
    fn emit_lw(&mut self, rd: u8, rs: u8, off: i32) {
        self.emit32(self.encode_itype(0x03, rd, 2, rs, off));
    }
    
    fn emit_lh(&mut self, rd: u8, rs: u8, off: i32) {
        self.emit32(self.encode_itype(0x03, rd, 1, rs, off));
    }
    
    fn emit_lb(&mut self, rd: u8, rs: u8, off: i32) {
        self.emit32(self.encode_itype(0x03, rd, 0, rs, off));
    }
    
    fn emit_sd(&mut self, rs1: u8, rs2: u8, off: i32) {
        self.emit32(self.encode_stype(0x23, 3, rs1, rs2, off));
    }
    
    fn emit_sw(&mut self, rs1: u8, rs2: u8, off: i32) {
        self.emit32(self.encode_stype(0x23, 2, rs1, rs2, off));
    }
    
    fn emit_sh(&mut self, rs1: u8, rs2: u8, off: i32) {
        self.emit32(self.encode_stype(0x23, 1, rs1, rs2, off));
    }
    
    fn emit_sb(&mut self, rs1: u8, rs2: u8, off: i32) {
        self.emit32(self.encode_stype(0x23, 0, rs1, rs2, off));
    }
    
    fn emit_beq(&mut self, rs1: u8, rs2: u8, off: i32) {
        self.emit32(self.encode_btype(0x63, 0, rs1, rs2, off));
    }
    
    fn emit_bne(&mut self, rs1: u8, rs2: u8, off: i32) {
        self.emit32(self.encode_btype(0x63, 1, rs1, rs2, off));
    }
    
    fn emit_bltu(&mut self, rs1: u8, rs2: u8, off: i32) {
        self.emit32(self.encode_btype(0x63, 6, rs1, rs2, off));
    }
    
    fn emit_bgeu(&mut self, rs1: u8, rs2: u8, off: i32) {
        self.emit32(self.encode_btype(0x63, 7, rs1, rs2, off));
    }
    
    fn emit_jal(&mut self, rd: u8, off: i32) {
        let imm = off as u32;
        let insn = 0x6F | ((rd as u32) << 7)
            | (((imm >> 12) & 0xFF) << 12)
            | (((imm >> 11) & 0x1) << 20)
            | (((imm >> 1) & 0x3FF) << 21)
            | (((imm >> 20) & 0x1) << 31);
        self.emit32(insn);
    }
}

/// 编译 eBPF 程序
pub fn compile(prog: &BpfProg) -> Result<Vec<u8>, BpfError> {
    let mut compiler = JitCompiler::new(prog);
    compiler.compile()
}
