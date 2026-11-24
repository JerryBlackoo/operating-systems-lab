/*
 * S-mode 陷阱入口。
 *
 * 进入时：
 *   - 从 U-mode 陷入：sscratch = 内核栈顶，sp = 用户栈
 *   - 从 S-mode 陷入：sscratch = 0，sp = 内核栈
 *
 * 帧布局与 trap.h 的 struct trap_frame 一致：
 *   x[0..31] 在 0..248，sepc 在 256，sstatus 在 264；每帧 288 字节。
 */

    .section .text
    .balign 4
    .globl trap_entry

trap_entry:
    csrrw   sp, sscratch, sp
    bnez    sp, .Lfrom_user

    /* 来自 S-mode：第一次交换后 sp=0，sscratch=旧 sp。 */
    csrrw   sp, sscratch, sp
    addi    sp, sp, -288
    sd      x1, 8(sp)
    sd      x5, 40(sp)
    addi    t0, sp, 288
    sd      t0, 16(sp)
    j       .Lsave

.Lfrom_user:
    addi    sp, sp, -288
    sd      x1, 8(sp)
    sd      x5, 40(sp)
    csrr    t0, sscratch
    sd      t0, 16(sp)
    csrw    sscratch, zero

.Lsave:
    sd      x3, 24(sp)
    sd      x4, 32(sp)
    sd      x6, 48(sp)
    sd      x7, 56(sp)
    sd      x8, 64(sp)
    sd      x9, 72(sp)
    sd      x10, 80(sp)
    sd      x11, 88(sp)
    sd      x12, 96(sp)
    sd      x13, 104(sp)
    sd      x14, 112(sp)
    sd      x15, 120(sp)
    sd      x16, 128(sp)
    sd      x17, 136(sp)
    sd      x18, 144(sp)
    sd      x19, 152(sp)
    sd      x20, 160(sp)
    sd      x21, 168(sp)
    sd      x22, 176(sp)
    sd      x23, 184(sp)
    sd      x24, 192(sp)
    sd      x25, 200(sp)
    sd      x26, 208(sp)
    sd      x27, 216(sp)
    sd      x28, 224(sp)
    sd      x29, 232(sp)
    sd      x30, 240(sp)
    sd      x31, 248(sp)

    csrr    t0, sepc
    sd      t0, 256(sp)
    csrr    t0, sstatus
    sd      t0, 264(sp)

    mv      a0, sp
    call    trap_dispatch

    mv      a0, sp
    j       trap_return

/*
 * void forkret(void);
 *
 * 新建进程第一次被调度时 ctx.ra 指向这里，sp 为内核栈顶。
 * 不建立栈帧，直接以栈顶下方的初始 trap frame 返回用户态。
 */
    .globl forkret
forkret:
    addi    a0, sp, -288
    j       trap_return

/*
 * void trap_return(struct trap_frame *tf);
 * 恢复全部整数寄存器、sepc、sstatus，然后 sret。
 */
    .globl trap_return
trap_return:
    mv      sp, a0

    ld      t0, 256(sp)
    csrw    sepc, t0
    ld      t0, 264(sp)
    csrw    sstatus, t0

    /* 返回 U-mode 时，下一次陷阱要从内核栈顶开始。 */
    li      t1, 0x100
    and     t1, t0, t1
    bnez    t1, .Lret_kernel
    addi    t1, sp, 288
    csrw    sscratch, t1
    j       .Lrestore

.Lret_kernel:
    csrw    sscratch, zero

.Lrestore:
    ld      x1, 8(sp)
    ld      x3, 24(sp)
    ld      x4, 32(sp)
    ld      x5, 40(sp)
    ld      x6, 48(sp)
    ld      x7, 56(sp)
    ld      x8, 64(sp)
    ld      x9, 72(sp)
    ld      x10, 80(sp)
    ld      x11, 88(sp)
    ld      x12, 96(sp)
    ld      x13, 104(sp)
    ld      x14, 112(sp)
    ld      x15, 120(sp)
    ld      x16, 128(sp)
    ld      x17, 136(sp)
    ld      x18, 144(sp)
    ld      x19, 152(sp)
    ld      x20, 160(sp)
    ld      x21, 168(sp)
    ld      x22, 176(sp)
    ld      x23, 184(sp)
    ld      x24, 192(sp)
    ld      x25, 200(sp)
    ld      x26, 208(sp)
    ld      x27, 216(sp)
    ld      x28, 224(sp)
    ld      x29, 232(sp)
    ld      x30, 240(sp)
    ld      x31, 248(sp)
    ld      x2, 16(sp)
    sret
