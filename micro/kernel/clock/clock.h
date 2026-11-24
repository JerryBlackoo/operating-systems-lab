#ifndef KERNEL_CLOCK_CLOCK_H
#define KERNEL_CLOCK_CLOCK_H

#include <stdbool.h>
#include <stdint.h>

/* 时间域：自 clock_init() 起的单调纳秒数。 */
typedef uint64_t clock_ns_t;

/* 硬件域：timebase ticks。 */
typedef uint64_t clock_ticks_t;

/*
 * QEMU virt 的 timebase 为 10 MHz（DTB 的 /cpus/timebase-frequency）。
 * 换平台时应改为从 DTB 读取，而不是修改换算逻辑。
 */
#define CLOCK_TIMEBASE_HZ 10000000ULL

enum clock_timer_state {
    CLOCK_TIMER_INACTIVE = 0,
    CLOCK_TIMER_ENQUEUED,
    CLOCK_TIMER_RUNNING,
};

/*
 * 侵入式定时器。调用者负责对象的生命周期：在 cancel 返回 true 或回调
 * 返回之前，不能释放该对象。
 */
struct clock_timer {
    struct clock_timer *next;
    clock_ns_t deadline;
    void (*callback)(struct clock_timer *timer);
    void *arg;
    uint64_t seq;
    uint32_t state;
};

/*
 * 初始化当前 hart 的定时器队列并撤防比较器。
 * 每个 hart 在开中断前各调用一次；epoch 只在第一次调用时建立，
 * 因此后续 hart 看到的是同一条单调时间轴。
 * 不会打开 STIE/SIE。
 */
void clock_init(void);

/* 当前单调时间，单位 ns。 */
clock_ns_t clock_now(void);

/*
 * 从 base 起 delta ns 的绝对 deadline，溢出时饱和到 UINT64_MAX。
 * 饱和值经过 ns -> ticks 换算后等价于“永不触发”，
 * 适合表达没有超时的等待。
 */
static inline clock_ns_t clock_deadline_after(clock_ns_t base, clock_ns_t delta)
{
    return delta > UINT64_MAX - base ? UINT64_MAX : base + delta;
}

/* 当前硬件 ticks（未减去 boot 时的基准值）。 */
clock_ticks_t clock_ticks(void);

/* 硬件 timebase 频率。 */
uint64_t clock_freq(void);

/*
 * 初始化一个定时器对象。callback 不得为 NULL 才能 arm。
 * 只能在 INACTIVE 状态调用；已入队的对象重复 init 会破坏队列链接。
 * callback 运行在定时器中断上下文：必须短、非阻塞、不得睡眠。
 */
void clock_timer_init(struct clock_timer *timer,
                      void (*callback)(struct clock_timer *timer),
                      void *arg);

/*
 * 以绝对 deadline（ns）arm 定时器。
 * - 定时器归 arm 它的 hart 所有，回调也只在那个 hart 上运行。
 * - 已 ENQUEUED 的定时器会被重新插入（re-arm）。
 * - 只有当前正在执行的回调可以重装自己；对其它 RUNNING 定时器 arm 返回 false。
 * - deadline <= now 时立即到期；实际回调由当前或下一次中断派发。
 * 返回 false 表示 callback 为空或定时器状态不允许。
 */
bool clock_timer_arm(struct clock_timer *timer, clock_ns_t deadline);

/*
 * 取消一个 ENQUEUED 定时器。
 * 返回 true 表示已从队列移除；false 表示未入队或已经进入派发（RUNNING），
 * 此时回调仍可能执行，调用者必须容忍这次“迟到的通知”。
 */
bool clock_timer_cancel(struct clock_timer *timer);

/* 是否仍在队列中等待。 */
bool clock_timer_is_pending(const struct clock_timer *timer);

/*
 * S-mode 定时器中断入口。
 * 必须在 SIE 关闭、且已在 scause 中确认是 supervisor timer 中断后调用。
 * 只处理当前 hart 的队列：先撤防，再弹出所有到期定时器、重装下一次
 * 比较值、执行回调。
 */
void clock_handle_interrupt(void);

#endif
