#ifndef KERNEL_SYNC_MUTEX_H
#define KERNEL_SYNC_MUTEX_H

#include "sync/sem.h"

struct mutex {
    struct sem sem;
};

void mutex_init(struct mutex *m);
void mutex_lock(struct mutex *m);
void mutex_unlock(struct mutex *m);

#endif
