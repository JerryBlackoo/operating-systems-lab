#include "idt.h"
#include <stddef.h>
#include <stdint.h>

struct idt_gate {
    uint16_t off_lo;
    uint16_t sel;
    uint8_t ist;
    uint8_t type;
    uint16_t off_hi;
} __attribute__((packed));

struct idt_ptr {
    uint16_t limit;
    uint32_t base;
} __attribute__((packed));

static struct idt_gate idt[256];

extern void hw_isr_irq0(void);
extern void hw_isr_pgfault(void);

static void idt_set(int vec, void (*handler)(void))
{
    uint32_t addr = (uint32_t)(uintptr_t)handler;
    idt[vec] = (struct idt_gate){
        .off_lo = (uint16_t)(addr & 0xffff),
        .sel = 0x08,
        .ist = 0,
        .type = 0x8e,
        .off_hi = (uint16_t)(addr >> 16),
    };
}

void hw_idt_init(void)
{
    for (int i = 0; i < 256; i++)
        idt[i] = (struct idt_gate){ 0 };
    idt_set(0x20, hw_isr_irq0);
    idt_set(0x0e, hw_isr_pgfault);
    struct idt_ptr p = {
        .limit = (uint16_t)(sizeof idt - 1),
        .base = (uint32_t)(uintptr_t)idt,
    };
    __asm__ volatile("lidt %0" : : "m"(p));
}