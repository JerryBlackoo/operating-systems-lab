#include "arch/arch_vm.h"

#include "console.h"
#include "lib/kprintf.h"
#include "lib/string.h"
#include "memory/memory.h"
#include "mm/vm.h"

/* ARMv7-A 短描述符：L1 16 KiB（4096 项），L2 1 KiB（256 项）。 */
#define L1_ENTRIES 4096
#define L2_ENTRIES 256

#define L1_COARSE 0x1u
#define L1_SECTION 0x2u
#define L2_SMALL_PAGE 0x2u

static void clean_range(const void *start, uint32_t len);

/* L1 必须 16 KiB 对齐；用一个静态表，避免要求物理页连续。 */
static uint32_t arm_l1[L1_ENTRIES] __attribute__((aligned(16384)));
static uint32_t *kernel_l1;
static int enabled;

static uint32_t section_desc(uint32_t pa, int device, int user, int exec, int write)
{
    uint32_t desc = (pa & 0xfff00000u) | L1_SECTION;
    uint32_t ap;

    /* 3=用户 RW，2=用户 RO（特权 RW），1=特权 RW；
     * 内核只读使用 AP[1:0]=01 + AP[2]=1（0b101），0b100 是保留编码。
     * ARMv7 没有“用户只写”编码，缺少 VM_READ 时仍按可读映射。 */
    ap = user ? (write ? 3u : 2u) : 1u;
    desc |= (0u << 5);          /* domain 0 */
    desc |= (ap << 10);
    if (!user && !write) {
        desc |= (1u << 15);     /* AP[2]=1：特权只读 */
    }
    if (device) {
        desc |= (1u << 4);      /* XN */
    } else {
        desc |= (1u << 2) | (1u << 3); /* C=1, B=1 */
        if (exec == 0) {
            desc |= (1u << 4);  /* XN：不可执行段（例如用户栈） */
        }
    }
    return desc;
}

static uint32_t page_desc(uint32_t pa, int user, int exec, int write)
{
    uint32_t desc = (pa & 0xfffff000u) | L2_SMALL_PAGE;
    uint32_t ap = user ? (write ? 3u : 2u) : 1u;

    /* 页描述符的 AP[1:0] 在 bit[5:4]（与 section 的 bit[11:10] 不同），
     * 特权只读的 AP[2] 在 bit[9]。 */
    desc |= (ap & 3u) << 4;
    if (!user && !write) {
        desc |= (1u << 9);
    }
    desc |= (1u << 2) | (1u << 3); /* C=1, B=1 */
    if (exec == 0) {
        desc |= 1u;             /* XN：小页描述符的 bit0 */
    }
    return desc;
}

static uint32_t *l2_for(uint32_t *l1, uint32_t va, int alloc)
{
    uint32_t index = va >> 20;
    uint32_t entry = l1[index];

    /* 0b01 = coarse page table（1 KiB L2 表，4 KiB 小页）。 */
    if ((entry & 3u) == L1_COARSE) {
        return (uint32_t *)(entry & 0xfffffc00u);
    }
    if (!alloc || (entry & 3u) == L1_SECTION) {
        return 0;
    }
    uint32_t page = (uint32_t)pmm_alloc_page();
    if (page == 0) {
        return 0;
    }
    memset((void *)(uintptr_t)page, 0, PAGE_SIZE);
    clean_range((void *)(uintptr_t)page, PAGE_SIZE);
    l1[index] = (page & 0xfffffc00u) | L1_COARSE;
    clean_range(&l1[index], sizeof(l1[index]));
    return (uint32_t *)(uintptr_t)page;
}

static void map_kernel(uint32_t *l1)
{
    /* DDR 0x00000000..0x20000000，1 MiB 段；内核段仅特权态可访问。 */
    for (uint32_t i = 0; i < 512; i++) {
        l1[i] = section_desc(i * 0x100000u, 0, 0, 1, 1);
    }
    /* UART0 与 GIC/私有定时器，设备内存。 */
    l1[0xE0000000u >> 20] = section_desc(0xE0000000u, 1, 0, 0, 1);
    l1[0xF8F00000u >> 20] = section_desc(0xF8F00000u, 1, 0, 0, 1);
}

static void clean_range(const void *start, uint32_t len)
{
    uint32_t addr = (uint32_t)(uintptr_t)start;
    uint32_t end = addr + len;

    for (; addr < end; addr += 32) {
        __asm__ volatile("mcr p15, 0, %0, c7, c10, 1" ::"r"(addr) : "memory");
    }
    __asm__ volatile("dsb");
    __asm__ volatile("isb");
}

void arch_vm_init(void)
{
    kernel_l1 = arm_l1;
    memset(kernel_l1, 0, sizeof(arm_l1));
    map_kernel(kernel_l1);

    __asm__ volatile("mcr p15, 0, %0, c2, c0, 0" ::"r"((uint32_t)kernel_l1)); /* TTBR0 */
    __asm__ volatile("mcr p15, 0, %0, c2, c0, 2" ::"r"(0u));                 /* TTBCR */
    __asm__ volatile("mcr p15, 0, %0, c3, c0, 0" ::"r"(1u));                 /* DACR */

    /* 页表与 TTBR0 的写入必须先对 MMU 可见，再打开 SCTLR.M。 */
    __asm__ volatile("dsb");
    __asm__ volatile("isb");

    uint32_t sctlr;
    __asm__ volatile("mrc p15, 0, %0, c1, c0, 0" : "=r"(sctlr));
    /* 先只开 MMU + I-cache；D-cache 的页表一致性留到后续里程碑。 */
    sctlr |= (1u << 0) | (1u << 12);
    __asm__ volatile("mcr p15, 0, %0, c1, c0, 0" ::"r"(sctlr));
    __asm__ volatile("mcr p15, 0, %0, c8, c7, 0" ::"r"(0u)); /* TLBIALL */
    __asm__ volatile("dsb");
    __asm__ volatile("isb");
    enabled = 1;

    kprintf("[vm] ARM short-descriptor MMU enabled, L1=");
    console_write_hex((uint64_t)(uintptr_t)kernel_l1);
    kprintf("\n");
    kprintf("[vm] mapped DDR 0..0x20000000, UART 0xE0000000, GIC 0xF8F00000\n");
}

uint64_t arch_vm_kernel_pagetable(void)
{
    return (uint64_t)(uintptr_t)kernel_l1;
}

uint64_t arch_vm_new(void)
{
    uint32_t *l1 = (uint32_t *)pmm_alloc_pages_aligned(4, 4);

    if (l1 == 0) {
        return 0;
    }
    memset(l1, 0, PAGE_SIZE * 4);
    map_kernel(l1);
    return (uint64_t)(uintptr_t)l1;
}

int arch_vm_map(uint64_t pagetable, uint64_t va, uint64_t pa, int perm)
{
    uint32_t *l1 = (uint32_t *)(uintptr_t)(pagetable != 0 ? pagetable : (uint64_t)(uintptr_t)kernel_l1);
    int user = (perm & VM_USER) != 0;
    int exec = (perm & VM_EXEC) != 0;
    int write = (perm & VM_WRITE) != 0;

    if ((perm & VM_SECTION) != 0) {
        if (((uint32_t)va & 0xfffffu) != 0 || ((uint32_t)pa & 0xfffffu) != 0) {
            return -1;
        }
        l1[(uint32_t)va >> 20] = section_desc((uint32_t)pa, 0, user, exec, write);
        clean_range(&l1[(uint32_t)va >> 20], sizeof(uint32_t));
    } else {
        uint32_t *l2 = l2_for(l1, (uint32_t)va, 1);
        uint32_t index;

        if (l2 == 0) {
            return -1;
        }
        index = ((uint32_t)va >> 12) & 0xffu;
        l2[index] = page_desc((uint32_t)pa, user, exec, write);
        clean_range(&l2[index], sizeof(l2[index]));
    }
    __asm__ volatile("mcr p15, 0, %0, c8, c7, 0" ::"r"(0u));
    __asm__ volatile("dsb");
    __asm__ volatile("isb");
    return 0;
}

void arch_vm_unmap(uint64_t pagetable, uint64_t va)
{
    uint32_t *l1 = (uint32_t *)(uintptr_t)(pagetable != 0 ? pagetable : (uint64_t)(uintptr_t)kernel_l1);

    if ((l1[(uint32_t)va >> 20] & 3u) == L1_SECTION) {
        l1[(uint32_t)va >> 20] = 0;
        clean_range(&l1[(uint32_t)va >> 20], sizeof(uint32_t));
    } else {
        uint32_t *l2 = l2_for(l1, (uint32_t)va, 0);
        if (l2 != 0) {
            uint32_t index = ((uint32_t)va >> 12) & 0xffu;
            l2[index] = 0;
            clean_range(&l2[index], sizeof(l2[index]));
        }
    }
    __asm__ volatile("mcr p15, 0, %0, c8, c7, 0" ::"r"(0u));
    __asm__ volatile("dsb");
    __asm__ volatile("isb");
}

uint64_t arch_vm_translate(uint64_t pagetable, uint64_t va)
{
    uint32_t *l1 = (uint32_t *)(uintptr_t)(pagetable != 0 ? pagetable : (uint64_t)(uintptr_t)kernel_l1);
    uint32_t entry = l1[(uint32_t)va >> 20];

    if ((entry & 3u) == L1_COARSE) {
        uint32_t *l2 = (uint32_t *)(entry & 0xfffffc00u);
        uint32_t pte = l2[((uint32_t)va >> 12) & 0xffu];
        /* 小页描述符 bit[1]=1；bit[0] 是 XN，不能按完整类型位比较。 */
        if ((pte & 2u) == 0) {
            return 0;
        }
        return (uint64_t)((pte & 0xfffff000u) + ((uint32_t)va & 0xfffu));
    }
    if ((entry & 3u) == L1_SECTION) {
        return (uint64_t)((entry & 0xfff00000u) + ((uint32_t)va & 0xfffffu));
    }
    return 0;
}

void arch_vm_free(uint64_t pagetable)
{
    uint32_t *l1 = (uint32_t *)(uintptr_t)pagetable;

    if (pagetable == 0 || pagetable == (uint64_t)(uintptr_t)kernel_l1) {
        return;
    }
    for (int i = 0; i < L1_ENTRIES; i++) {
        if ((l1[i] & 3u) == L1_COARSE) {
            pmm_free_page(l1[i] & 0xfffffc00u);
        }
    }
    pmm_free_pages(pagetable, 4);
}

void arch_vm_switch(uint64_t pagetable)
{
    if (!enabled || pagetable == 0) {
        return;
    }
    __asm__ volatile("mcr p15, 0, %0, c2, c0, 0" ::"r"((uint32_t)pagetable));
    __asm__ volatile("dsb");
    __asm__ volatile("isb");
    __asm__ volatile("mcr p15, 0, %0, c8, c7, 0" ::"r"(0u));
    /* 等待 TLBIALL 完成，避免切换后的上下文继续使用旧地址空间的 TLB 项。 */
    __asm__ volatile("dsb");
    __asm__ volatile("isb");
}

int arch_vm_enabled(void)
{
    return enabled;
}
