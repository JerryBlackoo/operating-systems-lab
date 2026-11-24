#include "arch.h"

#include "arch/arm/irq.h"
#include "console.h"

const char *arch_name(void)
{
    return "armv7-a (Zynq-7010, Cortex-A9)";
}

const char *arch_console_info(void)
{
    return "Cadence UART0 at 0xE0000000";
}

void arch_init(void)
{
    gic_init();
    console_write("[irq] GIC distributor 0xf8f01000, CPU interface 0xf8f00100\n");
    console_write("[irq] private timer interrupt 29 enabled\n");
}

void arch_irq_enable(void)
{
    __asm__ volatile("cpsie i" ::: "memory");
}

void arch_idle(void)
{
    __asm__ volatile("wfi");
}

uint64_t arch_intr_save(void)
{
    uint32_t cpsr;

    __asm__ volatile("mrs %0, cpsr" : "=r"(cpsr));
    __asm__ volatile("cpsid i" ::: "memory");
    return cpsr;
}

void arch_intr_restore(uint64_t flags)
{
    __asm__ volatile("msr cpsr_c, %0" ::"r"((uint32_t)flags) : "memory");
}

void arch_intr_disable(void)
{
    __asm__ volatile("cpsid i" ::: "memory");
}

__attribute__((noreturn)) void arch_halt(void)
{
    __asm__ volatile("cpsid i" ::: "memory");
    for (;;) {
        __asm__ volatile("wfi");
    }
}
