/* 希冀实验二：FCFS / SJF / RR 就绪队列插入 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

struct proc {
    int id;
    int burst;
    struct proc *link;
};

static struct proc *ready;
static struct proc *pfend;
static struct proc nodes[8];

static void reset(void)
{
    ready = pfend = 0;
}

static struct proc *mk(int id, int burst)
{
    nodes[id].id = id;
    nodes[id].burst = burst;
    nodes[id].link = 0;
    return &nodes[id];
}

static void fcfs(struct proc *p)
{
    if (!ready) {
        ready = pfend = p;
        return;
    }
    pfend->link = p;
    pfend = p;
}

static void sjf_insert(struct proc *p)
{
    if (!ready || p->burst < ready->burst) {
        p->link = ready;
        ready = p;
        if (!p->link)
            pfend = p;
        return;
    }
    struct proc *prev = ready;
    struct proc *cur = ready->link;
    while (cur && cur->burst <= p->burst) {
        prev = cur;
        cur = cur->link;
    }
    prev->link = p;
    p->link = cur;
    if (!cur)
        pfend = p;
}

static void rr_rotate(void)
{
    if (!ready || !ready->link)
        return;
    struct proc *head = ready;
    ready = ready->link;
    head->link = 0;
    pfend->link = head;
    pfend = head;
}

static int collect_ids(int *out, int max)
{
    int n = 0;
    for (struct proc *p = ready; p && n < max; p = p->link)
        out[n++] = p->id;
    return n;
}

int xiji_exp02_run(void)
{
    int ids[8];
    int n;

    reset();
    fcfs(mk(1, 5));
    fcfs(mk(2, 3));
    fcfs(mk(3, 7));
    n = collect_ids(ids, 8);
    XIJI_CHECK("exp02-fcfs", n == 3 && ids[0] == 1 && ids[1] == 2 && ids[2] == 3);

    reset();
    sjf_insert(mk(1, 8));
    sjf_insert(mk(2, 3));
    sjf_insert(mk(3, 5));
    sjf_insert(mk(4, 3));
    n = collect_ids(ids, 8);
    XIJI_CHECK("exp02-sjf", n == 4 && ids[0] == 2 && ids[1] == 4 && ids[2] == 3 && ids[3] == 1);

    reset();
    fcfs(mk(1, 2));
    fcfs(mk(2, 2));
    fcfs(mk(3, 2));
    rr_rotate();
    n = collect_ids(ids, 8);
    XIJI_CHECK("exp02-rr", n == 3 && ids[0] == 2 && ids[1] == 3 && ids[2] == 1);

    xiji_puts("PASS exp02-process-scheduling\n");
    return 0;
}