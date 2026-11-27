#ifndef XIJI_HW_ATA_H
#define XIJI_HW_ATA_H

#include <stdint.h>

#define ATA_STATUS_BSY 0x80
#define ATA_STATUS_DRQ 0x08
#define ATA_STATUS_DF  0x20
#define ATA_STATUS_ERR 0x01

int ata_wait_ready(void);
int ata_read_sector(uint32_t lba, void *buf);
int ata_write_sector(uint32_t lba, const void *buf);
uint8_t ata_status(void);

#endif