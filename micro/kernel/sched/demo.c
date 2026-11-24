#include "demo.h"
#include "sched.h"

#include "clock/clock.h"
#include "ipc/ipc.h"

#include "csr.h"
#include "trap.h"

/*
 * 这个 demo 把时间层接进真实内核路径：
 *   clock 定时器中断 → clock_handle_interrupt() → sched_tick()
 *   trap 返回前 → sched_check() → sched() → cpu_switch()
 *
 * 工作负载由 4 条普通线程和 1 条 idle 组成：
 *   - worker-a / worker-b：纯计算，不主动让出，验证时间片抢占；
 *   - client / server：一次同步 IPC，验证阻塞/唤醒与调度器协同；
 *   - idle：没有就绪线程时兜底。
 */

#define DEMO_TICK_NS 1000000ULL /* 1 ms */

/*
 * 时间片是 5 个 tick。worker-a 必须活得比它久，抢占才会真的发生；
 * 用 timebase 而不是固定循环次数计时，避免主机快慢导致测试抖动。
 */
#define DEMO_WORK_TICKS(freq) ((freq) / 100)  /* 10 ms */
#define DEMO_IDLE_TICKS(freq) ((freq) / 1000) /* 1 ms */

static _Alignas(16) uint8_t idle_stack[4096];
static _Alignas(16) uint8_t worker_a_stack[4096];
static _Alignas(16) uint8_t worker_b_stack[4096];
static _Alignas(16) uint8_t client_stack[4096];
static _Alignas(16) uint8_t server_stack[4096];

struct sched_case {
    struct thread *main_thread;
    struct waitq done;
    unsigned completed;
    unsigned target;

    volatile unsigned a_started;
    volatile unsigned a_done;
    volatile unsigned b_started;
    volatile unsigned b_done;
    volatile unsigned overlap;
    volatile unsigned ticks;

    struct endpoint *ep;
    bool client_ok;
    bool server_ok;
};

static struct sched_case *current_case;

static void finish_one(void)
{
    struct sched_case *c = current_case;
    uint64_t flags = sched_lock();

    c->completed++;
    if (c->completed == c->target) {
        thread_wake_locked(c->main_thread);
    }
    sched_unlock(flags);
}

static void on_tick(struct clock_timer *timer)
{
    struct sched_case *c = current_case;

    c->ticks++;
    sched_tick();

    /* 周期定时器锚定旧 deadline，避免累积漂移。 */
    clock_timer_arm(timer, timer->deadline + DEMO_TICK_NS);
}

static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    if (frame->scause != (SCAUSE_IRQ | IRQ_S_TIMER)) {
        return NULL;
    }

    clock_handle_interrupt();
    /* 从 trap 返回前完成抢占；trap_handler 的返回值允许是另一个线程。 */
    sched_check();
    return frame;
}

static void idle_loop(void *argument)
{
    (void)argument;
    for (;;) {
        __asm__ volatile("wfi");
    }
}

static void spin_ticks(clock_ticks_t ticks)
{
    clock_ticks_t start = clock_ticks();

    while (clock_ticks() - start < ticks) {
        for (volatile unsigned i = 0; i < 10000; i++) {
        }
    }
}

static void worker_a(void *argument)
{
    struct sched_case *c = argument;

    c->a_started = 1;
    spin_ticks(DEMO_WORK_TICKS(clock_freq()));
    c->a_done = 1;
    finish_one();
}

static void worker_b(void *argument)
{
    struct sched_case *c = argument;

    /* 如果在 a 完成前就获得了 CPU，说明发生了定时器抢占。 */
    if (!c->a_done) {
        c->overlap = 1;
    }
    c->b_started = 1;
    spin_ticks(DEMO_IDLE_TICKS(clock_freq()));
    c->b_done = 1;
    finish_one();
}

static void client(void *argument)
{
    struct sched_case *c = argument;
    const ipc_message request = {.tag = 0x21, .words = {100, 0, 0, 0}};
    ipc_message reply;

    c->client_ok = ep_call(c->ep, &request, &reply) == IPC_OK &&
                   reply.tag == 0x22 && reply.words[0] == 101;
    finish_one();
}

static void server(void *argument)
{
    struct sched_case *c = argument;
    ipc_message request;
    ipc_message reply;

    if (ep_recv(c->ep, &request) == IPC_OK && request.tag == 0x21 &&
        request.words[0] == 100 && request.reply_id != 0) {
        reply = (ipc_message){.tag = 0x22, .words = {101, 0, 0, 0}};
        c->server_ok = ep_reply(request.reply_id, &reply) == IPC_OK;
    }
    finish_one();
}

/*
 * 在“检查条件 + 阻塞”之间不能留时间窗口，否则定时器可以在两步
 * 之间调度，造成丢失唤醒。调用者持有 sched_lock()，sched() 也在
 * 同一关中断区间内运行。
 */
static void wait_all(struct sched_case *c)
{
    for (;;) {
        uint64_t flags = sched_lock();

        if (c->completed >= c->target) {
            sched_unlock(flags);
            return;
        }
        thread_block_locked(&c->done);
        sched();
        sched_unlock(flags);
    }
}

static void run(void)
{
    static struct thread runner;
    struct sched_case test = {0};
    struct clock_timer ticker;
    struct thread *idle;
    uint64_t sie;
    uint64_t sstatus;

    object_system_init();
    sched_init();
    ipc_init();
    waitq_init(&test.done);
    test.target = 4;

    runner.name = "bootstrap";
    sched_bootstrap(&runner);
    test.main_thread = &runner;
    current_case = &test;

    test.ep = ep_create();
    CHECK(test.ep != NULL);

    idle = thread_create(NULL, idle_loop, NULL, idle_stack, sizeof(idle_stack),
                         "idle");
    CHECK(idle != NULL);
    sched_set_idle(idle);

    CHECK(thread_create(NULL, worker_a, &test, worker_a_stack,
                        sizeof(worker_a_stack), "worker-a") != NULL);
    CHECK(thread_create(NULL, worker_b, &test, worker_b_stack,
                        sizeof(worker_b_stack), "worker-b") != NULL);
    CHECK(thread_create(NULL, client, &test, client_stack,
                        sizeof(client_stack), "client") != NULL);
    CHECK(thread_create(NULL, server, &test, server_stack,
                        sizeof(server_stack), "server") != NULL);

    clock_init();
    clock_timer_init(&ticker, on_tick, &test);
    CHECK(clock_timer_arm(&ticker, clock_now() + DEMO_TICK_NS));

    sie = csr_read(CSR_SIE);
    sstatus = csr_read(CSR_SSTATUS);
    csr_set(CSR_SIE, SIE_STIE);
    irq_enable();

    wait_all(&test);

    /*
     * 此时只有 idle 可运行；主线程睡 5 ms，由 clock 的 one-shot
     * deadline 唤醒。这同时验证 waitq、timer 和 idle 三者协作。
     */
    clock_ns_t sleep_start = clock_now();
    thread_sleep_ns(5 * DEMO_TICK_NS);
    CHECK(clock_now() - sleep_start >= 5 * DEMO_TICK_NS);

    /* 先关中断，再拆定时器，避免验证阶段被抢占。 */
    irq_disable();
    csr_write(CSR_SIE, sie);
    clock_timer_cancel(&ticker);
    current_case = NULL;

    CHECK(test.a_started && test.a_done);
    CHECK(test.b_started && test.b_done);
    CHECK(test.overlap);
    CHECK(test.ticks > SCHED_QUANTUM_TICKS);
    CHECK(test.client_ok && test.server_ok);
    CHECK(sched_ready_count() == 0);

    ep_unref(test.ep);
    if ((sstatus & SSTATUS_SIE) != 0) {
        irq_enable();
    } else {
        irq_disable();
    }
    sched_init();

    demo_log("sched: timer preemption, quantum, block/wake and IPC");
}

DEMO_MODULE(sched, false, run, handle_trap);
