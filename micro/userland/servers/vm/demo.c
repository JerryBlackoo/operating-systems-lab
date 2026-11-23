#include "vm.h"
#include "demo.h"

static struct memory_fixture {
    struct { uint8_t bytes[VM_PAGE]; unsigned refs; } frames[2];
    struct vm_page mapped[2];
    unsigned allocations;
    bool refuse_map;
} memory;

static uint32_t alloc(void *ctx)
{
    struct memory_fixture *m = ctx;
    for (unsigned i = 0; i < 2; i++) {
        if (!m->frames[i].refs) {
            m->allocations++;
            m->frames[i].refs = 1;
            memset(m->frames[i].bytes, 0xa5, VM_PAGE);
            return i + 1;
        }
    }
    return 0;
}

static void clear(void *ctx, uint32_t frame)
{
    struct memory_fixture *m = ctx;
    memset(m->frames[frame - 1].bytes, 0, VM_PAGE);
}

static void unref(void *ctx, uint32_t frame)
{
    struct memory_fixture *m = ctx;
    CHECK(frame > 0 && frame <= 2 && m->frames[frame - 1].refs);
    m->frames[frame - 1].refs--;
}

static bool map(void *ctx, uint32_t as, uint64_t va, uint32_t frame, unsigned rights)
{
    struct memory_fixture *m = ctx;
    CHECK(rights == ABI_READ);
    for (unsigned byte = 0; byte < VM_PAGE; byte++)
        CHECK(m->frames[frame - 1].bytes[byte] == 0);
    if (m->refuse_map)
        return false;
    for (unsigned i = 0; i < 2; i++) {
        if (!m->mapped[i].as) {
            m->mapped[i] = (struct vm_page){as, va, frame};
            m->frames[frame - 1].refs++;
            return true;
        }
    }
    return false;
}

static void unmap(void *ctx, uint32_t as, uint64_t va)
{
    struct memory_fixture *m = ctx;
    for (unsigned i = 0; i < 2; i++) {
        if (m->mapped[i].as == as && m->mapped[i].va == va) {
            unref(ctx, m->mapped[i].frame);
            m->mapped[i] = (struct vm_page){0};
            return;
        }
    }
    CHECK(false);
}

static void run(void)
{
    const struct vm_port port = {&memory, alloc, clear, map, unmap, unref};
    struct vm vm = {.port = &port};
    CHECK(vm_reserve(&vm, 1, 4096, 8192, ABI_READ) == VM_OK);
    CHECK(vm_reserve(&vm, 1, 8192, 4096, ABI_READ) == VM_CONFLICT);
    CHECK(vm_reserve(&vm, 2, 4096, 4096, ABI_READ) == VM_OK);
    CHECK(vm_fault(&vm, 1, 4096, ABI_READ) == VM_OK && memory.frames[0].refs == 1);
    CHECK(vm_fault(&vm, 1, 4099, ABI_READ) == VM_OK && memory.allocations == 1);
    CHECK(vm_fault(&vm, 1, 4099, ABI_WRITE) == VM_DENIED);
    CHECK(vm_fault(&vm, 1, 12288, ABI_READ) == VM_DENIED);
    memory.refuse_map = true;
    CHECK(vm_fault(&vm, 1, 8192, ABI_READ) == VM_MAP_ERROR);
    CHECK(memory.frames[1].refs == 0);
    memory.refuse_map = false;
    CHECK(vm_fault(&vm, 2, 4096, ABI_READ) == VM_OK && memory.frames[1].refs == 1);
    CHECK(vm_release(&vm, 1, 4096, 8192) == VM_OK && memory.frames[0].refs == 0);
    CHECK(vm_fault(&vm, 1, 4096, ABI_READ) == VM_DENIED);
    CHECK(vm_release(&vm, 2, 4096, 4096) == VM_OK && memory.frames[1].refs == 0);
    demo_log("memory: demand-zero, per-AS mappings, permission and failure cleanup");
}

DEMO_MODULE(vm, true, run, NULL);
