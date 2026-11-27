#ifndef XIJI_HW_ATA_H
#define XIJI_HW_ATA_H

#include <stdint.h>

#define XIJI_SECTOR_SIZE 512

#define ATA_STATUS_BSY 0x80
#define ATA_STATUS_RDY 0x40
#define ATA_STATUS_DF 0x20
#define ATA_STATUS_DRQ 0x08
#define ATA_STATUS_ERR 0x01

/* 返回原始状态字节，便于在测试里断言 */
uint8_t ata_status(void);

/* 等待 !BSY；超时返回 -1 */
int ata_wait_ready(void);

/* 等待 DRQ 且无错误；超时 -1，设备错误 -2 */
int ata_wait_drq(void);

int ata_read_sector(uint32_t lba, void *buf);
int ata_write_sector(uint32_t lba, const void *buf);

/* 写后刷盘，确保数据真正落到介质上（QEMU 里也会走完整命令路径） */
int ata_flush(void);

/* 识别命令：验证设备在线并能报告型号（仅取前 8 字节做存在性判断） */
int ata_identify(char *model_out, int cap);

#endif
