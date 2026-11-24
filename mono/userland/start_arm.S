    .syntax unified
    .arm
    .section .text.entry
    .globl _start

_start:
    bl      main
    mov     r7, #3
    svc     #0
1:
    b       1b
