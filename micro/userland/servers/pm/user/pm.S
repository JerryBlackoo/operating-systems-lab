    .section .text
    .globl _start
_start:
    mv      a0, a0
    call    pm_start
1:
    j       1b