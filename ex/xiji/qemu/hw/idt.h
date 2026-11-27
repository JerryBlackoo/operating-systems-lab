#ifndef XIJI_HW_IDT_H
#define XIJI_HW_IDT_H

#include <stdint.h>

void hw_idt_init(void);

/* 供 trap.S 的兜底入口调用 */
void hw_unexpected_vector(uint32_t vector);

#endif
