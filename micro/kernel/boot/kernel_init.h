#ifndef MICRO_BOOT_KERNEL_INIT_H
#define MICRO_BOOT_KERNEL_INIT_H

#include "bootinfo.h"
#include "memory.h"

struct as;

/* M1：自举清单结束后进入真实帧池 + idle，不再关机（仅 MICRO_POST_BOOT_RESIDENT）。 */
void kernel_enter_resident(const struct bootinfo *bi) __attribute__((noreturn));

struct as *kernel_resident_as(void);

/* M4：外部中断里临时切到内核 AS，以便访问 MMIO。 */
void kernel_irq_enter_devices(void);
void kernel_irq_leave_devices(void);

#endif /* MICRO_BOOT_KERNEL_INIT_H */