#include "ipc.h"

#include <assert.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static uint8_t stack_sender[8192] __attribute__((aligned(16)));
static uint8_t stack_receiver[8192] __attribute__((aligned(16)));
static uint8_t stack_idle[8192] __attribute__((aligned(16)));

static struct thread *sender;
static struct thread *receiver;
static struct thread *idle;

void cpu_switch(struct cpu_context *old_ctx, struct cpu_context *new_ctx)
{
    (void)old_ctx;
    (void)new_ctx;
}

void cpu_switch_to(struct cpu_context *new_ctx)
{
    (void)new_ctx;
}

static void worker(void *arg)
{
    (void)arg;
}

static void setup(void)
{
    object_system_init();
    sched_init();
    ipc_init();

    sender = thread_create(0, worker, 0, stack_sender, sizeof(stack_sender),
                           "sender");
    receiver = thread_create(0, worker, 0, stack_receiver,
                             sizeof(stack_receiver), "receiver");
    idle = thread_create(0, worker, 0, stack_idle, sizeof(stack_idle), "idle");
    assert(sender != 0 && receiver != 0 && idle != 0);
    sched_set_idle(idle);
}

static ipc_message make_message(uint32_t tag)
{
    ipc_message msg;

    msg.tag = tag;
    msg.reply_id = 0;
    for (int i = 0; i < IPC_MSG_WORDS; i++) {
        msg.words[i] = (uint64_t)tag * 100 + (uint64_t)i;
    }
    return msg;
}

static void test_message_layout(void)
{
    assert(IPC_MSG_WORDS == 4);
    assert(IPC_MSG_BYTES == 32);
    assert(sizeof(((ipc_message *)0)->words) == IPC_MSG_BYTES);
    assert(sizeof(ipc_message) == 40);
    assert(offsetof(ipc_message, tag) == 0);
    assert(offsetof(ipc_message, reply_id) == 4);
    assert(offsetof(ipc_message, words) == 8);

    printf("ipc: message layout ok\n");
}

static void test_argument_validation(void)
{
    struct endpoint *ep = ep_create();
    ipc_message msg = make_message(0x66);
    ipc_message out;

    assert(ep != 0);
    assert(ep_send(0, &msg) == IPC_EINVAL);
    assert(ep_send(ep, 0) == IPC_EINVAL);
    assert(ep_recv(0, &out) == IPC_EINVAL);
    assert(ep_recv(ep, 0) == IPC_EINVAL);
    assert(ep_call(0, &msg, &out) == IPC_EINVAL);
    assert(ep_call(ep, 0, &out) == IPC_EINVAL);
    assert(ep_call(ep, &msg, 0) == IPC_EINVAL);
    assert(ep_reply(0, &msg) == IPC_EINVAL);
    assert(ep_reply(0xdeadbeefu, &msg) == IPC_EINVAL);
    /* reply_id 必须指向 OBJ_REPLY，endpoint 的 id 不能冒用。 */
    assert(ep_reply(ep_id(ep), &msg) == IPC_EINVAL);
    assert(ep_reply(ep_id(ep), 0) == IPC_EINVAL);

    ep_unref(ep);
    printf("ipc: argument validation ok\n");
}

static void test_sender_blocks(void)
{
    struct endpoint *ep;
    ipc_message msg = make_message(0x11);
    ipc_message out;
    int rc;

    ep = ep_create();
    assert(ep != 0);

    sched_bootstrap(sender);
    rc = ep_send(ep, &msg);
    /*
     * 主机自测没有真实栈切换：ep_send 在 sched 返回后以 PENDING 返回。
     * 真正内核里发送者会停在这里，直到接收者把它唤醒。
     */
    assert(rc == IPC_PENDING);
    assert(sender->state == THREAD_BLOCKED);
    assert(ep->state == EP_SEND_WAIT);
    assert(!waitq_empty(&ep->sendq));
    /* 不变量：sendq 和 recvq 至多一个非空。 */
    assert(waitq_empty(&ep->recvq));
    assert(sched_current() == receiver);

    rc = ep_recv(ep, &out);
    assert(rc == IPC_OK);
    assert(out.tag == 0x11);
    for (int i = 0; i < IPC_MSG_WORDS; i++) {
        assert(out.words[i] == (uint64_t)0x11 * 100 + (uint64_t)i);
    }
    assert(sender->state == THREAD_READY);
    assert(ep->state == EP_IDLE);
    assert(waitq_empty(&ep->sendq));
    assert(waitq_empty(&ep->recvq));

    ep_unref(ep);
    printf("ipc: sender blocks, receiver receives ok\n");
}

static void test_receiver_blocks(void)
{
    struct endpoint *ep;
    ipc_message msg = make_message(0x22);
    ipc_message out;
    int rc;

    ep = ep_create();
    assert(ep != 0);

    sched_bootstrap(receiver);
    rc = ep_recv(ep, &out);
    assert(rc == IPC_PENDING);
    assert(receiver->state == THREAD_BLOCKED);
    assert(ep->state == EP_RECV_WAIT);
    assert(!waitq_empty(&ep->recvq));
    assert(waitq_empty(&ep->sendq));
    assert(sched_current() == sender);

    rc = ep_send(ep, &msg);
    assert(rc == IPC_OK);
    assert(receiver->state == THREAD_READY);
    assert(receiver->msg.tag == 0x22);
    for (int i = 0; i < IPC_MSG_WORDS; i++) {
        assert(receiver->msg.words[i] ==
               (uint64_t)0x22 * 100 + (uint64_t)i);
    }
    assert(ep->state == EP_IDLE);
    assert(waitq_empty(&ep->recvq));

    ep_unref(ep);
    printf("ipc: receiver blocks, sender delivers ok\n");
}

static void test_endpoint_destroy_wakes_waiter(void)
{
    struct endpoint *ep;
    ipc_message msg = make_message(0x33);

    ep = ep_create();
    assert(ep != 0);

    sched_bootstrap(sender);
    assert(ep_send(ep, &msg) == IPC_PENDING);
    assert(sender->state == THREAD_BLOCKED);

    ep_unref(ep);
    assert(sender->state == THREAD_READY);
    assert(sender->ipc_status == IPC_EDEAD);
    assert(waitq_empty(&ep->sendq));

    printf("ipc: endpoint destroy wakes waiter ok\n");
}

static void test_endpoint_destroy_wakes_receiver(void)
{
    struct endpoint *ep;
    ipc_message out;

    ep = ep_create();
    assert(ep != 0);

    sched_bootstrap(receiver);
    assert(ep_recv(ep, &out) == IPC_PENDING);
    assert(receiver->state == THREAD_BLOCKED);
    assert(ep->state == EP_RECV_WAIT);

    ep_unref(ep);
    assert(receiver->state == THREAD_READY);
    assert(receiver->ipc_status == IPC_EDEAD);
    assert(waitq_empty(&ep->recvq));

    printf("ipc: endpoint destroy wakes receiver ok\n");
}

static void test_endpoint_slot_generation(void)
{
    struct endpoint *ep;
    uint32_t first, second;

    ep = ep_create();
    assert(ep != 0);
    first = ep_id(ep);
    assert(first != 0);
    ep_unref(ep);

    /* 槽位复用后代际必须变化，旧 id 不能命中新 endpoint。 */
    ep = ep_create();
    assert(ep != 0);
    second = ep_id(ep);
    assert(second != 0 && second != first);
    ep_unref(ep);

    printf("ipc: endpoint slot generation ok\n");
}

static void test_endpoint_pool_exhaustion(void)
{
    struct endpoint *slots[IPC_MAX_ENDPOINTS];

    for (int i = 0; i < IPC_MAX_ENDPOINTS; i++) {
        slots[i] = ep_create();
        assert(slots[i] != 0);
    }
    assert(ep_create() == 0);

    for (int i = 0; i < IPC_MAX_ENDPOINTS; i++) {
        ep_unref(slots[i]);
    }

    /* 全部释放后必须能重新分配。 */
    struct endpoint *ep = ep_create();
    assert(ep != 0);
    ep_unref(ep);

    printf("ipc: endpoint pool exhaustion ok\n");
}

static void test_invalid_reply(void)
{
    ipc_message msg = make_message(0x44);

    assert(ep_reply(0, &msg) == IPC_EINVAL);
    assert(ep_reply(0xdeadbeefu, &msg) == IPC_EINVAL);

    printf("ipc: invalid reply ok\n");
}

static void test_reply_destroy(struct object *obj)
{
    struct reply *rep = (struct reply *)obj;

    rep->used = false;
}

static void test_reply_delivery(void)
{
    struct reply rep;
    ipc_message msg = make_message(0x55);
    uint32_t id;

    memset(&rep, 0, sizeof(rep));
    rep.used = true;
    waitq_init(&rep.waitq);
    assert(object_register(&rep.obj, OBJ_REPLY, test_reply_destroy) == 0);
    id = object_get_id(&rep.obj);

    assert(ep_reply(id, &msg) == IPC_OK);
    assert(rep.done);
    assert(rep.msg.tag == 0x55);
    for (int i = 0; i < IPC_MSG_WORDS; i++) {
        assert(rep.msg.words[i] == msg.words[i]);
    }

    /* 单次 reply：第二次必须失败。 */
    assert(ep_reply(id, &msg) == IPC_EBUSY);

    object_unref(&rep.obj);
    assert(!rep.used);

    printf("ipc: reply delivery ok\n");
}

int main(void)
{
    setup();

    test_message_layout();
    test_argument_validation();
    test_sender_blocks();
    test_receiver_blocks();
    test_endpoint_destroy_wakes_waiter();
    test_endpoint_destroy_wakes_receiver();
    test_endpoint_slot_generation();
    test_endpoint_pool_exhaustion();
    test_invalid_reply();
    test_reply_delivery();

    printf("ipc: all tests passed\n");
    return 0;
}
