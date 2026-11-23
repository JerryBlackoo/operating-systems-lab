/*
 * hart.h — per-hart 私有状态。
 *
 * 为什么需要它：
 *   trap 入口是汇编，它必须在不查内存表的前提下知道「我的内核栈在哪」。
 *   查表不行——那时 satp 可能已经指向别的地址空间，查表本身就可能踩空。
 *
 * 协议（本模块的第一不变量）：
 *   tp 恒等于 &hart_local[hartid]，永不改变。
 *   当且仅当 hart 在 U-mode 执行时，sscratch 持有该线程的内核栈顶。
 *   S-mode 执行期间 sscratch 的内容无意义（可能是脏值）。
 *
 * 后果（必须接受的限制）：
 *   用户态程序不能用 tp 做 TLS。本教学内核的用户态不需要 TLS。
 */
#ifndef MICRO_ARCH_RISCV_HART_H
#define MICRO_ARCH_RISCV_HART_H

#include "types.h"

struct hart_local {
    u64    hartid;
    u64    kernel_sp_top;   /* 当前线程内核栈顶；sscratch 的权威副本 */
    void  *current;         /* 不透明：将来指向 sched 的 thread，arch 不解释它 */
};

static inline void hart_set(struct hart_local *h)
{
    __asm__ volatile("mv tp, %0" : : "r"(h) : "memory");
}

static inline struct hart_local *hart_local_get(void)
{
    struct hart_local *h;
    __asm__ volatile("mv %0, tp" : "=r"(h) : );
    return h;
}

#endif /* MICRO_ARCH_RISCV_HART_H */
