#include "clock.h"
#include "platform.h"

#include <stddef.h>

/*
 * 每个 hart 一条有序侵入式链表 + 一个单槽比较器 D。
 *
 * 为什么按 hart 分开：
 *   CLINT 的 mtimecmp 和 SBI set_timer 都是 per-hart 的。把队列也按 hart
 *   切开，Q 与 D 才有一一对应关系；否则一个 hart 的 arm 会改写另一个
 *   hart 的比较器，唤醒语义就错了。
 *
 * 单 hart 上插入/取消都是 O(n)，取下一个 deadline 是 O(1)。微内核的内核
 * 定时器数量约等于阻塞线程数加调度事件数，通常很小；数量变大后再换
 * rbtree，不需要 Linux 的 8 层时间轮。
 *
 * 定时器归 arm 它的 hart 所有：只能由同一个 hart 再 arm/cancel，回调也
 * 只在该 hart 的中断上下文执行。
 */
#define CLOCK_MAX_HARTS 64

struct clock_hart {
    struct clock_timer *head;
    struct clock_timer *running;
    uint64_t programmed;
};

static struct clock_hart harts[CLOCK_MAX_HARTS];
/* hart id 超出表的兜底槽：宁可让这些 hart 共享一个队列，也不串到 hart 0。 */
static struct clock_hart overflow_hart;
static uint64_t next_seq = 1;

static struct clock_hart *this_hart(void)
{
    uint32_t id = clock_arch_hart_id();

    return id < CLOCK_MAX_HARTS ? &harts[id] : &overflow_hart;
}

static void q_insert(struct clock_hart *h, struct clock_timer *timer)
{
    struct clock_timer **link = &h->head;

    while (*link != NULL &&
           ((*link)->deadline < timer->deadline ||
            ((*link)->deadline == timer->deadline &&
             (*link)->seq < timer->seq))) {
        link = &(*link)->next;
    }

    timer->next = *link;
    *link = timer;
}

static bool q_remove(struct clock_hart *h, struct clock_timer *timer)
{
    struct clock_timer **link = &h->head;

    while (*link != NULL) {
        if (*link == timer) {
            *link = timer->next;
            timer->next = NULL;
            return true;
        }
        link = &(*link)->next;
    }

    return false;
}

static struct clock_timer *q_pop_leq(struct clock_hart *h, clock_ns_t now)
{
    struct clock_timer *expired = NULL;
    struct clock_timer **tail = &expired;

    while (h->head != NULL && h->head->deadline <= now) {
        struct clock_timer *timer = h->head;

        h->head = timer->next;
        timer->next = NULL;
        *tail = timer;
        tail = &timer->next;
    }

    return expired;
}

static void program_head(struct clock_hart *h)
{
    uint64_t ticks;

    if (h->head == NULL)
        ticks = CLOCK_DISARM;
    else
        ticks = clock_ns_to_ticks_ceil(h->head->deadline);

    /*
     * 只有比较值真正变化时才 ecall 进 M-mode。
     * 记录 programmed 也避免在“撤防 -> 撤防”这类路径上重复 SBI 调用。
     */
    if (ticks != h->programmed) {
        clock_arch_set_compare(ticks);
        h->programmed = ticks;
    }
}

void clock_init(void)
{
    for (int i = 0; i < CLOCK_MAX_HARTS; i++) {
        harts[i].head = NULL;
        harts[i].running = NULL;
        harts[i].programmed = CLOCK_DISARM;
    }
    overflow_hart.head = NULL;
    overflow_hart.running = NULL;
    overflow_hart.programmed = CLOCK_DISARM;

    next_seq = 1;
    clock_timebase_init();
}

void clock_timer_init(struct clock_timer *timer,
                      void (*callback)(struct clock_timer *timer),
                      void *arg)
{
    if (timer == NULL)
        return;

    timer->next = NULL;
    timer->deadline = 0;
    timer->callback = callback;
    timer->arg = arg;
    timer->seq = 0;
    timer->state = CLOCK_TIMER_INACTIVE;
}

bool clock_timer_arm(struct clock_timer *timer, clock_ns_t deadline)
{
    struct clock_hart *h;
    uint64_t flags;
    bool was_head;

    if (timer == NULL || timer->callback == NULL)
        return false;

    h = this_hart();
    flags = clock_arch_intr_save();
    was_head = (h->head == timer);

    if (timer->state == CLOCK_TIMER_ENQUEUED) {
        q_remove(h, timer);
    } else if (timer->state == CLOCK_TIMER_RUNNING && timer != h->running) {
        /* 只有当前回调可以重装自己。 */
        clock_arch_intr_restore(flags);
        return false;
    }

    timer->deadline = deadline;
    timer->seq = next_seq++;
    timer->next = NULL;
    timer->state = CLOCK_TIMER_ENQUEUED;
    q_insert(h, timer);

    /* 只有队列头变化时才需要写比较器。 */
    if (was_head || h->head == timer)
        program_head(h);

    clock_arch_intr_restore(flags);
    return true;
}

bool clock_timer_cancel(struct clock_timer *timer)
{
    struct clock_hart *h;
    uint64_t flags;
    bool removed = false;

    if (timer == NULL)
        return false;

    h = this_hart();
    flags = clock_arch_intr_save();

    if (timer->state == CLOCK_TIMER_ENQUEUED) {
        bool was_head = (h->head == timer);

        q_remove(h, timer);
        timer->state = CLOCK_TIMER_INACTIVE;
        if (was_head)
            program_head(h);
        removed = true;
    }

    clock_arch_intr_restore(flags);
    return removed;
}

bool clock_timer_is_pending(const struct clock_timer *timer)
{
    return timer != NULL && timer->state == CLOCK_TIMER_ENQUEUED;
}

void clock_handle_interrupt(void)
{
    struct clock_hart *h = this_hart();
    struct clock_timer *expired;
    clock_ns_t now;

    /*
     * 先撤防。QEMU 在 mtimecmp <= mtime 时会立刻再次拉高中断；
     * 不撤防就会在回调还没跑完时形成中断风暴。
     */
    clock_arch_set_compare(CLOCK_DISARM);
    h->programmed = CLOCK_DISARM;

    now = clock_now();
    expired = q_pop_leq(h, now);

    /*
     * 整批标记 RUNNING：一旦进入派发批次，cancel 返回 false，
     * 回调仍会执行；调用者必须容忍这次迟到通知。
     */
    for (struct clock_timer *timer = expired; timer != NULL;
         timer = timer->next) {
        timer->state = CLOCK_TIMER_RUNNING;
    }

    /* 先装下一次，再跑回调；回调里 arm 的更早 deadline 会再次改写。 */
    program_head(h);

    while (expired != NULL) {
        struct clock_timer *timer = expired;

        expired = timer->next;
        timer->next = NULL;

        h->running = timer;
        timer->callback(timer);
        h->running = NULL;

        /*
         * 回调可能重装自己：此时 state 已是 ENQUEUED，不能覆盖。
         * 否则回调结束即回到 INACTIVE。
         */
        if (timer->state == CLOCK_TIMER_RUNNING)
            timer->state = CLOCK_TIMER_INACTIVE;
    }
}
