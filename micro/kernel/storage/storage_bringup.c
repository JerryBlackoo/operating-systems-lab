/*
 * storage_bringup.c — 块设备驱动 bringup（virtio-blk → block 队列）。
 *
 * 【A1：策略层已迁出内核】
 *
 * 2026-11-25 之前，ext4、vfs、block、virtio_blk 全部链在内核镜像里，
 * `kernel_storage_bringup()` 在内核里挂 ext4 并直接读 `/motd`，U-mode 的
 * `fs` 服务只是一个转发壳。现在文件系统策略（ext4 + vfs）跑在 U-mode
 * `fs` 服务里，内核只保留：
 *
 *   - virtio-blk 驱动 + block 请求队列（本文件）
 *   - `ABI_BLOCK_READ`：把原始逻辑块读进调用者的用户缓冲区
 *
 * 内核不再解析路径、不遍历目录、不解码 inode 或 extent。`fs` 服务自己
 * 挂 ext4、自己走 vfs，缺页/读写语义都在用户态。
 */
#include "storage_bringup.h"

#include <stdint.h>

#include "abi/abi.h"
#include "sbi.h"

#include "block.h"
#include "demo.h"
#include "irq.h"
#include "memory.h"
#include "platform.h"
#include "sched.h"
#include "service_boot.h"
#include "virtio_blk.h"

enum {
    BLOCK_CALLER = 42,
    BLK_DEV = 0,
    /* 逻辑块大小。与 virtio-blk 的 sector 换算、ext4 的块大小一致。 */
    BLOCK_BYTES = 4096,
    /*
     * 单次 ABI_BLOCK_READ 允许的最大块数。
     *
     * 上限由 DMA 缓冲区大小决定：内核侧只有一块 4 KiB 读写窗口，因此
     * 一次只能读一个块。ext4 的 read_block 本来就是逐块调用，这个限制
     * 与上层用法一致；将来要做多块合并再扩大这里和 dma_buf。
     */
    BLOCK_READ_MAX_BLOCKS = 1,
};

struct storage_ctx {
    struct block_service svc;
    /* 端口必须是静态生命周期：svc.port 在 bringup 返回后仍被 block 层使用。 */
    struct block_port port;
    struct virtio_blk *vblk;
    volatile bool io_done;
    enum block_status io_status;
    uint8_t *dma_buf;
};

static struct storage_ctx g_store;
static uint8_t g_busy_disc_slots[BLOCK_PENDING][BLOCK_BYTES];
/*
 * block 请求会保存 buffer 指针；诊断请求若因异常状态留在队列里，
 * 这些描述符不能是栈变量。 */
static struct block_buffer g_disc_buffers[BLOCK_PENDING];
static struct block_buffer g_disc_spill;
static uint8_t g_disc_spill_buf[BLOCK_BYTES];
/* 读请求超时后可能仍留在 block/virtio 队列里，描述符不能是栈变量。 */
static struct block_buffer g_io_buffer;
static bool g_storage_live;

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

static void pump_irq(void)
{
    (void)irq_dispatch();
    if (g_store.vblk != 0) {
        virtio_blk_poll(g_store.vblk);
    }
}

static int storage_busy_discriminate(void)
{
    uint64_t ids[BLOCK_PENDING];
    enum block_status status;
    struct block_device *dev = &g_store.svc.devices[BLK_DEV];

    for (unsigned i = 0; i < BLOCK_PENDING; i++) {
        g_disc_buffers[i] = (struct block_buffer){
            .data = g_busy_disc_slots[i],
            .size = BLOCK_BYTES,
            .writable = true,
        };
        status = block_submit(&g_store.svc, BLOCK_CALLER, BLK_DEV, BLOCK_READ,
                              i, 1, &g_disc_buffers[i], &ids[i]);
        if (status != BLOCK_OK) {
            return -1;
        }
    }

    {
        uint64_t spill_id = 0;

        g_disc_spill = (struct block_buffer){
            .data = g_disc_spill_buf,
            .size = BLOCK_BYTES,
            .writable = true,
        };
        status = block_submit(&g_store.svc, BLOCK_CALLER, BLK_DEV, BLOCK_READ,
                              BLOCK_PENDING, 1, &g_disc_spill, &spill_id);
        if (status != BLOCK_BUSY) {
            return -1;
        }
    }

    for (unsigned guard = 0; guard < 500000U; guard++) {
        if (!dev->len && !dev->in_flight.id) {
            demo_log("[block] busy discriminate ok");
            return 0;
        }
        if (!dev->in_flight.id) {
            status = block_dispatch(&g_store.svc, BLK_DEV);
            if (status == BLOCK_EMPTY) {
                pump_irq();
                continue;
            }
            if (status != BLOCK_OK && status != BLOCK_BUSY) {
                return -1;
            }
        }
        pump_irq();
    }
    return -1;
}

static enum block_status driver_submit(void *ctx, const struct block_request *request)
{
    struct storage_ctx *st = ctx;
    return virtio_blk_submit(st->vblk, request);
}

static void driver_done(void *ctx, const struct block_request *request,
                        enum block_status status, size_t bytes)
{
    struct storage_ctx *st = ctx;

    (void)request;
    (void)bytes;
    st->io_status = status;
    st->io_done = true;
}

/*
 * 同步读一个逻辑块到内核 DMA 窗口。
 *
 * 这是整个内核里唯一的块 I/O 原语：`ABI_BLOCK_READ` 和 bringup 自检都
 * 走它。调用者必须已经确认 `g_storage_live`。
 */
static int storage_read_block(uint64_t block, uint8_t out[BLOCK_BYTES])
{
    struct storage_ctx *st = &g_store;
    uint64_t id = 0;
    enum block_status status;
    char line[72];
    char *p;

    if (!g_storage_live || st->dma_buf == 0) {
        return -1;
    }
    st->io_done = false;
    st->io_status = BLOCK_IO_ERROR;
    g_io_buffer = (struct block_buffer){
        .data = st->dma_buf, .size = BLOCK_BYTES, .writable = true,
    };
    {
        unsigned spin = 0;

        for (;;) {
            status = block_submit(&st->svc, BLOCK_CALLER, BLK_DEV, BLOCK_READ,
                                  block, 1, &g_io_buffer, &id);
            if (status != BLOCK_BUSY) {
                break;
            }
            if (++spin >= 5000000U) {
                demo_log("[block] submit busy timeout");
                return -1;
            }
            pump_irq();
        }
    }
    if (status != BLOCK_OK) {
        p = put_str(line, "[block] submit rejected status=");
        p = put_dec(p, (unsigned)status);
        *p = '\0';
        demo_log(line);
        return -1;
    }
    {
        unsigned spin = 0;

        for (;;) {
            status = block_dispatch(&st->svc, BLK_DEV);
            if (status != BLOCK_BUSY) {
                break;
            }
            if (++spin >= 5000000U) {
                demo_log("[block] dispatch busy timeout");
                return -1;
            }
            pump_irq();
        }
    }
    if (status != BLOCK_OK) {
        p = put_str(line, "[block] dispatch rejected status=");
        p = put_dec(p, (unsigned)status);
        *p = '\0';
        demo_log(line);
        return -1;
    }
    if (!st->io_done) {
        for (unsigned spin = 0; spin < 5000000U && !st->io_done; spin++) {
            pump_irq();
        }
    }
    if (!st->io_done) {
        demo_log("[block] completion timeout");
        return -1;
    }
    if (st->io_status != BLOCK_OK) {
        p = put_str(line, "[block] completion failed status=");
        p = put_dec(p, (unsigned)st->io_status);
        *p = '\0';
        demo_log(line);
        return -1;
    }
    memcpy(out, st->dma_buf, BLOCK_BYTES);
    return 0;
}

uint64_t kernel_storage_capacity_blocks(void)
{
    if (!g_storage_live) {
        return 0;
    }
    return g_store.svc.devices[BLK_DEV].capacity;
}

size_t kernel_storage_block_bytes(void)
{
    return BLOCK_BYTES;
}

int64_t kernel_storage_block_read_user(uint8_t caller_sid, uint64_t lba,
                                       uintptr_t udst, size_t blocks,
                                       size_t *bytes_out)
{
    struct thread *cur;
    uint64_t capacity;

    if (caller_sid != SERVICE_SID_FS || bytes_out == 0) {
        return ABI_DENIED;
    }
    if (!g_storage_live) {
        return ABI_CLOSED;
    }
    if (blocks == 0 || blocks > BLOCK_READ_MAX_BLOCKS) {
        return ABI_BAD_ARG;
    }
    capacity = g_store.svc.devices[BLK_DEV].capacity;
    if (lba >= capacity || blocks > capacity - lba) {
        return ABI_BAD_ARG;
    }

    /*
     * 逐块读进用户缓冲区。缓冲区必须属于调用者自己的地址空间：
     * `as_copyout` 走目标 AS 的页表，越界/未映射都会失败而不是写坏内核。
     */
    cur = sched_current();
    if (cur == 0 || cur->as == 0) {
        return ABI_DENIED;
    }
    for (size_t i = 0; i < blocks; i++) {
        uint8_t kbuf[BLOCK_BYTES];
        uintptr_t dst;

        /*
         * 目标地址也必须防溢出：`udst + i * BLOCK_BYTES` 在 32 位偏移上
         * 回绕会让 copyout 写到完全不同的 VA。当前 blocks 上限为 1，
         * 但这里不该依赖那个常数。
         */
        if (udst > UINTPTR_MAX - i * BLOCK_BYTES) {
            return ABI_BAD_ARG;
        }
        dst = udst + i * BLOCK_BYTES;
        if (storage_read_block(lba + i, kbuf) != 0) {
            return ABI_BAD_ARG;
        }
        if (as_copyout(cur->as, (vaddr_t)dst, kbuf, BLOCK_BYTES) != MEM_OK) {
            return ABI_BAD_ARG;
        }
    }
    *bytes_out = blocks * BLOCK_BYTES;
    return ABI_OK;
}

int kernel_storage_bringup(struct as *kas)
{
    char line[128];
    char *p;
    pfn_t dma_pfn;

    g_store = (struct storage_ctx){0};
    g_store.port = (struct block_port){
        .ctx = &g_store,
        .submit = driver_submit,
        .done = driver_done,
    };
    g_store.svc.port = &g_store.port;

    for (unsigned base = 0x10001000UL; base <= 0x10008000UL; base += 0x1000UL) {
        int rc = as_map_kernel_range(kas, (paddr_t)base, PAGE_SIZE);
        if (rc != MEM_OK && rc != MEM_EEXIST) {
            demo_log("[storage] virtio mmio map failed");
            return -1;
        }
    }

    if (virtio_blk_init(&g_store.vblk, &g_store.svc, BLK_DEV) != 0) {
        demo_log("[storage] virtio-blk init failed");
        return -1;
    }

    {
        struct block_device *d = &g_store.svc.devices[BLK_DEV];
        p = line;
        p = put_str(p, "[block] dev0: capacity=");
        p = put_dec(p, d->capacity);
        p = put_str(p, " blocks\n");
        *p = '\0';
        demo_log(line);
    }

    if (storage_busy_discriminate() != 0) {
        demo_log("[block] busy discriminate skipped");
    }

    /* 只在真正要读盘前分配 DMA 帧；失败路径统一回收。 */
    dma_pfn = frame_alloc();
    if (dma_pfn == PFN_NONE) {
        return -1;
    }
    g_store.dma_buf = frame_ptr(dma_pfn);
    if (g_store.dma_buf == 0) {
        frame_unref(dma_pfn);
        return -1;
    }

    g_storage_live = true;

    /*
     * 自检：读第 0 块并打印前 4 字节。
     *
     * 这里不再挂 ext4、不再校验 /motd 哈希——那是 U-mode `fs` 的职责。
     * 内核只证明「驱动能读到块」；文件系统正确性由 M5 判别脚本在
     * U-mode 侧端到端验证（motd/largefile 的 SHA256 仍会比对）。
     */
    {
        uint8_t kbuf[BLOCK_BYTES];

        if (storage_read_block(0, kbuf) != 0) {
            demo_log("[block] self-test read failed");
            g_storage_live = false;
            frame_unref(dma_pfn);
            return -1;
        }
        p = line;
        p = put_str(p, "[block] self-test ok, block0 head=");
        for (unsigned i = 0; i < 4; i++) {
            static const char hexd[] = "0123456789abcdef";

            *p++ = hexd[(kbuf[i] >> 4) & 0xf];
            *p++ = hexd[kbuf[i] & 0xf];
        }
        *p = '\0';
        demo_log(line);
    }

    return 0;
}
