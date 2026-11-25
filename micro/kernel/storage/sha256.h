#ifndef MICRO_KERNEL_SHA256_H
#define MICRO_KERNEL_SHA256_H

#include <stddef.h>
#include <stdint.h>

void sha256_digest(const void *data, size_t len, uint8_t out[32]);
void sha256_hex(const void *data, size_t len, char hex[65]);

struct sha256_ctx {
    uint32_t state[8];
    uint8_t block[64];
    size_t idx;
    uint64_t total;
};

void sha256_init(struct sha256_ctx *ctx);
void sha256_update(struct sha256_ctx *ctx, const void *data, size_t len);
void sha256_finish(struct sha256_ctx *ctx, uint8_t out[32]);
void sha256_finish_hex(struct sha256_ctx *ctx, char hex[65]);

#endif