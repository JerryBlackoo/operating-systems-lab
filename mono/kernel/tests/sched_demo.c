#include "tests/sched_demo.h"

#include "lib/kprintf.h"
#include "proc/proc.h"

static void kworker(void)
{
    for (int i = 0; i < 12; i++) {
        kprintf("[sched] pid=%d tick=%d\n", current->pid, i);
        for (volatile int spin = 0; spin < 20000000; spin++) {
        }
        sched_check();
    }
    kprintf("[sched] pid=%d done\n", current->pid);
    thread_exit();
}

void sched_demo_start(void)
{
    thread_create("worker-a", kworker);
    thread_create("worker-b", kworker);
    kprintf("[sched] demo ready: two kernel threads, slice=5 ticks\n");
    proc_dump();
}
