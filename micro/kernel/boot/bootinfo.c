/*
 * bootinfo.c — 把平台事实和 DTB 指针变成一份可验证的 BootInfo。
 *
 * 这里不做任何设备初始化、不分配内存、不解析 FDT 节点。
 * 只回答两个问题：
 *   1. 我现在信哪些内存？（RAM 列表）
 *   2. 哪些区间在交接后不能被随手覆盖？（保留区）
 *
 * 失败语义：能返回错误码就返回，不在这里打印、不在这里停机。
 * 打印和 fail-stop 属于 boot.c 的策略。
 */
#include "bootinfo.h"

#define FDT_MAGIC        0xd00dfeedUL
#define FDT_HEADER_SIZE  40UL
#define FDT_MAX_SIZE     (1UL << 20)

static void boot_memzero(volatile void *dst, u64 n)
{
    volatile u8 *p = (volatile u8 *)dst;

    while (n--)
        *p++ = 0;
}

static u32 be32(const u8 *p)
{
    return ((u32)p[0] << 24) | ((u32)p[1] << 16) |
           ((u32)p[2] << 8) | (u32)p[3];
}

static int range_ok(u64 base, u64 size)
{
    return size != 0 && base + size > base;
}

static int range_in_ram(const struct boot_platform *p, u64 base, u64 size)
{
    u64 ram_end;

    if (!range_ok(p->ram_base, p->ram_size))
        return 0;
    ram_end = p->ram_base + p->ram_size;
    return range_ok(base, size) && base >= p->ram_base &&
           base + size <= ram_end;
}

static int fdt_probe(const struct boot_platform *p, u64 dtb, u64 *size)
{
    const u8 *hdr = (const u8 *)dtb;
    u32 total;

    /* FDT 规范要求 blob 起始地址 8 字节对齐；不对齐就不猜。 */
    if ((dtb & 7UL) != 0)
        return 0;
    if (!range_in_ram(p, dtb, FDT_HEADER_SIZE))
        return 0;
    if (be32(hdr) != FDT_MAGIC)
        return 0;

    total = be32(hdr + 4);
    if (total < FDT_HEADER_SIZE || total > FDT_MAX_SIZE)
        return 0;
    if (!range_in_ram(p, dtb, total))
        return 0;

    *size = total;
    return 1;
}

static void region_add(struct boot_region *r, u32 *count, u32 type,
                       u64 base, u64 size)
{
    if (size == 0 || *count >= BOOTINFO_MAX_REGIONS)
        return;

    r[*count].base = base;
    r[*count].size = size;
    r[*count].type = type;
    r[*count].pad = 0;
    (*count)++;
}

int bootinfo_build(struct bootinfo *bi, const struct boot_platform *p,
                   u64 hartid, u64 dtb)
{
    boot_memzero(bi, sizeof(*bi));

    bi->magic = BOOTINFO_MAGIC;
    bi->version = BOOTINFO_VERSION;
    bi->size = (u16)sizeof(*bi);
    bi->hartid = hartid;
    bi->boot_cpu = hartid;
    bi->kernel_start = p->kernel_start;
    bi->kernel_end = p->kernel_end;

    region_add(bi->ram, &bi->ram_count, BOOT_REGION_RAM,
               p->ram_base, p->ram_size);
    region_add(bi->reserved, &bi->reserved_count, BOOT_REGION_FIRMWARE,
               p->firmware_base, p->firmware_size);
    if (p->kernel_end > p->kernel_start) {
        region_add(bi->reserved, &bi->reserved_count, BOOT_REGION_KERNEL,
                   p->kernel_start, p->kernel_end - p->kernel_start);
    }

    if (dtb == 0) {
        bi->dtb = 0;
        bi->dtb_size = 0;
        return BOOT_DTB_NONE;
    }

    bi->dtb = dtb;
    if (!fdt_probe(p, dtb, &bi->dtb_size))
        return BOOT_DTB_BAD;

    region_add(bi->reserved, &bi->reserved_count, BOOT_REGION_DTB,
               dtb, bi->dtb_size);
    return BOOT_DTB_VALID;
}

static int overlaps(const struct boot_region *a, const struct boot_region *b)
{
    return a->base < b->base + b->size && b->base < a->base + a->size;
}

static int region_in_ram(const struct bootinfo *bi, u64 base, u64 size)
{
    if (!range_ok(base, size))
        return 0;

    for (u32 i = 0; i < bi->ram_count; i++) {
        if (base >= bi->ram[i].base &&
            base + size <= bi->ram[i].base + bi->ram[i].size)
            return 1;
    }
    return 0;
}

static int reserved_type_valid(u32 type)
{
    switch (type) {
    case BOOT_REGION_FIRMWARE:
    case BOOT_REGION_KERNEL:
    case BOOT_REGION_DTB:
    case BOOT_REGION_INITRD:
        return 1;
    default:
        return 0;
    }
}

static int reserved_has(const struct bootinfo *bi, u32 type,
                        u64 base, u64 size)
{
    for (u32 i = 0; i < bi->reserved_count; i++) {
        if (bi->reserved[i].type == type &&
            bi->reserved[i].base == base &&
            bi->reserved[i].size == size)
            return 1;
    }
    return 0;
}

int bootinfo_validate(const struct bootinfo *bi)
{
    if (bi->magic != BOOTINFO_MAGIC)
        return BOOTINFO_EBADMAGIC;
    if (bi->version != BOOTINFO_VERSION)
        return BOOTINFO_EBADVERSION;
    if (bi->size != sizeof(*bi))
        return BOOTINFO_EBADSIZE;
    if (bi->ram_count == 0 || bi->ram_count > BOOTINFO_MAX_REGIONS)
        return BOOTINFO_EBADREGION;
    if (bi->reserved_count > BOOTINFO_MAX_REGIONS)
        return BOOTINFO_EBADREGION;

    for (u32 i = 0; i < bi->ram_count; i++) {
        if (bi->ram[i].type != BOOT_REGION_RAM)
            return BOOTINFO_EBADREGION;
        if (!range_ok(bi->ram[i].base, bi->ram[i].size))
            return BOOTINFO_EBADREGION;
        for (u32 j = i + 1; j < bi->ram_count; j++) {
            if (overlaps(&bi->ram[i], &bi->ram[j]))
                return BOOTINFO_EOVERLAP;
        }
    }

    for (u32 i = 0; i < bi->reserved_count; i++) {
        const struct boot_region *r = &bi->reserved[i];

        if (!reserved_type_valid(r->type))
            return BOOTINFO_EBADREGION;
        if (!range_ok(r->base, r->size))
            return BOOTINFO_EBADREGION;
        if (!region_in_ram(bi, r->base, r->size))
            return BOOTINFO_ENOTINRAM;
        for (u32 j = i + 1; j < bi->reserved_count; j++) {
            if (overlaps(r, &bi->reserved[j]))
                return BOOTINFO_EOVERLAP;
        }
    }

    if (bi->kernel_start >= bi->kernel_end)
        return BOOTINFO_EBADREGION;
    if (!region_in_ram(bi, bi->kernel_start,
                       bi->kernel_end - bi->kernel_start))
        return BOOTINFO_ENOTINRAM;
    if (!reserved_has(bi, BOOT_REGION_KERNEL, bi->kernel_start,
                      bi->kernel_end - bi->kernel_start))
        return BOOTINFO_EBADREGION;

    if (bi->dtb_size != 0) {
        if (!range_ok(bi->dtb, bi->dtb_size))
            return BOOTINFO_EBADREGION;
        if (!region_in_ram(bi, bi->dtb, bi->dtb_size))
            return BOOTINFO_ENOTINRAM;
        if (!reserved_has(bi, BOOT_REGION_DTB, bi->dtb, bi->dtb_size))
            return BOOTINFO_EBADREGION;
    }

    return BOOTINFO_OK;
}
