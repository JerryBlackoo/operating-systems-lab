#include "demo.h"
#include "context.h"
#include "csr.h"
#include "hart.h"
#include "platform.h"
#include "sbi.h"
#include "trap.h"

enum { DEMO_PRINT = 0x10000, DEMO_FAIL, DEMO_PASS };

static _Alignas(16) uint8_t user_stack[16384];
static _Alignas(16) uint8_t trap_stack[16384];
static _Alignas(16) uint8_t demo_stack[16384];
static struct context boot_context;
static const struct demo_module *active_module;
static bool running_user;

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
    while (*line)
        sbi_putchar(*line++);
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
    *(volatile uint32_t *)QEMU_TEST_BASE = code;
    for (;;)
        __asm__ volatile("wfi");
}

void demo_log(const char *line)
{
    if (running_user)
        user_control(DEMO_PRINT, (uintptr_t)line, 0);
    else {
        console_write(line);
        console_write("\n");
    }
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
    if (frame->scause == EXC_ECALL_U && !(frame->sstatus & SSTATUS_SPP)) {
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
extern const struct demo_module DEMO_SYMBOL;

void boot_main(uint64_t hart, uint64_t dtb)
{
    (void)hart;
    (void)dtb;
    trap_install();
    demo_run(&DEMO_SYMBOL);
    sbi_shutdown();
}
#endif
