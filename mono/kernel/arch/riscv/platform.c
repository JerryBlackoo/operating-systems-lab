#include "arch.h"

#include "arch/riscv/csr.h"
#include "arch/riscv/sbi.h"
#include "arch/riscv/trap.h"

const char *arch_name(void)
{
    return "riscv64 (QEMU virt, S-mode)";
}

const char *arch_console_info(void)
{
    return "NS16550A UART at 0x10000000";
}

void arch_init(void)
{
    trap_init();
}

void arch_irq_enable(void)
{
    trap_enable();
}

void arch_idle(void)
{
    __asm__ volatile("wfi");
}

uint64_t arch_intr_save(void)
{
    uint64_t status = csr_read(sstatus);

    csr_clear(sstatus, SSTATUS_SIE);
    return status;
}

void arch_intr_restore(uint64_t flags)
{
    if ((flags & SSTATUS_SIE) != 0) {
        csr_set(sstatus, SSTATUS_SIE);
    } else {
        csr_clear(sstatus, SSTATUS_SIE);
    }
}

void arch_intr_disable(void)
{
    csr_clear(sstatus, SSTATUS_SIE);
}

__attribute__((noreturn)) void arch_halt(void)
{
    sbi_shutdown();
    for (;;) {
        __asm__ volatile("wfi");
    }
}
