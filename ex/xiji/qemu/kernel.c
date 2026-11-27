#include "../common/xiji_io.h"
#include "../src/xiji_runner.h"
#include "hw/hw_verify.h"

typedef int (*test_fn)(void);

static const struct {
    const char *name;
    test_fn fn;
} algo_tests[] = {
    { "exp01", xiji_exp01_run },
    { "exp02", xiji_exp02_run },
    { "exp03", xiji_exp03_run },
    { "exp04", xiji_exp04_run },
    { "exp05", xiji_exp05_run },
    { "exp06", xiji_exp06_run },
    { "exp07", xiji_exp07_run },
};

static _Noreturn void finish(unsigned int code)
{
    __asm__ volatile("outl %0, $0xf4" : : "a"(code));
    for (;;)
        __asm__ volatile("hlt");
}

_Noreturn void kmain(void);

_Noreturn void kmain(void)
{
    xiji_puts("xiji-qemu: real devices (CMOS PIT PIC #PF IDE)\n");
    int failed = 0;
    if (hw_verify_all() != 0)
        failed++;

    for (unsigned i = 0; i < sizeof algo_tests / sizeof algo_tests[0]; i++) {
        if (algo_tests[i].fn() != 0)
            failed++;
    }

    if (failed)
        xiji_puts("FAIL: see messages above\n");
    else
        xiji_puts("ALL PASS (qemu+hw+algo)\n");
    finish(failed ? 0x11 : 0x10);
}