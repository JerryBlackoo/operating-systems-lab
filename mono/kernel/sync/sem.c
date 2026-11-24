#include "sync/sem.h"

#include "arch.h"
#include "lib/kprintf.h"

void sem_init(struct sem *s, int count)
{
    s->count = count;
    s->nwait = 0;
}

void sem_wait(struct sem *s)
{
    uint64_t flags = arch_intr_save();

    while (s->count == 0) {
        if (s->nwait >= MAX_PROCS) {
            panic("[sync] sem wait queue overflow");
        }
        s->waiters[s->nwait++] = current;
        current->state = PROC_SLEEPING;
        sched();
    }
    s->count--;
    arch_intr_restore(flags);
}

void sem_post(struct sem *s)
{
    uint64_t flags = arch_intr_save();

    s->count++;
    if (s->nwait > 0) {
        struct proc *p = s->waiters[0];
        for (int i = 1; i < s->nwait; i++) {
            s->waiters[i - 1] = s->waiters[i];
        }
        s->nwait--;
        if (p->state == PROC_SLEEPING) {
            p->state = PROC_READY;
        }
    }
    arch_intr_restore(flags);
}
