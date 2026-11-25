#ifndef MICRO_VIRTIO_BLK_H
#define MICRO_VIRTIO_BLK_H

#include <stddef.h>
#include <stdint.h>

#include "block.h"

struct virtio_blk;

int virtio_blk_probe_mmio(uintptr_t base);
int virtio_blk_init(struct virtio_blk **out, struct block_service *svc, unsigned dev);
enum block_status virtio_blk_submit(struct virtio_blk *dev, const struct block_request *request);
void virtio_blk_on_irq(struct virtio_blk *dev);
void virtio_blk_poll(struct virtio_blk *dev);
unsigned virtio_blk_irq_line(const struct virtio_blk *dev);

#endif