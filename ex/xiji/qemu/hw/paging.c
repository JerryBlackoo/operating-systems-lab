#include "paging.h"
#include "../common/xiji_io.h"
#include <stdint.h>
/*
 * 物理内存布局（低 4 MiB 恒等映射）
 *   0x020000  内核镜像（.text/.rodata/.data/.bss）
 *   0x040000  页目录（4 KiB）
 *   0x041000  内核线性映射页表（4 KiB，覆盖 0..4 MiB）
 *   0x042000  按需映射页表（4 KiB，覆盖 4..8 MiB）
 *   0x050000  按需分配的物理帧池（每帧 4 KiB）
 *   0x09f000  引导栈顶
 *
 * 内核不能放在 0x1000：40 KiB 的镜像会一路写到 0xAB00，
 * 把 0x7c00 处的 stage1/stage2 和引导栈覆盖掉（读盘读到一半就执行垃圾）。
 * 放到 64 KiB 对齐的 0x20000 既避开低端引导区，也满足 INT 13h 不得跨 64 KiB 边界的要求。
 */

#define PD_PHYS 0x00040000u
#define PT_KERNEL_PHYS 0x00041000u
#define PT_DEMAND_PHYS 0x00042000u
#define FRAME_POOL 0x00050000u

#define PF_VIRT 0x00408000u /* 4 MiB + 32 KiB，落在 DEMAND 页表覆盖区 */
#define PF_PAGES 4u
#define PF_PDE (PF_VIRT >> 22)
#define PT_MAPPED_PAGES 1024u

#define PTE_PRESENT 0x1u
#define PTE_WRITE 0x2u
#define PTE_USER 0x4u

static uint32_t *const page_dir = (uint32_t *)(uintptr_t)PD_PHYS;
static uint32_t *const pt_kernel = (uint32_t *)(uintptr_t)PT_KERNEL_PHYS;
static uint32_t *const pt_demand = (uint32_t *)(uintptr_t)PT_DEMAND_PHYS;

static struct paging_stats stats;
static volatile uint32_t next_frame;
static int paging_on;

static void invlpg(uintptr_t va)
{
    __asm__ volatile("invlpg (%0)" : : "r"(va) : "memory");
}

int hw_paging_enabled(void)
{
    uint32_t cr0;

    __asm__ volatile("movl %%cr0, %0" : "=r"(cr0));
    return (cr0 & 0x80000000u) != 0;
}

int hw_paging_init(void)
{
    /* 页目录：低 4 MiB 用内核页表，4..8 MiB 用按需页表 */
    for (unsigned i = 0; i < 1024; i++)
        page_dir[i] = 0;

    for (unsigned i = 0; i < PT_MAPPED_PAGES; i++)
        pt_kernel[i] = (i * 4096u) | PTE_PRESENT | PTE_WRITE; /* supervisor 可读写 */
    page_dir[0] = PT_KERNEL_PHYS | PTE_PRESENT | PTE_WRITE;

    /* 按需页表整表清空：第一次访问必然缺页 */
    for (unsigned i = 0; i < PT_MAPPED_PAGES; i++)
        pt_demand[i] = 0;
    page_dir[PF_PDE] = PT_DEMAND_PHYS | PTE_PRESENT | PTE_WRITE;

    stats = (struct paging_stats){ 0, 0, 0, 0, 0 };
    next_frame = FRAME_POOL;
    paging_on = 0;

    __asm__ volatile("movl %0, %%cr3" : : "r"((uint32_t)PD_PHYS) : "memory");

    uint32_t cr0;
    __asm__ volatile("movl %%cr0, %0" : "=r"(cr0));
    cr0 |= 0x80000000u; /* CR0.PG */
    cr0 |= 0x00010000u; /* CR0.WP：内核写只读页也要触发 #PF */
    __asm__ volatile("movl %0, %%cr0" : : "r"(cr0) : "memory");

    paging_on = 1;
    return hw_paging_enabled() ? 0 : -1;
}

void hw_paging_on_fault(uint32_t cr2, uint32_t err_code)
{
    unsigned idx;
    unsigned pte_idx;
    uint32_t frame;
    uint32_t *tab;

    stats.cr2_last = cr2;
    stats.err_last = err_code;

    if (cr2 < PF_VIRT || cr2 >= PF_VIRT + PF_PAGES * 4096u) {
        /* 真实内核会在这里杀掉进程；本实验只报告并不修复 */
        xiji_puts("hw: unexpected #PF at 0x");
        xiji_puthex32(cr2);
        xiji_putc('\n');
        return;
    }

    idx = (cr2 - PF_VIRT) >> 12;
    if (idx >= PF_PAGES)
        return;

    tab = pt_demand;
    pte_idx = (cr2 >> 12) & 0x3ffu;

    /* 写保护缺页（P=1 且 W=1）：页已在、只是只读，就地升级为可写 */
    if ((err_code & 0x3u) == 0x3u) {
        tab[pte_idx] |= PTE_WRITE;
        invlpg(cr2);
        stats.faults++;
        return;
    }

    /* 缺页（P=0）：分配一个新帧并补 PTE */
    if (next_frame >= FRAME_POOL + PF_PAGES * 4096u)
        return; /* 帧池耗尽：不再分配，保留缺页以便观测 */

    if (!(tab[pte_idx] & PTE_PRESENT)) {
        frame = next_frame;
        next_frame += 4096u;
        tab[pte_idx] = frame | PTE_PRESENT | PTE_WRITE | PTE_USER;
        stats.frames_used++;
    }
    invlpg(cr2);
    stats.faults++;
}

int hw_paging_demand_test(struct paging_stats *out)
{
    volatile uint8_t *p = (volatile uint8_t *)(uintptr_t)PF_VIRT;
    uint32_t faults_before = stats.faults;

    /* 每一页都写一次：必然触发一次缺页（PTE 全 0） */
    for (unsigned i = 0; i < PF_PAGES; i++)
        p[i * 4096u] = (uint8_t)(0xA0u + i);

    /* 再读回来：不应再缺页 */
    for (unsigned i = 0; i < PF_PAGES; i++) {
        if (p[i * 4096u] != (uint8_t)(0xA0u + i))
            return -1;
    }
    /* 页内其它偏移也要可写，且不能跨页污染 */
    for (unsigned i = 0; i < PF_PAGES; i++) {
        p[i * 4096u + 100] = (uint8_t)(i + 1);
        if (p[i * 4096u + 100] != (uint8_t)(i + 1))
            return -1;
    }

    if (out)
        *out = stats;
    if (stats.faults - faults_before != PF_PAGES)
        return -2; /* 恰好每页一次缺页；多了说明映射没生效，少了说明本来就在 */
    if (stats.frames_used != PF_PAGES)
        return -3;
    return 0;
}

/*
 * 写保护缺页：把已映射的按需页改成只读，再写它。
 * 期望 #PF 的 error code 同时带 P（页存在）与 W（写访问）两位，
 * 由 handler 就地升级为可写，写入随即成功。
 * 这验证的是"页保护真的生效"，而不是只验证了缺页路径。
 */
int hw_paging_protect_test(void)
{
    volatile uint8_t *p = (volatile uint8_t *)(uintptr_t)PF_VIRT;
    unsigned pte_idx = (PF_VIRT >> 12) & 0x3ffu;
    uint8_t before = p[0];
    uint32_t faults_before = stats.faults;

    pt_demand[pte_idx] &= ~PTE_WRITE;
    invlpg(PF_VIRT);
    p[0] = (uint8_t)(before ^ 0x5a);

    if (p[0] != (uint8_t)(before ^ 0x5a))
        return -1;
    if (stats.faults != faults_before + 1)
        return -2;
    if ((stats.err_last & 0x3u) != 0x3u)
        return -3; /* P=1, W=1：确实是写保护缺页 */
    return 0;
}
