/*
 * boot.c — 最小启动模型：RawHandoff -> BootInfo -> kmain。
 *
 * 上游：arch/riscv/boot.S 的 _start 建立好 C 运行环境后调用
 *       boot_main(hartid, dtb)。
 * 下游：kmain(&bi)。本文件在调用 kmain 之后不再拥有控制权。
 *
 * 只做五件事：
 *   1. 把入口假设变成断言（bss/satp/SIE）
 *   2. 建立早期控制台
 *   3. 构建并校验 BootInfo
 *   4. 调用 arch 的 hart_set / trap_install
 *   5. 交接给 kmain，失败即停
 *
 * 不做事：不开 MMU、不建堆、不开中断、不调度、不驱动设备、
 * 不挂文件系统、不创建首个线程。判据：如果 kmain 或 U-mode
 * server 能做，就不属于这里。
 */
#include "types.h"
#include "bootinfo.h"
#include "bootstrap.h"
#include "platform.h"
#include "csr.h"
#include "hart.h"
#include "trap.h"
#include "sbi.h"

extern char __kernel_start[], __kernel_end[];
extern char __boot_stack_top[];

/* .bss 探针：boot.S 若没清 BSS，这里读到的就不是 0。volatile 防止被折叠。 */
static volatile u64 bss_probe;

/* 单 hart 的 per-hart 状态；SMP 时改成按 hartid 索引的数组。 */
static struct hart_local boot_hart;

/* ---- 早期控制台：只输出，不接收，不注册驱动 ------------------------ */

static void con_puts(const char *s)
{
    while (*s)
        sbi_putchar(*s++);
}

static void con_puthex(u64 v)
{
    static const char digits[] = "0123456789abcdef";

    con_puts("0x");
    for (int i = 60; i >= 0; i -= 4)
        sbi_putchar(digits[(v >> i) & 0xf]);
}

static void con_putdec(u64 v)
{
    char buf[24];
    int n = 0;

    if (v == 0) {
        sbi_putchar('0');
        return;
    }
    while (v) {
        buf[n++] = '0' + (v % 10);
        v /= 10;
    }
    while (n)
        sbi_putchar(buf[--n]);
}

static const char *region_name(u32 type)
{
    switch (type) {
    case BOOT_REGION_RAM:      return "ram";
    case BOOT_REGION_FIRMWARE: return "firmware";
    case BOOT_REGION_KERNEL:   return "kernel";
    case BOOT_REGION_DTB:      return "dtb";
    case BOOT_REGION_INITRD:   return "initrd";
    default:                   return "unknown";
    }
}

static const char *bootinfo_error(int rc)
{
    switch (rc) {
    case BOOTINFO_EBADMAGIC:   return "bad magic";
    case BOOTINFO_EBADVERSION: return "bad version";
    case BOOTINFO_EBADSIZE:    return "bad size";
    case BOOTINFO_EBADREGION:  return "bad region";
    case BOOTINFO_EOVERLAP:    return "overlapping regions";
    case BOOTINFO_ENOTINRAM:   return "region outside RAM";
    default:                   return "unknown error";
    }
}

static __attribute__((noreturn)) void boot_panic(const char *why)
{
    con_puts("\n[boot] FAIL: ");
    con_puts(why);
    con_puts("\n");
    sbi_shutdown();
}

/* ---- 平台事实：唯一允许碰 platform.h 与链接符号的地方 -------------- */

static void platform_fill(struct boot_platform *p)
{
    *p = (struct boot_platform){0};
    p->ram_base = RAM_BASE;
    p->ram_size = RAM_SIZE;
    p->firmware_base = RAM_BASE;
    p->firmware_size = KERNEL_BASE - RAM_BASE;
    p->kernel_start = (u64)__kernel_start;
    p->kernel_end = (u64)__kernel_end;
}

/* ---- 时刻 1：entry —— 把 boot.S 的承诺变成断言 ---------------------- */

static void boot_check_entry(void)
{
    if (bss_probe != 0)
        boot_panic("bss not cleared");
    if (csr_read(CSR_SATP) != 0)
        boot_panic("satp is not zero");
    if (csr_read(CSR_SSTATUS) & SSTATUS_SIE)
        boot_panic("interrupts enabled at entry");
    if (csr_read(CSR_SIE) != 0)
        boot_panic("interrupts armed at entry");
    if (csr_read(CSR_SSCRATCH) != 0)
        boot_panic("sscratch is not zero");
}

/* ---- 时刻 2：console —— 只输出，不接收，不注册驱动 ------------------ */

static void boot_console_banner(u64 hartid, u64 dtb)
{
    con_puts("\n[micro] boot: minimal model\n");
    con_puts("[boot] raw handoff: hartid=");
    con_putdec(hartid);
    con_puts(" dtb=");
    con_puthex(dtb);
    con_puts("\n");
    con_puts("[boot] entry: bss=0, satp=0, SIE=0, sscratch=0\n");
    con_puts("[boot] console: SBI legacy putchar OK\n");
}

/* ---- 时刻 3/4：parse/validate + reserve ------------------------------ */

static void bootinfo_report(const struct bootinfo *bi)
{
    con_puts("[boot] bootinfo: v");
    con_putdec(bi->version);
    con_puts(" size=");
    con_putdec(bi->size);
    con_puts("\n");

    con_puts("[boot] kernel: ");
    con_puthex(bi->kernel_start);
    con_puts(" + ");
    con_puthex(bi->kernel_end - bi->kernel_start);
    con_puts("\n");

    for (u32 i = 0; i < bi->ram_count; i++) {
        con_puts("[boot] ram: ");
        con_puthex(bi->ram[i].base);
        con_puts(" + ");
        con_puthex(bi->ram[i].size);
        con_puts("\n");
    }
    for (u32 i = 0; i < bi->reserved_count; i++) {
        con_puts("[boot] reserved[");
        con_putdec(i);
        con_puts("]: ");
        con_puthex(bi->reserved[i].base);
        con_puts(" + ");
        con_puthex(bi->reserved[i].size);
        con_puts(" ");
        con_puts(region_name(bi->reserved[i].type));
        con_puts("\n");
    }
}

/* ---- 时刻 5：arch 归一化与交接 -------------------------------------- */

static void arch_ready(u64 hartid)
{
    boot_hart.hartid = hartid;
    boot_hart.kernel_sp_top = (u64)__boot_stack_top;
    boot_hart.current = NULL;
    hart_set(&boot_hart);
    if (hart_local_get() != &boot_hart)
        boot_panic("tp invariant broken");

    trap_install();
    if (csr_read(CSR_STVEC) == 0)
        boot_panic("stvec not installed");
    con_puts("[boot] arch: tp=&hart_local, stvec installed\n");
    con_puts("[boot] arch: satp=");
    con_puthex(csr_read(CSR_SATP));
    con_puts(" sstatus=");
    con_puthex(csr_read(CSR_SSTATUS));
    con_puts("\n");
}

/*
 * 交接点。完成早期初始化后，按本镜像的清单运行自举模块。
 */
__attribute__((noreturn)) void kmain(const struct bootinfo *bi)
{
    con_puts("[kmain] handoff: BootInfo at ");
    con_puthex((u64)bi);
    con_puts(", hartid=");
    con_putdec(bi->hartid);
    con_puts("\n");
    bootstrap_run();
    con_puts("[kmain] shutting down\n");
    sbi_shutdown();
}

void boot_main(u64 hartid, u64 dtb)
{
    struct boot_platform plat;
    struct bootinfo bi;
    int dtb_state;
    int rc;

    boot_check_entry();
    boot_console_banner(hartid, dtb);

    /* 3. 构建 BootInfo：平台事实 + DTB 指针，随后校验自洽性。 */
    platform_fill(&plat);
    dtb_state = bootinfo_build(&bi, &plat, hartid, dtb);
    if (dtb_state == BOOT_DTB_BAD)
        boot_panic("malformed FDT header");
    con_puts(dtb_state == BOOT_DTB_VALID
                 ? "[boot] fdt: header OK\n"
                 : "[boot] fdt: absent, using platform constants\n");

    rc = bootinfo_validate(&bi);
    if (rc != BOOTINFO_OK) {
        con_puts("[boot] bootinfo: ");
        con_puts(bootinfo_error(rc));
        con_puts("\n");
        boot_panic("BootInfo validation failed");
    }

    /* 4. reserve：BootInfo 已把 firmware/kernel/DTB 显式列为保留区。 */
    bootinfo_report(&bi);

    /* 5. handoff：先建立 tp 不变量并安装 stvec，再交给 kmain。 */
    arch_ready(hartid);
    con_puts("[boot] handoff: kmain(&bootinfo)\n");
    kmain(&bi);
    boot_panic("kmain returned");
}
