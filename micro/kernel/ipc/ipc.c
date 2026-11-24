#include "ipc.h"

#include <stddef.h>
#include <stdint.h>

#include "object.h"
#include "sched.h"

static struct endpoint endpoints[IPC_MAX_ENDPOINTS];
static struct reply replies[IPC_MAX_REPLIES];

static void memzero(void *dst, size_t len)
{
    uint8_t *p = (uint8_t *)dst;

    while (len-- > 0) {
        *p++ = 0;
    }
}

/* 在持有 sched_lock() 时唤醒整个队列，并给每个等待者写入返回码。 */
static void wake_all_locked(struct waitq *wq, int status)
{
    struct thread *t;

    while ((t = waitq_pop(wq)) != 0) {
        t->ipc_status = status;
        thread_wake_locked(t);
    }
}

static void ep_update_state(struct endpoint *ep)
{
    if (ep->state == EP_DYING) {
        return;
    }
    if (!waitq_empty(&ep->recvq)) {
        ep->state = EP_RECV_WAIT;
    } else if (!waitq_empty(&ep->sendq)) {
        ep->state = EP_SEND_WAIT;
    } else {
        ep->state = EP_IDLE;
    }
}

static void ep_destroy(struct object *obj)
{
    struct endpoint *ep = (struct endpoint *)obj;
    uint64_t flags;

    flags = sched_lock();

    /*
     * 先标记 DYING，再唤醒 sendq/recvq；此后的 ep_send/ep_recv 会
     * 直接返回 IPC_EDEAD，不会再有人进入队列。
     */
    ep->state = EP_DYING;
    wake_all_locked(&ep->sendq, IPC_EDEAD);
    wake_all_locked(&ep->recvq, IPC_EDEAD);

    /*
     * 已交付但尚未回复的 call 不在这两个队列里：按 reply 记录的所有者
     * 找到它们，标记 aborted 并唤醒调用者。之后到达的 ep_reply 返回
     * IPC_EDEAD，避免把回复投递给已经放弃等待的线程。
     */
    for (int i = 0; i < IPC_MAX_REPLIES; i++) {
        struct reply *rep = &replies[i];

        if (rep->used && rep->ep == ep && !rep->done && !rep->aborted) {
            rep->aborted = true;
            wake_all_locked(&rep->waitq, IPC_EDEAD);
        }
    }

    ep->used = false;
    sched_unlock(flags);
}

void ipc_init(void)
{
    memzero(endpoints, sizeof(endpoints));
    memzero(replies, sizeof(replies));
}

struct endpoint *ep_create(void)
{
    struct endpoint *ep = 0;
    uint64_t flags;

    flags = sched_lock();
    for (int i = 0; i < IPC_MAX_ENDPOINTS; i++) {
        if (!endpoints[i].used) {
            ep = &endpoints[i];
            break;
        }
    }
    if (ep == 0) {
        sched_unlock(flags);
        return 0;
    }

    memzero(ep, sizeof(*ep));
    ep->used = true;
    ep->state = EP_IDLE;
    waitq_init(&ep->sendq);
    waitq_init(&ep->recvq);

    if (object_register(&ep->obj, OBJ_ENDPOINT, ep_destroy) < 0) {
        ep->used = false;
        sched_unlock(flags);
        return 0;
    }
    sched_unlock(flags);
    return ep;
}

void ep_ref(struct endpoint *ep)
{
    if (ep != 0) {
        object_ref(&ep->obj);
    }
}

void ep_unref(struct endpoint *ep)
{
    if (ep != 0) {
        object_unref(&ep->obj);
    }
}

uint32_t ep_id(const struct endpoint *ep)
{
    return ep ? object_get_id(&ep->obj) : 0;
}

int ep_send(struct endpoint *ep, const ipc_message *msg)
{
    struct thread *self = sched_current();
    struct thread *receiver;
    uint64_t flags;

    if (ep == 0 || msg == 0 || self == 0) {
        return IPC_EINVAL;
    }

    flags = sched_lock();
    if (ep->state == EP_DYING) {
        sched_unlock(flags);
        return IPC_EDEAD;
    }
    if (self->state != THREAD_RUNNING) {
        /* 只有运行中的线程可以进入阻塞路径，否则会破坏队列不变量。 */
        sched_unlock(flags);
        return IPC_EINVAL;
    }

    receiver = waitq_pop(&ep->recvq);
    if (receiver != 0) {
        receiver->msg = *msg;
        receiver->ipc_status = IPC_OK;
        thread_wake_locked(receiver);
        ep_update_state(ep);
        sched_unlock(flags);
        return IPC_OK;
    }

    self->msg = *msg;
    self->ipc_status = IPC_PENDING;
    thread_block_locked(&ep->sendq);
    ep_update_state(ep);
    sched_unlock(flags);

    sched();
    return self->ipc_status;
}

int ep_recv(struct endpoint *ep, ipc_message *msg)
{
    struct thread *self = sched_current();
    struct thread *sender;
    uint64_t flags;

    if (ep == 0 || msg == 0 || self == 0) {
        return IPC_EINVAL;
    }

    flags = sched_lock();
    if (ep->state == EP_DYING) {
        sched_unlock(flags);
        return IPC_EDEAD;
    }
    if (self->state != THREAD_RUNNING) {
        sched_unlock(flags);
        return IPC_EINVAL;
    }

    sender = waitq_pop(&ep->sendq);
    if (sender != 0) {
        *msg = sender->msg;
        sender->ipc_status = IPC_OK;
        thread_wake_locked(sender);
        ep_update_state(ep);
        sched_unlock(flags);
        return IPC_OK;
    }

    self->ipc_status = IPC_PENDING;
    thread_block_locked(&ep->recvq);
    ep_update_state(ep);
    sched_unlock(flags);

    sched();

    if (self->ipc_status != IPC_OK) {
        return self->ipc_status;
    }
    *msg = self->msg;
    return IPC_OK;
}

static void reply_destroy(struct object *obj);

static struct reply *reply_alloc(struct endpoint *ep)
{
    struct reply *rep = 0;
    uint64_t flags;

    flags = sched_lock();
    for (int i = 0; i < IPC_MAX_REPLIES; i++) {
        if (!replies[i].used) {
            rep = &replies[i];
            break;
        }
    }
    if (rep == 0) {
        sched_unlock(flags);
        return 0;
    }

    memzero(rep, sizeof(*rep));
    rep->used = true;
    rep->done = false;
    rep->aborted = false;
    rep->ep = ep;
    waitq_init(&rep->waitq);

    if (object_register(&rep->obj, OBJ_REPLY, reply_destroy) < 0) {
        rep->used = false;
        sched_unlock(flags);
        return 0;
    }
    sched_unlock(flags);
    return rep;
}

static void reply_destroy(struct object *obj)
{
    struct reply *rep = (struct reply *)obj;
    uint64_t flags;

    flags = sched_lock();
    wake_all_locked(&rep->waitq, IPC_EDEAD);
    rep->ep = 0;
    rep->used = false;
    sched_unlock(flags);
}

int ep_call(struct endpoint *ep, const ipc_message *msg, ipc_message *reply)
{
    struct reply *rep;
    ipc_message request;
    int rc;

    if (ep == 0 || msg == 0 || reply == 0) {
        return IPC_EINVAL;
    }

    rep = reply_alloc(ep);
    if (rep == 0) {
        return IPC_ENOMEM;
    }

    request = *msg;
    request.reply_id = object_get_id(&rep->obj);
    rc = ep_send(ep, &request);
    if (rc != IPC_OK) {
        object_unref(&rep->obj);
        return rc;
    }

    for (;;) {
        uint64_t flags = sched_lock();

        if (rep->done) {
            *reply = rep->msg;
            sched_unlock(flags);
            rc = IPC_OK;
            break;
        }
        if (rep->aborted) {
            /* endpoint 已销毁：请求不会再有回复，放弃等待。 */
            sched_unlock(flags);
            rc = IPC_EDEAD;
            break;
        }
        thread_block_locked(&rep->waitq);
        sched_unlock(flags);
        sched();
    }

    object_unref(&rep->obj);
    return rc;
}

int ep_reply(uint32_t reply_id, const ipc_message *msg)
{
    struct object *obj;
    struct reply *rep;
    struct thread *t;
    uint64_t flags;

    if (msg == 0) {
        return IPC_EINVAL;
    }

    obj = object_lookup(reply_id, OBJ_REPLY);
    if (obj == 0) {
        return IPC_EINVAL;
    }
    rep = (struct reply *)obj;

    flags = sched_lock();
    if (rep->aborted) {
        sched_unlock(flags);
        object_unref(obj);
        return IPC_EDEAD;
    }
    if (rep->done) {
        sched_unlock(flags);
        object_unref(obj);
        return IPC_EBUSY;
    }

    /*
     * 回复内容、单次标记和调用者状态在同一临界区内完成：
     * 调用者要么在 waitq 上被直接唤醒，要么尚未进入等待循环并在
     * 下一次检查里看到 done。
     */
    rep->msg = *msg;
    rep->done = true;
    t = waitq_pop(&rep->waitq);
    if (t != 0) {
        t->ipc_status = IPC_OK;
        thread_wake_locked(t);
    }
    sched_unlock(flags);

    object_unref(obj);
    return IPC_OK;
}
