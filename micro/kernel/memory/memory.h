#ifndef MICRO_KERNEL_MEMORY_H
#define MICRO_KERNEL_MEMORY_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include "object.h"

/*
 * 内存切面：物理帧 + 地址空间。
 *
 * 最小模型只做两件事：
 *   1. 物理帧的所有权与引用计数（Frame）；
 *   2. Sv39 页表上的 VA -> Frame 部分映射（AddressSpace）。
 *
 * 不做 VMA、COW、swap、按需分页策略；这些属于用户态 VM server。
 */

#define PAGE_SHIFT 12
#define PAGE_SIZE ((size_t)1 << PAGE_SHIFT)
#define MEMORY_MAX_FRAMES 65536
#define AS_MAX 16

typedef uint64_t paddr_t;
typedef uint64_t vaddr_t;
typedef uint64_t pfn_t;

#define PFN_NONE ((pfn_t)-1)

#define MEM_R 0x1
#define MEM_W 0x2
#define MEM_X 0x4
#define MEM_U 0x8
#define MEM_KERNEL 0x10 /* 不设置 PTE_U，只允许内核访问 */

#define MEM_OK 0
#define MEM_EINVAL (-1)
#define MEM_ENOMEM (-2)
#define MEM_EEXIST (-3)
#define MEM_EFAULT (-4)
#define MEM_EPERM (-5)

/*
 * direct_map 是物理内存的内核直接映射起点；frame_ptr(pfn) 返回该帧的内核
 * 可访问地址。QEMU RISC-V virt 上通常传 0x80000000 对应的直接映射指针。
 */
int frame_init(paddr_t phys_base, size_t size, void *direct_map);
void frame_reserve(paddr_t pa, size_t size);

pfn_t frame_alloc(void);
int frame_free(pfn_t pfn);
void frame_ref(pfn_t pfn);
void frame_unref(pfn_t pfn);
uint32_t frame_refcount(pfn_t pfn);
uint64_t frame_free_count(void);
void *frame_ptr(pfn_t pfn);
paddr_t frame_paddr(pfn_t pfn);

struct as {
    struct object obj;
    pfn_t root_pfn;
    uint16_t kernel_start; /* Sv39 顶层 [kernel_start, 512) 为共享内核映射 */
    bool active;
};

struct as *as_create(void);
void as_ref(struct as *as);
void as_unref(struct as *as);

/*
 * 把 kernel_root 顶层 [first_index, 512) 的表项复制到 as。
 * 这些页表页由内核永久拥有，不参与 as 的引用计数，也不在 as_destroy 中释放；
 * 调用者必须保证 kernel_root 的生命周期长于 as。
 */
int as_share_kernel(struct as *as, pfn_t kernel_root, int first_index);

int as_map(struct as *as, vaddr_t va, pfn_t pfn, int rights);
int as_unmap(struct as *as, vaddr_t va);
int as_translate(struct as *as, vaddr_t va, int *rights, pfn_t *pfn);

/*
 * 无 pager 的最小 fault 模型：
 *   已映射且权限满足 -> MEM_OK；未映射 -> MEM_EFAULT；权限不足 -> MEM_EPERM。
 * 将来接用户态 VM server 时，把 MEM_EFAULT 转成 IPC 即可。
 */
int as_fault(struct as *as, vaddr_t va, int access, pfn_t *pfn);

int as_copyin(struct as *as, vaddr_t uva, void *dst, size_t len);
int as_copyout(struct as *as, vaddr_t uva, const void *src, size_t len);

/* RISC-V：写 satp 并刷新 TLB；主机自测为空实现。 */
void as_activate(struct as *as);
void as_flush_tlb(void);
bool as_is_canonical(vaddr_t va);

#endif /* MICRO_KERNEL_MEMORY_H */
