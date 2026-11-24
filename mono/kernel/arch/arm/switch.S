    .syntax unified
    .arm
    .section .text
    .globl swtch

/* void swtch(struct context *old, struct context *new); */
swtch:
    stmia   r0, {r4-r11}
    str     sp, [r0, #32]
    str     lr, [r0, #36]

    ldmia   r1, {r4-r11}
    ldr     sp, [r1, #32]
    ldr     lr, [r1, #36]
    bx      lr

/* void arm_enter_user(struct trap_frame *tf); 不返回。
 *
 * 不能用 cps 先切到 User 再切回 SVC：ARMv7 规定 CPS 在 User 模式
 * 是 UNPREDICTABLE（QEMU 当 NOP，真实 Cortex-A9 可能 UNDEF）。
 * 这里在 SVC 模式下设置 SPSR，用 LDM^ 恢复用户寄存器与用户 sp，
 * 最后 movs pc, lr 完成异常返回。 */
    .globl arm_enter_user
arm_enter_user:
    mov     r12, r0
    ldr     r2, [r12, #64]      @ 用户 CPSR
    msr     spsr_cxsf, r2
    ldr     lr, [r12, #60]      @ 用户 PC

    /* 用户代码由 memcpy 写入；D-cache 关闭时 I-cache 不会自动同步。
     * exec 可能复用刚释放的物理页，进入用户前必须失效整个 I-cache。 */
    mov     r0, #0
    dsb
    mcr     p15, 0, r0, c7, c5, 0   @ ICIALLU
    dsb
    isb

    /* 恢复 r0-r12、用户 sp 和用户 lr（tf->lr）。SVC 的 lr 已保存返回 PC，
     * LDM^ 只写 User 银行寄存器，不会破坏它。 */
    ldmia   r12, {r0-r12, sp, lr}^
    movs    pc, lr
