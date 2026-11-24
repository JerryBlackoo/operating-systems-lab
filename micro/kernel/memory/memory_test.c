#define _POSIX_C_SOURCE 200112L

#include "memory.h"

#include <assert.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define TEST_BASE 0x80000000ULL
#define TEST_SIZE (8ULL * 1024 * 1024)
#define USER_VA 0x40000000ULL

static uint8_t *phys;

static void test_map_copy_unmap(void)
{
    uint64_t baseline = frame_free_count();
    pfn_t data = frame_alloc();
    pfn_t data2 = frame_alloc();
    struct as *as;
    struct as *overflow[AS_MAX];
    int rights = 0;
    pfn_t translated = PFN_NONE;
    uint8_t buf[200];
    uint8_t pattern[16];
    uint8_t *data_ptr;
    uint8_t *data2_ptr;

    assert(data != PFN_NONE);
    assert(data2 != PFN_NONE);
    data_ptr = (uint8_t *)frame_ptr(data);
    data2_ptr = (uint8_t *)frame_ptr(data2);
    assert(data_ptr != 0 && data2_ptr != 0);

    for (size_t i = 0; i < PAGE_SIZE; i++) {
        data_ptr[i] = (uint8_t)i;
        data2_ptr[i] = (uint8_t)(i ^ 0xffu);
    }

    as = as_create();
    assert(as != 0);
    assert(frame_free_count() == baseline - 3); /* data + data2 + root */

    assert(as_map(as, USER_VA, data, MEM_R | MEM_W) == MEM_OK);
    assert(frame_refcount(data) == 2);
    frame_unref(data); /* 把分配引用转交给映射 */
    assert(frame_refcount(data) == 1);

    assert(as_map(as, USER_VA + PAGE_SIZE, data2, MEM_R | MEM_W) == MEM_OK);
    frame_unref(data2);
    assert(frame_refcount(data2) == 1);

    /* 重复映射 */
    assert(as_map(as, USER_VA, data, MEM_R | MEM_W) == MEM_EEXIST);

    /* 地址翻译，包括非页对齐地址 */
    assert(as_translate(as, USER_VA, &rights, &translated) == MEM_OK);
    assert(translated == data);
    assert((rights & (MEM_R | MEM_W)) == (MEM_R | MEM_W));
    assert(as_translate(as, USER_VA + 100, &rights, &translated) == MEM_OK);
    assert(translated == data);

    /* 缺页 / 权限 */
    assert(as_fault(as, USER_VA, MEM_R | MEM_W, &translated) == MEM_OK);
    assert(as_fault(as, USER_VA, MEM_X, 0) == MEM_EPERM);
    assert(as_fault(as, USER_VA + 0x10000000ULL, MEM_R, 0) == MEM_EFAULT);

    /* 用户拷贝 */
    assert(as_copyin(as, USER_VA, buf, 16) == MEM_OK);
    for (int i = 0; i < 16; i++) {
        assert(buf[i] == (uint8_t)i);
    }
    for (int i = 0; i < 16; i++) {
        pattern[i] = (uint8_t)(0xa0 + i);
    }
    assert(as_copyout(as, USER_VA, pattern, 16) == MEM_OK);
    for (int i = 0; i < 16; i++) {
        assert(data_ptr[i] == pattern[i]);
    }

    /* 跨页拷贝：先 data 的尾部 100 字节，再 data2 的头部 100 字节 */
    assert(as_copyin(as, USER_VA + PAGE_SIZE - 100, buf, 200) == MEM_OK);
    for (int i = 0; i < 100; i++) {
        assert(buf[i] == data_ptr[PAGE_SIZE - 100 + i]);
        assert(buf[100 + i] == data2_ptr[i]);
    }

    /* 参数检查 */
    assert(as_map(as, USER_VA + 1, data, MEM_R) == MEM_EINVAL);
    assert(as_map(as, 0x0000800000000000ULL, data, MEM_R) == MEM_EINVAL);
    {
        pfn_t free_pfn = frame_alloc();
        assert(free_pfn != PFN_NONE);
        assert(frame_free(free_pfn) == MEM_OK);
        assert(as_map(as, USER_VA + 2 * PAGE_SIZE, free_pfn, MEM_R) ==
               MEM_EINVAL);
    }
    /* 未定义的权限位必须拒绝，不能静默忽略 */
    assert(as_map(as, USER_VA + 3 * PAGE_SIZE, data, MEM_R | 0x40) ==
           MEM_EINVAL);
    assert(as_unmap(as, USER_VA + 0x20000000ULL) == MEM_EFAULT);

    /* 只读映射：写拷贝应失败，读拷贝应成功 */
    assert(as_unmap(as, USER_VA + PAGE_SIZE) == MEM_OK);
    assert(frame_refcount(data2) == 0);
    {
        pfn_t ro = frame_alloc();
        assert(ro != PFN_NONE);
        assert(as_map(as, USER_VA + PAGE_SIZE, ro, MEM_R) == MEM_OK);
        frame_unref(ro);
        assert(as_copyin(as, USER_VA + PAGE_SIZE, buf, 16) == MEM_OK);
        assert(as_copyout(as, USER_VA + PAGE_SIZE, pattern, 16) == MEM_EPERM);
        assert(as_unmap(as, USER_VA + PAGE_SIZE) == MEM_OK);
    }

    /*
     * 全量校验：第二页未映射时，跨页拷贝必须整体失败，
     * 已校验的第一页既不能被写入也不能被读出。
     */
    memset(buf, 0x5a, sizeof(buf));
    assert(as_copyin(as, USER_VA + PAGE_SIZE - 100, buf, 200) == MEM_EFAULT);
    for (size_t i = 0; i < sizeof(buf); i++) {
        assert(buf[i] == 0x5a);
    }
    {
        uint8_t saved = data_ptr[PAGE_SIZE - 100];

        assert(as_copyout(as, USER_VA + PAGE_SIZE - 100, pattern, 200) ==
               MEM_EFAULT);
        assert(data_ptr[PAGE_SIZE - 100] == saved);
    }

    assert(as_unmap(as, USER_VA) == MEM_OK);
    assert(frame_refcount(data) == 0);
    as_unref(as);
    assert(frame_free_count() == baseline);

    /*
     * 中间层按需分配失败时必须回滚：新 VA 需要 L1 + L0 两个页表页，
     * 只留 1 个空闲帧时 as_map 返回 MEM_ENOMEM，且空闲帧数不变。
     */
    {
        struct as *fresh = as_create();
        pfn_t hog[MEMORY_MAX_FRAMES];
        uint32_t count = 0;

        assert(fresh != 0);
        while (frame_free_count() > 1) {
            pfn_t page = frame_alloc();

            assert(page != PFN_NONE);
            hog[count++] = page;
        }
        assert(count > 0);
        assert(as_map(fresh, 0x50000000ULL, hog[0], MEM_R) == MEM_ENOMEM);
        assert(frame_free_count() == 1);
        while (count > 0) {
            assert(frame_free(hog[--count]) == MEM_OK);
        }
        as_unref(fresh);
        assert(frame_free_count() == baseline);
    }

    /* AS 池耗尽 */
    for (int i = 0; i < AS_MAX; i++) {
        overflow[i] = as_create();
        assert(overflow[i] != 0);
    }
    assert(as_create() == 0);
    for (int i = 0; i < AS_MAX; i++) {
        as_unref(overflow[i]);
    }
    assert(frame_free_count() == baseline);

    printf("memory: map/copy/unmap ok\n");
}

static void test_reserve_and_exhaust(void)
{
    uint64_t baseline = frame_free_count();
    pfn_t pfn = TEST_BASE >> PAGE_SHIFT;
    pfn_t pages[MEMORY_MAX_FRAMES];
    uint32_t count = 0;

    frame_reserve(TEST_BASE, 3 * PAGE_SIZE);
    assert(frame_free_count() == baseline - 3);
    for (pfn_t i = 0; i < 3; i++) {
        assert(frame_refcount(pfn + i) == 1);
        assert(frame_free(pfn + i) == MEM_OK);
    }
    assert(frame_free_count() == baseline);

    while (1) {
        pfn_t allocated = frame_alloc();
        if (allocated == PFN_NONE) {
            break;
        }
        assert(count < MEMORY_MAX_FRAMES);
        pages[count++] = allocated;
    }
    assert(frame_free_count() == 0);
    assert(count == baseline);

    for (uint32_t i = 0; i < count; i++) {
        assert(frame_free(pages[i]) == MEM_OK);
    }
    assert(frame_free_count() == baseline);

    printf("memory: reserve/exhaust ok\n");
}

static void test_kernel_share(void)
{
    uint64_t baseline = frame_free_count();
    struct as *kernel = as_create();
    struct as *user = as_create();
    pfn_t kpage = frame_alloc();
    pfn_t upage = frame_alloc();
    int rights = 0;
    pfn_t translated = PFN_NONE;
    uint8_t buf[16];

    assert(kernel != 0 && user != 0);
    assert(kpage != PFN_NONE && upage != PFN_NONE);

    assert(as_map(kernel, TEST_BASE, kpage, MEM_R | MEM_W | MEM_KERNEL) ==
           MEM_OK);
    frame_unref(kpage);
    assert(frame_refcount(kpage) == 1);

    assert(as_share_kernel(user, kernel->root_pfn, 2) == MEM_OK);
    assert(as_share_kernel(user, kernel->root_pfn, 2) == MEM_EEXIST);
    assert(as_translate(user, TEST_BASE, &rights, &translated) == MEM_OK);
    assert(translated == kpage);
    assert((rights & MEM_U) == 0);
    assert(as_copyin(user, TEST_BASE, buf, 16) == MEM_EPERM);

    /* 用户 AS 不能覆盖共享内核区间 */
    assert(as_map(user, TEST_BASE, upage, MEM_R) == MEM_EPERM);
    assert(as_unmap(user, TEST_BASE) == MEM_EPERM);

    /* 目标顶层区间已有映射时，内核共享必须拒绝，不能覆盖并泄漏 frame */
    {
        struct as *conflict = as_create();

        assert(conflict != 0);
        assert(as_map(conflict, TEST_BASE, upage, MEM_R | MEM_U) == MEM_OK);
        assert(as_share_kernel(conflict, kernel->root_pfn, 2) == MEM_EEXIST);
        assert(as_unmap(conflict, TEST_BASE) == MEM_OK);
        as_unref(conflict);
    }
    assert(frame_free(upage) == MEM_OK);

    as_unref(user);
    assert(frame_refcount(kpage) == 1);
    assert(as_translate(kernel, TEST_BASE, &rights, &translated) == MEM_OK);

    as_unref(kernel);
    assert(frame_free_count() == baseline);

    printf("memory: kernel share ok\n");
}

/*
 * as_map_kernel_range() 的叶子是非拥有映射：它不增加引用，as_unmap 和
 * AS 销毁都不得 frame_unref 目标页。用一个已由测试持有（refcount=1）
 * 的池帧验证：若回收路径误 unref，frame_refcount 会掉到 0。
 */
static void test_nonowned_kernel_map(void)
{
    uint64_t baseline = frame_free_count();
    struct as *kernel = as_create();
    pfn_t owned = frame_alloc();
    vaddr_t va;
    int rights = 0;
    pfn_t translated = PFN_NONE;

    assert(kernel != 0);
    assert(owned != PFN_NONE);
    assert(frame_refcount(owned) == 1);
    va = (vaddr_t)frame_paddr(owned);

    assert(as_map_kernel_range(kernel, (paddr_t)va, PAGE_SIZE) == MEM_OK);
    assert(as_translate(kernel, va, &rights, &translated) == MEM_OK);
    assert(translated == owned);
    assert((rights & MEM_U) == 0);
    assert((rights & (MEM_R | MEM_W | MEM_X)) == (MEM_R | MEM_W | MEM_X));
    assert(frame_refcount(owned) == 1); /* 非拥有映射不增加引用 */

    /* as_unmap 非拥有叶子不得释放 owner 的页 */
    assert(as_unmap(kernel, va) == MEM_OK);
    assert(frame_refcount(owned) == 1);

    /* 重新映射后销毁 AS，同样不得误回收 */
    assert(as_map_kernel_range(kernel, (paddr_t)va, PAGE_SIZE) == MEM_OK);
    as_unref(kernel);
    assert(frame_refcount(owned) == 1);

    assert(frame_free(owned) == MEM_OK);
    assert(frame_free_count() == baseline);
    printf("memory: non-owned kernel map ok\n");
}

int main(void)
{
    void *memory = 0;

    assert(posix_memalign(&memory, PAGE_SIZE, TEST_SIZE) == 0);
    phys = (uint8_t *)memory;

    object_system_init();
    assert(frame_init(TEST_BASE, TEST_SIZE, phys) == MEM_OK);
    assert(frame_free_count() == TEST_SIZE / PAGE_SIZE);

    test_map_copy_unmap();
    test_kernel_share();
    test_nonowned_kernel_map();
    test_reserve_and_exhaust();

    printf("memory: all tests passed\n");
    free(memory);
    return 0;
}
