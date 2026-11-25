/*
 * kernel_init.c — M1 正式内核尾段：BootInfo RAM 上建帧池、复核 trap、idle 常驻。
 *
 * 仅在默认 capability 镜像（-DMICRO_POST_BOOT_RESIDENT）链接；其它清单仍关机。
 */
#include "kernel_init.h"

#include "cap_as.h"

#include <stddef.h>
#include <stdint.h>

#include "bootinfo.h"
#include "clock/clock.h"
#include "csr.h"
#include "demo.h"
#include "loader/loader.h"
#include "kernel_spawn.h"
#include "service_boot.h"
#include "memory.h"
#include "object.h"
#include "sched.h"
#include "console/console.h"
#include "storage/storage_bringup.h"
#include "interrupt/irq.h"
#include "platform.h"
#include "plic.h"
#include "trap.h"
#include "types.h"

extern char __kernel_start[], __kernel_end[];

#define IDLE_STACK_SIZE 4096
#define HEARTBEAT_NS    2000000000ULL

static _Alignas(16) uint8_t idle_stack[IDLE_STACK_SIZE];
static struct as *kernel_as;
static struct as *irq_saved_as;
static u64 frame_pool_base;

static char *put_str(char *p, const char *s)
{
    while (*s) {
        *p++ = *s++;
    }
    return p;
}

static char *put_hex(char *p, uint64_t v)
{
    static const char digits[] = "0123456789abcdef";

    for (int i = 60; i >= 0; i -= 4) {
        *p++ = digits[(v >> i) & 0xf];
    }
    return p;
}

static char *put_dec(char *p, uint64_t v)
{
    char tmp[24];
    unsigned n = 0;

    if (v == 0) {
        *p++ = '0';
        return p;
    }
    while (v != 0) {
        tmp[n++] = (char)('0' + v % 10);
        v /= 10;
    }
    while (n != 0) {
        *p++ = tmp[--n];
    }
    return p;
}

static u64 align_up(u64 v, u64 align)
{
    return (v + align - 1) & ~(align - 1);
}

static const char *reserve_label(u32 type)
{
    switch (type) {
    case BOOT_REGION_FIRMWARE:
        return "firmware";
    case BOOT_REGION_KERNEL:
        return "kernel";
    case BOOT_REGION_DTB:
        return "dtb";
    case BOOT_REGION_INITRD:
        return "initrd";
    default:
        return "unknown";
    }
}

static int kernel_frame_pool_init(const struct bootinfo *bi)
{
    u64 pool_base;
    u64 ram_end;
    u64 pool_size;
    char line[160];
    char *p;

    if (bi == 0 || bi->ram_count == 0) {
        return MEM_EINVAL;
    }

    pool_base = align_up(bi->kernel_end, PAGE_SIZE);
    frame_pool_base = pool_base;
    ram_end = bi->ram[0].base + bi->ram[0].size;
    if (pool_base >= ram_end) {
        return MEM_EINVAL;
    }

    pool_size = ram_end - pool_base;
    if (frame_init((paddr_t)pool_base, (size_t)pool_size,
                   (void *)(uintptr_t)pool_base) != MEM_OK) {
        return MEM_EINVAL;
    }

    for (u32 i = 0; i < bi->reserved_count; i++) {
        frame_reserve((paddr_t)bi->reserved[i].base, (size_t)bi->reserved[i].size);
    }

    p = line;
    p = put_str(p, "[kmain] pool: base=0x");
    p = put_hex(p, pool_base);
    p = put_str(p, " bytes=");
    p = put_dec(p, pool_size);
    p = put_str(p, " frames=");
    p = put_dec(p, frame_total_count());
    p = put_str(p, " reserved=");
    p = put_dec(p, frame_total_count() - frame_free_count());
    p = put_str(p, " free=");
    p = put_dec(p, frame_free_count());
    *p = '\0';
    demo_log(line);

    for (u32 i = 0; i < bi->reserved_count; i++) {
        p = line;
        p = put_str(p, "[kmain] reserve: ");
        p = put_str(p, reserve_label(bi->reserved[i].type));
        p = put_str(p, " 0x");
        p = put_hex(p, bi->reserved[i].base);
        p = put_str(p, " +");
        p = put_dec(p, bi->reserved[i].size);
        *p = '\0';
        demo_log(line);
    }

    return MEM_OK;
}

static int kernel_address_space_init(const struct bootinfo *bi)
{
    uintptr_t kstart;
    size_t ksize;
    u64 pool_base;
    u64 ram_end;
    size_t pool_size;

    kstart = (uintptr_t)__kernel_start;
    ksize = (size_t)align_up((uintptr_t)__kernel_end - kstart, PAGE_SIZE);
    pool_base = align_up(bi->kernel_end, PAGE_SIZE);
    ram_end = bi->ram[0].base + bi->ram[0].size;
    pool_size = (size_t)(ram_end - pool_base);

    kernel_as = as_create();
    if (kernel_as == 0) {
        return MEM_ENOMEM;
    }
    if (as_map_kernel_range(kernel_as, (paddr_t)kstart, ksize) != MEM_OK) {
        as_unref(kernel_as);
        kernel_as = 0;
        return MEM_EINVAL;
    }
    if (as_map_kernel_range(kernel_as, (paddr_t)pool_base, pool_size) != MEM_OK) {
        as_unref(kernel_as);
        kernel_as = 0;
        return MEM_EINVAL;
    }
    if (as_map_kernel_range(kernel_as, (paddr_t)UART0_BASE, PAGE_SIZE) != MEM_OK ||
        as_map_kernel_range(kernel_as, (paddr_t)PLIC_BASE, 0x400000UL) != MEM_OK) {
        as_unref(kernel_as);
        kernel_as = 0;
        return MEM_EINVAL;
    }
    for (unsigned vmmio = 0x10001000UL; vmmio <= 0x10008000UL; vmmio += 0x1000UL) {
        int rc = as_map_kernel_range(kernel_as, (paddr_t)vmmio, PAGE_SIZE);

        if (rc != MEM_OK && rc != MEM_EEXIST) {
            as_unref(kernel_as);
            kernel_as = 0;
            return MEM_EINVAL;
        }
    }
    return MEM_OK;
}

struct as *kernel_resident_as(void)
{
    return kernel_as;
}

void kernel_irq_enter_devices(void)
{
    struct thread *cur = sched_current();

    irq_saved_as = (cur != NULL) ? cur->as : 0;
    if (kernel_as != 0) {
        as_activate(kernel_as);
    }
}

void kernel_irq_leave_devices(void)
{
    if (irq_saved_as != 0) {
        as_activate(irq_saved_as);
    } else {
        as_activate(0);
    }
}

static int kernel_console_bringup(void)
{
    char line[72];
    char *p;
    unsigned divisor;

    divisor = UART0_CLK / (16U * 115200U);
    plic_hw_init();
    if (irq_init(plic_irq_port()) != IRQ_OK) {
        return -1;
    }
    console_mmio_init(divisor);
    if (console_bind_uart_irq() != 0) {
        return -1;
    }
    as_activate(kernel_as);

    p = line;
    p = put_str(p, "[console] uart0 mmio ready (divisor=");
    p = put_dec(p, divisor);
    p = put_str(p, ")");
    *p = '\0';
    demo_log(line);

    console_mmio_enable(true);
    demo_log("[console] switched from SBI putchar");

    csr_set(CSR_SIE, SIE_SEIE);
    return 0;
}

static void kernel_idle_entry(void *argument)
{
    uint64_t next_heartbeat;
    char line[96];
    char *p;

    (void)argument;

    p = line;
    p = put_str(p, "[kmain] idle: pool=0x");
    p = put_hex(p, frame_pool_base);
    p = put_str(p, "+");
    p = put_dec(p, frame_total_count());
    p = put_str(p, " frames free=");
    p = put_dec(p, frame_free_count());
    *p = '\0';
    demo_log(line);

    clock_init();
    if (!sched_start_tick(1000000)) {
        demo_fail("idle tick", __LINE__);
    }
    next_heartbeat = clock_deadline_after(clock_now(), HEARTBEAT_NS);

    for (;;) {
        if (clock_now() >= next_heartbeat) {
            p = line;
            p = put_str(p, "[kmain] idle: heartbeat free=");
            p = put_dec(p, frame_free_count());
            p = put_str(p, " ext_irqs=");
            p = put_dec(p, console_ext_irqs());
            p = put_str(p, " timer_ticks=");
            p = put_dec(p, sched_tick_count());
            *p = '\0';
            demo_log(line);
            next_heartbeat = clock_deadline_after(clock_now(), HEARTBEAT_NS);
        }
        asm volatile("wfi");
    }
}

void kernel_enter_resident(const struct bootinfo *bi)
{
    struct thread *idle;
    struct thread *init = 0;

    irq_disable();
    sched_stop_tick();
    csr_clear(CSR_SIE, SIE_STIE);
    /*
     * bootstrap 的 U-mode demo 经 trap_return 回 U 态时把 sscratch 设成
     * 该线程的内核栈顶；进入常驻内核前必须恢复 S-mode 下 sscratch == 0
     * 的不变量（trap.S 靠它区分来自 U/S 的换栈路径），否则第一次 S 态
     * trap 会使用已退出线程的旧内核栈。
     */
    csr_write(CSR_SSCRATCH, 0);

    object_system_init();
    sched_init();

    if (kernel_frame_pool_init(bi) != MEM_OK) {
        demo_fail("frame pool init", __LINE__);
    }
    if (kernel_address_space_init(bi) != MEM_OK) {
        demo_fail("kernel as init", __LINE__);
    }
    if (kernel_console_bringup() != 0) {
        demo_fail("console bringup", __LINE__);
    }

    trap_install();
    if (csr_read(CSR_STVEC) == 0) {
        demo_fail("stvec not installed", __LINE__);
    }
    if (kernel_storage_bringup(kernel_as) != 0) {
        demo_log("[kmain] storage bringup failed (no virtio drive?)");
    }

    idle = thread_create(NULL, kernel_idle_entry, NULL, idle_stack,
                         sizeof(idle_stack), "idle");
    if (idle == NULL) {
        demo_fail("idle thread", __LINE__);
    }
    sched_set_idle(idle);

    {
        struct service_boot_layout layout = {0};

        if (service_boot_prepare(bi, kernel_as->root_pfn, &layout) != 0) {
            demo_log("[service] boot prepare failed");
        } else if (kernel_as != 0) {
            if (loader_pin_initrd(bi) != 0) {
                demo_log("[loader] initrd pin failed");
            }
            kernel_spawn_init(bi, kernel_as->root_pfn);
            {
                struct thread *spawned = 0;

                if (loader_spawn_user(bi, kernel_as->root_pfn, "sm", &layout.sm,
                                      &spawned) != 0) {
                    demo_log("[loader] sm not started");
                } else if (spawned != 0 && spawned->as != 0) {
                    (void)cap_vm_ensure_as_map(spawned->as);
                }
                spawned = 0;
                if (loader_spawn_user(bi, kernel_as->root_pfn, "pm", &layout.pm,
                                      &spawned) != 0) {
                    demo_log("[loader] pm not started");
                } else if (spawned != 0 && spawned->as != 0) {
                    (void)cap_vm_ensure_as_map(spawned->as);
                }
                spawned = 0;
                if (loader_spawn_user(bi, kernel_as->root_pfn, "vm", &layout.vm,
                                      &spawned) != 0) {
                    demo_log("[loader] vm not started");
                } else if (spawned != 0 && spawned->as != 0) {
                    (void)cap_vm_ensure_as_map(spawned->as);
                }
                spawned = 0;
                if (loader_spawn_user(bi, kernel_as->root_pfn, "heaptest",
                                      &layout.heaptest, &spawned) != 0) {
                    demo_log("[loader] heaptest not started");
                } else if (spawned != 0 && spawned->as != 0) {
                    (void)cap_vm_ensure_as_map(spawned->as);
                }
                spawned = 0;
                if (loader_spawn_user(bi, kernel_as->root_pfn, "heaptest2",
                                      &layout.heaptest2, &spawned) != 0) {
                    demo_log("[loader] heaptest2 not started");
                } else if (spawned != 0 && spawned->as != 0) {
                    (void)cap_vm_ensure_as_map(spawned->as);
                }
                if (loader_spawn_user(bi, kernel_as->root_pfn, "init",
                                      &layout.init, &init) != 0) {
                    demo_log("[loader] init not started (missing initrd or bad image)");
                } else if (init != 0 && init->as != 0) {
                    (void)cap_vm_ensure_as_map(init->as);
                }
            }
        }
    }

    sched_bootstrap(idle);

    {
        struct thread *first = sched_current();

        irq_enable();
        if (first != 0) {
            cpu_switch_to(&first->ctx);
        }
    }

    demo_fail("resident first switch returned", __LINE__);
}