#include "demo.h"
#include "ipc.h"

static _Alignas(16) uint8_t caller_stack[4096], server_stack[4096];

struct ipc_case {
    struct endpoint *ep;
    bool received, answered;
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

static void run(void)
{
    struct thread runner = {.name = "bootstrap"};
    object_system_init();
    sched_init();
    sched_bootstrap(&runner);
    ipc_init();
    struct ipc_case test = {.ep = ep_create()};
    CHECK(test.ep);
    uint32_t id = ep_id(test.ep);
    CHECK(thread_create(NULL, client, &test, caller_stack, sizeof(caller_stack), "client"));
    CHECK(thread_create(NULL, server, &test, server_stack, sizeof(server_stack), "server"));
    for (unsigned i = 0; i < 6 && !test.answered; i++)
        thread_yield();
    CHECK(test.received && test.answered && sched_ready_count() == 0);
    thread_yield();
    ep_unref(test.ep);
    CHECK(object_lookup(id, OBJ_ENDPOINT) == NULL);
    sched_init();
    demo_log("IPC: rendezvous, blocked caller and one-shot reply");
}

DEMO_MODULE(ipc, false, run, NULL);
