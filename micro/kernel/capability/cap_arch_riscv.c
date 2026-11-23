/*
 * cap_arch_riscv.c — 能力模块的 RISC-V 中断开关。
 *
 * 与 kernel/clock/event.c 一样，这里只碰 sstatus.SIE，不引入 arch 头文件，
 * 以便本目录的 Makefile 可以独立做交叉编译检查。
 */
#include "cap_platform.h"

#define SSTATUS_SIE (1UL << 1)

uint64_t cap_arch_intr_save(void)
{
    uint64_t old;

    __asm__ volatile("csrrc %0, sstatus, %1"
                     : "=r"(old)
                     : "r"((uint64_t)SSTATUS_SIE)
                     : "memory");
    return old;
}

void cap_arch_intr_restore(uint64_t flags)
{
    if ((flags & SSTATUS_SIE) != 0) {
        __asm__ volatile("csrs sstatus, %0"
                         :
                         : "r"((uint64_t)SSTATUS_SIE)
                         : "memory");
    }
}
