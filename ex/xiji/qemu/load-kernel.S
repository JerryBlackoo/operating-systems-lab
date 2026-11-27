.code16
.section .text
.global _start

_start:
    cli
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %ss
    movw $0x7c00, %sp
    sti

    movw $kernel_dap, %si
    movb $0x42, %ah
    movb $0x80, %dl
    pushw %ds
    int $0x13
    popw %ds
    jc read_failed
    ljmp $0, $0x1000

read_failed:
    cli
    cld
    movw $error_message, %si
1:  lodsb
    testb %al, %al
    jz 2f
    outb %al, $0xe9
    jmp 1b
2:  hlt
    jmp 2b

error_message: .asciz "stage2: kernel read failed\n"

.if KERNEL_SECTORS < 1 || KERNEL_SECTORS > 100
.error "Kernel sectors must fit in stage2 read window"
.endif

.balign 4
kernel_dap:
    .byte 0x10, 0
    .word KERNEL_SECTORS, 0x1000, 0
    .quad 2

.org 510, 0
.word 0xaa55