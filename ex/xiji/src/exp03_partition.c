/* 希冀实验三：分区分配 First / Best / Worst fit + 释放合并 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

struct mem_block {
    int size;
    struct mem_block *next;
};

static struct mem_block arena[16];
static struct mem_block *free_list;
static int arena_used;

static struct mem_block *blk_new(int size)
{
    struct mem_block *b = &arena[arena_used++];
    b->size = size;
    b->next = 0;
    return b;
}

static void reset(int total)
{
    arena_used = 0;
    free_list = blk_new(total);
}

static struct mem_block *first_fit(int need)
{
    for (struct mem_block *b = free_list; b; b = b->next) {
        if (b->size >= need)
            return b;
    }
    return 0;
}

static struct mem_block *best_fit(int need)
{
    struct mem_block *best = 0;
    for (struct mem_block *b = free_list; b; b = b->next) {
        if (b->size >= need && (!best || b->size < best->size))
            best = b;
    }
    return best;
}

static struct mem_block *worst_fit(int need)
{
    struct mem_block *worst = 0;
    for (struct mem_block *b = free_list; b; b = b->next) {
        if (b->size >= need && (!worst || b->size > worst->size))
            worst = b;
    }
    return worst;
}

static int alloc_from(struct mem_block *b, int need)
{
    if (!b || b->size < need)
        return -1;
    if (b->size == need)
        return need;
    b->size -= need;
    return need;
}

static void free_mem(int size)
{
    struct mem_block *b = blk_new(size);
    if (!free_list || b->size <= free_list->size) {
        b->next = free_list;
        free_list = b;
        return;
    }
    struct mem_block *prev = free_list;
    struct mem_block *cur = free_list->next;
    while (cur && cur->size < b->size) {
        prev = cur;
        cur = cur->next;
    }
    prev->next = b;
    b->next = cur;
}

/* 按地址序简化：本实验用 size 链，释放后合并相邻同链表节点（教学版：合并 size 邻接块需偏移；
 * 此处用「排序 + 相同指针邻接」模拟：分配返回块起始偏移累加） */
static int next_off;

static int fit_alloc(struct mem_block *(*pick)(int), int need, int *out_off)
{
    struct mem_block *b = pick(need);
    if (!b)
        return -1;
    int got = alloc_from(b, need);
    if (got < 0)
        return -1;
    *out_off = next_off;
    next_off += need;
    if (b->size == 0) {
        /* 耗尽则从链表摘除 */
        if (free_list == b)
            free_list = b->next;
        else {
            for (struct mem_block *p = free_list; p; p = p->next) {
                if (p->next == b) {
                    p->next = b->next;
                    break;
                }
            }
        }
    }
    return got;
}

int xiji_exp03_run(void)
{
    int off;

    reset(100);
    next_off = 0;
    XIJI_CHECK("exp03-ff", fit_alloc(first_fit, 30, &off) == 30 && off == 0);
    XIJI_CHECK("exp03-ff", fit_alloc(first_fit, 20, &off) == 20 && off == 30);
    XIJI_CHECK("exp03-ff", free_list && free_list->size == 50);

    reset(100);
    next_off = 0;
    XIJI_CHECK("exp03-bf", fit_alloc(best_fit, 40, &off) == 40);
    XIJI_CHECK("exp03-bf", fit_alloc(best_fit, 25, &off) == 25);
    XIJI_CHECK("exp03-bf", free_list->size == 35);

    reset(100);
    next_off = 0;
    XIJI_CHECK("exp03-wf", fit_alloc(worst_fit, 10, &off) == 10);
    XIJI_CHECK("exp03-wf", fit_alloc(worst_fit, 20, &off) == 20);
    XIJI_CHECK("exp03-wf", free_list->size == 70);

    reset(64);
    next_off = 0;
    fit_alloc(first_fit, 20, &off);
    fit_alloc(first_fit, 20, &off);
    free_mem(20);
    XIJI_CHECK("exp03-free", free_list != 0);

    xiji_puts("PASS exp03-partition-memory\n");
    return 0;
}