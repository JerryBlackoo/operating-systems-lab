#include "csr.h"
#include "demo.h"
#include "irq.h"

enum { DEMO_LINE = 10 };

static bool masked;
static int pending = IRQ_NONE;
static unsigned notified, completed;

static int claim(void)
{
    int line = pending;
    pending = IRQ_NONE;
    return line;
}

static void complete(int line)
{
    CHECK(line == DEMO_LINE);
    completed++;
}

static void mask(int line)
{
    if (line == DEMO_LINE)
        masked = true;
}

static void unmask(int line)
{
    CHECK(line == DEMO_LINE);
    masked = false;
}

static void notify(struct irq_sink *sink, int line)
{
    CHECK(line == DEMO_LINE && !masked);
    (*(unsigned *)sink->ctx)++;
}

static void run(void)
{
    static const struct irq_port port = {claim, complete, mask, unmask, irq_save, irq_restore};
    struct irq_sink sink = {notify, &notified};
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
    demo_log("interrupt: claim, one-shot notification, ack and unbind");
}

DEMO_MODULE(interrupt, false, run, NULL);
