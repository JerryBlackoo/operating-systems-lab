#ifndef KERNEL_CONSOLE_H
#define KERNEL_CONSOLE_H

#include <stdint.h>

/* 初始化控制台硬件；RISC-V 侧为空操作。 */
void console_init(void);

void console_putchar(char c);

/* 非阻塞读一个字符；没有数据返回 -1。 */
int console_try_getchar(void);

/* UART 是否有可读字符（不消费）。 */
int console_rx_ready(void);

void console_write(const char *s);
void console_write_hex(uint64_t value);
void console_write_dec(uint64_t value);

#endif
