#include "demo.h"
#include "ipc.h"

static _Alignas(16) uint8_t caller_stack[4096], server_stack[4096];
static _Alignas(16) uint8_t waiter_stack[4096];
static _Alignas(16) uint8_t abandoned_caller_stack[4096];
static _Alignas(16) uint8_t abandoned_server_stack[4096];

struct ipc_case {
    struct endpoint *ep;
    bool received, answered;
    int recv_rc, call_rc;
};

static void client(void *argument)
{
    struct ipc_case *test = argument;
    const ipc_message request = {.tag = 7, .words = {41}};
    ipc_message reply;

    CHECK(ep_call(test->ep, &request, &reply) == IPC_OK);
    CHECK(reply.tag == 8 && reply.words[0] == 42);
    test->answered = true;
}

static void server(void *argument)
{
    struct ipc_case *test = argument;
    ipc_message request;
    CHECK(ep_recv(test->ep, &request) == IPC_OK);
    CHECK(request.tag == 7 && request.words[0] == 41 && request.reply_id != 0);
    test->received = true;
    thread_yield();
    const ipc_message reply = {.tag = 8, .words = {42}};
    CHECK(ep_reply(request.reply_id, &reply) == IPC_OK);
    CHECK(ep_reply(request.reply_id, &reply) == IPC_EBUSY);
}

static void waiter(void *argument)
{
    struct ipc_case *test = argument;
    ipc_message request;

    test->recv_rc = ep_recv(test->ep, &request);
}

static void abandoned_caller(void *argument)
{
    struct ipc_case *test = argument;
    const ipc_message request = {.tag = 9, .words = {1, 2, 3, 4}};
    ipc_message reply;

    test->call_rc = ep_call(test->ep, &request, &reply);
}

static void abandoned_server(void *argument)
{
    struct ipc_case *test = argument;
    ipc_message request;

    test->recv_rc = ep_recv(test->ep, &request);
    if (test->recv_rc == IPC_OK) {
        CHECK(request.tag == 9 && request.reply_id != 0);
        test->received = true;
    }
    thread_yield(); /* 让调用者进入 reply 等待，然后不回复退出。 */
}

static void run(void)
{
    struct ipc_case test = {0}, wake = {0}, abandoned = {0};
    struct thread runner = {.name = "bootstrap"};
    struct thread *blocked, *caller;

    object_system_init();
    sched_init();
    sched_bootstrap(&runner);
    ipc_init();

    /* 1. call 阻塞到 reply，reply 只能使用一次。 */
    test.ep = ep_create();
    CHECK(test.ep);
    uint32_t id = ep_id(test.ep);
    CHECK(thread_create(NULL, client, &test, caller_stack, sizeof(caller_stack), "client"));
    CHECK(thread_create(NULL, server, &test, server_stack, sizeof(server_stack), "server"));
    for (unsigned i = 0; i < 6 && !test.answered; i++)
        thread_yield();
    CHECK(test.received && test.answered && sched_ready_count() == 0);
    CHECK(test.ep->state == EP_IDLE);
    CHECK(waitq_empty(&test.ep->sendq) && waitq_empty(&test.ep->recvq));
    thread_yield();
    ep_unref(test.ep);
    CHECK(object_lookup(id, OBJ_ENDPOINT) == NULL);

    /* 2. endpoint 销毁唤醒阻塞在 recv 的线程。 */
    wake.ep = ep_create();
    CHECK(wake.ep);
    uint32_t wake_id = ep_id(wake.ep);
    blocked = thread_create(NULL, waiter, &wake, waiter_stack,
                            sizeof(waiter_stack), "waiter");
    CHECK(blocked != NULL);
    thread_yield();
    CHECK(blocked->state == THREAD_BLOCKED && wake.ep->state == EP_RECV_WAIT);
    ep_unref(wake.ep);
    CHECK(blocked->state == THREAD_READY && blocked->ipc_status == IPC_EDEAD);
    thread_yield();
    CHECK(wake.recv_rc == IPC_EDEAD);
    CHECK(object_lookup(wake_id, OBJ_ENDPOINT) == NULL);

    /* 3. 借用端点的调用者已阻塞；所有者销毁 endpoint（模拟撤销 cap）。 */
    abandoned.ep = ep_create();
    CHECK(abandoned.ep);
    uint32_t abandoned_id = ep_id(abandoned.ep);
    caller = thread_create(NULL, abandoned_caller, &abandoned,
                           abandoned_caller_stack, sizeof(abandoned_caller_stack),
                           "caller2");
    CHECK(caller != NULL);
    CHECK(thread_create(NULL, abandoned_server, &abandoned,
                        abandoned_server_stack, sizeof(abandoned_server_stack),
                        "server2"));
    for (unsigned i = 0; i < 8 &&
                        !(abandoned.received && caller->state == THREAD_BLOCKED); i++)
        thread_yield();
    CHECK(abandoned.received && caller->state == THREAD_BLOCKED);
    ep_unref(abandoned.ep);
    CHECK(caller->state == THREAD_READY && caller->ipc_status == IPC_EDEAD);
    thread_yield();
    CHECK(abandoned.call_rc == IPC_EDEAD);
    CHECK(object_lookup(abandoned_id, OBJ_ENDPOINT) == NULL);
    CHECK(sched_ready_count() == 0);

    sched_init();
    demo_log("IPC: rendezvous, blocked caller, one-shot reply and endpoint death");
}

DEMO_MODULE(ipc, false, run, NULL);
