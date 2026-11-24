.code32
.section .text

.global switch_context
.type switch_context, @function
switch_context:
    # i386 C ABI。call 已经把恢复执行所需的 EIP 压到旧栈上。
    pushl %ebp
    pushl %ebx
    pushl %esi
    pushl %edi
    movl 20(%esp), %eax           # &old_sp
    movl 24(%esp), %edx           # new_sp
    movl %esp, (%eax)
    movl %edx, %esp
    popl %edi
    popl %esi
    popl %ebx
    popl %ebp
    ret
.size switch_context, . - switch_context

.global thread_entry
.type thread_entry, @function
thread_entry:
    # 首次 ret 后 ESP 在 16 字节对齐的栈顶，可以直接 call C。
    call run_current_thread
    ud2
.size thread_entry, . - thread_entry

.section .note.GNU-stack,"",@progbits
