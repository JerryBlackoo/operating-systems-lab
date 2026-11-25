.section .text
    .globl _start
_start:
    call    console_start
1:
    j       1b