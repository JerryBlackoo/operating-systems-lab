/* 希冀实验五：请求分页 FIFO / LRU / OPT */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

static const int ref_short[] = {
    7, 0, 1, 2, 0, 3, 0, 4, 2, 3, 0, 3, 2,
};
#define REF_SHORT_LEN (sizeof ref_short / sizeof ref_short[0])

/* 课程样例规模：固定 320 次访问（页号 0–19 循环扰动） */
static int ref_320[320];
static int ref_320_ready;

static void build_ref_320(void)
{
    if (ref_320_ready)
        return;
    for (int i = 0; i < 320; i++)
        ref_320[i] = (i * 7 + (i / 3) * 5 + (i % 11)) % 20;
    ref_320_ready = 1;
}

static int fifo_solve(const int *ref, int n, int frames)
{
    int mem[32];
    int faults = 0;
    int hand = 0;
    int filled = 0;
    for (int i = 0; i < n; i++) {
        int p = ref[i];
        int hit = 0;
        for (int f = 0; f < filled; f++) {
            if (mem[f] == p) {
                hit = 1;
                break;
            }
        }
        if (hit)
            continue;
        faults++;
        if (filled < frames) {
            mem[filled++] = p;
        } else {
            mem[hand] = p;
            hand = (hand + 1) % frames;
        }
    }
    return faults;
}

static int lru_solve(const int *ref, int n, int frames)
{
    int mem[32];
    int last[32];
    int faults = 0;
    int filled = 0;
    for (int i = 0; i < n; i++) {
        int p = ref[i];
        int slot = -1;
        for (int f = 0; f < filled; f++) {
            if (mem[f] == p) {
                slot = f;
                break;
            }
        }
        if (slot >= 0) {
            last[slot] = i;
            continue;
        }
        faults++;
        if (filled < frames) {
            mem[filled] = p;
            last[filled] = i;
            filled++;
            continue;
        }
        int victim = 0;
        for (int f = 1; f < frames; f++) {
            if (last[f] < last[victim])
                victim = f;
        }
        mem[victim] = p;
        last[victim] = i;
    }
    return faults;
}

static int opt_next_use(const int *ref, int n, int start, int page)
{
    for (int i = start + 1; i < n; i++) {
        if (ref[i] == page)
            return i;
    }
    return n + 1;
}

static int opt_solve(const int *ref, int n, int frames)
{
    int mem[32];
    int faults = 0;
    int filled = 0;
    for (int i = 0; i < n; i++) {
        int p = ref[i];
        int hit = 0;
        for (int f = 0; f < filled; f++) {
            if (mem[f] == p) {
                hit = 1;
                break;
            }
        }
        if (hit)
            continue;
        faults++;
        if (filled < frames) {
            mem[filled++] = p;
            continue;
        }
        int victim = 0;
        int farthest = -1;
        for (int f = 0; f < frames; f++) {
            int nx = opt_next_use(ref, n, i, mem[f]);
            if (nx > farthest) {
                farthest = nx;
                victim = f;
            }
        }
        mem[victim] = p;
    }
    return faults;
}

int xiji_exp05_run(void)
{
    const int frames = 3;
    int f_fifo = fifo_solve(ref_short, (int)REF_SHORT_LEN, frames);
    int f_lru = lru_solve(ref_short, (int)REF_SHORT_LEN, frames);
    int f_opt = opt_solve(ref_short, (int)REF_SHORT_LEN, frames);

    XIJI_CHECK("exp05-short-fifo", f_fifo == 10);
    XIJI_CHECK("exp05-short-lru", f_lru == 9);
    XIJI_CHECK("exp05-short-opt", f_opt == 7);

    build_ref_320();
    int big_fifo = fifo_solve(ref_320, 320, 4);
    int big_lru = lru_solve(ref_320, 320, 4);
    int big_opt = opt_solve(ref_320, 320, 4);

    XIJI_CHECK("exp05-320-order", big_opt <= big_fifo);
    XIJI_CHECK("exp05-320-lru", big_lru >= big_opt && big_lru <= 320);
    XIJI_CHECK("exp05-320-fifo", big_fifo >= 1 && big_fifo <= 320);

    (void)big_fifo;
    (void)big_lru;
    (void)big_opt;

    xiji_puts("PASS exp05-demand-paging\n");
    return 0;
}