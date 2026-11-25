    .section .text
    .globl _start
_start:
    mv      s0, a0
    mv      s1, a1
    mv      a0, s0
    mv      a1, s1
    call    stub_start
1:
    j       1b