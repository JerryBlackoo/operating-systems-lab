    .syntax unified
    .arm

    .section .vectors, "ax"
    .global vector_table
    .align 5
vector_table:
    b       _start
    b       undef_handler
    b       svc_handler
    b       prefetch_handler
    b       data_handler
    b       reserved_handler
    b       irq_handler
    b       fiq_handler

    .text

undef_handler:
    sub     lr, lr, #4
    mov     r1, lr
    mrs     r2, spsr
    and     r3, r2, #0x1f
    cmp     r3, #0x10
    ldr     r0, =msg_undef
    beq     user_fault_entry
    mov     r3, #0
    b       arm_fault

prefetch_handler:
    sub     lr, lr, #4
    mov     r1, lr
    mrs     r2, spsr
    and     r3, r2, #0x1f
    cmp     r3, #0x10
    ldr     r0, =msg_prefetch
    beq     user_fault_entry
    mov     r3, #1
    b       arm_fault

data_handler:
    sub     lr, lr, #8
    mov     r1, lr
    mrs     r2, spsr
    and     r3, r2, #0x1f
    cmp     r3, #0x10
    ldr     r0, =msg_data
    beq     user_fault_entry
    mov     r3, #2
    b       arm_fault

/* 来自 User 模式的异常：切到 SVC（SVC sp 是该进程的内核栈顶），
 * 由 C 代码回收地址空间并让出 CPU；不再返回故障指令。 */
user_fault_entry:
    cps     #0x13
    bl      arm_user_fault_kill
1:
    b       1b

svc_handler:
    sub     sp, sp, #72
    stmia   sp, {r0-r12, lr}    @ [0..55]：用户 r0-r12 + SVC 返回地址

    /* 记录本次 SVC 的完整用户寄存器/返回点/用户栈/用户 LR，供 fork 复制与
     * 进程切换后恢复。用户 sp 和用户 lr 只能用 STM^ 直接读取，不能先 CPS
     * 到 User 模式：ARMv7 规定在 User 模式执行 CPS 是 UNPREDICTABLE，而且
     * 中间窗口用户态中断是开启的，可能在 User 模式接收 IRQ。 */
    mov     r0, lr
    mrs     r1, spsr
    str     r1, [sp, #64]       @ 保存本进程的 SPSR；切回来时硬件 SPSR 可能已被其他进程覆盖
    add     r3, sp, #56
    stmia   r3, {sp, lr}^       @ [56]=用户 sp，[60]=用户 lr
    ldr     r2, [r3]
    ldr     r4, [sp, #60]       @ 用户 LR 作为 arm_prepare_syscall 的第 5 个参数
    mov     r3, sp              @ regs 指针
    sub     sp, sp, #8
    str     r4, [sp]
    bl      arm_prepare_syscall
    add     sp, sp, #8

    ldr     r1, [sp, #0]
    ldr     r2, [sp, #4]
    ldr     r3, [sp, #8]
    mov     r0, r7
    bl      arm_syscall_dispatch
    str     r0, [sp, #0]
    /* 系统调用返回是 ARM 的调度安全点：消费定时器置的 need_resched。
     * 返回值已存回帧，sched_check 可以 clobber 调用者寄存器。 */
    bl      sched_check
    /* sched_check 可能在 SVC 模式下切到别的进程，而 SPSR 是 SVC 模式共享的；
     * 返回用户态前恢复本进程进入 SVC 时保存的用户 CPSR。 */
    ldr     r1, [sp, #64]
    msr     spsr_cxsf, r1
    /* User sp / User lr 是本 CPU 唯一的 User 银行寄存器；sched_check 期间
     * 其他用户进程可能在 User 模式运行并改写它们。返回前恢复本进程进入
     * SVC 时保存的值（[sp+56]、[sp+60]），否则 pop/局部变量会用到错误的栈，
     * 恢复后的函数也会用错误的 LR 返回。 */
    add     r2, sp, #56
    ldmia   r2, {sp, lr}^
    ldmia   sp, {r0-r12, lr}
    add     sp, sp, #72
    movs    pc, lr

reserved_handler:
    mov     r1, lr
    mrs     r2, spsr
    mov     r3, #4
    ldr     r0, =msg_reserved
    b       arm_fault

fiq_handler:
    mov     r1, lr
    mrs     r2, spsr
    mov     r3, #5
    ldr     r0, =msg_fiq
    b       arm_fault

/* IRQ 使用 IRQ 模式自己的栈；C ABI 要求 8 字节对齐，
 * push 6 个寄存器正好 24 字节，栈仍保持对齐。 */
irq_handler:
    sub     lr, lr, #4
    push    {r0-r3, r12, lr}
    mov     r0, sp
    bl      irq_dispatch
    pop     {r0-r3, r12, lr}
    movs    pc, lr

    .section .rodata
msg_undef:      .asciz "undefined instruction"
msg_prefetch:   .asciz "prefetch abort"
msg_data:       .asciz "data abort"
msg_svc:        .asciz "unexpected SVC"
msg_reserved:   .asciz "reserved exception"
msg_fiq:        .asciz "unexpected FIQ"
