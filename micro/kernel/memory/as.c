#include "memory.h"

#include <stddef.h>
#include <stdint.h>

/*
 * Sv39 页表项（4 KiB 页）。
 *   63:54 保留；53:10 PPN；9:8 RSW；7 D；6 A；5 G；4 U；3 X；2 W；1 R；0 V
 */
#define PTE_V (1ULL << 0)
#define PTE_R (1ULL << 1)
#define PTE_W (1ULL << 2)
#define PTE_X (1ULL << 3)
#define PTE_U (1ULL << 4)
#define PTE_A (1ULL << 6)
#define PTE_D (1ULL << 7)
/*
 * RSW（9:8）由软件保留。bit 8 标记“非拥有映射”：叶子指向的物理页
 * 不由本 AS 持有引用，as_unmap/free_table 不得 frame_unref 它。
 */
#define PTE_RSW_NONOWNED (1ULL << 8)

#define PTE_PFN(pte) ((pfn_t)((pte) >> 10))
#define PTE_MAKE(pfn, flags) ((((uint64_t)(pfn)) << 10) | (uint64_t)(flags))

#define PTE_PERMS (PTE_R | PTE_W | PTE_X)

/* 对外权限位全集；as_map 拒绝未定义的位，避免拼写错误静默生效。 */
#define MEM_RIGHTS (MEM_R | MEM_W | MEM_X | MEM_U | MEM_KERNEL)

struct walk_result {
    uint64_t *pte;
    pfn_t pfn; /* PFN_NONE 表示叶子槽位为空 */
};

static struct as as_pool[AS_MAX];

static void memzero(void *dst, size_t len)
{
    uint8_t *p = (uint8_t *)dst;
    size_t i = 0;

    for (; i + sizeof(uint64_t) <= len; i += sizeof(uint64_t)) {
        *(uint64_t *)(p + i) = 0;
    }
    for (; i < len; i++) {
        p[i] = 0;
    }
}

static void memcopy(void *dst, const void *src, size_t len)
{
    uint8_t *d = (uint8_t *)dst;
    const uint8_t *s = (const uint8_t *)src;

    while (len-- > 0) {
        *d++ = *s++;
    }
}

bool as_is_canonical(vaddr_t va)
{
    uint64_t high = va >> 39;

    if (va & (1ULL << 38)) {
        return high == ((1ULL << 25) - 1);
    }
    return high == 0;
}

static int pte_rights(uint64_t pte)
{
    int rights = 0;

    if (pte & PTE_R) {
        rights |= MEM_R;
    }
    if (pte & PTE_W) {
        rights |= MEM_W;
    }
    if (pte & PTE_X) {
        rights |= MEM_X;
    }
    if (pte & PTE_U) {
        rights |= MEM_U;
    }
    return rights;
}

static uint64_t pte_flags(int rights)
{
    uint64_t flags = PTE_V | PTE_A;

    if ((rights & MEM_KERNEL) == 0) {
        flags |= PTE_U;
    }
    if (rights & MEM_R) {
        flags |= PTE_R;
    }
    if (rights & MEM_W) {
        flags |= PTE_R | PTE_W | PTE_D;
    }
    if (rights & MEM_X) {
        flags |= PTE_X;
    }
    return flags;
}

static void walk_rollback(pfn_t *tables, uint64_t **links, int count)
{
    for (int i = count - 1; i >= 0; i--) {
        *links[i] = 0;
        frame_unref(tables[i]);
    }
}

/*
 * 走三级页表。alloc=false 时只查找；alloc=true 时按需分配中间层。
 * level 0 的叶子槽位为空时返回 MEM_OK + pfn=PFN_NONE，由 as_map 填写。
 * 不支持 2 MiB / 1 GiB 大页。
 */
static int walk(struct as *as, vaddr_t va, bool alloc, struct walk_result *out)
{
    pfn_t table;
    pfn_t new_tables[2];
    uint64_t *new_links[2];
    int new_count = 0;
    int result = MEM_OK;

    if (as == 0 || !as->active || out == 0 || !as_is_canonical(va)) {
        return MEM_EINVAL;
    }

    table = as->root_pfn;
    for (int level = 2; level >= 0; level--) {
        uint64_t *page_table = (uint64_t *)frame_ptr(table);
        int index = (int)((va >> (PAGE_SHIFT + 9 * level)) & 0x1ff);
        uint64_t *entry;

        if (page_table == 0) {
            result = MEM_EINVAL;
            goto out;
        }
        entry = &page_table[index];

        if (level == 0) {
            if (!(*entry & PTE_V)) {
                if (!alloc) {
                    result = MEM_EFAULT;
                    goto out;
                }
                out->pte = entry;
                out->pfn = PFN_NONE;
                goto out;
            }
            if (!(*entry & PTE_PERMS)) {
                result = MEM_EINVAL;
                goto out;
            }
            out->pte = entry;
            out->pfn = PTE_PFN(*entry);
            goto out;
        }

        if (!(*entry & PTE_V)) {
            pfn_t child;

            if (!alloc) {
                result = MEM_EFAULT;
                goto out;
            }
            child = frame_alloc();
            if (child == PFN_NONE) {
                result = MEM_ENOMEM;
                goto out;
            }
            memzero(frame_ptr(child), PAGE_SIZE);
            *entry = PTE_MAKE(child, PTE_V);
            new_tables[new_count] = child;
            new_links[new_count] = entry;
            new_count++;
        } else if (*entry & PTE_PERMS) {
            /* 非叶子层出现叶子描述符：大页未实现。 */
            result = MEM_EINVAL;
            goto out;
        }

        table = PTE_PFN(*entry);
    }

    result = MEM_EINVAL;

out:
    if (result != MEM_OK) {
        walk_rollback(new_tables, new_links, new_count);
    }
    return result;
}

static void free_table(pfn_t table, int level, int root_limit)
{
    uint64_t *page_table = (uint64_t *)frame_ptr(table);
    int limit = (level == 2) ? root_limit : 512;

    if (page_table == 0) {
        return;
    }

    for (int i = 0; i < limit; i++) {
        uint64_t entry = page_table[i];
        pfn_t child;

        if (!(entry & PTE_V)) {
            continue;
        }
        child = PTE_PFN(entry);

        if (entry & PTE_PERMS) {
            /*
             * 只有 as_map() 建立的拥有映射才持有 frame 引用。
             * as_map_kernel_range() 的恒等映射标记为非拥有：物理页由
             * 启动资产或 frame 池的 owner 负责，销毁 AS 不能误回收。
             */
            if (!(entry & PTE_RSW_NONOWNED))
                frame_unref(child);
        } else if (level > 0) {
            free_table(child, level - 1, 512);
            frame_unref(child);
        }
    }
}

void cap_as_release(struct as *as) __attribute__((weak));

static void as_destroy(struct object *obj)
{
    struct as *as = (struct as *)obj;

    if (cap_as_release != 0) {
        cap_as_release(as);
    }
    if (as->active && as->root_pfn != PFN_NONE) {
        free_table(as->root_pfn, 2, as->kernel_start);
        frame_unref(as->root_pfn);
    }
    as->root_pfn = PFN_NONE;
    as->active = false;
    as->kernel_start = 512;
}

struct as *as_create(void)
{
    struct as *as = 0;
    pfn_t root;

    for (int i = 0; i < AS_MAX; i++) {
        if (!as_pool[i].active) {
            as = &as_pool[i];
            break;
        }
    }
    if (as == 0) {
        return 0;
    }

    root = frame_alloc();
    if (root == PFN_NONE) {
        return 0;
    }
    memzero(frame_ptr(root), PAGE_SIZE);

    as->root_pfn = root;
    as->kernel_start = 512;
    as->active = true;
    if (object_register(&as->obj, OBJ_AS, as_destroy) < 0) {
        frame_unref(root);
        as->root_pfn = PFN_NONE;
        as->active = false;
        return 0;
    }
    return as;
}

void as_ref(struct as *as)
{
    if (as != 0) {
        object_ref(&as->obj);
    }
}

void as_unref(struct as *as)
{
    if (as != 0) {
        object_unref(&as->obj);
    }
}

int as_share_kernel(struct as *as, pfn_t kernel_root, int first_index)
{
    uint64_t *dst;
    uint64_t *src;

    if (as == 0 || !as->active || first_index < 0 || first_index > 512) {
        return MEM_EINVAL;
    }
    if (as->kernel_start != 512) {
        return MEM_EEXIST; /* 只允许安装一次内核共享区间 */
    }
    if (kernel_root == PFN_NONE || frame_refcount(kernel_root) == 0) {
        return MEM_EINVAL;
    }
    if (as->root_pfn == kernel_root) {
        return MEM_EINVAL;
    }

    dst = (uint64_t *)frame_ptr(as->root_pfn);
    src = (uint64_t *)frame_ptr(kernel_root);
    if (dst == 0 || src == 0) {
        return MEM_EINVAL;
    }

    /*
     * 共享内核顶层表项：不增加页表页引用，as_destroy 只回收
     * [0, kernel_start) 的用户部分。
     *
     * 目标区间必须尚未映射，否则复制会静默覆盖用户 PTE 并泄漏其 frame。
     * 因此要求调用者先 as_share_kernel，再映射用户页。
     */
    for (int i = first_index; i < 512; i++) {
        if (dst[i] != 0) {
            return MEM_EEXIST;
        }
    }
    for (int i = first_index; i < 512; i++) {
        dst[i] = src[i];
    }
    as->kernel_start = (uint16_t)first_index;
    as_flush_tlb();
    return MEM_OK;
}

int as_map_kernel_range(struct as *as, paddr_t base, size_t size)
{
    paddr_t end;

    if (as == 0 || !as->active) {
        return MEM_EINVAL;
    }
    if (size == 0) {
        return MEM_OK;
    }
    if ((base & (paddr_t)(PAGE_SIZE - 1)) != 0 ||
        (size & (size_t)(PAGE_SIZE - 1)) != 0) {
        return MEM_EINVAL;
    }
    end = base + size;
    if (end < base) {
        return MEM_EINVAL;
    }
    /* 整个范围都必须落在共享内核区间之下（含跨顶层索引的末尾页）。 */
    if (((int)(((end - 1) >> 30) & 0x1ff)) >= as->kernel_start) {
        return MEM_EPERM;
    }

    /*
     * 逐页建立恒等叶子。中间页表页由 walk 按需从 frame 池分配，它们
     * 自身随后也会落在这个范围内（池窗口），所以最终同样可被 S 态
     * 通过物理地址访问。
     *
     * 叶子标记 PTE_RSW_NONOWNED：
     *   - 内核镜像页是链接脚本/固件资产，不是 frame_init 的对象；
     *   - 直接映射窗口是 frame_init 的帧池，但它由 frame_alloc/持有者
     *     管理引用，AS 的恒等映射不增加也不负责释放。
     * 因此 free_table()/as_unmap() 看到该位时跳过 frame_unref。
     */
    for (paddr_t pa = base; pa < end; pa += PAGE_SIZE) {
        struct walk_result result;
        int rc = walk(as, (vaddr_t)pa, true, &result);

        if (rc != MEM_OK) {
            return rc;
        }
        if (result.pfn != PFN_NONE) {
            return MEM_EEXIST;
        }
        *result.pte = PTE_MAKE(pa >> PAGE_SHIFT,
                              PTE_V | PTE_R | PTE_W | PTE_X | PTE_A | PTE_D |
                              PTE_RSW_NONOWNED);
    }
    as_flush_tlb();
    return MEM_OK;
}

int as_map(struct as *as, vaddr_t va, pfn_t pfn, int rights)
{
    struct walk_result result;
    int rc;

    if (as == 0 || !as->active) {
        return MEM_EINVAL;
    }
    if ((va & (vaddr_t)(PAGE_SIZE - 1)) != 0 || !as_is_canonical(va)) {
        return MEM_EINVAL;
    }
    if (((int)((va >> 30) & 0x1ff)) >= as->kernel_start) {
        return MEM_EPERM; /* 该顶层区间属于共享内核映射 */
    }
    if (pfn == PFN_NONE || frame_refcount(pfn) == 0) {
        return MEM_EINVAL;
    }
    if ((rights & ~MEM_RIGHTS) != 0 ||
        (rights & (MEM_R | MEM_W | MEM_X)) == 0) {
        return MEM_EINVAL;
    }
    if ((rights & MEM_W) && (rights & MEM_X)) {
        return MEM_EINVAL; /* W^X：不建立可写又可执行的用户页 */
    }

    rc = walk(as, va, true, &result);
    if (rc != MEM_OK) {
        return rc;
    }
    if (result.pfn != PFN_NONE) {
        return MEM_EEXIST;
    }

    if (rights & MEM_W) {
        rights |= MEM_R;
    }
    *result.pte = PTE_MAKE(pfn, pte_flags(rights));
    frame_ref(pfn);
    as_flush_tlb();
    return MEM_OK;
}

static int table_is_empty(const uint64_t *page_table, int limit)
{
    for (int i = 0; i < limit; i++) {
        if (page_table[i] & PTE_V) {
            return 0;
        }
    }
    return 1;
}

/*
 * 从 root 走到 va 的叶子，沿途记录经过的中间页表页，用于在解除映射后
 * 回收变空的页表。level2→level0，links[i] 指向指向下一级的那一项。
 */
int as_unmap(struct as *as, vaddr_t va)
{
    struct walk_result result;
    uint64_t *links[2];
    pfn_t tables[2];
    int rc;

    if (as == 0 || !as->active) {
        return MEM_EINVAL;
    }
    if ((va & (vaddr_t)(PAGE_SIZE - 1)) != 0 || !as_is_canonical(va)) {
        return MEM_EINVAL;
    }
    if (((int)((va >> 30) & 0x1ff)) >= as->kernel_start) {
        return MEM_EPERM;
    }

    rc = walk(as, va, false, &result);
    if (rc != MEM_OK) {
        return rc;
    }
    if (result.pfn == PFN_NONE) {
        return MEM_EFAULT;
    }

    {
        uint64_t pte = *result.pte;

        *result.pte = 0;
        /*
         * as_map_kernel_range() 的恒等叶子是非拥有映射；解除映射时不能
         * frame_unref，否则会释放内核镜像/帧池 owner 仍持有的页。
         */
        if (!(pte & PTE_RSW_NONOWNED)) {
            frame_unref(result.pfn);
        }
    }

    /*
     * 回收因这次 unmap 而变空的中间页表页。
     *
     * 不回收的话，反复 map/unmap 不同 VA 会把页表页永久吃掉：每新建一个
     * 叶子页表就要一帧，而 AS 存活期间没有任何其它路径归还它。demand paging
     * 场景每个新页都可能触发，长期运行必然耗尽帧池。
     */
    {
        pfn_t table = as->root_pfn;

        for (int level = 2; level >= 1; level--) {
            uint64_t *page_table = (uint64_t *)frame_ptr(table);
            int index = (int)((va >> (PAGE_SHIFT + 9 * level)) & 0x1ff);

            if (page_table == 0) {
                break;
            }
            links[2 - level] = &page_table[index];
            tables[2 - level] = PTE_PFN(page_table[index]);
            table = tables[2 - level];
        }
        /* 自底向上：子表空了就断开并归还，再看父表是否也空。 */
        for (int i = 1; i >= 0; i--) {
            uint64_t *child = (uint64_t *)frame_ptr(tables[i]);

            if (child == 0 || !table_is_empty(child, 512)) {
                break;
            }
            *links[i] = 0;
            frame_unref(tables[i]);
        }
    }

    as_flush_tlb();
    return MEM_OK;
}

int as_translate(struct as *as, vaddr_t va, int *rights, pfn_t *pfn)
{
    struct walk_result result;
    int rc;

    if (as == 0 || !as->active) {
        return MEM_EINVAL;
    }

    rc = walk(as, va, false, &result);
    if (rc != MEM_OK) {
        return rc;
    }
    if (result.pfn == PFN_NONE) {
        return MEM_EFAULT;
    }

    if (rights != 0) {
        *rights = pte_rights(*result.pte);
    }
    if (pfn != 0) {
        *pfn = result.pfn;
    }
    return MEM_OK;
}

int as_fault(struct as *as, vaddr_t va, int access, pfn_t *pfn)
{
    int rights = 0;
    pfn_t frame = PFN_NONE;
    int rc;

    rc = as_translate(as, va, &rights, &frame);
    if (rc != MEM_OK) {
        return rc;
    }
    if ((rights & access) != access) {
        return MEM_EPERM;
    }
    if (pfn != 0) {
        *pfn = frame;
    }
    return MEM_OK;
}

static int check_range(struct as *as, vaddr_t uva, size_t len, int need)
{
    vaddr_t end;

    if (len == 0) {
        return MEM_OK;
    }
    if (uva + (vaddr_t)len < uva) {
        return MEM_EFAULT;
    }
    end = uva + (vaddr_t)len;

    while (uva < end) {
        int rights = 0;
        pfn_t pfn = PFN_NONE;
        size_t chunk;
        int rc;

        rc = as_translate(as, uva, &rights, &pfn);
        if (rc != MEM_OK) {
            return rc;
        }
        if ((rights & need) != need) {
            return MEM_EPERM;
        }

        chunk = PAGE_SIZE - (size_t)(uva & (PAGE_SIZE - 1));
        if (chunk > (size_t)(end - uva)) {
            chunk = (size_t)(end - uva);
        }
        uva += chunk;
    }
    return MEM_OK;
}

static int copy_range(struct as *as, vaddr_t uva, void *buf, size_t len,
                      int need, bool to_user)
{
    size_t done = 0;
    int rc;

    rc = check_range(as, uva, len, need);
    if (rc != MEM_OK) {
        return rc;
    }

    while (done < len) {
        int rights = 0;
        pfn_t pfn = PFN_NONE;
        size_t offset;
        size_t chunk;
        uint8_t *frame;

        rc = as_translate(as, uva + done, &rights, &pfn);
        if (rc != MEM_OK) {
            return rc;
        }
        /*
         * check_range() 只保证调用开始时的权限；这里按真正拷贝时刻的
         * PTE 重新校验，避免窗口期内映射被改成只读/撤销后仍然写入。
         * 拷贝通过 direct map 访问物理帧，不经过用户 VA，因此不依赖
         * SSTATUS.SUM。
         */
        if ((rights & need) != need) {
            return MEM_EPERM;
        }
        frame = (uint8_t *)frame_ptr(pfn);
        if (frame == 0) {
            return MEM_EFAULT;
        }

        offset = (size_t)((uva + done) & (PAGE_SIZE - 1));
        chunk = PAGE_SIZE - offset;
        if (chunk > len - done) {
            chunk = len - done;
        }

        if (to_user) {
            memcopy(frame + offset, (const uint8_t *)buf + done, chunk);
        } else {
            memcopy((uint8_t *)buf + done, frame + offset, chunk);
        }
        done += chunk;
    }
    return MEM_OK;
}

int as_copyin(struct as *as, vaddr_t uva, void *dst, size_t len)
{
    return copy_range(as, uva, dst, len, MEM_R | MEM_U, false);
}

int as_copyout(struct as *as, vaddr_t uva, const void *src, size_t len)
{
    return copy_range(as, uva, (void *)src, len, MEM_W | MEM_U, true);
}

void as_flush_tlb(void)
{
#if defined(__riscv)
    __asm__ volatile("sfence.vma zero, zero" ::: "memory");
#endif
}

void as_activate(struct as *as)
{
#if defined(__riscv)
    if (as == 0 || !as->active) {
        __asm__ volatile("csrw satp, zero" ::: "memory");
    } else {
        uint64_t satp = (8ULL << 60) | (uint64_t)as->root_pfn;
        __asm__ volatile("csrw satp, %0" :: "r"(satp) : "memory");
    }
    as_flush_tlb();
#else
    (void)as;
    as_flush_tlb();
#endif
}
