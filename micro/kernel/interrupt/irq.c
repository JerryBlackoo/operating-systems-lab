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

/*
 * 拆除线表：先 mask 再 complete。
 *
 * 顺序与 irq_unbind() 相同：complete 是解闸，如果设备线仍为高，
 * 先 complete 再 mask 会让 source 在屏蔽前重新 pending。先 mask 后
 * complete 才能保证拆除后控制器侧确实安静。
 */
static void lines_teardown(const struct irq_port *port)
{
    for (int line = 0; line < IRQ_LINE_MAX; line++) {
        port->mask(line);
        if (g_lines[line].state == IRQ_ACTIVE) {
            port->complete(line);
        }
        g_lines[line] = (struct irq_line){.state = IRQ_DISARMED};
    }
}

int irq_init(const struct irq_port *port)
{
    if (!port_valid(port)) {
        return IRQ_EINVAL;
    }

    /* 重入：旧端口的 ACTIVE claim 必须先归还，再换端口。 */
    if (g_ready) {
        unsigned long flags = g_port->intr_save();

        lines_teardown(g_port);
        g_port->intr_restore(flags);
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

int irq_reset(void)
{
    const struct irq_port *port;
    unsigned long flags;

    if (!g_ready) {
        return IRQ_EINVAL;
    }

    port = g_port;
    flags = port->intr_save();
    lines_teardown(port);
    g_port = NULL;
    g_ready = false;
    port->intr_restore(flags);
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

    /*
     * 先置 ACTIVE 再 notify：notify 期间同一线的重复事件只能合并。
     * notify 允许同步调用 irq_ack()（内核内驱动线程模型），返回后
     * 状态可能已经是 ARMED；这里不再读写 l，避免假设为 ACTIVE。
     */
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
        int line;

        /* notify 可能调用 irq_reset()；端口注销后不再 claim。 */
        if (!g_ready) {
            break;
        }

        line = g_port->claim();

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
