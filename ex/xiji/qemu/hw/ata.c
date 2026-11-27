#include "ata.h"
#include "ports.h"
#include <stdint.h>

enum {
    ATA_DATA = 0x1f0,
    ATA_ERR = 0x1f1,
    ATA_SECCNT = 0x1f2,
    ATA_LBA0 = 0x1f3,
    ATA_LBA1 = 0x1f4,
    ATA_LBA2 = 0x1f5,
    ATA_DRIVE = 0x1f6,
    ATA_CMD = 0x1f7,
    ATA_CMD_READ = 0x20,
    ATA_CMD_WRITE = 0x30,
};

static int ata_wait_nobusy(void)
{
    for (int i = 0; i < 200000; i++) {
        uint8_t st = inb(ATA_CMD);
        if (!(st & ATA_STATUS_BSY))
            return 0;
    }
    return -1;
}

uint8_t ata_status(void)
{
    return inb(ATA_CMD);
}

int ata_wait_ready(void)
{
    return ata_wait_nobusy();
}

static int ata_wait_drq(void)
{
    for (int i = 0; i < 200000; i++) {
        uint8_t st = inb(ATA_CMD);
        if (st & ATA_STATUS_DF)
            return -2;
        if (st & ATA_STATUS_ERR)
            return -3;
        if (!(st & ATA_STATUS_BSY) && (st & ATA_STATUS_DRQ))
            return 0;
    }
    return -1;
}

static int ata_issue_lba28(uint32_t lba, uint8_t cmd)
{
    if (lba >= (1u << 28))
        return -1;
    if (ata_wait_nobusy() != 0)
        return -2;
    outb(1, ATA_SECCNT);
    outb(lba & 0xff, ATA_LBA0);
    outb((lba >> 8) & 0xff, ATA_LBA1);
    outb((lba >> 16) & 0xff, ATA_LBA2);
    outb(0xe0 | ((lba >> 24) & 0x0f), ATA_DRIVE);
    outb(cmd, ATA_CMD);
    return 0;
}

int ata_read_sector(uint32_t lba, void *buf)
{
    if (ata_issue_lba28(lba, ATA_CMD_READ) != 0)
        return -1;
    if (ata_wait_drq() != 0)
        return -2;
    uint16_t *w = buf;
    for (int i = 0; i < 256; i++)
        w[i] = inw(ATA_DATA);
    return 0;
}

int ata_write_sector(uint32_t lba, const void *buf)
{
    if (ata_issue_lba28(lba, ATA_CMD_WRITE) != 0)
        return -1;
    if (ata_wait_drq() != 0)
        return -2;
    const uint16_t *w = buf;
    for (int i = 0; i < 256; i++)
        outw(w[i], ATA_DATA);
    if (ata_wait_nobusy() != 0)
        return -3;
    return 0;
}