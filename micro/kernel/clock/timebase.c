#include "clock.h"
#include "platform.h"

#define NSEC_PER_SEC 1000000000ULL

/*
 * boot 时的计数器基准。QEMU 的 mtime 在复位后通常从 0 开始，但不要
 * 依赖这一点：记录首次读数并做模减。
 */
static clock_ticks_t epoch_ticks;
static clock_ns_t epoch_ns;

void clock_timebase_init(void)
{
    epoch_ticks = clock_arch_read_ticks();
    epoch_ns = 0;
    clock_arch_set_compare(CLOCK_DISARM);
}

clock_ticks_t clock_ticks(void)
{
    return clock_arch_read_ticks();
}

uint64_t clock_freq(void)
{
    return CLOCK_TIMEBASE_HZ;
}

/*
 * ticks -> ns。
 * 用“整秒 + 余数”拆分，避免 delta * 1e9 在 10 MHz 下约 30 分钟就溢出；
 * 结果与 mult/shift 仿射变换等价，只是这里用精确除法。
 */
uint64_t clock_ticks_to_ns(uint64_t ticks)
{
    uint64_t delta = ticks - epoch_ticks;
    uint64_t secs = delta / CLOCK_TIMEBASE_HZ;
    uint64_t rem = delta % CLOCK_TIMEBASE_HZ;

    return epoch_ns + secs * NSEC_PER_SEC +
           (rem * NSEC_PER_SEC) / CLOCK_TIMEBASE_HZ;
}

clock_ns_t clock_now(void)
{
    return clock_ticks_to_ns(clock_arch_read_ticks());
}

/*
 * ns -> ticks，向上取整。
 * 比较器语义是 C >= D 时触发；向下取整会让 D 落在 deadline 之前，
 * 造成早发。向上取整保证 phi(D) >= deadline。
 */
uint64_t clock_ns_to_ticks_ceil(uint64_t ns)
{
    uint64_t delta = ns - epoch_ns;
    uint64_t secs = delta / NSEC_PER_SEC;
    uint64_t rem = delta % NSEC_PER_SEC;
    uint64_t ticks = secs * CLOCK_TIMEBASE_HZ +
                     (rem * CLOCK_TIMEBASE_HZ + NSEC_PER_SEC - 1) /
                         NSEC_PER_SEC;

    return epoch_ticks + ticks;
}
