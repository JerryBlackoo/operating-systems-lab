/*
 * sbi.h — M-mode 服务的唯一出口。
 *
 * S-mode 够不着 mtimecmp / PMP / 真正断电，只能 ecall 到 OpenSBI。
 * ecall 从 S-mode 产生 scause=9，被 M-mode 截获，永远不会进我们的 trap_handler。
 * 如果真进了 —— 说明 OpenSBI 没加载，直接 panic。
 */
#ifndef MICRO_ARCH_RISCV_SBI_H
#define MICRO_ARCH_RISCV_SBI_H

#include "types.h"

struct sbiret {
    long error;
    long value;
};

struct sbiret sbi_call(long eid, long fid,
                       long a0, long a1, long a2, long a3);

void sbi_putchar(int ch);
void sbi_set_timer(u64 stime);
void sbi_shutdown(void) __attribute__((noreturn));

#endif /* MICRO_ARCH_RISCV_SBI_H */
