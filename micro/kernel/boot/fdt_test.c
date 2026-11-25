#define _POSIX_C_SOURCE 200112L

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include "bootinfo.h"
#include "fdt_initrd.h"

#define BLOB_SIZE 1024
#define FDT_BEGIN_NODE 1
#define FDT_END_NODE 2
#define FDT_PROP 3
#define FDT_END 9

static uint8_t blob[BLOB_SIZE] __attribute__((aligned(8)));

static void put32(uint8_t *p, uint32_t v)
{
    p[0] = (uint8_t)(v >> 24);
    p[1] = (uint8_t)(v >> 16);
    p[2] = (uint8_t)(v >> 8);
    p[3] = (uint8_t)v;
}

static void put_token(uint32_t *pos, uint32_t token)
{
    put32(blob + *pos, token);
    *pos += 4;
}

static void put_name(uint32_t *pos, const char *name)
{
    size_t len = strlen(name) + 1;

    memcpy(blob + *pos, name, len);
    *pos += (uint32_t)((len + 3) & ~3U);
}

static void put_prop(uint32_t *pos, uint32_t nameoff, const void *data,
                     uint32_t len)
{
    put_token(pos, FDT_PROP);
    put32(blob + *pos, len);
    put32(blob + *pos + 4, nameoff);
    *pos += 8;
    memcpy(blob + *pos, data, len);
    *pos += (len + 3) & ~3U;
}

static void build_fdt(uint32_t start_len, uint64_t start, uint64_t end,
                      int with_chosen)
{
    const uint32_t struct_off = 40;
    const uint32_t strings_off = 0x300;
    const char *start_name = "linux,initrd-start";
    const char *end_name = "linux,initrd-end";
    uint32_t pos = struct_off;
    uint32_t nameoff;
    uint8_t value[8];

    memset(blob, 0, sizeof(blob));
    put32(blob, 0xd00dfeedU);
    put32(blob + 8, struct_off);
    put32(blob + 12, strings_off);
    put32(blob + 16, 0x11);
    put32(blob + 20, 0x10);
    put32(blob + 24, 0);
    put32(blob + 28, 0);
    put32(blob + 32, (uint32_t)(strlen(start_name) + 1 + strlen(end_name) + 1));

    memcpy(blob + strings_off, start_name, strlen(start_name) + 1);
    nameoff = 0;
    memcpy(blob + strings_off + nameoff + strlen(start_name) + 1, end_name,
           strlen(end_name) + 1);

    put_token(&pos, FDT_BEGIN_NODE);
    put_name(&pos, "");
    if (with_chosen) {
        put_token(&pos, FDT_BEGIN_NODE);
        put_name(&pos, "chosen");

        if (start_len == 8) {
            put32(value, (uint32_t)(start >> 32));
            put32(value + 4, (uint32_t)start);
        } else {
            put32(value, (uint32_t)start);
        }
        put_prop(&pos, nameoff, value, start_len);

        if (end) {
            if (start_len == 8) {
                put32(value, (uint32_t)(end >> 32));
                put32(value + 4, (uint32_t)end);
            } else {
                put32(value, (uint32_t)end);
            }
            put_prop(&pos,
                     (uint32_t)(strlen(start_name) + 1), value, start_len);
        }
        put_token(&pos, FDT_END_NODE);
    }
    put_token(&pos, FDT_END_NODE);
    put_token(&pos, FDT_END);
    /* strings 在 0x300，total 必须覆盖它；结构块之后的内容按 blob 大小算。 */
    put32(blob + 4, BLOB_SIZE);
}

static void expect_ok64(void)
{
    uint64_t start = 0;
    uint64_t end = 0;

    build_fdt(8, 0x80000000ULL, 0x80100000ULL, 1);
    assert(fdt_chosen_initrd((u64)(uintptr_t)blob, &start, &end) == 1);
    assert(start == 0x80000000ULL && end == 0x80100000ULL);
    printf("fdt: 64-bit initrd ok\n");
}

static void expect_ok32(void)
{
    uint64_t start = 0;
    uint64_t end = 0;

    build_fdt(4, 0x12345000ULL, 0x12346000ULL, 1);
    assert(fdt_chosen_initrd((u64)(uintptr_t)blob, &start, &end) == 1);
    assert(start == 0x12345000ULL && end == 0x12346000ULL);
    printf("fdt: 32-bit initrd ok\n");
}

static void expect_missing(void)
{
    uint64_t start = 0;
    uint64_t end = 0;

    build_fdt(8, 0x80000000ULL, 0x80100000ULL, 0);
    assert(fdt_chosen_initrd((u64)(uintptr_t)blob, &start, &end) == 0);
    build_fdt(8, 0x80000000ULL, 0, 1);
    assert(fdt_chosen_initrd((u64)(uintptr_t)blob, &start, &end) == 0);
    printf("fdt: missing chosen/end rejected\n");
}

static void expect_bad_header(void)
{
    uint64_t start = 0;
    uint64_t end = 0;

    build_fdt(8, 0x80000000ULL, 0x80100000ULL, 1);
    put32(blob + 4, 39);
    assert(fdt_chosen_initrd((u64)(uintptr_t)blob, &start, &end) == 0);

    build_fdt(8, 0x80000000ULL, 0x80100000ULL, 1);
    put32(blob + 8, 41);
    assert(fdt_chosen_initrd((u64)(uintptr_t)blob, &start, &end) == 0);

    build_fdt(8, 0x80000000ULL, 0x80100000ULL, 1);
    put32(blob + 12, 0x500);
    assert(fdt_chosen_initrd((u64)(uintptr_t)blob, &start, &end) == 0);

    build_fdt(8, 0x80000000ULL, 0x80100000ULL, 1);
    put32(blob + 32, 0x200);
    assert(fdt_chosen_initrd((u64)(uintptr_t)blob, &start, &end) == 0);
    printf("fdt: bad header/offsets rejected\n");
}

static void expect_bad_prop(void)
{
    uint64_t start = 0;
    uint64_t end = 0;
    uint32_t pos = 40;

    build_fdt(8, 0x80000000ULL, 0x80100000ULL, 1);
    /* 找到 chosen 下的第一个 property 长度字段（结构块开头后第五个 token）。 */
    /* 直接损坏结构：在结构块内写一个大于 total 的 len。 */
    pos += 4; /* BEGIN_NODE root */
    pos += 4; /* empty name -> aligned */
    put_token(&pos, FDT_BEGIN_NODE);
    put_name(&pos, "chosen");
    put_token(&pos, FDT_PROP);
    put32(blob + pos, 0x400); /* len 超出 remaining */
    put32(blob + pos + 4, 0);
    assert(fdt_chosen_initrd((u64)(uintptr_t)blob, &start, &end) == 0);
    printf("fdt: oversized property rejected\n");
}

static void expect_bootinfo(void)
{
    struct boot_platform plat;
    struct bootinfo bi;
    uintptr_t base = (uintptr_t)blob;
    int rc;

    build_fdt(8, base + 0x600, base + 0x680, 1);
    plat = (struct boot_platform){
        .ram_base = base,
        .ram_size = 0x1000,
        .firmware_base = base + 0x500,
        .firmware_size = 0x100,
        .kernel_start = base + 0x400,
        .kernel_end = base + 0x500,
    };
    rc = bootinfo_build(&bi, &plat, 0, base);
    assert(rc == BOOT_DTB_VALID);
    assert(bootinfo_validate(&bi) == BOOTINFO_OK);
    assert(bi.reserved_count == 4); /* firmware/kernel/dtb/initrd */

    /* magic/version/size/ram_count 的基本校验。 */
    {
        struct bootinfo bad = bi;

        bad.magic = 0;
        assert(bootinfo_validate(&bad) == BOOTINFO_EBADMAGIC);
        bad = bi;
        bad.version = 0;
        assert(bootinfo_validate(&bad) == BOOTINFO_EBADVERSION);
        bad = bi;
        bad.size = 0;
        assert(bootinfo_validate(&bad) == BOOTINFO_EBADSIZE);
        bad = bi;
        bad.ram_count = 0;
        assert(bootinfo_validate(&bad) == BOOTINFO_EBADREGION);
    }

    /* 让 initrd 与 kernel 重叠，validate 必须拒绝。 */
    bi.reserved[3].base = plat.kernel_start;
    assert(bootinfo_validate(&bi) == BOOTINFO_EOVERLAP);

    /* DTB 不在 RAM 内。 */
    assert(bootinfo_build(&bi, &plat, 0, base + 0x2000) == BOOT_DTB_BAD);
    printf("bootinfo: initrd reserve and overlap rejection ok\n");
}

int main(void)
{
    expect_ok64();
    expect_ok32();
    expect_missing();
    expect_bad_header();
    expect_bad_prop();
    expect_bootinfo();
    printf("fdt: all tests passed\n");
    return 0;
}
