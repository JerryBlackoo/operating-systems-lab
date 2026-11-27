.code16
.section .text
.global _start

/*
 * stage2：从 LBA 2 读入内核到 0x20000，然后在本段（0x7e00，低于 64 KiB）
 * 里建立 GDT 并切到 32 位保护模式，最后以 32 位远跳直接进入内核入口。
 *
 * 为什么 GDT 放在这里而不是内核里：实模式只有 16 位偏移，无法引用
 * 0x20000 以上的符号。内核入口因此做成纯 32 位代码，切模式的责任留给
 * 这段仍位于低 64 KiB 的引导器——这也是绝大多数 bootloader 的分工。
 */
_start:
    cli
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %ss
    movw $0x7c00, %sp
    sti

    movw $kernel_dap, %si
    movb $0x42, %ah
    movb $0x80, %dl               /* QEMU 第一块 IDE 磁盘 */
    pushw %ds
    int $0x13
    popw %ds
    jc read_failed

    /* 进入保护模式：GDT 就在本段内，16 位偏移即可寻址 */
    cli
    lgdt gdt_descriptor

    movl %cr0, %eax
    orl $1, %eax                  /* CR0.PE */
    movl %eax, %cr0

    /* ljmp ptr16:32 (0x66 前缀)，目标 32 位偏移 = 内核入口 0x20000 */
    .byte 0x66, 0xea
    .long KERNEL_BASE
    .word 0x0008

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

/* DAP 的扇区计数字段只有 1 字节，最多 127 扇区 */
.if KERNEL_SECTORS < 1 || KERNEL_SECTORS > 120
.error "KERNEL_SECTORS must be between 1 and 120"
.endif

.balign 4
kernel_dap:
    .byte 0x10, 0                 /* 结构大小 16 字节 */
    .byte KERNEL_SECTORS, 0       /* 传输扇区数，保留 */
    .word KERNEL_BASE & 0xf, ((KERNEL_BASE >> 4) & 0xffff)  /* offset:segment */
    .quad 2                       /* LBA0 = stage1，LBA1 = 本文件 */

.balign 8
gdt:
    .quad 0
    .quad 0x00cf9a000000ffff      /* 0x08：32 位代码段，基址 0，限长 4 GiB */
    .quad 0x00cf92000000ffff      /* 0x10：32 位数据段，基址 0，限长 4 GiB */
gdt_end:
gdt_descriptor:
    .word gdt_end - gdt - 1
    .long gdt                     /* 本段内地址，低于 64 KiB */

.org 510, 0
.word 0xaa55
