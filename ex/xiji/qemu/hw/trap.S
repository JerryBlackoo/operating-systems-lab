.code32
.section .text

.global hw_isr_irq0
.global hw_isr_pgfault

/* IRQ0 @ vector 0x20 */
hw_isr_irq0:
    pusha
    call hw_pit_on_irq0
    movb $0x20, %al
    outb %al, $0x20
    popa
    iret

/* #PF @ vector 0x0e — CPU 已压入 error code */
hw_isr_pgfault:
    pushl %eax
    pushl %ecx
    pushl %edx
    movl 16(%esp), %eax
    pushl %eax
    movl %cr2, %eax
    pushl %eax
    call hw_paging_on_fault
    addl $8, %esp
    popl %edx
    popl %ecx
    popl %eax
    addl $4, %esp
    iret

.section .note.GNU-stack,"",@progbits