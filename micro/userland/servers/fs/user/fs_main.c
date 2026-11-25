/*
 * fs_main.c — U-mode 文件系统服务（M7 / A1）。
 *
 * 【A1：策略层在 U-mode】
 *
 * 2026-11-25 之前这个文件只是一个转发壳：路径交给内核，由内核里的
 * ext4/vfs 解析。现在 ext4、vfs 都链进这个 U-mode ELF，服务自己：
 *
 *   - 通过 ABI_BLOCK_INFO 问内核要容量，用 ABI_BLOCK_READ 读原始块；
 *   - 自己 mount ext4、自己走 vfs 解析路径；
 *   - 把读到的数据按 24 字节分块回给客户端。
 *
 * 内核侧只剩 virtio-blk 驱动和 block 队列，不再出现任何路径/inode 逻辑。
 *
 * shell 经 IPC 送 TAG_FS_PATH（分块路径）+ TAG_FS_READ（offset/len），
 * 应答按 service_proto 的 24 字节分块。
 */
#include "abi/abi.h"
#include "runtime.h"
#include "service_proto.h"
#include "user_io.h"

#include <stdbool.h>

#include "ext4.h"
#include "vfs.h"

#define FS_ROOT_CLIENT 1

static char fs_path[SVC_MAX_PATH + 1];
static size_t fs_path_len;
/*
 * 缓存上一次成功打开的 fd。
 *
 * 客户端按 24 字节分块读文件（`cat /largefile` 是 200 KiB → 约 8500 次
 * `TAG_FS_READ`）。若每次都重新 vfs_open，就要重复做 8500 次路径解析
 * （每层 ext4_lookup 都要读目录块）。这里让同一路径的连续读共用一次 open：
 * 收到新的 `TAG_FS_PATH` 或路径改变时关闭旧 fd。
 */
static unsigned fs_open_fd;
static bool fs_fd_valid;

/*
 * 服务层读预取缓冲。
 *
 * 客户端按 24 字节分块读文件，但一次 `vfs_read_at` 的固定开销（`load_inode`
 * 读 inode 表块 + 解析 extent + 映射块）与读多少字节无关。这里一次向
 * vfs 要一整块，把后续约 170 次小读变成纯内存拷贝。
 *
 * 放在静态区而不是栈上：FS 线程的用户栈是 16 KiB，syscall 路径上已经有
 * 多层调用，4 KiB 的栈缓冲会明显压缩余量。
 */
#define FS_PREFETCH_BYTES 4096

static uint8_t fs_prefetch[FS_PREFETCH_BYTES];
static uint64_t fs_prefetch_base; /* 缓冲首字节对应的文件偏移 */
static size_t fs_prefetch_len;    /* 有效字节数；0 表示无有效数据 */
static bool fs_prefetch_valid;

/* 挂载点。U-mode 服务启动时建立，整个服务生命周期有效。 */
static struct ext4 fs_ext4;
static struct vfs fs_vfs;
static struct vfs_port fs_vfs_port;
static struct ext4_blk_io fs_blk_io;
static bool fs_mounted;

static void pack24(const unsigned char *data, size_t n, uint64_t words[3])
{
    words[0] = 0;
    words[1] = 0;
    words[2] = 0;
    for (size_t i = 0; i < n && i < SVC_DATA_BYTES; i++) {
        words[i / 8U] |= (uint64_t)data[i] << (8U * (i % 8U));
    }
}

static void unpack16(uint64_t w2, uint64_t w3, unsigned char out[SVC_PATH_CHUNK])
{
    for (unsigned i = 0; i < 8; i++) {
        out[i] = (unsigned char)((w2 >> (8U * i)) & 0xffU);
        out[8 + i] = (unsigned char)((w3 >> (8U * i)) & 0xffU);
    }
}

static void reply_msg(uint32_t reply, uint32_t tag, uint64_t w0, uint64_t w1,
                      uint64_t w2, uint64_t w3)
{
    struct abi_message msg = {
        .tag = tag,
        .words = {w0, w1, w2, w3},
    };

    (void)user_ipc_reply(reply, &msg);
}

static char *put_str(char *p, const char *s)
{
    while (*s != '\0') {
        *p++ = *s++;
    }
    return p;
}

static char *put_dec(char *p, uint64_t v)
{
    char tmp[24];
    unsigned n = 0;

    if (v == 0) {
        *p++ = '0';
        return p;
    }
    while (v != 0) {
        tmp[n++] = (char)('0' + v % 10);
        v /= 10;
    }
    while (n != 0) {
        *p++ = tmp[--n];
    }
    return p;
}

/*
 * SHA-256（只读自检用）。
 *
 * 这段代码属于 U-mode 服务的自检，不是文件系统策略；放在这里是为了让
 * M5 判别仍能在启动时打印真实哈希——改镜像后哈希必须变，这条断言现在
 * 由 U-mode 侧产生，内核不再参与。
 */
struct sha256_ctx {
    uint32_t h[8];
    uint64_t len;
    uint8_t buf[64];
    size_t used;
};

static uint32_t rotr32(uint32_t x, unsigned n)
{
    return (x >> n) | (x << (32 - n));
}

static const uint32_t sha256_k[64] = {
    0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U, 0x3956c25bU,
    0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U, 0xd807aa98U, 0x12835b01U,
    0x243185beU, 0x550c7dc3U, 0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U,
    0xc19bf174U, 0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
    0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU, 0x983e5152U,
    0xa831c66dU, 0xb00327c8U, 0xbf597fc7U, 0xc6e00bf3U, 0xd5a79147U,
    0x06ca6351U, 0x14292967U, 0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU,
    0x53380d13U, 0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
    0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U, 0xd192e819U,
    0xd6990624U, 0xf40e3585U, 0x106aa070U, 0x19a4c116U, 0x1e376c08U,
    0x2748774cU, 0x34b0bcb5U, 0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU,
    0x682e6ff3U, 0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
    0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
};

static void sha256_block(struct sha256_ctx *c, const uint8_t *p)
{
    uint32_t w[64];

    for (unsigned i = 0; i < 16; i++) {
        w[i] = ((uint32_t)p[i * 4] << 24) | ((uint32_t)p[i * 4 + 1] << 16) |
               ((uint32_t)p[i * 4 + 2] << 8) | (uint32_t)p[i * 4 + 3];
    }
    for (unsigned i = 16; i < 64; i++) {
        uint32_t s0 = rotr32(w[i - 15], 7) ^ rotr32(w[i - 15], 18) ^
                      (w[i - 15] >> 3);
        uint32_t s1 = rotr32(w[i - 2], 17) ^ rotr32(w[i - 2], 19) ^
                      (w[i - 2] >> 10);

        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    uint32_t a = c->h[0], b = c->h[1], cc = c->h[2], d = c->h[3];
    uint32_t e = c->h[4], f = c->h[5], g = c->h[6], hh = c->h[7];

    for (unsigned i = 0; i < 64; i++) {
        uint32_t s1 = rotr32(e, 6) ^ rotr32(e, 11) ^ rotr32(e, 25);
        uint32_t ch = (e & f) ^ (~e & g);
        uint32_t t1 = hh + s1 + ch + sha256_k[i] + w[i];
        uint32_t s0 = rotr32(a, 2) ^ rotr32(a, 13) ^ rotr32(a, 22);
        uint32_t maj = (a & b) ^ (a & cc) ^ (b & cc);
        uint32_t t2 = s0 + maj;

        hh = g;
        g = f;
        f = e;
        e = d + t1;
        d = cc;
        cc = b;
        b = a;
        a = t1 + t2;
    }
    c->h[0] += a;
    c->h[1] += b;
    c->h[2] += cc;
    c->h[3] += d;
    c->h[4] += e;
    c->h[5] += f;
    c->h[6] += g;
    c->h[7] += hh;
}

static void sha256_init(struct sha256_ctx *c)
{
    static const uint32_t init[8] = {
        0x6a09e667U, 0xbb67ae85U, 0x3c6ef372U, 0xa54ff53aU,
        0x510e527fU, 0x9b05688cU, 0x1f83d9abU, 0x5be0cd19U,
    };

    for (unsigned i = 0; i < 8; i++) {
        c->h[i] = init[i];
    }
    c->len = 0;
    c->used = 0;
}

static void sha256_update(struct sha256_ctx *c, const void *data, size_t n)
{
    const uint8_t *p = data;

    c->len += n;
    while (n != 0) {
        size_t take = 64 - c->used;

        if (take > n) {
            take = n;
        }
        for (size_t i = 0; i < take; i++) {
            c->buf[c->used + i] = p[i];
        }
        c->used += take;
        p += take;
        n -= take;
        if (c->used == 64) {
            sha256_block(c, c->buf);
            c->used = 0;
        }
    }
}

static void sha256_finish_hex(struct sha256_ctx *c, char out[65])
{
    uint64_t bits = c->len * 8;

    /*
     * 标准填充：补 0x80，再补 0 直到 used == 56（留 8 字节写长度）。
     *
     * 单字节 update 会在 used 达到 64 时自动刷块并归零，因此「补到 56」
     * 一定能在有限步内达成；这里不用 `while (used != 56) { update(0) }`
     * 的写法，是因为它在 used 落在 57..63 时语义不直观（要靠刷块绕回），
     * 显式按块填充更容易核对。
     */
    c->buf[c->used++] = 0x80;
    if (c->used > 56) {
        for (size_t i = c->used; i < 64; i++) {
            c->buf[i] = 0;
        }
        sha256_block(c, c->buf);
        c->used = 0;
    }
    for (size_t i = c->used; i < 56; i++) {
        c->buf[i] = 0;
    }
    for (unsigned i = 0; i < 8; i++) {
        c->buf[56 + i] = (uint8_t)(bits >> (56 - i * 8));
    }
    sha256_block(c, c->buf);
    c->used = 0;

    static const char dg[] = "0123456789abcdef";

    for (unsigned i = 0; i < 8; i++) {
        for (int j = 28; j >= 0; j -= 4) {
            *out++ = dg[(c->h[i] >> j) & 0xf];
        }
    }
    *out = '\0';
}

/*
 * 启动自检：读 /motd 与 /largefile 并打印 SHA-256 与字节数。
 *
 * M5 判别依赖这些行：改镜像后哈希必须变。这条断言原本由内核打印，
 * A1 之后由 U-mode `fs` 打印，语义不变。
 */
static void fs_self_check(void)
{
    static const char *const paths[] = {"/motd", "/largefile"};
    uint8_t chunk[EXT4_BLOCK];

    for (unsigned i = 0; i < sizeof(paths) / sizeof(paths[0]); i++) {
        const char *path = paths[i];
        size_t plen = 0;
        struct sha256_ctx ctx;
        unsigned fd = 0;
        uint64_t off = 0;
        size_t total = 0;
        char line[96];
        char *p;
        char hex[65];

        while (path[plen] != '\0') {
            plen++;
        }
        if (vfs_open(&fs_vfs, FS_ROOT_CLIENT, path, &fd) != VFS_OK) {
            p = put_str(line, "[fs] open failed: ");
            p = put_str(p, path);
            *p = '\0';
            user_print(line);
            continue;
        }
        sha256_init(&ctx);
        {
            bool read_error = false;

            for (;;) {
                size_t n = 0;
                enum vfs_result rr = vfs_read_at(&fs_vfs, FS_ROOT_CLIENT, fd,
                                                 off, chunk, sizeof(chunk), &n);

                /*
                 * 读错误不能当成 EOF：那会算出一个「看起来正常」的哈希，
                 * 让 M5 的哈希判别假通过。出错时打印明确的失败行并跳过
                 * 该文件的哈希。
                 */
                if (rr != VFS_OK) {
                    read_error = true;
                    break;
                }
                if (n == 0) {
                    break;
                }
                sha256_update(&ctx, chunk, n);
                total += n;
                off += n;
                if (n < sizeof(chunk)) {
                    break;
                }
            }
            (void)vfs_close(&fs_vfs, FS_ROOT_CLIENT, fd);
            if (read_error) {
                p = put_str(line, "[fs] read error: ");
                p = put_str(p, path);
                *p = '\0';
                user_print(line);
                continue;
            }
        }
        sha256_finish_hex(&ctx, hex);

        p = line;
        if (i == 0) {
            p = put_str(p, "[fs] motd sha256=");
        } else {
            p = put_str(p, "[fs] largefile sha256=");
        }
        p = put_str(p, hex);
        *p = '\0';
        user_print(line);

        if (i == 1) {
            p = line;
            p = put_str(p, "[ext4] read /largefile: ");
            p = put_dec(p, total);
            p = put_str(p, " bytes");
            *p = '\0';
            user_print(line);
        }
    }

    /*
     * 负向：不存在的路径必须 NOT_FOUND，不能当成空文件成功。
     *
     * 若它意外成功，必须把 fd 关掉再报错——否则自检本身就泄漏一个 fd 槽，
     * 让后续 `cat` 在 VFS_FILES 耗尽后失败。
     */
    {
        unsigned fd = 0;
        enum vfs_result vr = vfs_open(&fs_vfs, FS_ROOT_CLIENT, "/no-such-file", &fd);

        if (vr == VFS_NOT_FOUND) {
            user_print("[fs] ENOENT");
        } else if (vr == VFS_OK) {
            (void)vfs_close(&fs_vfs, FS_ROOT_CLIENT, fd);
            user_print("[fs] ENOENT CHECK FAILED: nonexistent file opened");
        } else {
            user_print("[fs] ENOENT probe error");
        }
    }
}

/* ---- ext4 的块 I/O 端口：直接落到 ABI_BLOCK_READ ---- */

static enum ext4_result fs_read_block(void *ctx, uint64_t block,
                                      uint8_t buf[EXT4_BLOCK])
{
    size_t got = 0;
    int64_t st;

    (void)ctx;
    st = user_block_read(block, buf, 1, &got);
    if (st == ABI_OK) {
        /*
         * 块大小不匹配说明内核与用户态的几何假设不同（例如将来改成
         * 512 B 扇区），这是格式/协议层问题，不是 I/O 错误。
         */
        return got == EXT4_BLOCK ? EXT4_OK : EXT4_FORMAT;
    }
    if (st == ABI_CLOSED) {
        /* 存储后端未就绪/已下线：不是镜像损坏。 */
        return EXT4_NOT_MOUNTED;
    }
    if (st == ABI_DENIED) {
        /* 权限错误意味着 SID 装配出错，按不可用处理。 */
        return EXT4_NOT_MOUNTED;
    }
    return EXT4_FORMAT;
}

/* ---- vfs 端口：把 vfs 的抽象调用翻译成 ext4 调用 ---- */

static enum vfs_result fs_vfs_lookup(void *ctx, uint32_t dir, const char *name,
                                     size_t len, struct vfs_node *out)
{
    struct ext4 *fs = ctx;
    struct ext4_node node;
    enum ext4_result er = ext4_lookup(fs, dir, name, len, &node);

    if (er == EXT4_NOT_FOUND) {
        return VFS_NOT_FOUND;
    }
    if (er != EXT4_OK) {
        return VFS_IO_ERROR;
    }
    *out = (struct vfs_node){
        .inode = node.ino,
        .type = node.type == EXT4_DIRECTORY ? VFS_DIRECTORY : VFS_REGULAR,
    };
    return VFS_OK;
}

static enum vfs_result fs_vfs_retain(void *ctx, uint32_t inode)
{
    (void)ctx;
    (void)inode;
    return VFS_OK;
}

static void fs_vfs_release(void *ctx, uint32_t inode)
{
    (void)ctx;
    (void)inode;
}

static enum vfs_result fs_vfs_read_at(void *ctx, uint32_t inode, uint64_t offset,
                                      void *buffer, size_t wanted, size_t *actual)
{
    struct ext4 *fs = ctx;
    enum ext4_result er = ext4_read_at(fs, inode, offset, buffer, wanted, actual);

    if (er == EXT4_WRONG_TYPE) {
        return VFS_BAD_ARG;
    }
    if (er != EXT4_OK) {
        return VFS_IO_ERROR;
    }
    return VFS_OK;
}

/*
 * 挂载根文件系统。
 *
 * 容量必须从内核查：ext4 的超级块校验与 extent 边界都依赖它，而 U-mode
 * 无从得知 virtio 探测结果。块大小固定 4 KiB（与 EXT4_BLOCK 一致）。
 */
static int fs_mount(void)
{
    struct abi_message info = {0};
    int64_t st = abi_decode_result(
        abi_ecall(ABI_BLOCK_INFO, (struct abi_regs){0}), &info);
    uint64_t capacity;

    if (st != ABI_OK) {
        user_print("[fs-svc] block info failed");
        return -1;
    }
    capacity = info.words[0];
    if (capacity == 0 || info.words[1] != EXT4_BLOCK) {
        user_print("[fs-svc] unexpected block geometry");
        return -1;
    }

    fs_blk_io = (struct ext4_blk_io){.ctx = 0, .read_block = fs_read_block};
    if (ext4_mount_blk(&fs_ext4, &fs_blk_io, capacity) != EXT4_OK) {
        user_print("[fs-svc] ext4 mount failed");
        return -1;
    }

    fs_vfs_port = (struct vfs_port){
        .ctx = &fs_ext4,
        .lookup = fs_vfs_lookup,
        .retain = fs_vfs_retain,
        .release = fs_vfs_release,
        .read_at = fs_vfs_read_at,
    };
    fs_vfs.port = &fs_vfs_port;
    fs_vfs.root = EXT4_ROOT_INODE;
    fs_mounted = true;
    user_print("[fs-svc] ext4 mounted in U-mode");
    return 0;
}

static void register_to_sm(uint32_t sm_send, uint32_t sid)
{
    struct abi_message req = {
        .tag = TAG_SM_REGISTER,
        .words = {0, svc_name_pack("fs"), sid, 0},
    };

    for (;;) {
        struct abi_message rep = {0};

        if (user_ipc_call(sm_send, &req, &rep) == ABI_OK &&
            rep.tag == TAG_SM_OK) {
            return;
        }
        abi_ecall(ABI_YIELD, (struct abi_regs){0});
    }
}

void fs_start(uint32_t ready_send, uint64_t receipt, uint32_t fs_recv,
              uint32_t sid, uint32_t sm_send) __attribute__((used));
void fs_start(uint32_t ready_send, uint64_t receipt, uint32_t fs_recv,
              uint32_t sid, uint32_t sm_send)
{
    register_to_sm(sm_send, sid);
    if (fs_mount() == 0) {
        fs_self_check();
    }

    {
        struct abi_message ready = {
            .tag = TAG_READY,
            .words = {receipt, 0, 0, 0},
        };

        if (user_ipc_send(ready_send, &ready) != ABI_OK) {
            for (;;) {
                abi_ecall(ABI_YIELD, (struct abi_regs){0});
            }
        }
    }
    user_print("[fs-svc] ready");

    for (;;) {
        struct abi_message req = {0};

        if (user_ipc_recv(fs_recv, &req) != ABI_OK) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
            continue;
        }
        if (req.reply == 0) {
            continue;
        }

        if (req.tag == TAG_FS_PATH) {
            size_t total = (size_t)req.words[0];
            size_t offset = (size_t)req.words[1];
            unsigned char chunk[SVC_PATH_CHUNK];
            size_t n;

            if (total > SVC_MAX_PATH || offset > total) {
                reply_msg(req.reply, TAG_FS_ERROR, (uint64_t)ABI_BAD_ARG, 0, 0, 0);
                continue;
            }
            if (offset == 0) {
                fs_path_len = 0;
                /* 新路径开始：上一次的 fd 与预取缓冲都不再有效。 */
                if (fs_fd_valid) {
                    (void)vfs_close(&fs_vfs, FS_ROOT_CLIENT, fs_open_fd);
                    fs_fd_valid = false;
                }
                fs_prefetch_valid = false;
            }
            unpack16(req.words[2], req.words[3], chunk);
            n = total - offset;
            if (n > SVC_PATH_CHUNK) {
                n = SVC_PATH_CHUNK;
            }
            if (offset + n > SVC_MAX_PATH) {
                reply_msg(req.reply, TAG_FS_ERROR, (uint64_t)ABI_BAD_ARG, 0, 0, 0);
                continue;
            }
            for (size_t i = 0; i < n; i++) {
                fs_path[offset + i] = (char)chunk[i];
            }
            if (offset + n > fs_path_len) {
                fs_path_len = offset + n;
            }
            if (fs_path_len < sizeof(fs_path)) {
                fs_path[fs_path_len] = '\0';
            }
            reply_msg(req.reply, TAG_FS_PATH_OK, total, 0, 0, 0);
        } else if (req.tag == TAG_FS_READ) {
            uint64_t offset = req.words[0];
            size_t max = (size_t)req.words[1];
            char buf[SVC_DATA_BYTES];
            size_t n = 0;
            enum vfs_result vr;

            if (fs_path_len == 0 || max == 0) {
                reply_msg(req.reply, TAG_FS_ERROR, (uint64_t)ABI_BAD_ARG, 0, 0, 0);
                continue;
            }
            /*
             * 挂载失败与参数错误必须区分：客户端看到 ABI_CLOSED 才知道
             * 「服务在，但底层文件系统没起来」，而不是自己传错了路径。
             */
            if (!fs_mounted) {
                reply_msg(req.reply, TAG_FS_ERROR, (uint64_t)ABI_CLOSED,
                          0, 0, 0);
                continue;
            }
            if (max > SVC_DATA_BYTES) {
                max = SVC_DATA_BYTES;
            }
            /*
             * 同一路径的连续读复用一次 open：客户端按 24 B 分块拉取，
             * 大文件会有上万次 TAG_FS_READ，重复 vfs_open 等于重复全部
             * 路径解析。fd 在收到新 TAG_FS_PATH 时关闭（见上）。
             */
            if (!fs_fd_valid) {
                vr = vfs_open(&fs_vfs, FS_ROOT_CLIENT, fs_path, &fs_open_fd);
                if (vr == VFS_NOT_FOUND) {
                    reply_msg(req.reply, TAG_FS_ERROR, (uint64_t)ABI_NOT_FOUND,
                              0, 0, 0);
                    continue;
                }
                if (vr != VFS_OK) {
                    reply_msg(req.reply, TAG_FS_ERROR, (uint64_t)ABI_BAD_ARG,
                              0, 0, 0);
                    continue;
                }
                fs_fd_valid = true;
                fs_prefetch_valid = false;
            }

            /*
             * 命中预取缓冲：请求区间完整落在已读入的数据内，直接拷贝。
             *
             * 这一步是 `cat` 大文件的关键——`vfs_read_at` 的固定成本
             * （load_inode + extent 解析 + 块映射）与读多少字节无关，
             * 而客户端每次只要 24 B。预取一块后，后续小读不再触碰 ext4。
             *
             * 注意：只有当缓冲剩余长度 >= 请求量时才走命中路径。否则说明
             * 已经读到预取窗口尾部，必须重新预取——返回一个「短读」会被
             * 客户端当成 EOF，把 200 KiB 的文件截断在第一个窗口。
             */
            if (fs_prefetch_valid && offset >= fs_prefetch_base) {
                uint64_t rel = offset - fs_prefetch_base;

                if (rel < fs_prefetch_len &&
                    fs_prefetch_len - rel >= max) {
                    size_t in_block = (size_t)rel;

                    n = max;
                    for (size_t i = 0; i < n; i++) {
                        buf[i] = (char)fs_prefetch[in_block + i];
                    }
                    goto send_read_reply;
                }
            }

            /* 未命中：整块预取（末尾不足一块时按实际读到长度）。 */
            vr = vfs_read_at(&fs_vfs, FS_ROOT_CLIENT, fs_open_fd, offset,
                             fs_prefetch, sizeof(fs_prefetch), &n);
            if (vr != VFS_OK) {
                /*
                 * fd 可能已失效（例如底层实现变化）：关掉缓存，让下一次
                 * 读重新 open。当前 ext4 下不会走到这里，但保持自愈。
                 */
                (void)vfs_close(&fs_vfs, FS_ROOT_CLIENT, fs_open_fd);
                fs_fd_valid = false;
                reply_msg(req.reply, TAG_FS_ERROR,
                          (uint64_t)(vr == VFS_NOT_FOUND ? ABI_NOT_FOUND
                                                         : ABI_BAD_ARG),
                          0, 0, 0);
                continue;
            }
            {
                /*
                 * 预取成功：`fs_prefetch` 里已有从 `offset` 开始的 n 字节。
                 * 只把其中前 max 字节回给客户端，其余留在缓冲里供后续
                 * 小读直接命中。
                 */
                fs_prefetch_base = offset;
                fs_prefetch_len = n;
                fs_prefetch_valid = n != 0;
                if (n > max) {
                    n = max;
                }
                for (size_t i = 0; i < n; i++) {
                    buf[i] = (char)fs_prefetch[i];
                }
            }

send_read_reply:
            {
                uint64_t words[3];

                pack24((const unsigned char *)buf, n, words);
                reply_msg(req.reply, TAG_FS_READ_OK, n, words[0], words[1], words[2]);
            }
        } else {
            reply_msg(req.reply, TAG_FS_ERROR, (uint64_t)ABI_BAD_ARG, 0, 0, 0);
        }
    }
}
