/*
 * virtio-blk MMIO driver — 真实块后端（M5）。
 *
 * host block demo 仍用内存盘替身；本文件只用于 MICRO_POST_BOOT_RESIDENT 路径。
 */
#include "virtio_blk.h"

#include <stdint.h>

#include "demo.h"
#include "irq.h"
#include "memory.h"
#include "platform.h"

enum {
    VIRTIO_MMIO_MAGIC = 0x74726976U,
    VIRTIO_MMIO_VERSION = 2,
    VIRTIO_DEV_BLOCK = 2,
    VIRTIO_STATUS_ACK = 1,
    VIRTIO_STATUS_DRIVER = 2,
    VIRTIO_STATUS_DRIVER_OK = 4,
    VIRTIO_STATUS_FEATURES_OK = 8,
    VIRTIO_BLK_T_IN = 0,
    SECTOR = 512,
    QSIZE = 8,
    VIRTIO_PROBE_MAX = 8,
    /* virtio_config.h feature bits — 只协商驱动真正实现的能力 */
    VIRTIO_F_ACCESS_PLATFORM = 28,
    VIRTIO_RING_F_EVENT_IDX = 29,
    VIRTIO_F_VERSION_1 = 32, /* word-select 1 bit 0 */
    VIRTIO_F_ORDER_PLATFORM = 36,
    VIRTIO_BLK_F_RO = 5,
    VIRTIO_BLK_F_MQ = 12,
};

static uint32_t virtio_negotiate_feat0(uint32_t dev_feat)
{
    uint32_t guest = dev_feat;

    guest &= ~((1U << VIRTIO_F_ACCESS_PLATFORM) | (1U << VIRTIO_RING_F_EVENT_IDX) |
               (1U << VIRTIO_BLK_F_MQ));
    return guest;
}

struct virtq_desc {
    uint64_t addr;
    uint32_t len;
    uint16_t flags;
    uint16_t next;
};

struct virtq_avail {
    uint16_t flags;
    uint16_t idx;
    uint16_t ring[QSIZE];
    uint16_t used_event;
};

struct virtq_used_elem {
    uint32_t id;
    uint32_t len;
};

struct virtq_used {
    uint16_t flags;
    uint16_t idx;
    struct virtq_used_elem ring[QSIZE];
    uint16_t avail_event;
};

struct virtio_blk_req {
    uint32_t type;
    uint32_t reserved;
    uint64_t sector;
};

struct virtio_blk {
    volatile uint32_t *mmio;
    unsigned irq_line;
    bool legacy_mmio; /* QEMU virt 常为 version=1 legacy；寄存器布局与 v2 不同 */
    pfn_t desc_pfn;
    pfn_t avail_pfn;
    pfn_t used_pfn;
    pfn_t hdr_pfn;
    pfn_t status_pfn;
    volatile struct virtq_desc *desc;
    volatile struct virtq_avail *avail;
    volatile struct virtq_used *used;
    paddr_t desc_pa;
    struct virtio_blk_req *hdr;
    paddr_t hdr_pa;
    volatile uint8_t *status;
    paddr_t status_pa;
    bool busy;
    uint16_t last_used;
    struct block_request inflight;
    struct block_service *svc;
    unsigned dev;
};

static uintptr_t virtio_probe_bases[VIRTIO_PROBE_MAX] = {
    0x10001000UL, 0x10002000UL, 0x10003000UL, 0x10004000UL,
    0x10005000UL, 0x10006000UL, 0x10007000UL, 0x10008000UL,
};

static struct virtio_blk g_vblk;
static struct irq_sink virtio_sink;

static void virtio_complete_if_ready(struct virtio_blk *dev);

static uint32_t mmio_read(const volatile uint32_t *base, unsigned off)
{
    return base[off / 4U];
}

static void mmio_write(volatile uint32_t *base, unsigned off, uint32_t val)
{
    base[off / 4U] = val;
}

static void mmio_write64(volatile uint32_t *base, unsigned off, uint64_t val)
{
    mmio_write(base, off, (uint32_t)val);
    mmio_write(base, off + 4U, (uint32_t)(val >> 32));
}

int virtio_blk_probe_mmio(uintptr_t base)
{
    volatile uint32_t *mmio = (volatile uint32_t *)(uintptr_t)base;

    if (mmio_read(mmio, 0x000) != VIRTIO_MMIO_MAGIC) {
        return -1;
    }
    {
        uint32_t ver = mmio_read(mmio, 0x004);
        if (ver != 1U && ver != 2U) {
            return -1;
        }
    }
    if (mmio_read(mmio, 0x008) != VIRTIO_DEV_BLOCK) {
        return -1;
    }
    return 0;
}

static uintptr_t virtio_find_block(void)
{
    for (unsigned i = 0; i < VIRTIO_PROBE_MAX; i++) {
        if (virtio_blk_probe_mmio(virtio_probe_bases[i]) == 0) {
            return virtio_probe_bases[i];
        }
    }
    return 0;
}

static paddr_t ptr_to_paddr(const void *ptr)
{
    uintptr_t va = (uintptr_t)ptr;

    if (va >= RAM_BASE && va < RAM_BASE + RAM_SIZE) {
        return (paddr_t)va;
    }
    return 0;
}

static size_t virtio_used_offset(unsigned qsize, bool legacy_mmio)
{
    size_t avail_off = sizeof(struct virtq_desc) * qsize;
    /*
     * legacy 无 EVENT_IDX：avail 只有 flags+idx+ring；modern 另含 used_event 字段。
     */
    size_t avail_sz = sizeof(uint16_t) * (2U + qsize);

    if (!legacy_mmio) {
        avail_sz += sizeof(uint16_t);
    }
    size_t used_off = avail_off + avail_sz;

    return (used_off + 3U) & ~(size_t)3U;
}

static int virtio_map_queue(struct virtio_blk *dev)
{
    pfn_t queue = frame_alloc();
    pfn_t hdr = frame_alloc();
    pfn_t status = frame_alloc();
    uint8_t *page;
    paddr_t avail_pa;
    paddr_t used_pa;
    size_t used_off;
    uint32_t maxq;

    if (queue == PFN_NONE || hdr == PFN_NONE || status == PFN_NONE) {
        /* 部分成功时把已分配的帧交还池，否则每次 init 失败都永久泄漏 1~3 页。 */
        if (queue != PFN_NONE)
            frame_unref(queue);
        if (hdr != PFN_NONE)
            frame_unref(hdr);
        if (status != PFN_NONE)
            frame_unref(status);
        return -1;
    }
    page = frame_ptr(queue);
    if (page == 0 || frame_ptr(hdr) == 0 || frame_ptr(status) == 0) {
        frame_unref(queue);
        frame_unref(hdr);
        frame_unref(status);
        return -1;
    }

    memset(page, 0, PAGE_SIZE);
    dev->desc_pfn = queue;
    dev->avail_pfn = PFN_NONE;
    dev->used_pfn = PFN_NONE;
    dev->hdr_pfn = hdr;
    dev->status_pfn = status;
    dev->desc_pa = frame_paddr(queue);
    dev->desc = (volatile struct virtq_desc *)page;
    dev->avail = (volatile struct virtq_avail *)(page + sizeof(struct virtq_desc) * QSIZE);
    used_off = virtio_used_offset(QSIZE, dev->legacy_mmio);
    dev->used = (volatile struct virtq_used *)(page + used_off);
    avail_pa = dev->desc_pa + (paddr_t)(sizeof(struct virtq_desc) * QSIZE);
    used_pa = dev->desc_pa + (paddr_t)used_off;

    dev->hdr = frame_ptr(hdr);
    dev->status = frame_ptr(status);
    dev->hdr_pa = frame_paddr(hdr);
    dev->status_pa = frame_paddr(status);

    mmio_write(dev->mmio, 0x030, 0);
    maxq = mmio_read(dev->mmio, 0x034);
    if (maxq != 0 && maxq < QSIZE) {
        mmio_write(dev->mmio, 0x038, maxq);
    } else {
        mmio_write(dev->mmio, 0x038, QSIZE);
    }

    if (dev->legacy_mmio) {
        /*
         * legacy MMIO v1：
         *   0x028 = GuestPageSize，0x03c = QueueAlign，0x040 = QueuePFN。
         * 0x020 是 GuestFeatures，不能在这里再用页大小覆盖。
         *
         * 队列环放在一页内：desc(8*16=128) + avail(2+2+8*2=20)，
         * used ring 从页内偏移 148 开始。QueueAlign 取 4，使 QEMU
         * 写 used->idx 的位置与 virtio_used_offset() 计算一致；写成
         * PAGE_SIZE 会把 used ring 推到页外，驱动永远看不到完成。
         *
         * QueuePFN 只写一次有效值：QEMU 对 QueuePFN=0 会执行 soft reset，
         * 清掉已协商特性并让后续 notify 不再消费 avail ring。
         */
        mmio_write(dev->mmio, 0x028, PAGE_SIZE);
        mmio_write(dev->mmio, 0x03c, 4);
        mmio_write(dev->mmio, 0x040, (uint32_t)(dev->desc_pa >> 12));
        (void)avail_pa;
        (void)used_pa;
    } else {
        /*
         * modern MMIO v2：QueueDesc=0x080，QueueDriver(avail)=0x090，
         * QueueDevice(used)=0x0a0。QEMU 8.2 实测 0x088 不是 avail；
         * 写成 0x088/0x090 会让设备把 used 地址当作 avail ring。
         */
        mmio_write(dev->mmio, 0x044, 0);
        mmio_write64(dev->mmio, 0x080, dev->desc_pa);
        mmio_write64(dev->mmio, 0x090, avail_pa);
        mmio_write64(dev->mmio, 0x0a0, used_pa);
        mmio_write(dev->mmio, 0x044, 1);
    }
    return 0;
}

enum block_status virtio_blk_submit(struct virtio_blk *dev, const struct block_request *request)
{
    paddr_t data_pa;
    uint64_t first_sector;
    uint16_t slot;

    if (dev->busy) {
        return BLOCK_BUSY;
    }
    if (request->op != BLOCK_READ) {
        demo_log("[virtio-blk] reject: write unsupported");
        return BLOCK_IO_ERROR;
    }
    if (request->bytes % SECTOR != 0) {
        demo_log("[virtio-blk] reject: unaligned bytes");
        return BLOCK_IO_ERROR;
    }

    if (dev->svc->devices[request->dev].logical_bytes == 4096) {
        first_sector = request->lba * 8ULL;
    } else {
        first_sector = request->lba;
    }

    data_pa = ptr_to_paddr(request->buffer->data);
    if (data_pa == 0) {
        demo_log("[virtio-blk] reject: bad dma address");
        return BLOCK_IO_ERROR;
    }

    dev->hdr->type = VIRTIO_BLK_T_IN;
    dev->hdr->reserved = 0;
    dev->hdr->sector = first_sector;
    *dev->status = 0xff;

    dev->desc[0].addr = dev->hdr_pa;
    dev->desc[0].len = sizeof(*dev->hdr);
    dev->desc[0].flags = 1;
    dev->desc[0].next = 1;

    dev->desc[1].addr = data_pa;
    dev->desc[1].len = (uint32_t)request->bytes;
    dev->desc[1].flags = 3;
    dev->desc[1].next = 2;

    dev->desc[2].addr = dev->status_pa;
    dev->desc[2].len = 1;
    dev->desc[2].flags = 2;
    dev->desc[2].next = 0;

    slot = dev->avail->idx % QSIZE;
    dev->avail->ring[slot] = 0;
    asm volatile("fence w,w" ::: "memory");
    dev->avail->idx++;
    asm volatile("fence w,w" ::: "memory");

    dev->busy = true;
    dev->inflight = *request;
    asm volatile("fence rw,rw" ::: "memory");
    mmio_write(dev->mmio, 0x050, 0);
    /*
     * 完成必须作为后续事件由 IRQ/poll 交付；不要在 submit 内联完成，
     * 否则 block_complete 可能在 block_dispatch 返回前重入同一队首。
     */
    return BLOCK_OK;
}

static void virtio_complete_if_ready(struct virtio_blk *dev)
{
    enum block_status st = BLOCK_IO_ERROR;

    if (!dev->busy) {
        return;
    }
    asm volatile("fence r,r" ::: "memory");
    /*
     * 完成必须由设备推进 used->idx 触发。只有 status 变化而 used ring 未
     * 更新时提前清 busy，会让下一次 submit 与设备仍在处理的旧请求重叠；
     * 在 QEMU legacy-MMIO 上实测表现为偶发 completion timeout。
     */
    if (dev->used->idx == dev->last_used) {
        return;
    }
    dev->last_used = dev->used->idx;
    asm volatile("fence r,r" ::: "memory");

    if (*dev->status == 0) {
        st = BLOCK_OK;
    }

    /*
     * used->idx 已推进说明设备已释放该请求。即使 block 层不认这个 id
     * （重复完成/已被超时清理），也必须清 busy，否则队列会永久卡死。
     * 完成只交付当前请求；下一次派发由等待方（blk_io_read）在返回后
     * 驱动，避免在 block_dispatch 尚未返回时递归进入下一次派发。
     */
    (void)block_complete(dev->svc, dev->dev, dev->inflight.id, st);
    dev->busy = false;
    dev->inflight = (struct block_request){0};
}

static void virtio_sink_notify(struct irq_sink *self, int line)
{
    struct virtio_blk *dev = self->ctx;
    uint32_t isr;

    (void)line;

    /*
     * 先清设备 ISR，再归还 PLIC claim。即使请求已由 poll 完成，
     * 也必须清 ISR，否则中断线保持有效，complete 后会立刻重新 pending。
     */
    isr = mmio_read(dev->mmio, 0x060);
    if (isr != 0) {
        mmio_write(dev->mmio, 0x064, isr);
    }

    virtio_complete_if_ready(dev);
    (void)irq_ack((int)dev->irq_line);
}

void virtio_blk_poll(struct virtio_blk *dev)
{
    uint32_t isr;

    if (dev == 0) {
        return;
    }
    isr = mmio_read(dev->mmio, 0x060);
    if (isr != 0) {
        mmio_write(dev->mmio, 0x064, isr);
    }
    virtio_complete_if_ready(dev);
}

int virtio_blk_init(struct virtio_blk **out, struct block_service *svc, unsigned dev)
{
    uintptr_t base = virtio_find_block();
    struct virtio_blk *devp = &g_vblk;
    uint64_t cap_lo;
    uint64_t cap_hi;

    if (out == 0 || svc == 0) {
        return -1;
    }
    if (base == 0) {
        demo_log("[virtio-blk] probe: no mmio block device");
        return -1;
    }

    uint32_t feat0;
    uint32_t feat1;
    uint32_t mmio_ver;

    *devp = (struct virtio_blk){0};
    devp->mmio = (volatile uint32_t *)(uintptr_t)base;
    devp->svc = svc;
    devp->dev = dev;
    devp->irq_line = (unsigned)((base - 0x10001000UL) / 0x1000UL + 1UL);
    mmio_ver = mmio_read(devp->mmio, 0x004);
    devp->legacy_mmio = mmio_ver == 1U;

    mmio_write(devp->mmio, 0x070, 0);
    mmio_write(devp->mmio, 0x070, VIRTIO_STATUS_ACK);
    mmio_write(devp->mmio, 0x070, VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER);

    if (devp->legacy_mmio) {
        /* legacy v1：0x010 DeviceFeatures，0x020 GuestFeatures（32 位，无 sel）。 */
        feat0 = mmio_read(devp->mmio, 0x010);
        feat0 = virtio_negotiate_feat0(feat0);
        mmio_write(devp->mmio, 0x020, feat0);
    } else {
        /*
         * modern v2：先选 0x014 DeviceFeaturesSel 再读 0x010；
         * 写回时先选 0x024 DriverFeaturesSel，再写 0x020 DriverFeatures。
         * 顺序不能反，且必须协商 VERSION_1（word 1 bit 0）。
         */
        mmio_write(devp->mmio, 0x014, 0);
        feat0 = mmio_read(devp->mmio, 0x010);
        feat0 = virtio_negotiate_feat0(feat0);
        mmio_write(devp->mmio, 0x014, 1);
        feat1 = mmio_read(devp->mmio, 0x010);
        if ((feat1 & 1U) == 0U) {
            demo_log("[virtio-blk] VERSION_1 not offered");
            return -1;
        }
        mmio_write(devp->mmio, 0x024, 0);
        mmio_write(devp->mmio, 0x020, feat0);
        mmio_write(devp->mmio, 0x024, 1);
        mmio_write(devp->mmio, 0x020, 1);
    }

    mmio_write(devp->mmio, 0x070,
               VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK);
    if ((mmio_read(devp->mmio, 0x070) & VIRTIO_STATUS_FEATURES_OK) == 0) {
        demo_log("[virtio-blk] feature negotiation failed");
        return -1;
    }

    if (virtio_map_queue(devp) != 0) {
        demo_log("[virtio-blk] queue setup failed");
        return -1;
    }

    mmio_write(devp->mmio, 0x070,
               VIRTIO_STATUS_ACK | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK |
               VIRTIO_STATUS_DRIVER_OK);

    cap_lo = mmio_read(devp->mmio, 0x100);
    cap_hi = mmio_read(devp->mmio, 0x104);
    uint64_t capacity_sect = cap_lo | (cap_hi << 32U);

    svc->devices[dev].logical_bytes = 4096;
    svc->devices[dev].capacity = capacity_sect / 8ULL;
    if (svc->devices[dev].capacity == 0) {
        svc->devices[dev].logical_bytes = SECTOR;
        svc->devices[dev].capacity = capacity_sect;
    }

    virtio_sink.notify = virtio_sink_notify;
    virtio_sink.ctx = devp;
    (void)irq_bind((int)devp->irq_line, &virtio_sink);

    demo_log(devp->legacy_mmio ? "[virtio-blk] mmio ready legacy-v1"
                                 : "[virtio-blk] mmio ready");
    mmio_write(devp->mmio, 0x050, 0); /* probe: mmio notify path */

    *out = devp;
    return 0;
}

unsigned virtio_blk_irq_line(const struct virtio_blk *dev)
{
    return dev != 0 ? dev->irq_line : 0;
}

void virtio_blk_on_irq(struct virtio_blk *dev)
{
    virtio_blk_poll(dev);
}