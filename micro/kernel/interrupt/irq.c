#include "irq.h"

#include <stddef.h>

struct irq_line {
    enum irq_state state;
    struct irq_sink *sink;
    struct irq_stats stats;
};

static struct irq_line g_lines[IRQ_LINE_MAX];
static const struct irq_port *g_port;
static bool g_ready;

static bool line_valid(int line)
{
    return line >= 0 && line < IRQ_LINE_MAX;
}

static bool port_valid(const struct irq_port *port)
{
    return port != NULL && port->claim != NULL && port->complete != NULL &&
           port->mask != NULL && port->unmask != NULL &&
           port->intr_save != NULL && port->intr_restore != NULL;
}

int irq_init(const struct irq_port *port)
{
    if (!port_valid(port)) {
        return IRQ_EINVAL;
    }

    g_port = port;
    for (int line = 0; line < IRQ_LINE_MAX; line++) {
        g_lines[line] = (struct irq_line){.state = IRQ_DISARMED};
        /* DISARMED 的不变量：控制器侧必须处于屏蔽/闸住状态。 */
        port->mask(line);
    }

    g_ready = true;
    return IRQ_OK;
}

int irq_bind(int line, struct irq_sink *sink)
{
    if (!g_ready || !line_valid(line) || sink == NULL || sink->notify == NULL) {
        return IRQ_EINVAL;
    }

    unsigned long flags = g_port->intr_save();
    struct irq_line *l = &g_lines[line];

    if (l->state != IRQ_DISARMED) {
        g_port->intr_restore(flags);
        return IRQ_EBUSY;
    }

    /* 先挂 sink，再解闸：避免出现“已可投递但无人接收”的窗口。 */
    l->sink = sink;
    g_port->unmask(line);
    l->state = IRQ_ARMED;
    g_port->intr_restore(flags);
    return IRQ_OK;
}

int irq_unbind(int line)
{
    if (!g_ready || !line_valid(line)) {
        return IRQ_EINVAL;
    }

    unsigned long flags = g_port->intr_save();
    struct irq_line *l = &g_lines[line];

    if (l->state == IRQ_DISARMED) {
        g_port->intr_restore(flags);
        return IRQ_ENOENT;
    }

    /* 先 mask 再 complete：complete 可能让仍为高的电平线立刻重新 pending。 */
    g_port->mask(line);
    if (l->state == IRQ_ACTIVE) {
        g_port->complete(line);
    }

    l->sink = NULL;
    l->state = IRQ_DISARMED;
    g_port->intr_restore(flags);
    return IRQ_OK;
}

int irq_ack(int line)
{
    if (!g_ready || !line_valid(line)) {
        return IRQ_EINVAL;
    }

    unsigned long flags = g_port->intr_save();
    struct irq_line *l = &g_lines[line];

    if (l->state != IRQ_ACTIVE) {
        l->stats.bad_acks++;
        g_port->intr_restore(flags);
        return IRQ_EINVAL;
    }

    /*
     * complete 是“解闸”，不是“清设备”。
     * 驱动必须先清设备源；这里只负责允许下一次投递。
     */
    g_port->complete(line);
    l->state = IRQ_ARMED;
    l->stats.acks++;
    g_port->intr_restore(flags);
    return IRQ_OK;
}

int irq_fire(int line)
{
    if (!g_ready || !line_valid(line)) {
        return IRQ_EINVAL;
    }

    /*
     * trap 上下文：本地中断已由硬件/入口关闭。
     * 这里不做 save/restore，保证 top half 有界且不改变中断状态。
     */
    struct irq_line *l = &g_lines[line];

    if (l->state == IRQ_ACTIVE) {
        /* 控制器闸门正常时不会走到这里；防御性计数，绝不二次 notify。 */
        l->stats.coalesced++;
        return 0;
    }

    if (l->state != IRQ_ARMED || l->sink == NULL) {
        /*
         * 无主事件也必须 complete，否则 PLIC 的 source 会被永久闸住。
         * 对 timer 而言，complete 等价于重装。
         */
        l->stats.spurious++;
        g_port->complete(line);
        return 0;
    }

    l->state = IRQ_ACTIVE;
    l->stats.fires++;
    l->sink->notify(l->sink, line);
    return 1;
}

int irq_dispatch(void)
{
    if (!g_ready) {
        return IRQ_EINVAL;
    }

    int handled = 0;

    for (int budget = IRQ_DRAIN_BUDGET; budget > 0; budget--) {
        int line = g_port->claim();

        if (line == IRQ_NONE) {
            break;
        }
        if (!line_valid(line)) {
            /* 端口违约：无法 complete，直接停止，避免在坏 ID 上打转。 */
            break;
        }

        handled++;
        (void)irq_fire(line);
    }

    return handled;
}

int irq_get_state(int line, enum irq_state *out)
{
    if (!g_ready || !line_valid(line) || out == NULL) {
        return IRQ_EINVAL;
    }

    *out = g_lines[line].state;
    return IRQ_OK;
}

int irq_get_stats(int line, struct irq_stats *out)
{
    if (!g_ready || !line_valid(line) || out == NULL) {
        return IRQ_EINVAL;
    }

    *out = g_lines[line].stats;
    return IRQ_OK;
}

const char *irq_state_name(enum irq_state state)
{
    switch (state) {
    case IRQ_DISARMED:
        return "DISARMED";
    case IRQ_ARMED:
        return "ARMED";
    case IRQ_ACTIVE:
        return "ACTIVE";
    default:
        return "UNKNOWN";
    }
}
