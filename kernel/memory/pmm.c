#include "memory/memory.h"

#include "arch/riscv/sbi.h"
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
    uint64_t kernel_end = (uint64_t)__kernel_end;
    uint64_t reserved_bytes;
    uint64_t reserved_pages;

    if (kernel_end < ram.start || kernel_end > ram.end) {
        console_write("[mem] kernel image does not fit in recorded RAM\n");
        sbi_shutdown();
        for (;;) {
            __asm__ volatile("wfi");
        }
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
    console_write(" (128 MiB)\n");
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

uint64_t pmm_alloc_page(void)
{
    for (uint64_t i = alloc_hint; i < PAGE_COUNT; i++) {
        if (!page_used(i)) {
            page_mark_used(i);
            free_pages--;
            alloc_hint = i + 1;
            return ram.start + i * PAGE_SIZE;
        }
    }
    return 0;
}

void pmm_free_page(uint64_t page_addr)
{
    uint64_t index;

    if (page_addr < ram.start || page_addr >= ram.end) {
        return;
    }
    if ((page_addr & (PAGE_SIZE - 1)) != 0) {
        return;
    }

    index = (page_addr - ram.start) / PAGE_SIZE;
    if (!page_used(index)) {
        return;
    }

    page_mark_free(index);
    free_pages++;
    if (index < alloc_hint) {
        alloc_hint = index;
    }
}

uint64_t pmm_free_page_count(void)
{
    return free_pages;
}
