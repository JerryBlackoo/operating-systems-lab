#include "arch/arm/irq.h"

#include "arch.h"
#include "clock/clock.h"
#include "console.h"
#include "proc/proc.h"
#include "syscall/syscall.h"

/* Zynq 的 GIC（PL390）：distributor 与 CPU interface。 */
#define GICD_BASE 0xF8F01000UL
#define GICC_BASE 0xF8F00100UL

#define GICD_CTLR 0x000
#define GICD_ISENABLER 0x100
#define GICD_IPRIORITYR 0x400
#define GICD_ITARGETSR 0x800

#define GICC_CTLR 0x000
#define GICC_PMR 0x004
#define GICC_IAR 0x00c
#define GICC_EOIR 0x010

#define IRQ_PRIVATE_TIMER 29

static inline void mmio_write(uintptr_t address, uint32_t value)
{
    *(volatile uint32_t *)address = value;
}

static inline uint32_t mmio_read(uintptr_t address)
{
    return *(volatile uint32_t *)address;
}

void gic_init(void)
{
    mmio_write(GICC_BASE + GICC_CTLR, 0);
    mmio_write(GICD_BASE + GICD_CTLR, 0);

    for (uint32_t i = 0; i < 8; i++) {
        mmio_write(GICD_BASE + GICD_ITARGETSR + i * 4, 0x01010101);
    }
    *(volatile uint8_t *)(GICD_BASE + GICD_IPRIORITYR + IRQ_PRIVATE_TIMER) = 0xa0;
    mmio_write(GICD_BASE + GICD_ISENABLER, 1u << IRQ_PRIVATE_TIMER);

    mmio_write(GICC_BASE + GICC_PMR, 0xf0);
    mmio_write(GICC_BASE + GICC_CTLR, 1);
    mmio_write(GICD_BASE + GICD_CTLR, 1);
}

void irq_dispatch(void *frame)
{
    uint32_t iar = mmio_read(GICC_BASE + GICC_IAR);
    uint32_t id = iar & 0x3ff;

    (void)frame;

    if (id == IRQ_PRIVATE_TIMER) {
        clock_handle_interrupt();
        /* ARM 不在 IRQ 里直接切栈；need_resched 由长循环的 sched_check() 消费。 */
    } else if (id < 1020) {
        console_write("[irq] unexpected interrupt ");
        console_write_dec(id);
        console_write("\n");
    }

    if (id < 1020) {
        mmio_write(GICC_BASE + GICC_EOIR, iar);
    }
}

static uint32_t read_dfsr(void)
{
    uint32_t value;
    __asm__ volatile("mrc p15, 0, %0, c5, c0, 0" : "=r"(value));
    return value;
}

static uint32_t read_dfar(void)
{
    uint32_t value;
    __asm__ volatile("mrc p15, 0, %0, c6, c0, 0" : "=r"(value));
    return value;
}

static uint32_t read_ifsr(void)
{
    uint32_t value;
    __asm__ volatile("mrc p15, 0, %0, c5, c0, 1" : "=r"(value));
    return value;
}

static uint32_t read_ifar(void)
{
    uint32_t value;
    __asm__ volatile("mrc p15, 0, %0, c6, c0, 2" : "=r"(value));
    return value;
}

__attribute__((noreturn)) void arm_fault(const char *name, uint32_t lr,
                                         uint32_t spsr, uint32_t kind)
{
    console_write("[exception] ");
    console_write(name);
    console_write("\n");
    console_write("[exception] lr=");
    console_write_hex(lr);
    console_write(" spsr=");
    console_write_hex(spsr);

    if (kind == 1) {
        console_write(" ifsr=");
        console_write_hex(read_ifsr());
        console_write(" ifar=");
        console_write_hex(read_ifar());
    } else if (kind == 2) {
        console_write(" dfsr=");
        console_write_hex(read_dfsr());
        console_write(" dfar=");
        console_write_hex(read_dfar());
    }

    console_write("\n[exception] halting CPU\n");
    arch_halt();
}

/* 由 vectors.S 的 user_fault_entry 在 SVC 模式下调用：此时 SVC sp 是
 * 该进程的内核栈顶。回收用户地址空间后转为僵尸并让出 CPU。 */
__attribute__((noreturn)) void arm_user_fault_kill(const char *name,
                                                   uint32_t pc, uint32_t spsr)
{
    if (current == 0) {
        arch_halt();
    }

    console_write("[exception] user ");
    console_write(name);
    console_write(" pid=");
    console_write_dec((uint64_t)current->pid);
    console_write(" pc=");
    console_write_hex(pc);
    console_write(" spsr=");
    console_write_hex(spsr);
    console_write("\n");

    current->exit_code = -1;
    proc_exit_cleanup();
    current->state = PROC_ZOMBIE;
    for (;;) {
        yield();
    }
}
