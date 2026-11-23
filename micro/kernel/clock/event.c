#include "platform.h"

#define SSTATUS_SIE (1UL << 1)
#define SBI_EXT_TIME 0x54494D45UL

uint64_t clock_arch_read_ticks(void)
{
    uint64_t ticks;

    __asm__ volatile("rdtime %0" : "=r"(ticks));
    return ticks;
}

void clock_arch_set_compare(uint64_t ticks)
{
    register uint64_t a0 __asm__("a0") = ticks;
    register uint64_t a6 __asm__("a6") = 0;
    register uint64_t a7 __asm__("a7") = SBI_EXT_TIME;

    __asm__ volatile("ecall"
                     : "+r"(a0)
                     : "r"(a6), "r"(a7)
                     : "a1", "memory");
}

uint64_t clock_arch_intr_save(void)
{
    uint64_t old;

    __asm__ volatile("csrrc %0, sstatus, %1"
                     : "=r"(old)
                     : "r"((uint64_t)SSTATUS_SIE)
                     : "memory");
    return old;
}

void clock_arch_intr_restore(uint64_t flags)
{
    if ((flags & SSTATUS_SIE) != 0) {
        __asm__ volatile("csrs sstatus, %0"
                         :
                         : "r"((uint64_t)SSTATUS_SIE)
                         : "memory");
    }
}
