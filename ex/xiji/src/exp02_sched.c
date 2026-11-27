/*
 * 希冀实验二：进程调度 —— FCFS / SJF / RR
 *
 * 课内要求：三种策略共用同一个就绪队列与同一个 runner，只替换"新进程插到哪"。
 * 因此这里把策略做成函数指针表，插入规则各自一个函数，测试对同一批进程跑三种
 * 策略，比较最终出队顺序与平均周转时间。
 *
 * 关键坑（原实现踩到）：
 *   1. 队列只有一个节点时，插入队首后 tail 必须同步，否则 tail 悬空；
 *   2. SJF 是"最短作业优先"，同 burst 时应保持到达顺序稳定（stable）；
 *   3. RR 轮转后 tail 必须前移，且队列为空/单节点时不能乱转。
 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

#define NODE_MAX 8

struct proc {
    int id;
    int burst;   /* 需要运行的时钟数 */
    int remain;  /* 剩余时间 */
    int arrival;
    struct proc *link;
};

struct runq {
    struct proc *head;
    struct proc *tail;
    int count;
};

static struct runq rq;
static struct proc nodes[NODE_MAX];

/* ---------- 队列原语 ---------- */

static void q_push_tail(struct runq *q, struct proc *p)
{
    p->link = 0;
    if (!q->head)
        q->head = p;
    else
        q->tail->link = p;
    q->tail = p;
    q->count++;
}

static struct proc *q_pop_head(struct runq *q)
{
    struct proc *p = q->head;

    if (!p)
        return 0;
    q->head = p->link;
    if (!q->head)
        q->tail = 0;
    p->link = 0;
    q->count--;
    return p;
}

static int q_ok(const struct runq *q)
{
    const struct proc *p = q->head;
    int n = 0;

    if (!q->head)
        return q->tail == 0 && q->count == 0;
    if (!q->tail || q->tail->link)
        return 0;
    for (; p; p = p->link) {
        if (++n > NODE_MAX)
            return 0;
    }
    return n == q->count;
}

/* ---------- 三种插入策略 ---------- */

/* FCFS：直接接到队尾 */
static void fcfs_insert(struct proc *p)
{
    q_push_tail(&rq, p);
}

/* SJF：按 burst 升序插入，相等时排在已存在的同 burst 进程之后（稳定） */
static void sjf_insert(struct proc *p)
{
    struct proc *prev = 0;
    struct proc *cur = rq.head;

    while (cur && cur->burst <= p->burst) {
        prev = cur;
        cur = cur->link;
    }
    p->link = cur;
    if (prev)
        prev->link = p;
    else
        rq.head = p;
    if (!cur)
        rq.tail = p; /* 插到末尾（含空队列）时维护 tail */
    rq.count++;
}

/* RR：时间片用完后把队首挪到队尾 */
static void rr_rotate(void)
{
    struct proc *head = q_pop_head(&rq);

    if (head)
        q_push_tail(&rq, head);
}

/* ---------- 模拟器 ---------- */

struct sim_result {
    int order[NODE_MAX];
    int order_len;
    int turnaround_sum;
};

static void run_reset(void)
{
    rq = (struct runq){ 0, 0, 0 };
    for (int i = 0; i < NODE_MAX; i++)
        nodes[i] = (struct proc){ 0, 0, 0, 0, 0 };
}

static struct proc *mk(int id, int burst, int arrival)
{
    struct proc *p = &nodes[id];

    p->id = id;
    p->burst = burst;
    p->remain = burst;
    p->arrival = arrival;
    p->link = 0;
    return p;
}

static void sim_fcfs(struct sim_result *out)
{
    int now = 0;

    out->order_len = 0;
    out->turnaround_sum = 0;
    while (rq.count) {
        struct proc *p = q_pop_head(&rq);
        now += p->remain;
        p->remain = 0;
        out->turnaround_sum += now - p->arrival;
        out->order[out->order_len++] = p->id;
    }
}

static void sim_sjf(struct sim_result *out)
{
    /* 非抢占 SJF：队列本身已按 burst 有序，顺序执行即可 */
    sim_fcfs(out);
}

static void sim_rr(struct sim_result *out, int quantum)
{
    int now = 0;

    out->order_len = 0;
    out->turnaround_sum = 0;
    while (rq.count) {
        struct proc *p = q_pop_head(&rq);
        int slice = p->remain < quantum ? p->remain : quantum;

        p->remain -= slice;
        now += slice;
        if (p->remain == 0) {
            out->turnaround_sum += now - p->arrival;
            out->order[out->order_len++] = p->id;
        } else {
            q_push_tail(&rq, p);
        }
    }
}

static int order_is(const struct sim_result *r, const int *want, int n)
{
    if (r->order_len != n)
        return 0;
    for (int i = 0; i < n; i++) {
        if (r->order[i] != want[i])
            return 0;
    }
    return 1;
}

int xiji_exp02_run(void)
{
    const int fcfs_want[] = { 1, 2, 3, 4 };
    const int sjf_want[] = { 2, 4, 3, 1 };
    const int rr_want[] = { 2, 4, 1, 3 };
    struct sim_result res;

    /* FCFS：按到达顺序服务 */
    run_reset();
    fcfs_insert(mk(1, 8, 0));
    fcfs_insert(mk(2, 4, 0));
    fcfs_insert(mk(3, 9, 0));
    fcfs_insert(mk(4, 5, 0));
    XIJI_CHECK("exp02-fcfs-queue", q_ok(&rq) && rq.count == 4 && rq.tail->id == 4);
    sim_fcfs(&res);
    XIJI_CHECK("exp02-fcfs-order", order_is(&res, fcfs_want, 4));
    XIJI_CHECK("exp02-fcfs-turnaround", res.turnaround_sum == (8 + 12 + 21 + 26));

    /* SJF：短作业先，同 burst 稳定 */
    run_reset();
    sjf_insert(mk(1, 8, 0));
    sjf_insert(mk(2, 3, 0));
    sjf_insert(mk(3, 5, 0));
    sjf_insert(mk(4, 3, 0));
    XIJI_CHECK("exp02-sjf-queue", q_ok(&rq) && rq.count == 4);
    XIJI_CHECK("exp02-sjf-tail", rq.tail && rq.tail->id == 1);
    sim_sjf(&res);
    XIJI_CHECK("exp02-sjf-order", order_is(&res, sjf_want, 4));
    /* SJF 的平均周转必须不劣于 FCFS */
    XIJI_CHECK("exp02-sjf-better", res.turnaround_sum < (8 + 12 + 21 + 26));

    /* SJF 边界：插入比队首更短的进程，tail 不能被错误覆盖 */
    run_reset();
    sjf_insert(mk(1, 10, 0));
    sjf_insert(mk(2, 1, 0));
    XIJI_CHECK("exp02-sjf-front", q_ok(&rq) && rq.head->id == 2 &&
                                      rq.tail && rq.tail->id == 1 &&
                                      rq.count == 2);
    /* 单节点队列插入更短进程：head 与 tail 都要更新 */
    run_reset();
    sjf_insert(mk(1, 5, 0));
    sjf_insert(mk(2, 2, 0));
    XIJI_CHECK("exp02-sjf-single", q_ok(&rq) && rq.head->id == 2 &&
                                       rq.tail && rq.tail->id == 1);

    /* SJF 空队列 */
    run_reset();
    sjf_insert(mk(1, 4, 0));
    XIJI_CHECK("exp02-sjf-empty", q_ok(&rq) && rq.head == rq.tail &&
                                      rq.head->id == 1 && rq.count == 1);

    /* RR：时间片 3，验证轮转顺序与剩余时间收敛 */
    run_reset();
    fcfs_insert(mk(1, 8, 0));
    fcfs_insert(mk(2, 4, 0));
    fcfs_insert(mk(3, 9, 0));
    fcfs_insert(mk(4, 5, 0));
    sim_rr(&res, 3);
    XIJI_CHECK("exp02-rr-order", order_is(&res, rr_want, 4));
    XIJI_CHECK("exp02-rr-drained", rq.count == 0 && rq.head == 0 && rq.tail == 0);

    /* RR 单节点：轮转不得改变队列，也不得自我成环 */
    run_reset();
    fcfs_insert(mk(1, 2, 0));
    rr_rotate();
    XIJI_CHECK("exp02-rr-single", q_ok(&rq) && rq.count == 1 &&
                                      rq.head == rq.tail && rq.head->id == 1);
    rr_rotate();
    XIJI_CHECK("exp02-rr-single2", q_ok(&rq) && rq.count == 1 && rq.head->id == 1);

    /* RR 空队列：不能解引用 null */
    run_reset();
    rr_rotate();
    XIJI_CHECK("exp02-rr-empty", q_ok(&rq) && rq.count == 0 && rq.tail == 0);

    xiji_puts("PASS exp02-process-scheduling\n");
    return 0;
}
