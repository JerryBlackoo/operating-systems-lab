#include "paging.h"
#include "ports.h"
#include <stdint.h>

#define PF_VIRT 0x00408000u
#define FRAME_POOL 0x00500000u
#define PD_PHYS 0x0000a000u
#define PT_PHYS 0x0000b000u
#define FAULT_TAB_PHYS 0x0000c000u

static uint32_t *const page_dir = (uint32_t *)(uintptr_t)PD_PHYS;
static uint32_t *const page_tab = (uint32_t *)(uintptr_t)PT_PHYS;
static volatile uint32_t pf_faults;
static volatile uint32_t pf_frames;

static void invlpg(uintptr_t va)
{
    __asm__ volatile("invlpg (%0)" : : "r"(va) : "memory");
}

int hw_paging_init(void)
{
    for (int i = 0; i < 1024; i++) {
        page_dir[i] = 0;
        page_tab[i] = 0;
    }
    page_dir[0] = PT_PHYS | 0x3;
    for (int i = 0; i < 1024; i++)
        page_tab[i] = (uint32_t)(i * 4096u) | 0x3;

    pf_faults = 0;
    pf_frames = 0;

    __asm__ volatile("movl %0, %%cr3" : : "r"(page_dir) : "memory");
    uint32_t cr0;
    __asm__ volatile("movl %%cr0, %0" : "=r"(cr0));
    cr0 |= 0x80000000u;
    __asm__ volatile("movl %0, %%cr0" : : "r"(cr0) : "memory");
    return 0;
}

void hw_paging_on_fault(uint32_t cr2, uint32_t err_code)
{
    (void)err_code;
    if (cr2 < PF_VIRT || cr2 >= PF_VIRT + 4096u * 4u)
        return;

    unsigned idx = (cr2 - PF_VIRT) >> 12;
    if (idx >= 4)
        return;

    uint32_t frame = FRAME_POOL + idx * 4096u;
    uint32_t pde_idx = cr2 >> 22;
    if (!(page_dir[pde_idx] & 1))
        page_dir[pde_idx] = FAULT_TAB_PHYS | 0x3;

    uint32_t *tab = (uint32_t *)(uintptr_t)(page_dir[pde_idx] & 0xfffff000u);
    unsigned pte = (cr2 >> 12) & 0x3ff;
    if (!(tab[pte] & 1)) {
        tab[pte] = frame | 0x3;
        invlpg(cr2);
        pf_frames++;
    }
    pf_faults++;
}

int hw_paging_demand_test(struct paging_stats *out)
{
    volatile uint8_t *p = (volatile uint8_t *)(uintptr_t)PF_VIRT;
    for (unsigned i = 0; i < 4; i++) {
        p[i * 4096u] = (uint8_t)(0xA0 + i);
        (void)p[i * 4096u];
    }
    for (unsigned i = 0; i < 4; i++) {
        if (p[i * 4096u] != (uint8_t)(0xA0 + i))
            return -1;
    }
    if (out) {
        out->faults = pf_faults;
        out->frames_used = pf_frames;
    }
    return pf_faults >= 4 && pf_frames >= 4 ? 0 : -2;
}