#ifndef KERNEL_LIB_KPRINTF_H
#define KERNEL_LIB_KPRINTF_H

#include <stdint.h>

void kprintf(const char *fmt, ...);
__attribute__((noreturn)) void panic(const char *fmt, ...);

#endif
