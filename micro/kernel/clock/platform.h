#ifndef KERNEL_CLOCK_PLATFORM_H
#define KERNEL_CLOCK_PLATFORM_H

#include <stdint.h>

/* 写入比较器即撤防。 */
#define CLOCK_DISARM UINT64_MAX

/* 以下四个函数由 event.c 实现；宿主测试可提供替身。 */

/* 读取自由运行计数器（QEMU/RISC-V：rdtime）。 */
uint64_t clock_arch_read_ticks(void);

/* 写入绝对比较值；CLOCK_DISARM 表示撤防。 */
void clock_arch_set_compare(uint64_t ticks);

/* 关中断并返回原 sstatus 中与恢复相关的位。 */
uint64_t clock_arch_intr_save(void);

/* 按 clock_arch_intr_save() 的返回值恢复中断状态。 */
void clock_arch_intr_restore(uint64_t flags);

/* 模块内部：时基初始化与换算。 */
void clock_timebase_init(void);
uint64_t clock_ticks_to_ns(uint64_t ticks);
uint64_t clock_ns_to_ticks_ceil(uint64_t ns);

#endif
