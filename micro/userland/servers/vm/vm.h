#ifndef MICRO_MODEL_VM_H
#define MICRO_MODEL_VM_H

#include <stdbool.h>
#include "abi/abi.h"

enum { VM_PAGE = 4096, VM_REGIONS = 4, VM_PAGES = 8 };
enum vm_result { VM_OK, VM_BAD_RANGE, VM_CONFLICT, VM_DENIED, VM_NO_SPACE, VM_MAP_ERROR };

struct vm_port {
    void *ctx;
    uint32_t (*alloc)(void *);
    void (*clear)(void *, uint32_t frame);
    bool (*map)(void *, uint32_t as, uint64_t va, uint32_t frame, unsigned rights);
    void (*unmap)(void *, uint32_t as, uint64_t va);
    void (*unref)(void *, uint32_t frame);
};

struct vm_region { uint32_t as; uint64_t lo, hi; unsigned rights; };
struct vm_page { uint32_t as; uint64_t va; uint32_t frame; };
struct vm {
    const struct vm_port *port;
    struct vm_region regions[VM_REGIONS];
    struct vm_page pages[VM_PAGES];
};

enum vm_result vm_reserve(struct vm *, uint32_t as, uint64_t lo, uint64_t len, unsigned rights);
enum vm_result vm_fault(struct vm *, uint32_t as, uint64_t va, unsigned access);
enum vm_result vm_release(struct vm *, uint32_t as, uint64_t lo, uint64_t len);

#endif
