#include "clock.h"
#include "csr.h"
#include "demo.h"
#include "trap.h"

struct fired {
    volatile unsigned count;
    unsigned order[2];
};

struct event {
    struct fired *fired;
    unsigned id;
};

static void record(struct clock_timer *timer)
{
    struct event *event = timer->arg;
    unsigned index = event->fired->count;
    CHECK(index < 2 && event->id != 0 && clock_now() >= timer->deadline);
    event->fired->order[index] = event->id;
    event->fired->count = index + 1;
}

static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    if (frame->scause != (SCAUSE_IRQ | IRQ_S_TIMER))
        return NULL;
    clock_handle_interrupt();
    return frame;
}

static void run(void)
{
    struct fired fired = {0};
    struct event one = {&fired, 1}, two = {&fired, 2}, canceled = {&fired, 0};
    struct clock_timer first, second, removed;
    u64 flags = irq_save();
    u64 sie = csr_read(CSR_SIE);

    clock_init();
    clock_timer_init(&first, record, &one);
    clock_timer_init(&second, record, &two);
    clock_timer_init(&removed, record, &canceled);
    clock_ns_t now = clock_now();
    CHECK(clock_timer_arm(&removed, now + 1000000));
    CHECK(clock_timer_arm(&first, now + 2000000));
    CHECK(clock_timer_arm(&second, now + 2000000));
    CHECK(clock_timer_cancel(&removed) && !clock_timer_is_pending(&removed));

    clock_ticks_t start = clock_ticks();
    csr_set(CSR_SIE, SIE_STIE);
    irq_enable();
    while (fired.count < 2 && clock_ticks() - start < clock_freq())
        __asm__ volatile("nop" ::: "memory");
    irq_disable();
    csr_write(CSR_SIE, sie);
    irq_restore(flags);
    CHECK(fired.count == 2 && fired.order[0] == 1 && fired.order[1] == 2);
    CHECK(!clock_timer_is_pending(&first) && !clock_timer_is_pending(&second));
    demo_log("timer interrupts: canceled deadline and equal-deadline FIFO");
}

DEMO_MODULE(clock, false, run, handle_trap);
