#include "arch/riscv/sbi.h"

#define SBI_EXT_TIME 0x54494D45UL
#define SBI_EXT_SRST 0x53525354UL
#define SBI_SRST_TYPE_SHUTDOWN 0UL
#define SBI_SRST_REASON_NONE 0UL

void sbi_set_timer(uint64_t stime)
{
    register uint64_t a0 __asm__("a0") = stime;
    register uint64_t a6 __asm__("a6") = 0;
    register uint64_t a7 __asm__("a7") = SBI_EXT_TIME;

    __asm__ volatile("ecall" : "+r"(a0) : "r"(a6), "r"(a7) : "memory");
}

void sbi_shutdown(void)
{
    register uint64_t a0 __asm__("a0") = SBI_SRST_TYPE_SHUTDOWN;
    register uint64_t a1 __asm__("a1") = SBI_SRST_REASON_NONE;
    register uint64_t a6 __asm__("a6") = 0;
    register uint64_t a7 __asm__("a7") = SBI_EXT_SRST;

    __asm__ volatile("ecall" : "+r"(a0) : "r"(a1), "r"(a6), "r"(a7) : "memory");
}
