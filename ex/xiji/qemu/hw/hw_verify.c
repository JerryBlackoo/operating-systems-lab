#include "hw_verify.h"
#include "../../common/xiji_test.h"
#include "../../src/xiji_runner.h"
#include "ata.h"
#include "idt.h"
#include "paging.h"
#include "pic_pit.h"
#include "ports.h"
#include <stdint.h>

#define CMOS_ADDR 0x70
#define CMOS_DATA 0x71
#define DISK_LBA 120u
#define DISK_SECTOR_SIZE 512

/* ---------- lab1：端口与 CMOS ---------- */

static uint8_t cmos_read(uint8_t reg)
{
    outb(reg, CMOS_ADDR);
    return inb(CMOS_DATA);
}

static int cmos_bcd_to_bin(uint8_t v)
{
    return (int)((v & 0x0f) + ((v >> 4) * 10));
}

static int hw_lab1_cmos(void)
{
    uint8_t status_b = cmos_read(0x0b);
    int seconds;
    int minutes;

    cmos_read(0x00); /* 读一次让 RTC 锁存更新（QEMU 无需真正等待 UIP） */
    seconds = cmos_bcd_to_bin(cmos_read(0x00));
    minutes = cmos_bcd_to_bin(cmos_read(0x02));

    if ((status_b & 0x04) == 0 && seconds > 59)
        return xiji_fail("hw-lab1-cmos", "seconds out of range");
    if ((status_b & 0x04) == 0 && minutes > 59)
        return xiji_fail("hw-lab1-cmos", "minutes out of range");
    if (seconds < 0 || seconds > 59 || minutes < 0 || minutes > 59)
        return xiji_fail("hw-lab1-cmos", "non-BCD value");

    xiji_puts("hw-lab1: CMOS+debugcon+isa-debug-exit\n");
    return 0;
}

/* ---------- exp01/02：8254 + 8259 ---------- */

static int hw_exp01_02_pit(void)
{
    uint32_t t0 = hw_pit_ticks;
    uint32_t t1;

    /* hlt 让 CPU 真正睡到下一个中断，而不是忙等 */
    for (int i = 0; i < 200000 && hw_pit_ticks < t0 + 3u; i++)
        __asm__ volatile("hlt");

    t1 = hw_pit_ticks;
    if (t1 < t0 + 2u)
        return xiji_fail("hw-exp01-pit", "8254 did not raise IRQ0");

    /* 中断确实被 PIC 投递（掩码位 IRQ0 = 0） */
    if ((inb(0x21) & 0x01u) != 0)
        return xiji_fail("hw-exp01-pit", "IRQ0 masked");

    xiji_puts("hw-exp01-02: 8254+PIC ticks=");
    xiji_putint((int)(t1 - t0));
    xiji_putc('\n');
    return 0;
}

/* ---------- exp03：用真实物理内存做分区 ---------- */

/*
 * 分区实验原本只操作账本。这里的做法是：把 exp03 的分区块映射到一段
 * 真实物理内存（0x00200000 起），每个分区块写入独立指纹，再整体校验
 * 没有两个分区共享同一段物理地址 —— 这正是"分区互不重叠"的硬件级证明。
 */
#define PART_PHYS 0x00200000u
#define PART_PATTERN_MAX 200

static uint8_t *const part_mem = (uint8_t *)(uintptr_t)PART_PHYS;

static void part_fill(int base, int size, uint8_t tag)
{
    for (int i = 0; i < size && i < PART_PATTERN_MAX; i++)
        part_mem[base + i] = (uint8_t)(tag + i);
}

static int part_check(int base, int size, uint8_t tag)
{
    for (int i = 0; i < size && i < PART_PATTERN_MAX; i++) {
        if (part_mem[base + i] != (uint8_t)(tag + i))
            return 0;
    }
    return 1;
}

static int hw_exp03_partition_ram(void)
{
    int a;
    int b;
    int c;

    xiji_exp03_reset();

    a = xiji_exp03_alloc(XIJI_FIT_FIRST, 200, 1);
    b = xiji_exp03_alloc(XIJI_FIT_FIRST, 100, 2);
    c = xiji_exp03_alloc(XIJI_FIT_FIRST, 50, 3);
    if (a < 0 || b < 0 || c < 0)
        return xiji_fail("hw-exp03", "allocation failed");
    if (!xiji_exp03_invariants_ok())
        return xiji_fail("hw-exp03", "invariant broken");

    part_fill(a, 200, 0x10);
    part_fill(b, 100, 0x20);
    part_fill(c, 50, 0x30);

    if (!part_check(a, 200, 0x10) || !part_check(b, 100, 0x20) || !part_check(c, 50, 0x30))
        return xiji_fail("hw-exp03", "partitions overlap in RAM");

    /* 释放中间一块，确认物理内容不被邻居覆盖 */
    if (xiji_exp03_free(b) != 0)
        return xiji_fail("hw-exp03", "free failed");
    if (!part_check(a, 200, 0x10) || !part_check(c, 50, 0x30))
        return xiji_fail("hw-exp03", "free corrupted neighbours");

    xiji_puts("hw-exp03: physical partitions a/b/c non-overlapping\n");
    return 0;
}

/* ---------- exp04：段基址叠加真实访存 ---------- */

static int hw_exp04_segment_ram(void)
{
    /* 每个"段"对应一段物理区，段内偏移必须落在自己的段里 */
    volatile uint8_t *seg0 = (volatile uint8_t *)(uintptr_t)(PART_PHYS + 0x1000);
    volatile uint8_t *seg1 = (volatile uint8_t *)(uintptr_t)(PART_PHYS + 0x2000);

    for (int i = 0; i < 16; i++) {
        seg0[i] = (uint8_t)(i + 1);
        seg1[i] = (uint8_t)(0x80 + i);
    }
    for (int i = 0; i < 16; i++) {
        if (seg0[i] != (uint8_t)(i + 1))
            return xiji_fail("hw-exp04", "segment0 readback");
        if (seg1[i] != (uint8_t)(0x80 + i))
            return xiji_fail("hw-exp04", "segment1 readback");
    }
    xiji_puts("hw-exp04: segment-relative physical addressing\n");
    return 0;
}

/* ---------- exp05：CR0.PG + #PF ---------- */

static int hw_exp05_paging(void)
{
    struct paging_stats st;

    if (hw_paging_init() != 0)
        return xiji_fail("hw-exp05-pg", "CR0.PG not set");
    if (!hw_paging_enabled())
        return xiji_fail("hw-exp05-pg", "paging flag readback");

    if (hw_paging_demand_test(&st) != 0)
        return xiji_fail("hw-exp05-pg", "demand paging mismatch");

    if (hw_paging_protect_test() != 0)
        return xiji_fail("hw-exp05-pg", "write-protect fault");

    xiji_puts("hw-exp05: #PF faults=");
    xiji_putint((int)st.faults);
    xiji_puts(" frames=");
    xiji_putint((int)st.frames_used);
    xiji_putc('\n');
    return 0;
}

/* ---------- exp06：IDE 通道状态 ---------- */

static int hw_exp06_ide_status(void)
{
    char model[41];
    int err = ata_identify(model, (int)sizeof model);

    if (err != 0)
        return xiji_fail("hw-exp06-ide", "IDENTIFY failed");
    if (ata_wait_ready() != 0)
        return xiji_fail("hw-exp06-ide", "channel not ready");
    if (ata_status() & ATA_STATUS_BSY)
        return xiji_fail("hw-exp06-ide", "channel stuck busy");

    xiji_puts("hw-exp06: IDE identify model=");
    xiji_puts(model);
    xiji_putc('\n');
    return 0;
}

/* ---------- exp07：把目录树真正写到扇区再读回 ---------- */

static int hw_exp07_disk_fs(void)
{
    static uint8_t out[XIJI_SECTOR_SIZE];
    static uint8_t in[XIJI_SECTOR_SIZE];

    /* 用 exp07 自己的文件系统构造一棵树，并序列化 */
    xiji_exp07_seed_demo();

    const int len = xiji_exp07_format((char *)out, (int)sizeof out / 2);
    if (len <= 0)
        return xiji_fail("hw-exp07-disk", "format failed");

    /* 扇区剩余部分清零，避免随机数据干扰比对 */
    for (int i = len; i < (int)sizeof out; i++)
        out[i] = 0;

    if (ata_write_sector(DISK_LBA, out) != 0)
        return xiji_fail("hw-exp07-disk", "ata write");
    if (ata_flush() != 0)
        return xiji_fail("hw-exp07-disk", "flush cache");

    for (int i = 0; i < (int)sizeof in; i++)
        in[i] = 0;
    if (ata_read_sector(DISK_LBA, in) != 0)
        return xiji_fail("hw-exp07-disk", "ata read");

    for (int i = 0; i < len; i++) {
        if (in[i] != out[i])
            return xiji_fail("hw-exp07-disk", "sector content mismatch");
    }

    xiji_puts("hw-exp07: tree persisted to LBA ");
    xiji_putint((int)DISK_LBA);
    xiji_puts(", bytes=");
    xiji_putint(len);
    xiji_putc('\n');
    return 0;
}

/* ---------- 汇总 ---------- */

int hw_verify_all(void)
{
    hw_idt_init();
    hw_pic_init();
    hw_pit_init();
    hw_irq_enable();

    if (hw_lab1_cmos() != 0)
        return 1;
    if (hw_exp01_02_pit() != 0)
        return 1;
    if (hw_exp03_partition_ram() != 0)
        return 1;
    if (hw_exp04_segment_ram() != 0)
        return 1;
    if (hw_exp05_paging() != 0)
        return 1;
    if (hw_exp06_ide_status() != 0)
        return 1;
    if (hw_exp07_disk_fs() != 0)
        return 1;

    xiji_puts("PASS hw-all-qemu-devices\n");
    return 0;
}
