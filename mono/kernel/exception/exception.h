#ifndef KERNEL_EXCEPTION_EXCEPTION_H
#define KERNEL_EXCEPTION_EXCEPTION_H

#include "arch/riscv/trap.h"

/* 打印异常原因与现场，然后通过 SBI 关机；不会返回。 */
__attribute__((noreturn)) void exception_handle(uint64_t cause, uint64_t epc,
                                                uint64_t tval,
                                                const struct trap_frame *tf);

#endif
