    .section .text
    .globl _start
_start:
    mv      s0, a0
    mv      s1, a1
    mv      s2, a2
    mv      s3, a3
    mv      s4, a4
    mv      s5, a5

    /* 越权 send：只读 endpoint cap（ABI_IPC_SEND 形参 a0..a5） */
    mv      a0, s0
    li      a7, 3
    li      a1, 0x100
    li      a2, 0
    li      a3, 0
    li      a4, 0
    li      a5, 0
    ecall

    mv      a0, s0
    mv      a1, s1
    mv      a2, s2
    mv      a3, s3
    mv      a4, s4
    mv      a5, s5
    call    init_main
1:
    j       1b