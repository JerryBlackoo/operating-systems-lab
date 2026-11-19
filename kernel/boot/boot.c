#include "arch/riscv/trap.h"
#include "clock/clock.h"
#include "console.h"
#include "memory/memory.h"

static void idle_forever(void)
{
    for (;;) {
        __asm__ volatile("wfi");
    }
}

static void pmm_selftest(void)
{
    uint64_t first = pmm_alloc_page();
    uint64_t second = pmm_alloc_page();

    if (first == 0 || second == 0 || first == second) {
        console_write("[mem] selftest: FAILED to allocate two distinct pages\n");
        return;
    }

    console_write("[mem] selftest: allocated ");
    console_write_hex(first);
    console_write(" and ");
    console_write_hex(second);
    console_write("\n");

    pmm_free_page(first);
    pmm_free_page(second);

    console_write("[mem] selftest: pages returned, ");
    console_write_dec(pmm_free_page_count());
    console_write(" free pages\n");
}

void kmain(void)
{
    console_write("\n");
    console_write("[boot] operating-systems-lab: minimal RISC-V 64 kernel\n");
    console_write("[boot] boot path: QEMU virt -> OpenSBI -> S-mode entry\n");
    console_write("[boot] stack set up, BSS cleared\n");
    console_write("[boot] console: NS16550A UART at 0x10000000\n");

    /* 尽早安装 stvec，启动阶段的异常也能打印原因，而不是跳到 0 地址。 */
    trap_init();
    memory_init();
    pmm_selftest();

    clock_init();
    trap_enable();

    console_write("[boot] hello from the kernel\n");
    console_write("[boot] no scheduler, syscalls or virtual memory yet\n");
    console_write("[boot] entering wfi loop (Ctrl-A X to exit QEMU)\n");

    idle_forever();
}
