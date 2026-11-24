/* stage1.S —— MBR 引导扇区，BIOS 上电后加载到 0x7C00，必须正好 512 字节
 * 职责（对应教材第 ④⑤ 步）：
 *   1. 建立最小运行环境（段寄存器/栈）
 *   2. 用 int 13h AH=42h (LBA) 把 LBA=1 的扇区读到 0x7E00
 *   3. 远跳转到 stage2（对应教材第 ⑥⑦ 步的"控制权交接"）
 */
.code16
.section .text
.globl _start

_start:
	cli
	xorw	%ax, %ax
	movw	%ax, %ds
	movw	%ax, %es
	movw	%ax, %ss
	movw	$0x7C00, %sp
	sti

	movw	$msg1, %si
	call	puts

	/* int 13h AH=42h: Extended Read，DAP 在 dap 处 */
	movw	$dap, %si
	movb	$0x42, %ah
	movb	$0x80, %dl		/* 0x80 = 第一块硬盘 */
	int	$0x13
	jc	disk_err

	/* 远跳转到 0000:7E00 —— 控制权交给 stage2 */
	.byte	0xEA
	.word	0x7E00
	.word	0x0000

disk_err:
	movw	$msg_err, %si
	call	puts
1:	hlt
	jmp	1b

/* 输出 DS:SI 指向的 ASCIIZ；同时写 VGA(int 10h) 和 QEMU debugcon(0xE9) */
puts:
	lodsb
	testb	%al, %al
	jz	2f
	outb	%al, $0xE9		/* QEMU: -debugcon stdio 直接打到终端 */
	movb	$0x0E, %ah
	movw	$0x0007, %bx
	int	$0x10
	jmp	puts
2:	ret

msg1:	.asciz	"stage1: hello from MBR (0x7C00)\r\n"
msg_err:.asciz	"stage1: disk read failed\r\n"

.align 4
dap:					/* Disk Address Packet */
	.byte	0x10, 0x00		/* 结构大小 16 */
	.word	1			/* 读 1 个扇区 */
	.word	0x7E00			/* 目标 offset */
	.word	0x0000			/* 目标 segment */
	.long	1			/* LBA = 1 */
	.long	0

.org 510
.word 0xAA55				/* 教材说的"特定标识符" */
