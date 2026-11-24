#include "fs/bio.h"

#include "lib/kprintf.h"
#include "lib/string.h"

static uint8_t ramdisk[RAMDISK_BLOCKS * BLOCK_SIZE];
static struct buf buffers[NBUF];
static uint32_t use_clock;
static uint32_t hit_count;
static uint32_t miss_count;

static void ramdisk_read(uint32_t blockno, void *dst)
{
    memcpy(dst, ramdisk + blockno * BLOCK_SIZE, BLOCK_SIZE);
}

static void ramdisk_write(uint32_t blockno, const void *src)
{
    memcpy(ramdisk + blockno * BLOCK_SIZE, src, BLOCK_SIZE);
}

void bio_init(void)
{
    memset(ramdisk, 0, sizeof(ramdisk));
    memset(buffers, 0, sizeof(buffers));
    use_clock = 1;
    hit_count = 0;
    miss_count = 0;
}

static struct buf *find_buf(uint32_t blockno)
{
    for (int i = 0; i < NBUF; i++) {
        if (buffers[i].valid != 0 && buffers[i].blockno == blockno) {
            buffers[i].last_use = ++use_clock;
            hit_count++;
            return &buffers[i];
        }
    }
    return 0;
}

static struct buf *evict_buf(void)
{
    struct buf *best = &buffers[0];

    for (int i = 1; i < NBUF; i++) {
        if (buffers[i].valid == 0) {
            return &buffers[i];
        }
        if (buffers[i].last_use < best->last_use) {
            best = &buffers[i];
        }
    }
    if (best->dirty != 0) {
        ramdisk_write(best->blockno, best->data);
    }
    return best;
}

struct buf *bread(uint32_t blockno)
{
    struct buf *b = find_buf(blockno);

    if (b != 0) {
        return b;
    }
    b = evict_buf();
    b->blockno = blockno;
    b->valid = 1;
    b->dirty = 0;
    b->last_use = ++use_clock;
    ramdisk_read(blockno, b->data);
    miss_count++;
    return b;
}

void bwrite(struct buf *b)
{
    b->dirty = 1;
    ramdisk_write(b->blockno, b->data);
}

void brelse(struct buf *b)
{
    (void)b;
}

void bio_selftest(void)
{
    struct buf *b = bread(200);

    memset(b->data, 0, BLOCK_SIZE);
    memcpy(b->data, "OSLAB-BIO", 9);
    bwrite(b);
    brelse(b);

    b = bread(200);
    if (memcmp(b->data, "OSLAB-BIO", 9) != 0) {
        kprintf("[bio] pattern rw FAILED\n");
    } else {
        kprintf("[bio] pattern rw ok; hit=%d miss=%d\n",
                (int)hit_count, (int)miss_count);
    }
    brelse(b);
}

uint32_t bio_hits(void)
{
    return hit_count;
}

uint32_t bio_misses(void)
{
    return miss_count;
}
