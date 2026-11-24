#include "arch.h"
#include "clock/clock.h"
#include "console.h"
#include "dev/device.h"
#include "fs/bio.h"
#include "fs/disk_sched.h"
#include "fs/fs.h"
#include "memory/memory.h"
#include "mm/vm.h"
#include "proc/proc.h"
#include "proc/user.h"
#include "sync/banker.h"
#include "tests/prodcons.h"
#include "tests/sched_demo.h"

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
    console_init();

    console_write("\n");
    console_write("[boot] operating-systems-lab: minimal kernel\n");
    console_write("[boot] arch: ");
    console_write(arch_name());
    console_write("\n");
    console_write("[boot] console: ");
    console_write(arch_console_info());
    console_write("\n");
    console_write("[boot] stack set up, BSS cleared\n");

    /* 尽早安装异常向量，启动阶段的异常也能打印原因，而不是跳到 0 地址。 */
    arch_init();
    memory_init();
    pmm_selftest();
    vm_init();
    vm_selftest();

    bio_init();
    bio_selftest();
    fs_init();
    fs_selftest();
    dev_selftest();
    disk_sched_selftest();

    proc_init();
    proc_dump();

    clock_init();
    arch_irq_enable();

    console_write("[boot] hello from the kernel\n");
    console_write("[boot] starting first user program\n");

    user_hello_init();
    user_fork_init();
    user_fault_init();
    user_init_init();
    sched_demo_start();
    banker_selftest();
    prodcons_selftest();
    arch_intr_disable();
    current->state = PROC_SLEEPING;
    sched();

    /* sched() 对 SLEEPING 的 kmain 不再返回；idle 线程负责空闲等待。 */
}
