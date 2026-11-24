#include "mm/trap_vm.h"

#include "lib/kprintf.h"
#include "lib/string.h"
#include "memory/memory.h"
#include "mm/vm.h"
#include "proc/proc.h"
#include "proc/user.h"

#ifndef PLATFORM_ZYNQ

/* 覆盖所有进程的堆页都换出的最坏情况：每进程最多
 * (USER_HEAP_LIMIT - USER_HEAP_BASE) / PAGE_SIZE 页。 */
#define HEAP_PAGES ((USER_HEAP_LIMIT - USER_HEAP_BASE) / PAGE_SIZE)
#define SWAP_SLOTS (MAX_PROCS * HEAP_PAGES)

struct swap_slot {
    int used;
    int pid;
    uint64_t va;
    uint8_t data[PAGE_SIZE];
};

static struct swap_slot swap_area[SWAP_SLOTS];
static uint64_t evict_cursor = USER_HEAP_BASE;

int page_fault_count;
int evict_count;

struct proc *proc_at(int index);

static struct swap_slot *swap_find(int pid, uint64_t va)
{
    for (uint64_t i = 0; i < SWAP_SLOTS; i++) {
        if (swap_area[i].used != 0 && swap_area[i].pid == pid && swap_area[i].va == va) {
            return &swap_area[i];
        }
    }
    return 0;
}

static struct swap_slot *swap_alloc(int pid, uint64_t va)
{
    for (uint64_t i = 0; i < SWAP_SLOTS; i++) {
        if (swap_area[i].used == 0) {
            swap_area[i].used = 1;
            swap_area[i].pid = pid;
            swap_area[i].va = va;
            return &swap_area[i];
        }
    }
    return 0;
}

static int swap_has_free(void)
{
    for (uint64_t i = 0; i < SWAP_SLOTS; i++) {
        if (swap_area[i].used == 0) {
            return 1;
        }
    }
    return 0;
}

static int evict_proc_page(struct proc *p, uint64_t start, int skip_pid,
                           uint64_t skip_va)
{
    uint64_t heap_end;

    if (p == 0 || p->is_user == 0 || p->heap_top <= USER_HEAP_BASE) {
        return 0;
    }
    if (p->state == PROC_UNUSED || p->state == PROC_ZOMBIE) {
        return 0;
    }
    heap_end = p->heap_top;
    if (heap_end > USER_STACK_TOP - PAGE_SIZE) {
        heap_end = USER_STACK_TOP - PAGE_SIZE;
    }
    for (uint64_t va = start; va < heap_end; va += PAGE_SIZE) {
        uint64_t pa = vm_translate(p->pagetable, va);
        struct swap_slot *slot;

        if (pa == 0 || (p->pid == skip_pid && va == skip_va)) {
            continue;
        }
        slot = swap_alloc(p->pid, va);
        if (slot == 0) {
            return 0;
        }
        memcpy(slot->data, (void *)(uintptr_t)pa, PAGE_SIZE);
        vm_unmap(p->pagetable, va);
        pmm_free_page(pa);
        if (p->heap_pages > 0) {
            p->heap_pages--;
        }
        evict_count++;
        p->vm_evict_count++;
        evict_cursor = va + PAGE_SIZE;
        return 1;
    }
    return 0;
}

static int evict_scan(uint64_t start)
{
    for (int i = 0; i < MAX_PROCS; i++) {
        if (evict_proc_page(proc_at(i), start, -1, 0) != 0) {
            return 1;
        }
    }
    return 0;
}

static int evict_one(void)
{
    if (evict_scan(evict_cursor) != 0) {
        return 1;
    }
    evict_cursor = USER_HEAP_BASE;
    return evict_scan(USER_HEAP_BASE);
}

/* 常驻页上限必须由当前进程自己让出页面：全局驱逐可能换出别的进程，
 * 当前进程的 heap_pages 不会下降，上限就失效了。 */
static int evict_current_page(uint64_t skip_va)
{
    if (current == 0) {
        return 0;
    }
    if (evict_proc_page(current, evict_cursor, current->pid, skip_va) != 0) {
        return 1;
    }
    evict_cursor = USER_HEAP_BASE;
    return evict_proc_page(current, USER_HEAP_BASE, current->pid, skip_va);
}

int vm_handle_fault(uint64_t va, int write, int exec)
{
    uint64_t page;
    struct swap_slot *slot;

    (void)write;
    if (current == 0 || current->is_user == 0) {
        return 0;
    }

    page_fault_count++;
    current->vm_fault_count++;
    va &= ~(PAGE_SIZE - 1);

    /* 已映射页仍然缺页说明权限不匹配（例如从只读写的堆页取指）。
     * 此时若继续映射会覆盖并泄漏原物理页，而且重试后无限重复。 */
    if (exec != 0 || vm_translate(current->pagetable, va) != 0) {
        return 0;
    }

    slot = swap_find(current->pid, va);
    if (slot != 0) {
        /* 换入同样受常驻页上限约束：否则反复换入会把整个堆重新钉在内存里，
         * 使 USER_PAGE_LIMIT 只对 demand-zero 分配生效。
         *
         * 但预驱逐必须先确认还有空闲 swap slot：全部 slot 都在用时，目标页
         * 自己的 slot 即将释放，先换入即可腾出一个 slot；否则
         * evict_current_page() 会因 swap_alloc 失败而把一次合法换入误判成
         * 非法访问杀掉进程。 */
        if (current->heap_pages >= USER_PAGE_LIMIT && swap_has_free() != 0) {
            if (evict_current_page(0) == 0) {
                return 0;
            }
        }
        page = pmm_alloc_page();
        if (page == 0) {
            if (evict_current_page(0) != 0) {
                page = pmm_alloc_page();
            }
            if (page == 0 && evict_one() != 0) {
                page = pmm_alloc_page();
            }
        }
        if (page == 0) {
            return 0;
        }
        memcpy((void *)(uintptr_t)page, slot->data, PAGE_SIZE);
        if (vm_map(current->pagetable, va, page,
                   VM_READ | VM_WRITE | VM_USER) != 0) {
            pmm_free_page(page);
            return 0;
        }
        /* 映射成功后才释放槽位：映射失败时保留换出副本，重试仍能恢复数据。 */
        slot->used = 0;
        current->heap_pages++;
        /* 无空闲 slot 时上面跳过了预驱逐，这里可能多出 1 个常驻页；
         * 现在目标 slot 已释放，从当前进程里驱逐另一页（不要立刻换出刚
         * 换入的 va），把上限补回来。 */
        if (current->heap_pages > USER_PAGE_LIMIT) {
            (void)evict_current_page(va);
        }
        return 1;
    }

    if (va >= USER_HEAP_BASE && va < USER_STACK_TOP - PAGE_SIZE &&
        va < current->heap_top) {
        if (current->heap_pages >= USER_PAGE_LIMIT) {
            if (evict_current_page(0) == 0) {
                return 0;
            }
        }
        page = pmm_alloc_page();
        if (page == 0) {
            if (evict_current_page(0) != 0) {
                page = pmm_alloc_page();
            }
            if (page == 0 && evict_one() != 0) {
                page = pmm_alloc_page();
            }
        }
        if (page == 0) {
            return 0;
        }
        memset((void *)(uintptr_t)page, 0, PAGE_SIZE);
        if (vm_map(current->pagetable, va, page,
                   VM_READ | VM_WRITE | VM_USER) != 0) {
            pmm_free_page(page);
            return 0;
        }
        current->heap_pages++;
        if (current->heap_pages > USER_PAGE_LIMIT) {
            (void)evict_current_page(va);
        }
        return 1;
    }

    if (va == USER_STACK_TOP - PAGE_SIZE) {
        page = pmm_alloc_page();
        if (page == 0) {
            return 0;
        }
        memset((void *)(uintptr_t)page, 0, PAGE_SIZE);
        if (vm_map(current->pagetable, va, page,
                   VM_READ | VM_WRITE | VM_USER) != 0) {
            pmm_free_page(page);
            return 0;
        }
        return 1;
    }

    return 0;
}

void vm_stats_print(void)
{
    if (current != 0 &&
        (current->vm_fault_count > 0 || current->vm_evict_count > 0)) {
        kprintf("[vm] faults=%d evictions=%d\n",
                current->vm_fault_count, current->vm_evict_count);
    }
}

void vm_release_pid(int pid)
{
    for (uint64_t i = 0; i < SWAP_SLOTS; i++) {
        if (swap_area[i].used != 0 && swap_area[i].pid == pid) {
            swap_area[i].used = 0;
        }
    }
}

void vm_release_range(uint64_t pagetable, uint64_t start, uint64_t end)
{
    struct proc *p = current;

    /* 只回收完全位于 [start, end) 内的页：start 向上取整，
     * 避免释放仍有一部分在新堆顶之上的那一页。 */
    start = (start + PAGE_SIZE - 1) & ~(PAGE_SIZE - 1);
    for (uint64_t va = start; va < end; va += PAGE_SIZE) {
        uint64_t pa = vm_translate(pagetable, va);
        struct swap_slot *slot = p != 0 ? swap_find(p->pid, va) : 0;

        if (pa != 0) {
            vm_unmap(pagetable, va);
            pmm_free_page(pa);
            if (p != 0 && p->heap_pages > 0) {
                p->heap_pages--;
            }
        }
        if (slot != 0) {
            slot->used = 0;
        }
    }
}

int vm_swap_clone(int src_pid, uint64_t va, int dst_pid)
{
    uint64_t page_va = va & ~(PAGE_SIZE - 1);
    struct swap_slot *src = swap_find(src_pid, page_va);
    struct swap_slot *dst;

    if (src == 0) {
        return 0;
    }
    dst = swap_alloc(dst_pid, page_va);
    if (dst == 0) {
        return -1;
    }
    memcpy(dst->data, src->data, PAGE_SIZE);
    return 1;
}

#else

int page_fault_count;
int evict_count;

int vm_handle_fault(uint64_t va, int write, int exec)
{
    (void)va;
    (void)write;
    (void)exec;
    return 0;
}

void vm_stats_print(void)
{
}

void vm_release_pid(int pid)
{
    (void)pid;
}

void vm_release_range(uint64_t pagetable, uint64_t start, uint64_t end)
{
    (void)pagetable;
    (void)start;
    (void)end;
}

int vm_swap_clone(int src_pid, uint64_t va, int dst_pid)
{
    (void)src_pid;
    (void)va;
    (void)dst_pid;
    return 0;
}

#endif
