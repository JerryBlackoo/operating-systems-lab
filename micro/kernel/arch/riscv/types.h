/*
 * types.h — arch 层用到的最小编号类型。
 * 不属于 arch 语义，只是为了让其它头文件不依赖 <stdint.h>（-ffreestanding 下不保证有）。
 * 将来 shared/ 定稿后应从这里搬走。
 */
#ifndef MICRO_ARCH_RISCV_TYPES_H
#define MICRO_ARCH_RISCV_TYPES_H

typedef unsigned char  u8;
typedef unsigned short u16;
typedef unsigned int   u32;
typedef unsigned long  u64;
typedef signed long    i64;
typedef unsigned long  usize;

/* Sv39 的页表项就是 8 字节。 */
typedef u64 pte_t;

#define PAGE_SHIFT 12
#define PAGE_SIZE  (1UL << PAGE_SHIFT)

#define NULL ((void *)0)

#endif /* MICRO_ARCH_RISCV_TYPES_H */
