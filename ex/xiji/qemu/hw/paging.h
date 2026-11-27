#ifndef XIJI_HW_PAGING_H
#define XIJI_HW_PAGING_H

#include <stdint.h>

/*
 * 请求分页的最小可运行实现：
 *   - 恒等映射低 4 MiB 供内核自己运行（CR0.PG 打开后指令与栈都靠它）；
 *   - 0x00408000 起的 4 个页故意不给页表项，第一次访问触发 #PF，
 *     由 hw_paging_on_fault() 现场分配物理页并补上 PTE。
 */
struct paging_stats {
    /*
     * 这些计数由 #PF 处理程序在中断上下文里更新，而 C 侧在触发缺页之后
     * 还要读它们。若不加 volatile，编译器会把值缓存进寄存器，
     * 于是"触发了 4 次缺页"却读出 0——这正是本次实测暴露的问题。
     */
    volatile uint32_t faults;      /* 实际处理的 #PF 次数 */
    volatile uint32_t frames_used; /* 实际映射的物理页数 */
    volatile uint32_t pde_created;
    volatile uint32_t cr2_last;
    volatile uint32_t err_last;
};

void hw_paging_on_fault(uint32_t cr2, uint32_t err_code);
int hw_paging_init(void);
int hw_paging_enabled(void);

/* 触发 4 次按需缺页并校验读写一致，同时返回统计 */
int hw_paging_demand_test(struct paging_stats *out);

/* 对未映射地址的访问必须失败（返回非 0 表示确实触发了受控缺页） */
int hw_paging_protect_test(void);

#endif
