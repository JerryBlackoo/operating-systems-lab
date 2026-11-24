#include "sched.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>

static uint8_t stack_a[8192] __attribute__((aligned(16)));
static uint8_t stack_b[8192] __attribute__((aligned(16)));
static uint8_t stack_c[8192] __attribute__((aligned(16)));
static uint8_t stack_idle[8192] __attribute__((aligned(16)));

static int switch_calls;

/* 主机自测不真正切换栈，只记录调度决策。 */
void cpu_switch(struct cpu_context *old_ctx, struct cpu_context *new_ctx)
{
    (void)old_ctx;
    (void)new_ctx;
    switch_calls++;
}

void cpu_switch_to(struct cpu_context *new_ctx)
{
    (void)new_ctx;
    switch_calls++;
}

static void worker(void *arg)
{
    (void)arg;
}

static void idle_worker(void *arg)
{
    (void)arg;
}

static void test_round_robin(void)
{
    struct thread *a;
    struct thread *b;
    struct thread *c;
    struct thread *idle;

    object_system_init();
    sched_init();
    switch_calls = 0;

    a = thread_create(0, worker, (void *)1, stack_a, sizeof(stack_a), "a");
    b = thread_create(0, worker, (void *)2, stack_b, sizeof(stack_b), "b");
    c = thread_create(0, worker, (void *)3, stack_c, sizeof(stack_c), "c");
    idle = thread_create(0, idle_worker, 0, stack_idle, sizeof(stack_idle),
                         "idle");
    assert(a != 0 && b != 0 && c != 0 && idle != 0);
    sched_set_idle(idle);

    assert(sched_ready_count() == 3);

    sched();
    assert(sched_current() == a);
    assert(a->state == THREAD_RUNNING);
    assert(sched_ready_count() == 2);
    assert(switch_calls == 1);

    /* 4 个 tick 不够，第 5 个才触发重调度。 */
    for (int i = 0; i < SCHED_QUANTUM_TICKS - 1; i++) {
        sched_tick();
    }
    sched_check();
    assert(sched_current() == a);

    sched_tick();
    sched_check();
    assert(sched_current() == b);
    assert(a->state == THREAD_READY);
    assert(b->state == THREAD_RUNNING);

    thread_yield();
    assert(sched_current() == c);
    assert(b->state == THREAD_READY);
    assert(c->state == THREAD_RUNNING);

    printf("sched: round robin ok\n");
}

static void test_block_wake(void)
{
    struct thread *a;
    struct thread *b;
    struct thread *idle;
    struct waitq wq;

    object_system_init();
    sched_init();
    switch_calls = 0;

    a = thread_create(0, worker, 0, stack_a, sizeof(stack_a), "a");
    b = thread_create(0, worker, 0, stack_b, sizeof(stack_b), "b");
    idle = thread_create(0, idle_worker, 0, stack_idle, sizeof(stack_idle),
                         "idle");
    assert(a != 0 && b != 0 && idle != 0);
    sched_set_idle(idle);
    waitq_init(&wq);

    sched();
    assert(sched_current() == a);

    thread_block(&wq);
    assert(a->state == THREAD_BLOCKED);
    assert(a->waitq == &wq);
    assert(!waitq_empty(&wq));
    assert(sched_current() == b);

    thread_wake(a);
    assert(a->state == THREAD_READY);
    assert(a->waitq == 0);
    assert(waitq_empty(&wq));
    assert(sched_ready_count() == 1);

    /* 重复唤醒不能造成就绪队列重复项。 */
    thread_wake(a);
    assert(sched_ready_count() == 1);

    sched_bootstrap(a);
    thread_block(&wq);
    thread_wake(a);
    assert(a->state == THREAD_READY);

    printf("sched: block/wake ok\n");
}

static void test_idle_fallback_and_reap(void)
{
    struct thread *a;
    struct thread *idle;

    object_system_init();
    sched_init();
    switch_calls = 0;

    a = thread_create(0, worker, 0, stack_a, sizeof(stack_a), "a");
    idle = thread_create(0, idle_worker, 0, stack_idle, sizeof(stack_idle),
                         "idle");
    assert(a != 0 && idle != 0);
    sched_set_idle(idle);

    sched();
    assert(sched_current() == a);

    /* 把唯一就绪线程标记死亡，调度器应回退到 idle。 */
    a->state = THREAD_DEAD;
    sched();
    assert(sched_current() == idle);
    assert(idle->state == THREAD_RUNNING);

    /* idle 自己再次进入 sched 时不应在就绪队列里留下重复项。 */
    sched();
    assert(sched_current() == idle);
    assert(sched_ready_count() == 0);

    /* 切到另一个线程后，上一轮的死线程应被回收。 */
    sched_bootstrap(idle);
    sched();
    assert(!a->used);

    printf("sched: idle/reap ok\n");
}

static void test_idle_only(void)
{
    struct thread *idle;

    object_system_init();
    sched_init();
    switch_calls = 0;

    idle = thread_create(0, idle_worker, 0, stack_idle, sizeof(stack_idle),
                         "idle");
    assert(idle != 0);
    sched_set_idle(idle);
    assert(sched_ready_count() == 0);

    sched();
    assert(sched_current() == idle && idle->state == THREAD_RUNNING);
    assert(switch_calls == 1);

    /* idle 再次进入调度时既不入队，也不重复切换。 */
    sched();
    assert(sched_current() == idle && sched_ready_count() == 0);
    assert(switch_calls == 1);
    assert(sched_idle() == idle);

    printf("sched: idle-only fallback ok\n");
}

static void test_create_limits(void)
{
    struct thread *created[SCHED_MAX_THREADS];
    struct thread *extra;

    object_system_init();
    sched_init();

    assert(thread_create(0, 0, 0, stack_a, sizeof(stack_a), "no-entry") == 0);
    assert(thread_create(0, worker, 0, 0, 4096, "no-stack") == 0);
    assert(thread_create(0, worker, 0, stack_a, 16, "tiny") == 0);

    /*
     * 主机自测不会真正切栈，可以复用同一块栈验证槽位上限。
     */
    for (int i = 0; i < SCHED_MAX_THREADS; i++) {
        created[i] = thread_create(0, worker, 0, stack_a, sizeof(stack_a),
                                   "full");
        assert(created[i] != 0);
    }
    extra = thread_create(0, worker, 0, stack_a, sizeof(stack_a), "extra");
    assert(extra == 0);
    assert(sched_ready_count() == SCHED_MAX_THREADS);

    printf("sched: create validation/capacity ok\n");
}

static void test_waitq_order(void)
{
    struct waitq wq;
    struct thread *a;
    struct thread *b;
    struct thread *c;

    object_system_init();
    sched_init();
    waitq_init(&wq);

    a = thread_create(0, worker, 0, stack_a, sizeof(stack_a), "a");
    b = thread_create(0, worker, 0, stack_b, sizeof(stack_b), "b");
    c = thread_create(0, worker, 0, stack_c, sizeof(stack_c), "c");
    assert(a != 0 && b != 0 && c != 0);

    waitq_push(&wq, a);
    waitq_push(&wq, b);
    waitq_push(&wq, c);
    assert(waitq_pop(&wq) == a);
    assert(waitq_pop(&wq) == b);
    assert(waitq_pop(&wq) == c);
    assert(waitq_empty(&wq));

    waitq_push(&wq, a);
    waitq_push(&wq, b);
    waitq_push(&wq, c);
    waitq_remove(&wq, b);
    assert(waitq_pop(&wq) == a);
    assert(waitq_pop(&wq) == c);
    assert(waitq_empty(&wq));

    /* 重复入队不能制造环。 */
    waitq_push(&wq, a);
    waitq_push(&wq, a);
    assert(waitq_pop(&wq) == a);
    assert(waitq_empty(&wq));

    printf("sched: waitq FIFO/remove ok\n");
}

static void test_quantum_carry(void)
{
    struct thread *a;
    struct thread *b;

    object_system_init();
    sched_init();
    switch_calls = 0;

    a = thread_create(0, worker, 0, stack_a, sizeof(stack_a), "a");
    b = thread_create(0, worker, 0, stack_b, sizeof(stack_b), "b");
    assert(a != 0 && b != 0);

    sched();
    assert(sched_current() == a);

    /* 主动让出后，剩余时间片应保留到下次运行。 */
    for (int i = 0; i < 2; i++) {
        sched_tick();
    }
    assert(a->time_left == SCHED_QUANTUM_TICKS - 2);
    thread_yield();
    assert(sched_current() == b);

    thread_yield();
    assert(sched_current() == a);
    assert(a->time_left == SCHED_QUANTUM_TICKS - 2);

    for (int i = 0; i < 2; i++) {
        sched_tick();
    }
    sched_check();
    assert(sched_current() == a);
    assert(a->time_left == 1);

    sched_tick();
    sched_check();
    assert(sched_current() == b);
    assert(a->state == THREAD_READY);

    printf("sched: quantum carry ok\n");
}

int main(void)
{
    test_round_robin();
    test_block_wake();
    test_idle_fallback_and_reap();
    test_idle_only();
    test_create_limits();
    test_waitq_order();
    test_quantum_carry();
    printf("sched: all tests passed\n");
    return 0;
}
