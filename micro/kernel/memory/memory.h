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

/* arch/riscv/types.h 也定义了同名宏；允许两个模块出现在同一编译单元。 */
#ifndef PAGE_SHIFT
#define PAGE_SHIFT 12
#endif
#ifndef PAGE_SIZE
#define PAGE_SIZE ((size_t)1 << PAGE_SHIFT)
#endif
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
 * 目标区间必须尚未映射，否则返回 MEM_EEXIST；每个 as 只允许安装一次。
 * 这些页表页由内核永久拥有，不参与 as 的引用计数，也不在 as_destroy 中释放；
 * 调用者必须保证 kernel_root 的生命周期长于 as。
 */
int as_share_kernel(struct as *as, pfn_t kernel_root, int first_index);

/*
 * 为内核建立恒等映射（VA == PA），不参与 frame 引用计数。
 *
 * 两个用途的物理页性质不同：
 *   - 内核镜像由链接脚本/固件提供，不是 frame_init 的对象；
 *   - 直接映射窗口是 frame_init 管理的帧池，但池中的帧由
 *     frame_alloc()/持有者负责引用。
 * 二者对这个 AS 都是“非拥有映射”：PTE 用 RSW 位标记，as_unmap() 和
 * AS 销毁时跳过 frame_unref。因此不能走 as_map() 的 refcount 检查。
 *
 * 必须在目标 AS 第一次作为 satp 使用之前调用；重复映射返回 MEM_EEXIST。
 */
int as_map_kernel_range(struct as *as, paddr_t base, size_t size);

/*
 * rights 只能由 MEM_R/W/X/U/KERNEL 组成，且至少包含 R/W/X 之一。
 * MEM_W 自动带 MEM_R；不带 MEM_KERNEL 的映射设置 PTE_U。
 */
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
