/* 希冀实验六：SDT → DCT → COCT → CHCT 设备分配与删除 */
#include "xiji_runner.h"
#include "../common/xiji_test.h"

struct pcb_wait {
    int pid;
    struct pcb_wait *next;
};

struct chct {
    int id;
    int busy;
    struct pcb_wait *q;
};

struct coct {
    int id;
    int busy;
    struct chct *channel;
    struct pcb_wait *q;
};

struct dct {
    int id;
    int busy;
    struct coct *controller;
    struct pcb_wait *q;
};

struct sdt_node {
    int dev_id;
    struct dct *device;
    struct sdt_node *next;
};

static struct chct ch;
static struct coct co;
static struct dct d1, d2;
static struct sdt_node n1, n2, *sdt;
static struct pcb_wait waiters[8];
static int waiter_used;

static struct pcb_wait *wait_new(int pid)
{
    struct pcb_wait *w = &waiters[waiter_used++];
    w->pid = pid;
    w->next = 0;
    return w;
}

static void wait_enqueue(struct pcb_wait **head, int pid)
{
    struct pcb_wait *w = wait_new(pid);
    w->next = *head;
    *head = w;
}

static void model_init(void)
{
    waiter_used = 0;
    ch = (struct chct){ .id = 1, .busy = 0, .q = 0 };
    co = (struct coct){ .id = 1, .busy = 0, .channel = &ch, .q = 0 };
    d1 = (struct dct){ .id = 1, .busy = 0, .controller = &co, .q = 0 };
    d2 = (struct dct){ .id = 2, .busy = 0, .controller = &co, .q = 0 };
    n1 = (struct sdt_node){ .dev_id = 1, .device = &d1, .next = &n2 };
    n2 = (struct sdt_node){ .dev_id = 2, .device = &d2, .next = 0 };
    sdt = &n1;
}

enum apply_result { APPLY_OK = 0, APPLY_WAIT_DEV, APPLY_WAIT_CO, APPLY_WAIT_CH };

static enum apply_result apply_device(struct dct *dev, int pid)
{
    if (!dev)
        return APPLY_WAIT_DEV;
    if (dev->busy) {
        wait_enqueue(&dev->q, pid);
        return APPLY_WAIT_DEV;
    }
    struct coct *coctl = dev->controller;
    if (coctl->busy) {
        wait_enqueue(&coctl->q, pid);
        return APPLY_WAIT_CO;
    }
    struct chct *chan = coctl->channel;
    if (chan->busy) {
        wait_enqueue(&chan->q, pid);
        return APPLY_WAIT_CH;
    }
    dev->busy = coctl->busy = chan->busy = 1;
    return APPLY_OK;
}

static int delete_device(int dev_id)
{
    struct sdt_node *prev = 0;
    for (struct sdt_node *cur = sdt; cur; prev = cur, cur = cur->next) {
        if (cur->dev_id != dev_id)
            continue;
        struct dct *dev = cur->device;
        if (dev->busy || dev->q)
            return -2;
        if (prev)
            prev->next = cur->next;
        else
            sdt = cur->next;
        return 0;
    }
    return -1;
}

int xiji_exp06_run(void)
{
    model_init();

    XIJI_CHECK("exp06-apply", apply_device(&d1, 100) == APPLY_OK);
    XIJI_CHECK("exp06-busy", d1.busy && co.busy && ch.busy);
    XIJI_CHECK("exp06-contend", apply_device(&d2, 101) == APPLY_WAIT_CO);

    d1.busy = co.busy = ch.busy = 0;

    d2.busy = 1;
    XIJI_CHECK("exp06-del-busy", delete_device(2) == -2);
    d2.busy = 0;

    XIJI_CHECK("exp06-del", delete_device(2) == 0);
    XIJI_CHECK("exp06-find", delete_device(2) == -1);

    xiji_puts("PASS exp06-device-management\n");
    return 0;
}