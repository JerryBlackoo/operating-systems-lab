#ifndef XIJI_HW_PIC_PIT_H
#define XIJI_HW_PIC_PIT_H

#include <stdint.h>

extern volatile uint32_t hw_pit_ticks;

void hw_pic_init(void);
void hw_pit_init(void);
void hw_irq_enable(void);
void hw_pit_on_irq0(void);

#endif