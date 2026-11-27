#include "idt.h"
#include "../common/xiji_io.h"
#include <stdint.h>

struct idt_gate {
    uint16_t off_lo;
    uint16_t sel;
    uint8_t zero;
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
extern void hw_isr_spurious(void);

/*
 * 兜底处理：IDT 里所有"没有专门入口"的向量都指向一个小 stub。
 * 原实现把 254 个门全置 0，任何 #GP/#DE 都会立刻三重故障，QEMU 直接复位，
 * 表现成"测试莫名其妙消失"。这里统一指向打印向量号的 stub，至少能定位。
 */
static void idt_set_gate(int vec, uint32_t handler, uint8_t type)
{
    idt[vec] = (struct idt_gate){
        .off_lo = (uint16_t)(handler & 0xffff),
        .sel = 0x08,
        .zero = 0,
        .type = type,
        .off_hi = (uint16_t)(handler >> 16),
    };
}

/*
 * 兜底 stub 的机器码：push <vec>; jmp hw_isr_spurious
 * 这样每个向量都能报出自己的号，而不用写 256 段汇编。
 */
#define STUB_SIZE 16
static uint8_t stub_area[STUB_SIZE * 32] __attribute__((aligned(16)));
static int stub_used;

static uint32_t make_stub(uint8_t vector)
{
    uint8_t *p = &stub_area[stub_used * STUB_SIZE];
    uint32_t addr = (uint32_t)(uintptr_t)p;
    uint32_t target = (uint32_t)(uintptr_t)hw_isr_spurious;
    int i = 0;

    stub_used++;
    p[i++] = 0x6a;              /* push imm8 */
    p[i++] = vector;
    p[i++] = 0xe9;              /* jmp rel32 */
    {
        int32_t rel = (int32_t)(target - (addr + (uint32_t)i + 4));
        p[i++] = (uint8_t)(rel & 0xff);
        p[i++] = (uint8_t)((rel >> 8) & 0xff);
        p[i++] = (uint8_t)((rel >> 16) & 0xff);
        p[i++] = (uint8_t)((rel >> 24) & 0xff);
    }
    return addr;
}

void hw_unexpected_vector(uint32_t vector)
{
    xiji_puts("FATAL: unexpected vector 0x");
    xiji_puthex32(vector);
    xiji_puts(" (halting)\n");
    for (;;)
        __asm__ volatile("cli; hlt");
}

void hw_idt_init(void)
{
    stub_used = 0;
    for (int v = 0; v < 256; v++)
        idt_set_gate(v, make_stub((uint8_t)v), 0x8e);
    idt_set_gate(0x20, (uint32_t)(uintptr_t)hw_isr_irq0, 0x8e);
    idt_set_gate(0x0e, (uint32_t)(uintptr_t)hw_isr_pgfault, 0x8e);

    struct idt_ptr p = {
        .limit = (uint16_t)(sizeof idt - 1),
        .base = (uint32_t)(uintptr_t)idt,
    };
    __asm__ volatile("lidt %0" : : "m"(p));
}
