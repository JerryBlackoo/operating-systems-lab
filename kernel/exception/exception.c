#include "exception/exception.h"

#include "arch/riscv/sbi.h"
#include "console.h"

static const char *exception_name(uint64_t cause)
{
    switch (cause) {
    case 0:
        return "instruction address misaligned";
    case 1:
        return "instruction access fault";
    case 2:
        return "illegal instruction";
    case 3:
        return "breakpoint";
    case 4:
        return "load address misaligned";
    case 5:
        return "load access fault";
    case 6:
        return "store/AMO address misaligned";
    case 7:
        return "store/AMO access fault";
    case 8:
        return "ecall from U-mode";
    case 9:
        return "ecall from S-mode";
    case 12:
        return "instruction page fault";
    case 13:
        return "load page fault";
    case 15:
        return "store/AMO page fault";
    default:
        return "unknown exception";
    }
}

__attribute__((noreturn)) void exception_handle(uint64_t cause, uint64_t epc,
                                                uint64_t tval,
                                                const struct trap_frame *tf)
{
    console_write("[exception] ");
    console_write(exception_name(cause));
    console_write("\n");

    console_write("[exception] scause=");
    console_write_hex(cause);
    console_write(" sepc=");
    console_write_hex(epc);
    console_write(" stval=");
    console_write_hex(tval);
    console_write(" sp=");
    console_write_hex(tf->x[2]);
    console_write("\n");

    console_write("[exception] halting CPU\n");
    sbi_shutdown();

    for (;;) {
        __asm__ volatile("wfi");
    }
}
