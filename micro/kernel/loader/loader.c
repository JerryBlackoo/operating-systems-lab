#include "loader.h"

#include <stddef.h>
#include <stdint.h>

#include "bootinfo.h"
#include "demo.h"
#include "elf.h"
#include "memory.h"

#ifndef MICRO_LOADER_HOST_TEST
#include "cap_as.h"
#include "csr.h"
#include "hart.h"
#include "platform.h"
#include "trap.h"
#endif

#define MIRD_VERSION 1U
#define FRAME_RSV    2048
#define USER_KSTACK  8192
#define USER_LAUNCH_MAX 16

#ifndef MICRO_LOADER_HOST_TEST
struct user_launch {
    struct trap_frame *frame;
    _Alignas(16) uint8_t kstack[USER_KSTACK];
};

static struct user_launch g_launches[USER_LAUNCH_MAX];
static uint8_t g_launch_used[USER_LAUNCH_MAX];

static struct user_launch *launch_alloc(void)
{
    for (unsigned i = 0; i < USER_LAUNCH_MAX; i++) {
        if (!g_launch_used[i]) {
            g_launch_used[i] = 1;
            return &g_launches[i];
        }
    }
    return 0;
}

static void launch_free(struct user_launch *launch)
{
    unsigned index = (unsigned)(launch - g_launches);

    if (index < USER_LAUNCH_MAX) {
        g_launch_used[index] = 0;
    }
}
#endif

static const uint8_t *initrd_image;
static size_t initrd_image_size;

static char *put_str(char *p, const char *s)
{
    while (*s) {
        *p++ = *s++;
    }
    return p;
}

static char *put_hex(char *p, uint64_t v)
{
    static const char dg[] = "0123456789abcdef";

    for (int i = 60; i >= 0; i -= 4) {
        *p++ = dg[(v >> i) & 0xf];
    }
    return p;
}

static char *put_dec(char *p, uint64_t v)
{
    char t[24];
    unsigned n = 0;

    if (v == 0) {
        *p++ = '0';
        return p;
    }
    while (v) {
        t[n++] = (char)('0' + v % 10);
        v /= 10;
    }
    while (n) {
        *p++ = t[--n];
    }
    return p;
}

static int name_equals(const char *want, const char *got, size_t got_len)
{
    size_t i = 0;

    while (want[i] != 0) {
        if (i >= got_len || got[i] != want[i]) {
            return 0;
        }
        i++;
    }
    return i == got_len;
}

static uint32_t read_le32(const uint8_t *p)
{
    return (uint32_t)p[0] | (uint32_t)p[1] << 8 |
           (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24;
}

static void *local_memset(void *dst, int byte, size_t n)
{
    uint8_t *p = dst;

    for (size_t i = 0; i < n; i++) {
        p[i] = (uint8_t)byte;
    }
    return dst;
}

static void local_memcpy(void *dst, const void *src, size_t n)
{
    uint8_t *d = dst;
    const uint8_t *s = src;

    for (size_t i = 0; i < n; i++) {
        d[i] = s[i];
    }
}

static u64 initrd_base(const struct bootinfo *bi)
{
    for (u32 i = 0; i < bi->reserved_count; i++) {
        if (bi->reserved[i].type == BOOT_REGION_INITRD) {
            return bi->reserved[i].base;
        }
    }
    return 0;
}

static u64 initrd_size(const struct bootinfo *bi)
{
    for (u32 i = 0; i < bi->reserved_count; i++) {
        if (bi->reserved[i].type == BOOT_REGION_INITRD) {
            return bi->reserved[i].size;
        }
    }
    return 0;
}

int loader_pin_initrd(const struct bootinfo *bi)
{
    u64 base = initrd_base(bi);
    u64 size = initrd_size(bi);

    if (base == 0 || size == 0 || size > SIZE_MAX) {
        return -1;
    }
    if (initrd_image != 0) {
        return initrd_image == (const uint8_t *)(uintptr_t)base &&
               initrd_image_size == (size_t)size ? 0 : -1;
    }
    initrd_image = (const uint8_t *)(uintptr_t)base;
    initrd_image_size = (size_t)size;
    demo_log("[loader] initrd pinned");
    return 0;
}

int loader_find_initrd(const void *base, size_t size, const char *name,
                       const void **image, size_t *image_size)
{
    const uint8_t *p = base;
    size_t off = 12;
    uint32_t count;

    if (base == 0 || image == 0 || image_size == 0 || name == 0 || size < 12) {
        return -1;
    }
    if (p[0] != 'M' || p[1] != 'I' || p[2] != 'R' || p[3] != 'D' ||
        read_le32(p + 4) != MIRD_VERSION) {
        return -1;
    }
    count = read_le32(p + 8);
    for (uint32_t i = 0; i < count; i++) {
        uint32_t name_len;
        uint32_t data_len;
        size_t next;
        size_t aligned;

        if (off > size || size - off < 8) {
            return -1;
        }
        name_len = read_le32(p + off);
        data_len = read_le32(p + off + 4);
        off += 8;
        if (name_len == 0 || name_len > 255 || name_len > size - off) {
            return -1;
        }
        if (data_len > size - off - name_len) {
            return -1;
        }
        if (name_equals(name, (const char *)(p + off), name_len)) {
            *image = p + off + name_len;
            *image_size = data_len;
            return 0;
        }
        next = off + (size_t)name_len + (size_t)data_len;
        if (next > SIZE_MAX - 3) {
            return -1;
        }
        aligned = (next + 3) & ~(size_t)3;
        if (aligned > size) {
            return -1;
        }
        off = aligned;
    }
    return -1;
}

static int rights_from_ph(uint32_t flags)
{
    int rights = 0;

    if ((flags & ~(uint32_t)(PF_R | PF_W | PF_X)) != 0) {
        return MEM_EINVAL;
    }
    if (flags & PF_R) {
        rights |= MEM_R;
    }
    if (flags & PF_W) {
        rights |= MEM_W;
    }
    if (flags & PF_X) {
        rights |= MEM_X;
    }
    if (rights == 0 || ((rights & MEM_W) && (rights & MEM_X))) {
        return MEM_EINVAL;
    }
    return rights | MEM_U;
}

static const struct elf64_phdr *phdr_at(const struct elf64_ehdr *eh, unsigned i)
{
    const uint8_t *p = (const uint8_t *)eh;

    return (const struct elf64_phdr *)(p + eh->e_phoff +
                                      (size_t)i * sizeof(struct elf64_phdr));
}

static vaddr_t page_start(vaddr_t va)
{
    return va & ~(vaddr_t)(PAGE_SIZE - 1);
}

static vaddr_t page_end(vaddr_t va)
{
    return (va + PAGE_SIZE - 1) & ~(vaddr_t)(PAGE_SIZE - 1);
}

static int validate_elf(const uint8_t *img, size_t size, unsigned *loaded)
{
    const struct elf64_ehdr *eh = (const struct elf64_ehdr *)img;
    size_t phsize;
    unsigned count = 0;
    bool entry_ok = false;

    if (img == 0 || loaded == 0 || size < sizeof(*eh)) {
        return MEM_EINVAL;
    }
    if (eh->e_ident[0] != ELFMAG0 || eh->e_ident[1] != ELFMAG1 ||
        eh->e_ident[2] != ELFMAG2 || eh->e_ident[3] != ELFMAG3) {
        demo_log("[loader] reject: bad magic");
        return MEM_EINVAL;
    }
    if (eh->e_ident[4] != 2 || eh->e_ident[5] != 1) {
        demo_log("[loader] reject: bad class/data");
        return MEM_EINVAL;
    }
    if (eh->e_machine != EM_RISCV) {
        demo_log("[loader] reject: bad machine");
        return MEM_EINVAL;
    }
    if (eh->e_type != ET_EXEC) {
        demo_log("[loader] reject: bad type");
        return MEM_EINVAL;
    }
    if (eh->e_ehsize != sizeof(*eh) ||
        eh->e_phentsize != sizeof(struct elf64_phdr) || eh->e_phnum == 0 ||
        (eh->e_phoff & 7U) != 0) {
        return MEM_EINVAL;
    }
    phsize = (size_t)eh->e_phnum * sizeof(struct elf64_phdr);
    if (eh->e_phoff > size || phsize > size - (size_t)eh->e_phoff) {
        return MEM_EINVAL;
    }

    for (unsigned i = 0; i < eh->e_phnum; i++) {
        const struct elf64_phdr *ph = phdr_at(eh, i);
        vaddr_t end;

        if (ph->p_type != PT_LOAD) {
            continue;
        }
        if (ph->p_filesz > ph->p_memsz || ph->p_offset > size ||
            ph->p_filesz > size - (size_t)ph->p_offset ||
            ph->p_vaddr > UINT64_MAX - ph->p_memsz ||
            ((ph->p_vaddr ^ ph->p_offset) & (PAGE_SIZE - 1)) != 0 ||
            (ph->p_memsz != 0 && page_end((vaddr_t)(ph->p_vaddr + ph->p_memsz)) <=
                                      (vaddr_t)ph->p_vaddr)) {
            return MEM_EINVAL;
        }
        if (rights_from_ph(ph->p_flags) < 0) {
            return MEM_EINVAL;
        }
        end = (vaddr_t)(ph->p_vaddr + ph->p_memsz);
        if (eh->e_entry >= ph->p_vaddr && eh->e_entry < end &&
            (ph->p_flags & PF_X)) {
            entry_ok = true;
        }
        if (ph->p_memsz != 0) {
            count++;
        }
    }
    if (count == 0 || !entry_ok) {
        return MEM_EINVAL;
    }
    *loaded = count;
    return MEM_OK;
}

static int page_rights(const struct elf64_ehdr *eh, vaddr_t page)
{
    int rights = MEM_U;
    bool found = false;

    for (unsigned i = 0; i < eh->e_phnum; i++) {
        const struct elf64_phdr *ph = phdr_at(eh, i);
        vaddr_t first;
        vaddr_t last;
        int part;

        if (ph->p_type != PT_LOAD || ph->p_memsz == 0) {
            continue;
        }
        first = page_start((vaddr_t)ph->p_vaddr);
        last = page_end((vaddr_t)(ph->p_vaddr + ph->p_memsz));
        if (page < first || page >= last) {
            continue;
        }
        part = rights_from_ph(ph->p_flags);
        if (part < 0) {
            return part;
        }
        rights |= part;
        found = true;
    }
    if (!found || ((rights & MEM_W) && (rights & MEM_X))) {
        return MEM_EINVAL;
    }
    return rights;
}

static void unmap_segments(struct as *as, const struct elf64_ehdr *eh)
{
    for (unsigned i = 0; i < eh->e_phnum; i++) {
        const struct elf64_phdr *ph = phdr_at(eh, i);
        vaddr_t cur;

        if (ph->p_type != PT_LOAD || ph->p_memsz == 0) {
            continue;
        }
        cur = page_start((vaddr_t)ph->p_vaddr);
        while (cur < page_end((vaddr_t)(ph->p_vaddr + ph->p_memsz))) {
            pfn_t pfn;
            int rights;

            /*
             * 只回收 ELF 自己的 U 映射。设备 MMIO 也是恒等映射，
             * 但它是无 U 位的 NONOWNED 页；误 unmap 会让内核在
             * 这个 AS 里访问 UART/virtio 时缺页。
             */
            if (as_translate(as, cur, &rights, &pfn) == MEM_OK &&
                (rights & MEM_U)) {
                (void)as_unmap(as, cur);
            }
            cur += PAGE_SIZE;
        }
    }
}

static int map_segments(struct as *as, const struct elf64_ehdr *eh)
{
    for (unsigned i = 0; i < eh->e_phnum; i++) {
        const struct elf64_phdr *ph = phdr_at(eh, i);
        vaddr_t cur;
        vaddr_t end;

        if (ph->p_type != PT_LOAD || ph->p_memsz == 0) {
            continue;
        }
        cur = page_start((vaddr_t)ph->p_vaddr);
        end = page_end((vaddr_t)(ph->p_vaddr + ph->p_memsz));
        while (cur < end) {
            pfn_t pfn;
            int rc;

            int rights = 0;

            rc = as_translate(as, cur, &rights, &pfn);
            if (rc == MEM_OK) {
                /*
                 * 两段 PT_LOAD 共用一页时第二段会看到已映射页；只接受
                 * 真正的 U 页。设备 MMIO 同 VA 也在页表里，但它没有
                 * U 位，不能当成 ELF 段跳过，否则 copy_segments 会把
                 * 文件内容写进 MMIO（frame_ptr 为 NULL）。
                 */
                if (!(rights & MEM_U)) {
                    return MEM_EPERM;
                }
                cur += PAGE_SIZE;
                continue;
            }
            if (rc != MEM_EFAULT) {
                return rc;
            }
            pfn = frame_alloc();
            if (pfn == PFN_NONE) {
                return MEM_ENOMEM;
            }
            local_memset(frame_ptr(pfn), 0, PAGE_SIZE);
            rc = as_map(as, cur, pfn, page_rights(eh, cur));
            if (rc != MEM_OK) {
                frame_unref(pfn);
                return rc;
            }
            frame_unref(pfn);
            cur += PAGE_SIZE;
        }
    }
    return MEM_OK;
}

static int copy_segments(struct as *as, const uint8_t *img)
{
    const struct elf64_ehdr *eh = (const struct elf64_ehdr *)img;

    for (unsigned i = 0; i < eh->e_phnum; i++) {
        const struct elf64_phdr *ph = phdr_at(eh, i);
        size_t done = 0;

        if (ph->p_type != PT_LOAD) {
            continue;
        }
        while (done < ph->p_filesz) {
            vaddr_t va = (vaddr_t)ph->p_vaddr + done;
            pfn_t pfn;
            size_t offset = (size_t)(va & (PAGE_SIZE - 1));
            size_t chunk = PAGE_SIZE - offset;
            int rc = as_translate(as, va, 0, &pfn);

            if (rc != MEM_OK) {
                return rc;
            }
            if (chunk > ph->p_filesz - done) {
                chunk = (size_t)ph->p_filesz - done;
            }
            {
                uint8_t *frame = (uint8_t *)frame_ptr(pfn);

                if (frame == 0) {
                    return MEM_EFAULT;
                }
                local_memcpy(frame + offset, img + ph->p_offset + done, chunk);
            }
            done += chunk;
        }
    }
    return MEM_OK;
}

int loader_load_elf(struct as *as, const void *elf, size_t elf_size,
                    struct loader_image *out)
{
    const struct elf64_ehdr *eh = elf;
    unsigned loaded = 0;
    int rc;
    char line[96];
    char *p;

    if (as == 0 || out == 0) {
        return MEM_EINVAL;
    }
    rc = validate_elf(elf, elf_size, &loaded);
    if (rc != MEM_OK) {
        demo_log("[loader] validate failed");
        return rc;
    }
    rc = map_segments(as, eh);
    if (rc == MEM_OK) {
        rc = copy_segments(as, elf);
    }
    if (rc != MEM_OK) {
        p = put_str(line, "[loader] map/copy failed error=");
        p = put_dec(p, (uint64_t)(-rc));
        *p = '\0';
        demo_log(line);
        unmap_segments(as, eh);
        return rc;
    }

    out->as = as;
    out->entry = (vaddr_t)eh->e_entry;
    out->stack_top = USER_STACK_VA + PAGE_SIZE - 16;
    p = put_str(line, "[loader] elf: ");
    p = put_dec(p, loaded);
    p = put_str(p, " PT_LOAD, entry=0x");
    p = put_hex(p, eh->e_entry);
    *p = '\0';
    demo_log(line);
    return MEM_OK;
}

#ifndef MICRO_LOADER_HOST_TEST
static _Noreturn void user_launch_entry(void *arg)
{
    struct user_launch *u = arg;
    struct trap_frame *tf = u->frame;

    irq_disable();
    __asm__ volatile("mv sp, %0\n\tj trap_return\n\t" : : "r"(tf) : "memory");
    __builtin_unreachable();
}

/*
 * 用户栈页数。
 *
 * 单页（4 KiB）在 M7 早期够用，但 U-mode `fs` 现在自带 ext4+vfs（A1），
 * 挂载与路径解析的调用深度会让 4 KiB 栈在 `ext4_read_at` 一层溢出：
 * 表现为栈顶附近缺页，被 VM 以 `reason=rights` 拒绝、进程直接死掉。
 * 16 KiB 覆盖当前所有 U-mode 服务的深调用（ext4 extent 遍历 + vfs 递归
 * 解析 + IPC 包装），同时每个进程仍只占 4 帧。
 */
#define USER_STACK_PAGES 4

static int map_user_stack(struct as *as)
{
    /*
     * 栈向低地址增长：最高一页放在 USER_STACK_VA，其余排在其下方。
     * `loader_image.stack_top` 仍是 USER_STACK_VA + PAGE_SIZE - 16，
     * 因此不需要改 trap 帧的初始 sp。
     */
    for (unsigned i = 0; i < USER_STACK_PAGES; i++) {
        vaddr_t va = USER_STACK_VA - (vaddr_t)i * PAGE_SIZE;
        pfn_t stack = frame_alloc();

        if (stack == PFN_NONE) {
            return MEM_ENOMEM;
        }
        local_memset(frame_ptr(stack), 0, PAGE_SIZE);
        if (as_map(as, va, stack, MEM_U | MEM_R | MEM_W) != MEM_OK) {
            frame_unref(stack);
            return MEM_EINVAL;
        }
        frame_unref(stack);
    }
    return MEM_OK;
}

#ifndef MICRO_LOADER_HOST_TEST
/*
 * U-mode 线程在内核代其执行 syscall 时仍运行在自己的 satp 下。只共享
 * 0x80000000 以上的内核 RAM 顶层表项（as_share_kernel first_index=2）
 * 会让 UART/PLIC/virtio 低位 MMIO（0x0c/0x10）缺页，因此把内核的
 * 设备恒等映射（无 U 位，U-mode 不可访问）复制进每个用户 AS。
 */
static int loader_map_kernel_devices(struct as *as)
{
    if (as_map_kernel_range(as, (paddr_t)UART0_BASE, PAGE_SIZE) != MEM_OK ||
        as_map_kernel_range(as, (paddr_t)PLIC_BASE, 0x400000UL) != MEM_OK) {
        return MEM_EINVAL;
    }
    for (unsigned base = 0x10001000UL; base <= 0x10008000UL; base += 0x1000UL) {
        int rc = as_map_kernel_range(as, (paddr_t)base, PAGE_SIZE);

        if (rc != MEM_OK && rc != MEM_EEXIST) {
            return MEM_EINVAL;
        }
    }
    return MEM_OK;
}
#endif

int loader_spawn_user(const struct bootinfo *bi, pfn_t kernel_root,
                      const char *initrd_name, const struct loader_boot_args *boot,
                      struct thread **thread_out)
{
    u64 base = initrd_base(bi);
    u64 size = initrd_size(bi);
    const void *elf = 0;
    size_t elf_size = 0;
    struct as *as;
    struct loader_image img;
    struct user_launch *launch;
    struct thread *t;
    char line[80];
    char *p;

    if (initrd_image != 0) {
        base = (u64)(uintptr_t)initrd_image;
        size = initrd_image_size;
    }
    if (base == 0 || size == 0 ||
        loader_find_initrd((const void *)(uintptr_t)base, (size_t)size,
                           initrd_name, &elf, &elf_size) != 0) {
        demo_log("[loader] missing initrd member");
        return LOADER_ENOENT;
    }

    p = put_str(line, "[loader] initrd=0x");
    p = put_hex(p, base);
    p = put_str(p, " size=");
    p = put_dec(p, size);
    *p = '\0';
    demo_log(line);

    as = as_create();
    if (as == 0) {
        demo_log("[loader] as create failed");
        return LOADER_ENOMEM;
    }
#ifndef MICRO_LOADER_HOST_TEST
    if (cap_as_bind(as) != CAP_OK) {
        demo_log("[loader] as cap bind failed");
        as_unref(as);
        return LOADER_ENOMEM;
    }
#endif
    if (as_share_kernel(as, kernel_root, 2) != MEM_OK) {
        demo_log("[loader] share kernel failed");
        as_unref(as);
        return LOADER_EINVAL;
    }
#ifndef MICRO_LOADER_HOST_TEST
    if (loader_map_kernel_devices(as) != MEM_OK) {
        demo_log("[loader] device mmio map failed");
        as_unref(as);
        return LOADER_EMAP;
    }
#endif
    {
        int rc = loader_load_elf(as, elf, elf_size, &img);

        if (rc != MEM_OK) {
            p = put_str(line, "[loader] elf load failed error=");
            p = put_dec(p, (uint64_t)(-rc));
            *p = '\0';
            demo_log(line);
            as_unref(as);
            return rc == MEM_ENOMEM ? LOADER_ENOMEM : LOADER_EMAP;
        }
    }
    if (map_user_stack(as) != MEM_OK) {
        demo_log("[loader] user stack map failed");
        as_unref(as);
        return LOADER_ENOMEM;
    }

    launch = launch_alloc();
    if (launch == 0) {
        demo_log("[loader] launch table full");
        as_unref(as);
        return LOADER_EFULL;
    }
    local_memset(launch, 0, sizeof(*launch));

    {
        uintptr_t top = (uintptr_t)launch->kstack + sizeof(launch->kstack);
        struct trap_frame *tf = (void *)(top - FRAME_RSV);

        tf->sp = (uintptr_t)img.stack_top;
        tf->tp = (uintptr_t)hart_local_get();
        tf->sepc = (uintptr_t)img.entry;
        if (boot != NULL) {
            tf->a0 = boot->a0;
            tf->a1 = boot->a1;
            tf->a2 = boot->a2;
            tf->a3 = boot->a3;
            tf->a4 = boot->a4;
            tf->a5 = boot->a5;
        }
        tf->sstatus = csr_read(CSR_SSTATUS) &
                      ~(SSTATUS_SPP | SSTATUS_SIE | SSTATUS_SPIE);
        launch->frame = tf;
    }

    t = thread_create(as, user_launch_entry, launch, launch->kstack,
                      sizeof(launch->kstack), initrd_name);
    if (t == 0) {
        demo_log("[loader] thread_create failed");
        launch_free(launch);
        as_unref(as);
        return LOADER_ENOMEM;
    }
    as_unref(as);
    if (boot != NULL) {
        sched_thread_set_cap_sid(t, boot->cap_sid);
    }
    if (thread_out) {
        *thread_out = t;
    }
    return 0;
}

int loader_abort_spawn(struct thread *t)
{
    if (t == 0) {
        return -1;
    }
    /*
     * 只用在不曾运行用户入口的回滚路径。launch 槽不在这里释放：统一由
     * thread_destroy 钩子 loader_release_launch_for_thread 归还，这样即使
     * discard 失败（线程已 RUNNING）也不会留下悬空的 kstack 引用。
     */
    return sched_thread_discard(t);
}

/*
 * 终止一个可能已经跑起来的子进程。
 *
 * 与 loader_abort_spawn 的差别：那个只用于「尚未运行用户入口」的回滚，
 * 遇到 RUNNING 线程会失败。这里对 RUNNING / 阻塞在异步 syscall 的线程
 * 走 sched_thread_terminate()：释放 launch 槽、撤销在途 reply，再摘除或
 * 登记延迟退出。
 *
 * 返回 0 表示线程已被摘除或已登记退出；-1 表示目标不可终止。
 */
int loader_terminate_spawn(struct thread *t)
{
    if (t == 0) {
        return -1;
    }
    /*
     * launch 槽（含内核栈）由 sched 的 thread_destroy 钩子
     * loader_release_launch_for_thread 统一归还——RUNNING 线程还在用那块栈，
     * 这里绝不能提前释放。本函数只负责状态迁移。
     */
    return sched_thread_terminate(t);
}

/* sched 在线程真正销毁时回调，归还该线程的 launch 槽。 */
void loader_release_launch_for_thread(struct thread *t)
{
    if (t == 0 || t->entry != user_launch_entry || t->arg == 0) {
        return;
    }
    launch_free((struct user_launch *)t->arg);
    t->arg = 0;
}
#endif