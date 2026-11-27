#ifndef XIJI_IO_H
#define XIJI_IO_H

/*
 * 宿主（hosted C）与 QEMU 内核（freestanding）共用的最小输出层。
 * QEMU 侧走 0xe9 debugcon，host 侧走 stdout，两边断言代码完全相同。
 */

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

static inline void xiji_puthex32(unsigned int v)
{
    for (int shift = 28; shift >= 0; shift -= 4) {
        unsigned int d = (v >> shift) & 0xfu;
        xiji_putc((char)(d < 10 ? '0' + d : 'a' + d - 10));
    }
}

static inline void xiji_putint(int v)
{
    char buf[12];
    int n = 0;
    unsigned int u;

    if (v < 0) {
        xiji_putc('-');
        u = (unsigned int)(-(long)v);
    } else {
        u = (unsigned int)v;
    }
    do {
        buf[n++] = (char)('0' + u % 10u);
        u /= 10u;
    } while (u && n < (int)sizeof buf);
    while (n--)
        xiji_putc(buf[n]);
}

#endif
