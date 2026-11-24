#ifndef KERNEL_ARCH_ARCH_VM_H
#define KERNEL_ARCH_ARCH_VM_H

#include <stdint.h>

void arch_vm_init(void);
uint64_t arch_vm_kernel_pagetable(void);
uint64_t arch_vm_new(void);
int arch_vm_map(uint64_t pagetable, uint64_t va, uint64_t pa, int perm);
void arch_vm_unmap(uint64_t pagetable, uint64_t va);
uint64_t arch_vm_translate(uint64_t pagetable, uint64_t va);
void arch_vm_switch(uint64_t pagetable);
void arch_vm_free(uint64_t pagetable);
int arch_vm_enabled(void);

#endif
