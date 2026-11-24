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
    movb $0x80, %dl               # 和 stage1 一样，从 QEMU 的第一块 IDE 磁盘读。
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

.if KERNEL_SECTORS < 1 || KERNEL_SECTORS > 46
.error "Kernel sectors must fit between 0x1000 and the stack at 0x6c00"
.endif

.balign 4
kernel_dap:
    .byte 0x10, 0
    .word KERNEL_SECTORS, 0x1000, 0
    .quad 2                      # LBA 0 是 stage1，LBA 1 是本文件。

.org 510, 0
.word 0xaa55
