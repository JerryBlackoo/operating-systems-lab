#include "sched.h"

#include <stddef.h>
#include <stdint.h>

#include "memory.h"

static struct thread threads[SCHED_MAX_THREADS];

static struct thread *ready_head;
static struct thread *ready_tail;
static struct thread *current_thread;
static struct thread *idle_thread;

static bool need_resched;

static void thread_bootstrap(void);

static void memzero(void *dst, size_t len)
{
    uint8_t *p = (uint8_t *)dst;

    while (len-- > 0) {
        *p++ = 0;
    }
}

static void ready_enqueue(struct thread *t)
{
    if (t == 0 || t->in_ready) {
        return;
    }

    t->ready_next = 0;
    t->in_ready = true;
    if (ready_tail != 0) {
        ready_tail->ready_next = t;
    } else {
        ready_head = t;
    }
    ready_tail = t;
}

static struct thread *ready_dequeue(void)
{
    struct thread *t = ready_head;

    if (t == 0) {
        return 0;
    }
    ready_head = t->ready_next;
    if (ready_head == 0) {
        ready_tail = 0;
    }
    t->ready_next = 0;
    t->in_ready = false;
    return t;
}

static void ready_remove(struct thread *t)
{
    struct thread *prev = 0;
    struct thread *cur = ready_head;

    while (cur != 0) {
        if (cur == t) {
            if (prev != 0) {
                prev->ready_next = cur->ready_next;
            } else {
                ready_head = cur->ready_next;
            }
            if (ready_tail == cur) {
                ready_tail = prev;
            }
            cur->ready_next = 0;
            cur->in_ready = false;
            return;
        }
        prev = cur;
        cur = cur->ready_next;
    }
}

void waitq_init(struct waitq *wq)
{
    if (wq != 0) {
        wq->head = 0;
        wq->tail = 0;
    }
}

bool waitq_empty(const struct waitq *wq)
{
    return wq == 0 || wq->head == 0;
}

void waitq_push(struct waitq *wq, struct thread *t)
{
    if (wq == 0 || t == 0 || t->waitq != 0) {
        return;
    }

    t->waitq = wq;
    t->wait_next = 0;
    if (wq->tail != 0) {
        wq->tail->wait_next = t;
    } else {
        wq->head = t;
    }
    wq->tail = t;
}

struct thread *waitq_pop(struct waitq *wq)
{
    struct thread *t;

    if (wq == 0) {
        return 0;
    }
    t = wq->head;
    if (t == 0) {
        return 0;
    }

    wq->head = t->wait_next;
    if (wq->head == 0) {
        wq->tail = 0;
    }
    t->wait_next = 0;
    t->waitq = 0;
    return t;
}

void waitq_remove(struct waitq *wq, struct thread *t)
{
    struct thread *prev = 0;
    struct thread *cur;

    if (wq == 0 || t == 0) {
        return;
    }

    cur = wq->head;
    while (cur != 0) {
        if (cur == t) {
            if (prev != 0) {
                prev->wait_next = cur->wait_next;
            } else {
                wq->head = cur->wait_next;
            }
            if (wq->tail == cur) {
                wq->tail = prev;
            }
            cur->wait_next = 0;
            cur->waitq = 0;
            return;
        }
        prev = cur;
        cur = cur->wait_next;
    }
}

static void thread_destroy(struct object *obj)
{
    struct thread *t = (struct thread *)obj;

    t->used = false;
    t->state = THREAD_DEAD;
    t->as = 0;
    t->entry = 0;
    t->arg = 0;
    t->name = 0;
    t->waitq = 0;
    t->wait_next = 0;
    t->ready_next = 0;
    t->in_ready = false;
    t->reaped = true;
}

static void reap_dead(void)
{
    struct thread *dead[SCHED_MAX_THREADS];
    struct as *dead_as[SCHED_MAX_THREADS];
    int count = 0;
    uint64_t flags;

    flags = sched_lock();
    for (int i = 0; i < SCHED_MAX_THREADS; i++) {
        struct thread *t = &threads[i];

        if (t->used && t->state == THREAD_DEAD && !t->reaped &&
            t != current_thread && t != idle_thread) {
            dead[count] = t;
            dead_as[count] = t->as;
            t->as = 0;
            t->reaped = true;
            count++;
        }
    }
    sched_unlock(flags);

    /*
     * 切换完成后再释放 AS/对象：死线程的页表可能直到下一次 satp 切换前
     * 仍被使用，这里由调度器在安全点统一回收。
     */
    for (int i = 0; i < count; i++) {
        if (dead_as[i] != 0) {
            as_unref(dead_as[i]);
        }
        object_unref(&dead[i]->obj);
    }
}

void sched_init(void)
{
    memzero(threads, sizeof(threads));
    ready_head = 0;
    ready_tail = 0;
    current_thread = 0;
    idle_thread = 0;
    need_resched = false;

    /* 对象注册表与调度器共享同一把单核锁。 */
    object_set_lock(sched_lock, sched_unlock);
}

struct thread *thread_create(struct as *as, void (*entry)(void *), void *arg,
                             void *stack_base, size_t stack_size,
                             const char *name)
{
    struct thread *t = 0;
    uintptr_t stack_top;
    uint64_t flags;

    if (entry == 0 || stack_base == 0 || stack_size < 256) {
        return 0;
    }

    flags = sched_lock();
    for (int i = 0; i < SCHED_MAX_THREADS; i++) {
        if (!threads[i].used) {
            t = &threads[i];
            break;
        }
    }
    if (t == 0) {
        sched_unlock(flags);
        return 0;
    }

    memzero(t, sizeof(*t));
    t->used = true;
    t->state = THREAD_READY;
    t->as = as;
    t->time_left = SCHED_QUANTUM_TICKS;
    t->entry = entry;
    t->arg = arg;
    t->name = name;

    stack_top = (uintptr_t)stack_base + stack_size;
    stack_top &= ~(uintptr_t)15;
    t->ctx.ra = (uint64_t)(uintptr_t)thread_bootstrap;
    t->ctx.sp = (uint64_t)stack_top;

    if (as != 0) {
        as_ref(as);
    }

    if (object_register(&t->obj, OBJ_THREAD, thread_destroy) < 0) {
        if (as != 0) {
            as_unref(as);
        }
        t->used = false;
        sched_unlock(flags);
        return 0;
    }

    ready_enqueue(t);
    sched_unlock(flags);
    return t;
}

static void thread_bootstrap(void)
{
    struct thread *t = current_thread;

    if (t != 0 && t->entry != 0) {
        t->entry(t->arg);
    }
    thread_exit();
}

void sched(void)
{
    struct thread *prev;
    struct thread *next;
    uint64_t flags;

    reap_dead();

    flags = sched_lock();
    prev = current_thread;

    if (prev != 0 && prev->state == THREAD_RUNNING) {
        prev->state = THREAD_READY;
        ready_enqueue(prev);
    }

    next = ready_dequeue();
    if (next == 0) {
        next = idle_thread;
        if (next != 0) {
            /* 如果 current 就是 idle，它刚被重新入队，这里摘掉。 */
            ready_remove(next);
        }
    }
    if (next == 0) {
        sched_unlock(flags);
        return;
    }

    next->state = THREAD_RUNNING;
    current_thread = next;
    sched_unlock(flags);

    if (prev == next) {
        return;
    }
    if (next->as != 0) {
        /* 用户线程：切页表并刷 TLB；内核线程沿用当前内核映射。 */
        as_activate(next->as);
    }
    if (prev != 0) {
        cpu_switch(&prev->ctx, &next->ctx);
    } else {
        cpu_switch_to(&next->ctx);
    }
}

void thread_block_locked(struct waitq *wq)
{
    if (current_thread == 0 || wq == 0) {
        return;
    }
    if (current_thread->state != THREAD_RUNNING) {
        return;
    }

    current_thread->state = THREAD_BLOCKED;
    waitq_push(wq, current_thread);
}

void thread_wake_locked(struct thread *t)
{
    if (t == 0 || t->state != THREAD_BLOCKED) {
        return;
    }

    if (t->waitq != 0) {
        waitq_remove(t->waitq, t);
    }
    t->state = THREAD_READY;
    ready_enqueue(t);
}

void thread_block(struct waitq *wq)
{
    uint64_t flags = sched_lock();

    thread_block_locked(wq);
    sched_unlock(flags);
    sched();
}

void thread_wake(struct thread *t)
{
    uint64_t flags = sched_lock();

    thread_wake_locked(t);
    sched_unlock(flags);
}

void thread_yield(void)
{
    sched();
}

void thread_exit(void)
{
    uint64_t flags = sched_lock();

    if (current_thread != 0) {
        current_thread->state = THREAD_DEAD;
    }
    sched_unlock(flags);

    sched();

    /* 正常情况下 sched 不会再回到这里。 */
    for (;;) {
    }
}

void sched_tick(void)
{
    uint64_t flags = sched_lock();

    if (current_thread != 0 && current_thread->state == THREAD_RUNNING) {
        if (current_thread->time_left > 0) {
            current_thread->time_left--;
        }
        if (current_thread->time_left == 0) {
            current_thread->time_left = SCHED_QUANTUM_TICKS;
            need_resched = true;
        }
    }
    sched_unlock(flags);
}

void sched_check(void)
{
    bool resched;
    uint64_t flags = sched_lock();

    resched = need_resched;
    need_resched = false;
    sched_unlock(flags);

    if (resched) {
        sched();
    }
}

void sched_bootstrap(struct thread *t)
{
    uint64_t flags;

    if (t == 0) {
        return;
    }

    flags = sched_lock();
    if (current_thread != 0 && current_thread != t &&
        current_thread->state == THREAD_RUNNING) {
        current_thread->state = THREAD_READY;
        ready_enqueue(current_thread);
    }
    ready_remove(t);
    t->state = THREAD_RUNNING;
    current_thread = t;
    sched_unlock(flags);

    if (t->as != 0) {
        as_activate(t->as);
    }
}

void sched_set_idle(struct thread *t)
{
    uint64_t flags = sched_lock();

    idle_thread = t;
    if (t != 0) {
        ready_remove(t);
        if (t->state != THREAD_RUNNING) {
            t->state = THREAD_READY;
        }
    }
    sched_unlock(flags);
}

struct thread *sched_current(void)
{
    return current_thread;
}

struct thread *sched_idle(void)
{
    return idle_thread;
}

int sched_ready_count(void)
{
    int count = 0;
    struct thread *t = ready_head;

    while (t != 0) {
        count++;
        t = t->ready_next;
    }
    return count;
}

uint64_t sched_lock(void)
{
#if defined(__riscv) && !defined(SCHED_HOST_TEST)
    uint64_t old;

    /*
     * 清 sstatus.SIE 并返回旧值；restore 时只恢复 SIE 位。
     * 单核模型足够；SMP 时这里要换成 per-CPU 自旋锁。
     */
    __asm__ volatile("csrrci %0, sstatus, 2" : "=r"(old) :: "memory");
    return old;
#else
    return 0;
#endif
}

void sched_unlock(uint64_t flags)
{
#if defined(__riscv) && !defined(SCHED_HOST_TEST)
    if ((flags & 2) != 0) {
        __asm__ volatile("csrsi sstatus, 2" ::: "memory");
    }
#else
    (void)flags;
#endif
}
