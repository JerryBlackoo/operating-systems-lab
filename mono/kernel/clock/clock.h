#ifndef KERNEL_CLOCK_CLOCK_H
#define KERNEL_CLOCK_CLOCK_H

#include <stdint.h>

/* 编程第一次时钟比较值；调用后时钟中断源可用，但全局中断尚未打开。 */
void clock_init(void);

/* 命中一次 S-mode 时钟中断：计数、重装比较值，并在前几次打印。 */
void clock_handle_interrupt(void);

uint64_t clock_ticks(void);

#endif
