#include "ipc.h"

#include <stddef.h>
#include <stdint.h>

#include "object.h"
#include "sched.h"

enum {
    IPC_ASYNC_ABI_OK = 0,
    IPC_ASYNC_ABI_CLOSED = -6,
};

void user_syscall_complete_async_ipc(struct thread *t, uint32_t reply_id,
                                     int64_t status,
                                     const struct thread_msg *reply)
    __attribute__((weak));

void user_syscall_complete_async_ipc(struct thread *t, uint32_t reply_id,
                                     int64_t status,
                                     const struct thread_msg *reply)
{
    (void)t;
    (void)reply_id;
    (void)status;
    (void)reply;
}

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

/* 在 replies[] 里按对象 id 找在途 reply；调用者持 sched_lock。 */
static struct reply *reply_find_locked(uint32_t reply_id)
{
    if (reply_id == 0) {
        return 0;
    }
    for (int i = 0; i < IPC_MAX_REPLIES; i++) {
        if (replies[i].used && object_get_id(&replies[i].obj) == reply_id) {
            return &replies[i];
        }
    }
    return 0;
}

/*
 * 请求交付给接收者的瞬间记下 owner。
 *
 * reply_id 只是一个整数值，任何进程都能猜/伪造；因此"谁有权回复"不能
 * 由令牌本身决定，必须绑定到真正从 endpoint 取走这条消息的线程。
 * ep_reply_from() 据此拒绝冒名回复。
 */
static void bind_reply_owner_locked(const ipc_message *msg,
                                    const struct thread *receiver)
{
    struct reply *rep;

    if (msg == 0 || receiver == 0 || msg->reply_id == 0) {
        return;
    }
    rep = reply_find_locked(msg->reply_id);
    if (rep != 0 && !rep->done && !rep->aborted) {
        rep->owner_sid = receiver->cap_sid;
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
            struct thread *async = rep->async_caller;

            rep->aborted = true;
            rep->async_caller = 0;
            wake_all_locked(&rep->waitq, IPC_EDEAD);
            if (async != 0) {
                uint32_t rid = object_get_id(&rep->obj);

                sched_unlock(flags);
                user_syscall_complete_async_ipc(async, rid,
                                                IPC_ASYNC_ABI_CLOSED, 0);
                object_unref(&rep->obj); /* 归还异步 call 的创建引用 */
                flags = sched_lock();
            }
        }
    }

    ep->used = false;
    sched_unlock(flags);
}

/*
 * 线程销毁钩子（由 sched 在线程真正释放时调用）。
 *
 * 阻塞在 ep_send 的线程把自己的消息体放在 TCB 里，接收者会直接读那块
 * 内存；阻塞在 ep_recv 的线程则挂在 recvq 上。线程被强制终止时这两处
 * 都必须清掉，否则：
 *   - 接收者会读到已回收线程的 msg（垃圾数据）；
 *   - recvq 里残留悬空指针，下一次 ep_send 会踩坏内存。
 *
 * 这里在 sched_lock 下完成，与 ep_send/ep_recv 的临界区互斥。
 */
void ipc_release_thread(struct thread *t)
{
    uint64_t flags;

    if (t == 0) {
        return;
    }
    flags = sched_lock();
    for (int i = 0; i < IPC_MAX_ENDPOINTS; i++) {
        struct endpoint *ep = &endpoints[i];

        if (!ep->used) {
            continue;
        }
        if (t->waitq == &ep->sendq) {
            waitq_remove(&ep->sendq, t);
        }
        if (t->waitq == &ep->recvq) {
            waitq_remove(&ep->recvq, t);
        }
        ep_update_state(ep);
    }
    for (int i = 0; i < IPC_MAX_REPLIES; i++) {
        if (replies[i].used && replies[i].async_caller == t) {
            replies[i].async_caller = 0;
        }
    }
    memzero(&t->msg, sizeof(t->msg));
    sched_unlock(flags);
}

void ipc_init(void)
{    memzero(endpoints, sizeof(endpoints));
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
        bind_reply_owner_locked(msg, receiver);
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
        bind_reply_owner_locked(msg, self);
        thread_wake_locked(sender);
        ep_update_state(ep);
        sched_unlock(flags);
        return IPC_OK;
    }

    /* 内核排队投递优先于阻塞：VM/FS 等接收者回来时先取走已入队的请求。 */
    if (ep->post_count != 0) {
        *msg = ep->posts[ep->post_head];
        bind_reply_owner_locked(msg, self);
        ep->post_head = (uint8_t)((ep->post_head + 1) % IPC_MAX_POSTS);
        ep->post_count--;
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

int ep_post(struct endpoint *ep, const ipc_message *msg)
{
    struct thread *receiver;
    uint64_t flags;

    if (ep == 0 || msg == 0) {
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

    if (ep->post_count >= IPC_MAX_POSTS) {
        sched_unlock(flags);
        return IPC_ENOMEM;
    }
    ep->posts[ep->post_tail] = *msg;
    ep->post_tail = (uint8_t)((ep->post_tail + 1) % IPC_MAX_POSTS);
    ep->post_count++;
    sched_unlock(flags);
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
    /*
     * owner_sid 先留 0：此刻请求还没交给任何接收者，无从判断谁有权回复。
     * ep_send/ep_recv/ep_post 把消息真正移交时会调用
     * bind_reply_owner_locked() 写入接收者的 sid。
     */
    rep->owner_sid = 0;
    /*
     * 抄下调用者的真实身份。这一步是服务端身份校验的唯一可信来源：
     * ipc_message 里没有发送者字段，用户态自报的 sid 一律不可信。
     * 若当前没有线程（内核直调），留 0 表示"无身份"。
     */
    {
        struct thread *self = sched_current();

        rep->caller_sid = (self != 0) ? self->cap_sid : (uint8_t)0;
    }
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

int ep_call_open(struct endpoint *ep, uint32_t *reply_id_out)
{
    struct reply *rep;

    if (ep == 0 || reply_id_out == 0) {
        return IPC_EINVAL;
    }

    rep = reply_alloc(ep);
    if (rep == 0) {
        return IPC_ENOMEM;
    }

    rep->async_caller = sched_current();
    *reply_id_out = object_get_id(&rep->obj);
    return IPC_OK;
}

void ep_call_cancel_async(uint32_t reply_id)
{
    struct object *obj;
    struct reply *rep;
    uint64_t flags;

    obj = object_lookup(reply_id, OBJ_REPLY);
    if (obj == 0) {
        return;
    }
    rep = (struct reply *)obj;
    flags = sched_lock();
    rep->async_caller = 0;
    rep->aborted = true;
    sched_unlock(flags);
    object_unref(obj);           /* lookup 引用 */
    object_unref(&rep->obj);     /* ep_call_open 的创建引用：归还 reply 槽位 */
}

int ep_call_deliver(struct endpoint *ep, uint32_t reply_id,
                    const ipc_message *msg)
{
    struct object *obj;
    struct reply *rep;
    ipc_message request;
    int rc;

    if (ep == 0 || msg == 0 || reply_id == 0) {
        return IPC_EINVAL;
    }

    obj = object_lookup(reply_id, OBJ_REPLY);
    if (obj == 0) {
        return IPC_EINVAL;
    }
    rep = (struct reply *)obj;
    if (rep->async_caller != sched_current()) {
        object_unref(obj);
        return IPC_EINVAL;
    }

    request = *msg;
    request.reply_id = reply_id;
    rc = ep_send(ep, &request);
    if (rc != IPC_OK) {
        rep->async_caller = 0;
        object_unref(obj);
        return rc;
    }
    object_unref(obj);
    return IPC_OK;
}

int ep_call_submit(struct endpoint *ep, const ipc_message *msg,
                   uint32_t *reply_id_out)
{
    uint32_t rid;
    int rc;

    rc = ep_call_open(ep, &rid);
    if (rc != IPC_OK) {
        return rc;
    }
    rc = ep_call_deliver(ep, rid, msg);
    if (rc != IPC_OK) {
        return rc;
    }
    *reply_id_out = rid;
    return IPC_OK;
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
    return ep_reply_from(0, reply_id, msg);
}

int ep_reply_from(uint8_t sid, uint32_t reply_id, const ipc_message *msg)
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
     * 归属校验：sid != 0 表示调用者来自用户态，必须与当初接收该请求的
     * 线程一致。reply token 只是 bearer 整数，若不做这一步，任何进程都能
     * 用猜到的 id 抢答别人的请求、伪造应答内容。
     */
    if (sid != 0 && rep->owner_sid != 0 && rep->owner_sid != sid) {
        sched_unlock(flags);
        object_unref(obj);
        return IPC_EINVAL;
    }

    /*
     * 回复内容、单次标记和调用者状态在同一临界区内完成：
     * 调用者要么在 waitq 上被直接唤醒，要么尚未进入等待循环并在
     * 下一次检查里看到 done。
     */
    rep->msg = *msg;
    rep->done = true;
    t = rep->async_caller;
    if (t != 0) {
        rep->async_caller = 0;
        sched_unlock(flags);
        user_syscall_complete_async_ipc(t, reply_id, IPC_ASYNC_ABI_OK,
                                        &rep->msg);
        object_unref(obj);       /* lookup 引用 */
        object_unref(obj);       /* ep_call_open 的创建引用：归还 reply 槽位 */
        return IPC_OK;
    }
    t = waitq_pop(&rep->waitq);
    if (t != 0) {
        t->ipc_status = IPC_OK;
        thread_wake_locked(t);
    }
    sched_unlock(flags);

    object_unref(obj);
    return IPC_OK;
}

int ipc_reply_caller_sid(uint32_t reply_id, uint8_t *sid)
{
    struct object *obj;
    struct reply *rep;
    int rc = IPC_EINVAL;

    if (sid == 0) {
        return IPC_EINVAL;
    }

    obj = object_lookup(reply_id, OBJ_REPLY);
    if (obj == 0) {
        return IPC_EINVAL;
    }
    rep = (struct reply *)obj;

    {
        uint64_t flags = sched_lock();

        if (!rep->aborted) {
            *sid = rep->caller_sid;
            rc = IPC_OK;
        }
        sched_unlock(flags);
    }

    object_unref(obj);
    return rc;
}
