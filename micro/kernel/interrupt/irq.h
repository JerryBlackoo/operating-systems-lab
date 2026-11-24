#ifndef MICRO_KERNEL_INTERRUPT_IRQ_H
#define MICRO_KERNEL_INTERRUPT_IRQ_H

#include <stdbool.h>
#include <stdint.h>

/*
 * 线命名空间：
 *   0      : S-mode timer（scause = 5）
 *   1..95  : QEMU virt PLIC source ID
 *
 * PLIC 的 ID 0 表示“没有中断”，正好留给 timer，
 * 因此 line 与 PLIC source ID 可以直接一一对应。
 */
#define IRQ_TIMER        0
#define IRQ_LINE_MAX     96
#define IRQ_NONE         (-1)

#define IRQ_OK           0
#define IRQ_EINVAL       (-2)
#define IRQ_EBUSY        (-3)
#define IRQ_ENOENT       (-4)

/* 每次 trap 最多处理的线数；预算用完由 SEIP 再次触发。 */
#define IRQ_DRAIN_BUDGET 32

enum irq_state {
    IRQ_DISARMED = 0,   /* 未绑定；控制器侧必须处于屏蔽/闸住状态 */
    IRQ_ARMED,          /* 已绑定；允许投递 */
    IRQ_ACTIVE,         /* 已 claim；等待用户 ack */
};

struct irq_sink {
    /*
     * 只负责“投递”。实现必须非阻塞，可以在 trap 上下文调用；
     * 是否唤醒线程、是否立即切换由模块外部决定。
     */
    void (*notify)(struct irq_sink *self, int line);
    void *ctx;
};

/*
 * 平台端口：本模块只依赖这 6 个操作，不包含任何 PLIC/CSR 细节。
 *
 * 约定：
 *   - claim() 返回下一条待处理线，或 IRQ_NONE；只用于外部中断。
 *   - complete() 必须“解闸”：PLIC 写 complete，timer 重装；
 *     若该线已 mask，complete 不得让它重新 pending。
 *   - mask()/unmask() 只影响一条线，不得睡眠。
 *   - intr_save() 返回旧的中断状态；intr_restore() 原样恢复。
 *   - 所有操作都必须能在关中断上下文中完成。
 */
struct irq_port {
    int (*claim)(void);
    void (*complete)(int line);
    void (*mask)(int line);
    void (*unmask)(int line);
    unsigned long (*intr_save)(void);
    void (*intr_restore)(unsigned long flags);
};

struct irq_stats {
    uint64_t fires;       /* ARMED -> ACTIVE 的次数 */
    uint64_t acks;        /* 成功 ack 的次数 */
    uint64_t spurious;    /* 无主/未绑定线上的事件 */
    uint64_t coalesced;   /* ACTIVE 期间重复到达的事件 */
    uint64_t bad_acks;    /* 非 ACTIVE 状态的 ack */
};

/*
 * 初始化：清空所有线、屏蔽所有线。
 * 可重复调用：若旧端口仍有 ACTIVE 线，先 mask 再 complete，再切换端口，
 * 因此不会把旧 claim 永久留在控制器里。调用后所有线回到 DISARMED，
 * 需要再用 irq_bind() 逐条解闸。
 */
int irq_init(const struct irq_port *port);

/*
 * 撤销当前端口：mask 所有线、complete 所有 ACTIVE 线、清空 sink 与统计，
 * 然后注销端口。之后所有入口（bind/fire/ack/query）返回 IRQ_EINVAL，
 * 直到再次 irq_init()。
 */
int irq_reset(void);

/* 绑定/解绑：状态迁移期间会在本地关中断。 */
int irq_bind(int line, struct irq_sink *sink);
int irq_unbind(int line);

/*
 * 用户态 ack：complete + 回到 ARMED。
 * 电平线如果仍然有效，会在这里之后立即重新 pending。
 */
int irq_ack(int line);

/*
 * trap 侧入口。
 *
 * irq_fire(): 已知线的事件。外部线必须已经 claim；timer 由 scause 解码。
 *             返回 1 表示已 notify，0 表示被合并或属于伪中断，负数为参数错误。
 *             必须在本地中断关闭的 trap 上下文调用。
 *
 * irq_dispatch(): 从 port->claim() 循环取线，最多 IRQ_DRAIN_BUDGET 次。
 *                 返回本次处理的线数，负数为错误。
 */
int irq_fire(int line);
int irq_dispatch(void);

/* 观测接口：不改变状态。 */
int irq_get_state(int line, enum irq_state *out);
int irq_get_stats(int line, struct irq_stats *out);
const char *irq_state_name(enum irq_state state);

#endif /* MICRO_KERNEL_INTERRUPT_IRQ_H */
