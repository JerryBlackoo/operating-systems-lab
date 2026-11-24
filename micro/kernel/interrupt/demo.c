#include "csr.h"
#include "demo.h"
#include "ipc/ipc.h"
#include "irq.h"
#include "sbi.h"
#include "sched.h"
#include "trap.h"

enum {
    DEMO_LINE = 10,
    IRQ_DRIVER_TAG = 0x495251,
    /*
     * QEMU virt 的 timebase 是 10 MHz，50 ms 足够让驱动线程完成
     * “被唤醒 → ack → 退出”，又不会在 unbind 前再次触发。
     */
    TIMER_INTERVAL_TICKS = 500000,
};

/* ------------------------------------------------------------------ */
/* 阶段一：模拟控制器。验证 claim 闸门、一次性通知与电平重采样。      */
/* ------------------------------------------------------------------ */

static bool masked;
static int pending = IRQ_NONE;
static unsigned notified, completed;

static int fake_claim(void)
{
    int line = pending;
    pending = IRQ_NONE;
    return line;
}

static void fake_complete(int line)
{
    CHECK(line == DEMO_LINE);
    completed++;
}

static void fake_mask(int line)
{
    if (line == DEMO_LINE)
        masked = true;
}

static void fake_unmask(int line)
{
    CHECK(line == DEMO_LINE);
    masked = false;
}

static void fake_notify(struct irq_sink *sink, int line)
{
    CHECK(line == DEMO_LINE && !masked);
    (*(unsigned *)sink->ctx)++;
}

static void model_cases(void)
{
    static const struct irq_port port = {
        fake_claim, fake_complete, fake_mask, fake_unmask, irq_save, irq_restore,
    };
    struct irq_sink sink = {fake_notify, &notified};
    enum irq_state state;
    struct irq_stats stats;

    CHECK(irq_init(&port) == IRQ_OK && masked);
    CHECK(irq_bind(DEMO_LINE, &sink) == IRQ_OK && !masked);
    pending = DEMO_LINE;
    CHECK(irq_dispatch() == 1 && notified == 1 && completed == 0);
    CHECK(irq_fire(DEMO_LINE) == 0 && notified == 1);
    CHECK(irq_ack(DEMO_LINE) == IRQ_OK && completed == 1);
    CHECK(irq_get_state(DEMO_LINE, &state) == IRQ_OK && state == IRQ_ARMED);
    pending = DEMO_LINE;
    CHECK(irq_dispatch() == 1 && notified == 2);
    CHECK(irq_unbind(DEMO_LINE) == IRQ_OK && masked && completed == 2);
    CHECK(irq_get_stats(DEMO_LINE, &stats) == IRQ_OK);
    CHECK(stats.fires == 2 && stats.coalesced == 1 && stats.acks == 1);
    CHECK(irq_reset() == IRQ_OK);
}

/* ------------------------------------------------------------------ */
/* 阶段二：真实 S-mode 定时器 + 真实陷阱 + 线程 + IPC。               */
/*                                                                    */
/* trap 入口只做识别与撤防；irq_fire() 把事件交给 sink；sink 在中断   */
/* 上下文里向已经阻塞的驱动线程投递 IPC 消息；驱动线程 ack 解闸。      */
/* ------------------------------------------------------------------ */

static bool timer_masked = true;
static unsigned timer_rearms;

static int hw_claim(void)
{
    /*
     * 本 demo 未接 PLIC MMIO：外部线没有后端，claim 路径由主机 Mock 覆盖。
     * 真实 PLIC 端口应放在 arch/riscv 的机器服务层，而不是本模块。
     */
    return IRQ_NONE;
}

static void hw_mask(int line)
{
    if (line != IRQ_TIMER)
        return;

    csr_clear(CSR_SIE, SIE_STIE);
    timer_masked = true;
    sbi_set_timer(UINT64_MAX);
}

static void hw_unmask(int line)
{
    if (line != IRQ_TIMER)
        return;

    timer_masked = false;
    sbi_set_timer(csr_read(CSR_TIME) + TIMER_INTERVAL_TICKS);
    csr_set(CSR_SIE, SIE_STIE);
}

static void hw_complete(int line)
{
    if (line != IRQ_TIMER)
        return;

    if (timer_masked) {
        /* 已 mask 的线 complete 不得让它重新 pending。 */
        sbi_set_timer(UINT64_MAX);
        return;
    }

    sbi_set_timer(csr_read(CSR_TIME) + TIMER_INTERVAL_TICKS);
    timer_rearms++;
}

static const struct irq_port hw_port = {
    hw_claim, hw_complete, hw_mask, hw_unmask, irq_save, irq_restore,
};

static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    if (frame->scause != (SCAUSE_IRQ | IRQ_S_TIMER))
        return NULL;

    /*
     * 先撤防：SBI timer 是一次性的电平式中断，比较值仍在过去会持续
     * 拉高 STIP。撤防后 ACTIVE 期间才真正只有“合并”，不会中断风暴。
     */
    sbi_set_timer(UINT64_MAX);
    CHECK(irq_fire(IRQ_TIMER) == 1);
    return frame;
}

struct irq_route {
    struct irq_sink sink;
    struct endpoint *ep;
    volatile unsigned delivered;
    volatile unsigned dropped;
    volatile bool failed;
    volatile bool acked;
    volatile bool handled;
};

/*
 * 中断上半部：非阻塞。只有驱动线程已经阻塞在 recv 上时才握手，
 * 否则计数丢弃——这里绝不能调用会阻塞的 ep_send。
 */
static void route_notify(struct irq_sink *self, int line)
{
    struct irq_route *route = self->ctx;
    const ipc_message msg = {
        .tag = IRQ_DRIVER_TAG,
        .reply_id = 0,
        .words = {(uint64_t)line},
    };

    if (waitq_empty(&route->ep->recvq)) {
        route->dropped++;
        return;
    }
    if (ep_send(route->ep, &msg) != IPC_OK) {
        route->failed = true;
        return;
    }
    route->delivered++;
}

static void irq_driver(void *argument)
{
    struct irq_route *route = argument;
    ipc_message msg;
    enum irq_state state;

    CHECK(ep_recv(route->ep, &msg) == IPC_OK);
    CHECK(msg.tag == IRQ_DRIVER_TAG && msg.words[0] == IRQ_TIMER);
    CHECK(irq_get_state(IRQ_TIMER, &state) == IRQ_OK && state == IRQ_ACTIVE);
    CHECK(irq_ack(IRQ_TIMER) == IRQ_OK);

    route->acked = true;
    route->handled = true;
}

static void integration_case(void)
{
    static _Alignas(16) uint8_t driver_stack[4096];
    static struct irq_route route;
    struct thread runner = {.name = "irq-demo"};
    struct irq_stats stats;
    enum irq_state state;
    u64 start;

    route.sink.notify = route_notify;
    route.sink.ctx = &route;
    route.ep = NULL;
    route.delivered = 0;
    route.dropped = 0;
    route.failed = false;
    route.acked = false;
    route.handled = false;
    timer_rearms = 0;

    object_system_init();
    sched_init();
    ipc_init();
    sched_bootstrap(&runner);

    route.ep = ep_create();
    CHECK(route.ep != NULL);
    CHECK(thread_create(NULL, irq_driver, &route, driver_stack,
                        sizeof(driver_stack), "irq-driver") != NULL);

    /* 先让驱动线程阻塞在 endpoint 上，再武装 timer。 */
    for (int spins = 0; spins < 8 && route.ep->state != EP_RECV_WAIT; spins++)
        thread_yield();
    CHECK(route.ep->state == EP_RECV_WAIT);

    CHECK(irq_init(&hw_port) == IRQ_OK);
    CHECK(irq_bind(IRQ_TIMER, &route.sink) == IRQ_OK);
    CHECK(timer_rearms == 0);

    irq_enable();
    start = csr_read(CSR_TIME);
    while (!route.handled &&
           csr_read(CSR_TIME) - start < TIMER_INTERVAL_TICKS * 10)
        thread_yield();
    irq_disable();

    CHECK(route.handled && route.acked);
    CHECK(route.delivered == 1 && route.dropped == 0 && !route.failed);
    CHECK(irq_get_state(IRQ_TIMER, &state) == IRQ_OK && state == IRQ_ARMED);
    CHECK(irq_get_stats(IRQ_TIMER, &stats) == IRQ_OK);
    CHECK(stats.fires == 1 && stats.acks == 1 && stats.spurious == 0);
    CHECK(timer_rearms == 1);

    CHECK(irq_unbind(IRQ_TIMER) == IRQ_OK);
    CHECK(irq_reset() == IRQ_OK);

    ep_unref(route.ep);
    sched_init();
    object_system_init();
}

static void run(void)
{
    /* 上一个模块可能留下未撤防的 timer；先建立一个安静起点。 */
    irq_disable();
    csr_clear(CSR_SIE, SIE_STIE);
    sbi_set_timer(UINT64_MAX);
    timer_masked = true;

    model_cases();
    integration_case();
    demo_log("interrupt: one-shot model, real timer, IPC driver and ack");
}

DEMO_MODULE(interrupt, false, run, handle_trap);
