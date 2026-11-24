#ifndef KERNEL_PROC_USER_H
#define KERNEL_PROC_USER_H

#include <stdint.h>

#define USER_BASE 0x40000000UL
#define USER_HEAP_BASE 0x40080000UL
#ifdef PLATFORM_ZYNQ
/* ARM 用 1 MiB 段映射栈区，栈顶取段尾；堆位于代码段内，不能越过栈段起点。 */
#define USER_STACK_TOP 0x40200000UL
#define USER_HEAP_LIMIT (USER_STACK_TOP - 0x100000UL)
#else
/* RISC-V 只映射栈顶一页，堆不能占用该页。 */
#define USER_STACK_TOP 0x40100000UL
#define USER_HEAP_LIMIT (USER_STACK_TOP - 0x1000UL)
#endif
#define TRAP_FRAME_SIZE 288UL

void user_hello_init(void);
void user_fork_init(void);
void user_fault_init(void);
void user_init_init(void);
void forkret(void);
struct proc *proc_fork(void);
void uvm_destroy(uint64_t pagetable, uint64_t heap_top);

/* 按名字加载内嵌用户程序；成功返回 0，失败返回负的 OS_* 错误码。 */
int user_exec_current(const char *name);

#endif
