#include "arch.h"
#include "console.h"

/* QEMU virt 机器上的 NS16550A UART。 */
#define UART0_BASE 0x10000000UL
#define UART_RBR (UART0_BASE + 0x00)
#define UART_THR (UART0_BASE + 0x00)
#define UART_LSR (UART0_BASE + 0x05)

#define UART_LSR_DR (1u << 0)
#define UART_LSR_THRE (1u << 5)

void console_init(void)
{
}

void console_putchar(char c)
{
    volatile unsigned char *lsr = (volatile unsigned char *)UART_LSR;
    volatile unsigned char *thr = (volatile unsigned char *)UART_THR;

    while ((*lsr & UART_LSR_THRE) == 0) {
    }
    *thr = (unsigned char)c;
}

int console_try_getchar(void)
{
    volatile unsigned char *lsr = (volatile unsigned char *)UART_LSR;
    volatile unsigned char *rbr = (volatile unsigned char *)UART_RBR;

    if ((*lsr & UART_LSR_DR) == 0) {
        return -1;
    }
    return (int)(*rbr);
}

int console_rx_ready(void)
{
    volatile unsigned char *lsr = (volatile unsigned char *)UART_LSR;

    return (*lsr & UART_LSR_DR) != 0;
}

void console_write(const char *s)
{
    uint64_t flags = arch_intr_save();

    while (*s != '\0') {
        if (*s == '\n') {
            console_putchar('\r');
        }
        console_putchar(*s);
        s++;
    }
    arch_intr_restore(flags);
}

void console_write_hex(uint64_t value)
{
    static const char digits[] = "0123456789abcdef";
    uint64_t flags = arch_intr_save();
    int shift = 60;

    console_write("0x");
    while (shift > 0 && ((value >> shift) & 0xf) == 0) {
        shift -= 4;
    }
    for (; shift >= 0; shift -= 4) {
        console_putchar(digits[(value >> shift) & 0xf]);
    }
    arch_intr_restore(flags);
}

void console_write_dec(uint64_t value)
{
    char buffer[20];
    uint64_t flags = arch_intr_save();
    int index = 0;

    if (value == 0) {
        console_putchar('0');
        arch_intr_restore(flags);
        return;
    }
    while (value != 0) {
        buffer[index++] = (char)('0' + (value % 10));
        value /= 10;
    }
    while (index > 0) {
        console_putchar(buffer[--index]);
    }
    arch_intr_restore(flags);
}
