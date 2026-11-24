#include "demo.h"
#include "sched.h"

#include "clock/clock.h"
#include "ipc/ipc.h"

#include "csr.h"
#include "trap.h"

/*
 * 这个 demo 把时间层接进真实内核路径：
 *   sched_start_tick() → clock one-shot → S 态 timer 中断
 *   clock_handle_interrupt() → sched_tick()
 *   trap 返回前 → sched_check() → sched() → cpu_switch()
 *
 * 工作负载由 4 条普通线程和 1 条 idle 组成：
 *   - worker-a / worker-b：纯计算，不主动让出，验证时间片抢占；
 *   - client / server：一次同步 IPC，验证阻塞/唤醒与调度器协同；
 *   - idle：没有就绪线程时兜底。
 *
 * 串口同时打印 IRQ 次数、真实 tick 次数、切换次数和抢占双方名字，
 * 让“定时器真的触发并导致切换”可以直接从 QEMU 输出核对。
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
    volatile unsigned timer_irqs;
    volatile unsigned preempt_switches;
    volatile bool a_preempted;
    clock_ticks_t last_irq_tick;
    struct thread *worker_a_thread;

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

static char *log_str(char *p, const char *s)
{
    while (*s != '\0') {
        *p++ = *s++;
    }
    return p;
}

static char *log_dec(char *p, unsigned value)
{
    char digits[10];
    unsigned count = 0;

    if (value == 0) {
        *p++ = '0';
        return p;
    }
    while (value != 0) {
        digits[count++] = (char)('0' + value % 10);
        value /= 10;
    }
    while (count != 0) {
        *p++ = digits[--count];
    }
    return p;
}

static char *log_hex64(char *p, uint64_t value)
{
    static const char hex[] = "0123456789abcdef";

    *p++ = '0';
    *p++ = 'x';
    for (int shift = 60; shift >= 0; shift -= 4) {
        *p++ = hex[(value >> shift) & 0xf];
    }
    return p;
}

static void log_buffer(char *buf, char *end)
{
    *end = '\0';
    demo_log(buf);
}

static void log_timer_irq(struct sched_case *c, const struct trap_frame *frame,
                          clock_ticks_t tick, clock_ticks_t delta)
{
    char buf[128];
    char *p = buf;

    p = log_str(p, "[sched] S-timer IRQ #");
    p = log_dec(p, c->timer_irqs);
    p = log_str(p, ": scause=");
    p = log_hex64(p, frame->scause);
    p = log_str(p, " tick=");
    p = log_dec(p, (unsigned)tick);
    if (delta != 0) {
        p = log_str(p, " delta=");
        p = log_dec(p, (unsigned)delta);
    }
    log_buffer(buf, p);
}

static void log_summary(const struct sched_case *c)
{
    char buf[192];
    char *p = buf;

    p = log_str(p, "[sched] integrated: irqs=");
    p = log_dec(p, c->timer_irqs);
    p = log_str(p, " ticks=");
    p = log_dec(p, (unsigned)sched_tick_count());
    p = log_str(p, " switches=");
    p = log_dec(p, (unsigned)sched_switch_count());
    p = log_str(p, " quantum_expired=");
    p = log_dec(p, (unsigned)sched_quantum_expiration_count());
    p = log_str(p, " preempt_switches=");
    p = log_dec(p, c->preempt_switches);
    log_buffer(buf, p);
}

static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    struct sched_case *c = current_case;
    struct thread *cur;
    uint64_t ticks_before = 0;
    bool worker_a_last_tick = false;

    if (frame->scause != (SCAUSE_IRQ | IRQ_S_TIMER)) {
        return NULL;
    }

    if (c != NULL) {
        clock_ticks_t tick = clock_ticks();

        c->timer_irqs++;
        if (c->timer_irqs <= 3) {
            clock_ticks_t delta =
                c->timer_irqs == 1 ? 0 : tick - c->last_irq_tick;

            log_timer_irq(c, frame, tick, delta);
        }
        c->last_irq_tick = tick;

        /*
         * 在派发 clock 队列前记录：本次 tick 是否正好耗尽 worker-a 的
         * 最后一个时间片。只有 tick_count 确实增加才算，避免把唤醒
         * sleep 的 timer 中断误记成调度 tick。
         */
        cur = sched_current();
        ticks_before = sched_tick_count();
        worker_a_last_tick =
            cur != NULL && cur == c->worker_a_thread && cur->time_left == 1;
    }

    clock_handle_interrupt();

    if (worker_a_last_tick && sched_tick_count() != ticks_before) {
        c->a_preempted = true;
        demo_log("[sched] sched_tick: worker-a time_left 1->0, "
                 "need_resched set");
    }

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
    demo_log("[sched] worker-a running without yielding");
    spin_ticks(DEMO_WORK_TICKS(clock_freq()));
    c->a_done = 1;
    if (c->a_preempted) {
        demo_log("[sched] worker-a resumed after timer preemption");
    }
    finish_one();
}

static void worker_b(void *argument)
{
    struct sched_case *c = argument;

    /* 如果在 a 完成前就获得了 CPU，说明发生了定时器抢占。 */
    if (!c->a_done) {
        c->overlap = 1;
        c->preempt_switches++;
        if (c->a_preempted) {
            char buf[128];
            char *p = buf;

            p = log_str(p, "[sched] switch: worker-a -> worker-b, "
                           "sched switches=");
            p = log_dec(p, (unsigned)sched_switch_count());
            log_buffer(buf, p);
        }
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
    struct thread *idle;
    struct thread *worker;
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

    worker = thread_create(NULL, worker_a, &test, worker_a_stack,
                           sizeof(worker_a_stack), "worker-a");
    CHECK(worker != NULL);
    test.worker_a_thread = worker;
    CHECK(thread_create(NULL, worker_b, &test, worker_b_stack,
                        sizeof(worker_b_stack), "worker-b") != NULL);
    CHECK(thread_create(NULL, client, &test, client_stack,
                        sizeof(client_stack), "client") != NULL);
    CHECK(thread_create(NULL, server, &test, server_stack,
                        sizeof(server_stack), "server") != NULL);

    clock_init();
    sie = csr_read(CSR_SIE);
    sstatus = csr_read(CSR_SSTATUS);
    CHECK(sched_start_tick(DEMO_TICK_NS));
    CHECK((csr_read(CSR_SIE) & SIE_STIE) != 0);
    demo_log("[sched] timer: SBI one-shot tick source armed at 1 ms, "
             "sie.STIE=1");
    irq_enable();
    CHECK(irq_enabled());

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
    sched_stop_tick();
    csr_write(CSR_SIE, sie);
    current_case = NULL;

    CHECK(test.a_started && test.a_done);
    CHECK(test.b_started && test.b_done);
    CHECK(test.overlap);
    CHECK(test.timer_irqs >= 2);
    CHECK(sched_tick_count() > SCHED_QUANTUM_TICKS);
    CHECK(sched_quantum_expiration_count() >= 1);
    CHECK(sched_switch_count() >= 1);
    CHECK(test.preempt_switches >= 1);
    CHECK(test.client_ok && test.server_ok);
    CHECK(sched_ready_count() == 0);

    log_summary(&test);

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
