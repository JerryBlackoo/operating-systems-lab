#ifndef KERNEL_MM_TRAP_VM_H
#define KERNEL_MM_TRAP_VM_H

#include <stdint.h>

/* 每进程驻留堆页上限；fork 后的子进程也必须满足该上限。 */
#define USER_PAGE_LIMIT 48

extern int page_fault_count;
extern int evict_count;

/* 处理一次用户页故障；返回 1 表示已处理，0 表示非法访问。 */
int vm_handle_fault(uint64_t va, int write, int exec);
void vm_stats_print(void);

/* 进程退出时释放其交换槽位（非 RISC-V 架构为空实现）。 */
void vm_release_pid(int pid);

/* 释放 [start, end) 区间内的用户页与交换槽位（非 RISC-V 架构为空实现）。 */
void vm_release_range(uint64_t pagetable, uint64_t start, uint64_t end);

/* fork 时把 src_pid 的换出页复制成 dst_pid 的换出页（不占用常驻页）。
 * 返回 1=已复制，0=源页无换出副本，-1=交换区已满。 */
int vm_swap_clone(int src_pid, uint64_t va, int dst_pid);

#endif
