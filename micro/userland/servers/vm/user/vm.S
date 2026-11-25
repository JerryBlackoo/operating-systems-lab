    .section .text
    .globl _start
_start:
    /* a0=recv, a1=ready(PM), a2=receipt(PM), a3=sid, a4=sm_send */
    call    vm_start
1:
    j       1b