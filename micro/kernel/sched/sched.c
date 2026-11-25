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

/*
 * loader 的 launch 槽回收钩子（弱符号）。
 *
 * launch 里含该线程的内核栈，必须等线程真正销毁才释放；但 sched 不能
 * 反过来依赖 loader。loader 在 loader_spawn_user 成功后设置这个钩子；
 * 主机自测没有 loader，符号为 NULL 时直接跳过。
 */
void loader_release_launch_for_thread(struct thread *t) __attribute__((weak));
static void (*const loader_release_launch)(struct thread *) =
    loader_release_launch_for_thread;

/*
 * IPC 的线程销毁钩子（弱符号）：把该线程从 endpoint 的 sendq/recvq 和
 * reply 的所有者记录里清掉。阻塞在 sendq 上的线程把消息体存在自己 TCB
 * 里，接收者会直接读它；线程销毁后必须先撤销这些引用。
 */
void ipc_release_thread(struct thread *t) __attribute__((weak));
static void (*const ipc_forget_thread)(struct thread *) = ipc_release_thread;

/* 观测计数：只增不减，供真实 QEMU 验收核对。 */
static uint64_t tick_count;
static uint64_t switch_count;
static uint64_t quantum_expirations;

static void thread_bootstrap(void);

static void memzero(void *dst, size_t len)
{
    uint8_t *p = (uint8_t *)dst;
    size_t i = 0;

    for (; i + sizeof(uint64_t) <= len; i += sizeof(uint64_t)) {
        *(uint64_t *)(p + i) = 0;
    }
    for (; i < len; i++) {
        p[i] = 0;
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

    /*
     * 线程真正销毁时才归还 loader 的 launch 槽（含它的内核栈）。
     * 不能在请求终止的那一刻归还：RUNNING 线程还在用自己的 kstack，
     * 提前释放会让下一次 spawn 复用同一块内存并踩坏它。
     */
    if (loader_release_launch != 0) {
        loader_release_launch(t);
    }
    /*
     * 线程已不在任何队列里（discard/terminate 摘除时做过 waitq_remove），
     * 但阻塞在 ep_sendq 上的线程还把自己的 ipc_message 存在 TCB 里：
     * 接收者会从这个 TCB 读消息体。因此必须先清掉载荷并让 ipc 侧知道
     * 该线程已死，否则接收者可能读到已回收线程的垃圾。
     * 见 ipc_forget_thread()。
     */
    if (ipc_forget_thread != 0) {
        ipc_forget_thread(t);
    }
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
    t->cap_sid = (uint8_t)255;
    waitq_init(&t->syscall_wq);

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

    if (prev != 0 && prev->state == THREAD_RUNNING && prev != idle_thread) {
        /*
         * idle 是兜底线程：不参与 FIFO，也不要在让出时降成 READY，
         * 否则观测上会像「可抢占的就绪线程」而实际只在无他事时运行。
         */
        prev->state = THREAD_READY;
        ready_enqueue(prev);
    }

    next = ready_dequeue();
    if (next == 0) {
        next = idle_thread;
    } else if (next == prev && prev != idle_thread && idle_thread != 0) {
        /*
         * 只有让出者自己可运行：把它放回就绪队列，再把 CPU 交给 idle；
         * timer 或其它线程唤醒后它仍能被重新调度。
         */
        ready_enqueue(next);
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
        /*
         * 主动退出等同于接受了终止请求；清掉标记，避免 stale 状态影响
         * 该槽位后续复用（thread_create 会 memzero，这里只是显式收尾）。
         */
        current_thread->kill_pending = false;
        current_thread->async_parked = false;
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

int sched_thread_discard(struct thread *t)
{
    struct as *as;
    uint64_t flags;

    if (t == 0) {
        return -1;
    }
    flags = sched_lock();
    if (!t->used || t == current_thread || t == idle_thread) {
        sched_unlock(flags);
        return -1;
    }
    if (t->state == THREAD_RUNNING) {
        sched_unlock(flags);
        return -1;
    }
    /*
     * 阻塞在异步 syscall 上的线程不能摘：它的 trap 帧仍被在途 reply
     * 引用，ep_reply 之后会 syscall_resume 写回该帧。这类线程必须由
     * reply/endpoint 销毁路径先续完或取消，再走正常 reap。
     * sched_thread_terminate() 会先调 user_syscall_abandon_async() 清掉
     * async_parked，因此强制终止路径不受此限制。
     */
    if (t->async_parked) {
        sched_unlock(flags);
        return -1;
    }
    ready_remove(t);
    if (t->waitq != 0) {
        waitq_remove(t->waitq, t);
    }
    as = t->as;
    t->as = 0;
    t->state = THREAD_DEAD;
    t->reaped = true;
    sched_unlock(flags);

    if (as != 0 && as == active_as &&
        (current_thread == 0 || current_thread->as != active_as)) {
        as_activate(0);
        active_as = 0;
    }
    if (as != 0) {
        as_unref(as);
    }
    object_unref(&t->obj);
    return 0;
}

bool sched_thread_kill_pending(const struct thread *t)
{
    return t != 0 && t->kill_pending;
}

/*
 * 由 syscall 后端提供的弱钩子：放弃该线程在途的异步 reply。
 * 不实现时（主机自测）忽略，此时只能靠 discard 的常规拒绝。
 */
void user_syscall_abandon_async(struct thread *t) __attribute__((weak));

int sched_thread_terminate(struct thread *t)
{
    uint64_t flags;

    if (t == 0) {
        return -1;
    }
    flags = sched_lock();
    if (!t->used || t == current_thread || t == idle_thread) {
        sched_unlock(flags);
        return -1;
    }
    if (t->state == THREAD_READY) {
        sched_unlock(flags);
        return sched_thread_discard(t);
    }
    if (t->state == THREAD_BLOCKED) {
        bool on_syscall = t->async_parked;

        /*
         * 跑起来的子进程可能正阻塞在 ep_send/ep_recv/pager 上：直接摘除。
         * 阻塞在异步 syscall 上时，trap 帧仍被在途 reply 引用；必须先撤销
         * 那次 reply（否则稍后的 ep_reply 会写进已释放的栈），再摘除。
         */
        if (on_syscall && user_syscall_abandon_async != 0) {
            sched_unlock(flags);
            user_syscall_abandon_async(t);
            return sched_thread_discard(t);
        }
        sched_unlock(flags);
        return sched_thread_discard(t);
    }
    if (t->state == THREAD_RUNNING) {
        /*
         * 运行中的线程不能当场释放（它可能正用着自己的栈和 AS）。置位后
         * 由 resident_trap 在下次进入内核时兑现退出。本模型里所有 U-mode
         * 代码都是「请求—应答」循环，必然会再次 ecall。
         */
        t->kill_pending = true;
        sched_unlock(flags);
        return 0;
    }
    sched_unlock(flags);
    return -1;
}

void sched_thread_clear_kill(struct thread *t)
{
    uint64_t flags = sched_lock();

    if (t != 0) {
        t->kill_pending = false;
    }
    sched_unlock(flags);
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

/*
 * 按 object id 在 TCB 池里查线程（不增加对象引用）。
 *
 * 供内核侧「手上有 tid 但不在 syscall 返回路径」的模块使用，例如 pager
 * 的超时看门狗需要在定时器中断里定位缺页线程。调用者必须持 sched_lock，
 * 且只在同一个临界区内使用返回指针。
 */
struct thread *sched_thread_by_id_locked(uint32_t id)
{
    for (int i = 0; i < SCHED_MAX_THREADS; i++) {
        struct thread *t = &threads[i];

        if (t->used && object_get_id(&t->obj) == id) {
            return t;
        }
    }
    return 0;
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
    if (!clock_timer_arm(&waiter.timer, deadline)) {
        /*
         * 定时器池满时不能阻塞：没有 deadline 中断，线程可能永远
         * 睡在 idle 之外。直接返回，让调用者继续跑。
         */
        sched_unlock(flags);
        return;
    }
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

void sched_thread_set_cap_sid(struct thread *t, uint8_t sid)
{
    if (t != NULL) {
        t->cap_sid = sid;
    }
}

uint8_t sched_thread_cap_sid(const struct thread *t)
{
    if (t == NULL) {
        return (uint8_t)255;
    }
    return t->cap_sid;
}
