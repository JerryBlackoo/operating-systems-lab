#include "clock.h"
#include "platform.h"

#include <stddef.h>

/*
 * 单 hart 的有序侵入式链表。min 在 head，插入 O(n)，取消 O(n)，
 * 取下一个 deadline O(1)。微内核里内核定时器数量很小，这比
 * Linux 的 timer wheel 更直接；数量变大后再换成 rbtree。
 */
static struct clock_timer *head;
static struct clock_timer *running;
static uint64_t next_seq = 1;

static void q_insert(struct clock_timer *timer)
{
    struct clock_timer **link = &head;

    while (*link != NULL &&
           ((*link)->deadline < timer->deadline ||
            ((*link)->deadline == timer->deadline &&
             (*link)->seq < timer->seq))) {
        link = &(*link)->next;
    }

    timer->next = *link;
    *link = timer;
}

static bool q_remove(struct clock_timer *timer)
{
    struct clock_timer **link = &head;

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

static struct clock_timer *q_pop_leq(clock_ns_t now)
{
    struct clock_timer *expired = NULL;
    struct clock_timer **tail = &expired;

    while (head != NULL && head->deadline <= now) {
        struct clock_timer *timer = head;

        head = timer->next;
        timer->next = NULL;
        *tail = timer;
        tail = &timer->next;
    }

    return expired;
}

static void program_head(void)
{
    if (head == NULL)
        clock_arch_set_compare(CLOCK_DISARM);
    else
        clock_arch_set_compare(clock_ns_to_ticks_ceil(head->deadline));
}

void clock_init(void)
{
    head = NULL;
    running = NULL;
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
    uint64_t flags;
    bool was_head;

    if (timer == NULL || timer->callback == NULL)
        return false;

    flags = clock_arch_intr_save();
    was_head = (head == timer);

    if (timer->state == CLOCK_TIMER_ENQUEUED) {
        q_remove(timer);
    } else if (timer->state == CLOCK_TIMER_RUNNING && timer != running) {
        /* 只有当前回调可以重装自己。 */
        clock_arch_intr_restore(flags);
        return false;
    }

    timer->deadline = deadline;
    timer->seq = next_seq++;
    timer->next = NULL;
    timer->state = CLOCK_TIMER_ENQUEUED;
    q_insert(timer);

    /* 只有队列头变化时才需要写比较器。 */
    if (was_head || head == timer)
        program_head();

    clock_arch_intr_restore(flags);
    return true;
}

bool clock_timer_cancel(struct clock_timer *timer)
{
    uint64_t flags;
    bool removed = false;

    if (timer == NULL)
        return false;

    flags = clock_arch_intr_save();

    if (timer->state == CLOCK_TIMER_ENQUEUED) {
        bool was_head = (head == timer);

        q_remove(timer);
        timer->state = CLOCK_TIMER_INACTIVE;
        if (was_head)
            program_head();
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
    struct clock_timer *expired;
    clock_ns_t now;

    /*
     * 先撤防。QEMU 在 mtimecmp <= mtime 时会立刻再次拉高中断；
     * 不撤防就会在回调还没跑完时形成中断风暴。
     */
    clock_arch_set_compare(CLOCK_DISARM);

    now = clock_now();
    expired = q_pop_leq(now);

    /*
     * 整批标记 RUNNING：一旦进入派发批次，cancel 返回 false，
     * 回调仍会执行；调用者必须容忍这次迟到通知。
     */
    for (struct clock_timer *timer = expired; timer != NULL;
         timer = timer->next) {
        timer->state = CLOCK_TIMER_RUNNING;
    }

    /* 先装下一次，再跑回调；回调里 arm 的更早 deadline 会再次改写。 */
    program_head();

    while (expired != NULL) {
        struct clock_timer *timer = expired;

        expired = timer->next;
        timer->next = NULL;

        running = timer;
        timer->callback(timer);
        running = NULL;

        /*
         * 回调可能重装自己：此时 state 已是 ENQUEUED，不能覆盖。
         * 否则回调结束即回到 INACTIVE。
         */
        if (timer->state == CLOCK_TIMER_RUNNING)
            timer->state = CLOCK_TIMER_INACTIVE;
    }
}
