/*
 * user_prog.S — 多地址空间闭环里的真实 U-mode 程序。
 *
 * 这段代码在内核镜像里只作为“模板”存在；启动时被逐字节拷进一个物理帧，
 * 再把这个帧映射到两个不同 AS 的同一用户 VA。因此这里不能出现任何需要
 * 重定位的符号引用：只用立即数、PC 相对跳转和 ecall。
 *
 * 入口参数（由 closure.c 构造的初始 trap frame 给出）：
 *   a0 = role（1 = 客户端，2 = 服务端）
 *   a1 = endpoint 句柄
 *   a2 = probe VA（A 的 AS 可写，B 的 AS 故意不映射）
 *
 * 所有打印都由内核在 ecall/fault 路径完成，用户程序不访问 UART。
 */
#define SYS_THREAD_EXIT 2
#define SYS_IPC_CALL    5
#define SYS_IPC_RECV    4
#define SYS_IPC_REPLY   6

#define TAG_CALL        0x101
#define TAG_REPLY       0x102

#define SPIN_CLIENT     12000000
#define SPIN_SERVER     15000000

    .section .text
    .option norvc
    .balign 16
    .globl user_prog_start
    .globl user_prog_end

user_prog_start:
    mv      s0, a0              /* role */
    mv      s1, a1              /* endpoint handle */
    mv      s2, a2              /* probe VA */
    li      t0, 1
    beq     s0, t0, .Lclient
    j       .Lserver

/* ---------------------------------------------------------------- */
/* 客户端：先证明自己的用户页可写，再发起一次阻塞 CALL。            */
/* ---------------------------------------------------------------- */
.Lclient:
    li      t0, 0x5a5a5a5a5a5a5a5a
    sd      t0, 0(s2)
    ld      a2, 0(s2)           /* word0 携带实际读回的值 */

    li      t0, SPIN_CLIENT
1:
    addi    t0, t0, -1
    bnez    t0, 1b

    mv      a0, s1
    li      a1, TAG_CALL
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a7, SYS_IPC_CALL
    ecall

    bnez    a0, .Lclient_fail
    slli    t1, a1, 32          /* meta[31:0] = tag */
    srli    t1, t1, 32
    li      t0, TAG_REPLY
    bne     t1, t0, .Lclient_fail
    li      t0, 0x5a5a5a5a5a5a5a5b
    bne     a2, t0, .Lclient_fail

    li      a0, 0
    li      a1, 0
    li      a2, 0
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a7, SYS_THREAD_EXIT
    ecall
    j       .

.Lclient_fail:
    li      a0, 0
    li      a1, 0
    li      a2, 0
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a7, 99               /* 非法 syscall：内核必须记录并拒绝 */
    ecall
    li      a0, 0
    li      a1, 0
    li      a2, 0
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a7, SYS_THREAD_EXIT
    ecall
    j       .

/* ---------------------------------------------------------------- */
/* 服务端：RECV 阻塞 -> REPLY -> 非法 syscall -> 跨 AS 非法访问。    */
/* ---------------------------------------------------------------- */
.Lserver:
    li      t0, SPIN_SERVER
2:
    addi    t0, t0, -1
    bnez    t0, 2b

    mv      a0, s1
    li      a1, 0
    li      a2, 0
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a7, SYS_IPC_RECV
    ecall

    bnez    a0, .Lserver_fail
    slli    t1, a1, 32
    srli    t1, t1, 32
    li      t0, TAG_CALL
    bne     t1, t0, .Lserver_fail
    li      t0, 0x5a5a5a5a5a5a5a5a
    bne     a2, t0, .Lserver_fail

    srli    a0, a1, 32          /* 内核签发的单次 reply token */
    beqz    a0, .Lserver_fail
    li      a1, TAG_REPLY
    li      a2, 0x5a5a5a5a5a5a5a5b
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a7, SYS_IPC_REPLY
    ecall
    bnez    a0, .Lserver_fail

    /* 错误路径一：非法 syscall，必须返回 ABI_BAD_NR（-1）。 */
    li      a0, 0
    li      a1, 0
    li      a2, 0
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a7, 99
    ecall

    /*
     * 非法调用必须失败返回 ABI_BAD_NR（-1）。若内核误把它当成成功，
     * 这里直接走 fail，而不是继续做隔离测试。
     */
    li      t0, -1
    bne     a0, t0, .Lserver_fail

    /* 错误路径二：访问 A 可写、B 未映射的同一 VA，必须缺页。 */
    ld      t0, 0(s2)

    /* 如果上面没有 fault，说明隔离失败。 */
    j       .Lserver_fail

.Lserver_fail:
    li      a0, 0
    li      a1, 0
    li      a2, 0
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a7, 99
    ecall
    li      a0, 0
    li      a1, 0
    li      a2, 0
    li      a3, 0
    li      a4, 0
    li      a5, 0
    li      a7, SYS_THREAD_EXIT
    ecall
    j       .

user_prog_end: