#include "proc/user.h"

#include "console.h"
#include "fs/fs.h"
#include "lib/kprintf.h"
#include "lib/string.h"
#include "memory/memory.h"
#include "mm/trap_vm.h"
#include "mm/vm.h"
#include "proc/proc.h"
#include "shared/abi/syscall.h"

/* fork 时子进程继承父进程的 fd 槽位；打开文件由 refs 共享。 */
static void proc_fd_copy(struct proc *child, struct proc *parent)
{
    for (int fd = 3; fd < 8; fd++) {
        int slot = parent->fd[fd];
        struct file *f;

        if (slot <= 0) {
            continue;
        }
        f = fs_file_at(slot - 1);
        if (f == 0 || f->used == 0) {
            continue;
        }
        fs_file_dup(f);
        child->fd[fd] = slot;
    }
}

#ifndef PLATFORM_ZYNQ

#include "arch/riscv/csr.h"

extern char _user_hello_start[];
extern char _user_hello_end[];
extern char _user_forktest_start[];
extern char _user_forktest_end[];
extern char _user_faulttest_start[];
extern char _user_faulttest_end[];
extern char _user_init_start[];
extern char _user_init_end[];
extern char _user_sh_start[];
extern char _user_sh_end[];

struct user_image {
    const char *name;
    uint8_t *start;
    uint8_t *end;
};

static struct user_image images[] = {
    {"hello", (uint8_t *)_user_hello_start, (uint8_t *)_user_hello_end},
    {"forktest", (uint8_t *)_user_forktest_start, (uint8_t *)_user_forktest_end},
    {"faulttest", (uint8_t *)_user_faulttest_start, (uint8_t *)_user_faulttest_end},
    {"init", (uint8_t *)_user_init_start, (uint8_t *)_user_init_end},
    {"sh", (uint8_t *)_user_sh_start, (uint8_t *)_user_sh_end},
};

void *proc_kstack_top(struct proc *p);

static struct user_image *find_image(const char *name)
{
    for (uint64_t i = 0; i < sizeof(images) / sizeof(images[0]); i++) {
        if (strcmp(images[i].name, name) == 0) {
            return &images[i];
        }
    }
    return 0;
}

/* RISC-V 写入指令页后必须执行 fence.i；否则真实硬件可能继续执行
 * I-cache 中的旧指令（exec/fork 复用物理页时尤其危险）。ARM 侧对应
 * 的同步在 arm_enter_user 的 ICIALLU。 */
static void user_fence_i(void)
{
    __asm__ volatile("fence.i" ::: "memory");
}

static int map_range(uint64_t pagetable, uint64_t va, const void *src, uint64_t len, int perm)
{
    const uint8_t *bytes = (const uint8_t *)src;
    uint64_t done = 0;

    while (done < len) {
        uint64_t page = pmm_alloc_page();
        uint64_t n = len - done;
        uint64_t va_page = (va + done) & ~(PAGE_SIZE - 1);
        uint64_t page_off = (va + done) & (PAGE_SIZE - 1);

        if (page == 0) {
            return -1;
        }
        memset((void *)(uintptr_t)page, 0, PAGE_SIZE);
        if (n > PAGE_SIZE - page_off) {
            n = PAGE_SIZE - page_off;
        }
        if (src != 0) {
            memcpy((void *)(uintptr_t)page + page_off, bytes + done, n);
        }
        if (vm_map(pagetable, va_page, page, perm) != 0) {
            pmm_free_page(page);
            return -1;
        }
        done += n;
    }
    user_fence_i();
    return 0;
}

static int load_image(struct proc *p, struct user_image *img)
{
    uint64_t size = (uint64_t)(img->end - img->start);
    uint64_t stack;

    if (size > USER_HEAP_BASE - USER_BASE) {
        return -1;
    }
    if (p->pagetable == 0) {
        p->pagetable = vm_new();
    }
    if (p->pagetable == 0) {
        return -1;
    }
    if (map_range(p->pagetable, USER_BASE, img->start, size,
                  VM_READ | VM_WRITE | VM_EXEC | VM_USER) != 0) {
        return -1;
    }

    stack = pmm_alloc_page();
    if (stack == 0) {
        return -1;
    }
    memset((void *)(uintptr_t)stack, 0, PAGE_SIZE);
    if (vm_map(p->pagetable, USER_STACK_TOP - PAGE_SIZE, stack,
               VM_READ | VM_WRITE | VM_USER) != 0) {
        pmm_free_page(stack);
        return -1;
    }

    p->heap_top = USER_HEAP_BASE;
    return 0;
}

static void build_initial_frame(struct proc *p)
{
    /* 初始 trap frame 放在内核栈顶下方，与 trap_entry 的布局一致；
     * forkret 是汇编叶子函数，用 sp - TRAP_FRAME_SIZE 定位它，
     * 因此第一次调度时 sp 必须给内核栈顶。 */
    p->tf = (struct trap_frame *)((uint8_t *)proc_kstack_top(p) - TRAP_FRAME_SIZE);
    memset(p->tf, 0, TRAP_FRAME_SIZE);
    p->tf->x[2] = USER_STACK_TOP;
    p->tf->sepc = USER_BASE;
    p->tf->sstatus = SSTATUS_SPIE | SSTATUS_SUM;
    p->ctx.ra = (uint64_t)(uintptr_t)forkret;
    p->ctx.sp = (uint64_t)(uintptr_t)proc_kstack_top(p);
    p->state = PROC_READY;
}

static struct proc *user_create(const char *name, const char *image)
{
    struct user_image *img = find_image(image);
    struct proc *p;

    if (img == 0) {
        kprintf("[user] cannot create %s: unknown image\n", name);
        return 0;
    }
    p = proc_alloc();
    if (p == 0) {
        kprintf("[user] no free proc slot for %s\n", name);
        return 0;
    }
    strncpy(p->name, name, sizeof(p->name) - 1);
    p->is_user = 1;
    p->pagetable = 0;
    if (load_image(p, img) != 0) {
        kprintf("[user] cannot load %s\n", name);
        uvm_destroy(p->pagetable, USER_HEAP_BASE);
        proc_free(p);
        return 0;
    }
    build_initial_frame(p);
    kprintf("[user] %s pid=%d code=%d bytes at 0x40000000 pt=",
            name, p->pid, (int)(img->end - img->start));
    console_write_hex(p->pagetable);
    console_write(" code_pa=");
    console_write_hex(vm_translate(p->pagetable, USER_BASE));
    console_write("\n");
    return p;
}

/* forkret 由 arch/riscv/trap.S 提供：它不起栈帧，直接从内核栈顶
 * 下方的初始 trap frame 返回用户态。 */

void user_hello_init(void)
{
    user_create("hello", "hello");
}

void user_fork_init(void)
{
    user_create("forktest", "forktest");
}

void user_fault_init(void)
{
    user_create("faulttest", "faulttest");
}

void user_init_init(void)
{
    user_create("init", "init");
}

void uvm_destroy(uint64_t pagetable, uint64_t heap_top)
{
    if (pagetable == 0 || pagetable == vm_kernel_pagetable()) {
        return;
    }
    for (uint64_t va = USER_BASE; va < heap_top; va += PAGE_SIZE) {
        uint64_t pa = vm_translate(pagetable, va);
        if (pa != 0) {
            vm_unmap(pagetable, va);
            pmm_free_page(pa);
        }
    }
    for (uint64_t va = USER_STACK_TOP - PAGE_SIZE; va < USER_STACK_TOP; va += PAGE_SIZE) {
        uint64_t pa = vm_translate(pagetable, va);
        if (pa != 0) {
            vm_unmap(pagetable, va);
            pmm_free_page(pa);
        }
    }
    vm_free(pagetable);
}

static int copy_user_range(uint64_t src_pt, uint64_t dst_pt, uint64_t start,
                           uint64_t end, int perm, int src_pid, int dst_pid)
{
    for (uint64_t va = start; va < end; va += PAGE_SIZE) {
        uint64_t src = vm_translate(src_pt, va);
        uint64_t page;

        if (src == 0) {
            int cloned = vm_swap_clone(src_pid, va, dst_pid);

            if (cloned < 0) {
                /* swap 区已满：不能静默丢掉父进程换出的数据。 */
                return -1;
            }
            /* cloned > 0：子进程也持有换出副本；0：父进程从未访问过该页。
             * 两种情况都不分配常驻页，因此不会突破 USER_PAGE_LIMIT。 */
            continue;
        }
        page = pmm_alloc_page();
        if (page == 0) {
            return -1;
        }
        memcpy((void *)(uintptr_t)page, (void *)(uintptr_t)src, PAGE_SIZE);
        if (vm_map(dst_pt, va, page, perm) != 0) {
            pmm_free_page(page);
            return -1;
        }
    }
    /* 复制用户代码页后同步取指流，避免子进程执行到旧 I-cache 行。 */
    user_fence_i();
    return 0;
}

static int count_mapped_pages(uint64_t pagetable, uint64_t start, uint64_t end)
{
    int count = 0;

    for (uint64_t va = start; va < end; va += PAGE_SIZE) {
        if (vm_translate(pagetable, va) != 0) {
            count++;
        }
    }
    return count;
}

struct proc *proc_fork(void)
{
    struct proc *parent = current;
    struct proc *child = proc_alloc();
    uint64_t sample_va = USER_BASE;

    if (child == 0) {
        return 0;
    }
    strncpy(child->name, "child", sizeof(child->name) - 1);
    child->is_user = 1;
    child->parent = parent;
    child->pagetable = vm_new();
    child->heap_top = parent->heap_top;
    if (child->pagetable == 0) {
        proc_free(child);
        return 0;
    }

    /* 代码段可执行；堆和栈保持 request-paging 时的读写权限，不能因为
     * fork 复制就额外获得执行权限。 */
    if (copy_user_range(parent->pagetable, child->pagetable, USER_BASE, USER_HEAP_BASE,
                        VM_READ | VM_WRITE | VM_EXEC | VM_USER,
                        parent->pid, child->pid) != 0 ||
        copy_user_range(parent->pagetable, child->pagetable, USER_HEAP_BASE, parent->heap_top,
                        VM_READ | VM_WRITE | VM_USER,
                        parent->pid, child->pid) != 0 ||
        copy_user_range(parent->pagetable, child->pagetable, USER_STACK_TOP - PAGE_SIZE,
                        USER_STACK_TOP, VM_READ | VM_WRITE | VM_USER,
                        parent->pid, child->pid) != 0) {
        vm_release_pid(child->pid);
        uvm_destroy(child->pagetable, child->heap_top);
        proc_free(child);
        return 0;
    }

    child->tf = (struct trap_frame *)((uint8_t *)proc_kstack_top(child) - TRAP_FRAME_SIZE);
    memcpy(child->tf, parent->tf, TRAP_FRAME_SIZE);
    child->tf->x[10] = 0;
    /* 换出页以独立 swap 副本交给子进程，只有父进程的常驻页被复制成
     * 子进程常驻页；正常情况下计数不会超过 USER_PAGE_LIMIT，这里保留
     * 防御性检查，避免未来改动重新引入 fork 绕过上限的问题。 */
    child->heap_pages = count_mapped_pages(child->pagetable, USER_HEAP_BASE, child->heap_top);
    if (child->heap_pages > USER_PAGE_LIMIT) {
        vm_release_pid(child->pid);
        uvm_destroy(child->pagetable, child->heap_top);
        proc_free(child);
        return 0;
    }
    child->ctx.ra = (uint64_t)(uintptr_t)forkret;
    child->ctx.sp = (uint64_t)(uintptr_t)proc_kstack_top(child);
    child->state = PROC_READY;

    if (parent->heap_top > USER_HEAP_BASE) {
        sample_va = USER_HEAP_BASE;
    }
    kprintf("[vm] fork parent pid=%d child pid=%d va=", parent->pid, child->pid);
    console_write_hex(sample_va);
    console_write(" pa=");
    console_write_hex(vm_translate(parent->pagetable, sample_va));
    console_write("/");
    console_write_hex(vm_translate(child->pagetable, sample_va));
    console_write("\n");
    proc_fd_copy(child, parent);
    return child;
}

int user_exec_current(const char *name)
{
    struct user_image *img = find_image(name);
    uint64_t pt;
    uint64_t old_pt = current->pagetable;
    uint64_t old_heap = current->heap_top;

    if (img == 0) {
        return OS_ENOENT;
    }
    pt = vm_new();
    if (pt == 0) {
        return OS_ENOMEM;
    }

    struct proc tmp;
    memset(&tmp, 0, sizeof(tmp));
    tmp.pagetable = pt;
    if (load_image(&tmp, img) != 0) {
        uvm_destroy(pt, USER_HEAP_BASE);
        return OS_ENOMEM;
    }

    current->pagetable = pt;
    current->heap_top = USER_HEAP_BASE;
    current->heap_pages = 0;
    current->vm_fault_count = 0;
    current->vm_evict_count = 0;
    strncpy(current->name, name, sizeof(current->name) - 1);
    vm_switch(pt);
    vm_release_pid(current->pid);
    uvm_destroy(old_pt, old_heap);

    /* exec 后新程序不应继承旧程序的通用寄存器；trap_return 会恢复整帧。 */
    memset(current->tf, 0, TRAP_FRAME_SIZE);
    current->tf->sepc = USER_BASE;
    current->tf->x[2] = USER_STACK_TOP;
    current->tf->x[10] = 0;
    current->tf->sstatus = SSTATUS_SPIE | SSTATUS_SUM;

    (void)old_pt;
    kprintf("[user] exec %s pid=%d\n", name, current->pid);
    return 0;
}

#else /* PLATFORM_ZYNQ */

#define ARM_SECTION_SIZE 0x100000UL
#define ARM_SECTION_PAGES 256UL

void *proc_kstack_base(struct proc *p);

extern char _user_hello_start[];
extern char _user_hello_end[];
extern char _user_forktest_start[];
extern char _user_forktest_end[];
extern char _user_init_start[];
extern char _user_init_end[];
extern char _user_sh_start[];
extern char _user_sh_end[];

struct user_image {
    const char *name;
    uint8_t *start;
    uint8_t *end;
};

static struct user_image images[] = {
    {"hello", (uint8_t *)_user_hello_start, (uint8_t *)_user_hello_end},
    {"forktest", (uint8_t *)_user_forktest_start, (uint8_t *)_user_forktest_end},
    {"init", (uint8_t *)_user_init_start, (uint8_t *)_user_init_end},
    {"sh", (uint8_t *)_user_sh_start, (uint8_t *)_user_sh_end},
};

void *proc_kstack_top(struct proc *p);

static struct user_image *find_image(const char *name)
{
    for (uint64_t i = 0; i < sizeof(images) / sizeof(images[0]); i++) {
        if (strcmp(images[i].name, name) == 0) {
            return &images[i];
        }
    }
    return 0;
}

void uvm_destroy(uint64_t pagetable, uint64_t heap_top)
{
    uint64_t code;
    uint64_t stack;

    (void)heap_top;
    if (pagetable == 0 || pagetable == vm_kernel_pagetable()) {
        return;
    }
    code = vm_translate(pagetable, USER_BASE);
    stack = vm_translate(pagetable, USER_STACK_TOP - ARM_SECTION_SIZE);
    if (code != 0) {
        pmm_free_pages(code, ARM_SECTION_PAGES);
    }
    if (stack != 0) {
        pmm_free_pages(stack, ARM_SECTION_PAGES);
    }
    vm_free(pagetable);
}

static int map_image(uint64_t pagetable, struct user_image *img)
{
    uint64_t size = (uint64_t)(img->end - img->start);
    uint64_t code_pa;
    uint64_t stack_pa;

    /* 堆从 USER_HEAP_BASE 开始生长，镜像不能覆盖堆区。 */
    if (size > USER_HEAP_BASE - USER_BASE) {
        return -1;
    }
    code_pa = pmm_alloc_pages_aligned(ARM_SECTION_PAGES, ARM_SECTION_PAGES);
    stack_pa = pmm_alloc_pages_aligned(ARM_SECTION_PAGES, ARM_SECTION_PAGES);
    if (code_pa == 0 || stack_pa == 0) {
        if (code_pa != 0) {
            pmm_free_pages(code_pa, ARM_SECTION_PAGES);
        }
        if (stack_pa != 0) {
            pmm_free_pages(stack_pa, ARM_SECTION_PAGES);
        }
        return -1;
    }
    memset((void *)(uintptr_t)code_pa, 0, ARM_SECTION_SIZE);
    memcpy((void *)(uintptr_t)code_pa, img->start, size);
    memset((void *)(uintptr_t)stack_pa, 0, ARM_SECTION_SIZE);

    if (vm_map(pagetable, USER_BASE, code_pa,
               VM_READ | VM_WRITE | VM_EXEC | VM_USER | VM_SECTION) != 0) {
        pmm_free_pages(code_pa, ARM_SECTION_PAGES);
        pmm_free_pages(stack_pa, ARM_SECTION_PAGES);
        return -1;
    }
    if (vm_map(pagetable, USER_STACK_TOP - ARM_SECTION_SIZE, stack_pa,
               VM_READ | VM_WRITE | VM_USER | VM_SECTION) != 0) {
        vm_unmap(pagetable, USER_BASE);
        pmm_free_pages(code_pa, ARM_SECTION_PAGES);
        pmm_free_pages(stack_pa, ARM_SECTION_PAGES);
        return -1;
    }
    return 0;
}

void arm_enter_user(struct trap_frame *tf);

void forkret(void)
{
    arm_enter_user(current->tf);
}

void arm_prepare_syscall(uint32_t pc, uint32_t spsr, uint32_t sp,
                         const uint32_t *regs, uint32_t user_lr)
{
    for (int i = 0; i < 13; i++) {
        current->tf->r[i] = regs[i];
    }
    current->tf->sp = sp;
    /* User LR 是每个进程独立的上下文，不能随其他用户进程的 bl/bx 被覆盖。 */
    current->tf->lr = user_lr;
    current->tf->pc = pc;
    current->tf->cpsr = spsr;
}

static void build_user_frame(struct proc *p)
{
    /* 放在内核栈底部，避免 forkret/C 调用的栈增长覆盖 trap frame。 */
    p->tf = (struct trap_frame *)proc_kstack_base(p);
    memset(p->tf, 0, sizeof(*p->tf));
    p->tf->pc = USER_BASE;
    p->tf->sp = USER_STACK_TOP;
    p->tf->cpsr = 0x10;
    p->ctx.lr = (uint32_t)(uintptr_t)forkret;
    p->ctx.sp = (uint32_t)(uintptr_t)proc_kstack_top(p);
    p->state = PROC_READY;
}

static struct proc *user_create(const char *name, const char *image)
{
    struct user_image *img = find_image(image);
    struct proc *p;

    if (img == 0) {
        return 0;
    }
    p = proc_alloc();
    if (p == 0) {
        kprintf("[user] no free proc slot for %s\n", name);
        return 0;
    }
    strncpy(p->name, name, sizeof(p->name) - 1);
    p->is_user = 1;
    p->pagetable = vm_new();
    if (p->pagetable == 0) {
        proc_free(p);
        return 0;
    }
    if (map_image(p->pagetable, img) != 0) {
        uvm_destroy(p->pagetable, USER_HEAP_BASE);
        proc_free(p);
        return 0;
    }
    p->heap_top = USER_HEAP_BASE;
    build_user_frame(p);
    kprintf("[user] ARM %s pid=%d code=%d bytes at 0x40000000\n",
            name, p->pid, (int)(img->end - img->start));
    return p;
}

void user_hello_init(void)
{
    user_create("hello", "hello");
}

void user_fork_init(void)
{
    user_create("forktest", "forktest");
}

void user_fault_init(void)
{
}

void user_init_init(void)
{
    user_create("init", "init");
}

struct proc *proc_fork(void)
{
    struct proc *parent = current;
    struct proc *child = proc_alloc();
    uint64_t code_src;
    uint64_t stack_src;
    uint64_t code_dst;
    uint64_t stack_dst;

    if (child == 0) {
        return 0;
    }
    strncpy(child->name, "child", sizeof(child->name) - 1);
    child->is_user = 1;
    child->parent = parent;
    child->pagetable = vm_new();
    if (child->pagetable == 0) {
        proc_free(child);
        return 0;
    }

    code_src = vm_translate(parent->pagetable, USER_BASE);
    stack_src = vm_translate(parent->pagetable, USER_STACK_TOP - ARM_SECTION_SIZE);
    code_dst = pmm_alloc_pages_aligned(ARM_SECTION_PAGES, ARM_SECTION_PAGES);
    stack_dst = pmm_alloc_pages_aligned(ARM_SECTION_PAGES, ARM_SECTION_PAGES);
    if (code_src == 0 || stack_src == 0 || code_dst == 0 || stack_dst == 0) {
        if (code_dst != 0) {
            pmm_free_pages(code_dst, ARM_SECTION_PAGES);
        }
        if (stack_dst != 0) {
            pmm_free_pages(stack_dst, ARM_SECTION_PAGES);
        }
        uvm_destroy(child->pagetable, USER_HEAP_BASE);
        proc_free(child);
        return 0;
    }

    memcpy((void *)(uintptr_t)code_dst, (void *)(uintptr_t)code_src, ARM_SECTION_SIZE);
    memcpy((void *)(uintptr_t)stack_dst, (void *)(uintptr_t)stack_src, ARM_SECTION_SIZE);
    if (vm_map(child->pagetable, USER_BASE, code_dst,
               VM_READ | VM_WRITE | VM_EXEC | VM_USER | VM_SECTION) != 0 ||
        vm_map(child->pagetable, USER_STACK_TOP - ARM_SECTION_SIZE, stack_dst,
               VM_READ | VM_WRITE | VM_USER | VM_SECTION) != 0) {
        /* 先解除可能已成功的段映射，避免 uvm_destroy 再次释放同一物理页。 */
        vm_unmap(child->pagetable, USER_BASE);
        vm_unmap(child->pagetable, USER_STACK_TOP - ARM_SECTION_SIZE);
        pmm_free_pages(code_dst, ARM_SECTION_PAGES);
        pmm_free_pages(stack_dst, ARM_SECTION_PAGES);
        uvm_destroy(child->pagetable, USER_HEAP_BASE);
        proc_free(child);
        return 0;
    }

    child->heap_top = parent->heap_top;
    child->tf = (struct trap_frame *)proc_kstack_base(child);
    memcpy(child->tf, parent->tf, sizeof(*child->tf));
    child->tf->r[0] = 0;
    child->ctx.lr = (uint32_t)(uintptr_t)forkret;
    child->ctx.sp = (uint32_t)(uintptr_t)proc_kstack_top(child);
    child->state = PROC_READY;

    kprintf("[vm] ARM fork parent pid=%d child pid=%d\n", parent->pid, child->pid);
    proc_fd_copy(child, parent);
    return child;
}

int user_exec_current(const char *name)
{
    struct user_image *img = find_image(name);
    uint64_t pt;
    uint64_t old_pt = current->pagetable;
    uint64_t old_heap = current->heap_top;

    if (img == 0) {
        return OS_ENOENT;
    }
    pt = vm_new();
    if (pt == 0) {
        return OS_ENOMEM;
    }
    if (map_image(pt, img) != 0) {
        uvm_destroy(pt, USER_HEAP_BASE);
        return OS_ENOMEM;
    }

    current->pagetable = pt;
    current->heap_top = USER_HEAP_BASE;
    current->heap_pages = 0;
    current->vm_fault_count = 0;
    current->vm_evict_count = 0;
    strncpy(current->name, name, sizeof(current->name) - 1);
    vm_switch(pt);
    uvm_destroy(old_pt, old_heap);

    /* 丢弃当前 SVC 帧，直接从新镜像入口返回用户态。 */
    memset(current->tf->r, 0, sizeof(current->tf->r));
    current->tf->pc = USER_BASE;
    current->tf->sp = USER_STACK_TOP;
    current->tf->lr = 0;
    current->tf->cpsr = 0x10;
    __asm__ volatile("mov sp, %0" ::"r"((uint32_t)(uintptr_t)proc_kstack_top(current)));
    arm_enter_user(current->tf);
    return 0;
}

#endif
