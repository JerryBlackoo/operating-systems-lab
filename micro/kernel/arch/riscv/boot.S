/*
 * boot.S — 时刻 1：让 C 能跑起来。
 *
 * 进入条件（OpenSBI 交接时保证）：
 *   a0 = hartid,  a1 = DTB 物理地址
 *   satp = 0 (bare), sstatus.SIE = 0, sp = 无意义
 *
 * 离开条件（call boot_main 的瞬间）只保证四件事：
 *   sp 有效 / .bss 全零 / a0 a1 完好 / 中断全关
 * 别的什么都没保证。
 *
 * 判据：这里每多一行，都必须能回答「不这样做 C 就跑不起来」。否则越界。
 */
    .section .text.entry
    .globl _start

_start:
    /* 0. 非 boot hart 不得碰共享启动栈和 .bss。
     *    有的固件会让所有 hart 从入口开始执行；这里直接停车，
     *    把单 hart 假设变成可执行事实（a0 = hartid，boot hart 为 0）。 */
    bnez    a0, 3f

    /* 1. 静默三个会立刻咬人的中断源。
     *    OpenSBI 已关 sstatus.SIE，但 sie/sip 的位还可能是脏的。 */
    csrw    sie, zero
    csrw    sip, zero

    /* 2. 建立不变量：S-mode 下 sscratch == 0。
     *    trap.S 靠这个零值区分「从 U 陷入」和「从 S 陷入」。 */
    csrw    sscratch, zero

    /* 3. 假栈。C 函数第一行就可能用栈，所以这必须发生在任何 call 之前。 */
    la      sp, __boot_stack_top

    /* 4. 把 a0/a1 搬进 callee-saved —— 清 bss 会用到 t0/t1。 */
    mv      s0, a0
    mv      s1, a1

    /* 5. 清 .bss。不清的话全局变量初始值全是 QEMU 内存里的垃圾。 */
    la      t0, __bss_start
    la      t1, __bss_end
1:
    bgeu    t0, t1, 2f
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       1b
2:
    /* 6. 交付给 C。 */
    mv      a0, s0
    mv      a1, s1
    call    boot_main

    /* boot_main 正常返回 = 设计错误。停机。 */
3:
    wfi
    j       3b
