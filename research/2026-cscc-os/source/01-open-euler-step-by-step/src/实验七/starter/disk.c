#include "disk.h"

Disk* disk_init(const char *filename) {
    Disk *disk = (Disk *)malloc(sizeof(Disk));
    if (!disk) return NULL;

    disk->name = strdup(filename);
    disk->total_blocks = TOTAL_BLOCKS;
    disk->size = DISK_SIZE;

    disk->fp = fopen(filename, "r+b");
    if (!disk->fp) {
        disk->fp = fopen(filename, "w+b");
        if (!disk->fp) {
            free(disk->name);
            free(disk);
            return NULL;
        }
        // TODO: 初始化磁盘内容为零
        char zero[BLOCK_SIZE] = {0};
        for (int i = 0; i < TOTAL_BLOCKS; i++) {
            fwrite(zero, 1, BLOCK_SIZE, disk->fp);
        }
    }
    return disk;
}

int disk_read(Disk *disk, int block_num, void *buffer) {
    // TODO: 检查块号有效性 (block_num < 0 || block_num >= disk->total_blocks)
    fseek(disk->fp, block_num * BLOCK_SIZE, SEEK_SET);
    size_t read_size = fread(buffer, 1, BLOCK_SIZE, disk->fp);
    return (read_size == BLOCK_SIZE) ? 0 : -1;
}

int disk_write(Disk *disk, int block_num, const void *buffer) {
    // TODO: 检查块号有效性
    fseek(disk->fp, block_num * BLOCK_SIZE, SEEK_SET);
    size_t write_size = fwrite(buffer, 1, BLOCK_SIZE, disk->fp);
    fflush(disk->fp);
    return (write_size == BLOCK_SIZE) ? 0 : -1;
}

void disk_close(Disk *disk) {
    if (disk) {
        if (disk->fp) fclose(disk->fp);
        if (disk->name) free(disk->name);
        free(disk);
    }
}
