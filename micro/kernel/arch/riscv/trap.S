/*
 * trap.S — 时刻 2：唯一的硬件 → 内核入口。
 *
 * stvec 用 Direct 模式（mode=0）：所有 trap 都跳到 trap_entry。
 *
 * 进入这一刻的寄存器状态：
 *   来自 U-mode: sp = 用户栈，sscratch = 该线程内核栈顶
 *   来自 S-mode: sp = 内核栈，sscratch = 0（boot.S 建立的不变量）
 *
 * 帧布局必须与 trap.h 的 struct trap_frame 逐字节对应（288 字节）。
 *
 * 出口有三个：
 *   trap_return(tf)              —— 恢复 tf 并 sret
 *   （未来）kernel_thread_bootstrap —— 首次调度内核线程
 *   （未来）user_thread_bootstrap   —— 首次调度用户线程 = 拿构造好的 frame 调 trap_return
 */
    .section .text
    .globl trap_install
    .balign 4
trap_install:
    la      t0, trap_entry
    csrw    stvec, t0
    ret

    .balign 4
    .globl trap_entry
    .globl trap_return

/* ---------------------------------------------------------------- */
trap_entry:
    /*
     * 一条指令换栈。
     *   来自 U: sp ← sscratch（内核栈顶），sscratch ← 用户 sp
     *   来自 S: sp ← 0，sscratch ← 原内核 sp
     * 这依赖「S-mode 下 sscratch 恒为 0」这个不变量。
     */
    csrrw   sp, sscratch, sp
    bnez    sp, .Lfrom_u

/* ---- 来自 S-mode（内核自己出错，或将来内核里的 ebreak） ---- */
.Lfrom_s:
    csrr    sp, sscratch          /* 取回原内核 sp */
    csrw    sscratch, zero        /* 恢复不变量 */
    addi    sp, sp, -288

    /* t0 即将被用作临时寄存器，先存原始值 */
    sd      t0, 16(sp)
    addi    t0, sp, 288           /* 原始 sp = 帧基址 + 288 */
    sd      t0, 232(sp)
    j       .Lsave_rest

/* ---- 来自 U-mode ---- */
.Lfrom_u:
    addi    sp, sp, -288

    sd      t0, 16(sp)            /* 先存用户的 t0 */
    csrr    t0, sscratch          /* 用户 sp 暂存在 sscratch 里 */
    sd      t0, 232(sp)
    csrw    sscratch, zero        /* 内核态期间 sscratch 归零 */
    j       .Lsave_rest

/* ---- 两条路径汇合：保存剩余通用寄存器 ---- */
.Lsave_rest:
    sd      ra,    0(sp)
    sd      gp,    8(sp)
    sd      t1,   24(sp)
    sd      t2,   32(sp)
    sd      s0,   40(sp)
    sd      s1,   48(sp)
    sd      a0,   56(sp)
    sd      a1,   64(sp)
    sd      a2,   72(sp)
    sd      a3,   80(sp)
    sd      a4,   88(sp)
    sd      a5,   96(sp)
    sd      a6,  104(sp)
    sd      a7,  112(sp)
    sd      s2,  120(sp)
    sd      s3,  128(sp)
    sd      s4,  136(sp)
    sd      s5,  144(sp)
    sd      s6,  152(sp)
    sd      s7,  160(sp)
    sd      s8,  168(sp)
    sd      s9,  176(sp)
    sd      s10, 184(sp)
    sd      s11, 192(sp)
    sd      t3,  200(sp)
    sd      t4,  208(sp)
    sd      t5,  216(sp)
    sd      t6,  224(sp)
    sd      tp,  240(sp)

    /* 硬件已经写好的四个 CSR，抄进帧里 */
    csrr    t0, sepc
    sd      t0, 248(sp)
    csrr    t0, sstatus
    sd      t0, 256(sp)
    csrr    t0, scause
    sd      t0, 264(sp)
    csrr    t0, stval
    sd      t0, 272(sp)

    /* trap_handler(tf) -> 下一个要跑的 tf' */
    mv      a0, sp
    call    trap_handler
    /* a0 = tf' */
    j       trap_return

/* ---------------------------------------------------------------- */
/*
 * void trap_return(struct trap_frame *tf);
 *
 * 这是全模块唯一合法的「回到某个上下文」的出口。
 * 它可以回到「刚才陷入的那个」，也可以回到「另一个线程」——
 * 微内核必须在类型层面允许后者（trap_handler 返回别的 frame）。
 */
trap_return:
    mv      sp, a0

    /*
     * 设定 sscratch：
     *   回到 U-mode -> sscratch = 内核栈顶（= tf + 288）
     *   回到 S-mode -> sscratch = 0
     * 判据只能是 sstatus.SPP，不能靠「sscratch 是否为 0」这种魔法值。
     */
    ld      t0, 256(sp)
    andi    t0, t0, 0x100         /* SSTATUS_SPP */
    bnez    t0, .Lret_to_s
    addi    t0, sp, 288
    csrw    sscratch, t0
    j       .Lret_csr

.Lret_to_s:
    csrw    sscratch, zero

.Lret_csr:
    /* 先写 sstatus（含 SPIE），sret 会用它恢复 SIE */
    ld      t0, 256(sp)
    csrw    sstatus, t0
    ld      t0, 248(sp)
    csrw    sepc, t0

    /* 恢复通用寄存器；t0 / tp / sp 必须最后三个 */
    ld      ra,    0(sp)
    ld      gp,    8(sp)
    ld      t1,   24(sp)
    ld      t2,   32(sp)
    ld      s0,   40(sp)
    ld      s1,   48(sp)
    ld      a0,   56(sp)
    ld      a1,   64(sp)
    ld      a2,   72(sp)
    ld      a3,   80(sp)
    ld      a4,   88(sp)
    ld      a5,   96(sp)
    ld      a6,  104(sp)
    ld      a7,  112(sp)
    ld      s2,  120(sp)
    ld      s3,  128(sp)
    ld      s4,  136(sp)
    ld      s5,  144(sp)
    ld      s6,  152(sp)
    ld      s7,  160(sp)
    ld      s8,  168(sp)
    ld      s9,  176(sp)
    ld      s10, 184(sp)
    ld      s11, 192(sp)
    ld      t3,  200(sp)
    ld      t4,  208(sp)
    ld      t5,  216(sp)
    ld      t6,  224(sp)

    ld      t0,  16(sp)
    ld      tp, 240(sp)
    ld      sp, 232(sp)           /* 最后恢复 sp：此后再无帧可访问 */
    sret
