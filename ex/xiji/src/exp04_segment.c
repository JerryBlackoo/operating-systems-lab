/* 希冀实验四：段式存储 apply / diaodu / zhuanhuan */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

enum { MAX_SEG = 10, MEM_TOTAL = 512 };

struct segment {
    int size;
    int rel_base;
};

struct proc_image {
    char name[16];
    int seg_count;
    struct segment segs[MAX_SEG];
    int loaded_base;
    int in_mem;
    int load_order;
};

static struct proc_image procs[4];
static int proc_count;
static int mem_occupied;
static int load_seq;

static int apply(struct proc_image *pi, const char *name, const int *sizes, int n)
{
    if (n > MAX_SEG || n <= 0)
        return -1;
    int off = 0;
    for (int i = 0; i < n; i++) {
        pi->segs[i].size = sizes[i];
        pi->segs[i].rel_base = off;
        off += sizes[i];
    }
    pi->seg_count = n;
    for (int i = 0; name[i] && i < 15; i++)
        pi->name[i] = name[i];
    pi->name[15] = '\0';
    pi->loaded_base = 0;
    pi->in_mem = 0;
    pi->load_order = -1;
    return off;
}

static int diaodu(struct proc_image *pi)
{
    int need = 0;
    for (int i = 0; i < pi->seg_count; i++)
        need += pi->segs[i].size;
    if (mem_occupied + need > MEM_TOTAL)
        return -1;
    pi->loaded_base = mem_occupied;
    pi->in_mem = 1;
    pi->load_order = load_seq++;
    mem_occupied += need;
    return 0;
}

static int zhuanhuan(const struct proc_image *pi, int seg, int off)
{
    if (!pi->in_mem)
        return -1;
    if (seg < 0 || seg >= pi->seg_count)
        return -1;
    if (off < 0 || off >= pi->segs[seg].size)
        return -1;
    return pi->loaded_base + pi->segs[seg].rel_base + off;
}

int xiji_exp04_run(void)
{
    proc_count = 0;
    mem_occupied = 0;
    load_seq = 0;

    struct proc_image *a = &procs[proc_count++];
    int sizes_a[] = { 100, 50, 30 };
    XIJI_CHECK("exp04-apply", apply(a, "A", sizes_a, 3) == 180);
    XIJI_CHECK("exp04-apply", a->segs[1].rel_base == 100);

    struct proc_image *b = &procs[proc_count++];
    int sizes_b[] = { 60, 40 };
    XIJI_CHECK("exp04-apply", apply(b, "B", sizes_b, 2) == 100);

    XIJI_CHECK("exp04-diaodu", diaodu(a) == 0 && a->loaded_base == 0);
    XIJI_CHECK("exp04-diaodu", diaodu(b) == 0 && b->loaded_base == 180);

    XIJI_CHECK("exp04-xlate", zhuanhuan(a, 2, 10) == 160);
    XIJI_CHECK("exp04-xlate", zhuanhuan(b, 1, 5) == 245);
    XIJI_CHECK("exp04-bounds", zhuanhuan(a, 2, 30) == -1);

    xiji_puts("PASS exp04-segment-memory\n");
    return 0;
}