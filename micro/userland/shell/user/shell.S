.section .text
    .globl _start
_start:
    call    shell_start
1:
    j       1b