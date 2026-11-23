#include "demo.h"
#include "memory.h"

static _Alignas(PAGE_SIZE) uint8_t frames[8 * PAGE_SIZE];

static void run(void)
{
    const vaddr_t va = 0x4000;
    const uint32_t payload = 0x12345678;
    uint32_t observed = 0;
    pfn_t pfn, translated;
    int rights;

    object_system_init();
    CHECK(frame_init((paddr_t)(uintptr_t)frames, sizeof(frames), frames) == MEM_OK);
    uint64_t free = frame_free_count();
    struct as *as = as_create();
    CHECK(as != NULL);
    pfn = frame_alloc();
    CHECK(pfn != PFN_NONE);
    CHECK(as_map(as, va, pfn, MEM_R | MEM_W) == MEM_OK);
    CHECK(as_map(as, va, pfn, MEM_R) == MEM_EEXIST);
    CHECK(frame_refcount(pfn) == 2);
    frame_unref(pfn);
    CHECK(as_translate(as, va, &rights, &translated) == MEM_OK && translated == pfn);
    CHECK((rights & (MEM_R | MEM_W)) == (MEM_R | MEM_W));
    CHECK(as_copyout(as, va + 5, &payload, sizeof(payload)) == MEM_OK);
    CHECK(as_copyin(as, va + 5, &observed, sizeof(observed)) == MEM_OK && observed == payload);
    CHECK(as_unmap(as, va) == MEM_OK && frame_refcount(pfn) == 0);
    CHECK(as_fault(as, va, MEM_R, &translated) == MEM_EFAULT);
    as_unref(as);
    CHECK(frame_free_count() == free);
    demo_log("memory: Sv39 map/copy/unmap and frame reclamation");
}

DEMO_MODULE(memory, false, run, NULL);
