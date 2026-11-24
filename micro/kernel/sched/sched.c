#include "sched.h"

#include <stddef.h>
#include <stdint.h>

#include "memory.h"

#ifndef SCHED_HOST_TEST
#include "clock/clock.h"
#include "csr.h"
#endif

/*
 * 新线程第一次被调度时，sched() 在关中断状态下把控制权交给
 * thread_bootstrap；由它恢复中断再进入线程入口。这样定时器可以
 * 从第一条 C 语句开始抢占内核线程。
 */
#define SCHED_SIE_BIT (1ULL << 1)

static struct thread threads[SCHED_MAX_THREADS];

static struct thread *ready_head;
static struct thread *ready_tail;
static struct thread *current_thread;
static struct thread *idle_thread;

/* 当前 satp 指向的用户地址空间；NULL 表示仍是内核初始映射。 */
static struct as *active_as;

static bool need_resched;

/* 观测计数：只增不减，供真实 QEMU 验收核对。 */
static uint64_t tick_count;
static uint64_t switch_count;
static uint64_t quantum_expirations;

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

/*
 * 死线程的回收点。调用者可能处于关中断状态，因此这里只做
 * “摘除 + 记录”，真正的引用释放放到锁外。
 *
 * 页表安全：如果死线程持有的正是 satp 当前指向的地址空间，
 * 必须先退回内核 bare 映射，否则 as_unref() 释放页表后，
 * 下一次 TLB miss 会走进已经归还的物理页。
 */
static void reap_dead(void)
{
    struct thread *dead[SCHED_MAX_THREADS];
    struct as *dead_as[SCHED_MAX_THREADS];
    int count = 0;
    uint64_t flags;

    flags = sched_lock();
    for (int i = 0; i < SCHED_MAX_THREADS; i++) {
        struct thread *t = &threads[i];

        if (!t->used || t->state != THREAD_DEAD || t->reaped ||
            t == current_thread || t == idle_thread) {
            continue;
        }

        if (t->as != 0 && t->as == active_as &&
            (current_thread == 0 || current_thread->as != active_as)) {
            /* 当前执行流不再依赖该用户地址空间，可以安全切回 bare。 */
            as_activate(0);
            active_as = 0;
        }

        dead[count] = t;
        dead_as[count] = t->as;
        t->as = 0;
        t->reaped = true;
        count++;
    }
    sched_unlock(flags);

    /*
     * 切换完成后再释放 AS/对象：死线程的页表可能直到下一次 satp
     * 切换前仍被使用，这里由调度器在安全点统一回收。
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
    active_as = 0;
    need_resched = false;
    tick_count = 0;
    switch_count = 0;
    quantum_expirations = 0;

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
    if ((uintptr_t)stack_base + stack_size < (uintptr_t)stack_base) {
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

    /*
     * 第一次被调度时中断仍是关的（sched() 在切换期间不恢复 SIE）。
     * 内核线程从这里开始允许定时器抢占。
     */
    sched_unlock(SCHED_SIE_BIT);

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

    /*
     * 从挑选 next 到 cpu_switch 完成，全程保持关中断。解锁留给
     * 恢复运行的线程：它从自己的 cpu_switch 返回后，用自己栈上的
     * flags 恢复 SIE。这样不会出现“已改 current 但还没换栈”的窗口。
     */
    flags = sched_lock();
    prev = current_thread;

    if (prev != 0 && prev->state == THREAD_RUNNING) {
        prev->state = THREAD_READY;
        if (prev != idle_thread) {
            /*
             * idle 是兜底线程，不参与 FIFO：只要还有普通线程可跑，
             * 就不应把 idle 插到队尾。
             */
            ready_enqueue(prev);
        }
    }

    next = ready_dequeue();
    if (next == 0) {
        next = idle_thread;
    }
    if (next == 0) {
        /*
         * 没有就绪线程也没有 idle：正式内核必须创建 idle。
         * 教学模型里保留当前线程继续运行，避免在无任务可切时
         * 把控制流丢进空指针。
         */
        if (prev != 0 && prev->state == THREAD_READY) {
            prev->state = THREAD_RUNNING;
        }
        sched_unlock(flags);
        return;
    }

    next->state = THREAD_RUNNING;
    current_thread = next;

    if (next->as != 0) {
        /* 用户线程：切页表并刷 TLB。 */
        as_activate(next->as);
        active_as = next->as;
    } else if (active_as != 0) {
        /*
         * 内核线程没有地址空间。若从用户线程切回来，必须退回 bare，
         * 否则 satp 仍指向死掉/无关的用户页表，后续内核线程会在错误
         * 的地址空间里运行。bare 下内核镜像与直接映射窗口按物理地址
         * 访问，仍然有效。
         */
        as_activate(0);
        active_as = 0;
    }

    if (prev == next) {
        sched_unlock(flags);
        return;
    }

    switch_count++;
    if (prev != 0) {
        cpu_switch(&prev->ctx, &next->ctx);
    } else {
        cpu_switch_to(&next->ctx);
    }

    /* 只有“本线程重新被调度”的路径会回到这里。 */
    sched_unlock(flags);
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

    /*
     * 唤醒意味着可能有更值得运行的就绪线程。trap 返回前调用
     * sched_check() 就能把“被唤醒”变成一次实际的上下文切换，
     * 不需要额外的周期 tick。
     */
    need_resched = true;
}

/*
 * 阻塞与调度必须在同一个关中断区间内完成：如果先释放锁、
 * 再调用 sched()，定时器可能在两步之间插入一次调度，导致
 * 线程在“已阻塞但还没让出 CPU”的状态下被恢复。
 */
void thread_block(struct waitq *wq)
{
    uint64_t flags = sched_lock();

    thread_block_locked(wq);
    sched();
    sched_unlock(flags);
}

void thread_wake(struct thread *t)
{
    uint64_t flags = sched_lock();

    thread_wake_locked(t);
    sched_unlock(flags);
}

void thread_yield(void)
{
    uint64_t flags = sched_lock();

    sched();
    sched_unlock(flags);
}

void thread_exit(void)
{
    uint64_t flags = sched_lock();

    if (current_thread != 0) {
        current_thread->state = THREAD_DEAD;
    }
    sched();
    sched_unlock(flags);

    /* 没有 idle 兜底时，至少不要回到已经死亡的线程入口。 */
    for (;;) {
#if defined(__riscv) && !defined(SCHED_HOST_TEST)
        __asm__ volatile("wfi");
#endif
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
            quantum_expirations++;
        }
    }
    sched_unlock(flags);
}

uint64_t sched_tick_count(void)
{
    return tick_count;
}

uint64_t sched_switch_count(void)
{
    return switch_count;
}

uint64_t sched_quantum_expiration_count(void)
{
    return quantum_expirations;
}

#ifndef SCHED_HOST_TEST
/*
 * 周期性 tick 是调度策略的一部分，但触发机制仍由 clock 提供：
 * 这里只持有 timer 对象，回调在 S 态定时器中断上下文执行。
 *
 * 与 demo 局部定时器的区别：tick 源由 sched 自己武装和撤销，
 * 不再需要在每个使用者的 demo 里复制一份回调。
 *
 * 注意：sched_start_tick() 只负责武装 timer 和置 STIE；要真正形成抢占，
 * 还需要 trap 路径在 S 态 timer 中断里调用 clock_handle_interrupt()，
 * 并在返回前调用 sched_check()。正式镜像的这段 glue 在 shared/demo.c 的
 * trap_handler 默认分支；模块 trap 可先接管（clock/interrupt 专用 demo），
 * 未接管时由统一路径执行。arch/riscv/trap.c 只属于 ARCH_SELFTEST 镜像。
 */
static struct clock_timer tick_timer;
static uint64_t tick_period;
static bool tick_running;
static bool tick_stie_was_set;

static void sched_tick_callback(struct clock_timer *timer)
{
    tick_count++;
    sched_tick();

    /* 锚定旧 deadline，避免 UART 打印等延迟累积成漂移。 */
    clock_timer_arm(timer, timer->deadline + tick_period);
}

bool sched_start_tick(uint64_t period_ns)
{
    uint64_t flags;

    if (period_ns == 0) {
        return false;
    }

    flags = sched_lock();
    if (tick_running) {
        sched_unlock(flags);
        return false;
    }

    tick_period = period_ns;
    tick_count = 0;
    quantum_expirations = 0;
    clock_timer_init(&tick_timer, sched_tick_callback, 0);
    if (!clock_timer_arm(&tick_timer, clock_now() + period_ns)) {
        sched_unlock(flags);
        return false;
    }

    /*
     * timer 已武装后再解闸 STIE。sstatus.SIE 不在这里打开：
     * 调用者通常还要完成自己的布置，最后再统一开中断。
     * 记录原来的 STIE，停止时不要误关别的定时器使用者。
     */
    tick_stie_was_set = (csr_read(CSR_SIE) & SIE_STIE) != 0;
    csr_set(CSR_SIE, SIE_STIE);
    tick_running = true;
    sched_unlock(flags);
    return true;
}

void sched_stop_tick(void)
{
    uint64_t flags = sched_lock();

    if (tick_running) {
        clock_timer_cancel(&tick_timer);
        if (!tick_stie_was_set)
            csr_clear(CSR_SIE, SIE_STIE);
        tick_running = false;
    }
    sched_unlock(flags);
}
#endif /* SCHED_HOST_TEST */

void sched_check(void)
{
    bool resched;
    uint64_t flags = sched_lock();

    resched = need_resched;
    need_resched = false;
    if (resched) {
        sched();
    }
    sched_unlock(flags);
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
    if (t->as != 0) {
        as_activate(t->as);
        active_as = t->as;
    }
    sched_unlock(flags);
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
    struct thread *t;
    uint64_t flags = sched_lock();

    t = ready_head;
    while (t != 0) {
        count++;
        t = t->ready_next;
    }
    sched_unlock(flags);
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

#ifndef SCHED_HOST_TEST
/*
 * 睡眠等待者完全在调用者栈上：线程被唤醒前不会返回，timer 回调也只
 * 在阻塞期间访问它。单 hart 上回调与调用者不会并发。
 */
struct sleep_waiter {
    struct thread *thread;
    struct waitq queue;
    struct clock_timer timer;
};

static void sleep_timeout(struct clock_timer *timer)
{
    struct sleep_waiter *waiter = timer->arg;
    uint64_t flags = sched_lock();

    thread_wake_locked(waiter->thread);
    sched_unlock(flags);
}

void thread_sleep_until(uint64_t deadline)
{
    struct thread *self = current_thread;
    struct sleep_waiter waiter;
    uint64_t flags;

    if (self == 0)
        return;
    if (clock_now() >= (clock_ns_t)deadline)
        return;

    waiter.thread = self;
    waitq_init(&waiter.queue);
    clock_timer_init(&waiter.timer, sleep_timeout, &waiter);

    flags = sched_lock();
    clock_timer_arm(&waiter.timer, deadline);
    thread_block_locked(&waiter.queue);

    for (;;) {
        if (self->state != THREAD_BLOCKED) {
            sched_unlock(flags);
            break;
        }
        if (clock_now() >= (clock_ns_t)deadline) {
            /* 没有可运行线程时 timer 回调可能还没到，自己唤醒。 */
            if (self->waitq == &waiter.queue)
                waitq_remove(&waiter.queue, self);
            self->state = THREAD_RUNNING;
            sched_unlock(flags);
            break;
        }

        /*
         * 在关中断区间内进入 sched：timer 不可能在“已阻塞但还没让出”
         * 的窗口里把线程唤醒后又立刻被本线程重新调度。
         */
        sched();

        /*
         * sched() 只有在本线程重新被调度时才会回到这里；若它因为
         * 没有 next/idle 而直接返回，状态仍是 BLOCKED，需要放开锁
         * 让 deadline 中断能够到达。
         */
        if (self->state == THREAD_BLOCKED) {
            sched_unlock(flags);
            flags = sched_lock();
        }
    }

    clock_timer_cancel(&waiter.timer);
}

void thread_sleep_ns(uint64_t ns)
{
    thread_sleep_until(clock_deadline_after(clock_now(), ns));
}
#endif
