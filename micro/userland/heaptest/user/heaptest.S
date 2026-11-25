    .section .text
    .globl _start
_start:
    /* a0=vm endpoint, a1=role（由 loader 传入） */
    call    heaptest_start
1:
    j       1b