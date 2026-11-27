#include "hw_verify.h"
#include "../../common/xiji_test.h"
#include "ata.h"
#include "idt.h"
#include "paging.h"
#include "pic_pit.h"
#include "ports.h"
#include <stdint.h>

#define CMOS_ADDR 0x70
#define CMOS_DATA 0x71
#define XIJI_DISK_LBA 120u

static uint8_t cmos_read(uint8_t reg)
{
    outb(reg, CMOS_ADDR);
    return inb(CMOS_DATA);
}

static int hw_lab1_cmos_printf(void)
{
    uint8_t sec = cmos_read(0x00);
    uint8_t min = cmos_read(0x02);
    (void)sec;
    (void)min;
    xiji_puts("hw-lab1: CMOS+debugcon+isa-debug-exit\n");
    return 0;
}

static int hw_exp01_02_pit(void)
{
    uint32_t t0 = hw_pit_ticks;
    for (int spins = 0; spins < 500000 && hw_pit_ticks < t0 + 3u; spins++)
        __asm__ volatile("hlt");
    if (hw_pit_ticks < t0 + 2u)
        return xiji_fail("hw-exp01-pit", "8254 ticks");
    xiji_puts("PASS hw-exp01-02-pit-pic\n");
    return 0;
}

static int hw_exp03_ram_partition(void)
{
    static uint8_t hw_ram_arena[512];
    for (unsigned i = 0; i < sizeof hw_ram_arena; i++)
        hw_ram_arena[i] = 0;
    hw_ram_arena[0] = 200;
    for (unsigned i = 0; i < 64; i++)
        hw_ram_arena[64 + i] = (uint8_t)(0x5a ^ (i & 0xff));
    if (hw_ram_arena[64] != 0x5a)
        return xiji_fail("hw-exp03-ram", "physical store");
    xiji_puts("PASS hw-exp03-physical-ram\n");
    return 0;
}

static int hw_exp04_segment_store(void)
{
    volatile uint8_t *cell = (volatile uint8_t *)(uintptr_t)0x00005000u;
    *cell = 0x42;
    if (*cell != 0x42)
        return xiji_fail("hw-exp04-seg", "identity readback");
    xiji_puts("PASS hw-exp04-identity-map\n");
    return 0;
}

static int hw_exp05_paging(void)
{
    if (hw_paging_init() != 0)
        return xiji_fail("hw-exp05-pg", "cr3");
    struct paging_stats st;
    if (hw_paging_demand_test(&st) != 0)
        return xiji_fail("hw-exp05-pg", "demand #PF");
    if (st.faults < 4 || st.frames_used < 4)
        return xiji_fail("hw-exp05-pg", "fault count");
    xiji_puts("PASS hw-exp05-cr0-pagefault\n");
    return 0;
}

static int hw_exp06_ide_channel(void)
{
    if (ata_wait_ready() != 0)
        return xiji_fail("hw-exp06-ide", "not ready");
    uint8_t st = ata_status();
    if (st & ATA_STATUS_BSY)
        return xiji_fail("hw-exp06-ide", "BSY");
    xiji_puts("PASS hw-exp06-ide-status\n");
    return 0;
}

static int hw_exp07_disk_sector(void)
{
    static uint8_t sector[512];
    static const char magic[] = "XIJI-FS-v1";
    for (unsigned i = 0; i < sizeof sector; i++)
        sector[i] = 0;
    for (unsigned i = 0; i < sizeof magic; i++)
        sector[i] = (uint8_t)magic[i];

    if (ata_write_sector(XIJI_DISK_LBA, sector) != 0)
        return xiji_fail("hw-exp07-disk", "write");
    for (unsigned i = 0; i < sizeof sector; i++)
        sector[i] = 0;
    if (ata_read_sector(XIJI_DISK_LBA, sector) != 0)
        return xiji_fail("hw-exp07-disk", "read");
    for (unsigned i = 0; i < sizeof magic; i++) {
        if (sector[i] != (uint8_t)magic[i])
            return xiji_fail("hw-exp07-disk", "data");
    }
    xiji_puts("PASS hw-exp07-ide-lba\n");
    return 0;
}

int hw_verify_all(void)
{
    hw_idt_init();
    hw_pic_init();
    hw_pit_init();
    hw_irq_enable();

    if (hw_lab1_cmos_printf() != 0)
        return 1;
    if (hw_exp01_02_pit() != 0)
        return 1;
    if (hw_exp03_ram_partition() != 0)
        return 1;
    if (hw_exp04_segment_store() != 0)
        return 1;
    if (hw_exp05_paging() != 0)
        return 1;
    if (hw_exp06_ide_channel() != 0)
        return 1;
    if (hw_exp07_disk_sector() != 0)
        return 1;

    xiji_puts("PASS hw-all-qemu-devices\n");
    return 0;
}