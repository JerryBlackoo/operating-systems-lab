#ifndef XIJI_IO_H
#define XIJI_IO_H

#ifdef XIJI_FREESTANDING

static inline void xiji_putc(char ch)
{
    __asm__ volatile("outb %0, $0xe9" : : "a"((unsigned char)ch));
}

static inline void xiji_puts(const char *s)
{
    while (s && *s)
        xiji_putc(*s++);
}

#else

#include <stdio.h>

static inline void xiji_putc(char ch)
{
    fputc(ch, stdout);
}

static inline void xiji_puts(const char *s)
{
    fputs(s, stdout);
}

#endif

#endif