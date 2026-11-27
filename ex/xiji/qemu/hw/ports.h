#ifndef XIJI_HW_PORTS_H
#define XIJI_HW_PORTS_H

#include <stdint.h>

static inline void outb(uint8_t v, uint16_t port)
{
    __asm__ volatile("outb %0, %1" : : "a"(v), "Nd"(port));
}

static inline void outw(uint16_t v, uint16_t port)
{
    __asm__ volatile("outw %0, %1" : : "a"(v), "Nd"(port));
}

static inline void outl(uint32_t v, uint16_t port)
{
    __asm__ volatile("outl %0, %1" : : "a"(v), "Nd"(port));
}

static inline uint8_t inb(uint16_t port)
{
    uint8_t r;
    __asm__ volatile("inb %1, %0" : "=a"(r) : "Nd"(port));
    return r;
}

static inline uint16_t inw(uint16_t port)
{
    uint16_t r;
    __asm__ volatile("inw %1, %0" : "=a"(r) : "Nd"(port));
    return r;
}

#endif