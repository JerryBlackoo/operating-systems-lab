#ifndef KERNEL_ARCH_RISCV_CSR_H
#define KERNEL_ARCH_RISCV_CSR_H

#include <stdint.h>

/* sstatus / sie 中本次用到的最小位定义。 */
#define SSTATUS_SIE (1UL << 1)
#define SIE_STIE (1UL << 5)

#define csr_read(name)                                                         \
    ({                                                                         \
        uint64_t __csr_value;                                                  \
        __asm__ volatile("csrr %0, " #name : "=r"(__csr_value));               \
        __csr_value;                                                           \
    })

#define csr_write(name, value)                                                 \
    __asm__ volatile("csrw " #name ", %0" ::"r"((uint64_t)(value)))

#define csr_set(name, value)                                                   \
    __asm__ volatile("csrs " #name ", %0" ::"r"((uint64_t)(value)))

#endif
