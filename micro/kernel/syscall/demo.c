#include "syscall.h"
#include "demo.h"
#include "csr.h"

enum { CLIENT = 1, ENDPOINT = 0x10002, SPACE = 0x10003, FRAME = 0x10004, REPLY = 9 };

static const struct {
    uint32_t handle;
    enum syscall_kind type;
    unsigned rights;
} caps[] = {
    {ENDPOINT, SYSCALL_ENDPOINT, SYSCALL_SEND | SYSCALL_RECV},
    {SPACE, SYSCALL_AS, SYSCALL_MAP},
    {FRAME, SYSCALL_FRAME, ABI_READ | ABI_WRITE},
};

static struct {
    unsigned invokes, pins;
    uint32_t last_reply;
    bool reply_issued, mapped;
    uint64_t mapped_va;
    struct abi_message sent;
} fixture = {.last_reply = REPLY - 1};

static int64_t acquire(void *ctx, uint32_t caller, uint32_t handle,
                       enum syscall_kind kind, unsigned rights, uint32_t *ref)
{
    (void)ctx;
    if (caller != CLIENT)
        return ABI_BAD_HANDLE;
    if (kind == SYSCALL_REPLY) {
        if (!fixture.reply_issued || fixture.last_reply != handle)
            return ABI_BAD_HANDLE;
        CHECK(rights == SYSCALL_ANSWER);
        *ref = 4;
        fixture.pins++;
        return ABI_OK;
    }
    for (unsigned i = 0; i < sizeof(caps) / sizeof(caps[0]); i++) {
        if (caps[i].handle != handle)
            continue;
        if (caps[i].type != kind || (rights & ~caps[i].rights))
            return ABI_DENIED;
        *ref = i + 1;
        fixture.pins++;
        return ABI_OK;
    }
    return ABI_BAD_HANDLE;
}

static void release(void *ctx, uint32_t ref)
{
    (void)ctx;
    CHECK(ref > 0 && fixture.pins > 0);
    fixture.pins--;
}

static enum syscall_phase invoke(void *ctx, const struct syscall_request *request,
                                 int64_t *status, struct abi_message *reply)
{
    (void)ctx;
    fixture.invokes++;
    switch (request->nr) {
    case ABI_THREAD_EXIT:
        return SYSCALL_EXITED;
    case ABI_IPC_CALL:
        fixture.sent = abi_decode_message(request->args);
        return SYSCALL_WAITING;
    case ABI_IPC_SEND:
        fixture.sent = abi_decode_message(request->args);
        break;
    case ABI_IPC_RECV:
        if (fixture.reply_issued || fixture.last_reply == UINT32_MAX) {
            *status = ABI_BUSY;
        } else {
            *reply = (struct abi_message){.tag = 7, .reply = ++fixture.last_reply, .words = {42}};
            fixture.reply_issued = true;
        }
        break;
    case ABI_IPC_REPLY:
        fixture.reply_issued = false;
        break;
    case ABI_AS_MAP:
        CHECK(request->refs[0] == 2 && request->refs[1] == 3);
        if (fixture.mapped)
            *status = ABI_ALREADY_MAPPED;
        else {
            fixture.mapped = true;
            fixture.mapped_va = request->args.a[1];
        }
        break;
    case ABI_AS_UNMAP:
        if (fixture.mapped && fixture.mapped_va == request->args.a[1])
            fixture.mapped = false;
        else
            *status = ABI_NOT_MAPPED;
        break;
    case ABI_YIELD:
        break;
    }
    return SYSCALL_COMPLETE;
}

static const struct syscall_backend backend = {.acquire = acquire, .release = release, .invoke = invoke};
static struct syscall_call call;

static void complete_frame(struct trap_frame *frame, int64_t status)
{
    CHECK(syscall_dispatch(&call, frame, CLIENT, &backend) == SYSCALL_COMPLETE);
    CHECK(frame->a0 == (uint64_t)status && fixture.pins == 0);
}

static void model_cases(void)
{
    struct trap_frame frame = {.scause = EXC_ECALL_S, .sepc = 100, .a7 = ABI_YIELD};
    CHECK(syscall_dispatch(&call, &frame, CLIENT, &backend) == SYSCALL_IGNORED);
    CHECK(frame.sepc == 100);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .sepc = 100, .a7 = 999, .a6 = 123, .a1 = 1};
    complete_frame(&frame, ABI_BAD_NR);
    CHECK(frame.sepc == 104 && frame.a1 == 0 && frame.a6 == 123 && frame.a7 == 999);

    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_SEND, .a0 = 2};
    complete_frame(&frame, ABI_BAD_HANDLE);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_AS_MAP, .a0 = SPACE,
                                .a1 = 4097, .a2 = FRAME, .a3 = ABI_READ};
    complete_frame(&frame, ABI_BAD_ARG);
    frame.a0 = SPACE;
    frame.a1 = 4096;
    frame.a2 = FRAME;
    frame.a3 = ABI_EXEC;
    complete_frame(&frame, ABI_DENIED);
    CHECK(fixture.invokes == 0);
    frame.a0 = SPACE;
    frame.a1 = 4096;
    frame.a2 = FRAME;
    frame.a3 = ABI_READ | ABI_WRITE;
    complete_frame(&frame, ABI_OK);
    CHECK(fixture.mapped_va == 4096);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_AS_UNMAP, .a0 = SPACE, .a1 = 4096};
    complete_frame(&frame, ABI_OK);
    CHECK(!fixture.mapped);

    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_RECV, .a0 = ENDPOINT};
    complete_frame(&frame, ABI_OK);
    CHECK(frame.a1 == ((uint64_t)REPLY << 32 | 7) && frame.a2 == 42);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_REPLY, .a0 = REPLY, .a1 = 8};
    complete_frame(&frame, ABI_OK);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_RECV, .a0 = ENDPOINT};
    complete_frame(&frame, ABI_OK);
    CHECK(frame.a1 >> 32 == REPLY + 1);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_REPLY, .a0 = REPLY};
    complete_frame(&frame, ABI_BAD_HANDLE);
    frame.a0 = REPLY + 1;
    complete_frame(&frame, ABI_OK);

    frame = (struct trap_frame){.scause = EXC_ECALL_U, .sepc = 100, .a7 = ABI_IPC_CALL,
                                .a0 = ENDPOINT, .a1 = 0x32, .a2 = 0xdead};
    unsigned before = fixture.invokes;
    CHECK(syscall_dispatch(&call, &frame, CLIENT, &backend) == SYSCALL_WAITING);
    CHECK(frame.sepc == 104 && fixture.invokes == before + 1 && fixture.pins == 1);
    CHECK(fixture.sent.tag == 0x32 && fixture.sent.words[0] == 0xdead);
    CHECK(syscall_dispatch(&call, &frame, CLIENT, &backend) == SYSCALL_WAITING);
    CHECK(syscall_resume(&call, ABI_OK, (struct abi_message){.tag = 55, .words = {12}}));
    CHECK(!syscall_resume(&call, ABI_OK, (struct abi_message){0}));
    CHECK(fixture.invokes == before + 1 && fixture.pins == 0);
    CHECK(frame.sepc == 104 && frame.a0 == 0 && frame.a1 == 55 && frame.a2 == 12);
    demo_log("dispatch: rejected frames, map/unmap, one-shot reply, delayed completion");
}

static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    enum syscall_phase phase = syscall_dispatch(&call, frame, CLIENT, &backend);
    if (phase == SYSCALL_IGNORED)
        return NULL;
    if (phase == SYSCALL_EXITED)
        demo_pass();
    CHECK(phase == SYSCALL_COMPLETE);
    return frame;
}

static void user_demo(uintptr_t unused)
{
    (void)unused;
    CHECK(abi_decode_status(abi_ecall(ABI_YIELD, (struct abi_regs){0})) == ABI_OK);
    CHECK(abi_decode_status(abi_ecall(99, (struct abi_regs){0})) == ABI_BAD_NR);
    struct abi_message msg = {.tag = 17, .words = {1, 2, 3, 4}};
    CHECK(abi_decode_status(abi_ecall(ABI_IPC_SEND, abi_message_args(ENDPOINT, &msg))) == ABI_OK);
    CHECK(fixture.sent.words[3] == 4);
    demo_log("U-mode ecall: yield, rejected number, send and thread exit");
    abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    CHECK(false);
}

static void run(void)
{
    model_cases();
    demo_enter_user(user_demo, 0);
}

DEMO_MODULE(syscall, false, run, handle_trap);
