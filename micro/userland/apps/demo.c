#include "echo.h"
#include "demo.h"
#include "csr.h"
#include "trap.h"

enum { ECHO_ENDPOINT = 7, ECHO_VALUE = 42 };

struct echo_harness { unsigned calls, reports, refs; enum echo_state result; };

/* 回显替身：每次 U-mode 调用后由陷阱处理器给出应答。 */
enum stand_in_mode {
    STAND_IN_ECHO,
    STAND_IN_WRONG_TAG,
    STAND_IN_WRONG_VALUE,
    STAND_IN_DIRTY_WORD,
    STAND_IN_CLOSED,
};

static struct { enum stand_in_mode mode; unsigned calls; } stand_in;

static int64_t call(void *ctx, uint32_t endpoint, const struct abi_message *msg,
                    struct abi_message *reply)
{
    struct echo_harness *harness = ctx;
    CHECK(endpoint == ECHO_ENDPOINT && harness->refs == 1);
    CHECK(msg->tag == ECHO_REQUEST && msg->reply == 0 && msg->words[0] == ECHO_VALUE);
    CHECK((msg->words[1] | msg->words[2] | msg->words[3]) == 0);
    harness->calls++;

    struct abi_regs out = abi_ecall(ABI_IPC_CALL, abi_message_args(endpoint, msg));
    int64_t status = abi_decode_status(out);
    if (status == ABI_OK)
        *reply = abi_decode_message(out);
    return status;
}

static void report(void *ctx, enum echo_state result)
{
    struct echo_harness *harness = ctx;
    harness->reports++;
    harness->result = result;
}

static void release(void *ctx, uint32_t endpoint)
{
    struct echo_harness *harness = ctx;
    CHECK(endpoint == ECHO_ENDPOINT && harness->refs == 1);
    harness->refs--;
}

static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    if (frame->scause != EXC_ECALL_U || (frame->sstatus & SSTATUS_SPP))
        return NULL;
    CHECK(frame->a7 == ABI_IPC_CALL);
    CHECK(frame->a0 == ECHO_ENDPOINT && frame->a1 == ECHO_REQUEST);
    CHECK(frame->a2 == ECHO_VALUE && (frame->a3 | frame->a4 | frame->a5) == 0);

    struct abi_message reply = {.tag = ECHO_REPLY, .reply = 0, .words = {ECHO_VALUE}};
    int64_t status = ABI_OK;
    switch (stand_in.mode) {
    case STAND_IN_WRONG_TAG:
        reply.tag = ECHO_REQUEST;
        break;
    case STAND_IN_WRONG_VALUE:
        reply.words[0] = ECHO_VALUE + 1;
        break;
    case STAND_IN_DIRTY_WORD:
        reply.words[2] = 1;
        break;
    case STAND_IN_CLOSED:
        status = ABI_CLOSED;
        break;
    default:
        break;
    }

    struct abi_regs out = abi_result(status, status == ABI_OK ? &reply : NULL);
    frame->a0 = out.a[0];
    frame->a1 = out.a[1];
    frame->a2 = out.a[2];
    frame->a3 = out.a[3];
    frame->a4 = out.a[4];
    frame->a5 = out.a[5];
    frame->sepc += 4;
    stand_in.calls++;
    return frame;
}

static void run_case(enum stand_in_mode mode, enum echo_state expected)
{
    struct echo_harness harness = {.refs = 1};
    const struct echo_port port = {.ctx = &harness, .call = call, .report = report, .release = release};
    struct echo_app app = {.port = &port, .endpoint = ECHO_ENDPOINT, .value = ECHO_VALUE};

    stand_in.mode = mode;
    stand_in.calls = 0;
    CHECK(echo_start(&app) && stand_in.calls == 1);
    CHECK(app.state == expected && harness.result == expected);
    CHECK(harness.calls == 1 && harness.reports == 1 && harness.refs == 0);
    CHECK(!app.endpoint);
    CHECK(!echo_start(&app));
    CHECK(!echo_on_reply(&app, ABI_OK, NULL));
}

static void run(void)
{
    run_case(STAND_IN_ECHO, ECHO_DONE);
    run_case(STAND_IN_WRONG_TAG, ECHO_FAILED);
    run_case(STAND_IN_WRONG_VALUE, ECHO_FAILED);
    run_case(STAND_IN_DIRTY_WORD, ECHO_FAILED);
    run_case(STAND_IN_CLOSED, ECHO_FAILED);
    demo_log("echo: one request, full message check and endpoint release");
}

DEMO_MODULE(apps, true, run, handle_trap);
