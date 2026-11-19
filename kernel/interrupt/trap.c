#include "arch/riscv/trap.h"

#include "arch/riscv/csr.h"
#include "clock/clock.h"
#include "console.h"
#include "exception/exception.h"

#define SCAUSE_INTERRUPT (1UL << 63)
#define INTERRUPT_S_TIMER 5UL

void trap_init(void)
{
    csr_write(stvec, (uint64_t)trap_entry);
    csr_set(sie, SIE_STIE);
    csr_write(sscratch, 0);

    console_write("[trap] stvec installed at ");
    console_write_hex((uint64_t)trap_entry);
    console_write("\n");
}

void trap_enable(void)
{
    csr_set(sstatus, SSTATUS_SIE);
}

void trap_dispatch(struct trap_frame *tf)
{
    uint64_t scause = csr_read(scause);
    uint64_t sepc = csr_read(sepc);
    uint64_t stval = csr_read(stval);

    if ((scause & SCAUSE_INTERRUPT) != 0) {
        uint64_t code = scause & ~SCAUSE_INTERRUPT;

        if (code == INTERRUPT_S_TIMER) {
            clock_handle_interrupt();
            return;
        }

        console_write("[trap] unexpected interrupt, scause=");
        console_write_hex(scause);
        console_write(" sepc=");
        console_write_hex(sepc);
        console_write("\n");
        return;
    }

    exception_handle(scause, sepc, stval, tf);
}
