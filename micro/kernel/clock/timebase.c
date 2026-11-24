#include "clock.h"
#include "platform.h"

#define NSEC_PER_SEC 1000000000ULL

/*
 * boot 时的计数器基准。QEMU 的 mtime 在复位后通常从 0 开始，但不要
 * 依赖这一点：记录首次读数并做模减。
 */
static clock_ticks_t epoch_ticks;
static clock_ns_t epoch_ns;
static bool timebase_ready;

void clock_timebase_init(void)
{
    /*
     * epoch 是全局时间原点，只有第一个 hart 能建立它。
     * 后续 hart 或后续 clock_init() 只撤防自己的比较器。
     */
    if (!timebase_ready) {
        epoch_ticks = clock_arch_read_ticks();
        epoch_ns = 0;
        timebase_ready = true;
    }
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

    if (secs > (UINT64_MAX - epoch_ns) / NSEC_PER_SEC)
        return UINT64_MAX;

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
    uint64_t delta;
    uint64_t secs;
    uint64_t rem;
    uint64_t ticks;

    /* 早于 epoch 的 deadline 立即到期。 */
    if (ns <= epoch_ns)
        return epoch_ticks;

    delta = ns - epoch_ns;
    secs = delta / NSEC_PER_SEC;
    rem = delta % NSEC_PER_SEC;

    /* 换算结果装不下 64 位 ticks 时饱和到撤防值，语义是“永不触发”。 */
    if (secs > (UINT64_MAX - epoch_ticks) / CLOCK_TIMEBASE_HZ)
        return CLOCK_DISARM;

    ticks = secs * CLOCK_TIMEBASE_HZ +
            (rem * CLOCK_TIMEBASE_HZ + NSEC_PER_SEC - 1) /
                NSEC_PER_SEC;

    if (ticks > UINT64_MAX - epoch_ticks)
        return CLOCK_DISARM;

    return epoch_ticks + ticks;
}
