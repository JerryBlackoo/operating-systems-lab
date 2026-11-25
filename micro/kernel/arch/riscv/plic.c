/*
 * plic.c — QEMU virt PLIC MMIO，实现 irq_port。
 *
 * 地址与 kernel/interrupt/README.md §5.1 一致（DTB dump 复核）：
 *   enable ctx1 word0 : PLIC_BASE + 0x2080
 *   claim/complete    : PLIC_BASE + 0x201004
 */
#include "plic.h"

#include "csr.h"
#include "platform.h"
#include "sbi.h"
#include "clock/clock.h"

#define PLIC_PRIO(id)   (PLIC_BASE + 4UL * (unsigned)(id))
#define PLIC_ENABLE(ctx, word) (PLIC_BASE + 0x2000UL + (unsigned)(ctx) * 0x80UL + (unsigned)(word) * 4UL)
#define PLIC_CTX(ctx)   (PLIC_BASE + 0x200000UL + (unsigned)(ctx) * 0x1000UL)
#define PLIC_THRESHOLD(ctx) (PLIC_CTX(ctx) + 0x00UL)
#define PLIC_CLAIM(ctx)     (PLIC_CTX(ctx) + 0x04UL)

static volatile uint32_t *plic_reg(uintptr_t pa)
{
    return (volatile uint32_t *)(uintptr_t)pa;
}

static uint32_t plic_read(uintptr_t pa)
{
    return *plic_reg(pa);
}

static void plic_write(uintptr_t pa, uint32_t v)
{
    *plic_reg(pa) = v;
}

static void plic_enable_set(int line)
{
    unsigned word = (unsigned)line / 32U;
    unsigned bit = (unsigned)line % 32U;
    uintptr_t addr = PLIC_ENABLE(PLIC_CTX_S, word);
    uint32_t cur = plic_read(addr);

    cur |= 1U << bit;
    plic_write(addr, cur);
}

static void plic_enable_clear(int line)
{
    unsigned word = (unsigned)line / 32U;
    unsigned bit = (unsigned)line % 32U;
    uintptr_t addr = PLIC_ENABLE(PLIC_CTX_S, word);
    uint32_t cur = plic_read(addr);

    cur &= ~(1U << bit);
    plic_write(addr, cur);
}

static int plic_claim(void)
{
    uint32_t id = plic_read(PLIC_CLAIM(PLIC_CTX_S));

    return id == 0U ? IRQ_NONE : (int)id;
}

static void plic_complete_line(int line)
{
    if (line == IRQ_TIMER) {
        return;
    }
    plic_write(PLIC_CLAIM(PLIC_CTX_S), (uint32_t)line);
}

static bool timer_masked = true;

static int port_claim(void)
{
    return plic_claim();
}

static void port_complete(int line)
{
    if (line == IRQ_TIMER) {
        if (!timer_masked) {
            (void)sbi_set_timer(clock_now() + 1000000ULL);
        } else {
            (void)sbi_set_timer(UINT64_MAX);
        }
        return;
    }
    plic_complete_line(line);
}

static void port_mask(int line)
{
    if (line == IRQ_TIMER) {
        timer_masked = true;
        csr_clear(CSR_SIE, SIE_STIE);
        (void)sbi_set_timer(UINT64_MAX);
        return;
    }
    if (line > 0 && line < IRQ_LINE_MAX) {
        plic_enable_clear(line);
    }
}

static void port_unmask(int line)
{
    if (line == IRQ_TIMER) {
        timer_masked = false;
        (void)sbi_set_timer(clock_now() + 1000000ULL);
        csr_set(CSR_SIE, SIE_STIE);
        return;
    }
    if (line > 0 && line < IRQ_LINE_MAX) {
        if (plic_read(PLIC_PRIO(line)) == 0U) {
            plic_write(PLIC_PRIO(line), 1U);
        }
        plic_enable_set(line);
    }
}

static unsigned long port_intr_save(void)
{
    unsigned long s = csr_read(CSR_SSTATUS);

    csr_clear(CSR_SSTATUS, SSTATUS_SIE);
    return s & SSTATUS_SIE;
}

static void port_intr_restore(unsigned long flags)
{
    if (flags) {
        csr_set(CSR_SSTATUS, SSTATUS_SIE);
    } else {
        csr_clear(CSR_SSTATUS, SSTATUS_SIE);
    }
}

static const struct irq_port g_plic_port = {
    .claim = port_claim,
    .complete = port_complete,
    .mask = port_mask,
    .unmask = port_unmask,
    .intr_save = port_intr_save,
    .intr_restore = port_intr_restore,
};

void plic_hw_init(void)
{
    unsigned i;

    for (i = 1; i <= (unsigned)PLIC_NDEV; i++) {
        plic_write(PLIC_PRIO(i), 0U);
    }
    plic_write(PLIC_THRESHOLD(PLIC_CTX_S), 0U);
    timer_masked = true;
}

const struct irq_port *plic_irq_port(void)
{
    return &g_plic_port;
}