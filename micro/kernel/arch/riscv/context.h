/*
 * context.h — 时刻 3：换线程时保存什么。
 *
 * 只有 14 个字。为什么不是 31 个：
 *   这是内核自己的函数调用。C 编译器已经保证了 caller-saved
 *   (a0-a7, t0-t6) 在函数调用后无意义；需要跨调用保住的值，
 *   编译器已经压到栈上了，而栈由 sp 代表。
 *   所以「sp + callee-saved」足以恢复任意内核执行流。
 */
#ifndef MICRO_ARCH_RISCV_CONTEXT_H
#define MICRO_ARCH_RISCV_CONTEXT_H

#include "types.h"

struct context {
    u64 ra;   /* 0   —— 恢复后 ret 跳到这里 */
    u64 sp;   /* 8   —— 该执行流的栈顶 */
    u64 s0;   /* 16  */
    u64 s1;   /* 24  */
    u64 s2;   /* 32  */
    u64 s3;   /* 40  */
    u64 s4;   /* 48  */
    u64 s5;   /* 56  */
    u64 s6;   /* 64  */
    u64 s7;   /* 72  */
    u64 s8;   /* 80  */
    u64 s9;   /* 88  */
    u64 s10;  /* 96  */
    u64 s11;  /* 104 */
};

_Static_assert(sizeof(struct context) == 112, "context must be 112 bytes");

/*
 * 保存当前执行流到 from，从 to 恢复并 ret。
 * 首次调度一个内核线程时，把 to->ra 填成线程入口、to->sp 填成栈顶即可 ——
 * 它会像「从这个函数返回」一样直接开始执行。
 */
void context_switch(struct context *from, struct context *to);

#endif /* MICRO_ARCH_RISCV_CONTEXT_H */
