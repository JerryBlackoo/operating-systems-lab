/*
 * context.S — 时刻 3 的实现。14 存 + 14 读 + ret。
 *
 * void context_switch(struct context *from, struct context *to);
 *   a0 = from, a1 = to
 *
 * 注意 ret 不是 return 到 context_switch 的调用者，
 * 而是 return 到 *to* 上次被保存时的 ra —— 也就是「那个线程当时停在哪」。
 */
    .section .text
    .globl context_switch

context_switch:
    sd      ra,  0(a0)
    sd      sp,  8(a0)
    sd      s0, 16(a0)
    sd      s1, 24(a0)
    sd      s2, 32(a0)
    sd      s3, 40(a0)
    sd      s4, 48(a0)
    sd      s5, 56(a0)
    sd      s6, 64(a0)
    sd      s7, 72(a0)
    sd      s8, 80(a0)
    sd      s9, 88(a0)
    sd      s10, 96(a0)
    sd      s11, 104(a0)

    ld      ra,  0(a1)
    ld      sp,  8(a1)
    ld      s0, 16(a1)
    ld      s1, 24(a1)
    ld      s2, 32(a1)
    ld      s3, 40(a1)
    ld      s4, 48(a1)
    ld      s5, 56(a1)
    ld      s6, 64(a1)
    ld      s7, 72(a1)
    ld      s8, 80(a1)
    ld      s9, 88(a1)
    ld      s10, 96(a1)
    ld      s11, 104(a1)
    ret
