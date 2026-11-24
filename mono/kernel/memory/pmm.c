#include "memory/memory.h"

#include "arch.h"
#include "console.h"

#define PAGE_SHIFT 12
#define PAGE_COUNT (PHYS_MEM_SIZE / PAGE_SIZE)

extern char __kernel_end[];

static struct phys_mem_range ram = {
    .start = PHYS_MEM_BASE,
    .end = PHYS_MEM_BASE + PHYS_MEM_SIZE,
};

/* 1 bit = 1 页，128 MiB / 4 KiB / 8 = 4 KiB 位图，放在内核 BSS 中。 */
static uint8_t page_bitmap[PAGE_COUNT / 8];
static uint64_t free_pages;
static uint64_t alloc_hint;

static int page_used(uint64_t index)
{
    return (page_bitmap[index / 8] >> (index % 8)) & 1;
}

static void page_mark_used(uint64_t index)
{
    page_bitmap[index / 8] |= (uint8_t)(1u << (index % 8));
}

static void page_mark_free(uint64_t index)
{
    page_bitmap[index / 8] &= (uint8_t)~(1u << (index % 8));
}

void memory_init(void)
{
    uint64_t kernel_end = (uintptr_t)__kernel_end;
    uint64_t reserved_bytes;
    uint64_t reserved_pages;

    if (kernel_end < ram.start || kernel_end > ram.end) {
        console_write("[mem] kernel image does not fit in recorded RAM\n");
        arch_halt();
    }

    for (uint64_t i = 0; i < PAGE_COUNT; i++) {
        page_mark_free(i);
    }

    /* [RAM 起点, 内核结束) 全部保留，位图本身也在内核 BSS 内。 */
    reserved_bytes = kernel_end - ram.start;
    reserved_bytes = (reserved_bytes + PAGE_SIZE - 1) & ~(PAGE_SIZE - 1);
    reserved_pages = reserved_bytes / PAGE_SIZE;
    for (uint64_t i = 0; i < reserved_pages; i++) {
        page_mark_used(i);
    }

    free_pages = PAGE_COUNT - reserved_pages;
    alloc_hint = reserved_pages;

    console_write("[mem] physical memory: ");
    console_write_hex(ram.start);
    console_write(" - ");
    console_write_hex(ram.end);
    console_write(" (");
    console_write_dec(PHYS_MEM_SIZE / (1024UL * 1024UL));
    console_write(" MiB)\n");
    console_write("[mem] kernel ends at ");
    console_write_hex(ram.start + reserved_pages * PAGE_SIZE);
    console_write(", page allocator ready with ");
    console_write_dec(free_pages);
    console_write(" free pages\n");
}

const struct phys_mem_range *memory_phys_range(void)
{
    return &ram;
}

/* 调用者需已关中断。从 from 开始找一页空闲页并标记占用。 */
static uint64_t alloc_page_from(uint64_t from)
{
    for (uint64_t i = from; i < PAGE_COUNT; i++) {
        if (!page_used(i)) {
            page_mark_used(i);
            free_pages--;
            alloc_hint = i + 1;
            return ram.start + i * PAGE_SIZE;
        }
    }
    return 0;
}

uint64_t pmm_alloc_page(void)
{
    uint64_t flags = arch_intr_save();
    uint64_t page = alloc_page_from(alloc_hint);

    /* 对齐分配会跳过 alloc_hint 以下的空闲页；提示位扫描失败时回头补扫，
     * 避免「仍有空闲页却返回 0」。 */
    if (page == 0 && alloc_hint != 0) {
        page = alloc_page_from(0);
    }
    arch_intr_restore(flags);
    return page;
}

/* 调用者需已关中断。从 from 开始找 count 个连续空闲页并全部标记占用。 */
static uint64_t alloc_pages_from(uint64_t from, uint64_t count)
{
    uint64_t run = 0;
    uint64_t start = 0;

    for (uint64_t i = from; i < PAGE_COUNT; i++) {
        if (!page_used(i)) {
            if (run == 0) {
                start = i;
            }
            run++;
            if (run == count) {
                for (uint64_t j = start; j < start + count; j++) {
                    page_mark_used(j);
                }
                free_pages -= count;
                alloc_hint = start + count;
                return ram.start + start * PAGE_SIZE;
            }
        } else {
            run = 0;
        }
    }
    return 0;
}

uint64_t pmm_alloc_pages(uint64_t count)
{
    uint64_t flags;
    uint64_t page;

    if (count == 0) {
        return 0;
    }
    /* 位图是「读-改-写」；关中断避免在 kmain 与缺页处理之间交错修改同一字节。 */
    flags = arch_intr_save();
    page = alloc_pages_from(alloc_hint, count);
    if (page == 0 && alloc_hint != 0) {
        page = alloc_pages_from(0, count);
    }
    arch_intr_restore(flags);
    return page;
}

uint64_t pmm_alloc_pages_aligned(uint64_t count, uint64_t align_pages)
{
    uint64_t flags;

    if (count == 0 || align_pages == 0) {
        return 0;
    }
    flags = arch_intr_save();
    for (uint64_t start = 0; start + count <= PAGE_COUNT; start += align_pages) {
        uint64_t i = 0;

        for (; i < count; i++) {
            if (page_used(start + i)) {
                break;
            }
        }
        if (i == count) {
            for (uint64_t j = 0; j < count; j++) {
                page_mark_used(start + j);
            }
            free_pages -= count;
            if (start + count > alloc_hint) {
                alloc_hint = start + count;
            }
            arch_intr_restore(flags);
            return ram.start + start * PAGE_SIZE;
        }
    }
    arch_intr_restore(flags);
    return 0;
}

void pmm_free_page(uint64_t page_addr)
{
    uint64_t index;
    uint64_t flags = arch_intr_save();

    if (page_addr < ram.start || page_addr >= ram.end) {
        arch_intr_restore(flags);
        return;
    }
    if ((page_addr & (PAGE_SIZE - 1)) != 0) {
        arch_intr_restore(flags);
        return;
    }

    index = (page_addr - ram.start) / PAGE_SIZE;
    if (!page_used(index)) {
        arch_intr_restore(flags);
        return;
    }

    page_mark_free(index);
    free_pages++;
    if (index < alloc_hint) {
        alloc_hint = index;
    }
    arch_intr_restore(flags);
}

void pmm_free_pages(uint64_t page_addr, uint64_t count)
{
    for (uint64_t i = 0; i < count; i++) {
        pmm_free_page(page_addr + i * PAGE_SIZE);
    }
}

uint64_t pmm_free_page_count(void)
{
    return free_pages;
}
