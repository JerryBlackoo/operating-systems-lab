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

static void ep_update_state(struct endpoint *ep)
{
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
    struct thread *t;
    uint64_t flags;

    flags = sched_lock();
    ep->state = EP_DYING;

    while ((t = waitq_pop(&ep->sendq)) != 0) {
        t->ipc_status = IPC_EDEAD;
        thread_wake_locked(t);
    }
    while ((t = waitq_pop(&ep->recvq)) != 0) {
        t->ipc_status = IPC_EDEAD;
        thread_wake_locked(t);
    }
    sched_unlock(flags);

    ep->used = false;
}

void ipc_init(void)
{
    memzero(endpoints, sizeof(endpoints));
    memzero(replies, sizeof(replies));
}

struct endpoint *ep_create(void)
{
    struct endpoint *ep = 0;

    for (int i = 0; i < IPC_MAX_ENDPOINTS; i++) {
        if (!endpoints[i].used) {
            ep = &endpoints[i];
            break;
        }
    }
    if (ep == 0) {
        return 0;
    }

    memzero(ep, sizeof(*ep));
    ep->used = true;
    ep->state = EP_IDLE;
    waitq_init(&ep->sendq);
    waitq_init(&ep->recvq);

    if (object_register(&ep->obj, OBJ_ENDPOINT, ep_destroy) < 0) {
        ep->used = false;
        return 0;
    }
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

static struct reply *reply_alloc(void)
{
    struct reply *rep = 0;

    for (int i = 0; i < IPC_MAX_REPLIES; i++) {
        if (!replies[i].used) {
            rep = &replies[i];
            break;
        }
    }
    if (rep == 0) {
        return 0;
    }

    memzero(rep, sizeof(*rep));
    rep->used = true;
    rep->done = false;
    waitq_init(&rep->waitq);

    if (object_register(&rep->obj, OBJ_REPLY, reply_destroy) < 0) {
        rep->used = false;
        return 0;
    }
    return rep;
}

static void reply_destroy(struct object *obj)
{
    struct reply *rep = (struct reply *)obj;
    struct thread *t;
    uint64_t flags;

    flags = sched_lock();
    while ((t = waitq_pop(&rep->waitq)) != 0) {
        t->ipc_status = IPC_EDEAD;
        thread_wake_locked(t);
    }
    sched_unlock(flags);
    rep->used = false;
}

int ep_call(struct endpoint *ep, const ipc_message *msg, ipc_message *reply)
{
    struct reply *rep;
    ipc_message request;
    int rc;

    if (ep == 0 || msg == 0 || reply == 0) {
        return IPC_EINVAL;
    }

    rep = reply_alloc();
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
            break;
        }
        thread_block_locked(&rep->waitq);
        sched_unlock(flags);
        sched();
    }

    object_unref(&rep->obj);
    return IPC_OK;
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
    if (rep->done) {
        sched_unlock(flags);
        object_unref(obj);
        return IPC_EBUSY;
    }

    rep->msg = *msg;
    rep->done = true;
    t = waitq_pop(&rep->waitq);
    if (t != 0) {
        thread_wake_locked(t);
    }
    sched_unlock(flags);

    object_unref(obj);
    return IPC_OK;
}
