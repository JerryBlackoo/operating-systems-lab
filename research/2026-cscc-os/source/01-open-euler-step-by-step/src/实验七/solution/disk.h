#ifndef DISK_H
#define DISK_H

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>

#define BLOCK_SIZE 512
#define DISK_SIZE (64 * 1024 * 1024)  // 64MB virtual disk
#define TOTAL_BLOCKS (DISK_SIZE / BLOCK_SIZE)

typedef struct {
    FILE *fp;
    char *name;
    int size;
    int total_blocks;
} Disk;

Disk* disk_init(const char *filename);
int disk_read(Disk *disk, int block_num, void *buffer);
int disk_write(Disk *disk, int block_num, const void *buffer);
void disk_close(Disk *disk);

#endif
