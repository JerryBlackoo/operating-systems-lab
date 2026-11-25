/*
 * fdt_initrd.c — 从 DTB /chosen 读取 linux,initrd-start/end（QEMU -initrd）。
 */
#include <stdint.h>

#include "types.h"

#define FDT_BEGIN_NODE 1
#define FDT_END_NODE 2
#define FDT_PROP 3
#define FDT_NOP 4
#define FDT_END 9

static u32 fdt32(const u8 *p)
{
    return ((u32)p[0] << 24) | ((u32)p[1] << 16) | ((u32)p[2] << 8) | (u32)p[3];
}

static u64 fdt64(const u8 *p)
{
    return ((u64)fdt32(p) << 32) | fdt32(p + 4);
}

static int prop_name_eq(const char *base, u32 nameoff, u32 max, const char *want)
{
    const char *name = base + nameoff;
    u32 i = 0;

    while (i < max && want[i] && name[i] == want[i]) {
        i++;
    }
    return i < max && want[i] == 0 && name[i] == 0;
}

int fdt_chosen_initrd(u64 dtb, u64 *start, u64 *end)
{
    const u8 *blob = (const u8 *)(uintptr_t)dtb;
    u32 off_struct;
    u32 off_strings;
    u32 size_strings;
    u32 total;
    u32 pos;
    int depth;
    int in_chosen;
    u64 got_start;
    u64 got_end;

    if (dtb == 0 || start == 0 || end == 0) {
        return 0;
    }
    *start = 0;
    *end = 0;
    if (fdt32(blob) != 0xd00dfeedU) {
        return 0;
    }
    total = fdt32(blob + 4);
    if (total < 40) {
        return 0;
    }
    off_struct = fdt32(blob + 8);
    off_strings = fdt32(blob + 12);
    size_strings = fdt32(blob + 32);
    if ((off_struct & 3U) != 0 || off_struct >= total ||
        off_strings > total || size_strings > total - off_strings) {
        return 0;
    }

    got_start = 0;
    got_end = 0;
    depth = 0;
    in_chosen = 0;
    pos = off_struct;

    while (pos + 4 <= total) {
        u32 token = fdt32(blob + pos);
        pos += 4;

        if (token == FDT_NOP) {
            continue;
        }
        if (token == FDT_END) {
            break;
        }
        if (token == FDT_BEGIN_NODE) {
            const char *name = (const char *)(blob + pos);
            u32 nlen = 0;
            u32 room = total > pos ? total - pos : 0;

            while (nlen < room && name[nlen]) {
                nlen++;
            }
            if (nlen == room) {
                return 0; /* 节点名没有 NUL 或越过 blob 末尾 */
            }
            pos = (pos + nlen + 1U + 3U) & ~3U;
            depth++;
            if (nlen == 6U && name[0] == 'c' && name[1] == 'h' && name[2] == 'o' &&
                name[3] == 's' && name[4] == 'e' && name[5] == 'n') {
                in_chosen = 1;
            }
            continue;
        }
        if (token == FDT_END_NODE) {
            if (in_chosen && depth == 2) {
                in_chosen = 0;
            }
            if (depth > 0) {
                depth--;
            }
            continue;
        }
        if (token == FDT_PROP) {
            u32 len;
            u32 nameoff;
            const u8 *data;

            if (pos + 8U > total) {
                return 0;
            }
            len = fdt32(blob + pos);
            nameoff = fdt32(blob + pos + 4);
            data = blob + pos + 8;
            if (len > total - (pos + 8U)) {
                return 0;
            }
            pos = (pos + 8U + len + 3U) & ~3U;
            if (!in_chosen || nameoff >= size_strings) {
                continue;
            }
            {
                const char *base = (const char *)(blob + off_strings);
                u32 name_max = size_strings - nameoff;
                int is_start =
                    prop_name_eq(base, nameoff, name_max, "linux,initrd-start") ||
                    prop_name_eq(base, nameoff, name_max, "initrd-start");
                int is_end = prop_name_eq(base, nameoff, name_max,
                                          "linux,initrd-end") ||
                             prop_name_eq(base, nameoff, name_max, "initrd-end");

                if (is_start) {
                    if (len >= 8) {
                        got_start = fdt64(data);
                    } else if (len >= 4) {
                        got_start = fdt32(data);
                    }
                }
                if (is_end) {
                    if (len >= 8) {
                        got_end = fdt64(data);
                    } else if (len >= 4) {
                        got_end = fdt32(data);
                    }
                }
            }
        }
    }

    if (got_start == 0 || got_end <= got_start) {
        return 0;
    }
    *start = got_start;
    *end = got_end;
    return 1;
}