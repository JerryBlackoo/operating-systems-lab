.code32
.section .text

.global hw_isr_irq0
.global hw_isr_pgfault
.global hw_isr_spurious

/*
 * 中断/异常入口约定
 * ------------------
 * CPU 在进入时已经压好:  [eflags][cs][eip]            （同特权级）
 *                        [eflags][cs][eip][esp][ss]   （跨特权级）
 * 有错误码的异常（#PF=14, #GP=13, #DF=8 ...）再多压一个 error code。
 *
 * 约定：所有入口都不修改被中断的通用寄存器语义，C 侧看到的顺序是
 *   hw_xxx_handler(error_code, vector)
 * 因此先把 error code 从正确偏移取出来，再按 cdecl 压栈。
 */

/* ---------- IRQ0：8254 定时器 ---------- */
hw_isr_irq0:
    pushal
    call hw_pit_on_irq0
    /* 通知 8259 主片：EOI */
    movb $0x20, %al
    outb %al, $0x20
    popal
    iret

/* ---------- #PF（向量 0x0e）：CPU 已压 error code ---------- */
hw_isr_pgfault:
    /* 保存会被 C 调用破坏的寄存器（不需要保存段寄存器：本内核全程用内核段） */
    pushl %eax
    pushl %ecx
    pushl %edx

    /* 此时栈：[edx][ecx][eax][error_code][eip][cs][eflags]  → error code 在 12(%esp) */
    pushl 12(%esp)          /* 第二参数: error_code */
    movl  %cr2, %eax
    pushl %eax              /* 第一参数: cr2（出错线性地址） */
    call  hw_paging_on_fault
    addl  $8, %esp

    popl %edx
    popl %ecx
    popl %eax
    addl $4, %esp           /* 丢弃 error code */
    iret

/* ---------- 兜底：任何未预期异常都打印向量号后停机，而不是三重故障复位 ---------- */
hw_isr_spurious:
    pushal
    movl 32(%esp), %eax     /* 压栈布局: 8 个通用寄存器后是中断号 */
    pushl %eax
    call hw_unexpected_vector
    addl $4, %esp
    popal
    iret

.section .note.GNU-stack,"",@progbits
