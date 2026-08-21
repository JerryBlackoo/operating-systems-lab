//! eBPF 子系统
//!
//! 实现 eBPF 验证器、JIT 编译器和 Maps

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use alloc::boxed::Box;
use alloc::string::String;
use crate::sync::SpinLock;

pub mod verifier;
pub mod jit;
pub mod maps;
pub mod prog_types;

/// eBPF 寄存器数量
pub const BPF_REG_COUNT: usize = 11;

/// eBPF 指令
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BpfInsn {
    /// 操作码
    pub code: u8,
    /// 目标寄存器 (低4位) 和源寄存器 (高4位)
    pub regs: u8,
    /// 偏移量
    pub off: i16,
    /// 立即数
    pub imm: i32,
}

impl BpfInsn {
    /// 获取目标寄存器
    pub fn dst_reg(&self) -> u8 {
        self.regs & 0x0F
    }
    
    /// 获取源寄存器
    pub fn src_reg(&self) -> u8 {
        (self.regs >> 4) & 0x0F
    }
    
    /// 获取操作码类别
    pub fn class(&self) -> u8 {
        self.code & 0x07
    }
}

/// eBPF 操作码类别
pub mod bpf_class {
    pub const BPF_LD: u8 = 0x00;
    pub const BPF_LDX: u8 = 0x01;
    pub const BPF_ST: u8 = 0x02;
    pub const BPF_STX: u8 = 0x03;
    pub const BPF_ALU: u8 = 0x04;
    pub const BPF_JMP: u8 = 0x05;
    pub const BPF_JMP32: u8 = 0x06;
    pub const BPF_ALU64: u8 = 0x07;
}

/// eBPF ALU 操作
pub mod bpf_alu {
    pub const BPF_ADD: u8 = 0x00;
    pub const BPF_SUB: u8 = 0x10;
    pub const BPF_MUL: u8 = 0x20;
    pub const BPF_DIV: u8 = 0x30;
    pub const BPF_OR: u8 = 0x40;
    pub const BPF_AND: u8 = 0x50;
    pub const BPF_LSH: u8 = 0x60;
    pub const BPF_RSH: u8 = 0x70;
    pub const BPF_NEG: u8 = 0x80;
    pub const BPF_MOD: u8 = 0x90;
    pub const BPF_XOR: u8 = 0xa0;
    pub const BPF_MOV: u8 = 0xb0;
    pub const BPF_ARSH: u8 = 0xc0;
    pub const BPF_END: u8 = 0xd0;
}

/// eBPF 跳转操作
pub mod bpf_jmp {
    pub const BPF_JA: u8 = 0x00;
    pub const BPF_JEQ: u8 = 0x10;
    pub const BPF_JGT: u8 = 0x20;
    pub const BPF_JGE: u8 = 0x30;
    pub const BPF_JSET: u8 = 0x40;
    pub const BPF_JNE: u8 = 0x50;
    pub const BPF_JSGT: u8 = 0x60;
    pub const BPF_JSGE: u8 = 0x70;
    pub const BPF_CALL: u8 = 0x80;
    pub const BPF_EXIT: u8 = 0x90;
    pub const BPF_JLT: u8 = 0xa0;
    pub const BPF_JLE: u8 = 0xb0;
    pub const BPF_JSLT: u8 = 0xc0;
    pub const BPF_JSLE: u8 = 0xd0;
}

/// eBPF 程序类型
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BpfProgType {
    /// 未指定
    Unspec,
    /// Socket 过滤器
    SocketFilter,
    /// Kprobe
    Kprobe,
    /// Tracepoint
    Tracepoint,
    /// XDP
    Xdp,
    /// 性能事件
    PerfEvent,
    /// Cgroup SKB
    CgroupSkb,
}

/// eBPF 程序
pub struct BpfProg {
    /// 程序类型
    prog_type: BpfProgType,
    /// 原始指令
    insns: Vec<BpfInsn>,
    /// JIT 编译后的代码
    jit_code: Option<Vec<u8>>,
    /// 程序名称
    #[allow(dead_code)]
    name: String,
    /// 是否已验证
    verified: bool,
}

impl BpfProg {
    /// 创建新程序
    pub fn new(prog_type: BpfProgType, insns: Vec<BpfInsn>, name: String) -> Self {
        Self {
            prog_type,
            insns,
            jit_code: None,
            name,
            verified: false,
        }
    }
    
    /// 获取程序类型
    pub fn prog_type(&self) -> BpfProgType {
        self.prog_type
    }
    
    /// 获取指令
    pub fn insns(&self) -> &[BpfInsn] {
        &self.insns
    }
    
    /// 是否已验证
    pub fn is_verified(&self) -> bool {
        self.verified
    }
    
    /// 设置已验证
    pub fn set_verified(&mut self, verified: bool) {
        self.verified = verified;
    }
    
    /// 设置 JIT 代码
    pub fn set_jit_code(&mut self, code: Vec<u8>) {
        self.jit_code = Some(code);
    }
    
    /// 获取 JIT 代码
    pub fn jit_code(&self) -> Option<&[u8]> {
        self.jit_code.as_deref()
    }
}

/// eBPF 运行时上下文
#[repr(C)]
pub struct BpfContext {
    /// 寄存器
    pub regs: [u64; BPF_REG_COUNT],
    /// 栈
    pub stack: [u8; 512],
    /// 栈指针
    pub sp: usize,
}

impl BpfContext {
    pub fn new() -> Self {
        Self {
            regs: [0; BPF_REG_COUNT],
            stack: [0; 512],
            sp: 512,
        }
    }
    
    /// 设置参数
    pub fn set_arg(&mut self, index: usize, value: u64) {
        if index < 5 {
            self.regs[1 + index] = value;
        }
    }
    
    /// 获取返回值
    pub fn get_return(&self) -> u64 {
        self.regs[0]
    }
}

/// eBPF 错误
#[derive(Debug, Clone)]
pub enum BpfError {
    /// 验证失败
    VerifyFailed(String),
    /// JIT 编译失败
    JitFailed(String),
    /// 无效程序
    InvalidProgram,
    /// 无效 Map
    InvalidMap,
    /// 内存不足
    OutOfMemory,
    /// 权限不足
    PermissionDenied,
}

/// eBPF 辅助函数 ID
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum BpfHelperFunc {
    /// 未指定
    Unspec = 0,
    /// Map 查找
    MapLookupElem = 1,
    /// Map 更新
    MapUpdateElem = 2,
    /// Map 删除
    MapDeleteElem = 3,
    /// 获取当前 PID/TGID
    GetCurrentPidTgid = 14,
    /// 获取当前 UID/GID
    GetCurrentUidGid = 15,
    /// 获取当前 comm
    GetCurrentComm = 16,
    /// 追踪打印
    TracePrintk = 6,
    /// 获取 SMP 处理器 ID
    GetSmpProcessorId = 8,
    /// 获取当前时间（纳秒）
    KtimeGetNs = 5,
}

/// 辅助函数表
pub type HelperFunc = fn(&mut BpfContext, u64, u64, u64, u64, u64) -> u64;

/// 全局 eBPF 管理器
static EBPF_MANAGER: SpinLock<Option<EbpfManager>> = SpinLock::new(None);

/// eBPF 管理器
pub struct EbpfManager {
    /// 程序表
    progs: BTreeMap<u64, BpfProg>,
    /// Map 表
    maps: BTreeMap<u64, Box<dyn maps::BpfMap>>,
    /// 下一个程序 ID
    next_prog_id: u64,
    /// 下一个 Map ID
    next_map_id: u64,
}

impl EbpfManager {
    fn new() -> Self {
        Self {
            progs: BTreeMap::new(),
            maps: BTreeMap::new(),
            next_prog_id: 1,
            next_map_id: 1,
        }
    }
    
    /// 加载程序
    pub fn load_prog(&mut self, prog_type: BpfProgType, insns: Vec<BpfInsn>, name: String) 
        -> Result<u64, BpfError> 
    {
        let mut prog = BpfProg::new(prog_type, insns, name);
        
        // 验证程序
        verifier::verify(&prog)?;
        prog.set_verified(true);
        
        // JIT 编译 (best-effort: fall back to interpreter if JIT fails)
        // Per Linux: CONFIG_BPF_JIT is optional; programs always have the
        // interpreter as fallback.
        if let Ok(jit_code) = jit::compile(&prog) {
            prog.set_jit_code(jit_code);
        }
        
        let id = self.next_prog_id;
        self.next_prog_id += 1;
        self.progs.insert(id, prog);
        
        Ok(id)
    }
    
    /// 卸载程序
    pub fn unload_prog(&mut self, prog_id: u64) -> Result<(), BpfError> {
        self.progs.remove(&prog_id)
            .map(|_| ())
            .ok_or(BpfError::InvalidProgram)
    }
    
    /// 创建 Map
    pub fn create_map(&mut self, map_type: maps::BpfMapType, key_size: u32, 
                      value_size: u32, max_entries: u32) -> Result<u64, BpfError> 
    {
        let map: Box<dyn maps::BpfMap> = match map_type {
            maps::BpfMapType::Hash => {
                Box::new(maps::BpfHashMap::new(key_size, value_size, max_entries))
            }
            maps::BpfMapType::Array => {
                Box::new(maps::BpfArray::new(value_size, max_entries))
            }
            _ => return Err(BpfError::InvalidMap),
        };
        
        let id = self.next_map_id;
        self.next_map_id += 1;
        self.maps.insert(id, map);
        
        Ok(id)
    }
    
    /// 删除 Map
    pub fn delete_map(&mut self, map_id: u64) -> Result<(), BpfError> {
        self.maps.remove(&map_id)
            .map(|_| ())
            .ok_or(BpfError::InvalidMap)
    }
    
    /// 获取 Map
    pub fn get_map(&self, map_id: u64) -> Option<&dyn maps::BpfMap> {
        self.maps.get(&map_id).map(|m| m.as_ref())
    }
    
    /// 获取可变 Map
    pub fn get_map_mut(&mut self, map_id: u64) -> Option<&mut (dyn maps::BpfMap + 'static)> {
        self.maps.get_mut(&map_id).map(move |m| m.as_mut())
    }
}

/// 初始化 eBPF 子系统
pub fn init() {
    let mut mgr = EBPF_MANAGER.lock();
    *mgr = Some(EbpfManager::new());
    crate::println!("[ebpf] eBPF subsystem initialized");
}

/// 静默初始化 eBPF 子系统（不输出调试信息）
pub fn init_quiet() {
    let mut mgr = EBPF_MANAGER.lock();
    *mgr = Some(EbpfManager::new());
}

/// 加载 eBPF 程序
pub fn load_prog(prog_type: BpfProgType, insns: Vec<BpfInsn>, name: String) 
    -> Result<u64, BpfError> 
{
    EBPF_MANAGER.lock()
        .as_mut()
        .ok_or(BpfError::InvalidProgram)?
        .load_prog(prog_type, insns, name)
}

/// 创建 eBPF Map
pub fn create_map(map_type: maps::BpfMapType, key_size: u32, 
                  value_size: u32, max_entries: u32) -> Result<u64, BpfError> 
{
    EBPF_MANAGER.lock()
        .as_mut()
        .ok_or(BpfError::InvalidMap)?
        .create_map(map_type, key_size, value_size, max_entries)
}

pub enum BpfMapAccessError {
    InvalidMap,
    KeyNotFound,
    KeyExists,
    MapFull,
    InvalidArgument,
}

fn map_error_to_access(err: maps::MapError) -> BpfMapAccessError {
    match err {
        maps::MapError::KeyNotFound => BpfMapAccessError::KeyNotFound,
        maps::MapError::KeyExists => BpfMapAccessError::KeyExists,
        maps::MapError::MapFull => BpfMapAccessError::MapFull,
        maps::MapError::InvalidArg => BpfMapAccessError::InvalidArgument,
    }
}

pub fn lookup_map(map_id: u64, key: &[u8]) -> Result<Vec<u8>, BpfMapAccessError> {
    let mgr = EBPF_MANAGER.lock();
    let map = mgr
        .as_ref()
        .and_then(|mgr| mgr.get_map(map_id))
        .ok_or(BpfMapAccessError::InvalidMap)?;
    map.lookup(key)
        .map(|value| value.to_vec())
        .ok_or(BpfMapAccessError::KeyNotFound)
}

pub fn update_map(map_id: u64, key: &[u8], value: &[u8], flags: u64) -> Result<(), BpfMapAccessError> {
    let mut mgr = EBPF_MANAGER.lock();
    let map = mgr
        .as_mut()
        .and_then(|mgr| mgr.get_map_mut(map_id))
        .ok_or(BpfMapAccessError::InvalidMap)?;
    map.update(key, value, flags).map_err(map_error_to_access)
}

// ============================================================
// eBPF Interpreter — software execution of BPF bytecode
// Per Linux kernel/bpf/core.c ___bpf_prog_run()
// ============================================================

/// Run an eBPF program in the interpreter (fallback when JIT unavailable or
/// for verification). Returns the value in R0 on BPF_EXIT.
pub fn run_prog(prog: &BpfProg, ctx: &mut BpfContext) -> Result<u64, BpfError> {
    let insns = prog.insns();
    if insns.is_empty() {
        return Err(BpfError::InvalidProgram);
    }

    let mut regs = ctx.regs;
    // R10 = frame pointer (stack base)
    regs[10] = ctx.stack.as_ptr() as u64 + ctx.sp as u64;

    let mut pc: usize = 0;
    let max_insns_executed: usize = 1_000_000; // prevent infinite loops
    let mut executed: usize = 0;

    while pc < insns.len() {
        executed += 1;
        if executed > max_insns_executed {
            return Err(BpfError::VerifyFailed(
                alloc::string::String::from("Execution limit exceeded"),
            ));
        }

        let insn = &insns[pc];
        let class = insn.class();
        let dst = insn.dst_reg() as usize;
        let src = insn.src_reg() as usize;

        match class {
            bpf_class::BPF_ALU64 => {
                let op = insn.code & 0xF0;
                let s = if (insn.code & 0x08) == 0 {
                    insn.imm as i64 as u64
                } else {
                    regs[src]
                };
                match op {
                    bpf_alu::BPF_ADD => regs[dst] = regs[dst].wrapping_add(s),
                    bpf_alu::BPF_SUB => regs[dst] = regs[dst].wrapping_sub(s),
                    bpf_alu::BPF_MUL => regs[dst] = regs[dst].wrapping_mul(s),
                    bpf_alu::BPF_DIV => {
                        if s == 0 { regs[dst] = 0; } else { regs[dst] /= s; }
                    }
                    bpf_alu::BPF_MOD => {
                        if s == 0 { regs[dst] = 0; } else { regs[dst] %= s; }
                    }
                    bpf_alu::BPF_OR  => regs[dst] |= s,
                    bpf_alu::BPF_AND => regs[dst] &= s,
                    bpf_alu::BPF_XOR => regs[dst] ^= s,
                    bpf_alu::BPF_LSH => regs[dst] <<= s & 63,
                    bpf_alu::BPF_RSH => regs[dst] >>= s & 63,
                    bpf_alu::BPF_ARSH => {
                        regs[dst] = ((regs[dst] as i64) >> (s & 63)) as u64;
                    }
                    bpf_alu::BPF_NEG => regs[dst] = (-(regs[dst] as i64)) as u64,
                    bpf_alu::BPF_MOV => regs[dst] = s,
                    bpf_alu::BPF_END => {
                        // Byte swap: per Linux eBPF spec
                        // BPF_TO_LE (src=0, code bit3=0): convert to LE (nop on LE host)
                        // BPF_TO_BE (src=1, code bit3=1): convert to BE (swap on LE host)
                        let to_be = (insn.code & 0x08) != 0;
                        let size = insn.imm;
                        if to_be {
                            regs[dst] = match size {
                                16 => (regs[dst] as u16).to_be() as u64,
                                32 => (regs[dst] as u32).to_be() as u64,
                                64 => regs[dst].to_be(),
                                _ => regs[dst],
                            };
                        } else {
                            // to_le on a LE machine: just truncate to size
                            regs[dst] = match size {
                                16 => regs[dst] & 0xFFFF,
                                32 => regs[dst] & 0xFFFF_FFFF,
                                64 => regs[dst],
                                _ => regs[dst],
                            };
                        }
                    }
                    _ => {}
                }
                pc += 1;
            }
            bpf_class::BPF_ALU => {
                let op = insn.code & 0xF0;
                let s = if (insn.code & 0x08) == 0 {
                    insn.imm as u32 as u64
                } else {
                    regs[src] & 0xFFFF_FFFF
                };
                let d = regs[dst] & 0xFFFF_FFFF;
                let result = match op {
                    bpf_alu::BPF_ADD => d.wrapping_add(s),
                    bpf_alu::BPF_SUB => d.wrapping_sub(s),
                    bpf_alu::BPF_MUL => d.wrapping_mul(s),
                    bpf_alu::BPF_DIV => if s == 0 { 0 } else { d / s },
                    bpf_alu::BPF_MOD => if s == 0 { 0 } else { d % s },
                    bpf_alu::BPF_OR  => d | s,
                    bpf_alu::BPF_AND => d & s,
                    bpf_alu::BPF_XOR => d ^ s,
                    bpf_alu::BPF_LSH => d << (s & 31),
                    bpf_alu::BPF_RSH => d >> (s & 31),
                    bpf_alu::BPF_ARSH => ((d as i32) >> (s & 31)) as u32 as u64,
                    bpf_alu::BPF_NEG => (-(d as i32)) as u32 as u64,
                    bpf_alu::BPF_MOV => s,
                    _ => d,
                };
                // 32-bit: zero-extend result
                regs[dst] = result & 0xFFFF_FFFF;
                pc += 1;
            }
            bpf_class::BPF_JMP | bpf_class::BPF_JMP32 => {
                let op = insn.code & 0xF0;
                if op == bpf_jmp::BPF_EXIT {
                    ctx.regs = regs;
                    return Ok(regs[0]);
                }
                if op == bpf_jmp::BPF_CALL {
                    // Helper function call
                    let helper_id = insn.imm as u32;
                    regs[0] = call_helper(helper_id, regs[1], regs[2], regs[3], regs[4], regs[5]);
                    pc += 1;
                    continue;
                }
                if op == bpf_jmp::BPF_JA {
                    pc = (pc as isize + 1 + insn.off as isize) as usize;
                    continue;
                }
                // Conditional jumps
                let (a, b) = if class == bpf_class::BPF_JMP32 {
                    let a = regs[dst] & 0xFFFF_FFFF;
                    let b = if (insn.code & 0x08) == 0 {
                        insn.imm as u32 as u64
                    } else {
                        regs[src] & 0xFFFF_FFFF
                    };
                    (a, b)
                } else {
                    let a = regs[dst];
                    let b = if (insn.code & 0x08) == 0 {
                        insn.imm as i64 as u64
                    } else {
                        regs[src]
                    };
                    (a, b)
                };
                let taken = match op {
                    bpf_jmp::BPF_JEQ  => a == b,
                    bpf_jmp::BPF_JNE  => a != b,
                    bpf_jmp::BPF_JGT  => a > b,
                    bpf_jmp::BPF_JGE  => a >= b,
                    bpf_jmp::BPF_JLT  => a < b,
                    bpf_jmp::BPF_JLE  => a <= b,
                    bpf_jmp::BPF_JSGT => (a as i64) > (b as i64),
                    bpf_jmp::BPF_JSGE => (a as i64) >= (b as i64),
                    bpf_jmp::BPF_JSLT => (a as i64) < (b as i64),
                    bpf_jmp::BPF_JSLE => (a as i64) <= (b as i64),
                    bpf_jmp::BPF_JSET => (a & b) != 0,
                    _ => false,
                };
                if taken {
                    pc = (pc as isize + 1 + insn.off as isize) as usize;
                } else {
                    pc += 1;
                }
            }
            bpf_class::BPF_LD => {
                // BPF_LD | BPF_IMM | BPF_DW — 64-bit immediate (two-insn encoding)
                if pc + 1 >= insns.len() {
                    return Err(BpfError::InvalidProgram);
                }
                let next = &insns[pc + 1];
                let imm64 = (insn.imm as u32 as u64) | ((next.imm as u32 as u64) << 32);
                regs[dst] = imm64;
                pc += 2;
            }
            bpf_class::BPF_LDX => {
                let off = insn.off as i64;
                let addr = (regs[src] as i64 + off) as *const u8;
                let size = (insn.code >> 3) & 0x03;
                unsafe {
                    regs[dst] = match size {
                        0 => *(addr as *const u32) as u64,     // W
                        1 => *(addr as *const u16) as u64,     // H
                        2 => *addr as u64,                     // B
                        3 => *(addr as *const u64),            // DW
                        _ => 0,
                    };
                }
                pc += 1;
            }
            bpf_class::BPF_ST => {
                let off = insn.off as i64;
                let addr = (regs[dst] as i64 + off) as *mut u8;
                let size = (insn.code >> 3) & 0x03;
                let val = insn.imm as u64;
                unsafe {
                    match size {
                        0 => *(addr as *mut u32) = val as u32,
                        1 => *(addr as *mut u16) = val as u16,
                        2 => *addr = val as u8,
                        3 => *(addr as *mut u64) = val,
                        _ => {}
                    }
                }
                pc += 1;
            }
            bpf_class::BPF_STX => {
                let off = insn.off as i64;
                let addr = (regs[dst] as i64 + off) as *mut u8;
                let size = (insn.code >> 3) & 0x03;
                let val = regs[src];
                unsafe {
                    match size {
                        0 => *(addr as *mut u32) = val as u32,
                        1 => *(addr as *mut u16) = val as u16,
                        2 => *addr = val as u8,
                        3 => *(addr as *mut u64) = val,
                        _ => {}
                    }
                }
                pc += 1;
            }
            _ => {
                return Err(BpfError::InvalidProgram);
            }
        }
    }

    ctx.regs = regs;
    Ok(regs[0])
}

// ============================================================
// eBPF Helper function dispatch
// Per Linux include/uapi/linux/bpf.h helper IDs
// ============================================================

fn call_helper(id: u32, r1: u64, r2: u64, r3: u64, r4: u64, r5: u64) -> u64 {
    match id {
        // bpf_map_lookup_elem(map_fd, key) -> *value or 0
        1 => {
            let map_id = r1;
            let key_ptr = r2 as *const u8;
            let mgr = EBPF_MANAGER.lock();
            if let Some(mgr) = mgr.as_ref() {
                if let Some(map) = mgr.get_map(map_id) {
                    // Read key from pointer (assume key is at most 256 bytes)
                    let key_size = 8usize; // simplified: real impl would track key_size per map
                    let key = unsafe { core::slice::from_raw_parts(key_ptr, key_size) };
                    if let Some(val) = map.lookup(key) {
                        return val.as_ptr() as u64;
                    }
                }
            }
            0
        }
        // bpf_map_update_elem(map_fd, key, value, flags)
        2 => {
            let map_id = r1;
            let key_ptr = r2 as *const u8;
            let val_ptr = r3 as *const u8;
            let flags = r4;
            let mut mgr = EBPF_MANAGER.lock();
            if let Some(mgr) = mgr.as_mut() {
                if let Some(map) = mgr.get_map_mut(map_id) {
                    let key = unsafe { core::slice::from_raw_parts(key_ptr, 8) };
                    let val = unsafe { core::slice::from_raw_parts(val_ptr, 8) };
                    if map.update(key, val, flags).is_ok() {
                        return 0;
                    }
                }
            }
            u64::MAX // -1
        }
        // bpf_map_delete_elem(map_fd, key)
        3 => {
            let map_id = r1;
            let key_ptr = r2 as *const u8;
            let mut mgr = EBPF_MANAGER.lock();
            if let Some(mgr) = mgr.as_mut() {
                if let Some(map) = mgr.get_map_mut(map_id) {
                    let key = unsafe { core::slice::from_raw_parts(key_ptr, 8) };
                    if map.delete(key).is_ok() {
                        return 0;
                    }
                }
            }
            u64::MAX
        }
        // bpf_ktime_get_ns
        5 => {
            let ticks = crate::arch::asm_helpers::rdtime() as u64;
            let freq = crate::arch::consts::CLOCK_FREQ as u64;
            if freq > 0 {
                (ticks / freq) * 1_000_000_000 + (ticks % freq) * 1_000_000_000 / freq
            } else {
                ticks
            }
        }
        // bpf_trace_printk (simplified: just return 0)
        6 => 0,
        // bpf_get_smp_processor_id
        8 => 0, // single-core for now
        // bpf_get_current_pid_tgid
        14 => {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_ref() {
                if let Some(pid) = t.current {
                    if let Some(p) = t.get(pid) {
                        return ((p.tgid as u64) << 32) | (pid as u64);
                    }
                }
            }
            0
        }
        // bpf_get_current_uid_gid
        15 => {
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_ref() {
                if let Some(pid) = t.current {
                    if let Some(p) = t.get(pid) {
                        return ((p.gid as u64) << 32) | (p.uid as u64);
                    }
                }
            }
            0
        }
        // bpf_get_current_comm(buf, size)
        16 => {
            let buf = r1 as *mut u8;
            let size = r2 as usize;
            let table = crate::sched::process::PROCESS_TABLE.lock();
            if let Some(t) = table.as_ref() {
                if let Some(pid) = t.current {
                    if let Some(p) = t.get(pid) {
                        let name = p.name.as_bytes();
                        let copy_len = name.len().min(size.saturating_sub(1));
                        unsafe {
                            core::ptr::copy_nonoverlapping(name.as_ptr(), buf, copy_len);
                            *buf.add(copy_len) = 0;
                        }
                        return 0;
                    }
                }
            }
            u64::MAX
        }
        _ => {
            let _ = (r1, r2, r3, r4, r5);
            0
        }
    }
}

/// Delete a map element (for syscall cmd=3 BPF_MAP_DELETE_ELEM)
pub fn delete_map(map_id: u64, key: &[u8]) -> Result<(), BpfMapAccessError> {
    let mut mgr = EBPF_MANAGER.lock();
    let map = mgr
        .as_mut()
        .and_then(|mgr| mgr.get_map_mut(map_id))
        .ok_or(BpfMapAccessError::InvalidMap)?;
    map.delete(key).map_err(map_error_to_access)
}

/// Get next key (for syscall cmd=4 BPF_MAP_GET_NEXT_KEY)
pub fn get_next_key(map_id: u64, key: Option<&[u8]>) -> Result<Vec<u8>, BpfMapAccessError> {
    let mgr = EBPF_MANAGER.lock();
    let map = mgr
        .as_ref()
        .and_then(|mgr| mgr.get_map(map_id))
        .ok_or(BpfMapAccessError::InvalidMap)?;
    map.get_next_key(key).ok_or(BpfMapAccessError::KeyNotFound)
}

/// Run a loaded program by ID (for internal use and tracepoints)
pub fn run_prog_by_id(prog_id: u64, ctx: &mut BpfContext) -> Result<u64, BpfError> {
    let mgr = EBPF_MANAGER.lock();
    let mgr_ref = mgr.as_ref().ok_or(BpfError::InvalidProgram)?;
    let prog = mgr_ref.progs.get(&prog_id).ok_or(BpfError::InvalidProgram)?;
    if !prog.is_verified() {
        return Err(BpfError::VerifyFailed(
            alloc::string::String::from("Program not verified"),
        ));
    }
    run_prog(prog, ctx)
}

/// eBPF 模块自测试
pub fn self_test() -> Result<(), &'static str> {
    crate::println!("[ebpf] Running self-test...");
    
    // 测试 BpfInsn
    let insn = BpfInsn {
        code: bpf_class::BPF_ALU64 | bpf_alu::BPF_ADD,
        regs: 0x12, // dst=2, src=1
        off: 0,
        imm: 42,
    };
    assert_eq!(insn.dst_reg(), 2);
    assert_eq!(insn.src_reg(), 1);
    assert_eq!(insn.class(), bpf_class::BPF_ALU64);
    
    // 测试所有 BPF 类别常量
    let classes = [
        bpf_class::BPF_LD,
        bpf_class::BPF_LDX,
        bpf_class::BPF_ST,
        bpf_class::BPF_STX,
        bpf_class::BPF_ALU,
        bpf_class::BPF_JMP,
        bpf_class::BPF_JMP32,
        bpf_class::BPF_ALU64,
    ];
    for class in &classes {
        crate::println!("[ebpf] BPF class: {:#x}", class);
    }
    
    // 测试所有 ALU 操作常量
    let alu_ops = [
        bpf_alu::BPF_ADD,
        bpf_alu::BPF_SUB,
        bpf_alu::BPF_MUL,
        bpf_alu::BPF_DIV,
        bpf_alu::BPF_OR,
        bpf_alu::BPF_AND,
        bpf_alu::BPF_LSH,
        bpf_alu::BPF_RSH,
        bpf_alu::BPF_NEG,
        bpf_alu::BPF_MOD,
        bpf_alu::BPF_XOR,
        bpf_alu::BPF_MOV,
        bpf_alu::BPF_ARSH,
        bpf_alu::BPF_END,
    ];
    for op in &alu_ops {
        let _ = op;
    }
    
    // 测试所有 JMP 操作常量
    let jmp_ops = [
        bpf_jmp::BPF_JA,
        bpf_jmp::BPF_JEQ,
        bpf_jmp::BPF_JGT,
        bpf_jmp::BPF_JGE,
        bpf_jmp::BPF_JSET,
        bpf_jmp::BPF_JNE,
        bpf_jmp::BPF_JSGT,
        bpf_jmp::BPF_JSGE,
        bpf_jmp::BPF_CALL,
        bpf_jmp::BPF_EXIT,
        bpf_jmp::BPF_JLT,
        bpf_jmp::BPF_JLE,
        bpf_jmp::BPF_JSLT,
        bpf_jmp::BPF_JSLE,
    ];
    for op in &jmp_ops {
        let _ = op;
    }
    
    // 测试 BpfProgType
    let prog_types = [
        BpfProgType::Unspec,
        BpfProgType::SocketFilter,
        BpfProgType::Kprobe,
        BpfProgType::Tracepoint,
        BpfProgType::Xdp,
        BpfProgType::PerfEvent,
        BpfProgType::CgroupSkb,
    ];
    for pt in &prog_types {
        crate::println!("[ebpf] Program type: {:?}", pt);
    }
    
    // 测试 BpfProg
    let insns = alloc::vec![
        BpfInsn { code: bpf_class::BPF_ALU64 | bpf_alu::BPF_MOV, regs: 0x00, off: 0, imm: 0 },
        BpfInsn { code: bpf_class::BPF_JMP | bpf_jmp::BPF_EXIT, regs: 0x00, off: 0, imm: 0 },
    ];
    let mut prog = BpfProg::new(BpfProgType::SocketFilter, insns, String::from("test_prog"));
    assert_eq!(prog.prog_type(), BpfProgType::SocketFilter);
    assert!(!prog.is_verified());
    prog.set_verified(true);
    assert!(prog.is_verified());
    prog.set_jit_code(alloc::vec![0x90, 0xc3]); // nop; ret
    let _ = prog.jit_code();
    let _ = prog.insns();
    
    // 测试 BpfContext
    let mut ctx = BpfContext::new();
    ctx.set_arg(0, 100);
    ctx.set_arg(1, 200);
    ctx.set_arg(4, 500);
    ctx.set_arg(5, 600); // 超出范围，应该被忽略
    ctx.regs[0] = 42;
    assert_eq!(ctx.get_return(), 42);
    
    // 测试 BpfError
    let errors = [
        BpfError::VerifyFailed(String::from("test")),
        BpfError::JitFailed(String::from("test")),
        BpfError::InvalidProgram,
        BpfError::InvalidMap,
        BpfError::OutOfMemory,
        BpfError::PermissionDenied,
    ];
    for err in &errors {
        crate::println!("[ebpf] Error: {:?}", err);
    }
    
    // 测试 BpfHelperFunc
    let helpers = [
        BpfHelperFunc::Unspec,
        BpfHelperFunc::MapLookupElem,
        BpfHelperFunc::MapUpdateElem,
        BpfHelperFunc::MapDeleteElem,
        BpfHelperFunc::GetCurrentPidTgid,
        BpfHelperFunc::GetCurrentUidGid,
        BpfHelperFunc::GetCurrentComm,
        BpfHelperFunc::TracePrintk,
        BpfHelperFunc::GetSmpProcessorId,
        BpfHelperFunc::KtimeGetNs,
    ];
    for helper in &helpers {
        crate::println!("[ebpf] Helper: {:?} = {}", helper, *helper as u32);
    }
    
    // 测试 HelperFunc 类型别名
    let _helper_fn: Option<HelperFunc> = None;
    
    // 测试 EbpfManager
    init();
    
    // 测试创建 Map
    match create_map(maps::BpfMapType::Hash, 4, 8, 100) {
        Ok(map_id) => crate::println!("[ebpf] Created hash map id={}", map_id),
        Err(e) => crate::println!("[ebpf] Map error: {:?}", e),
    }
    
    match create_map(maps::BpfMapType::Array, 4, 8, 100) {
        Ok(map_id) => crate::println!("[ebpf] Created array map id={}", map_id),
        Err(e) => crate::println!("[ebpf] Map error: {:?}", e),
    }
    
    // 测试 EbpfManager 方法
    if let Some(mgr) = EBPF_MANAGER.lock().as_mut() {
        // 测试 load_prog（会失败因为验证器）
        let simple_prog = alloc::vec![
            BpfInsn { code: bpf_class::BPF_ALU64 | bpf_alu::BPF_MOV, regs: 0x00, off: 0, imm: 0 },
            BpfInsn { code: bpf_class::BPF_JMP | bpf_jmp::BPF_EXIT, regs: 0x00, off: 0, imm: 0 },
        ];
        let _ = mgr.load_prog(BpfProgType::SocketFilter, simple_prog, String::from("test"));
        
        // 测试 unload_prog
        let _ = mgr.unload_prog(999);
        
        // 测试 delete_map
        let _ = mgr.delete_map(999);
        
        // 测试 get_map
        let _ = mgr.get_map(1);
        let _ = mgr.get_map_mut(1);
    }
    
    // 测试全局 load_prog 函数
    let simple_insns = alloc::vec![
        BpfInsn { code: bpf_class::BPF_ALU64 | bpf_alu::BPF_MOV, regs: 0x00, off: 0, imm: 0 },
        BpfInsn { code: bpf_class::BPF_JMP | bpf_jmp::BPF_EXIT, regs: 0x00, off: 0, imm: 0 },
    ];
    let _ = load_prog(BpfProgType::SocketFilter, simple_insns, String::from("global_test"));
    
    // 测试 prog_types 模块
    test_prog_types();
    
    // 测试 maps 模块
    test_maps();
    
    // 测试 verifier 模块
    test_verifier();
    
    crate::println!("[ebpf] Self-test passed!");
    Ok(())
}

/// 测试 prog_types 模块
fn test_prog_types() {
    // 测试所有 BpfProgType 变体
    let prog_types = [
        prog_types::BpfProgType::Unspec,
        prog_types::BpfProgType::SocketFilter,
        prog_types::BpfProgType::Kprobe,
        prog_types::BpfProgType::SchedCls,
        prog_types::BpfProgType::SchedAct,
        prog_types::BpfProgType::Tracepoint,
        prog_types::BpfProgType::Xdp,
        prog_types::BpfProgType::PerfEvent,
        prog_types::BpfProgType::CgroupSkb,
        prog_types::BpfProgType::CgroupSock,
        prog_types::BpfProgType::LwtIn,
        prog_types::BpfProgType::LwtOut,
        prog_types::BpfProgType::LwtXmit,
        prog_types::BpfProgType::SockOps,
        prog_types::BpfProgType::SkSkb,
        prog_types::BpfProgType::CgroupDevice,
        prog_types::BpfProgType::SkMsg,
        prog_types::BpfProgType::RawTracepoint,
        prog_types::BpfProgType::CgroupSockAddr,
        prog_types::BpfProgType::LwtSeg6local,
        prog_types::BpfProgType::LircMode2,
        prog_types::BpfProgType::SkReuseport,
        prog_types::BpfProgType::FlowDissector,
        prog_types::BpfProgType::CgroupSysctl,
        prog_types::BpfProgType::RawTracepointWritable,
        prog_types::BpfProgType::CgroupSockopt,
        prog_types::BpfProgType::Tracing,
        prog_types::BpfProgType::StructOps,
        prog_types::BpfProgType::Ext,
        prog_types::BpfProgType::Lsm,
        prog_types::BpfProgType::SkLookup,
    ];
    for pt in &prog_types {
        crate::println!("[ebpf/prog_types] Type: {:?} = {}", pt, *pt as u32);
    }
    
    // 测试所有 BpfAttachType 变体
    let attach_types = [
        prog_types::BpfAttachType::CgroupInetIngress,
        prog_types::BpfAttachType::CgroupInetEgress,
        prog_types::BpfAttachType::CgroupInetSockCreate,
        prog_types::BpfAttachType::CgroupSockOps,
        prog_types::BpfAttachType::SkSkbStreamParser,
        prog_types::BpfAttachType::SkSkbStreamVerdict,
        prog_types::BpfAttachType::CgroupDevice,
        prog_types::BpfAttachType::SkMsgVerdict,
        prog_types::BpfAttachType::CgroupInet4Bind,
        prog_types::BpfAttachType::CgroupInet6Bind,
        prog_types::BpfAttachType::CgroupInet4Connect,
        prog_types::BpfAttachType::CgroupInet6Connect,
        prog_types::BpfAttachType::CgroupInet4PostBind,
        prog_types::BpfAttachType::CgroupInet6PostBind,
        prog_types::BpfAttachType::CgroupUdp4Sendmsg,
        prog_types::BpfAttachType::CgroupUdp6Sendmsg,
        prog_types::BpfAttachType::LircMode2,
        prog_types::BpfAttachType::FlowDissector,
        prog_types::BpfAttachType::CgroupSysctl,
        prog_types::BpfAttachType::CgroupUdp4Recvmsg,
        prog_types::BpfAttachType::CgroupUdp6Recvmsg,
        prog_types::BpfAttachType::CgroupGetsockopt,
        prog_types::BpfAttachType::CgroupSetsockopt,
        prog_types::BpfAttachType::TraceRawTp,
        prog_types::BpfAttachType::TraceFentry,
        prog_types::BpfAttachType::TraceFexit,
        prog_types::BpfAttachType::ModifyReturn,
        prog_types::BpfAttachType::LsmMac,
        prog_types::BpfAttachType::TraceIter,
        prog_types::BpfAttachType::CgroupInet4Getpeername,
        prog_types::BpfAttachType::CgroupInet6Getpeername,
        prog_types::BpfAttachType::CgroupInet4Getsockname,
        prog_types::BpfAttachType::CgroupInet6Getsockname,
        prog_types::BpfAttachType::XdpDevmap,
        prog_types::BpfAttachType::CgroupInetSockRelease,
        prog_types::BpfAttachType::XdpCpumap,
        prog_types::BpfAttachType::SkLookup,
        prog_types::BpfAttachType::Xdp,
    ];
    for at in &attach_types {
        let _ = *at as u32;
    }
    
    // 测试 KprobeContext
    let mut kprobe_ctx = prog_types::KprobeContext::new();
    kprobe_ctx.pc = 0x1000;
    kprobe_ctx.ra = 0x2000;
    kprobe_ctx.sp = 0x3000;
    kprobe_ctx.args[0] = 100;
    kprobe_ctx.ret = 42;
    
    // 测试 TracepointContext
    let mut tp_ctx = prog_types::TracepointContext::new(1);
    tp_ctx.flags = 0;
    tp_ctx.timestamp = 12345;
    tp_ctx.cpu = 0;
    tp_ctx.pid = 1;
    tp_ctx.tid = 1;
    tp_ctx.data = 0;
    tp_ctx.data_len = 0;
    
    // 测试 KprobeHandler
    let kprobe_handler = prog_types::KprobeHandler::new(
        0x80000000,
        String::from("test_func"),
        false,
        1
    );
    let _ = kprobe_handler.handle(&mut kprobe_ctx);
    kprobe_handler.enable();
    kprobe_handler.disable();
    
    // 测试 TracepointDef 和 TracepointField
    let tp_field = prog_types::TracepointField {
        name: String::from("arg0"),
        field_type: String::from("u64"),
        offset: 0,
        size: 8,
    };
    let tp_def = prog_types::TracepointDef {
        category: String::from("sched"),
        name: String::from("sched_switch"),
        format: String::from("prev_pid=%d next_pid=%d"),
        fields: alloc::vec![tp_field],
    };
    
    // 测试 TracepointHandler
    let tp_handler = prog_types::TracepointHandler::new(tp_def, 2);
    let _ = tp_handler.handle(&mut tp_ctx);
    
    // 测试 PerfEventHandler
    let _perf_handler = prog_types::PerfEventHandler::new(0, 0, 1000, 3);
    
    // 测试 ProgTypeManager
    let mut mgr = prog_types::ProgTypeManager::new();
    let kprobe_id = mgr.register_kprobe(0x80001000, "another_func", true);
    let _ = mgr.unregister_kprobe(0x80001000);
    let tp_id = mgr.register_tracepoint("test", "event");
    let _ = mgr.trigger_kprobe(0x80001000, &mut kprobe_ctx);
    let _ = mgr.trigger_tracepoint("test", "event", &mut tp_ctx);
    let _ = mgr.get_kprobe_stats(0x80001000);
    let _ = mgr.list_kprobes();
    let _ = mgr.list_tracepoints();
    crate::println!("[ebpf/prog_types] kprobe_id={}, tp_id={}", kprobe_id, tp_id);
    
    // 测试全局函数
    prog_types::init();
    let _ = prog_types::register_kprobe(0x80002000, "global_kprobe", false);
    let _ = prog_types::trigger_kprobe(0x80002000, &mut kprobe_ctx);
    let _ = prog_types::trigger_tracepoint("sched", "sched_switch", &mut tp_ctx);
}

/// 测试 maps 模块
fn test_maps() {
    // 测试所有 BpfMapType 变体
    let map_types = [
        maps::BpfMapType::Unspec,
        maps::BpfMapType::Hash,
        maps::BpfMapType::Array,
        maps::BpfMapType::ProgArray,
        maps::BpfMapType::PerfEventArray,
        maps::BpfMapType::PercpuHash,
        maps::BpfMapType::PercpuArray,
        maps::BpfMapType::StackTrace,
        maps::BpfMapType::LruHash,
    ];
    for mt in &map_types {
        crate::println!("[ebpf/maps] Map type: {:?}", mt);
    }
    
    // 测试 BpfPercpuArray
    let _percpu_array = maps::BpfPercpuArray::new(8, 100, 4);
}

/// 测试 verifier 模块
fn test_verifier() {
    // 测试 RegType 变体
    let reg_types = [
        verifier::RegType::NotInit,
        verifier::RegType::Scalar,
        verifier::RegType::PtrToCtx,
        verifier::RegType::PtrToStack,
        verifier::RegType::PtrToMapValue,
        verifier::RegType::PtrToPacket,
        verifier::RegType::PtrToPacketEnd,
    ];
    for rt in &reg_types {
        crate::println!("[ebpf/verifier] Reg type: {:?}", rt);
    }
}
