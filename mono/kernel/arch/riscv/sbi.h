#ifndef KERNEL_ARCH_RISCV_SBI_H
#define KERNEL_ARCH_RISCV_SBI_H

#include <stdint.h>

/* 通过 SBI 的 TIME 扩展设置下一次时钟比较值。 */
void sbi_set_timer(uint64_t stime);

/* 通过 SBI 的 SRST 扩展关机；正常情况下不返回。 */
void sbi_shutdown(void);

#endif
