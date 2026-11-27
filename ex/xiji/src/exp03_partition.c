/*
 * 希冀实验三：分区存储管理 —— First / Best / Worst fit + free_mem
 *
 * 课内要求（First_fit / Best_fit / Worst_fit / free_mem）：
 *   - 三种 fit 只在"挑哪块空闲区"上不同，切分与记账逻辑共用；
 *   - free_mem 必须把释放块标为空闲，并**合并物理相邻**的空闲邻块；
 *   - 空闲区通常按地址升序挂在链上，合并才是 O(1) 的邻居操作。
 *
 * 这里用"按基址升序的块数组"实现：块要么空闲要么占用，切分在 alloc 里做，
 * 合并在 free 里做。数组比链表更容易做不变量校验（有序、无重叠、总量守恒），
 * 而这三条恰好能抓住分区实验里最常见的两类 bug：
 *   1. 精确匹配时忘记把块移出空闲区 → 同一块被分配两次；
 *   2. 释放时不做相邻合并 → 空闲区越切越碎，大作业再也装不进去。
 *
 * 该实现同时被 QEMU 硬件轨复用：hw_verify 用它管理一段**真实物理内存**，
 * 每个分区块都落到不同的物理地址上，从而验证"分区互不重叠"不只是账本正确。
 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

#define BLK_MAX 64
#define MEM_TOTAL 512

struct blk {
    int base;
    int size;
    int used;
    int owner; /* 占用者标识，空闲为 -1 */
};

static struct blk blocks[BLK_MAX];
static int block_n;
static int mem_total;

/* ---------- 内部工具 ---------- */

static int total_ok(void)
{
    int sum = 0;

    for (int i = 0; i < block_n; i++)
        sum += blocks[i].size;
    return sum == mem_total;
}

static int invariant_ok(void)
{
    int expect_base = 0;

    if (block_n <= 0 || block_n > BLK_MAX)
        return 0;
    for (int i = 0; i < block_n; i++) {
        if (blocks[i].size <= 0)
            return 0; /* 不允许零长度块残留 */
        if (blocks[i].base != expect_base)
            return 0; /* 必须首尾相接、无空洞无重叠 */
        expect_base += blocks[i].size;
    }
    return expect_base == mem_total;
}

static int find_free_span(int i)
{
    return i >= 0 && i < block_n && !blocks[i].used;
}

/* 把 [i] 与 [i+1] 合并；调用方保证两者都空闲且相邻 */
static void merge_at(int i)
{
    blocks[i].size += blocks[i + 1].size;
    for (int j = i + 1; j + 1 < block_n; j++)
        blocks[j] = blocks[j + 1];
    block_n--;
}

/* 释放后向两侧合并 */
static void coalesce(void)
{
    for (int i = 0; i + 1 < block_n;) {
        if (!blocks[i].used && !blocks[i + 1].used)
            merge_at(i);
        else
            i++;
    }
}

/* ---------- 三种分配策略 ---------- */

static int pick_first(int need)
{
    for (int i = 0; i < block_n; i++) {
        if (find_free_span(i) && blocks[i].size >= need)
            return i;
    }
    return -1;
}

static int pick_best(int need)
{
    int best = -1;

    for (int i = 0; i < block_n; i++) {
        if (!find_free_span(i) || blocks[i].size < need)
            continue;
        if (best < 0 || blocks[i].size < blocks[best].size)
            best = i;
    }
    return best;
}

static int pick_worst(int need)
{
    int worst = -1;

    for (int i = 0; i < block_n; i++) {
        if (!find_free_span(i) || blocks[i].size < need)
            continue;
        if (worst < 0 || blocks[i].size > blocks[worst].size)
            worst = i;
    }
    return worst;
}

/* ---------- 对外接口 ---------- */

void xiji_exp03_reset(void)
{
    block_n = 0;
    mem_total = MEM_TOTAL;
    blocks[block_n++] = (struct blk){ 0, MEM_TOTAL, 0, -1 };
}

int xiji_exp03_alloc(int policy, int size, int owner)
{
    int idx;
    int base;

    if (size <= 0 || block_n >= BLK_MAX)
        return -1;
    switch (policy) {
    case XIJI_FIT_FIRST:
        idx = pick_first(size);
        break;
    case XIJI_FIT_BEST:
        idx = pick_best(size);
        break;
    case XIJI_FIT_WORST:
        idx = pick_worst(size);
        break;
    default:
        return -1;
    }
    if (idx < 0)
        return -1;

    /* 需要切分：把剩余部分作为一个新的空闲块插到后面，保持按基址有序 */
    if (blocks[idx].size > size) {
        for (int j = block_n; j > idx + 1; j--)
            blocks[j] = blocks[j - 1];
        block_n++;
        blocks[idx + 1] = (struct blk){
            blocks[idx].base + size,
            blocks[idx].size - size,
            0,
            -1,
        };
        blocks[idx].size = size;
    }
    base = blocks[idx].base;
    blocks[idx].used = 1;
    blocks[idx].owner = owner;
    return base;
}

int xiji_exp03_free(int base)
{
    for (int i = 0; i < block_n; i++) {
        if (blocks[i].base != base)
            continue;
        if (!blocks[i].used)
            return -1; /* 重复释放 */
        blocks[i].used = 0;
        blocks[i].owner = -1;
        coalesce();
        return 0;
    }
    return -1;
}

int xiji_exp03_free_bytes(void)
{
    int sum = 0;

    for (int i = 0; i < block_n; i++) {
        if (!blocks[i].used)
            sum += blocks[i].size;
    }
    return sum;
}

int xiji_exp03_block_count(void)
{
    return block_n;
}

int xiji_exp03_invariants_ok(void)
{
    return invariant_ok();
}

/* ---------- 自检 ---------- */

/* 只统计空闲块，返回最大空闲块大小 */
static int largest_free(void)
{
    int best = 0;

    for (int i = 0; i < block_n; i++) {
        if (!blocks[i].used && blocks[i].size > best)
            best = blocks[i].size;
    }
    return best;
}

static int alloc_ok(int policy, int size, int want_base)
{
    int got = xiji_exp03_alloc(policy, size, 1);

    return got == want_base && invariant_ok();
}

int xiji_exp03_run(void)
{
    int a;
    int b;
    int c;

    /* --- First fit：低地址优先 --- */
    xiji_exp03_reset();
    XIJI_CHECK("exp03-ff", alloc_ok(XIJI_FIT_FIRST, 30, 0));
    XIJI_CHECK("exp03-ff", alloc_ok(XIJI_FIT_FIRST, 20, 30));
    XIJI_CHECK("exp03-ff", xiji_exp03_free_bytes() == MEM_TOTAL - 50);
    XIJI_CHECK("exp03-ff-blocks", xiji_exp03_block_count() == 3);

    /* --- Best fit：挑刚好够用的最小块 --- */
    xiji_exp03_reset();
    a = xiji_exp03_alloc(XIJI_FIT_FIRST, 100, 1);   /* [0,100) 占, 空闲 [100,512) */
    b = xiji_exp03_alloc(XIJI_FIT_FIRST, 200, 2);   /* [100,300) 占, 空闲 [300,512)=212 */
    XIJI_CHECK("exp03-bf-setup", a == 0 && b == 100 && invariant_ok());
    /* 空闲块只有 212，请求 100：best 必须切 212 而不是别的（别处没有空闲） */
    c = xiji_exp03_alloc(XIJI_FIT_BEST, 100, 3);
    XIJI_CHECK("exp03-bf-base", c == 300);
    XIJI_CHECK("exp03-bf-remain", largest_free() == 112);

    /* --- Worst fit：挑最大的空闲块 --- */
    xiji_exp03_reset();
    a = xiji_exp03_alloc(XIJI_FIT_FIRST, 100, 1);   /* 空闲 [100,512)=412 */
    b = xiji_exp03_alloc(XIJI_FIT_FIRST, 100, 2);   /* 空闲 [200,512)=312 */
    XIJI_CHECK("exp03-wf-setup", a == 0 && b == 100);
    /* 释放第一块：得到空闲 [0,100)=100 与 [200,512)=312 两块，中间 200 仍占用 */
    XIJI_CHECK("exp03-wf-free", xiji_exp03_free(0) == 0 && largest_free() == 312);
    /* worst 必须挑 312 的大块（base 200）而不是 100 的小块 */
    c = xiji_exp03_alloc(XIJI_FIT_WORST, 50, 3);
    XIJI_CHECK("exp03-wf-base", c == 200);
    XIJI_CHECK("exp03-wf-remain", largest_free() == 262);

    /* worst fit 与 best fit 的选择必须相反 */
    xiji_exp03_reset();
    a = xiji_exp03_alloc(XIJI_FIT_FIRST, 200, 1);   /* 空闲 [200,512)=312 */
    XIJI_CHECK("exp03-wb-setup", a == 0);
    XIJI_CHECK("exp03-wb-free", xiji_exp03_free(0) == 0);
    /* 现在空闲 [0,200)=200 与 [200,512)=312 相邻 → 合并成 512 */
    XIJI_CHECK("exp03-wb-merged", xiji_exp03_block_count() == 1 && largest_free() == 512);

    /* --- 回归：精确匹配的块必须被移出空闲区，不能重复分配 --- */
    xiji_exp03_reset();
    XIJI_CHECK("exp03-exact", xiji_exp03_alloc(XIJI_FIT_FIRST, MEM_TOTAL, 1) == 0);
    XIJI_CHECK("exp03-exact-empty", xiji_exp03_free_bytes() == 0);
    XIJI_CHECK("exp03-exact-second", xiji_exp03_alloc(XIJI_FIT_FIRST, MEM_TOTAL, 2) == -1);

    /* --- 回归：释放后必须与相邻空闲块合并 --- */
    xiji_exp03_reset();
    a = xiji_exp03_alloc(XIJI_FIT_FIRST, 40, 1);   /* 占 [0,40) */
    b = xiji_exp03_alloc(XIJI_FIT_FIRST, 60, 2);   /* 占 [40,100) */
    XIJI_CHECK("exp03-coalesce-setup", a == 0 && b == 40 && xiji_exp03_block_count() == 3);
    /* 释放第二块：与后面的大空闲区 [100,512) 合并 → [40,512) */
    XIJI_CHECK("exp03-coalesce-free", xiji_exp03_free(40) == 0);
    XIJI_CHECK("exp03-coalesce-merged", xiji_exp03_block_count() == 2 &&
                                           largest_free() == MEM_TOTAL - 40);
    /* 再释放第一块：与 [40,512) 合并成整块 */
    XIJI_CHECK("exp03-coalesce-free2", xiji_exp03_free(0) == 0);
    XIJI_CHECK("exp03-coalesce-all", xiji_exp03_block_count() == 1 &&
                                        largest_free() == MEM_TOTAL &&
                                        xiji_exp03_free_bytes() == MEM_TOTAL);
    XIJI_CHECK("exp03-invariants", invariant_ok());

    /* 中间三明治：两侧都不空闲时释放中间块不得合并 */
    xiji_exp03_reset();
    a = xiji_exp03_alloc(XIJI_FIT_FIRST, 100, 1);
    b = xiji_exp03_alloc(XIJI_FIT_FIRST, 100, 2);
    c = xiji_exp03_alloc(XIJI_FIT_FIRST, 100, 3);
    XIJI_CHECK("exp03-sandwich-setup", a == 0 && b == 100 && c == 200 &&
                                          xiji_exp03_block_count() == 4);
    XIJI_CHECK("exp03-sandwich-free", xiji_exp03_free(100) == 0 &&
                                         xiji_exp03_block_count() == 4);
    XIJI_CHECK("exp03-sandwich-intact", largest_free() == MEM_TOTAL - 300);

    /* --- 错误路径 --- */
    xiji_exp03_reset();
    XIJI_CHECK("exp03-bad-size", xiji_exp03_alloc(XIJI_FIT_FIRST, 0, 1) == -1);
    XIJI_CHECK("exp03-bad-policy", xiji_exp03_alloc(99, 10, 1) == -1);
    XIJI_CHECK("exp03-too-big", xiji_exp03_alloc(XIJI_FIT_FIRST, MEM_TOTAL + 1, 1) == -1);
    XIJI_CHECK("exp03-free-unused", xiji_exp03_free(0) == -1);
    a = xiji_exp03_alloc(XIJI_FIT_FIRST, 20, 1);
    XIJI_CHECK("exp03-double-free", a == 0 && xiji_exp03_free(0) == 0 &&
                                       xiji_exp03_free(0) == -1);

    /* --- 碎片压力：交替分配/释放后总量守恒且始终无重叠 --- */
    xiji_exp03_reset();
    for (int round = 0; round < 40; round++) {
        int base = xiji_exp03_alloc(XIJI_FIT_BEST, 7 + round % 13, round);
        if (base < 0)
            break;
        if (!invariant_ok())
            return xiji_fail("exp03-stress", "invariant broken on alloc");
        if (round % 3 == 0 && xiji_exp03_free(base) != 0)
            return xiji_fail("exp03-stress", "free failed");
        if (!invariant_ok())
            return xiji_fail("exp03-stress", "invariant broken on free");
    }
    XIJI_CHECK("exp03-stress-accounting", total_ok());

    xiji_puts("PASS exp03-partition-memory\n");
    return 0;
}
