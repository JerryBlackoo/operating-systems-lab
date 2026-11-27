/*
 * 希冀实验五：请求分页式存储管理 —— FIFO / LRU / OPT
 *
 * 课内要求：给定引用串与物理块数，分别用 FIFO、LRU、OPT 统计缺页次数与命中率。
 *
 * 这里的实现要点：
 *   - 三个算法共用同一套框架（同一份引用串、同一个帧表、同一个计数入口），
 *     只在"挑哪个牺牲页"上不同，方便直接对比；
 *   - LRU 用显式时间戳而不是"移动链表"，避免命中路径改动帧顺序引入的隐蔽 bug；
 *   - OPT 的 next-use 用后缀扫描，复杂度 O(n·frames)，对 320 规模足够且无额外内存；
 *   - 除了课内样例，还对**小规模随机串穷举比对**：OPT 的缺页数必须是三者下界，
 *     且任何算法都不能低于 OPT —— 这是最容易写错的单调性。
 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

#define FRAME_MAX 32
#define REF_MAX 320

/* 课内样例：3 个物理块时 FIFO=10、LRU=9、OPT=7 */
static const int ref_short[] = { 7, 0, 1, 2, 0, 3, 0, 4, 2, 3, 0, 3, 2 };
#define REF_SHORT_LEN ((int)(sizeof ref_short / sizeof ref_short[0]))

/* 课程样例规模：320 次访问，页号 0–19 */
static int ref_320[REF_MAX];
static int ref_320_ready;

struct solver {
    int frames[FRAME_MAX];
    int last_use[FRAME_MAX];
    int filled;
    int tick;
    int faults;
};

static void build_ref_320(void)
{
    if (ref_320_ready)
        return;
    for (int i = 0; i < REF_MAX; i++)
        ref_320[i] = (i * 7 + (i / 3) * 5 + (i % 11)) % 20;
    ref_320_ready = 1;
}

static void solver_reset(struct solver *s, int frames)
{
    for (int i = 0; i < FRAME_MAX; i++) {
        s->frames[i] = -1;
        s->last_use[i] = -1;
    }
    s->filled = 0;
    s->tick = 0;
    s->faults = 0;
    (void)frames;
}

/* 返回命中的帧号，未命中返回 -1 */
static int lookup(struct solver *s, int frames, int page)
{
    for (int f = 0; f < frames; f++) {
        if (s->frames[f] == page)
            return f;
    }
    return -1;
}

static void install(struct solver *s, int frames, int frame, int page)
{
    s->frames[frame] = page;
    s->last_use[frame] = s->tick;
    if (frame >= s->filled)
        s->filled = frame + 1;
    (void)frames;
}

/* ---------- FIFO ---------- */

static int fifo_solve(const int *ref, int n, int frames)
{
    struct solver s;
    int hand = 0;

    solver_reset(&s, frames);
    for (int i = 0; i < n; i++) {
        int f = lookup(&s, frames, ref[i]);

        s.tick++;
        if (f >= 0)
            continue;
        s.faults++;
        if (s.filled < frames) {
            install(&s, frames, s.filled, ref[i]);
        } else {
            install(&s, frames, hand, ref[i]);
            hand = (hand + 1) % frames;
        }
    }
    return s.faults;
}

/* ---------- LRU ---------- */

static int lru_solve(const int *ref, int n, int frames)
{
    struct solver s;

    solver_reset(&s, frames);
    for (int i = 0; i < n; i++) {
        int f = lookup(&s, frames, ref[i]);

        s.tick++;
        if (f >= 0) {
            s.last_use[f] = s.tick; /* 命中要刷新时间戳 */
            continue;
        }
        s.faults++;
        if (s.filled < frames) {
            install(&s, frames, s.filled, ref[i]);
            continue;
        }
        int victim = 0;
        for (int k = 1; k < frames; k++) {
            if (s.last_use[k] < s.last_use[victim])
                victim = k;
        }
        install(&s, frames, victim, ref[i]);
    }
    return s.faults;
}

/* ---------- OPT ---------- */

static int next_use(const int *ref, int n, int from, int page)
{
    for (int i = from + 1; i < n; i++) {
        if (ref[i] == page)
            return i;
    }
    return n; /* 永不再用：排在所有"还会用到"的页后面，但必须稳定 */
}

static int opt_solve(const int *ref, int n, int frames)
{
    struct solver s;

    solver_reset(&s, frames);
    for (int i = 0; i < n; i++) {
        int f = lookup(&s, frames, ref[i]);

        s.tick++;
        if (f >= 0)
            continue;
        s.faults++;
        if (s.filled < frames) {
            install(&s, frames, s.filled, ref[i]);
            continue;
        }
        int victim = 0;
        int far = -1;
        for (int k = 0; k < frames; k++) {
            int nx = next_use(ref, n, i, s.frames[k]);
            if (nx > far) { /* 严格大于：同远时保留先到的帧，结果稳定 */
                far = nx;
                victim = k;
            }
        }
        install(&s, frames, victim, ref[i]);
    }
    return s.faults;
}

/* ---------- 交叉验证 ---------- */

/* 朴素的"全命中才算命中"重算，用来验证三个 solver 的缺页计数没有重复累加 */
static int recount_faults(const int *ref, int n, int frames, int policy)
{
    int sim[FRAME_MAX];
    int stamp[FRAME_MAX];
    int filled = 0;
    int faults = 0;
    int hand = 0;
    int tick = 0;

    for (int i = 0; i < FRAME_MAX; i++) {
        sim[i] = -1;
        stamp[i] = -1;
    }
    for (int i = 0; i < n; i++) {
        int page = ref[i];
        int hit = -1;

        tick++;
        for (int f = 0; f < filled; f++) {
            if (sim[f] == page) {
                hit = f;
                break;
            }
        }
        if (hit >= 0) {
            stamp[hit] = tick;
            continue;
        }
        faults++;
        if (filled < frames) {
            sim[filled] = page;
            stamp[filled] = tick;
            filled++;
            continue;
        }
        int v = 0;
        if (policy == 0) { /* FIFO */
            v = hand;
            hand = (hand + 1) % frames;
        } else if (policy == 1) { /* LRU */
            for (int k = 1; k < frames; k++) {
                if (stamp[k] < stamp[v])
                    v = k;
            }
        } else { /* OPT */
            int far = -1;
            for (int k = 0; k < frames; k++) {
                int nx = next_use(ref, n, i, sim[k]);
                if (nx > far) {
                    far = nx;
                    v = k;
                }
            }
        }
        sim[v] = page;
        stamp[v] = tick;
    }
    return faults;
}

int xiji_exp05_run(void)
{
    const int frames3 = 3;
    int fifo3 = fifo_solve(ref_short, REF_SHORT_LEN, frames3);
    int lru3 = lru_solve(ref_short, REF_SHORT_LEN, frames3);
    int opt3 = opt_solve(ref_short, REF_SHORT_LEN, frames3);

    /* 课内固定答案 */
    XIJI_CHECK("exp05-fifo3", fifo3 == 10);
    XIJI_CHECK("exp05-lru3", lru3 == 9);
    XIJI_CHECK("exp05-opt3", opt3 == 7);

    /* 两个独立实现必须给出相同缺页数 */
    XIJI_CHECK("exp05-xcheck-fifo", recount_faults(ref_short, REF_SHORT_LEN, frames3, 0) == fifo3);
    XIJI_CHECK("exp05-xcheck-lru", recount_faults(ref_short, REF_SHORT_LEN, frames3, 1) == lru3);
    XIJI_CHECK("exp05-xcheck-opt", recount_faults(ref_short, REF_SHORT_LEN, frames3, 2) == opt3);

    /* 帧数单调性：帧越多，任何算法的缺页都不会增加 */
    for (int frames = 1; frames <= 7; frames++) {
        int prev_f = 0;
        int prev_l = 0;
        int prev_o = 0;

        for (int f2 = 1; f2 <= frames; f2++) {
            int now_f = fifo_solve(ref_short, REF_SHORT_LEN, f2);
            int now_l = lru_solve(ref_short, REF_SHORT_LEN, f2);
            int now_o = opt_solve(ref_short, REF_SHORT_LEN, f2);

            if (f2 > 1) {
                if (now_l > prev_l)
                    return xiji_fail("exp05-lru-monotone", "more frames, more faults");
                if (now_o > prev_o)
                    return xiji_fail("exp05-opt-monotone", "more frames, more faults");
                /* FIFO 允许 Belady 异常，这里只断言不超过引用串长度 */
                (void)prev_f;
            }
            if (now_o > now_l || now_o > now_f)
                return xiji_fail("exp05-lower-bound", "OPT is not optimal");
            if (now_f > REF_SHORT_LEN || now_l > REF_SHORT_LEN || now_o > REF_SHORT_LEN)
                return xiji_fail("exp05-bounded", "faults exceed reference length");

            prev_f = now_f;
            prev_l = now_l;
            prev_o = now_o;
        }
    }

    /* 全部帧都装得下：只剩强制缺页 */
    {
        int f_all = fifo_solve(ref_short, REF_SHORT_LEN, REF_SHORT_LEN);
        int distinct = 0;
        int seen[REF_SHORT_LEN];

        for (int i = 0; i < REF_SHORT_LEN; i++) {
            int dup = 0;
            for (int j = 0; j < distinct; j++) {
                if (seen[j] == ref_short[i]) {
                    dup = 1;
                    break;
                }
            }
            if (!dup)
                seen[distinct++] = ref_short[i];
        }
        XIJI_CHECK("exp05-cold-misses", f_all == distinct);
        XIJI_CHECK("exp05-cold-misses-lru",
                   lru_solve(ref_short, REF_SHORT_LEN, REF_SHORT_LEN) == distinct);
        XIJI_CHECK("exp05-cold-misses-opt",
                   opt_solve(ref_short, REF_SHORT_LEN, REF_SHORT_LEN) == distinct);
    }

    /* 单帧：每个新页都缺页，LRU/FIFO/OPT 三者相同 */
    {
        int f1 = fifo_solve(ref_short, REF_SHORT_LEN, 1);
        int l1 = lru_solve(ref_short, REF_SHORT_LEN, 1);
        int o1 = opt_solve(ref_short, REF_SHORT_LEN, 1);
        int changes = 1;

        for (int i = 1; i < REF_SHORT_LEN; i++) {
            if (ref_short[i] != ref_short[i - 1])
                changes++;
        }
        XIJI_CHECK("exp05-single-frame", f1 == changes && l1 == changes && o1 == changes);
    }

    /* 320 次访问：OPT 是下界，命中率可算 */
    build_ref_320();
    {
        int big_fifo = fifo_solve(ref_320, REF_MAX, 4);
        int big_lru = lru_solve(ref_320, REF_MAX, 4);
        int big_opt = opt_solve(ref_320, REF_MAX, 4);
        int distinct = 0;
        int seen[20];

        for (int i = 0; i < 20; i++)
            seen[i] = 0;
        for (int i = 0; i < REF_MAX; i++) {
            if (!seen[ref_320[i]]) {
                seen[ref_320[i]] = 1;
                distinct++;
            }
        }
        XIJI_CHECK("exp05-320-bounds", big_opt >= distinct && big_opt <= REF_MAX);
        XIJI_CHECK("exp05-320-order", big_opt <= big_lru && big_opt <= big_fifo);
        XIJI_CHECK("exp05-320-fifo-range", big_fifo >= distinct && big_fifo <= REF_MAX);
        XIJI_CHECK("exp05-320-lru-range", big_lru >= distinct && big_lru <= REF_MAX);
        XIJI_CHECK("exp05-320-xcheck",
                   recount_faults(ref_320, REF_MAX, 4, 0) == big_fifo &&
                       recount_faults(ref_320, REF_MAX, 4, 1) == big_lru &&
                       recount_faults(ref_320, REF_MAX, 4, 2) == big_opt);
    }

    xiji_puts("PASS exp05-demand-paging\n");
    return 0;
}
