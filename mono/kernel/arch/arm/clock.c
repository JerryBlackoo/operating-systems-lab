#include "clock/clock.h"

#include "console.h"
#include "proc/proc.h"
#include "syscall/syscall.h"

/* Cortex-A9 私有定时器（SCU 内），时钟为 CPU 频率的一半。
 * Zynq-7010 的 CPU 通常跑 667 MHz，所以定时器约 333 MHz。
 * 10 ms = 3,333,333 tick。 */
#define PRIVATE_TIMER_BASE 0xF8F00600UL
#define TIMER_CLOCK_HZ 3333333UL

#define TIMER_LOAD 0x00
#define TIMER_CONTROL 0x08
#define TIMER_ISR 0x0c

#define TIMER_CTRL_ENABLE 0x01
#define TIMER_CTRL_AUTO_RELOAD 0x02
#define TIMER_CTRL_IRQ 0x04

#define TIMER_LOG_LIMIT 3UL

static uint64_t ticks;

void clock_init(void)
{
    ticks = 0;
    *(volatile uint32_t *)(PRIVATE_TIMER_BASE + TIMER_ISR) = 1;
    *(volatile uint32_t *)(PRIVATE_TIMER_BASE + TIMER_LOAD) = TIMER_CLOCK_HZ;
    *(volatile uint32_t *)(PRIVATE_TIMER_BASE + TIMER_CONTROL) =
        TIMER_CTRL_ENABLE | TIMER_CTRL_AUTO_RELOAD | TIMER_CTRL_IRQ;
    console_write("[clock] private timer armed: 3333333 ticks (10 ms at 333 MHz)\n");
}

void clock_handle_interrupt(void)
{
    *(volatile uint32_t *)(PRIVATE_TIMER_BASE + TIMER_ISR) = 1;
    ticks++;
    sched_tick();
    console_input_poll();

    if (ticks <= TIMER_LOG_LIMIT) {
        console_write("[clock] tick ");
        console_write_dec(ticks);
        console_write("\n");
    }
}

uint64_t clock_ticks(void)
{
    return ticks;
}
