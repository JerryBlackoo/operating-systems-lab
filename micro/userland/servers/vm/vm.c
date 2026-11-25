#include <stddef.h>
#include "vm.h"

static bool valid_range(uint32_t as, uint64_t lo, uint64_t len)
{
    return as && len && !((lo | len) & (VM_PAGE - 1)) &&
           lo < VM_LIMIT && len <= VM_LIMIT - lo;
}

static const struct vm_region *find_region(const struct vm *vm, uint32_t as, uint64_t va)
{
    for (unsigned i = 0; i < VM_REGIONS; i++) {
        const struct vm_region *region = &vm->regions[i];
        if (region->as == as && region->lo <= va && va < region->hi)
            return region;
    }
    return NULL;
}

enum vm_result vm_reserve(struct vm *vm, uint32_t as, uint64_t lo, uint64_t len, unsigned rights)
{
    if (!valid_range(as, lo, len))
        return VM_BAD_RANGE;
    if (!rights || (rights & ~(ABI_READ | ABI_WRITE | ABI_EXEC)) ||
        ((rights & ABI_WRITE) && !(rights & ABI_READ)) ||
        ((rights & ABI_WRITE) && (rights & ABI_EXEC)))
        return VM_DENIED; /* 与 loader 一致：不生成 W+X 页 */


    const uint64_t hi = lo + len;
    struct vm_region *free_region = NULL;
    for (unsigned i = 0; i < VM_REGIONS; i++) {
        struct vm_region *region = &vm->regions[i];
        if (region->as == as && region->lo < hi && lo < region->hi)
            return VM_CONFLICT;
        if (!region->as && !free_region)
            free_region = region;
    }
    if (!free_region)
        return VM_NO_SPACE;

    *free_region = (struct vm_region){.as = as, .lo = lo, .hi = hi, .rights = rights};
    return VM_OK;
}

enum vm_result vm_fault(struct vm *vm, uint32_t as, uint64_t va, unsigned access)
{
    if (!as || (access != ABI_READ && access != ABI_WRITE && access != ABI_EXEC))
        return VM_DENIED;

    const struct vm_region *region = find_region(vm, as, va);
    if (!region || !(region->rights & access))
        return VM_DENIED;

    const uint64_t page_va = va & ~(uint64_t)(VM_PAGE - 1);
    struct vm_page *free_page = NULL;
    for (unsigned i = 0; i < VM_PAGES; i++) {
        struct vm_page *page = &vm->pages[i];
        if (page->as == as && page->va == page_va)
            return VM_RESUME; /* 已驻留：权限已在上方检查 */
        if (!page->as && !free_page)
            free_page = page;
    }
    if (!free_page)
        return VM_NO_SPACE;

    const struct vm_port *port = vm->port;
    const uint32_t frame = port->alloc(port->ctx);
    if (!frame)
        return VM_NO_SPACE;

    port->clear(port->ctx, frame);
    const bool mapped = port->map(port->ctx, as, page_va, frame, region->rights);
    /*
     * map 成功时页表持有自己的引用；这里统一释放分配时的临时引用。
     * map 失败时这是最后一枚引用，帧会被回收。
     */
    port->unref(port->ctx, frame);
    if (!mapped)
        return VM_MAP_ERROR;

    *free_page = (struct vm_page){.as = as, .va = page_va, .frame = frame};
    return VM_RESUME;
}

enum vm_result vm_release(struct vm *vm, uint32_t as, uint64_t lo, uint64_t len)
{
    if (!valid_range(as, lo, len))
        return VM_BAD_RANGE;

    const uint64_t hi = lo + len;
    for (unsigned i = 0; i < VM_REGIONS; i++) {
        struct vm_region *region = &vm->regions[i];
        if (region->as != as || region->lo != lo || region->hi != hi)
            continue;

        for (unsigned j = 0; j < VM_PAGES; j++) {
            struct vm_page *page = &vm->pages[j];
            if (page->as == as && lo <= page->va && page->va < hi) {
                vm->port->unmap(vm->port->ctx, as, page->va);
                *page = (struct vm_page){0};
            }
        }
        *region = (struct vm_region){0};
        return VM_OK;
    }
    return VM_BAD_RANGE;
}

unsigned vm_forget(struct vm *vm, uint32_t as)
{
    unsigned dropped = 0;

    if (vm == 0 || as == 0) {
        return 0;
    }
    for (unsigned i = 0; i < VM_PAGES; i++) {
        struct vm_page *page = &vm->pages[i];

        if (page->as == as) {
            vm->port->unmap(vm->port->ctx, as, page->va);
            *page = (struct vm_page){0};
        }
    }
    for (unsigned i = 0; i < VM_REGIONS; i++) {
        struct vm_region *region = &vm->regions[i];

        if (region->as == as) {
            *region = (struct vm_region){0};
            dropped++;
        }
    }
    return dropped;
}
