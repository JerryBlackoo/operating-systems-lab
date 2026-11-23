#include "syscall.h"
#include "csr.h"

struct spec {
    unsigned argc;
    bool tag, result;
    enum syscall_kind kind;
    unsigned rights;
};

static const struct spec specs[ABI_AS_UNMAP + 1] = {
    [ABI_YIELD]       = {0, false, false, SYSCALL_NONE, 0},
    [ABI_THREAD_EXIT] = {0, false, false, SYSCALL_NONE, 0},
    [ABI_IPC_SEND]    = {6, true, false, SYSCALL_ENDPOINT, SYSCALL_SEND},
    [ABI_IPC_RECV]    = {1, false, true, SYSCALL_ENDPOINT, SYSCALL_RECV},
    [ABI_IPC_CALL]    = {6, true, true, SYSCALL_ENDPOINT, SYSCALL_SEND},
    [ABI_IPC_REPLY]   = {6, true, false, SYSCALL_REPLY, SYSCALL_ANSWER},
    [ABI_AS_MAP]      = {4, false, false, SYSCALL_AS, SYSCALL_MAP},
    [ABI_AS_UNMAP]    = {2, false, false, SYSCALL_AS, SYSCALL_MAP},
};

static bool valid_args(const struct syscall_request *request, const struct spec *spec)
{
    const uint64_t *args = request->args.a;

    for (unsigned i = spec->argc; i < ABI_REGS; i++) {
        if (args[i])
            return false;
    }
    if ((spec->kind != SYSCALL_NONE && args[0] > UINT32_MAX) ||
        (spec->tag && args[1] > UINT32_MAX))
        return false;
    if (request->nr == ABI_AS_MAP || request->nr == ABI_AS_UNMAP) {
        if ((args[1] & 4095) || args[1] >= (UINT64_C(1) << 38))
            return false;
    }
    if (request->nr == ABI_AS_MAP) {
        uint64_t rights = args[3];
        return args[2] <= UINT32_MAX && rights > 0 && rights <= 7 &&
               (!(rights & ABI_WRITE) || (rights & ABI_READ));
    }
    return true;
}

static int64_t acquire(struct syscall_call *call, uint32_t handle,
                       enum syscall_kind kind, unsigned rights)
{
    const struct syscall_backend *port = call->backend;
    uint32_t *ref = &call->request.refs[call->held];
    int64_t status = port->acquire(port->ctx, call->request.caller, handle, kind, rights, ref);

    if (status == ABI_OK)
        call->held++;
    return status;
}

static enum syscall_phase finish(struct syscall_call *call, enum syscall_phase phase,
                                 int64_t status, const struct abi_message *message)
{
    const struct syscall_backend *port = call->backend;
    struct trap_frame *frame = call->frame;

    if (phase == SYSCALL_COMPLETE) {
        struct abi_regs out = abi_result(status, message);
        frame->a0 = out.a[0];
        frame->a1 = out.a[1];
        frame->a2 = out.a[2];
        frame->a3 = out.a[3];
        frame->a4 = out.a[4];
        frame->a5 = out.a[5];
    }
    while (call->held)
        port->release(port->ctx, call->request.refs[--call->held]);
    call->frame = NULL;
    call->phase = phase;
    return phase;
}

enum syscall_phase syscall_dispatch(struct syscall_call *call, struct trap_frame *frame,
                                    uint32_t caller, const struct syscall_backend *backend)
{
    if (call->phase == SYSCALL_WAITING)
        return SYSCALL_WAITING;
    if (frame->scause != EXC_ECALL_U || (frame->sstatus & SSTATUS_SPP))
        return SYSCALL_IGNORED;

    *call = (struct syscall_call){.frame = frame, .backend = backend};
    frame->sepc += 4;
    if (frame->a7 < ABI_YIELD || frame->a7 > ABI_AS_UNMAP)
        return finish(call, SYSCALL_COMPLETE, ABI_BAD_NR, NULL);

    struct syscall_request *request = &call->request;
    request->caller = caller;
    request->nr = (enum abi_number)frame->a7;
    request->args = (struct abi_regs){{frame->a0, frame->a1, frame->a2,
                                      frame->a3, frame->a4, frame->a5}};
    const struct spec *spec = &specs[request->nr];
    if (!valid_args(request, spec))
        return finish(call, SYSCALL_COMPLETE, ABI_BAD_ARG, NULL);

    int64_t status = ABI_OK;
    if (spec->kind != SYSCALL_NONE)
        status = acquire(call, request->args.a[0], spec->kind, spec->rights);
    if (status == ABI_OK && request->nr == ABI_AS_MAP)
        status = acquire(call, request->args.a[2], SYSCALL_FRAME, request->args.a[3]);
    if (status != ABI_OK)
        return finish(call, SYSCALL_COMPLETE, status, NULL);

    struct abi_message result = {0};
    enum syscall_phase phase = backend->invoke(backend->ctx, request, &status, &result);
    if (phase == SYSCALL_WAITING) {
        call->phase = phase;
        return phase;
    }
    if (phase == SYSCALL_EXITED)
        return finish(call, phase, status, NULL);
    if (request->nr == ABI_IPC_CALL)
        result.reply = 0;
    return finish(call, SYSCALL_COMPLETE, status, spec->result ? &result : NULL);
}

bool syscall_resume(struct syscall_call *call, int64_t status, struct abi_message message)
{
    if (call->phase != SYSCALL_WAITING)
        return false;
    if (call->request.nr == ABI_IPC_CALL)
        message.reply = 0;
    finish(call, SYSCALL_COMPLETE, status, specs[call->request.nr].result ? &message : NULL);
    return true;
}
