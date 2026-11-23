#include "echo.h"
#include "demo.h"

struct endpoint_fixture { unsigned calls, reports, refs; enum echo_state result; };

static int64_t call(void *ctx, uint32_t endpoint, const struct abi_message *msg)
{
    struct endpoint_fixture *ep = ctx;
    CHECK(endpoint == 7 && ep->refs == 1);
    CHECK(msg->tag == ECHO_REQUEST && msg->words[0] == 42);
    CHECK((msg->words[1] | msg->words[2] | msg->words[3]) == 0);
    ep->calls++;
    return ABI_OK;
}

static void report(void *ctx, enum echo_state result)
{
    struct endpoint_fixture *ep = ctx;
    ep->reports++;
    ep->result = result;
}

static void release(void *ctx, uint32_t endpoint)
{
    struct endpoint_fixture *ep = ctx;
    CHECK(endpoint == 7 && ep->refs == 1);
    ep->refs--;
}

static void run_reply(int64_t status, uint32_t tag, uint64_t value, enum echo_state expected)
{
    struct endpoint_fixture ep = {.refs = 1};
    const struct echo_port port = {.ctx = &ep, .call = call, .report = report, .release = release};
    struct echo_app app = {.port = &port, .endpoint = 7, .value = 42};
    const struct abi_message reply = {.tag = tag, .words = {value}};

    CHECK(echo_start(&app) && app.state == ECHO_WAITING);
    CHECK(!echo_start(&app));
    CHECK(echo_on_reply(&app, status, &reply));
    CHECK(!echo_on_reply(&app, status, &reply));
    CHECK(app.state == expected && ep.result == expected);
    CHECK(ep.calls == 1 && ep.reports == 1 && ep.refs == 0);
}

static void run(void)
{
    run_reply(ABI_OK, ECHO_REPLY, 42, ECHO_DONE);
    run_reply(ABI_OK, ECHO_REQUEST, 42, ECHO_FAILED);
    run_reply(ABI_OK, ECHO_REPLY, 9, ECHO_FAILED);
    run_reply(ABI_CLOSED, ECHO_REPLY, 42, ECHO_FAILED);
    demo_log("echo: one request, validated reply and endpoint release");
}

DEMO_MODULE(apps, true, run, NULL);
