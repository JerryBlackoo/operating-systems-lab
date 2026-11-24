    .syntax unified
    .arm
    .fpu neon
    .section .text.entry, "ax"
    .global _start

/* Zynq 上电后由 FSBL/U-Boot（或 QEMU）跳到 _start：
 * MMU、D-cache 关闭，CPU 处于 SVC 模式。 */
_start:
    cpsid   if

    /* U-Boot 的 go 命令不会关闭 MMU/cache。入口处先关 MMU、D-cache、I-cache，
     * 失效 TLB 与 cache，之后再由 arch_vm_init 用本内核的页表重新打开。 */
    mrc     p15, 0, r0, c1, c0, 0
    bic     r0, r0, #1          @ MMU
    bic     r0, r0, #4          @ D-cache
    bic     r0, r0, #0x1000     @ I-cache
    mcr     p15, 0, r0, c1, c0, 0
    isb

    mov     r0, #0
    mcr     p15, 0, r0, c8, c7, 0   @ TLBIALL
    mcr     p15, 0, r0, c7, c5, 0   @ ICIALLU
    mcr     p15, 0, r0, c7, c6, 0   @ invalidate D-cache
    dsb
    isb

    /* 先安装异常向量，避免早期异常跳到 0 地址。 */
    ldr     r0, =vector_table
    mcr     p15, 0, r0, c12, c0, 0
    isb

    /* 为每个异常模式准备独立栈。 */
    msr     cpsr_c, #0xd2          @ IRQ
    ldr     sp, =__irq_stack_top
    msr     cpsr_c, #0xd7          @ ABT
    ldr     sp, =__abt_stack_top
    msr     cpsr_c, #0xdb          @ UND
    ldr     sp, =__und_stack_top
    msr     cpsr_c, #0xd1          @ FIQ
    ldr     sp, =__fiq_stack_top
    msr     cpsr_c, #0xd3          @ SVC
    ldr     sp, =__svc_stack_top

    /* 打开 VFP/NEON，防止编译器在 -O2 下生成向量指令。 */
    mrc     p15, 0, r0, c1, c0, 2
    orr     r0, r0, #(0xf << 20)
    mcr     p15, 0, r0, c1, c0, 2
    isb
    ldr     r0, =0x40000000
    vmsr    fpexc, r0
    isb

    /* 清零 .bss。 */
    ldr     r0, =__bss_start
    ldr     r1, =__bss_end
    mov     r2, #0
1:
    cmp     r0, r1
    bhs     2f
    str     r2, [r0], #4
    b       1b
2:
    bl      kmain

3:
    wfi
    b       3b
