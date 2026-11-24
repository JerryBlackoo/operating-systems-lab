#include "tests/prodcons.h"

#include "lib/kprintf.h"
#include "proc/proc.h"
#include "sync/mutex.h"
#include "sync/sem.h"

#define BUF_N 8
#define ITEMS 1000

static int buffer[BUF_N];
static int in_index;
static int out_index;
static struct sem empty;
static struct sem full;
static struct mutex lock;
static volatile int consumed;

static void producer(void)
{
    for (int i = 0; i < ITEMS; i++) {
        sem_wait(&empty);
        mutex_lock(&lock);
        buffer[in_index] = i;
        in_index = (in_index + 1) % BUF_N;
        mutex_unlock(&lock);
        sem_post(&full);
        sched_check();
    }
    kprintf("[sync] producer done\n");
    thread_exit();
}

static void consumer(void)
{
    int last = -1;

    for (int i = 0; i < ITEMS; i++) {
        sem_wait(&full);
        mutex_lock(&lock);
        last = buffer[out_index];
        out_index = (out_index + 1) % BUF_N;
        mutex_unlock(&lock);
        sem_post(&empty);
        consumed++;
        sched_check();
        if (last != i) {
            kprintf("[sync] order error got=%d want=%d\n", last, i);
        }
    }
    kprintf("[sync] prodcons ok count=%d\n", consumed);
    thread_exit();
}

void prodcons_selftest(void)
{
    sem_init(&empty, BUF_N);
    sem_init(&full, 0);
    mutex_init(&lock);
    in_index = 0;
    out_index = 0;
    consumed = 0;

    thread_create("producer", producer);
    thread_create("consumer", consumer);
    kprintf("[sync] prodcons started: N=%d items=%d\n", BUF_N, ITEMS);
}
