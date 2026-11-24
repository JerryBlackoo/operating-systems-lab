#ifndef KERNEL_SYNC_SEM_H
#define KERNEL_SYNC_SEM_H

#include "proc/proc.h"

struct sem {
    int count;
    int nwait;
    struct proc *waiters[MAX_PROCS];
};

void sem_init(struct sem *s, int count);
void sem_wait(struct sem *s);
void sem_post(struct sem *s);

#endif
