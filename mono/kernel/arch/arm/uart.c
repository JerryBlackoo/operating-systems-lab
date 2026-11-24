#include "arch.h"
#include "console.h"

/* Zynq PS UART0（Cadence UART），MIO14/MIO15，115200-8-N-1。
 * UART 参考时钟按 100 MHz 计算：100 MHz / (124 * (6 + 1)) ≈ 115207。 */
#define UART0_BASE 0xE0000000UL

#define UART_CR 0x00
#define UART_MR 0x04
#define UART_BAUDGEN 0x18
#define UART_CHANNEL_STS 0x2c
#define UART_TX_RX_FIFO 0x30
#define UART_BAUDDIV 0x34

#define UART_CR_RXRST 0x01
#define UART_CR_TXRST 0x02
#define UART_CR_RXEN 0x04
#define UART_CR_TXEN 0x10
#define UART_MR_8N1 0x20
#define UART_SR_RXEMPTY 0x02
#define UART_SR_TXFULL 0x10

static inline void uart_write(uint32_t offset, uint32_t value)
{
    *(volatile uint32_t *)(UART0_BASE + offset) = value;
}

static inline uint32_t uart_read(uint32_t offset)
{
    return *(volatile uint32_t *)(UART0_BASE + offset);
}

void console_init(void)
{
    uart_write(UART_CR, 0);
    uart_write(UART_MR, UART_MR_8N1);
    uart_write(UART_BAUDGEN, 124);
    uart_write(UART_BAUDDIV, 6);
    uart_write(UART_CR, UART_CR_RXRST | UART_CR_TXRST);
    uart_write(UART_CR, UART_CR_RXEN | UART_CR_TXEN);
}

void console_putchar(char c)
{
    while ((uart_read(UART_CHANNEL_STS) & UART_SR_TXFULL) != 0) {
    }
    uart_write(UART_TX_RX_FIFO, (uint32_t)(unsigned char)c);
}

int console_try_getchar(void)
{
    if ((uart_read(UART_CHANNEL_STS) & UART_SR_RXEMPTY) != 0) {
        return -1;
    }
    return (int)(uart_read(UART_TX_RX_FIFO) & 0xff);
}

int console_rx_ready(void)
{
    return (uart_read(UART_CHANNEL_STS) & UART_SR_RXEMPTY) == 0;
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
