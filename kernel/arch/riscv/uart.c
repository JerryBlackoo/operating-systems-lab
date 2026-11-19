#include "console.h"

/* QEMU virt 机器上的 NS16550A UART。 */
#define UART0_BASE 0x10000000UL
#define UART_THR (UART0_BASE + 0x00)
#define UART_LSR (UART0_BASE + 0x05)

#define UART_LSR_THRE (1u << 5)

void console_putchar(char c)
{
    volatile unsigned char *lsr = (volatile unsigned char *)UART_LSR;
    volatile unsigned char *thr = (volatile unsigned char *)UART_THR;

    while ((*lsr & UART_LSR_THRE) == 0) {
    }
    *thr = (unsigned char)c;
}

void console_write(const char *s)
{
    while (*s != '\0') {
        if (*s == '\n') {
            console_putchar('\r');
        }
        console_putchar(*s);
        s++;
    }
}

void console_write_hex(uint64_t value)
{
    static const char digits[] = "0123456789abcdef";
    int shift = 60;

    console_write("0x");
    while (shift > 0 && ((value >> shift) & 0xf) == 0) {
        shift -= 4;
    }
    for (; shift >= 0; shift -= 4) {
        console_putchar(digits[(value >> shift) & 0xf]);
    }
}

void console_write_dec(uint64_t value)
{
    char buffer[20];
    int index = 0;

    if (value == 0) {
        console_putchar('0');
        return;
    }
    while (value != 0) {
        buffer[index++] = (char)('0' + (value % 10));
        value /= 10;
    }
    while (index > 0) {
        console_putchar(buffer[--index]);
    }
}
