#include "vm.h"
#include "demo.h"
#include "memory.h"

/*
 * 两套验证：
 *   1. 内存替身：验证区间表、权限、驻留页复用和失败回滚；
 *   2. kernel/memory 端口：把 vm_port 真正绑到 frame/as API，验证
 *      alloc -> clear -> map -> unref 的引用交接。
 *
 * 因为第二套要调用 Sv39 页表操作，demo 在 S-mode 运行。模型本身不依赖
 * 特权级；正式系统里 vm_port 的实现位于内核，跨特权级的缝是 IPC。
 */

enum { FAKE_FRAMES = 8, FAKE_MAPPINGS = 8, REAL_FRAMES = 16 };

/* ---- 内存替身 ---------------------------------------------------- */

struct fake_frame { uint8_t bytes[VM_PAGE]; unsigned refs; };

struct fake_memory {
    struct fake_frame frames[FAKE_FRAMES];
    struct vm_page mapped[FAKE_MAPPINGS];
    unsigned frame_limit;
    unsigned allocations, clears, unmaps;
    unsigned expect_rights;
    bool check_rights;
    bool refuse_map;
};

static struct fake_memory fake;

static uint32_t fake_alloc(void *ctx)
{
    struct fake_memory *m = ctx;
    for (unsigned i = 0; i < m->frame_limit && i < FAKE_FRAMES; i++) {
        if (!m->frames[i].refs) {
            m->frames[i].refs = 1;
            m->allocations++;
            memset(m->frames[i].bytes, 0xa5, VM_PAGE);
            return i + 1;
        }
    }
    return 0;
}

static void fake_clear(void *ctx, uint32_t frame)
{
    struct fake_memory *m = ctx;
    CHECK(frame > 0 && frame <= m->frame_limit && m->frames[frame - 1].refs);
    memset(m->frames[frame - 1].bytes, 0, VM_PAGE);
    m->clears++;
}

static void fake_unref(void *ctx, uint32_t frame)
{
    struct fake_memory *m = ctx;
    CHECK(frame > 0 && frame <= m->frame_limit && m->frames[frame - 1].refs);
    m->frames[frame - 1].refs--;
}

static bool fake_map(void *ctx, uint32_t as, uint64_t va, uint32_t frame, unsigned rights)
{
    struct fake_memory *m = ctx;
    CHECK(frame > 0 && frame <= m->frame_limit && m->frames[frame - 1].refs);
    if (m->check_rights)
        CHECK(rights == m->expect_rights);
    for (unsigned byte = 0; byte < VM_PAGE; byte++)
        CHECK(m->frames[frame - 1].bytes[byte] == 0);
    if (m->refuse_map)
        return false;
    for (unsigned i = 0; i < FAKE_MAPPINGS; i++) {
        if (!m->mapped[i].as) {
            m->mapped[i] = (struct vm_page){as, va, frame};
            m->frames[frame - 1].refs++;
            return true;
        }
    }
    return false;
}

static void fake_unmap(void *ctx, uint32_t as, uint64_t va)
{
    struct fake_memory *m = ctx;
    for (unsigned i = 0; i < FAKE_MAPPINGS; i++) {
        if (m->mapped[i].as == as && m->mapped[i].va == va) {
            m->unmaps++;
            fake_unref(ctx, m->mapped[i].frame);
            m->mapped[i] = (struct vm_page){0};
            return;
        }
    }
    CHECK(false);
}

static const struct vm_port fake_port = {
    &fake, fake_alloc, fake_clear, fake_map, fake_unmap, fake_unref,
};

static struct fake_memory *fake_begin(struct vm *vm, unsigned frames)
{
    fake = (struct fake_memory){.frame_limit = frames};
    *vm = (struct vm){.port = &fake_port};
    return &fake;
}

static unsigned fake_refs(void)
{
    unsigned refs = 0;
    for (unsigned i = 0; i < FAKE_FRAMES; i++)
        refs += fake.frames[i].refs;
    return refs;
}

static bool fake_mapped(uint32_t as, uint64_t va)
{
    for (unsigned i = 0; i < FAKE_MAPPINGS; i++) {
        if (fake.mapped[i].as == as && fake.mapped[i].va == va)
            return true;
    }
    return false;
}

/* ---- 区间登记 ---------------------------------------------------- */

static void reserve_cases(void)
{
    struct vm vm;
    fake_begin(&vm, FAKE_FRAMES);

    CHECK(vm_reserve(&vm, 0, 0x1000, VM_PAGE, ABI_READ) == VM_BAD_RANGE);
    CHECK(vm_reserve(&vm, 1, 0x1001, VM_PAGE, ABI_READ) == VM_BAD_RANGE);
    CHECK(vm_reserve(&vm, 1, 0x1000, VM_PAGE + 1, ABI_READ) == VM_BAD_RANGE);
    CHECK(vm_reserve(&vm, 1, 0x1000, 0, ABI_READ) == VM_BAD_RANGE);
    CHECK(vm_reserve(&vm, 1, VM_LIMIT, VM_PAGE, ABI_READ) == VM_BAD_RANGE);
    CHECK(vm_reserve(&vm, 1, VM_LIMIT - VM_PAGE, 2 * VM_PAGE, ABI_READ) == VM_BAD_RANGE);

    CHECK(vm_reserve(&vm, 1, 0x1000, VM_PAGE, 0) == VM_DENIED);
    CHECK(vm_reserve(&vm, 1, 0x1000, VM_PAGE, 8) == VM_DENIED);
    CHECK(vm_reserve(&vm, 1, 0x1000, VM_PAGE, ABI_WRITE) == VM_DENIED);
    CHECK(vm_reserve(&vm, 1, 0x1000, VM_PAGE, ABI_READ | 8) == VM_DENIED);
    CHECK(vm_reserve(&vm, 1, 0x1000, VM_PAGE,
                     ABI_READ | ABI_WRITE | ABI_EXEC) == VM_DENIED);

    CHECK(vm_reserve(&vm, 1, 0x1000, 2 * VM_PAGE, ABI_READ | ABI_WRITE) == VM_OK);
    CHECK(vm_reserve(&vm, 1, 0x2000, VM_PAGE, ABI_READ) == VM_CONFLICT);
    CHECK(vm_reserve(&vm, 1, 0x3000, VM_PAGE, ABI_READ) == VM_OK); /* 邻接不算冲突 */
    CHECK(vm_reserve(&vm, 2, 0x1000, 2 * VM_PAGE, ABI_READ) == VM_OK); /* 不同 AS */
    CHECK(vm_reserve(&vm, 1, 0x4000, VM_PAGE, ABI_EXEC) == VM_OK);

    CHECK(vm_reserve(&vm, 1, 0x5000, VM_PAGE, ABI_READ) == VM_NO_SPACE);
    CHECK(vm_release(&vm, 1, 0x4000, VM_PAGE) == VM_OK);
    CHECK(vm_reserve(&vm, 1, 0x5000, VM_PAGE, ABI_READ) == VM_OK);
    demo_log("vm: reserve rejects alignment, overflow, bad rights and overlap");
}

/* ---- 缺页 -------------------------------------------------------- */

static void fault_cases(void)
{
    struct vm vm;
    struct fake_memory *m = fake_begin(&vm, FAKE_FRAMES);
    CHECK(vm_reserve(&vm, 1, 0x1000, 3 * VM_PAGE, ABI_READ | ABI_WRITE) == VM_OK);
    m->check_rights = true;
    m->expect_rights = ABI_READ | ABI_WRITE;

    CHECK(vm_fault(&vm, 1, 0x1000, ABI_READ) == VM_RESUME);
    CHECK(m->allocations == 1 && m->clears == 1 && m->frames[0].refs == 1);
    CHECK(m->mapped[0].as == 1 && m->mapped[0].va == 0x1000 && m->mapped[0].frame == 1);

    CHECK(vm_fault(&vm, 1, 0x1fff, ABI_READ) == VM_RESUME);
    CHECK(vm_fault(&vm, 1, 0x1000, ABI_WRITE) == VM_RESUME);
    CHECK(m->allocations == 1 && m->frames[0].refs == 1);

    CHECK(vm_fault(&vm, 1, 0x1000, ABI_EXEC) == VM_DENIED);
    CHECK(vm_fault(&vm, 1, 0x1000, 0) == VM_DENIED);
    CHECK(vm_fault(&vm, 1, 0x1000, ABI_READ | ABI_WRITE) == VM_DENIED);
    CHECK(vm_fault(&vm, 1, 0x4000, ABI_READ) == VM_DENIED);
    CHECK(vm_fault(&vm, 2, 0x1000, ABI_READ) == VM_DENIED);

    m->refuse_map = true;
    CHECK(vm_fault(&vm, 1, 0x2000, ABI_READ) == VM_MAP_ERROR);
    CHECK(m->allocations == 2 && m->frames[1].refs == 0);
    m->refuse_map = false;
    CHECK(vm_fault(&vm, 1, 0x2000, ABI_READ) == VM_RESUME);
    CHECK(m->frames[1].refs == 1 && fake_refs() == 2);
    demo_log("vm: demand-zero fault, resident reuse, MAP_ERROR rollback");
}

static void exhaustion_cases(void)
{
    struct vm vm;
    fake_begin(&vm, 2);
    CHECK(vm_reserve(&vm, 1, 0x1000, 3 * VM_PAGE, ABI_READ) == VM_OK);
    CHECK(vm_fault(&vm, 1, 0x1000, ABI_READ) == VM_RESUME);
    CHECK(vm_fault(&vm, 1, 0x2000, ABI_READ) == VM_RESUME);
    CHECK(vm_fault(&vm, 1, 0x3000, ABI_READ) == VM_NO_SPACE);
    CHECK(fake.allocations == 2 && fake_refs() == 2);

    fake_begin(&vm, FAKE_FRAMES);
    CHECK(vm_reserve(&vm, 1, 0x1000, (VM_PAGES + 1) * VM_PAGE, ABI_READ) == VM_OK);
    for (unsigned i = 0; i < VM_PAGES; i++)
        CHECK(vm_fault(&vm, 1, 0x1000 + (uint64_t)i * VM_PAGE, ABI_READ) == VM_RESUME);
    CHECK(vm_fault(&vm, 1, 0x1000 + (uint64_t)VM_PAGES * VM_PAGE, ABI_READ) == VM_NO_SPACE);
    CHECK(fake_refs() == VM_PAGES);
    demo_log("vm: frame and resident-table exhaustion are reported separately");
}

/* ---- 释放 -------------------------------------------------------- */

static void release_cases(void)
{
    struct vm vm;
    struct fake_memory *m = fake_begin(&vm, FAKE_FRAMES);
    CHECK(vm_reserve(&vm, 1, 0x1000, 4 * VM_PAGE, ABI_READ | ABI_WRITE) == VM_OK);
    CHECK(vm_reserve(&vm, 2, 0x1000, VM_PAGE, ABI_READ) == VM_OK);
    CHECK(vm_fault(&vm, 1, 0x1000, ABI_READ) == VM_RESUME);
    CHECK(vm_fault(&vm, 1, 0x2000, ABI_READ) == VM_RESUME);
    CHECK(vm_fault(&vm, 1, 0x4000, ABI_READ) == VM_RESUME);
    CHECK(vm_fault(&vm, 2, 0x1000, ABI_READ) == VM_RESUME);
    CHECK(fake_refs() == 4);

    CHECK(vm_release(&vm, 1, 0x1000, 3 * VM_PAGE) == VM_BAD_RANGE);
    CHECK(vm_release(&vm, 2, 0x1000, 4 * VM_PAGE) == VM_BAD_RANGE);
    CHECK(vm_release(&vm, 1, 0x1000, 4 * VM_PAGE) == VM_OK);
    CHECK(m->unmaps == 3 && fake_refs() == 1);
    CHECK(!fake_mapped(1, 0x1000) && !fake_mapped(1, 0x2000) && !fake_mapped(1, 0x4000));
    CHECK(fake_mapped(2, 0x1000));
    CHECK(vm_fault(&vm, 1, 0x1000, ABI_READ) == VM_DENIED);
    CHECK(vm_release(&vm, 1, 0x1000, 4 * VM_PAGE) == VM_BAD_RANGE);

    CHECK(vm_release(&vm, 2, 0x1000, VM_PAGE) == VM_OK);
    CHECK(m->unmaps == 4 && fake_refs() == 0);
    demo_log("vm: release unmaps every resident page and frees the region");

    /*
     * 槽位长期复用：region 表和驻留页表都是固定大小，反复
     * reserve→fault→release 必须能把槽位还回去。
     *
     * 这条路径过去在 resident 里根本没有入口（协议没有 release 消息），
     * 于是每次 reserve 都永久占用一个 region 槽，VM_REGIONS 用完后新缺页
     * 一律 VM_NO_SPACE。轮数刻意超过 VM_REGIONS 与 VM_PAGES。
     */
    {
        struct vm vm2;
        struct fake_memory *m2 = fake_begin(&vm2, FAKE_FRAMES);

        for (unsigned i = 0; i < VM_REGIONS * 3U; i++) {
            CHECK(vm_reserve(&vm2, 1, 0x10000, VM_PAGE,
                             ABI_READ | ABI_WRITE) == VM_OK);
            CHECK(vm_fault(&vm2, 1, 0x10000, ABI_READ) == VM_RESUME);
            CHECK(vm_release(&vm2, 1, 0x10000, VM_PAGE) == VM_OK);
        }
        CHECK(fake_refs() == 0);
        CHECK(m2->unmaps == VM_REGIONS * 3U);
        demo_log("vm: region and resident slots are reused across many cycles");

        /*
         * 进程退出清理：vm_forget 必须一次性丢掉该 AS 的所有 region 与
         * 驻留页。这是 PM 在 discard 后唯一能触达的回收入口——VM 自己
         * 看不到进程生命周期。
         */
        {
            struct vm vm3;
            struct fake_memory *m3 = fake_begin(&vm3, FAKE_FRAMES);

            for (unsigned i = 0; i < VM_REGIONS / 2; i++) {
                uint64_t va = 0x20000 + (uint64_t)i * VM_PAGE;

                CHECK(vm_reserve(&vm3, 5, va, VM_PAGE,
                                 ABI_READ | ABI_WRITE) == VM_OK);
                CHECK(vm_fault(&vm3, 5, va, ABI_READ) == VM_RESUME);
            }
            /* 另一个 AS 的区间必须不受影响。 */
            CHECK(vm_reserve(&vm3, 6, 0x90000, VM_PAGE, ABI_READ) == VM_OK);
            CHECK(vm_fault(&vm3, 6, 0x90000, ABI_READ) == VM_RESUME);
            CHECK(fake_refs() == VM_REGIONS / 2 + 1);
            CHECK(vm_forget(&vm3, 5) == VM_REGIONS / 2);
            CHECK(fake_refs() == 1);
            CHECK(vm_fault(&vm3, 5, 0x20000, ABI_READ) == VM_DENIED);
            CHECK(vm_fault(&vm3, 6, 0x90000, ABI_READ) == VM_RESUME);
            CHECK(vm_forget(&vm3, 0) == 0);
            CHECK(m3->unmaps == VM_REGIONS / 2);
        }
        demo_log("vm: forget drops one address space without touching others");
    }
}

/* ---- kernel/memory 端口 ------------------------------------------ */

struct kernel_memory {
    uint32_t ids[2];
    struct as *spaces[2];
};

static _Alignas(4096) uint8_t kernel_frames[REAL_FRAMES * PAGE_SIZE];

static struct as *kernel_space(struct kernel_memory *m, uint32_t as)
{
    for (unsigned i = 0; i < 2; i++) {
        if (m->ids[i] == as)
            return m->spaces[i];
    }
    return NULL;
}

static uint32_t kernel_alloc(void *ctx)
{
    (void)ctx;
    pfn_t pfn = frame_alloc();
    if (pfn == PFN_NONE)
        return 0;
    if (pfn > UINT32_MAX) {
        frame_unref(pfn);
        return 0;
    }
    return (uint32_t)pfn;
}

static void kernel_clear(void *ctx, uint32_t frame)
{
    (void)ctx;
    uint8_t *page = frame_ptr((pfn_t)frame);
    CHECK(page != NULL);
    memset(page, 0, PAGE_SIZE);
}

static bool kernel_map(void *ctx, uint32_t as, uint64_t va, uint32_t frame, unsigned rights)
{
    struct kernel_memory *memory = ctx;
    struct as *space = kernel_space(memory, as);
    if (!space)
        return false;
    return as_map(space, va, (pfn_t)frame, (int)rights | MEM_U) == MEM_OK;
}

static void kernel_unmap(void *ctx, uint32_t as, uint64_t va)
{
    struct kernel_memory *memory = ctx;
    struct as *space = kernel_space(memory, as);
    CHECK(space != NULL && as_unmap(space, va) == MEM_OK);
}

static void kernel_unref(void *ctx, uint32_t frame)
{
    (void)ctx;
    frame_unref((pfn_t)frame);
}

static void kernel_cases(void)
{
    static struct kernel_memory memory;
    static const struct vm_port port = {
        &memory, kernel_alloc, kernel_clear, kernel_map, kernel_unmap, kernel_unref,
    };

    object_system_init();
    CHECK(frame_init((paddr_t)(uintptr_t)kernel_frames, sizeof(kernel_frames),
                     kernel_frames) == MEM_OK);
    uint64_t free_start = frame_free_count();

    memory = (struct kernel_memory){0};
    memory.ids[0] = 7;
    memory.spaces[0] = as_create();
    CHECK(memory.spaces[0] != NULL);
    memory.ids[1] = 9;
    memory.spaces[1] = as_create();
    CHECK(memory.spaces[1] != NULL);

    struct vm vm = {.port = &port};
    CHECK(vm_reserve(&vm, 7, 0x10000, 2 * VM_PAGE, ABI_READ | ABI_WRITE) == VM_OK);
    CHECK(vm_reserve(&vm, 9, 0x10000, VM_PAGE, ABI_READ) == VM_OK);

    CHECK(vm_fault(&vm, 7, 0x10000, ABI_READ) == VM_RESUME);
    pfn_t pfn_a = PFN_NONE;
    int rights = 0;
    CHECK(as_translate(memory.spaces[0], 0x10000, &rights, &pfn_a) == MEM_OK);
    CHECK((rights & (MEM_R | MEM_W | MEM_U)) == (MEM_R | MEM_W | MEM_U));
    CHECK(frame_refcount(pfn_a) == 1);
    const uint8_t *page = frame_ptr(pfn_a);
    CHECK(page != NULL);
    for (size_t i = 0; i < PAGE_SIZE; i++)
        CHECK(page[i] == 0);
    CHECK(vm_fault(&vm, 7, 0x10fff, ABI_WRITE) == VM_RESUME);
    CHECK(frame_refcount(pfn_a) == 1);

    CHECK(vm_fault(&vm, 9, 0x10000, ABI_READ) == VM_RESUME);
    pfn_t pfn_b = PFN_NONE;
    CHECK(as_translate(memory.spaces[1], 0x10000, &rights, &pfn_b) == MEM_OK);
    CHECK(pfn_b != pfn_a && frame_refcount(pfn_b) == 1);

    /* 有区间但没有对应内核地址空间：映射失败，临时帧必须退回。 */
    CHECK(vm_reserve(&vm, 11, 0x20000, VM_PAGE, ABI_READ) == VM_OK);
    uint64_t free_before = frame_free_count();
    CHECK(vm_fault(&vm, 11, 0x20000, ABI_READ) == VM_MAP_ERROR);
    CHECK(frame_free_count() == free_before);

    CHECK(vm_release(&vm, 7, 0x10000, 2 * VM_PAGE) == VM_OK);
    CHECK(frame_refcount(pfn_a) == 0);
    CHECK(as_translate(memory.spaces[0], 0x10000, &rights, &pfn_a) == MEM_EFAULT);
    CHECK(frame_refcount(pfn_b) == 1);
    CHECK(vm_release(&vm, 9, 0x10000, VM_PAGE) == VM_OK);
    CHECK(frame_refcount(pfn_b) == 0);

    as_unref(memory.spaces[0]);
    as_unref(memory.spaces[1]);
    CHECK(frame_free_count() == free_start);
    demo_log("vm: vm_port bound to kernel frame/Sv39 map, refs returned on release");
}

static void run(void)
{
    reserve_cases();
    fault_cases();
    exhaustion_cases();
    release_cases();
    kernel_cases();
}

DEMO_MODULE(vm, false, run, NULL);
