/*
 * bootinfo.h — boot 与内核其余部分之间唯一的启动契约。
 *
 * 为什么需要它：
 *   arch 交接时只给了 a0(hartid) / a1(dtb)。这些是「固件方言」，
 *   不是内核其余部分该理解的东西。boot 的职责就是把它们归一化成
 *   一个与固件无关、带版本号的结构。
 *
 * 三个硬要求（Linux boot_params 的第一课）：
 *   1. magic/version/size 必须存在 —— 消费方要能验证自己读的是什么。
 *   2. 区域是「基址 + 长度」，不是「起止指针」—— 避免 end 指向最后一个字节
 *      还是最后一个字节之后这种歧义。
 *   3. 保留区必须显式列出 —— 「可用 RAM」不等于「现在可以写」。
 */
#ifndef MICRO_BOOT_BOOTINFO_H
#define MICRO_BOOT_BOOTINFO_H

#include "types.h"

#define BOOTINFO_MAGIC   0x424f4f54UL   /* "BOOT" */
#define BOOTINFO_VERSION 1
#define BOOTINFO_MAX_REGIONS 8

enum boot_region_type {
    BOOT_REGION_RAM      = 0,
    BOOT_REGION_FIRMWARE = 1,
    BOOT_REGION_KERNEL   = 2,
    BOOT_REGION_DTB      = 3,
    BOOT_REGION_INITRD   = 4,
};

struct boot_region {
    u64 base;
    u64 size;
    u32 type;
    u32 pad;        /* 显式填充，保证 8 字节对齐 */
};

/*
 * 平台事实。由 boot.c 从 arch 的 platform.h 与链接脚本填入，
 * bootinfo.c 不直接依赖任何 arch 头文件。
 */
struct boot_platform {
    u64 ram_base;
    u64 ram_size;
    u64 firmware_base;
    u64 firmware_size;
    u64 kernel_start;
    u64 kernel_end;
};

struct bootinfo {
    u32 magic;
    u16 version;
    u16 size;
    u64 hartid;
    u64 boot_cpu;
    u64 dtb;
    u64 dtb_size;
    u32 ram_count;
    u32 reserved_count;
    struct boot_region ram[BOOTINFO_MAX_REGIONS];
    struct boot_region reserved[BOOTINFO_MAX_REGIONS];
    u64 kernel_start;
    u64 kernel_end;
};

_Static_assert(sizeof(struct bootinfo) <= 0xffff,
               "BootInfo must fit the u16 size field");

/* bootinfo_build 的 DTB 结论 */
#define BOOT_DTB_NONE   0
#define BOOT_DTB_VALID  1
#define BOOT_DTB_BAD   (-1)

/* bootinfo_validate 的返回码 */
#define BOOTINFO_OK           0
#define BOOTINFO_EBADMAGIC   (-1)
#define BOOTINFO_EBADVERSION (-2)
#define BOOTINFO_EBADSIZE    (-3)
#define BOOTINFO_EBADREGION  (-4)
#define BOOTINFO_EOVERLAP    (-5)
#define BOOTINFO_ENOTINRAM   (-6)

/*
 * 构建 BootInfo。
 * 返回 BOOT_DTB_NONE / BOOT_DTB_VALID / BOOT_DTB_BAD。
 * 只校验 FDT 头的魔数、长度和所在 RAM 范围，不解析节点。
 */
int bootinfo_build(struct bootinfo *bi, const struct boot_platform *p,
                   u64 hartid, u64 dtb);

/* 校验 BootInfo 自身的一致性；返回 BOOTINFO_*。 */
int bootinfo_validate(const struct bootinfo *bi);

#endif /* MICRO_BOOT_BOOTINFO_H */
