/*
 * 希冀实验四：段式存储管理 —— apply / diaodu / zhuanhuan
 *
 * 课内要求：
 *   apply()     读入进程名、段数（上限 10）与各段长度，算出段表的相对基址与总长；
 *   diaodu()    若剩余空间够，就把进程装入内存，realaddr = 段相对基址 + 已占用；
 *   zhuanhuan() 把 (进程, 段号, 段内偏移) 换算成物理地址，必须做存在性与越界检查。
 *
 * 本实现把"装/卸"做成显式的区间分配（首次适配 + 空洞回收），
 * 因此也回答了笔记里的思考题：内存不足时按 FIFO 淘汰最早装入的进程再重试。
 * 段表本身按"相对基址"存放，装入后才叠加 loaded_base —— 这一点是段式与
 * 分区实验最容易混淆的地方：相对基址只依赖段长，与装入位置无关。
 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

enum { MAX_SEG = 10, MAX_PROC = 6, MEM_TOTAL = 512 };

struct segment {
    int size;
    int rel_base; /* 相对进程起点的偏移，apply 阶段算好 */
};

struct proc_image {
    char name[12];
    int seg_count;
    struct segment segs[MAX_SEG];
    int total;
    int loaded_base;
    int in_mem;
    int load_seq;
};

static struct proc_image procs[MAX_PROC];
static int proc_count;
static int load_seq;

/* ---------- 进程镜像 ---------- */

static int copy_name(char *dst, int cap, const char *src)
{
    int i = 0;

    if (!src || cap <= 0)
        return -1;
    for (; src[i] && i < cap - 1; i++)
        dst[i] = src[i];
    dst[i] = '\0';
    return src[i] ? -2 : 0; /* 名字过长直接报错，而不是静默截断 */
}

/* 返回进程总长，失败返回负值 */
static int apply(struct proc_image *pi, const char *name, const int *sizes, int n)
{
    int off = 0;

    if (!pi || !sizes || n <= 0 || n > MAX_SEG)
        return -1;
    if (copy_name(pi->name, (int)sizeof pi->name, name) != 0)
        return -1;

    for (int i = 0; i < n; i++) {
        if (sizes[i] <= 0)
            return -1;
        pi->segs[i].size = sizes[i];
        pi->segs[i].rel_base = off;
        off += sizes[i];
        if (off > MEM_TOTAL)
            return -1;
    }
    pi->seg_count = n;
    pi->total = off;
    pi->loaded_base = 0;
    pi->in_mem = 0;
    pi->load_seq = -1;
    return off;
}

/* ---------- 物理内存占用位图（1 = 被某进程占用） ---------- */

static unsigned char mem_map[MEM_TOTAL];
static int mem_used;

static void mem_reset(void)
{
    for (int i = 0; i < MEM_TOTAL; i++)
        mem_map[i] = 0;
    mem_used = 0;
}

static int span_free(int base, int size)
{
    if (base < 0 || size <= 0 || base + size > MEM_TOTAL)
        return 0;
    for (int i = 0; i < size; i++) {
        if (mem_map[base + i])
            return 0;
    }
    return 1;
}

static void span_mark(int base, int size, unsigned char v)
{
    for (int i = 0; i < size; i++)
        mem_map[base + i] = v;
}

static int find_hole(int size)
{
    for (int base = 0; base + size <= MEM_TOTAL; base++) {
        if (span_free(base, size))
            return base;
    }
    return -1;
}

/* ---------- 调度（装入）与换出 ---------- */

static void unload(struct proc_image *pi)
{
    if (!pi->in_mem)
        return;
    span_mark(pi->loaded_base, pi->total, 0);
    mem_used -= pi->total;
    pi->in_mem = 0;
    pi->loaded_base = 0;
    pi->load_seq = -1;
}

static int load_at(struct proc_image *pi, int base)
{
    if (!span_free(base, pi->total))
        return -1;
    span_mark(base, pi->total, 1);
    pi->loaded_base = base;
    pi->in_mem = 1;
    pi->load_seq = load_seq++;
    mem_used += pi->total;
    return 0;
}

/* 首次适配装入；失败返回 -1 */
static int diaodu(struct proc_image *pi)
{
    int base;

    if (!pi || pi->seg_count <= 0)
        return -1;
    if (pi->in_mem)
        return 0; /* 已在内存中，重复调度视为成功 */
    base = find_hole(pi->total);
    if (base < 0)
        return -1;
    return load_at(pi, base);
}

static int diaodu_fifo(struct proc_image *pi)
{
    if (diaodu(pi) == 0)
        return 0;
    /* 内存不足：按装入时序淘汰最早的进程，直到装得下或没有可淘汰者 */
    for (;;) {
        struct proc_image *victim = 0;
        for (int i = 0; i < proc_count; i++) {
            if (&procs[i] == pi || !procs[i].in_mem)
                continue;
            if (!victim || procs[i].load_seq < victim->load_seq)
                victim = &procs[i];
        }
        if (!victim)
            return -1;
        unload(victim);
        if (diaodu(pi) == 0)
            return 0;
    }
}

/* ---------- 地址转换 ---------- */

/* 成功返回物理地址，失败返回 -1 并置 *why */
static int zhuanhuan(const struct proc_image *pi, int seg, int off, const char **why)
{
    if (!pi) {
        if (why)
            *why = "no process";
        return -1;
    }
    if (!pi->in_mem) {
        if (why)
            *why = "not in memory";
        return -1;
    }
    if (seg < 0 || seg >= pi->seg_count) {
        if (why)
            *why = "segment out of range";
        return -1;
    }
    if (off < 0 || off >= pi->segs[seg].size) {
        if (why)
            *why = "offset out of range";
        return -1;
    }
    if (why)
        *why = 0;
    return pi->loaded_base + pi->segs[seg].rel_base + off;
}

/* 反向校验：物理地址必须落在该进程的装入区间且对应段的实际区间 */
static int zhuanhuan_physical_ok(const struct proc_image *pi, int seg, int off)
{
    int addr = zhuanhuan(pi, seg, off, 0);
    int seg_lo;
    int seg_hi;

    if (addr < 0)
        return 0;
    seg_lo = pi->loaded_base + pi->segs[seg].rel_base;
    seg_hi = seg_lo + pi->segs[seg].size;
    return addr >= seg_lo && addr < seg_hi && span_free(addr, 1) == 0;
}

/* ---------- 自检 ---------- */

static struct proc_image *new_proc(const char *name, const int *sizes, int n)
{
    struct proc_image *pi;

    if (proc_count >= MAX_PROC)
        return 0;
    pi = &procs[proc_count++];
    if (apply(pi, name, sizes, n) < 0)
        return 0;
    return pi;
}

static void run_reset(void)
{
    proc_count = 0;
    load_seq = 0;
    mem_used = 0;
    mem_reset();
    for (int i = 0; i < MAX_PROC; i++)
        procs[i] = (struct proc_image){ 0 };
}

int xiji_exp04_run(void)
{
    const int sizes_a[] = { 100, 50, 30 };
    const int sizes_b[] = { 60, 40 };
    const int too_long[] = { 300, 300 };
    const int eleven[] = { 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1 };
    const int bad_zero[] = { 10, 0, 10 };
    const char *why = 0;
    struct proc_image *a;
    struct proc_image *b;
    struct proc_image *big;

    run_reset();

    /* --- apply：相对基址与总长 --- */
    a = new_proc("A", sizes_a, 3);
    XIJI_CHECK("exp04-apply", a && a->total == 180);
    XIJI_CHECK("exp04-apply-relbase", a->segs[0].rel_base == 0 && a->segs[1].rel_base == 100 &&
                                          a->segs[2].rel_base == 150);
    XIJI_CHECK("exp04-apply-state", a->seg_count == 3 && !a->in_mem);
    b = new_proc("B", sizes_b, 2);
    XIJI_CHECK("exp04-apply-b", b && b->total == 100 && b->segs[1].rel_base == 60);

    /* apply 的错误路径 */
    XIJI_CHECK("exp04-apply-empty", proc_count == 2 &&
                                        !new_proc("Z", (const int[]){ 1 }, 0));
    XIJI_CHECK("exp04-apply-toomany", !new_proc("Z", eleven, 11));
    XIJI_CHECK("exp04-apply-zero", !new_proc("Z", bad_zero, 3));
    XIJI_CHECK("exp04-apply-name-long", !new_proc("a-very-long-name", (const int[]){ 1 }, 1));
    XIJI_CHECK("exp04-apply-huge", !new_proc("Z", too_long, 2));

    /* --- diaodu：首次适配，物理区间互补重叠 --- */
    XIJI_CHECK("exp04-diaodu-a", diaodu(a) == 0 && a->loaded_base == 0 && a->in_mem);
    XIJI_CHECK("exp04-diaodu-b", diaodu(b) == 0 && b->loaded_base == 180 && b->in_mem);
    XIJI_CHECK("exp04-diaodu-mem", mem_used == 280);
    XIJI_CHECK("exp04-diaodu-idempotent", diaodu(a) == 0 && a->loaded_base == 0 &&
                                              mem_used == 280);

    /* --- zhuanhuan：段内偏移 + 越界 --- */
    XIJI_CHECK("exp04-xlate-a", zhuanhuan(a, 2, 10, &why) == 160 && !why);
    XIJI_CHECK("exp04-xlate-b", zhuanhuan(b, 1, 5, &why) == 245 && !why);
    XIJI_CHECK("exp04-xlate-seg-lo", zhuanhuan(a, 1, 0, 0) == 100);
    XIJI_CHECK("exp04-xlate-seg-hi", zhuanhuan(a, 1, 49, 0) == 149);
    XIJI_CHECK("exp04-bounds-off", zhuanhuan(a, 2, 30, &why) == -1 && why);
    XIJI_CHECK("exp04-bounds-neg", zhuanhuan(a, 0, -1, 0) == -1);
    XIJI_CHECK("exp04-bounds-seg", zhuanhuan(a, 3, 0, &why) == -1 && why);
    XIJI_CHECK("exp04-bounds-segneg", zhuanhuan(a, -1, 0, 0) == -1);
    XIJI_CHECK("exp04-xlate-physical", zhuanhuan_physical_ok(a, 1, 25) &&
                                           zhuanhuan_physical_ok(b, 1, 39));

    /* 未装入的进程不能转换 */
    run_reset();
    a = new_proc("A", sizes_a, 3);
    XIJI_CHECK("exp04-xlate-notloaded", a && zhuanhuan(a, 0, 0, &why) == -1 && why);

    /* --- 卸载后可复用空洞 --- */
    run_reset();
    a = new_proc("A", sizes_a, 3);
    b = new_proc("B", sizes_b, 2);
    XIJI_CHECK("exp04-unload-setup", diaodu(a) == 0 && diaodu(b) == 0);
    unload(a);
    XIJI_CHECK("exp04-unload", !a->in_mem && mem_used == 100);
    big = new_proc("C", (const int[]){ 150 }, 1);
    XIJI_CHECK("exp04-unload-hole", big && diaodu(big) == 0 && big->loaded_base == 0);
    XIJI_CHECK("exp04-unload-nooverlap", zhuanhuan_physical_ok(big, 0, 149) &&
                                             zhuanhuan_physical_ok(b, 1, 0));

    /* --- FIFO 换出满足大进程 --- */
    run_reset();
    a = new_proc("A", (const int[]){ 200 }, 1);
    b = new_proc("B", (const int[]){ 200 }, 1);
    XIJI_CHECK("exp04-fifo-setup", diaodu_fifo(a) == 0 && diaodu_fifo(b) == 0);
    XIJI_CHECK("exp04-fifo-order", a->load_seq < b->load_seq);
    /* C 需要 300：淘汰 A（最早装入）后 [0,200) 与 [400,512) 仍不连续，
     * 首次适配找不到 300 的连续空洞，还需淘汰 B 才能装下。 */
    big = new_proc("C", (const int[]){ 300 }, 1);
    XIJI_CHECK("exp04-fifo-load", big && diaodu_fifo(big) == 0);
    XIJI_CHECK("exp04-fifo-evicted", !a->in_mem && !b->in_mem && big->in_mem);
    XIJI_CHECK("exp04-fifo-mem", mem_used == big->total && big->loaded_base == 0);

    /* FIFO 只淘汰必要的：C 只需 200 时只淘汰 A */
    run_reset();
    a = new_proc("A", (const int[]){ 200 }, 1);
    b = new_proc("B", (const int[]){ 200 }, 1);
    XIJI_CHECK("exp04-fifo2-setup", diaodu_fifo(a) == 0 && diaodu_fifo(b) == 0);
    big = new_proc("C", (const int[]){ 150 }, 1);
    XIJI_CHECK("exp04-fifo2-load", big && diaodu_fifo(big) == 0);
    XIJI_CHECK("exp04-fifo2-keep", !a->in_mem && b->in_mem && big->in_mem);
    XIJI_CHECK("exp04-fifo2-nooverlap", zhuanhuan_physical_ok(b, 0, 199) &&
                                            zhuanhuan_physical_ok(big, 0, 149));

    /* 装不下且无可淘汰者 → 失败，账本不变 */
    run_reset();
    a = new_proc("A", (const int[]){ 500 }, 1);
    b = new_proc("B", (const int[]){ 400 }, 1);
    XIJI_CHECK("exp04-fail-setup", diaodu(a) == 0);
    XIJI_CHECK("exp04-fail", diaodu(b) == -1 && !b->in_mem && mem_used == 500);

    xiji_puts("PASS exp04-segment-memory\n");
    return 0;
}
