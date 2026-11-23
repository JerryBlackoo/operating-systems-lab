/*
 * sbi.c — legacy SBI 封装。
 *
 * 为什么用 legacy 而不是 v0.2 extension：
 *   legacy 调用最简单（只有 EID，FID 固定 0），且 OpenSBI 仍完整支持。
 *   代价是没有返回值语义（失败就是什么都没发生）。
 *   在 10ms 级的中断频率下，ecall 的开销可以忽略。
 */
#include "sbi.h"
#include "platform.h"

struct sbiret sbi_call(long eid, long fid,
                       long a0, long a1, long a2, long a3)
{
    register long r_a0 __asm__("a0") = a0;
    register long r_a1 __asm__("a1") = a1;
    register long r_a2 __asm__("a2") = a2;
    register long r_a3 __asm__("a3") = a3;
    register long r_a6 __asm__("a6") = fid;
    register long r_a7 __asm__("a7") = eid;

    __asm__ volatile("ecall"
                     : "+r"(r_a0), "+r"(r_a1)
                     : "r"(r_a2), "r"(r_a3), "r"(r_a6), "r"(r_a7)
                     : "memory");

    return (struct sbiret){ r_a0, r_a1 };
}

void sbi_putchar(int ch)
{
    sbi_call(SBI_EXT_LEGACY_PUTCHAR, 0, ch, 0, 0, 0);
}

void sbi_set_timer(u64 stime)
{
    sbi_call(SBI_EXT_LEGACY_SET_TIMER, 0, (long)stime, 0, 0, 0);
}

void sbi_shutdown(void)
{
    /* SRST extension：a0=0 关机，a1=0 无附加原因 */
    sbi_call(SBI_EXT_SRST, 0, 0, 0, 0, 0);

    /* SRST 不可用时的退路：QEMU virt 的 test 设备。 */
    *(volatile u32 *)QEMU_TEST_BASE = QEMU_POWEROFF;

    for (;;)
        __asm__ volatile("wfi");
}
