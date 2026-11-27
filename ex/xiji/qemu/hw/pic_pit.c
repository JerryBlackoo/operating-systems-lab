#include "pic_pit.h"
#include "ports.h"

volatile uint32_t hw_pit_ticks;

static void io_wait(void)
{
    outb(0, 0x80);
}

void hw_pic_init(void)
{
    outb(0x11, 0x20);
    outb(0x11, 0xa0);
    io_wait();

    outb(0x20, 0x21);
    outb(0x28, 0xa1);
    io_wait();

    outb(0x04, 0x21);
    outb(0x02, 0xa1);
    io_wait();

    outb(0x01, 0x21);
    outb(0x01, 0xa1);
    io_wait();

    outb(0xfe, 0x21);
    outb(0xff, 0xa1);
}

void hw_pit_init(void)
{
    /* 通道 0，模式 3，约 100Hz */
    const uint16_t divisor = 11932;
    outb(0x36, 0x43);
    outb(divisor & 0xff, 0x40);
    outb(divisor >> 8, 0x40);
}

void hw_irq_enable(void)
{
    __asm__ volatile("sti");
}

void hw_pit_on_irq0(void)
{
    hw_pit_ticks++;
}