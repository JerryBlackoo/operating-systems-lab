/*
 * cap_platform.h — 能力模块与机器层的唯一内部契约。
 *
 * 最小模型要求每个转移是原子的。单 hart 上这等价于关中断；
 * SMP 时应替换成「每 CSpace 自旋锁 + 关中断」。
 */
#ifndef MICRO_KERNEL_CAPABILITY_PLATFORM_H
#define MICRO_KERNEL_CAPABILITY_PLATFORM_H

#include <stdint.h>

/* 关中断并返回原 sstatus 中与恢复相关的位。 */
uint64_t cap_arch_intr_save(void);

/* 按 cap_arch_intr_save() 的返回值恢复中断状态。 */
void cap_arch_intr_restore(uint64_t flags);

#endif /* MICRO_KERNEL_CAPABILITY_PLATFORM_H */
