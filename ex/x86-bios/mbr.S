.code16
.section .text
.global _start

_start:
    # BIOS 在 0x7c00 进入。先搬走整个 MBR，给将要读入的 PBR 腾出空间。
    cli
    xorw %ax, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    movw $0x7c00, %sp
    cld
    movw $0x7c00, %si
    movw $0x0600, %di
    movw $256, %cx
    rep movsw
    ljmp $0, $mbr_main

mbr_main:
    sti
    movb %dl, boot_drive
    movw $mbr_message, %si
    call print

    movw $partition_table, %si
    movw $4, %cx
find_active:
    cmpb $0x80, (%si)
    je read_pbr
    addw $16, %si
    loop find_active
    movw $no_active_message, %si
    jmp stop

read_pbr:
    movw %si, active_entry
    movl 8(%si), %eax            # 分区表项 +8 是起始 LBA，32 位小端。
    movl %eax, pbr_lba

    movb boot_drive, %dl
    movw $0x55aa, %bx
    movb $0x41, %ah              # 查询 BIOS 的扩展读盘能力。
    int $0x13
    jc disk_error
    cmpw $0xaa55, %bx
    jne disk_error
    testw $1, %cx
    jz disk_error

    movb boot_drive, %dl
    movw $disk_address_packet, %si
    movb $0x42, %ah              # 由 BIOS 按 DAP 读一个扇区到 0000:7c00。
    int $0x13
    jc disk_error
    cmpw $0xaa55, 0x7dfe
    jne bad_pbr

    movb boot_drive, %dl
    movw active_entry, %si       # 交给 PBR：DL 是磁盘号，DS:SI 指向分区表项。
    ljmp $0, $0x7c00

disk_error:
    movw $disk_error_message, %si
    jmp stop
bad_pbr:
    movw $bad_pbr_message, %si
stop:
    call print
    cli
1:  hlt
    jmp 1b

print:
    lodsb
    testb %al, %al
    jz 1f
    outb %al, $0xe9              # QEMU 调试控制台。
    jmp print
1:  ret

mbr_message: .asciz "MBR: stage 1\n"
no_active_message: .asciz "MBR: no active partition\n"
disk_error_message: .asciz "MBR: BIOS disk read unavailable or failed\n"
bad_pbr_message: .asciz "MBR: bad PBR signature\n"
boot_drive: .byte 0
active_entry: .word 0

.balign 4
disk_address_packet:
    .byte 0x10, 0                # DAP 大小、保留字节。
    .word 1, 0x7c00, 0           # 扇区数、目标 offset、目标 segment。
pbr_lba:
    .quad 0

.org 440, 0
    .long 0                      # 磁盘签名。
    .word 0                      # 保留字段。
partition_table:
    .byte 0x80, 0xfe, 0xff, 0xff  # 活动分区、CHS 占位。
    .byte 0x83, 0xfe, 0xff, 0xff  # 分区类型、CHS 占位。
    .long PARTITION_LBA, PARTITION_SECTORS
    .zero 16 * 3
    .word 0xaa55                 # 磁盘偏移 0x1fe 处的 55 aa。
