#include "clock/clock.h"

#include "arch/riscv/csr.h"
#include "arch/riscv/sbi.h"
#include "console.h"

/* QEMU virt 的 timebase 为 10 MHz，1 s = 10,000,000 tick。 */
#define TIMER_INTERVAL 10000000UL
#define TIMER_LOG_LIMIT 3UL

static uint64_t ticks;

void clock_init(void)
{
    ticks = 0;
    sbi_set_timer(csr_read(time) + TIMER_INTERVAL);
    console_write("[clock] timer armed, interval 1 s (timebase 10 MHz)\n");
}

void clock_handle_interrupt(void)
{
    ticks++;
    sbi_set_timer(csr_read(time) + TIMER_INTERVAL);

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
