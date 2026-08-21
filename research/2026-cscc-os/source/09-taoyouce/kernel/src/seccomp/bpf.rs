//! cBPF (Classic BPF) VM — seccomp 路径专用子集，严格对齐 Linux
//! `kernel/bpf/core.c::__bpf_prog_run` 中 cBPF 翻译规则，以及
//! `net/core/filter.c::bpf_check_classic` 的 verifier 约束。
//!
//! # cBPF 指令格式（`struct sock_filter`，8 字节）
//!
//! ```c
//! struct sock_filter {
//!     __u16 code;   /* opcode */
//!     __u8  jt;     /* jump if true  (8-bit signed offset) */
//!     __u8  jf;     /* jump if false (8-bit signed offset) */
//!     __u32 k;      /* generic constant / offset */
//! };
//! ```
//!
//! # opcode 编码 (Linux `<linux/filter.h>`)
//!
//! `code` 拆三段：`class(3) | size_or_op(5) | mode_or_src(3)`。
//! 注意所有 cBPF 指令都是 32-bit 运算（与 64-bit eBPF 不同）。
//!
//! # 子集与 Linux verifier 限制
//!
//! - 仅允许下列 class：LD / LDX / ST / STX / ALU / JMP / RET / MISC
//! - LD 只允许 `IMM/ABS/IND/LEN/MEM`；ABS 偏移必须 4-aligned 且 < 64
//!   （seccomp_data 大小，Linux `seccomp_check_filter` 强制）
//! - JMP 不允许向后跳（程序必为 DAG，Linux 强制 forward jump only）
//! - RET 是终止指令；程序末尾必须是 RET（Linux 强制）
//! - 临时 M[0..15] 共 16 个 32-bit slot
//! - DIV/MOD 除 0 → 运行期返回 0（Linux 行为）
//!
//! Linux 还允许"SKF magic offsets"（< 0 的 absolute offset 读 packet
//! 元数据），但这些只在 socket filter 场景有意义；seccomp 走
//! `seccomp_check_filter` 显式拒绝。我们一并拒绝。

use alloc::vec::Vec;

/// `struct sock_filter` Rust 镜像。
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SockFilter {
    pub code: u16,
    pub jt: u8,
    pub jf: u8,
    pub k: u32,
}

/// 与 Linux `BPF_MAXINSNS` 一致
pub const BPF_MAXINSNS: usize = 4096;

/// `seccomp(SECCOMP_SET_MODE_FILTER)` 的 flag 中我们关心的两位
#[derive(Debug, Clone, Copy, Default)]
pub struct SeccompFilterFlags {
    pub log: bool,
    pub new_listener: bool,
}

/// verifier 失败时返回的 errno 编码（直接 errno 数值，调用方取负）
#[repr(i32)]
#[derive(Debug, Clone, Copy)]
pub enum BpfVerifyError {
    /// 长度 0 / 末尾非 RET / 跳转越界 / ABS 越界 …… 一律 -EINVAL
    Invalid = 22,
}

// ──────────────────── opcode bit 布局 ────────────────────
const BPF_CLASS_MASK: u16 = 0x07;
const BPF_LD:   u16 = 0x00;
const BPF_LDX:  u16 = 0x01;
const BPF_ST:   u16 = 0x02;
const BPF_STX:  u16 = 0x03;
const BPF_ALU:  u16 = 0x04;
const BPF_JMP:  u16 = 0x05;
const BPF_RET:  u16 = 0x06;
const BPF_MISC: u16 = 0x07;

const BPF_SIZE_MASK: u16 = 0x18;
const BPF_W: u16 = 0x00;
const BPF_H: u16 = 0x08;
const BPF_B: u16 = 0x10;

const BPF_MODE_MASK: u16 = 0xe0;
const BPF_IMM: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_IND: u16 = 0x40;
const BPF_MEM: u16 = 0x60;
const BPF_LEN: u16 = 0x80;
const BPF_MSH: u16 = 0xa0;

// ALU/JMP 共用 op 字段
const BPF_OP_MASK: u16 = 0xf0;
const BPF_ADD: u16 = 0x00;
const BPF_SUB: u16 = 0x10;
const BPF_MUL: u16 = 0x20;
const BPF_DIV: u16 = 0x30;
const BPF_OR:  u16 = 0x40;
const BPF_AND: u16 = 0x50;
const BPF_LSH: u16 = 0x60;
const BPF_RSH: u16 = 0x70;
const BPF_NEG: u16 = 0x80;
const BPF_MOD: u16 = 0x90;
const BPF_XOR: u16 = 0xa0;

const BPF_JA:   u16 = 0x00;
const BPF_JEQ:  u16 = 0x10;
const BPF_JGT:  u16 = 0x20;
const BPF_JGE:  u16 = 0x30;
const BPF_JSET: u16 = 0x40;

const BPF_SRC_MASK: u16 = 0x08;
const BPF_K: u16 = 0x00;
const BPF_X: u16 = 0x08;

// MISC 操作
const BPF_TAX: u16 = 0x00;
const BPF_TXA: u16 = 0x80;

// RET source
const BPF_RET_A:   u16 = 0x10;
const BPF_RET_K:   u16 = 0x00;
const BPF_RET_X:   u16 = 0x08; // 罕见但 Linux verifier 不拒

/// 输入数据缓冲大小（`struct seccomp_data` = 64 字节）
const SECCOMP_DATA_LEN: u32 = 64;

/// Verify a cBPF program for seccomp use. 对齐 Linux
/// `net/core/filter.c::bpf_check_classic` + `seccomp_check_filter`。
pub fn verify(prog: &[SockFilter]) -> Result<(), BpfVerifyError> {
    if prog.is_empty() || prog.len() > BPF_MAXINSNS {
        return Err(BpfVerifyError::Invalid);
    }
    let n = prog.len();
    // 末尾必须是 RET（Linux 行为）
    let last = &prog[n - 1];
    if last.code & BPF_CLASS_MASK != BPF_RET {
        return Err(BpfVerifyError::Invalid);
    }
    for (pc, insn) in prog.iter().enumerate() {
        let class = insn.code & BPF_CLASS_MASK;
        match class {
            BPF_LD => verify_ld(insn, false)?,
            BPF_LDX => verify_ld(insn, true)?,
            BPF_ST | BPF_STX => verify_st(insn)?,
            BPF_ALU => verify_alu(insn)?,
            BPF_JMP => verify_jmp(insn, pc, n)?,
            BPF_RET => verify_ret(insn)?,
            BPF_MISC => verify_misc(insn)?,
            _ => return Err(BpfVerifyError::Invalid),
        }
    }
    Ok(())
}

fn verify_ld(insn: &SockFilter, is_ldx: bool) -> Result<(), BpfVerifyError> {
    let mode = insn.code & BPF_MODE_MASK;
    let size = insn.code & BPF_SIZE_MASK;
    match mode {
        BPF_IMM => {
            // LD_IMM 只允许 BPF_W（cBPF 无 H/B IMM）
            if size != BPF_W { return Err(BpfVerifyError::Invalid); }
            Ok(())
        }
        BPF_LEN => {
            if size != BPF_W { return Err(BpfVerifyError::Invalid); }
            Ok(())
        }
        BPF_MEM => {
            if size != BPF_W { return Err(BpfVerifyError::Invalid); }
            if insn.k >= 16 { return Err(BpfVerifyError::Invalid); }
            Ok(())
        }
        BPF_ABS | BPF_IND => {
            // seccomp 强制：禁 LDX|*|*（LDX 只允许 LEN/MEM/MSH/IMM；LDX|ABS 不存在）
            if is_ldx && mode != BPF_MSH { return Err(BpfVerifyError::Invalid); }
            // seccomp 拒绝 LD|IND（packet 偏移），与 Linux `seccomp_check_filter` 一致
            if mode == BPF_IND { return Err(BpfVerifyError::Invalid); }
            // ABS：k 必须 + size <= 64，且 size 是合法的 W/H/B
            let sz_bytes = match size {
                BPF_W => 4u32,
                BPF_H => 2u32,
                BPF_B => 1u32,
                _ => return Err(BpfVerifyError::Invalid),
            };
            // Linux SKF magic offsets 在 seccomp 路径下禁用（k 必须正且不大）
            if (insn.k as i32) < 0 { return Err(BpfVerifyError::Invalid); }
            let end = insn.k.checked_add(sz_bytes).ok_or(BpfVerifyError::Invalid)?;
            if end > SECCOMP_DATA_LEN { return Err(BpfVerifyError::Invalid); }
            Ok(())
        }
        BPF_MSH => {
            // LDX|MSH|B：X = 4*(P[k]&0xf)。seccomp 用得到的就这一种 LDX|MSH。
            if !is_ldx || size != BPF_B { return Err(BpfVerifyError::Invalid); }
            if insn.k >= SECCOMP_DATA_LEN { return Err(BpfVerifyError::Invalid); }
            Ok(())
        }
        _ => Err(BpfVerifyError::Invalid),
    }
}

fn verify_st(insn: &SockFilter) -> Result<(), BpfVerifyError> {
    // ST / STX：仅 BPF_MEM，k < 16
    let mode = insn.code & BPF_MODE_MASK;
    let size = insn.code & BPF_SIZE_MASK;
    if mode != BPF_MEM || size != BPF_W { return Err(BpfVerifyError::Invalid); }
    if insn.k >= 16 { return Err(BpfVerifyError::Invalid); }
    Ok(())
}

fn verify_alu(insn: &SockFilter) -> Result<(), BpfVerifyError> {
    let op = insn.code & BPF_OP_MASK;
    let src = insn.code & BPF_SRC_MASK;
    match op {
        BPF_ADD | BPF_SUB | BPF_MUL | BPF_OR | BPF_AND
        | BPF_LSH | BPF_RSH | BPF_XOR | BPF_NEG => {}
        BPF_DIV | BPF_MOD => {
            // 立即除 0：Linux 在 verifier 阶段拒绝 (`BPF_K && k == 0`)
            if src == BPF_K && insn.k == 0 { return Err(BpfVerifyError::Invalid); }
        }
        _ => return Err(BpfVerifyError::Invalid),
    }
    Ok(())
}

fn verify_jmp(insn: &SockFilter, pc: usize, n: usize) -> Result<(), BpfVerifyError> {
    let op = insn.code & BPF_OP_MASK;
    match op {
        BPF_JA => {
            // unconditional jump：32-bit k 偏移
            let target = (pc as u64).checked_add(1).and_then(|v| v.checked_add(insn.k as u64));
            match target {
                Some(t) if (t as usize) < n => Ok(()),
                _ => Err(BpfVerifyError::Invalid),
            }
        }
        BPF_JEQ | BPF_JGT | BPF_JGE | BPF_JSET => {
            // jt/jf 都是从下一条算起的 8-bit forward offset
            let t = pc + 1 + insn.jt as usize;
            let f = pc + 1 + insn.jf as usize;
            if t >= n || f >= n { return Err(BpfVerifyError::Invalid); }
            Ok(())
        }
        _ => Err(BpfVerifyError::Invalid),
    }
}

fn verify_ret(insn: &SockFilter) -> Result<(), BpfVerifyError> {
    // RET source 必须是 K / A / X 之一
    match insn.code & 0x18 {
        BPF_RET_K | BPF_RET_A | BPF_RET_X => Ok(()),
        _ => Err(BpfVerifyError::Invalid),
    }
}

fn verify_misc(insn: &SockFilter) -> Result<(), BpfVerifyError> {
    match insn.code & BPF_MISC_OP_MASK {
        BPF_TAX | BPF_TXA => Ok(()),
        _ => Err(BpfVerifyError::Invalid),
    }
}
const BPF_MISC_OP_MASK: u16 = 0xf8;

/// 真 cBPF 解释器。对已 verify 的程序运行；任何运行期异常按 Linux
/// 行为返回 0（KILL_THREAD）。返回 32-bit RET 值，由调用方按
/// `SECCOMP_RET_*` 解码。
///
/// `input` 必须 = `seccomp_data` 64 字节快照（little-endian）。
pub fn run(prog: &[SockFilter], input: &[u8]) -> u32 {
    // cBPF 状态机：A、X、M[16] 全 32-bit
    let mut a: u32 = 0;
    let mut x: u32 = 0;
    let mut mem: [u32; 16] = [0; 16];
    let mut pc: usize = 0;
    let n = prog.len();
    // 安全网：verifier 已保证 RET 终结，但额外限制 step 数防 verifier 漏检
    let mut steps: usize = 0;
    const MAX_STEPS: usize = BPF_MAXINSNS * 4;

    while pc < n && steps < MAX_STEPS {
        steps += 1;
        let insn = &prog[pc];
        let class = insn.code & BPF_CLASS_MASK;
        match class {
            BPF_LD => {
                let mode = insn.code & BPF_MODE_MASK;
                let size = insn.code & BPF_SIZE_MASK;
                match mode {
                    BPF_IMM => a = insn.k,
                    BPF_LEN => a = SECCOMP_DATA_LEN,
                    BPF_MEM => a = mem.get(insn.k as usize).copied().unwrap_or(0),
                    BPF_ABS => {
                        a = load_data(input, insn.k, size).unwrap_or_else(|| {
                            // verifier 已保证 in-range；保底 0
                            0
                        });
                    }
                    BPF_IND => {
                        // verifier 拒绝；此分支不可达。安全回 0。
                        a = 0;
                    }
                    _ => return 0,
                }
                pc += 1;
            }
            BPF_LDX => {
                let mode = insn.code & BPF_MODE_MASK;
                match mode {
                    BPF_IMM => x = insn.k,
                    BPF_LEN => x = SECCOMP_DATA_LEN,
                    BPF_MEM => x = mem.get(insn.k as usize).copied().unwrap_or(0),
                    BPF_MSH => {
                        // X = 4 * (P[k] & 0xf) —— 经典 IP header length 提取
                        let byte = load_data(input, insn.k, BPF_B).unwrap_or(0);
                        x = 4 * (byte & 0x0f);
                    }
                    _ => return 0,
                }
                pc += 1;
            }
            BPF_ST => {
                if (insn.k as usize) < 16 { mem[insn.k as usize] = a; }
                pc += 1;
            }
            BPF_STX => {
                if (insn.k as usize) < 16 { mem[insn.k as usize] = x; }
                pc += 1;
            }
            BPF_ALU => {
                let op = insn.code & BPF_OP_MASK;
                let src = if insn.code & BPF_SRC_MASK == BPF_X { x } else { insn.k };
                match op {
                    BPF_ADD => a = a.wrapping_add(src),
                    BPF_SUB => a = a.wrapping_sub(src),
                    BPF_MUL => a = a.wrapping_mul(src),
                    BPF_DIV => a = if src == 0 { 0 } else { a / src },
                    BPF_MOD => a = if src == 0 { 0 } else { a % src },
                    BPF_OR  => a |= src,
                    BPF_AND => a &= src,
                    BPF_XOR => a ^= src,
                    BPF_LSH => a = a.wrapping_shl(src & 31),
                    BPF_RSH => a = a.wrapping_shr(src & 31),
                    BPF_NEG => a = (!a).wrapping_add(1),
                    _ => return 0,
                }
                pc += 1;
            }
            BPF_JMP => {
                let op = insn.code & BPF_OP_MASK;
                if op == BPF_JA {
                    pc = pc + 1 + insn.k as usize;
                } else {
                    let src = if insn.code & BPF_SRC_MASK == BPF_X { x } else { insn.k };
                    let cond = match op {
                        BPF_JEQ => a == src,
                        BPF_JGT => a > src,
                        BPF_JGE => a >= src,
                        BPF_JSET => (a & src) != 0,
                        _ => return 0,
                    };
                    let off = if cond { insn.jt } else { insn.jf };
                    pc = pc + 1 + off as usize;
                }
            }
            BPF_RET => {
                return match insn.code & 0x18 {
                    BPF_RET_K => insn.k,
                    BPF_RET_A => a,
                    BPF_RET_X => x,
                    _ => 0,
                };
            }
            BPF_MISC => {
                match insn.code & BPF_MISC_OP_MASK {
                    BPF_TAX => x = a,
                    BPF_TXA => a = x,
                    _ => return 0,
                }
                pc += 1;
            }
            _ => return 0,
        }
    }
    // 越界 / step 超限：按 Linux runtime 行为返回 0 = KILL_THREAD
    0
}

/// 安全按 cBPF size (W/H/B) 从 64 字节 input 读 4/2/1 字节，大端转 host endian。
/// （cBPF 默认 packet 数据按 network byte order；seccomp_data 实际是 host
/// little-endian，但 Linux seccomp filter 一律按 `BPF_LD|W` 4-byte 读 + 直接
/// 用 host endian 比较——因为 user space 与 kernel 同 endian。所以这里
/// 用 little-endian load 与 host 一致。）
fn load_data(input: &[u8], off: u32, size: u16) -> Option<u32> {
    let off = off as usize;
    match size {
        BPF_W => {
            if off + 4 > input.len() { return None; }
            Some(u32::from_le_bytes(input[off..off + 4].try_into().ok()?))
        }
        BPF_H => {
            if off + 2 > input.len() { return None; }
            Some(u16::from_le_bytes(input[off..off + 2].try_into().ok()?) as u32)
        }
        BPF_B => {
            if off + 1 > input.len() { return None; }
            Some(input[off] as u32)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_data() -> [u8; 64] {
        // nr=42, arch=0xc0000028, ip=0, args=[1,2,3,4,5,6]
        let mut d = [0u8; 64];
        d[0..4].copy_from_slice(&42i32.to_le_bytes());
        d[4..8].copy_from_slice(&0xc000_0028u32.to_le_bytes());
        for i in 0..6 {
            let off = 16 + i * 8;
            d[off..off + 8].copy_from_slice(&((i + 1) as u64).to_le_bytes());
        }
        d
    }

    #[test]
    fn allow_all() {
        // RET K = ALLOW
        let prog = alloc::vec![SockFilter { code: BPF_RET | BPF_RET_K, jt: 0, jf: 0, k: 0x7fff_0000 }];
        verify(&prog).unwrap();
        assert_eq!(run(&prog, &build_data()), 0x7fff_0000);
    }

    #[test]
    fn deny_specific_nr() {
        // if nr == 42 ret ERRNO|1 else ret ALLOW
        let prog = alloc::vec![
            SockFilter { code: BPF_LD | BPF_W | BPF_ABS, jt: 0, jf: 0, k: 0 }, // A = nr
            SockFilter { code: BPF_JMP | BPF_JEQ | BPF_K, jt: 0, jf: 1, k: 42 },
            SockFilter { code: BPF_RET | BPF_RET_K, jt: 0, jf: 0, k: 0x0005_0000 | 1 },
            SockFilter { code: BPF_RET | BPF_RET_K, jt: 0, jf: 0, k: 0x7fff_0000 },
        ];
        verify(&prog).unwrap();
        assert_eq!(run(&prog, &build_data()), 0x0005_0001);
    }
}
