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

    while (len-- > 0) {
        *p++ = 0;
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
            frame_unref(child);
        } else if (level > 0) {
            free_table(child, level - 1, 512);
            frame_unref(child);
        }
    }
}

static void as_destroy(struct object *obj)
{
    struct as *as = (struct as *)obj;

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

int as_unmap(struct as *as, vaddr_t va)
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
        return MEM_EPERM;
    }

    rc = walk(as, va, false, &result);
    if (rc != MEM_OK) {
        return rc;
    }
    if (result.pfn == PFN_NONE) {
        return MEM_EFAULT;
    }

    *result.pte = 0;
    frame_unref(result.pfn);
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
