#include "lib/kprintf.h"

#include "arch.h"
#include "console.h"

typedef __builtin_va_list va_list;

static void print_unsigned(uint64_t value, uint32_t base)
{
    char buf[32];
    uint32_t n = 0;

    if (value == 0) {
        console_putchar('0');
        return;
    }
    while (value != 0) {
        uint32_t digit = (uint32_t)(value % base);
        buf[n++] = (char)(digit < 10 ? '0' + digit : 'a' + digit - 10);
        value /= base;
    }
    while (n != 0) {
        console_putchar(buf[--n]);
    }
}

static void vkprintf(const char *fmt, va_list ap)
{
    while (*fmt != '\0') {
        if (*fmt != '%') {
            if (*fmt == '\n') {
                console_putchar('\r');
            }
            console_putchar(*fmt++);
            continue;
        }

        fmt++;
        switch (*fmt) {
        case 'd':
        case 'i': {
            int value = __builtin_va_arg(ap, int);
            if (value < 0) {
                console_putchar('-');
                /* 先转 int64_t，避免 INT_MIN 取反溢出。 */
                print_unsigned((uint64_t)(-(int64_t)value), 10);
            } else {
                print_unsigned((uint64_t)value, 10);
            }
            break;
        }
        case 'u':
            print_unsigned(__builtin_va_arg(ap, unsigned int), 10);
            break;
        case 'x':
        case 'p':
            /* unsigned long 在 ARM 为 32 位、RISC-V 为 64 位，避免 %p 截断指针。 */
            console_write_hex(__builtin_va_arg(ap, unsigned long));
            break;
        case 'c':
            console_putchar((char)__builtin_va_arg(ap, int));
            break;
        case 's': {
            const char *s = __builtin_va_arg(ap, const char *);
            console_write(s != 0 ? s : "(null)");
            break;
        }
        case '%':
            console_putchar('%');
            break;
        default:
            console_putchar('%');
            if (*fmt != '\0') {
                console_putchar(*fmt);
            }
            break;
        }
        if (*fmt != '\0') {
            fmt++;
        }
    }
}

void kprintf(const char *fmt, ...)
{
    va_list ap;
    uint64_t flags = arch_intr_save();

    __builtin_va_start(ap, fmt);
    vkprintf(fmt, ap);
    __builtin_va_end(ap);
    arch_intr_restore(flags);
}

__attribute__((noreturn)) void panic(const char *fmt, ...)
{
    va_list ap;

    arch_intr_disable();
    console_write("[panic] ");
    __builtin_va_start(ap, fmt);
    vkprintf(fmt, ap);
    __builtin_va_end(ap);
    console_write("\n");
    arch_halt();
}
