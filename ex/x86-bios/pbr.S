.code16
.section .text
.global _start

_start:
    # MBR 传来 DL 和 DS:SI。镜像把内核放在分区内的第 2 个扇区。
    movl 8(%si), %eax
    incl %eax
    movl %eax, kernel_lba

    cld
    movw $pbr_message, %si
    call print

    movw $disk_address_packet, %si
    movb $0x42, %ah
    int $0x13
    jc disk_error
    ljmp $0, $0x1000

disk_error:
    movw $disk_error_message, %si
    call print
    cli
1:  hlt
    jmp 1b

print:
    lodsb
    testb %al, %al
    jz 1f
    outb %al, $0xe9
    jmp print
1:  ret

pbr_message: .asciz "PBR: stage 2\n"
disk_error_message: .asciz "PBR: kernel read failed\n"

.balign 4
disk_address_packet:
    .byte 0x10, 0
    .word 1, 0x1000, 0
kernel_lba:
    .quad 0

.org 510, 0
    .word 0xaa55
