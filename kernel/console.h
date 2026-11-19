#ifndef KERNEL_CONSOLE_H
#define KERNEL_CONSOLE_H

#include <stdint.h>

void console_putchar(char c);
void console_write(const char *s);
void console_write_hex(uint64_t value);
void console_write_dec(uint64_t value);

#endif
