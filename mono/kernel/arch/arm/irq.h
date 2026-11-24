#ifndef KERNEL_ARCH_ARM_IRQ_H
#define KERNEL_ARCH_ARM_IRQ_H

#include <stdint.h>

void gic_init(void);
void irq_dispatch(void *frame);

/* kind: 0=undefined, 1=prefetch abort, 2=data abort, 3=SVC, 4=reserved, 5=FIQ */
__attribute__((noreturn)) void arm_fault(const char *name, uint32_t lr,
                                         uint32_t spsr, uint32_t kind);

#endif
