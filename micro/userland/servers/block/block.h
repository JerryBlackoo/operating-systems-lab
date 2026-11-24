#ifndef MICRO_MODEL_BLOCK_H
#define MICRO_MODEL_BLOCK_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/*
 * 所有块设备共用的请求策略：每设备一条有界 FIFO 和一个在途请求槽。
 *
 *   QUEUED ──dispatch 且驱动接收──> IN_FLIGHT ──complete 匹配──> DONE
 *      └────dispatch 且驱动永久拒绝──────────────────────────> DONE
 *
 * 驱动返回 BLOCK_BUSY 时队首原地保留，不移入在途槽，也不交付完成。
 * 本层只处理逻辑块和缓冲区，不碰任何硬件寄存器或传输协议。
 */

enum { BLOCK_DEVICES = 2, BLOCK_PENDING = 4 };
enum block_op { BLOCK_READ, BLOCK_WRITE };
enum block_state { BLOCK_QUEUED, BLOCK_IN_FLIGHT, BLOCK_DONE };
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
    enum block_state state; /* 驱动 submit 看到 QUEUED；done 看到 DONE。 */
    uint64_t lba, blocks;
    size_t bytes;
    struct block_buffer *buffer;
};

struct block_port {
    void *ctx;
    /* 复制请求内容后立即返回；完成必须作为后续事件通过 block_complete 交付。 */
    enum block_status (*submit)(void *, const struct block_request *);
    /* 缓冲区已归还。成功时 bytes 为传输字节数，失败时为零。 */
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
