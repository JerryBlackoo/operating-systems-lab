#include "arch/arch_vm.h"

#include "arch/riscv/csr.h"
#include "console.h"
#include "lib/kprintf.h"
#include "lib/string.h"
#include "memory/memory.h"
#include "mm/vm.h"

#define PTE_V (1UL << 0)
#define PTE_R (1UL << 1)
#define PTE_W (1UL << 2)
#define PTE_X (1UL << 3)
#define PTE_U (1UL << 4)
#define PTE_A (1UL << 6)
#define PTE_D (1UL << 7)

#define SATP_SV39 (8UL << 60)
#define PTE_PPN_SHIFT 10

static uint64_t kernel_root;
static int enabled;

static uint64_t *pte_table(uint64_t entry)
{
    return (uint64_t *)((entry >> PTE_PPN_SHIFT) << 12);
}

static int pte_is_leaf(uint64_t entry)
{
    return (entry & (PTE_R | PTE_W | PTE_X)) != 0;
}

static uint64_t *walk(uint64_t root, uint64_t va, int alloc)
{
    uint64_t *pt = (uint64_t *)root;

    for (int level = 2; level > 0; level--) {
        uint64_t index = (va >> (12 + 9 * level)) & 0x1ff;
        uint64_t entry = pt[index];

        if ((entry & PTE_V) != 0 && pte_is_leaf(entry)) {
            /* 中间层已有大页/1GiB 叶表项：不支持在其下再映射 4 KiB 页，
             * 返回失败而不是覆盖该叶表项（避免破坏内核恒等映射）。 */
            return 0;
        }
        if ((entry & PTE_V) == 0) {
            uint64_t page;
            if (!alloc) {
                return 0;
            }
            page = pmm_alloc_page();
            if (page == 0) {
                return 0;
            }
            memset((void *)(uintptr_t)page, 0, PAGE_SIZE);
            pt[index] = ((page >> 12) << PTE_PPN_SHIFT) | PTE_V;
            entry = pt[index];
        }
        pt = pte_table(entry);
    }
    return &pt[(va >> 12) & 0x1ff];
}

static uint64_t make_pte(uint64_t pa, int perm)
{
    uint64_t pte = ((pa >> 12) << PTE_PPN_SHIFT) | PTE_V | PTE_A | PTE_D;

    if ((perm & VM_READ) != 0) {
        pte |= PTE_R;
    }
    if ((perm & VM_WRITE) != 0) {
        pte |= PTE_W;
    }
    if ((perm & VM_EXEC) != 0) {
        pte |= PTE_X;
    }
    if ((perm & VM_USER) != 0) {
        pte |= PTE_U;
    }
    return pte;
}

static int map_leaf(uint64_t root, uint64_t va, uint64_t pa, int perm)
{
    uint64_t *pte = walk(root, va, 1);

    if (pte == 0) {
        return -1;
    }
    *pte = make_pte(pa, perm);
    /* 修改页表后必须刷新 TLB/PTE 缓存：缺页处理会把新 PTE 装进正在使用的页表，
     * 只写内存不执行 sfence.vma 时，硬件可能继续使用旧的无效翻译。 */
    __asm__ volatile("sfence.vma zero, zero" ::: "memory");
    return 0;
}

static int map_kernel(uint64_t root)
{
    uint64_t *table = (uint64_t *)(uintptr_t)root;

    /* RAM 0x80000000 起 1 GiB，用 1 GiB 叶表项恒等映射。 */
    table[2] = make_pte(0x80000000UL, VM_READ | VM_WRITE | VM_EXEC);
    /* UART0。 */
    return map_leaf(root, 0x10000000UL, 0x10000000UL, VM_READ | VM_WRITE);
}

void arch_vm_init(void)
{
    kernel_root = pmm_alloc_page();
    if (kernel_root == 0) {
        panic("[vm] cannot allocate root page table");
    }
    memset((void *)(uintptr_t)kernel_root, 0, PAGE_SIZE);
    if (map_kernel(kernel_root) != 0) {
        panic("[vm] cannot map UART into kernel page table");
    }

    uint64_t satp = SATP_SV39 | (kernel_root >> 12);
    __asm__ volatile("csrw satp, %0" ::"r"(satp));
    __asm__ volatile("sfence.vma zero, zero" ::: "memory");
    enabled = 1;

    kprintf("[vm] Sv39 enabled, root=");
    console_write_hex(kernel_root);
    kprintf("\n");
    kprintf("[vm] kernel identity map: RAM 0x80000000 +1GiB, UART 0x10000000\n");
}

uint64_t arch_vm_kernel_pagetable(void)
{
    return kernel_root;
}

uint64_t arch_vm_new(void)
{
    uint64_t root = pmm_alloc_page();

    if (root == 0) {
        return 0;
    }
    memset((void *)(uintptr_t)root, 0, PAGE_SIZE);
    if (map_kernel(root) != 0) {
        arch_vm_free(root);
        return 0;
    }
    return root;
}

int arch_vm_map(uint64_t pagetable, uint64_t va, uint64_t pa, int perm)
{
    if (pagetable == 0) {
        pagetable = kernel_root;
    }
    return map_leaf(pagetable, va, pa, perm);
}

void arch_vm_unmap(uint64_t pagetable, uint64_t va)
{
    uint64_t *pte;

    if (pagetable == 0) {
        pagetable = kernel_root;
    }
    pte = walk(pagetable, va, 0);
    if (pte != 0 && (*pte & PTE_V) != 0) {
        *pte = 0;
        __asm__ volatile("sfence.vma zero, zero" ::: "memory");
    }
}

uint64_t arch_vm_translate(uint64_t pagetable, uint64_t va)
{
    uint64_t *pt;
    uint64_t entry;
    int level;

    if (pagetable == 0) {
        pagetable = kernel_root;
    }
    pt = (uint64_t *)pagetable;

    for (level = 2; level >= 0; level--) {
        uint64_t index = (va >> (12 + 9 * level)) & 0x1ff;

        entry = pt[index];
        if ((entry & PTE_V) == 0) {
            return 0;
        }
        if (pte_is_leaf(entry)) {
            uint64_t offset = va & ((1UL << (12 + 9 * level)) - 1);
            return ((entry >> PTE_PPN_SHIFT) << 12) + offset;
        }
        if (level == 0) {
            return 0;
        }
        pt = pte_table(entry);
    }
    return 0;
}

static void free_table(uint64_t table, int level)
{
    uint64_t *pt = (uint64_t *)(uintptr_t)table;

    for (int i = 0; i < 512; i++) {
        uint64_t entry = pt[i];

        if ((entry & PTE_V) == 0 || pte_is_leaf(entry)) {
            continue;
        }
        if (level > 0) {
            free_table((entry >> PTE_PPN_SHIFT) << 12, level - 1);
        }
    }
    pmm_free_page(table);
}

void arch_vm_free(uint64_t pagetable)
{
    if (pagetable == 0 || pagetable == kernel_root) {
        return;
    }
    free_table(pagetable, 2);
}

void arch_vm_switch(uint64_t pagetable)
{
    if (!enabled || pagetable == 0) {
        return;
    }
    uint64_t satp = SATP_SV39 | (pagetable >> 12);
    __asm__ volatile("csrw satp, %0" ::"r"(satp));
    __asm__ volatile("sfence.vma zero, zero" ::: "memory");
}

int arch_vm_enabled(void)
{
    return enabled;
}
