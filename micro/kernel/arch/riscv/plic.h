#ifndef MICRO_ARCH_RISCV_PLIC_H
#define MICRO_ARCH_RISCV_PLIC_H

#include "interrupt/irq.h"

/* QEMU virt hart0 S-mode context (M=0, S=1). */
#define PLIC_CTX_S 1

void plic_hw_init(void);
const struct irq_port *plic_irq_port(void);

#endif /* MICRO_ARCH_RISCV_PLIC_H */