#ifndef MICRO_KERNEL_CONSOLE_H
#define MICRO_KERNEL_CONSOLE_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define CONSOLE_LINE_MAX 80

void console_mmio_init(unsigned divisor);
void console_mmio_enable(bool on);
bool console_mmio_active(void);

void console_putc(char c);
void console_puts(const char *s);

int console_bind_uart_irq(void);

uint64_t console_timer_ticks(void);
uint64_t console_ext_irqs(void);
void console_note_timer_tick(void);

/* 阻塞直到 UART 收到完整一行（不含换行）；返回拷贝字节数，无行则 -1。 */
int console_readline(char *dst, size_t max);

/* 非阻塞取出一整行（不含换行）；false 表示当前没有完整行。 */
bool console_read_line(char *dst, size_t max, size_t *n_out);

#endif /* MICRO_KERNEL_CONSOLE_H */