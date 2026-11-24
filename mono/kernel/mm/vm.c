#include "mm/vm.h"

#include "arch/arch_vm.h"
#include "console.h"
#include "lib/kprintf.h"
#include "memory/memory.h"

void vm_init(void)
{
    arch_vm_init();
}

uint64_t vm_kernel_pagetable(void)
{
    return arch_vm_kernel_pagetable();
}

uint64_t vm_new(void)
{
    return arch_vm_new();
}

int vm_map(uint64_t pagetable, uint64_t va, uint64_t pa, int perm)
{
    return arch_vm_map(pagetable, va, pa, perm);
}

void vm_unmap(uint64_t pagetable, uint64_t va)
{
    arch_vm_unmap(pagetable, va);
}

uint64_t vm_translate(uint64_t pagetable, uint64_t va)
{
    return arch_vm_translate(pagetable, va);
}

void vm_switch(uint64_t pagetable)
{
    arch_vm_switch(pagetable);
}

void vm_free(uint64_t pagetable)
{
    arch_vm_free(pagetable);
}

int vm_enabled(void)
{
    return arch_vm_enabled();
}

/* 在 pt 上完成映射自测；调用前 pt 必须已生效，测试 VA 才能被访问。 */
static void vm_selftest_on(uint64_t pt)
{
    const uint64_t va = 0x50000000UL;
    const uint64_t ro_va = 0x50001000UL;
    volatile uint64_t *p;
    uint64_t page;
    uint64_t ro_page;

    page = pmm_alloc_page();
    if (page == 0) {
        kprintf("[vm] selftest: no free page\n");
        return;
    }
    if (vm_map(pt, va, page, VM_READ | VM_WRITE) != 0) {
        kprintf("[vm] selftest: map failed\n");
        pmm_free_page(page);
        return;
    }
    p = (volatile uint64_t *)va;
    *p = 0x12345678UL;
    if (*p != 0x12345678UL) {
        kprintf("[vm] selftest: readback mismatch\n");
    } else {
#ifdef PLATFORM_ZYNQ
        kprintf("[vm] selftest map/unmap ok (ARM 4KiB)\n");
#else
        kprintf("[vm] selftest map/unmap ok\n");
#endif
    }
    vm_unmap(pt, va);
    pmm_free_page(page);

    /* 只读映射自测：内核只读页可读；ARM 侧同时覆盖 AP[1:0]=01 + AP[2]=1
     * 的特权只读编码，避免再次落入 0b100 保留编码。 */
    ro_page = pmm_alloc_page();
    if (ro_page == 0) {
        kprintf("[vm] read-only selftest: no free page\n");
        return;
    }
    if (vm_map(pt, ro_va, ro_page, VM_READ) != 0) {
        kprintf("[vm] read-only selftest: map failed\n");
        pmm_free_page(ro_page);
        return;
    }
    p = (volatile uint64_t *)ro_va;
    (void)*p;
    vm_unmap(pt, ro_va);
    pmm_free_page(ro_page);
    kprintf("[vm] read-only map ok\n");
}

void vm_selftest(void)
{
    uint64_t free_before = pmm_free_page_count();
    /* 用临时页表自测：测试结束后 vm_free 会把 walk()/l2_for() 为
     * 0x50000000 新建的中间页表一并回收，不在内核页表里留下空表。 */
    uint64_t pt = vm_new();

    if (pt == 0) {
        kprintf("[vm] selftest: no free page table\n");
        return;
    }
    vm_switch(pt);
    vm_selftest_on(pt);
    vm_switch(vm_kernel_pagetable());
    vm_free(pt);

    if (pmm_free_page_count() != free_before) {
        kprintf("[vm] selftest: page accounting mismatch\n");
    }
}
