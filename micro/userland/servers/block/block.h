#ifndef MICRO_MODEL_BLOCK_H
#define MICRO_MODEL_BLOCK_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

enum { BLOCK_DEVICES = 2, BLOCK_PENDING = 4 };
enum block_op { BLOCK_READ, BLOCK_WRITE };
enum block_status { BLOCK_OK, BLOCK_BAD_ARG, BLOCK_BUSY, BLOCK_IO_ERROR, BLOCK_EMPTY };

struct block_buffer {
    void *data;
    size_t size;
    bool readable, writable, borrowed;
};

struct block_request {
    uint64_t id;
    uint32_t caller;
    unsigned dev;
    enum block_op op;
    uint64_t lba, blocks;
    size_t bytes;
    struct block_buffer *buffer;
};

struct block_port {
    void *ctx;
    /* Copy the request before returning. Deliver completion as a later event. */
    enum block_status (*submit)(void *, const struct block_request *);
    void (*done)(void *, const struct block_request *, enum block_status, size_t bytes);
};

struct block_device {
    uint64_t capacity;
    size_t logical_bytes;
    struct block_request pending[BLOCK_PENDING], in_flight;
    unsigned head, len;
};

struct block_service {
    const struct block_port *port;
    uint64_t last_id;
    struct block_device devices[BLOCK_DEVICES];
};

enum block_status block_submit(struct block_service *, uint32_t caller, unsigned dev,
                               enum block_op, uint64_t lba, uint64_t blocks,
                               struct block_buffer *, uint64_t *id);
enum block_status block_dispatch(struct block_service *, unsigned dev);
bool block_complete(struct block_service *, unsigned dev, uint64_t id, enum block_status);

#endif
