#ifndef KERNEL_FS_BIO_H
#define KERNEL_FS_BIO_H

#include <stdint.h>

#define BLOCK_SIZE 4096
#define RAMDISK_BLOCKS 256
#define NBUF 16

struct buf {
    int valid;
    int dirty;
    uint32_t blockno;
    uint32_t last_use;
    uint8_t data[BLOCK_SIZE];
};

void bio_init(void);
struct buf *bread(uint32_t blockno);
void bwrite(struct buf *b);
void brelse(struct buf *b);
void bio_selftest(void);

uint32_t bio_hits(void);
uint32_t bio_misses(void);

#endif
