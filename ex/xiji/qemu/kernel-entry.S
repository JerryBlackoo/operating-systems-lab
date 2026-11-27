.code32
.section .text.entry,"ax"
.global _start

/*
 * 内核入口：由 stage2 在保护模式下以 32 位远跳进入（CS=0x08）。
 * 这里已经是平坦 32 位环境，负责最后的 C 运行环境准备：
 *   - 装载内核数据段选择子
 *   - 建立引导栈（位于页表区之上、1 MiB 之下）
 *   - 清零 .bss
 *   - 调用 kmain
 */
_start:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %fs
    movw %ax, %gs
    movw %ax, %ss
    movl $0x9f000, %esp
    cld

    movl $__bss_start, %edi
    movl $__bss_end, %ecx
    subl %edi, %ecx
    xorl %eax, %eax
    rep stosb

    call kmain
1:  cli
    hlt
    jmp 1b

.section .note.GNU-stack,"",@progbits
