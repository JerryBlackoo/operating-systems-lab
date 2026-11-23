/*
 * 独立架构自检：链接已有 boot.S、trap.S、context.S、trap.c 和 sbi.c。
 * boot.S 的 call boot_main 落到这里，不链接正式 kernel/boot/boot.c。
 */
#include "context.h"
#include "csr.h"
#include "hart.h"
#include "platform.h"
#include "sbi.h"
#include "trap.h"

extern char trap_entry[];

static struct hart_local boot_hart;
static struct context ctx_a, ctx_b;
static u64 stack_b[512] __attribute__((aligned(16)));
static volatile u64 switch_count;

static void puts_raw(const char *s)
{
    while (*s)
        sbi_putchar(*s++);
}

static void put_hex(u64 value)
{
    static const char digits[] = "0123456789abcdef";
    puts_raw("0x");
    for (int shift = 60; shift >= 0; shift -= 4)
        sbi_putchar(digits[(value >> shift) & 0xf]);
}

static void put_dec(u64 value)
{
    char digits[20];
    int n = 0;
    do {
        digits[n++] = '0' + value % 10;
        value /= 10;
    } while (value);
    while (n)
        sbi_putchar(digits[--n]);
}

static __attribute__((noreturn)) void fail(const char *reason)
{
    puts_raw("[selftest] FAIL: ");
    puts_raw(reason);
    puts_raw("\n");
    /* QEMU virt sifive_test: 0x3333 + code 1，令 QEMU 返回非零。 */
    *(volatile u32 *)QEMU_TEST_BASE = (1U << 16) | 0x3333U;
    for (;;)
        __asm__ volatile("wfi");
}

static __attribute__((noreturn)) void thread_b(void)
{
    puts_raw("[ctx] in B, switching back to A\n");
    switch_count++;
    context_switch(&ctx_b, &ctx_a);
    fail("thread B unexpectedly resumed");
}

void boot_main(u64 hartid, u64 dtb)
{
    u64 fault_pc;

    puts_raw("\n[micro] arch/riscv minimal model\n");
    puts_raw("[boot] hartid=");
    put_dec(hartid);
    puts_raw("  dtb=");
    put_hex(dtb);
    puts_raw("\n");

    boot_hart.hartid = hartid;
    hart_set(&boot_hart);
    if (hart_local_get() != &boot_hart)
        fail("tp does not point to boot_hart");
    puts_raw("[boot] tp = &hart_local  OK\n");

    if (switch_count != 0)
        fail("BSS probe is not zero");
    puts_raw("[boot] bss cleared (switch_count=0)  OK\n");

    trap_install();
    if (csr_read(CSR_STVEC) != (u64)trap_entry)
        fail("stvec does not point to trap_entry");
    puts_raw("[trap] stvec installed  OK\n");

    ctx_b.ra = (u64)thread_b;
    ctx_b.sp = (u64)(stack_b + sizeof(stack_b) / sizeof(stack_b[0]));
    puts_raw("[ctx] switching A -> B\n");
    context_switch(&ctx_a, &ctx_b);
    if (switch_count != 1)
        fail("context switch did not return from B exactly once");
    puts_raw("[ctx] back in A, switch_count=1  OK\n");

    puts_raw("[trap] writing read-only CSR (time)...\n");
    /* 记录故障指令地址；禁用压缩确保被 handler 跳过的是 4 字节。 */
    __asm__ volatile(
        ".option push\n"
        ".option norvc\n"
        "la %0, 1f\n"
        "1: csrw 0xc01, zero\n"
        ".option pop\n"
        : "=&r"(fault_pc) : : "memory");
    if (csr_read(CSR_SCAUSE) != EXC_INST_ILLEGAL ||
        csr_read(CSR_SEPC) != fault_pc + 4)
        fail("illegal instruction trap or return was not observed");
    puts_raw("[trap] survived, trap return works  OK\n");

    puts_raw("\n[micro] all three moments verified, shutting down\n");
    sbi_shutdown();
}
