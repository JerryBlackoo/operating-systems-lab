/* stage2.S —— 第二级引导，被 stage1 加载到 0x7E00
 * 演示两件事：
 *   1. 代码确实在 0x0000:0x7E00 执行（打印 CS:IP）
 *   2. 控制权从 stage1 完整交接
 *
 * 注意：BIOS 的 int 10h / int 13h 不保证保留寄存器。
 * 早期版本用 %al 当临时变量传递字符，结果把 %ax 高位覆盖，
 * 打印出假的 CS=0003。这里改用内存变量，避免被中断污染。
 */
.code16
.section .text
.globl _start

_start:
	cli

	movw	$msg2, %si
	call	puts

	movw	%cs, %ax
	call	put_hex4

	movb	$':', %al
	call	put_char

	movw	$here, %ax
	call	put_hex4

	movw	$msg_ds, %si
	call	puts
	movw	%ds, %ax
	call	put_hex4

	movw	$msg_nl, %si
	call	puts

1:	hlt
	jmp	1b

here:

puts:
	lodsb
	testb	%al, %al
	jz	9f
	call	put_char
	jmp	puts
9:	ret

/* al = 字符 */
put_char:
	pushw	%ax
	pushw	%bx
	outb	%al, $0xE9
	movb	$0x0E, %ah
	movw	$0x0007, %bx
	int	$0x10
	popw	%bx
	popw	%ax
	ret

/* ax = 16 位值，打印 4 位 hex。
 * 值存内存，因为 int 10h 会改 ax/bx/cx。 */
put_hex4:
	movw	%ax, hexval
	movb	$4, hexcnt
1:	rolw	$4, hexval
	movw	hexval, %ax
	andb	$0x0F, %al
	cmpb	$10, %al
	jb	2f
	addb	$('a' - 10 - '0'), %al
2:	addb	$'0', %al
	call	put_char
	decb	hexcnt
	jnz	1b
	ret

hexval:	.word	0
hexcnt:	.byte	0

msg2:	.asciz	"stage2: hello, CS:IP="
msg_ds:	.asciz	" DS="
msg_nl:	.asciz	"\r\nstage2: control transfer ok, halting\r\n"

.org 510
.word 0xAA55
