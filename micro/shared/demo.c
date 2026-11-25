#include "demo.h"
#include "context.h"
#include "csr.h"
#include "hart.h"
#include "platform.h"
#include "sbi.h"
#include "trap.h"

#ifdef MICRO_POST_BOOT_RESIDENT
#include "console/console.h"
#include "interrupt/irq.h"
#include "kernel_init.h"
#include "pager/pager.h"

extern void console_note_timer_tick(void);
extern void thread_exit(void) __attribute__((noreturn));
#endif

enum { DEMO_PRINT = 0x10000, DEMO_FAIL, DEMO_PASS };

static _Alignas(16) uint8_t user_stack[16384];
static _Alignas(16) uint8_t trap_stack[16384];
static _Alignas(16) uint8_t demo_stack[16384];
static struct context boot_context;
static const struct demo_module *active_module;
static bool running_user;
static bool reported_first_umode_ecall;

/*
 * 正式启动路径的通用 S 态定时器 glue。
 *
 * clock/sched 不是每个单模块镜像都会链接（例如 memory/objects），因此这里用
 * 弱符号：链接到对应实现时就是正常的 clock_handle_interrupt + sched_check，
 * 没链接时归一化为空，不会把“模块尚未接入”变成链接失败。
 *
 * 强符号定义位置：kernel/clock/timer.c、kernel/sched/sched.c。
 */
extern void clock_handle_interrupt(void) __attribute__((weak));
extern void sched_check(void) __attribute__((weak));
struct trap_frame *resident_user_trap(struct trap_frame *frame)
    __attribute__((weak));

static void (*const clock_irq_hook)(void) = clock_handle_interrupt;
static void (*const sched_check_hook)(void) = sched_check;

void *memcpy(void *dst, const void *src, size_t n)
{
    uint8_t *out = dst;
    const uint8_t *in = src;

    for (size_t i = 0; i < n; i++)
        out[i] = in[i];
    return dst;
}

void *memset(void *dst, int byte, size_t n)
{
    uint8_t *out = dst;

    for (size_t i = 0; i < n; i++)
        out[i] = (uint8_t)byte;
    return dst;
}

int memcmp(const void *left, const void *right, size_t n)
{
    const uint8_t *a = left;
    const uint8_t *b = right;

    for (size_t i = 0; i < n; i++) {
        if (a[i] != b[i])
            return (int)a[i] - b[i];
    }
    return 0;
}

static void console_write(const char *line)
{
#ifdef MICRO_POST_BOOT_RESIDENT
    if (console_mmio_active()) {
        console_puts(line);
        return;
    }
#endif
    while (*line) {
        sbi_putchar(*line++);
    }
}

static void console_hex(uint64_t value)
{
    const char hex[] = "0123456789abcdef";

    console_write("0x");
    for (int shift = 60; shift >= 0; shift -= 4)
        sbi_putchar(hex[(value >> shift) & 15]);
}

static void user_control(uint64_t op, uintptr_t arg, uintptr_t detail)
{
    register uintptr_t a0 __asm__("a0") = arg;
    register uintptr_t a1 __asm__("a1") = detail;
    register uint64_t a7 __asm__("a7") = op;

    __asm__ volatile("ecall" : "+r"(a0), "+r"(a1) : "r"(a7) : "memory");
}

static _Noreturn void stop(uint32_t code)
{
    /*
     * 失败路径可能发生在用户地址空间里（satp != 0）。QEMU 的测试地址
     * 0x00100000 不在任何用户页表中，先退回 bare 才能写入。
     */
    csr_write(CSR_SATP, 0);
    __asm__ volatile("sfence.vma zero, zero" ::: "memory");
    *(volatile uint32_t *)QEMU_TEST_BASE = code;
    for (;;)
        __asm__ volatile("wfi");
}

void demo_log(const char *line)
{
#if defined(MICRO_POST_BOOT_RESIDENT)
    /*
     * 常驻镜像里 running_user 表示当前正在替 U-mode 线程处理 syscall。
     * 此时再发 S-mode DEMO_PRINT ecall 会落回 resident_user_trap 且不推进
     * sepc，导致静默死循环；内核日志必须直接走 console（MMIO 优先，未
     * 初始化时回退 SBI）。
     */
    (void)running_user;
    console_write(line);
    console_write("\n");
#else
    if (running_user)
        user_control(DEMO_PRINT, (uintptr_t)line, 0);
    else {
        console_write(line);
        console_write("\n");
    }
#endif
}

_Noreturn void demo_pass(void)
{
    if (running_user) {
        user_control(DEMO_PASS, 0, 0);
        __builtin_unreachable();
    }
    CHECK(active_module != NULL);
    console_write("[");
    console_write(active_module->name);
    console_write("] PASS\n");

    struct context completed;
    context_switch(&completed, &boot_context);
    demo_fail("resumed a completed demo", 0);
}

_Noreturn void demo_fail(const char *condition, unsigned line)
{
    if (running_user) {
        user_control(DEMO_FAIL, (uintptr_t)condition, line);
        __builtin_unreachable();
    }
    console_write("[");
    console_write(active_module ? active_module->name : "bootstrap");
    console_write("] FAIL at line ");
    console_hex(line);
    console_write(" ");
    demo_log(condition);
    stop(0x10000u | 0x3333u);
}

static _Noreturn void unexpected_trap(const struct trap_frame *frame)
{
    console_write("scause=");
    console_hex(frame->scause);
    console_write(" sepc=");
    console_hex(frame->sepc);
    console_write(" stval=");
    console_hex(frame->stval);
    console_write("\n");
    demo_fail("unexpected trap", 0);
}

struct trap_frame *trap_handler(struct trap_frame *frame)
{
    running_user = false;

    /*
     * S 态定时器中断的通用路径。
     *
     * 先给模块一次接管机会：clock/interrupt 的 demo 需要直接驱动各自的
     * 定时器/irq 层，不能先被 clock 队列的“撤防”截断。模块返回 NULL
     * 表示不接管，此时正式路径统一执行
     *
     *   clock_handle_interrupt() -> sched_tick()
     *   sched_check()            -> sched() -> cpu_switch()
     *
     * 这条路径不再依赖 sched/demo.c 的局部 glue。
     */
    if ((frame->scause & SCAUSE_IRQ) &&
        (frame->scause & ~SCAUSE_IRQ) == IRQ_S_TIMER) {
        struct trap_frame *next = NULL;

        if (active_module && active_module->trap)
            next = active_module->trap(frame);
        if (next) {
            running_user = !(next->sstatus & SSTATUS_SPP);
            return next;
        }
        if (clock_irq_hook) {
            clock_irq_hook();
#ifdef MICRO_POST_BOOT_RESIDENT
            console_note_timer_tick();
#endif
        }
        if (sched_check_hook)
            sched_check_hook();
        running_user = !(frame->sstatus & SSTATUS_SPP);
        return frame;
    }

#ifdef MICRO_POST_BOOT_RESIDENT
    if ((frame->scause & SCAUSE_IRQ) &&
        (frame->scause & ~SCAUSE_IRQ) == IRQ_S_EXT) {
        struct trap_frame *next = NULL;

        if (active_module && active_module->trap) {
            next = active_module->trap(frame);
        }
        if (next) {
            running_user = !(next->sstatus & SSTATUS_SPP);
            return next;
        }
        kernel_irq_enter_devices();
        (void)irq_dispatch();
        kernel_irq_leave_devices();
        if (sched_check_hook) {
            sched_check_hook();
        }
        running_user = !(frame->sstatus & SSTATUS_SPP);
        return frame;
    }
#endif

    if (frame->scause == EXC_ECALL_U && !(frame->sstatus & SSTATUS_SPP)) {
        if (!active_module && resident_user_trap != NULL) {
            running_user = true;
            return resident_user_trap(frame);
        }
        if (!reported_first_umode_ecall) {
            reported_first_umode_ecall = true;
            console_write("[umode] first ecall: scause=");
            console_hex(frame->scause);
            console_write(" SPP=");
            console_write((frame->sstatus & SSTATUS_SPP) ? "1" : "0");
            console_write(" sepc=");
            console_hex(frame->sepc);
            console_write("\n");
        }
        if (resident_user_trap != NULL) {
            running_user = true;
            return resident_user_trap(frame);
        }
        switch (frame->a7) {
        case DEMO_PRINT:
            demo_log((const char *)(uintptr_t)frame->a0);
            frame->sepc += 4;
            running_user = true;
            return frame;
        case DEMO_FAIL:
            demo_fail((const char *)(uintptr_t)frame->a0, frame->a1);
        case DEMO_PASS:
            demo_pass();
        }
    }

#ifdef MICRO_POST_BOOT_RESIDENT
    if (!(frame->sstatus & SSTATUS_SPP) &&
        (frame->scause == EXC_LOAD_PAGEFAULT ||
         frame->scause == EXC_STORE_PAGEFAULT ||
         frame->scause == EXC_INST_PAGEFAULT)) {
        if (pager_handle_user_fault(frame)) {
            running_user = true;
            return frame;
        }
    }
    if (!(frame->sstatus & SSTATUS_SPP)) {
        /*
         * 走到这里的 U-mode 异常没有内核策略可处理（illegal instruction、
         * ebreak、misaligned、access fault…）。终止该线程即可，不能把
         * 一个用户进程的非法指令升级成整机 panic。
         */
        demo_log("[trap] killing user thread: unhandled exception");
        thread_exit();
    }
#endif

    struct trap_frame *next = NULL;
    if (active_module && active_module->trap)
        next = active_module->trap(frame);
    if (!next)
        unexpected_trap(frame);
    running_user = !(next->sstatus & SSTATUS_SPP);
    return next;
}

_Noreturn void demo_enter_user(void (*entry)(uintptr_t), uintptr_t argument)
{
    struct trap_frame *frame =
        (void *)(trap_stack + sizeof(trap_stack) - sizeof(*frame));

    *frame = (struct trap_frame){
        .a0 = argument,
        .sp = (uintptr_t)(user_stack + sizeof(user_stack)),
        .tp = (uintptr_t)hart_local_get(),
        .sepc = (uintptr_t)entry,
        .sstatus = csr_read(CSR_SSTATUS) & ~(SSTATUS_SPP | SSTATUS_SIE | SSTATUS_SPIE),
    };
    running_user = true;
    trap_return(frame);
}

static void user_entry(uintptr_t unused)
{
    (void)unused;
    active_module->run();
    demo_pass();
}

static void supervisor_entry(void)
{
    if (active_module->user)
        demo_enter_user(user_entry, 0);
    active_module->run();
    demo_pass();
}

void demo_run(const struct demo_module *module)
{
    CHECK(!active_module && !running_user && module && module->name && module->run);
    active_module = module;
    console_write("[");
    console_write(module->name);
    console_write("] start\n");

    struct context start = {
        .ra = (uintptr_t)supervisor_entry,
        .sp = (uintptr_t)(demo_stack + sizeof(demo_stack)),
    };
    context_switch(&boot_context, &start);
    active_module = NULL;
}

#ifdef DEMO_SYMBOL
/* 单模块镜像不经过 kernel/boot，所以在这里建立同一套 tp 不变量。 */
static struct hart_local demo_hart;

extern const struct demo_module DEMO_SYMBOL;

void boot_main(uint64_t hart, uint64_t dtb)
{
    (void)dtb;

    demo_hart.hartid = hart;
    demo_hart.kernel_sp_top = 0;
    demo_hart.current = NULL;
    hart_set(&demo_hart);
    CHECK(hart_local_get() == &demo_hart);

    trap_install();
    demo_run(&DEMO_SYMBOL);
    sbi_shutdown();
}
#endif
