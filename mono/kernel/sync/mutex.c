#include "sync/mutex.h"

void mutex_init(struct mutex *m)
{
    sem_init(&m->sem, 1);
}

void mutex_lock(struct mutex *m)
{
    sem_wait(&m->sem);
}

void mutex_unlock(struct mutex *m)
{
    sem_post(&m->sem);
}
