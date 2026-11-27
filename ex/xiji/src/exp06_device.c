/*
 * 希冀实验六：设备管理 —— SDT → DCT → COCT → CHCT 四级表与分配/回收
 *
 * 课内模型：
 *   SDT（系统设备表）→ 每个设备 DCT → 控制器 COCT → 通道 CHCT；
 *   ApplyDevice() 自下而上检查忙标志：设备忙 → 等设备队列；
 *   否则控制器忙 → 等控制器队列；否则通道忙 → 等通道队列；
 *   都不忙则三级同时置忙，并把进程挂到运行队列。
 *   DeleteDevice() 拒绝不存在或正忙（含仍有等待者）的设备，摘链并释放。
 *
 * 关键点与常见错误：
 *   1. 等待队列是 FIFO，且"第一个等待者"应该在该级释放时被唤醒（这里用显式的
 *      release 函数体现，而不是只把 pid 塞进链表就完事）；
 *   2. 只有真正的"占用者"释放时才清忙标志，重复释放必须报错；
 *   3. 删除设备时如果控制器/通道上还有别的设备，控制器不能被一并回收。
 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

#define DEV_MAX 4
#define CTRL_MAX 2
#define CHAN_MAX 2
#define WAIT_MAX 16

struct waiter {
    int pid;
    int used;
};

struct fifo {
    struct waiter slot[WAIT_MAX];
    int head;
    int tail;
    int count;
};

struct chct {
    int id;
    int busy;
    int holder; /* 占用者 pid，-1 表示空闲 */
    struct fifo q;
};

struct coct {
    int id;
    int busy;
    int holder;
    struct chct *channel;
    struct fifo q;
};

struct dct {
    int id;
    int busy;
    int holder;
    struct coct *controller;
    struct fifo q;
};

struct sdt_node {
    int dev_id;
    struct dct *device;
    struct sdt_node *next;
};

enum apply_result {
    APPLY_OK = 0,
    APPLY_WAIT_DEV = 1,
    APPLY_WAIT_CO = 2,
    APPLY_WAIT_CH = 3,
    APPLY_BAD_DEV = 4,
};

enum delete_result {
    DEL_OK = 0,
    DEL_NOT_FOUND = -1,
    DEL_BUSY = -2,
    DEL_HAS_WAITERS = -3,
};

/* ---------- 全局模型 ---------- */

static struct chct channels[CHAN_MAX];
static struct coct controllers[CTRL_MAX];
static struct dct devices[DEV_MAX];
static struct sdt_node sdt_nodes[DEV_MAX];
static struct sdt_node *sdt_head;
static int chan_n;
static int ctrl_n;
static int dev_n;

/* ---------- FIFO 队列 ---------- */

static void fifo_reset(struct fifo *q)
{
    for (int i = 0; i < WAIT_MAX; i++)
        q->slot[i] = (struct waiter){ 0, 0 };
    q->head = 0;
    q->tail = 0;
    q->count = 0;
}

static int fifo_push(struct fifo *q, int pid)
{
    if (q->count >= WAIT_MAX)
        return -1;
    q->slot[q->tail].pid = pid;
    q->slot[q->tail].used = 1;
    q->tail = (q->tail + 1) % WAIT_MAX;
    q->count++;
    return 0;
}

/* 取出队首等待者；空队列返回 -1 */
static int fifo_pop(struct fifo *q)
{
    int pid;

    if (!q->count)
        return -1;
    pid = q->slot[q->head].pid;
    q->slot[q->head].used = 0;
    q->head = (q->head + 1) % WAIT_MAX;
    q->count--;
    return pid;
}

static int fifo_peek(const struct fifo *q)
{
    return q->count ? q->slot[q->head].pid : -1;
}

static int fifo_ok(const struct fifo *q)
{
    int n = 0;

    if (q->count < 0 || q->count > WAIT_MAX)
        return 0;
    for (int i = 0; i < WAIT_MAX; i++) {
        if (q->slot[i].used)
            n++;
    }
    return n == q->count;
}

/* ---------- 建模 ---------- */

static void model_reset(void)
{
    sdt_head = 0;
    chan_n = 1;
    ctrl_n = 1;
    dev_n = 2;

    channels[0] = (struct chct){ .id = 1, .busy = 0, .holder = -1 };
    fifo_reset(&channels[0].q);

    controllers[0] = (struct coct){ .id = 1, .busy = 0, .holder = -1,
                                    .channel = &channels[0] };
    fifo_reset(&controllers[0].q);

    for (int i = 0; i < dev_n; i++) {
        devices[i] = (struct dct){ .id = i + 1, .busy = 0, .holder = -1,
                                   .controller = &controllers[0] };
        fifo_reset(&devices[i].q);
    }

    /* SDT 逆序挂：dev_id 2 在前，用来验证删除时中间/头部摘链都正确 */
    sdt_nodes[0] = (struct sdt_node){ .dev_id = 2, .device = &devices[1], .next = 0 };
    sdt_nodes[1] = (struct sdt_node){ .dev_id = 1, .device = &devices[0], .next = 0 };
    sdt_nodes[0].next = &sdt_nodes[1];
    sdt_head = &sdt_nodes[0];
}

static struct sdt_node **sdt_find_link(int dev_id)
{
    for (struct sdt_node **pp = &sdt_head; *pp; pp = &(*pp)->next) {
        if ((*pp)->dev_id == dev_id)
            return pp;
    }
    return 0;
}

static struct dct *sdt_find(int dev_id)
{
    struct sdt_node **pp = sdt_find_link(dev_id);

    return pp ? (*pp)->device : 0;
}

static int sdt_count(void)
{
    int n = 0;

    for (const struct sdt_node *p = sdt_head; p; p = p->next)
        n++;
    return n;
}

static int sdt_ok(void)
{
    int n = 0;

    for (const struct sdt_node *p = sdt_head; p; p = p->next) {
        if (!p->device)
            return 0;
        if (++n > DEV_MAX)
            return 0;
    }
    return n == sdt_count();
}

/* ---------- ApplyDevice ---------- */

static enum apply_result apply_device(struct dct *dev, int pid)
{
    struct coct *ctl;
    struct chct *chan;

    if (!dev)
        return APPLY_BAD_DEV;
    ctl = dev->controller;
    if (!ctl)
        return APPLY_BAD_DEV;
    chan = ctl->channel;
    if (!chan)
        return APPLY_BAD_DEV;

    /* 自下而上：设备 → 控制器 → 通道 */
    if (dev->busy) {
        if (fifo_push(&dev->q, pid) != 0)
            return APPLY_BAD_DEV;
        return APPLY_WAIT_DEV;
    }
    if (ctl->busy) {
        if (fifo_push(&ctl->q, pid) != 0)
            return APPLY_BAD_DEV;
        return APPLY_WAIT_CO;
    }
    if (chan->busy) {
        if (fifo_push(&chan->q, pid) != 0)
            return APPLY_BAD_DEV;
        return APPLY_WAIT_CH;
    }

    dev->busy = ctl->busy = chan->busy = 1;
    dev->holder = ctl->holder = chan->holder = pid;
    return APPLY_OK;
}

/*
 * 释放设备：三级同时释放，并把每一级队列的队首唤醒者返回给调用方。
 * 真实的 OS 在这里会把这些进程置为就绪；本实验只关心"唤醒的是不是最早的等待者"，
 * 因此把三级各自的唤醒结果写进 *woken，返回 0 表示释放成功。
 */
struct release_report {
    int dev_pid;
    int ctl_pid;
    int chan_pid;
};

static int release_device(struct dct *dev, int pid, struct release_report *out)
{
    struct coct *ctl = dev->controller;
    struct chct *chan = ctl->channel;

    if (!dev->busy || dev->holder != pid)
        return -1; /* 不是占用者，不能释放 */

    dev->busy = ctl->busy = chan->busy = 0;
    dev->holder = ctl->holder = chan->holder = -1;

    if (out) {
        out->dev_pid = fifo_pop(&dev->q);
        out->ctl_pid = fifo_pop(&ctl->q);
        out->chan_pid = fifo_pop(&chan->q);
    } else {
        (void)fifo_pop(&dev->q);
        (void)fifo_pop(&ctl->q);
        (void)fifo_pop(&chan->q);
    }
    return 0;
}

/* ---------- DeleteDevice ---------- */

static enum delete_result delete_device(int dev_id)
{
    struct sdt_node **pp = sdt_find_link(dev_id);
    struct dct *dev;
    struct coct *ctl;
    int others = 0;

    if (!pp)
        return DEL_NOT_FOUND;
    dev = (*pp)->device;
    if (!dev)
        return DEL_NOT_FOUND;
    if (dev->busy)
        return DEL_BUSY;
    if (dev->q.count)
        return DEL_HAS_WAITERS;

    ctl = dev->controller;
    /* 摘链：pp 指向的是"指向该节点的指针"，头/中/尾一视同仁 */
    *pp = (*pp)->next;

    /* 控制器 / 通道只在没有其它设备引用时才回收 */
    if (ctl) {
        for (const struct sdt_node *p = sdt_head; p; p = p->next) {
            if (p->device && p->device->controller == ctl)
                others++;
        }
        if (others == 0) {
            ctl->busy = 0;
            ctl->holder = -1;
            fifo_reset(&ctl->q);
            if (ctl->channel) {
                ctl->channel->busy = 0;
                ctl->channel->holder = -1;
                fifo_reset(&ctl->channel->q);
            }
        }
    }
    return DEL_OK;
}

/* ---------- 自检 ---------- */

static int waiter_count(const struct fifo *q)
{
    int n = 0;

    for (int i = 0; i < WAIT_MAX; i++) {
        if (q->slot[i].used)
            n++;
    }
    return n;
}

int xiji_exp06_run(void)
{
    struct release_report rep;
    struct dct *d1;
    struct dct *d2;
    struct coct *co;
    struct chct *ch;

    model_reset();
    d1 = sdt_find(1);
    d2 = sdt_find(2);
    co = &controllers[0];
    ch = &channels[0];

    /* --- 基本结构 --- */
    XIJI_CHECK("exp06-sdt", sdt_ok() && sdt_count() == 2);
    XIJI_CHECK("exp06-sdt-order", sdt_head->dev_id == 2);
    XIJI_CHECK("exp06-lookup", d1 && d2 && sdt_find(9) == 0);
    XIJI_CHECK("exp06-topology", d1->controller == co && d2->controller == co &&
                                     co->channel == ch);

    /* --- 分配：三级一起置忙 --- */
    XIJI_CHECK("exp06-apply", apply_device(d1, 100) == APPLY_OK);
    XIJI_CHECK("exp06-busy", d1->busy && co->busy && ch->busy);
    XIJI_CHECK("exp06-holder", d1->holder == 100 && co->holder == 100 && ch->holder == 100);

    /* --- 同一设备再申请 → 等设备队列 --- */
    XIJI_CHECK("exp06-wait-dev", apply_device(d1, 101) == APPLY_WAIT_DEV);
    XIJI_CHECK("exp06-wait-dev-queue", d1->q.count == 1 && fifo_peek(&d1->q) == 101);

    /* --- 换设备但控制器忙 → 等控制器队列 --- */
    XIJI_CHECK("exp06-wait-co", apply_device(d2, 102) == APPLY_WAIT_CO);
    XIJI_CHECK("exp06-wait-co-queue", co->q.count == 1 && fifo_peek(&co->q) == 102);
    XIJI_CHECK("exp06-wait-co-not-dev", d2->q.count == 0);

    /* --- 释放：三级清忙，队列 FIFO 唤醒 --- */
    XIJI_CHECK("exp06-release-wrong", release_device(d1, 999, 0) == -1);
    XIJI_CHECK("exp06-release", release_device(d1, 100, &rep) == 0);
    XIJI_CHECK("exp06-released-idle", !d1->busy && !co->busy && !ch->busy);
    XIJI_CHECK("exp06-wake-order", rep.dev_pid == 101 && rep.ctl_pid == 102 &&
                                       rep.chan_pid == -1);
    XIJI_CHECK("exp06-queues-empty", d1->q.count == 0 && co->q.count == 0 && ch->q.count == 0);
    XIJI_CHECK("exp06-release-twice", release_device(d1, 100, 0) == -1);

    /* --- 通道忙 → 等通道队列（构造成控制器不忙但通道忙的场景） --- */
    model_reset();
    d1 = sdt_find(1);
    d2 = sdt_find(2);
    co = &controllers[0];
    ch = &channels[0];
    ch->busy = 1;
    ch->holder = 900; /* 被别的控制器占用 */
    XIJI_CHECK("exp06-wait-ch", apply_device(d1, 200) == APPLY_WAIT_CH);
    XIJI_CHECK("exp06-wait-ch-queue", ch->q.count == 1 && fifo_peek(&ch->q) == 200);
    XIJI_CHECK("exp06-wait-ch-untouched", !d1->busy && !co->busy);

    /* --- FIFO 等待队列顺序 --- */
    model_reset();
    d1 = sdt_find(1);
    XIJI_CHECK("exp06-fifo-setup", apply_device(d1, 1) == APPLY_OK);
    for (int pid = 10; pid <= 13; pid++)
        XIJI_CHECK("exp06-fifo-push", apply_device(d1, pid) == APPLY_WAIT_DEV);
    XIJI_CHECK("exp06-fifo-head", fifo_peek(&d1->q) == 10 && d1->q.count == 4);
    XIJI_CHECK("exp06-fifo-ok", fifo_ok(&d1->q));
    for (int pid = 10; pid <= 13; pid++)
        XIJI_CHECK("exp06-fifo-pop", fifo_pop(&d1->q) == pid);
    XIJI_CHECK("exp06-fifo-empty", fifo_pop(&d1->q) == -1 && d1->q.count == 0);

    /* --- 删除设备 --- */
    model_reset();
    d1 = sdt_find(1);
    d2 = sdt_find(2);

    d2->busy = 1;
    d2->holder = 7;
    XIJI_CHECK("exp06-del-busy", delete_device(2) == DEL_BUSY);
    d2->busy = 0;
    d2->holder = -1;

    XIJI_CHECK("exp06-del-waiters", (fifo_push(&d2->q, 5) == 0) &&
                                        delete_device(2) == DEL_HAS_WAITERS);
    fifo_reset(&d2->q);
    XIJI_CHECK("exp06-del-waiters-gone", d2->q.count == 0);

    /* 删头部节点（sdt 头是 dev 2） */
    XIJI_CHECK("exp06-del-head", delete_device(2) == DEL_OK && sdt_head &&
                                     sdt_head->dev_id == 1 && sdt_count() == 1);
    XIJI_CHECK("exp06-del-head-gone", sdt_find(2) == 0);
    XIJI_CHECK("exp06-del-twice", delete_device(2) == DEL_NOT_FOUND);
    XIJI_CHECK("exp06-del-unknown", delete_device(99) == DEL_NOT_FOUND);

    /* 删最后一个节点：头指针要变空 */
    XIJI_CHECK("exp06-del-last", delete_device(1) == DEL_OK && sdt_head == 0 &&
                                     sdt_count() == 0);

    /* 中间节点删除：三个设备，删中间那个 */
    model_reset();
    dev_n = 3;
    for (int i = 2; i < dev_n; i++) {
        devices[i] = (struct dct){ .id = i + 1, .busy = 0, .holder = -1,
                                   .controller = &controllers[0] };
        fifo_reset(&devices[i].q);
    }
    sdt_nodes[0] = (struct sdt_node){ .dev_id = 2, .device = &devices[1], .next = 0 };
    sdt_nodes[1] = (struct sdt_node){ .dev_id = 1, .device = &devices[0], .next = 0 };
    sdt_nodes[2] = (struct sdt_node){ .dev_id = 3, .device = &devices[2], .next = 0 };
    sdt_nodes[0].next = &sdt_nodes[1];
    sdt_nodes[1].next = &sdt_nodes[2];
    sdt_head = &sdt_nodes[0];
    XIJI_CHECK("exp06-del-middle-setup", sdt_ok() && sdt_count() == 3);
    XIJI_CHECK("exp06-del-middle", delete_device(1) == DEL_OK && sdt_count() == 2);
    XIJI_CHECK("exp06-del-middle-order", sdt_head->dev_id == 2 && sdt_head->next &&
                                             sdt_head->next->dev_id == 3);
    XIJI_CHECK("exp06-del-middle-ok", sdt_ok());

    /* 释放时控制器还有别的设备 → 控制器不能被回收 */
    model_reset();
    d1 = sdt_find(1);
    XIJI_CHECK("exp06-ctrl-keep-setup", sdt_count() == 2);
    XIJI_CHECK("exp06-ctrl-keep", delete_device(1) == DEL_OK && sdt_count() == 1);
    XIJI_CHECK("exp06-ctrl-keep-alive", controllers[0].channel == &channels[0] &&
                                            sdt_find(2) != 0);

    /* --- 压力：反复申请/释放，队列不变量始终成立 --- */
    model_reset();
    d1 = sdt_find(1);
    d2 = sdt_find(2);
    for (int step = 0; step < 200; step++) {
        struct dct *dev = (step % 2) ? d1 : d2;
        int pid = 1000 + step;
        enum apply_result r = apply_device(dev, pid);

        if (r == APPLY_OK) {
            struct release_report rr;

            if (release_device(dev, pid, &rr) != 0)
                return xiji_fail("exp06-stress", "release");
        }
        if (!fifo_ok(&d1->q) || !fifo_ok(&d2->q))
            return xiji_fail("exp06-stress", "queue invariant");
        if (!sdt_ok())
            return xiji_fail("exp06-stress", "sdt invariant");
        if (d1->busy && d1->holder < 0)
            return xiji_fail("exp06-stress", "busy without holder");
    }
    XIJI_CHECK("exp06-stress-quiescent", !d1->busy && !d2->busy &&
                                             waiter_count(&d1->q) == 0);

    xiji_puts("PASS exp06-device-management\n");
    return 0;
}
