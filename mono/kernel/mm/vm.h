#ifndef KERNEL_MM_VM_H
#define KERNEL_MM_VM_H

#include <stdint.h>

#define VM_READ  0x1
#define VM_WRITE 0x2
#define VM_EXEC  0x4
#define VM_USER  0x8
#define VM_SECTION 0x100 /* ARM 1 MiB 段映射；RISC-V 忽略。 */

/* 建立内核页表并打开 MMU；必须在 memory_init() 之后调用。 */
void vm_init(void);

/* 内核页表（物理地址）。 */
uint64_t vm_kernel_pagetable(void);

/* 新建一个用户页表，复制内核映射。 */
uint64_t vm_new(void);

int vm_map(uint64_t pagetable, uint64_t va, uint64_t pa, int perm);
void vm_unmap(uint64_t pagetable, uint64_t va);
uint64_t vm_translate(uint64_t pagetable, uint64_t va);

/* 切换当前页表；pagetable 为 0 表示保持当前。 */
void vm_switch(uint64_t pagetable);
void vm_free(uint64_t pagetable);

int vm_enabled(void);
void vm_selftest(void);

#endif
