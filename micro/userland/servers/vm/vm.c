#include <stddef.h>
#include "vm.h"

static bool valid_range(uint32_t as, uint64_t lo, uint64_t len)
{
    const uint64_t limit = UINT64_C(1) << 38;
    return as && len && !((lo | len) & (VM_PAGE - 1)) && lo < limit && len <= limit - lo;
}

enum vm_result vm_reserve(struct vm *vm, uint32_t as, uint64_t lo, uint64_t len, unsigned rights)
{
    if (!valid_range(as, lo, len))
        return VM_BAD_RANGE;
    if (!rights || (rights & ~(ABI_READ | ABI_WRITE | ABI_EXEC)) ||
        ((rights & ABI_WRITE) && !(rights & ABI_READ)))
        return VM_DENIED;
    uint64_t hi = lo + len;
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
    unsigned rights = 0;
    for (unsigned i = 0; i < VM_REGIONS; i++) {
        const struct vm_region *r = &vm->regions[i];
        if (r->as == as && r->lo <= va && va < r->hi) {
            rights = r->rights;
            break;
        }
    }
    if (!(rights & access))
        return VM_DENIED;

    uint64_t page_va = va & ~(uint64_t)(VM_PAGE - 1);
    struct vm_page *free_page = NULL;
    for (unsigned i = 0; i < VM_PAGES; i++) {
        struct vm_page *p = &vm->pages[i];
        if (p->as == as && p->va == page_va)
            return VM_OK;
        if (!p->as && !free_page)
            free_page = p;
    }
    if (!free_page)
        return VM_NO_SPACE;

    const struct vm_port *port = vm->port;
    uint32_t frame = port->alloc(port->ctx);
    if (!frame)
        return VM_NO_SPACE;
    port->clear(port->ctx, frame);
    bool mapped = port->map(port->ctx, as, page_va, frame, rights);
    port->unref(port->ctx, frame);
    if (!mapped)
        return VM_MAP_ERROR;

    *free_page = (struct vm_page){.as = as, .va = page_va, .frame = frame};
    return VM_OK;
}

enum vm_result vm_release(struct vm *vm, uint32_t as, uint64_t lo, uint64_t len)
{
    if (!valid_range(as, lo, len))
        return VM_BAD_RANGE;
    uint64_t hi = lo + len;
    for (unsigned i = 0; i < VM_REGIONS; i++) {
        struct vm_region *r = &vm->regions[i];
        if (r->as != as || r->lo != lo || r->hi != hi)
            continue;
        for (unsigned j = 0; j < VM_PAGES; j++) {
            struct vm_page *p = &vm->pages[j];
            if (p->as == as && lo <= p->va && p->va < hi) {
                vm->port->unmap(vm->port->ctx, as, p->va);
                *p = (struct vm_page){0};
            }
        }
        *r = (struct vm_region){0};
        return VM_OK;
    }
    return VM_BAD_RANGE;
}
