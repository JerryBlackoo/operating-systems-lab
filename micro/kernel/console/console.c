/*
 * console.c — UART0 (16550) MMIO + PLIC 行输入（M4）。
 *
 * panic / 早期 boot 仍用 SBI；MMIO 初始化完成后由 demo.c 切换输出路径。
 */
#include "console.h"

#include "platform.h"
#include "irq.h"
#include "sched.h"
#include "sbi.h"

enum {
    UART_RBR = 0,
    UART_THR = 0,
    UART_IER = 1,
    UART_LCR = 3,
    UART_LSR = 5,
    LCR_DLAB = 0x80U,
    LCR_8N1 = 0x03U,
    LSR_DR = 0x01U,
    LSR_THRE = 0x20U,
    IER_RX = 0x01U,
};

#define CONSOLE_QUEUE_LINES 8

static volatile uint8_t *uart_base(void)
{
    return (volatile uint8_t *)(uintptr_t)UART0_BASE;
}

static uint8_t uart_read(unsigned reg)
{
    return uart_base()[reg];
}

static void uart_write(unsigned reg, uint8_t v)
{
    uart_base()[reg] = v;
}

static bool mmio_on;
static unsigned saved_divisor;
static char line_buf[CONSOLE_LINE_MAX];
static unsigned line_len;
static char line_queue[CONSOLE_QUEUE_LINES][CONSOLE_LINE_MAX];
static size_t line_queue_len[CONSOLE_QUEUE_LINES];
static unsigned line_queue_head;
static unsigned line_queue_count;
static struct waitq line_waitq;
static uint64_t ext_irqs;
static uint64_t timer_ticks;

static void mmio_putc(char c)
{
    while ((uart_read(UART_LSR) & LSR_THRE) == 0) {
    }
    uart_write(UART_THR, (uint8_t)c);
}

static void echo_backspace(void)
{
    if (line_len == 0) {
        return;
    }
    line_len--;
    if (mmio_on) {
        mmio_putc('\b');
        mmio_putc(' ');
        mmio_putc('\b');
    }
}

static void queue_push(const char *line, size_t len)
{
    unsigned slot;

    if (len > CONSOLE_LINE_MAX) {
        len = CONSOLE_LINE_MAX;
    }
    if (line_queue_count == CONSOLE_QUEUE_LINES) {
        line_queue_head = (line_queue_head + 1U) % CONSOLE_QUEUE_LINES;
        line_queue_count--;
    }
    slot = (line_queue_head + line_queue_count) % CONSOLE_QUEUE_LINES;
    for (size_t i = 0; i < len; i++) {
        line_queue[slot][i] = line[i];
    }
    line_queue_len[slot] = len;
    line_queue_count++;
}

bool console_read_line(char *dst, size_t max, size_t *n_out)
{
    unsigned slot;
    size_t len;
    uint64_t flags;
    bool ok = false;

    if (dst == NULL || n_out == NULL || max == 0) {
        return false;
    }
    /*
     * 队列由 UART IRQ（flush_line）和生产/消费两侧共同修改。这里不能
     * 依赖“调用点恰好 SIE=0”：阻塞版 console_readline 与将来可能在
     * 开中断上下文里的调用都必须安全。
     */
    flags = sched_lock();
    if (line_queue_count != 0) {
        slot = line_queue_head;
        len = line_queue_len[slot];
        if (len > max) {
            len = max;
        }
        for (size_t i = 0; i < len; i++) {
            dst[i] = line_queue[slot][i];
        }
        line_queue_head = (line_queue_head + 1U) % CONSOLE_QUEUE_LINES;
        line_queue_count--;
        *n_out = len;
        ok = true;
    }
    sched_unlock(flags);
    return ok;
}

static void flush_line(void)
{
    char msg[CONSOLE_LINE_MAX + 24];
    char *p = msg;
    unsigned i;
    uint64_t flags;

    if (mmio_on) {
        mmio_putc('\r');
        mmio_putc('\n');
    }

    flags = sched_lock();
    queue_push(line_buf, line_len);
    {
        struct thread *t;

        while ((t = waitq_pop(&line_waitq)) != 0) {
            t->ipc_status = 0;
            thread_wake_locked(t);
        }
    }
    sched_unlock(flags);

    p[0] = '[';
    p[1] = 'c';
    p[2] = 'o';
    p[3] = 'n';
    p[4] = 's';
    p[5] = 'o';
    p[6] = 'l';
    p[7] = 'e';
    p[8] = ']';
    p[9] = ' ';
    p[10] = 'l';
    p[11] = 'i';
    p[12] = 'n';
    p[13] = 'e';
    p[14] = '=';
    p[15] = '"';
    p += 16;
    for (i = 0; i < line_len && i < CONSOLE_LINE_MAX; i++) {
        *p++ = line_buf[i];
    }
    *p++ = '"';
    *p++ = '\n';
    *p = '\0';
    console_puts(msg);

    line_len = 0;
    (void)saved_divisor;
}

static void console_uart_notify(struct irq_sink *self, int line)
{
    (void)self;

    if (line != (int)UART0_IRQ) {
        (void)irq_ack(line);
        return;
    }

    ext_irqs++;

    while ((uart_read(UART_LSR) & LSR_DR) != 0) {
        uint8_t ch = uart_read(UART_RBR);

        if (ch == '\r' || ch == '\n') {
            {
                char banner[48];
                char *q = banner;
                unsigned n = 0;

                *q++ = '[';
                *q++ = 'c';
                *q++ = 'o';
                *q++ = 'n';
                *q++ = 's';
                *q++ = 'o';
                *q++ = 'l';
                *q++ = 'e';
                *q++ = ']';
                *q++ = ' ';
                *q++ = 'i';
                *q++ = 'r';
                *q++ = 'q';
                *q++ = ':';
                *q++ = ' ';
                *q++ = 'l';
                *q++ = 'i';
                *q++ = 'n';
                *q++ = 'e';
                *q++ = '=';
                n = (unsigned)UART0_IRQ;
                if (n >= 10U) {
                    *q++ = (char)('0' + n / 10U);
                    n %= 10U;
                }
                *q++ = (char)('0' + n);
                *q++ = '\n';
                *q = '\0';
                console_puts(banner);
            }
            flush_line();
            break;
        }
        if (ch == '\b' || ch == 0x7fU) {
            echo_backspace();
            continue;
        }
        if (ch >= 0x20U && ch < 0x7fU && line_len + 1U < CONSOLE_LINE_MAX) {
            line_buf[line_len++] = (char)ch;
            if (mmio_on) {
                mmio_putc((char)ch);
            }
        }
    }

    (void)irq_ack((int)UART0_IRQ);
}

static struct irq_sink console_sink = {
    .notify = console_uart_notify,
    .ctx = NULL,
};

void console_mmio_init(unsigned divisor)
{
    saved_divisor = divisor;
    waitq_init(&line_waitq);
    line_queue_head = 0;
    line_queue_count = 0;

    uart_write(UART_IER, 0U);
    uart_write(UART_LCR, LCR_DLAB);
    uart_write(UART_RBR, (uint8_t)(divisor & 0xffU));
    uart_write(UART_IER, (uint8_t)((divisor >> 8) & 0xffU));
    uart_write(UART_LCR, LCR_8N1);
    uart_write(UART_IER, IER_RX);
    line_len = 0;
    mmio_on = false;
}

void console_mmio_enable(bool on)
{
    mmio_on = on;
}

bool console_mmio_active(void)
{
    return mmio_on;
}

void console_putc(char c)
{
    if (mmio_on) {
        mmio_putc(c);
        return;
    }
    sbi_putchar(c);
}

void console_puts(const char *s)
{
    if (s == NULL) {
        return;
    }
    while (*s != '\0') {
        console_putc(*s++);
    }
}

int console_bind_uart_irq(void)
{
    return irq_bind((int)UART0_IRQ, &console_sink) == IRQ_OK ? 0 : -1;
}

uint64_t console_ext_irqs(void)
{
    return ext_irqs;
}

uint64_t console_timer_ticks(void)
{
    return timer_ticks;
}

void console_note_timer_tick(void)
{
    timer_ticks++;
}

int console_readline(char *dst, size_t max)
{
    struct thread *self = sched_current();
    size_t n;
    uint64_t flags;

    if (dst == 0 || max == 0 || self == 0) {
        return -1;
    }

    for (;;) {
        flags = sched_lock();
        if (line_queue_count > 0) {
            unsigned slot = line_queue_head;

            n = line_queue_len[slot];
            if (n > max - 1U) {
                n = max - 1U;
            }
            for (size_t i = 0; i < n; i++) {
                dst[i] = line_queue[slot][i];
            }
            dst[n] = '\0';
            line_queue_head = (line_queue_head + 1U) % CONSOLE_QUEUE_LINES;
            line_queue_count--;
            sched_unlock(flags);
            return (int)n;
        }
        if (self->state != THREAD_RUNNING) {
            sched_unlock(flags);
            return -1;
        }
        thread_block_locked(&line_waitq);
        sched_unlock(flags);
        sched();
    }
}