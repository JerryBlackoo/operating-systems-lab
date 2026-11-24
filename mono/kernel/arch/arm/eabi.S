/*
 * ARM EABI 除法辅助函数的 RTABI 入口。
 *
 * RTABI 约定：
 *   __aeabi_uldivmod(n in r0:r1, d in r2:r3) -> quot in r0:r1, rem in r2:r3
 *   __aeabi_uidivmod(n in r0, d in r1)       -> quot in r0, rem in r1
 *
 * C 语言无法直接表达这种“返回值放在多个寄存器”的约定，因此这里用汇编
 * 包装普通 AAPCS 的算法实现 arm_*divmod_c；否则编译器按 struct 返回约定
 * 传一个隐藏指针，调用方按 RTABI 传参，会直接破坏内存。
 */

    .syntax unified
    .arm
    .text

    .globl __aeabi_uldivmod
__aeabi_uldivmod:
    push    {r4, lr}
    sub     sp, sp, #24         @ [sp]=out 指针，[sp+8..23]=结果缓冲区
    add     r4, sp, #8
    str     r4, [sp]
    bl      arm_uldivmod_c
    ldr     r0, [r4]            @ quot 低 32 位
    ldr     r1, [r4, #4]        @ quot 高 32 位
    ldr     r2, [r4, #8]        @ rem 低 32 位
    ldr     r3, [r4, #12]       @ rem 高 32 位
    add     sp, sp, #24
    pop     {r4, pc}

    .globl __aeabi_uidivmod
__aeabi_uidivmod:
    push    {r4, lr}            @ 保持 8 字节栈对齐
    sub     sp, sp, #8
    mov     r2, sp
    bl      arm_uidivmod_c
    ldr     r0, [sp]            @ quot
    ldr     r1, [sp, #4]        @ rem
    add     sp, sp, #8
    pop     {r4, pc}
