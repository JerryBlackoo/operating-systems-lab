    .section .text.entry
    .globl _start

_start:
    /* OpenSBI 以 S-mode 进入，中断已屏蔽。 */
    csrw sie, zero
    csrw sip, zero

    la sp, __stack_top

    /* 清零 .bss。 */
    la t0, __bss_start
    la t1, __bss_end
1:
    bgeu t0, t1, 2f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 1b
2:
    call kmain

3:
    wfi
    j 3b
