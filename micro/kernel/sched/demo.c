#include "demo.h"
#include "sched.h"

static _Alignas(16) uint8_t waiting_stack[4096], waking_stack[4096];

struct schedule_case {
    struct thread *waiter;
    struct waitq queue;
    unsigned step;
};

static void wait(void *argument)
{
    struct schedule_case *test = argument;
    CHECK(test->step == 0 && sched_current() == test->waiter);
    test->step = 1;
    thread_block(&test->queue);
    CHECK(test->step == 2 && waitq_empty(&test->queue));
    test->step = 3;
}

static void wake(void *argument)
{
    struct schedule_case *test = argument;
    CHECK(test->step == 1 && test->waiter->state == THREAD_BLOCKED);
    test->step = 2;
    thread_wake(test->waiter);
}

static void run(void)
{
    struct schedule_case test = {0};
    struct thread runner = {.name = "bootstrap"};
    object_system_init();
    sched_init();
    sched_bootstrap(&runner);
    waitq_init(&test.queue);
    test.waiter = thread_create(NULL, wait, &test, waiting_stack, sizeof(waiting_stack), "waiter");
    CHECK(test.waiter);
    CHECK(thread_create(NULL, wake, &test, waking_stack, sizeof(waking_stack), "waker"));

    thread_yield();
    CHECK(test.step == 2 && sched_ready_count() == 1);
    thread_yield();
    CHECK(test.step == 3 && sched_current() == &runner && sched_ready_count() == 0);
    thread_yield();
    sched_init();
    demo_log("sched: RV64 context switch, FIFO ready queue and block/wake");
}

DEMO_MODULE(sched, false, run, NULL);
