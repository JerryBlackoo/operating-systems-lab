/*
 * RISC-V 64 上下文切换。
 *
 * void cpu_switch(struct cpu_context *old_ctx, struct cpu_context *new_ctx);
 * void cpu_switch_to(struct cpu_context *new_ctx);
 *
 * 只保存 callee-saved 寄存器；caller-saved 已由 trap 入口保存在陷阱帧中。
 * 偏移必须与 sched.h 的 struct cpu_context 一致。
 */

    .section .text
    .align 2
    .globl cpu_switch
    .type cpu_switch, @function
cpu_switch:
    sd ra,   0(a0)
    sd sp,   8(a0)
    sd s0,  16(a0)
    sd s1,  24(a0)
    sd s2,  32(a0)
    sd s3,  40(a0)
    sd s4,  48(a0)
    sd s5,  56(a0)
    sd s6,  64(a0)
    sd s7,  72(a0)
    sd s8,  80(a0)
    sd s9,  88(a0)
    sd s10, 96(a0)
    sd s11, 104(a0)

    ld ra,   0(a1)
    ld sp,   8(a1)
    ld s0,  16(a1)
    ld s1,  24(a1)
    ld s2,  32(a1)
    ld s3,  40(a1)
    ld s4,  48(a1)
    ld s5,  56(a1)
    ld s6,  64(a1)
    ld s7,  72(a1)
    ld s8,  80(a1)
    ld s9,  88(a1)
    ld s10, 96(a1)
    ld s11, 104(a1)
    ret
    .size cpu_switch, . - cpu_switch

    .globl cpu_switch_to
    .type cpu_switch_to, @function
cpu_switch_to:
    ld ra,   0(a0)
    ld sp,   8(a0)
    ld s0,  16(a0)
    ld s1,  24(a0)
    ld s2,  32(a0)
    ld s3,  40(a0)
    ld s4,  48(a0)
    ld s5,  56(a0)
    ld s6,  64(a0)
    ld s7,  72(a0)
    ld s8,  80(a0)
    ld s9,  88(a0)
    ld s10, 96(a0)
    ld s11, 104(a0)
    ret
    .size cpu_switch_to, . - cpu_switch_to
