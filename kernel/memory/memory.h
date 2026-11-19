#ifndef KERNEL_MEMORY_MEMORY_H
#define KERNEL_MEMORY_MEMORY_H

#include <stdint.h>

#define PAGE_SIZE 4096UL

struct phys_mem_range {
    uint64_t start;
    uint64_t end;
};

/* QEMU virt 默认 -m 128M。设备树解析接入前，先按这个范围记录。 */
#define PHYS_MEM_BASE 0x80000000UL
#define PHYS_MEM_SIZE (128UL * 1024 * 1024)

/* 记录物理内存范围，并把内核镜像占用的页标记为已使用。 */
void memory_init(void);

const struct phys_mem_range *memory_phys_range(void);

/* 简单物理页分配器：成功返回页基地址，失败返回 0。 */
uint64_t pmm_alloc_page(void);
void pmm_free_page(uint64_t page_addr);
uint64_t pmm_free_page_count(void);

#endif
