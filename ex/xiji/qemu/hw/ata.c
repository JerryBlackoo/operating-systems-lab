#include "ata.h"
#include "ports.h"
#include <stdint.h>

/*
 * ATA PIO（LBA28）最小驱动
 *
 * 相比原实现补上了三处：
 *   1. 命令发出后先等 BSY 清零再等 DRQ —— 只等 DRQ 会在慢盘上误判；
 *   2. 每次传输都检查 ERR/DF，别把错误当成功；
 *   3. 写入后发 FLUSH CACHE（0xE7），否则数据只在驱动器缓存里，
 *      掉电即丢，所谓"持久化"并不成立。
 */
enum {
    ATA_DATA = 0x1f0,
    ATA_ERROR = 0x1f1,
    ATA_SECCOUNT = 0x1f2,
    ATA_LBA0 = 0x1f3,
    ATA_LBA1 = 0x1f4,
    ATA_LBA2 = 0x1f5,
    ATA_DRIVE = 0x1f6,
    ATA_STATUS_CMD = 0x1f7,
    ATA_CMD_READ = 0x20,
    ATA_CMD_WRITE = 0x30,
    ATA_CMD_FLUSH = 0xe7,
    ATA_CMD_IDENTIFY = 0xec,
};

#define ATA_SPIN_LIMIT 1000000

uint8_t ata_status(void)
{
    return inb(ATA_STATUS_CMD);
}

int ata_wait_ready(void)
{
    for (int i = 0; i < ATA_SPIN_LIMIT; i++) {
        uint8_t st = inb(ATA_STATUS_CMD);

        if (st == 0xff)
            return -2; /* 无设备：总线浮空 */
        if (!(st & ATA_STATUS_BSY))
            return (st & ATA_STATUS_ERR) ? -3 : 0;
    }
    return -1;
}

int ata_wait_drq(void)
{
    for (int i = 0; i < ATA_SPIN_LIMIT; i++) {
        uint8_t st = inb(ATA_STATUS_CMD);

        if (st & (ATA_STATUS_ERR | ATA_STATUS_DF))
            return -2;
        if (!(st & ATA_STATUS_BSY) && (st & ATA_STATUS_DRQ))
            return 0;
    }
    return -1;
}

static int ata_select_lba28(uint32_t lba, uint8_t cmd)
{
    int err = ata_wait_ready();

    if (err)
        return err;
    if (lba >= (1u << 28))
        return -4;

    outb(0, ATA_SECCOUNT); /* 先清计数，避免残留 */
    outb(1, ATA_SECCOUNT);
    outb((uint8_t)(lba & 0xff), ATA_LBA0);
    outb((uint8_t)((lba >> 8) & 0xff), ATA_LBA1);
    outb((uint8_t)((lba >> 16) & 0xff), ATA_LBA2);
    outb((uint8_t)(0xe0 | ((lba >> 24) & 0x0f)), ATA_DRIVE);
    outb(cmd, ATA_STATUS_CMD);
    return 0;
}

int ata_read_sector(uint32_t lba, void *buf)
{
    uint16_t *w = buf;
    int err = ata_select_lba28(lba, ATA_CMD_READ);

    if (err)
        return err;
    err = ata_wait_drq();
    if (err)
        return err;
    for (int i = 0; i < XIJI_SECTOR_SIZE / 2; i++)
        w[i] = inw(ATA_DATA);
    return ata_wait_ready();
}

int ata_write_sector(uint32_t lba, const void *buf)
{
    const uint16_t *w = buf;
    int err = ata_select_lba28(lba, ATA_CMD_WRITE);

    if (err)
        return err;
    err = ata_wait_drq();
    if (err)
        return err;
    for (int i = 0; i < XIJI_SECTOR_SIZE / 2; i++)
        outw(w[i], ATA_DATA);
    return ata_wait_ready();
}

int ata_flush(void)
{
    int err = ata_wait_ready();

    if (err)
        return err;
    outb(ATA_CMD_FLUSH, ATA_STATUS_CMD);
    return ata_wait_ready();
}

int ata_identify(char *model_out, int cap)
{
    uint16_t id[256];
    int err;

    if (!model_out || cap <= 0)
        return -1;
    err = ata_select_lba28(0, ATA_CMD_IDENTIFY);
    if (err)
        return err;
    if (inb(ATA_STATUS_CMD) == 0)
        return -2; /* 无设备 */
    err = ata_wait_drq();
    if (err)
        return err;
    for (int i = 0; i < 256; i++)
        id[i] = inw(ATA_DATA);

    /* 型号在 word 27..46，按 ATA 规范是大端字序 */
    int n = 0;
    for (int i = 27; i <= 46 && n + 1 < cap; i++) {
        model_out[n++] = (char)(id[i] >> 8);
        model_out[n++] = (char)(id[i] & 0xff);
    }
    while (n > 0 && (model_out[n - 1] == ' ' || model_out[n - 1] == '\0'))
        n--;
    model_out[n] = '\0';
    return 0;
}
