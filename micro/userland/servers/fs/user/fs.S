    .section .text
    .globl _start
_start:
    /* a0/a1/a2 已由 loader 填入 ready_send / receipt / fs_recv */
    call    fs_start
1:
    j       1b