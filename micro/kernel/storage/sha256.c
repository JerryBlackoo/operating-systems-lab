#include "sha256.h"

#include "demo.h"

static uint32_t rotr(uint32_t x, unsigned n)
{
    return (x >> n) | (x << (32U - n));
}

static const uint32_t k[64] = {
    0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U, 0x3956c25bU, 0x59f111f1U,
    0x923f82a4U, 0xab1c5ed5U, 0xd807aa98U, 0x12835b01U, 0x243185beU, 0x550c7dc3U,
    0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U, 0xc19bf174U, 0xe49b69c1U, 0xefbe4786U,
    0x0fc19dc6U, 0x240ca1ccU, 0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU,
    0x983e5152U, 0xa831c66dU, 0xb00327c8U, 0xbf597fc7U, 0xc6e00bf3U, 0xd5a79147U,
    0x06ca6351U, 0x14292967U, 0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU, 0x53380d13U,
    0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U, 0xa2bfe8a1U, 0xa81a664bU,
    0xc24b8b70U, 0xc76c51a3U, 0xd192e819U, 0xd6990624U, 0xf40e3585U, 0x106aa070U,
    0x19a4c116U, 0x1e376c08U, 0x2748774cU, 0x34b0bcb5U, 0x391c0cb3U, 0x4ed8aa4aU,
    0x5b9cca4fU, 0x682e6ff3U, 0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
    0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
};

static void compress(uint32_t state[8], const uint8_t block[64])
{
    uint32_t w[64];

    for (unsigned i = 0; i < 16; i++) {
        w[i] = ((uint32_t)block[i * 4] << 24) | ((uint32_t)block[i * 4 + 1] << 16) |
               ((uint32_t)block[i * 4 + 2] << 8) | (uint32_t)block[i * 4 + 3];
    }
    for (unsigned i = 16; i < 64; i++) {
        uint32_t s0 = rotr(w[i - 15], 7) ^ rotr(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = rotr(w[i - 2], 17) ^ rotr(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }

    uint32_t a = state[0];
    uint32_t b = state[1];
    uint32_t c = state[2];
    uint32_t d = state[3];
    uint32_t e = state[4];
    uint32_t f = state[5];
    uint32_t g = state[6];
    uint32_t h = state[7];

    for (unsigned i = 0; i < 64; i++) {
        uint32_t s1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
        uint32_t ch = (e & f) ^ (~e & g);
        uint32_t t1 = h + s1 + ch + k[i] + w[i];
        uint32_t s0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
        uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
        uint32_t t2 = s0 + maj;
        h = g;
        g = f;
        f = e;
        e = d + t1;
        d = c;
        c = b;
        b = a;
        a = t1 + t2;
    }

    state[0] += a;
    state[1] += b;
    state[2] += c;
    state[3] += d;
    state[4] += e;
    state[5] += f;
    state[6] += g;
    state[7] += h;
}

void sha256_init(struct sha256_ctx *ctx)
{
    static const uint32_t init[8] = {0x6a09e667U, 0xbb67ae85U, 0x3c6ef372U,
                                   0xa54ff53aU, 0x510e527fU, 0x9b05688cU,
                                   0x1f83d9abU, 0x5be0cd19U};

    if (ctx == 0) {
        return;
    }
    for (unsigned i = 0; i < 8; i++) {
        ctx->state[i] = init[i];
    }
    ctx->idx = 0;
    ctx->total = 0;
}

void sha256_update(struct sha256_ctx *ctx, const void *data, size_t len)
{
    const uint8_t *in;

    if (ctx == 0 || (len != 0 && data == 0)) {
        return;
    }
    in = data;
    ctx->total += len;
    while (len > 0) {
        size_t n = 64 - ctx->idx;

        if (n > len) {
            n = len;
        }
        memcpy(ctx->block + ctx->idx, in, n);
        ctx->idx += n;
        in += n;
        len -= n;
        if (ctx->idx == 64) {
            compress(ctx->state, ctx->block);
            ctx->idx = 0;
        }
    }
}

void sha256_finish(struct sha256_ctx *ctx, uint8_t out[32])
{
    uint64_t bits;

    if (ctx == 0 || out == 0) {
        return;
    }
    bits = ctx->total * 8U;
    ctx->block[ctx->idx++] = 0x80;
    if (ctx->idx > 56) {
        while (ctx->idx < 64) {
            ctx->block[ctx->idx++] = 0;
        }
        compress(ctx->state, ctx->block);
        ctx->idx = 0;
    }
    while (ctx->idx < 56) {
        ctx->block[ctx->idx++] = 0;
    }
    for (int i = 7; i >= 0; i--) {
        ctx->block[ctx->idx++] = (uint8_t)(bits >> (8 * i));
    }
    compress(ctx->state, ctx->block);

    for (unsigned i = 0; i < 8; i++) {
        out[i * 4] = (uint8_t)(ctx->state[i] >> 24);
        out[i * 4 + 1] = (uint8_t)(ctx->state[i] >> 16);
        out[i * 4 + 2] = (uint8_t)(ctx->state[i] >> 8);
        out[i * 4 + 3] = (uint8_t)ctx->state[i];
    }
}

void sha256_digest(const void *data, size_t len, uint8_t out[32])
{
    struct sha256_ctx ctx;

    sha256_init(&ctx);
    sha256_update(&ctx, data, len);
    sha256_finish(&ctx, out);
}

static void digest_to_hex(const uint8_t digest[32], char hex[65])
{
    static const char digits[] = "0123456789abcdef";

    for (unsigned i = 0; i < 32; i++) {
        hex[i * 2] = digits[digest[i] >> 4];
        hex[i * 2 + 1] = digits[digest[i] & 0xf];
    }
    hex[64] = '\0';
}

void sha256_hex(const void *data, size_t len, char hex[65])
{
    uint8_t digest[32];

    sha256_digest(data, len, digest);
    digest_to_hex(digest, hex);
}

void sha256_finish_hex(struct sha256_ctx *ctx, char hex[65])
{
    uint8_t digest[32];

    sha256_finish(ctx, digest);
    digest_to_hex(digest, hex);
}