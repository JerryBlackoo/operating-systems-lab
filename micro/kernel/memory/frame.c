#include "memory.h"

#include <stdint.h>

#define FRAME_INDEX_NONE ((uint32_t)-1)

static uint8_t used_bitmap[MEMORY_MAX_FRAMES / 8];
static uint16_t refcounts[MEMORY_MAX_FRAMES];

static pfn_t first_pfn;
static uint32_t frame_count;
static uint64_t free_count;
static uint32_t alloc_hint;
static void *direct_map;

static bool bitmap_used(uint32_t index)
{
    return (used_bitmap[index / 8] >> (index % 8)) & 1u;
}

static void bitmap_set(uint32_t index)
{
    used_bitmap[index / 8] |= (uint8_t)(1u << (index % 8));
}

static void bitmap_clear(uint32_t index)
{
    used_bitmap[index / 8] &= (uint8_t)~(1u << (index % 8));
}

static uint32_t frame_index(pfn_t pfn)
{
    uint64_t index;

    if (pfn < first_pfn) {
        return FRAME_INDEX_NONE;
    }
    index = pfn - first_pfn;
    if (index >= frame_count) {
        return FRAME_INDEX_NONE;
    }
    return (uint32_t)index;
}

int frame_init(paddr_t phys_base, size_t size, void *direct_map_ptr)
{
    uint64_t count64;
    uint32_t count;

    if (direct_map_ptr == 0 || size < PAGE_SIZE) {
        return MEM_EINVAL;
    }
    if ((phys_base & (paddr_t)(PAGE_SIZE - 1)) != 0) {
        return MEM_EINVAL;
    }

    count64 = (uint64_t)(size >> PAGE_SHIFT);
    if (count64 == 0 || count64 > MEMORY_MAX_FRAMES) {
        return MEM_EINVAL;
    }
    count = (uint32_t)count64;

    first_pfn = phys_base >> PAGE_SHIFT;
    frame_count = count;
    free_count = count;
    alloc_hint = 0;
    direct_map = direct_map_ptr;

    for (uint32_t i = 0; i < count; i++) {
        bitmap_clear(i);
        refcounts[i] = 0;
    }
    return MEM_OK;
}

void frame_reserve(paddr_t pa, size_t size)
{
    paddr_t start;
    paddr_t end;

    if (size == 0) {
        return;
    }
    start = pa & ~(paddr_t)(PAGE_SIZE - 1);
    end = (pa + size + PAGE_SIZE - 1) & ~(paddr_t)(PAGE_SIZE - 1);

    for (paddr_t addr = start; addr < end; addr += PAGE_SIZE) {
        uint32_t index = frame_index(addr >> PAGE_SHIFT);

        if (index == FRAME_INDEX_NONE || bitmap_used(index)) {
            continue;
        }
        bitmap_set(index);
        refcounts[index] = 1;
        free_count--;
    }
}

pfn_t frame_alloc(void)
{
    uint32_t index = FRAME_INDEX_NONE;

    if (frame_count == 0) {
        return PFN_NONE;
    }

    for (uint32_t i = alloc_hint; i < frame_count; i++) {
        if (!bitmap_used(i)) {
            index = i;
            break;
        }
    }
    if (index == FRAME_INDEX_NONE) {
        for (uint32_t i = 0; i < alloc_hint && i < frame_count; i++) {
            if (!bitmap_used(i)) {
                index = i;
                break;
            }
        }
    }
    if (index == FRAME_INDEX_NONE) {
        return PFN_NONE;
    }

    bitmap_set(index);
    refcounts[index] = 1;
    free_count--;
    alloc_hint = index + 1;
    return first_pfn + index;
}

static void frame_release_index(uint32_t index)
{
    bitmap_clear(index);
    refcounts[index] = 0;
    free_count++;
    if (index < alloc_hint) {
        alloc_hint = index;
    }
}

int frame_free(pfn_t pfn)
{
    uint32_t index = frame_index(pfn);

    if (index == FRAME_INDEX_NONE || !bitmap_used(index)) {
        return MEM_EINVAL;
    }
    if (refcounts[index] != 1) {
        return MEM_EINVAL;
    }

    frame_release_index(index);
    return MEM_OK;
}

void frame_ref(pfn_t pfn)
{
    uint32_t index = frame_index(pfn);

    if (index == FRAME_INDEX_NONE || !bitmap_used(index)) {
        return;
    }
    if (refcounts[index] == 0 || refcounts[index] == UINT16_MAX) {
        return;
    }
    refcounts[index]++;
}

void frame_unref(pfn_t pfn)
{
    uint32_t index = frame_index(pfn);

    if (index == FRAME_INDEX_NONE || !bitmap_used(index)) {
        return;
    }
    if (refcounts[index] == 0) {
        return;
    }

    refcounts[index]--;
    if (refcounts[index] == 0) {
        frame_release_index(index);
    }
}

uint32_t frame_refcount(pfn_t pfn)
{
    uint32_t index = frame_index(pfn);

    if (index == FRAME_INDEX_NONE) {
        return 0;
    }
    return refcounts[index];
}

uint64_t frame_free_count(void)
{
    return free_count;
}

void *frame_ptr(pfn_t pfn)
{
    uint32_t index = frame_index(pfn);

    if (index == FRAME_INDEX_NONE || direct_map == 0) {
        return 0;
    }
    return (uint8_t *)direct_map + ((size_t)index << PAGE_SHIFT);
}

paddr_t frame_paddr(pfn_t pfn)
{
    return pfn << PAGE_SHIFT;
}
