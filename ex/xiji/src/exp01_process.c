/* 希冀实验一：就绪 / 阻塞队列上的 block、wake */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

struct pcb {
    char state;
    struct pcb *link;
    int id;
};

static struct pcb *ready;
static struct pcb *ready_tail;
static struct pcb *blocked;
static struct pcb *blocked_tail;

static struct pcb pool[8];
static int pool_used;

static struct pcb *pcb_new(int id)
{
    struct pcb *p = &pool[pool_used++];
    p->id = id;
    p->state = 'R';
    p->link = 0;
    return p;
}

static void ready_enqueue(struct pcb *p)
{
    p->state = 'R';
    p->link = 0;
    if (!ready) {
        ready = ready_tail = p;
        return;
    }
    ready_tail->link = p;
    ready_tail = p;
}

static int block(void)
{
    if (!ready)
        return -1;
    struct pcb *p = ready;
    ready = ready->link;
    if (!ready)
        ready_tail = 0;

    if (!blocked) {
        blocked = blocked_tail = p;
    } else {
        blocked_tail->link = p;
        blocked_tail = p;
    }
    p->state = 'b';
    p->link = 0;
    return 0;
}

static int wake(void)
{
    if (!blocked)
        return -1;
    struct pcb *p = blocked;
    blocked = blocked->link;
    if (!blocked)
        blocked_tail = 0;

    if (!ready) {
        ready = ready_tail = p;
    } else {
        ready_tail->link = p;
        ready_tail = p;
    }
    p->state = 'R';
    p->link = 0;
    return 0;
}

int xiji_exp01_run(void)
{
    pool_used = 0;
    ready = ready_tail = blocked = blocked_tail = 0;

    ready_enqueue(pcb_new(1));
    ready_enqueue(pcb_new(2));
    ready_enqueue(pcb_new(3));

    XIJI_CHECK("exp01", block() == 0 && ready && ready->id == 2);
    XIJI_CHECK("exp01", blocked && blocked->id == 1 && blocked->state == 'b');

    XIJI_CHECK("exp01", block() == 0 && ready->id == 3);
    XIJI_CHECK("exp01", blocked_tail->id == 2);

    XIJI_CHECK("exp01", wake() == 0 && ready_tail->id == 1 && ready_tail->state == 'R');
    XIJI_CHECK("exp01", blocked->id == 2);

    XIJI_CHECK("exp01", wake() == 0 && ready_tail->id == 2);
    XIJI_CHECK("exp01", !blocked);

    XIJI_CHECK("exp01", wake() == -1);

    xiji_puts("PASS exp01-process-management\n");
    return 0;
}