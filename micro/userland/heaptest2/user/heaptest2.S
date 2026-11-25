    .section .text
    .globl _start
_start:
    /* a0=vm endpoint, a1=role（loader：heaptest2 为 1） */
    call    heaptest_start
1:
    j       1b