#ifndef KERNEL_MEMORY_MEMORY_H
#define KERNEL_MEMORY_MEMORY_H

#include <stdint.h>

#define PAGE_SIZE 4096UL

struct phys_mem_range {
    uint64_t start;
    uint64_t end;
};

/* Zynq 的 DDR 从 0x00100000 开始（低 1 MiB 是 OCM），
 * 512 MiB DDR 的可用范围是 0x00100000..0x20000000。
 * QEMU virt 默认 -m 128M。设备树解析接入前，先按这些范围记录。 */
#ifdef PLATFORM_ZYNQ
#define PHYS_MEM_BASE 0x00100000UL
#define PHYS_MEM_SIZE (511UL * 1024 * 1024)
#else
#define PHYS_MEM_BASE 0x80000000UL
#define PHYS_MEM_SIZE (128UL * 1024 * 1024)
#endif

/* 记录物理内存范围，并把内核镜像占用的页标记为已使用。 */
void memory_init(void);

const struct phys_mem_range *memory_phys_range(void);

/* 简单物理页分配器：成功返回页基地址，失败返回 0。 */
uint64_t pmm_alloc_page(void);
/* 分配 count 个连续物理页；失败返回 0。 */
uint64_t pmm_alloc_pages(uint64_t count);
/* 分配 count 个连续物理页，起始页号按 align_pages 对齐。 */
uint64_t pmm_alloc_pages_aligned(uint64_t count, uint64_t align_pages);
void pmm_free_page(uint64_t page_addr);
void pmm_free_pages(uint64_t page_addr, uint64_t count);
uint64_t pmm_free_page_count(void);

#endif
