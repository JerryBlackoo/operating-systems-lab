/*
 * trap.h — 陷阱帧。
 *
 * 与 context 的区别（本模块最易错的地方）：
 *
 *              trap_frame              context
 *   保存什么   用户可见的全部机器状态     内核执行流的最小续跑集
 *   内容       x1..x31 + 4 个 CSR        ra, sp, s0..s11
 *   大小       35 字 = 280 -> 对齐 288   14 字 = 112
 *   谁写       trap.S                   context.S
 *   何时       每次进/出内核             每次线程切换
 *
 * 为什么不存 x0：它恒为 0，存了浪费 8 字节。
 *
 * 顺序即 ABI：trap.S 里的偏移量必须与这里逐字段对应。
 * 改动任何字段顺序必须同时改 trap.S。
 */
#ifndef MICRO_ARCH_RISCV_TRAP_H
#define MICRO_ARCH_RISCV_TRAP_H

#include "types.h"

struct trap_frame {
    /* 通用寄存器。x0 不存；x2(sp) 单独处理；x4(tp) 也不存——tp 是 per-hart 常量。 */
    u64 ra;    /* x1  */
    u64 gp;    /* x3  */
    u64 t0;    /* x5  */
    u64 t1;    /* x6  */
    u64 t2;    /* x7  */
    u64 s0;    /* x8  */
    u64 s1;    /* x9  */
    u64 a0;    /* x10 */
    u64 a1;    /* x11 */
    u64 a2;    /* x12 */
    u64 a3;    /* x13 */
    u64 a4;    /* x14 */
    u64 a5;    /* x15 */
    u64 a6;    /* x16 */
    u64 a7;    /* x17 */
    u64 s2;    /* x18 */
    u64 s3;    /* x19 */
    u64 s4;    /* x20 */
    u64 s5;    /* x21 */
    u64 s6;    /* x22 */
    u64 s7;    /* x23 */
    u64 s8;    /* x24 */
    u64 s9;    /* x25 */
    u64 s10;   /* x26 */
    u64 s11;   /* x27 */
    u64 t3;    /* x28 */
    u64 t4;    /* x29 */
    u64 t5;    /* x30 */
    u64 t6;    /* x31 */
    u64 sp;    /* x2  —— 最后存，因为它就是 frame 基址 */
    u64 tp;    /* x4  —— 存但恢复时忽略（tp 恒为 hart_local） */

    /* 硬件写、软件抄进来的四个 CSR */
    u64 sepc;
    u64 sstatus;
    u64 scause;
    u64 stval;

    /* 填充到 288 字节（2^5 * 9），保持 16 字节对齐 */
    u64 pad;
};

_Static_assert(sizeof(struct trap_frame) == 288, "trap_frame must be 288 bytes");

/* 字段偏移，给 trap.S 用（改这里必须同步改 .S） */
#define TF_RA       0
#define TF_SP       232
#define TF_TP       240
#define TF_SEPC     248
#define TF_SSTATUS  256
#define TF_SCAUSE   264
#define TF_STVAL    272

/*
 * 安装 trap 入口（写 stvec，Direct 模式）。
 * 由 kernel/boot 在建立好 hart_local 之后调用一次。
 */
void trap_install(void);

/*
 * 唯一的 trap 接收函数。
 * 返回「下一个要跑的 frame」，不一定等于入参。
 */
struct trap_frame *trap_handler(struct trap_frame *tf);

/* 恢复 tf 并 sret。trap.S 提供。 */
void trap_return(struct trap_frame *tf) __attribute__((noreturn));

/* 停机：打印三元组后关机。trap.c 提供。 */
__attribute__((noreturn)) void panic(const char *why,
                                     const struct trap_frame *tf);

#endif /* MICRO_ARCH_RISCV_TRAP_H */
