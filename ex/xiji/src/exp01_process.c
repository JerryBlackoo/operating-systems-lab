/*
 * 希冀实验一：进程管理 —— 就绪队列与阻塞队列之间的 block / wake
 *
 * 课内要求：
 *   block()：把就绪队首移到阻塞队尾，state 置 'b'；
 *   wake() ：把阻塞队首移到就绪队尾，state 置 'R'；
 *   两个队列都是 head/tail 双指针，空队列必须同时清 tail。
 *
 * 这里把队列抽成 struct queue 统一维护 head/tail/count：
 * 典型错误是"只改 head 忘了 tail"，一旦队列抽空，tail 变成悬空指针，
 * 下次入队就写到已经释放的 PCB 上。计数 + 结构不变量让这类错误立刻暴露。
 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

#define POOL_MAX 16

struct pcb {
    int id;
    char state; /* 'R' = ready, 'b' = blocked */
    struct pcb *link;
};

struct queue {
    struct pcb *head;
    struct pcb *tail;
    int count;
};

static struct queue ready_q;
static struct queue blocked_q;
static struct pcb pool[POOL_MAX];
static int pool_used;

/* ---------- 队列原语 ---------- */

static void q_push(struct queue *q, struct pcb *p)
{
    p->link = 0;
    if (!q->head)
        q->head = p;
    else
        q->tail->link = p;
    q->tail = p;
    q->count++;
}

static struct pcb *q_pop(struct queue *q)
{
    struct pcb *p = q->head;
    if (!p)
        return 0;
    q->head = p->link;
    if (!q->head)
        q->tail = 0;
    p->link = 0;
    q->count--;
    return p;
}

static int q_ok(const struct queue *q)
{
    const struct pcb *p = q->head;
    int n = 0;

    if (!q->head)
        return q->tail == 0 && q->count == 0;
    if (!q->tail || q->tail->link)
        return 0;
    for (; p; p = p->link) {
        if (++n > POOL_MAX)
            return 0; /* 环 */
    }
    return n == q->count;
}

static int queues_ok(void)
{
    return q_ok(&ready_q) && q_ok(&blocked_q);
}

static struct pcb *pcb_new(int id)
{
    struct pcb *p;

    if (pool_used >= POOL_MAX)
        return 0;
    p = &pool[pool_used++];
    p->id = id;
    p->state = 'R';
    p->link = 0;
    return p;
}

static void ready_enqueue(struct pcb *p)
{
    p->state = 'R';
    q_push(&ready_q, p);
}

/* ---------- 课内要填的两个函数 ---------- */

static int block(void)
{
    struct pcb *p = q_pop(&ready_q);

    if (!p)
        return -1;
    p->state = 'b';
    q_push(&blocked_q, p);
    return 0;
}

static int wake(void)
{
    struct pcb *p = q_pop(&blocked_q);

    if (!p)
        return -1;
    p->state = 'R';
    q_push(&ready_q, p);
    return 0;
}

/* 按 pid 阻塞（真实内核里通常由事件对象指定等待者，而不是"队首"） */
static int block_pid(int id)
{
    struct pcb *prev = 0;
    struct pcb *p = ready_q.head;

    while (p && p->id != id) {
        prev = p;
        p = p->link;
    }
    if (!p)
        return -1;
    if (prev)
        prev->link = p->link;
    else
        ready_q.head = p->link;
    if (ready_q.tail == p)
        ready_q.tail = prev;
    ready_q.count--;
    p->link = 0;
    p->state = 'b';
    q_push(&blocked_q, p);
    return 0;
}

/* ---------- 自检 ---------- */

static int count_ids(const struct queue *q, int *out, int max)
{
    int n = 0;

    for (const struct pcb *p = q->head; p && n < max; p = p->link)
        out[n++] = p->id;
    return n;
}

static int ids_are(const struct queue *q, const int *want, int n)
{
    int got[POOL_MAX];
    int have = count_ids(q, got, POOL_MAX);

    if (have != n)
        return 0;
    for (int i = 0; i < n; i++) {
        if (got[i] != want[i])
            return 0;
    }
    return 1;
}

static void run_reset(void)
{
    pool_used = 0;
    ready_q = (struct queue){ 0, 0, 0 };
    blocked_q = (struct queue){ 0, 0, 0 };
    for (int i = 0; i < POOL_MAX; i++)
        pool[i] = (struct pcb){ 0, 0, 0 };
}

int xiji_exp01_run(void)
{
    const int after_block[] = { 2, 3, 4, 5 };
    const int after_drain[] = { 2, 3, 4, 5, 1 };
    const int ring[] = { 1, 3 };

    /* 空队列上的两个错误路径：不能碰 head/tail，更不能崩 */
    run_reset();
    XIJI_CHECK("exp01-block-empty", block() == -1 && queues_ok());
    XIJI_CHECK("exp01-wake-empty", wake() == -1 && queues_ok());

    for (int i = 1; i <= 5; i++) {
        struct pcb *p = pcb_new(i);
        XIJI_CHECK("exp01-pool", p != 0);
        ready_enqueue(p);
    }
    XIJI_CHECK("exp01-enqueue", queues_ok() && ready_q.count == 5);
    XIJI_CHECK("exp01-enqueue-tail", ready_q.tail && ready_q.tail->id == 5);

    /* block：就绪队首 1 进阻塞队尾 */
    XIJI_CHECK("exp01-block", block() == 0 && queues_ok());
    XIJI_CHECK("exp01-block-ready", ids_are(&ready_q, after_block, 4));
    XIJI_CHECK("exp01-block-state", blocked_q.head && blocked_q.head->id == 1 &&
                                        blocked_q.head->state == 'b');
    XIJI_CHECK("exp01-block-counts", ready_q.count == 4 && blocked_q.count == 1);

    /* 再 block 一次，阻塞队列变 [1,2] */
    XIJI_CHECK("exp01-block2", block() == 0 && blocked_q.count == 2 &&
                                   blocked_q.tail && blocked_q.tail->id == 2);

    /* wake：阻塞队首 1 进就绪队尾 */
    XIJI_CHECK("exp01-wake", wake() == 0 && queues_ok());
    XIJI_CHECK("exp01-wake-tail", ready_q.tail && ready_q.tail->id == 1 &&
                                      ready_q.tail->state == 'R');
    XIJI_CHECK("exp01-wake-head", blocked_q.head && blocked_q.head->id == 2);

    /* 抽干就绪队列：3,4,5,1 依次进阻塞队列尾 */
    while (ready_q.count)
        XIJI_CHECK("exp01-drain", block() == 0 && queues_ok());
    XIJI_CHECK("exp01-ready-empty", block() == -1 && ready_q.tail == 0);
    XIJI_CHECK("exp01-blocked-fifo", blocked_q.count == 5 && blocked_q.tail &&
                                         blocked_q.tail->id == 1);

    /* 全部唤醒，顺序必须保持 FIFO：2,3,4,5,1 */
    while (blocked_q.count)
        XIJI_CHECK("exp01-restore", wake() == 0 && queues_ok());
    XIJI_CHECK("exp01-blocked-empty", wake() == -1 && blocked_q.tail == 0);
    XIJI_CHECK("exp01-restored", ids_are(&ready_q, after_drain, 5));

    /* 指定 pid 阻塞：从中间摘除，尾指针要跟着走 */
    run_reset();
    for (int i = 1; i <= 3; i++)
        ready_enqueue(pcb_new(i));
    XIJI_CHECK("exp01-block-pid", block_pid(2) == 0 && queues_ok());
    XIJI_CHECK("exp01-block-pid-order", ids_are(&ready_q, ring, 2));
    XIJI_CHECK("exp01-block-pid-miss", block_pid(9) == -1);
    XIJI_CHECK("exp01-block-pid-tail", ready_q.tail && ready_q.tail->id == 3);

    /* 守恒：任意时刻 ready + blocked == 已创建的进程数 */
    run_reset();
    for (int i = 1; i <= POOL_MAX; i++)
        ready_enqueue(pcb_new(i));
    for (int step = 0; step < 200; step++) {
        int want_block = (step * 7 + 3) % 5 < 3;

        if (want_block && ready_q.count) {
            if (block() != 0)
                return xiji_fail("exp01-stress", "block failed");
        } else if (!want_block && blocked_q.count) {
            if (wake() != 0)
                return xiji_fail("exp01-stress", "wake failed");
        }
        if (!queues_ok())
            return xiji_fail("exp01-stress", "queue invariant");
        if (ready_q.count + blocked_q.count != POOL_MAX)
            return xiji_fail("exp01-stress", "pcb lost");
        for (const struct pcb *p = ready_q.head; p; p = p->link) {
            if (p->state != 'R')
                return xiji_fail("exp01-stress", "ready state");
        }
        for (const struct pcb *p = blocked_q.head; p; p = p->link) {
            if (p->state != 'b')
                return xiji_fail("exp01-stress", "blocked state");
        }
    }
    /* 压力结束后必须仍能完整抽干（无环、无丢失）：
     * 先把阻塞队列全部唤醒，再全部阻塞，最后再全部唤醒，
     * 每一步的数量都必须守恒。 */
    {
        while (blocked_q.count) {
            if (wake() != 0)
                return xiji_fail("exp01-stress", "drain wake");
            if (!queues_ok() || ready_q.count + blocked_q.count != POOL_MAX)
                return xiji_fail("exp01-stress", "drain invariant");
        }
        while (ready_q.count) {
            if (block() != 0)
                return xiji_fail("exp01-stress", "drain block");
            if (!queues_ok() || ready_q.count + blocked_q.count != POOL_MAX)
                return xiji_fail("exp01-stress", "drain invariant2");
        }
        /* 此刻全部在阻塞队列里，数量必须是满池 */
        if (blocked_q.count != POOL_MAX)
            return xiji_fail("exp01-stress", "lost pcb");
        while (blocked_q.count) {
            if (wake() != 0)
                return xiji_fail("exp01-stress", "drain wake2");
        }
        if (ready_q.count != POOL_MAX || blocked_q.count != 0)
            return xiji_fail("exp01-stress", "drain total");
    }

    xiji_puts("PASS exp01-process-management\n");
    return 0;
}
