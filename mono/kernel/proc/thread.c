#include "proc/proc.h"

#include "arch.h"
#include "lib/kprintf.h"
#include "lib/string.h"

void *proc_kstack_top(struct proc *p);

void thread_trampoline(void)
{
    arch_irq_enable();
    if (current->kthread_fn != 0) {
        current->kthread_fn();
    }
    thread_exit();
}

struct proc *thread_create(const char *name, void (*fn)(void))
{
    struct proc *p = proc_alloc();

    if (p == 0) {
        kprintf("[proc] no free proc slot for %s\n", name);
        return 0;
    }
    strncpy(p->name, name, sizeof(p->name) - 1);
    p->kthread_fn = fn;
#ifdef PLATFORM_ZYNQ
    p->ctx.lr = (uint32_t)(uintptr_t)thread_trampoline;
#else
    p->ctx.ra = (uint64_t)(uintptr_t)thread_trampoline;
#endif
    p->ctx.sp = (uint64_t)(uintptr_t)proc_kstack_top(p);
    p->state = PROC_READY;
    kprintf("[proc] created pid=%d name=%s\n", p->pid, p->name);
    return p;
}

void thread_exit(void)
{
    arch_intr_disable();
    current->state = PROC_ZOMBIE;
    sched();
    kprintf("[proc] thread_exit returned unexpectedly\n");
    for (;;) {
    }
}

void idle_loop(void)
{
    for (;;) {
        /* sched() 内部会修改 current/队列并切栈，必须关中断进入。 */
        arch_intr_disable();
        if (current->state == PROC_RUNNING) {
            current->state = PROC_READY;
        }
        sched();
        arch_irq_enable();
        __asm__ volatile("wfi");
    }
}
