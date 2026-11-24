#include "syscall.h"
#include "demo.h"
#include "csr.h"
#include "capability.h"
#include "ipc.h"
#include "memory.h"
#include "sched.h"

/*
 * syscall/demo.c 分两段验证同一套分发器：
 *
 *   1. 模型用例：用带计数的假后端逐条覆盖形状、权限、阻塞与续完，
 *      这里只关心 syscall.c 自己的状态机。
 *   2. 全链路用例：后端换成 capability + ipc + memory + sched 的真实实现，
 *      从 U-mode ecall 一路走到能力检查、端点队列、页表和调度器。
 *
 * 真实后端的 acquire 只做「句柄 -> 对象类别 + 权限」校验，对象在
 * cap_invoke 的 pins 区间内保持存活；release 仅用于核对在途引用计数。
 */

/* ---------------- 模型用例：受控的假后端 ---------------- */

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

static int64_t fake_acquire(void *ctx, uint32_t caller, uint32_t handle,
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

static void fake_release(void *ctx, uint32_t ref)
{
    (void)ctx;
    CHECK(ref > 0 && fixture.pins > 0);
    fixture.pins--;
}

static enum syscall_phase fake_invoke(void *ctx, const struct syscall_request *request,
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

static const struct syscall_backend fake_backend = {
    .acquire = fake_acquire, .release = fake_release, .invoke = fake_invoke,
};
static struct syscall_call fake_call;

static void complete_frame(struct trap_frame *frame, int64_t status)
{
    CHECK(syscall_dispatch(&fake_call, frame, CLIENT, &fake_backend) == SYSCALL_COMPLETE);
    CHECK(frame->a0 == (uint64_t)status && fixture.pins == 0);
}

static void model_cases(void)
{
    struct trap_frame frame = {.scause = EXC_ECALL_S, .sepc = 100, .a7 = ABI_YIELD};

    /* 非 U-mode ecall 不进入分发，也不推进 sepc。 */
    CHECK(syscall_dispatch(&fake_call, &frame, CLIENT, &fake_backend) == SYSCALL_IGNORED);
    CHECK(frame.sepc == 100);

    /* 未知编号：受理（sepc += 4）后以 BAD_NR 完成，参数不被改写。 */
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .sepc = 100, .a7 = 999,
                                .a6 = 123, .a1 = 1};
    complete_frame(&frame, ABI_BAD_NR);
    CHECK(frame.sepc == 104 && frame.a1 == 0 && frame.a6 == 123 && frame.a7 == 999);

    /* 句柄错误：后端 invoke 次数保持为零。 */
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_SEND, .a0 = 2};
    complete_frame(&frame, ABI_BAD_HANDLE);

    /* 未对齐的映射地址与越权的帧权限。 */
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_AS_MAP, .a0 = SPACE,
                                .a1 = 4097, .a2 = FRAME, .a3 = ABI_READ};
    complete_frame(&frame, ABI_BAD_ARG);
    frame.a0 = SPACE;
    frame.a1 = 4096;
    frame.a2 = FRAME;
    frame.a3 = ABI_EXEC;
    complete_frame(&frame, ABI_DENIED);
    CHECK(fixture.invokes == 0);

    /* 一次成功映射，再撤销。 */
    frame.a0 = SPACE;
    frame.a1 = 4096;
    frame.a2 = FRAME;
    frame.a3 = ABI_READ | ABI_WRITE;
    complete_frame(&frame, ABI_OK);
    CHECK(fixture.mapped_va == 4096);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_AS_UNMAP,
                                .a0 = SPACE, .a1 = 4096};
    complete_frame(&frame, ABI_OK);
    CHECK(!fixture.mapped);

    /* RECV 拿到一次性回复令牌；REPLY 只能使用一次。 */
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_RECV, .a0 = ENDPOINT};
    complete_frame(&frame, ABI_OK);
    CHECK(frame.a1 == ((uint64_t)REPLY << 32 | 7) && frame.a2 == 42);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_REPLY,
                                .a0 = REPLY, .a1 = 8};
    complete_frame(&frame, ABI_OK);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_RECV, .a0 = ENDPOINT};
    complete_frame(&frame, ABI_OK);
    CHECK(frame.a1 >> 32 == REPLY + 1);
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .a7 = ABI_IPC_REPLY, .a0 = REPLY};
    complete_frame(&frame, ABI_BAD_HANDLE);
    frame.a0 = REPLY + 1;
    complete_frame(&frame, ABI_OK);

    /* CALL 进入 WAITING：重复分发不重入后端，续完只写一次调用帧。 */
    frame = (struct trap_frame){.scause = EXC_ECALL_U, .sepc = 100, .a7 = ABI_IPC_CALL,
                                .a0 = ENDPOINT, .a1 = 0x32, .a2 = 0xdead};
    unsigned before = fixture.invokes;
    CHECK(syscall_dispatch(&fake_call, &frame, CLIENT, &fake_backend) == SYSCALL_WAITING);
    CHECK(frame.sepc == 104 && fixture.invokes == before + 1 && fixture.pins == 1);
    CHECK(fixture.sent.tag == 0x32 && fixture.sent.words[0] == 0xdead);
    CHECK(syscall_dispatch(&fake_call, &frame, CLIENT, &fake_backend) == SYSCALL_WAITING);
    CHECK(syscall_resume(&fake_call, ABI_OK, (struct abi_message){.tag = 55, .words = {12}}));
    CHECK(!syscall_resume(&fake_call, ABI_OK, (struct abi_message){0}));
    CHECK(fixture.invokes == before + 1 && fixture.pins == 0);
    CHECK(frame.sepc == 104 && frame.a0 == 0 && frame.a1 == 55 && frame.a2 == 12);
    demo_log("dispatch: rejected frames, map/unmap, one-shot reply, delayed completion");
}

/* ---------------- 全链路用例：真实模块组成的后端 ---------------- */

enum {
    CALLER = 0,
    CAPTYPE_ENDPOINT = 1,
    CAPTYPE_SPACE,
    CAPTYPE_FRAME,
};
enum {
    SLOT_ENDPOINT = 1,
    SLOT_RECV_ENDPOINT,
    SLOT_SPACE,
    SLOT_SPACE_RO,
    SLOT_FRAME,
    SLOT_FRAME_RO,
};
enum { TAG_CALL = 0x32, TAG_CALL_REPLY = 0x34, TAG_SEND = 0x36, TAG_RECV = 0x38 };

struct frame_payload {
    pfn_t pfn;
};

struct endpoint_op {
    bool call;
    ipc_message message;
    ipc_message reply;
    int result;
};

struct space_op {
    vaddr_t va;
    pfn_t pfn;
    int rights;
    bool unmap;
    int result;
};

static int endpoint_invoke(struct cap_object *object, uint32_t op, void *args)
{
    struct endpoint *endpoint = object->payload;
    struct endpoint_op *request = args;

    switch (op) {
    case CAP_OP_SEND:
        request->result = request->call
            ? ep_call(endpoint, &request->message, &request->reply)
            : ep_send(endpoint, &request->message);
        break;
    case CAP_OP_RECV:
        request->result = ep_recv(endpoint, &request->reply);
        break;
    default:
        return CAP_E_TYPE;
    }
    return CAP_OK;
}

static int space_invoke(struct cap_object *object, uint32_t op, void *args)
{
    struct as *space = object->payload;
    struct space_op *request = args;

    if (op != CAP_OP_MAP)
        return CAP_E_TYPE;
    request->result = request->unmap
        ? as_unmap(space, request->va)
        : as_map(space, request->va, request->pfn, request->rights);
    return CAP_OK;
}

static int frame_invoke(struct cap_object *object, uint32_t op, void *args)
{
    struct frame_payload *frame = object->payload;
    struct space_op *request = args;

    if (op != CAP_OP_MAP)
        return CAP_E_TYPE;
    request->pfn = frame->pfn;
    return CAP_OK;
}

static const struct cap_object_ops endpoint_ops = {.invoke = endpoint_invoke};
static const struct cap_object_ops space_ops = {.invoke = space_invoke};
static const struct cap_object_ops frame_ops = {.invoke = frame_invoke};

static const struct cap_type endpoint_type = {
    .type = CAPTYPE_ENDPOINT,
    .rights_mask = CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT,
    .supported = (1u << CAP_OP_SEND) | (1u << CAP_OP_RECV),
    .required = {
        [CAP_OP_SEND] = CAP_RIGHT_WRITE,
        [CAP_OP_RECV] = CAP_RIGHT_READ,
    },
    .ops = &endpoint_ops,
};

static const struct cap_type space_type = {
    .type = CAPTYPE_SPACE,
    .rights_mask = CAP_RIGHT_READ | CAP_RIGHT_MAP,
    .supported = 1u << CAP_OP_MAP,
    .required = {[CAP_OP_MAP] = CAP_RIGHT_MAP},
    .ops = &space_ops,
};

static const struct cap_type frame_type = {
    .type = CAPTYPE_FRAME,
    .rights_mask = CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_EXEC | CAP_RIGHT_MAP,
    .supported = 1u << CAP_OP_MAP,
    .required = {[CAP_OP_MAP] = CAP_RIGHT_MAP},
    .ops = &frame_ops,
};

struct kernel_fixture {
    struct thread runner;
    struct as *space;
    pfn_t frame;
    struct frame_payload frame_payload;
    struct endpoint *endpoint;
    struct endpoint *recv_endpoint;
    unsigned acquires, releases, invokes, served;
    bool mapped;
    vaddr_t mapped_va;
};

static struct kernel_fixture kernel;

struct user_env {
    cap_handle_t endpoint;
    cap_handle_t recv_endpoint;
    cap_handle_t space;
    cap_handle_t space_ro;
    cap_handle_t frame_rw;
    cap_handle_t frame_ro;
    vaddr_t map_va;
};

static struct user_env env;

static int memory_rights(uint64_t abi)
{
    int rights = 0;

    if (abi & ABI_READ)
        rights |= MEM_R;
    if (abi & ABI_WRITE)
        rights |= MEM_W;
    if (abi & ABI_EXEC)
        rights |= MEM_X;
    return rights;
}

static int64_t status_from_cap(int rc)
{
    switch (rc) {
    case CAP_OK:
        return ABI_OK;
    case CAP_E_BAD_SLOT:
    case CAP_E_BAD_HANDLE:
    case CAP_E_BAD_OBJECT:
    case CAP_E_TYPE:
        return ABI_BAD_HANDLE;
    case CAP_E_RIGHTS:
        return ABI_DENIED;
    case CAP_E_NOMEM:
        return ABI_NO_MEMORY;
    case CAP_E_BUSY:
        return ABI_BUSY;
    case CAP_E_BAD_ARG:
        return ABI_BAD_ARG;
    default:
        return ABI_DENIED;
    }
}

static int64_t status_from_ipc(int rc)
{
    switch (rc) {
    case IPC_OK:
        return ABI_OK;
    case IPC_PENDING:
        return ABI_BUSY;
    case IPC_EDEAD:
        return ABI_CLOSED;
    case IPC_EBUSY:
        return ABI_BUSY;
    case IPC_EINVAL:
        return ABI_BAD_ARG;
    case IPC_ENOMEM:
        return ABI_NO_MEMORY;
    default:
        return ABI_DENIED;
    }
}

static int64_t status_from_memory(int rc)
{
    switch (rc) {
    case MEM_OK:
        return ABI_OK;
    case MEM_EINVAL:
        return ABI_BAD_ARG;
    case MEM_ENOMEM:
        return ABI_NO_MEMORY;
    case MEM_EEXIST:
        return ABI_ALREADY_MAPPED;
    case MEM_EFAULT:
        return ABI_NOT_MAPPED;
    case MEM_EPERM:
        return ABI_DENIED;
    default:
        return ABI_DENIED;
    }
}

static cap_type_t cap_type_for(enum syscall_kind kind)
{
    switch (kind) {
    case SYSCALL_ENDPOINT:
        return CAPTYPE_ENDPOINT;
    case SYSCALL_AS:
        return CAPTYPE_SPACE;
    case SYSCALL_FRAME:
        return CAPTYPE_FRAME;
    default:
        return 0;
    }
}

static cap_rights_t cap_rights_for(enum syscall_kind kind, unsigned rights)
{
    cap_rights_t need = 0;

    switch (kind) {
    case SYSCALL_ENDPOINT:
        if (rights & SYSCALL_SEND)
            need |= CAP_RIGHT_WRITE;
        if (rights & SYSCALL_RECV)
            need |= CAP_RIGHT_READ;
        break;
    case SYSCALL_AS:
        need = CAP_RIGHT_MAP;
        break;
    case SYSCALL_FRAME:
        need = CAP_RIGHT_MAP;
        if (rights & ABI_READ)
            need |= CAP_RIGHT_READ;
        if (rights & ABI_WRITE)
            need |= CAP_RIGHT_WRITE;
        if (rights & ABI_EXEC)
            need |= CAP_RIGHT_EXEC;
        break;
    case SYSCALL_REPLY:
        need = CAP_RIGHT_GRANT;
        break;
    default:
        break;
    }
    return need;
}

static int64_t kernel_acquire(void *ctx, uint32_t caller, uint32_t handle,
                              enum syscall_kind kind, unsigned rights, uint32_t *ref)
{
    struct kernel_fixture *test = ctx;
    struct cap_info info;

    if (caller != CALLER)
        return ABI_BAD_HANDLE;
    if (cap_info((cap_sid_t)caller, handle, &info) != CAP_OK)
        return ABI_BAD_HANDLE;
    if (info.type != cap_type_for(kind))
        return ABI_BAD_HANDLE;
    if (cap_rights_for(kind, rights) & ~info.rights)
        return ABI_DENIED;

    test->acquires++;
    *ref = handle;
    return ABI_OK;
}

static void kernel_release(void *ctx, uint32_t ref)
{
    struct kernel_fixture *test = ctx;

    CHECK(ref != 0 && test->releases < test->acquires);
    test->releases++;
}

static enum syscall_phase kernel_invoke(void *ctx, const struct syscall_request *request,
                                        int64_t *status, struct abi_message *result)
{
    struct kernel_fixture *test = ctx;
    int rc;

    test->invokes++;
    switch (request->nr) {
    case ABI_YIELD:
        thread_yield();
        *status = ABI_OK;
        break;

    case ABI_THREAD_EXIT:
        *status = ABI_OK;
        return SYSCALL_EXITED;

    case ABI_IPC_SEND:
    case ABI_IPC_CALL: {
        struct abi_message message = abi_decode_message(request->args);
        struct endpoint_op op = {
            .call = request->nr == ABI_IPC_CALL,
            .message = {
                .tag = message.tag,
                .words = {message.words[0], message.words[1],
                          message.words[2], message.words[3]},
            },
        };

        rc = cap_invoke(CALLER, request->refs[0], CAP_OP_SEND, &op);
        *status = rc == CAP_OK ? status_from_ipc(op.result) : status_from_cap(rc);
        if (*status == ABI_OK) {
            result->tag = op.reply.tag;
            for (unsigned i = 0; i < ABI_WORDS; i++)
                result->words[i] = op.reply.words[i];
        }
        break;
    }

    case ABI_IPC_RECV: {
        struct endpoint_op op = {0};

        rc = cap_invoke(CALLER, request->refs[0], CAP_OP_RECV, &op);
        *status = rc == CAP_OK ? status_from_ipc(op.result) : status_from_cap(rc);
        if (*status == ABI_OK) {
            result->tag = op.reply.tag;
            for (unsigned i = 0; i < ABI_WORDS; i++)
                result->words[i] = op.reply.words[i];
        }
        break;
    }

    case ABI_IPC_REPLY:
        /* 回复令牌只发给接收者；本 demo 的服务线程直接调用 ep_reply。 */
        *status = ABI_BAD_HANDLE;
        break;

    case ABI_AS_MAP: {
        struct space_op op = {
            .va = request->args.a[1],
            .rights = memory_rights(request->args.a[3]),
        };

        rc = cap_invoke(CALLER, request->refs[1], CAP_OP_MAP, &op);
        if (rc == CAP_OK)
            rc = cap_invoke(CALLER, request->refs[0], CAP_OP_MAP, &op);
        *status = rc == CAP_OK ? status_from_memory(op.result) : status_from_cap(rc);
        if (*status == ABI_OK) {
            test->mapped = true;
            test->mapped_va = op.va;
        }
        break;
    }

    case ABI_AS_UNMAP: {
        struct space_op op = {.va = request->args.a[1], .unmap = true};

        rc = cap_invoke(CALLER, request->refs[0], CAP_OP_MAP, &op);
        *status = rc == CAP_OK ? status_from_memory(op.result) : status_from_cap(rc);
        if (*status == ABI_OK)
            test->mapped = false;
        break;
    }

    default:
        *status = ABI_BAD_NR;
        break;
    }
    return SYSCALL_COMPLETE;
}

static const struct syscall_backend kernel_backend = {
    .acquire = kernel_acquire, .release = kernel_release, .invoke = kernel_invoke,
};
static struct syscall_call kernel_call;

static _Alignas(PAGE_SIZE) uint8_t kernel_frames[4 * PAGE_SIZE];
static _Alignas(16) uint8_t call_server_stack[4096];
static _Alignas(16) uint8_t send_server_stack[4096];

/* 服务线程：先应答复用 CALL，再接收一次 SEND，然后退出。 */
static void call_server(void *argument)
{
    struct kernel_fixture *test = argument;
    ipc_message request, reply;

    CHECK(ep_recv(test->endpoint, &request) == IPC_OK);
    CHECK(request.tag == TAG_CALL && request.words[0] == 41 && request.reply_id != 0);
    reply = (ipc_message){.tag = TAG_CALL_REPLY, .words = {43}};
    CHECK(ep_reply(request.reply_id, &reply) == IPC_OK);

    CHECK(ep_recv(test->endpoint, &request) == IPC_OK);
    CHECK(request.tag == TAG_SEND && request.words[0] == 7);
    test->served++;
    thread_exit();
}

/* 发送线程：在 RECV 到达前阻塞在 sendq，用来验证真实的唤醒路径。 */
static void send_server(void *argument)
{
    struct kernel_fixture *test = argument;
    ipc_message message = {.tag = TAG_RECV, .words = {0x55}};

    CHECK(ep_send(test->recv_endpoint, &message) == IPC_OK);
    test->served++;
    thread_exit();
}

static void kernel_setup(void)
{
    cap_oid_t endpoint_oid, recv_oid, space_oid, frame_oid;

    object_system_init();
    sched_init();
    ipc_init();
    cap_system_init();
    CHECK(frame_init((paddr_t)(uintptr_t)kernel_frames, sizeof(kernel_frames),
                     kernel_frames) == MEM_OK);

    kernel.space = as_create();
    kernel.frame = frame_alloc();
    kernel.endpoint = ep_create();
    kernel.recv_endpoint = ep_create();
    CHECK(kernel.space && kernel.frame != PFN_NONE &&
          kernel.endpoint && kernel.recv_endpoint);

    CHECK(cap_object_create(&endpoint_type, kernel.endpoint, &endpoint_oid) == CAP_OK);
    CHECK(cap_root_install(CALLER, SLOT_ENDPOINT, endpoint_oid,
                           CAP_RIGHT_READ | CAP_RIGHT_WRITE, 0,
                           &env.endpoint) == CAP_OK);
    CHECK(cap_object_create(&endpoint_type, kernel.recv_endpoint, &recv_oid) == CAP_OK);
    CHECK(cap_root_install(CALLER, SLOT_RECV_ENDPOINT, recv_oid,
                           CAP_RIGHT_READ | CAP_RIGHT_WRITE, 0,
                           &env.recv_endpoint) == CAP_OK);

    CHECK(cap_object_create(&space_type, kernel.space, &space_oid) == CAP_OK);
    CHECK(cap_root_install(CALLER, SLOT_SPACE, space_oid, CAP_RIGHT_MAP, 0,
                           &env.space) == CAP_OK);
    CHECK(cap_root_install(CALLER, SLOT_SPACE_RO, space_oid, CAP_RIGHT_READ, 0,
                           &env.space_ro) == CAP_OK);

    kernel.frame_payload.pfn = kernel.frame;
    CHECK(cap_object_create(&frame_type, &kernel.frame_payload, &frame_oid) == CAP_OK);
    CHECK(cap_root_install(CALLER, SLOT_FRAME, frame_oid,
                           CAP_RIGHT_MAP | CAP_RIGHT_READ | CAP_RIGHT_WRITE, 0,
                           &env.frame_rw) == CAP_OK);
    CHECK(cap_root_install(CALLER, SLOT_FRAME_RO, frame_oid,
                           CAP_RIGHT_MAP | CAP_RIGHT_READ, 0,
                           &env.frame_ro) == CAP_OK);

    env.map_va = 0x4000;
    CHECK(thread_create(NULL, call_server, &kernel, call_server_stack,
                        sizeof(call_server_stack), "call-server"));
    CHECK(thread_create(NULL, send_server, &kernel, send_server_stack,
                        sizeof(send_server_stack), "send-server"));

    kernel.runner.name = "caller";
    sched_bootstrap(&kernel.runner);
}

/*
 * U-mode 程序。句柄来自真实能力空间；映射、IPC 和退出都走 ecall。
 * 这里直接读 kernel 结构体，是 demo 环境（satp=0）的测试便利，
 * 不是用户 ABI 的一部分。
 */
static void user_demo(uintptr_t argument)
{
    const struct user_env *user = (const struct user_env *)argument;
    struct abi_regs regs, out;
    struct abi_message message, reply;
    unsigned before;

    CHECK(abi_decode_status(abi_ecall(ABI_YIELD, (struct abi_regs){0})) == ABI_OK);
    CHECK(abi_decode_status(abi_ecall(99, (struct abi_regs){0})) == ABI_BAD_NR);

    /* 参数错误和权限不足都在后端之前被拒绝。 */
    before = kernel.invokes;
    regs = (struct abi_regs){{user->space, user->map_va + 1, user->frame_rw,
                              ABI_READ | ABI_WRITE}};
    CHECK(abi_decode_status(abi_ecall(ABI_AS_MAP, regs)) == ABI_BAD_ARG);
    regs = (struct abi_regs){{user->space_ro, user->map_va, user->frame_rw, ABI_READ}};
    CHECK(abi_decode_status(abi_ecall(ABI_AS_MAP, regs)) == ABI_DENIED);
    regs = (struct abi_regs){{user->space, user->map_va, user->frame_ro,
                              ABI_READ | ABI_WRITE}};
    CHECK(abi_decode_status(abi_ecall(ABI_AS_MAP, regs)) == ABI_DENIED);
    CHECK(kernel.invokes == before);

    /* 真实页表：地址空间 + 物理帧。 */
    regs = (struct abi_regs){{user->space, user->map_va, user->frame_rw,
                              ABI_READ | ABI_WRITE}};
    CHECK(abi_decode_status(abi_ecall(ABI_AS_MAP, regs)) == ABI_OK);
    CHECK(kernel.mapped && kernel.mapped_va == user->map_va);

    /* CALL 阻塞调用者，服务线程收包并回复。 */
    message = (struct abi_message){.tag = TAG_CALL, .words = {41}};
    out = abi_ecall(ABI_IPC_CALL, abi_message_args(user->endpoint, &message));
    CHECK(abi_decode_status(out) == ABI_OK);
    reply = abi_decode_message(out);
    CHECK(reply.tag == TAG_CALL_REPLY && reply.reply == 0 && reply.words[0] == 43);

    /* SEND 不阻塞：服务线程已经在 recvq 等待；让出 CPU 后它处理第二条消息。 */
    message = (struct abi_message){.tag = TAG_SEND, .words = {7}};
    CHECK(abi_decode_status(abi_ecall(ABI_IPC_SEND,
                                      abi_message_args(user->endpoint, &message))) == ABI_OK);
    CHECK(abi_decode_status(abi_ecall(ABI_YIELD, (struct abi_regs){0})) == ABI_OK);
    CHECK(kernel.served == 1);

    /* RECV 唤醒阻塞在 sendq 的发送线程并拿到消息。 */
    out = abi_ecall(ABI_IPC_RECV, (struct abi_regs){{user->recv_endpoint}});
    CHECK(abi_decode_status(out) == ABI_OK);
    reply = abi_decode_message(out);
    CHECK(reply.tag == TAG_RECV && reply.words[0] == 0x55);

    regs = (struct abi_regs){{user->space, user->map_va}};
    CHECK(abi_decode_status(abi_ecall(ABI_AS_UNMAP, regs)) == ABI_OK);
    CHECK(!kernel.mapped);

    /* 让发送线程结束，核对每个在途引用都已释放。 */
    CHECK(abi_decode_status(abi_ecall(ABI_YIELD, (struct abi_regs){0})) == ABI_OK);
    CHECK(kernel.served == 2);
    CHECK(kernel.acquires == kernel.releases);

    demo_log("U-mode ecall: capability-checked IPC, real map/unmap and thread exit");
    abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    CHECK(false);
}

static struct trap_frame *handle_trap(struct trap_frame *frame)
{
    enum syscall_phase phase = syscall_dispatch(&kernel_call, frame, CALLER, &kernel_backend);

    if (phase == SYSCALL_IGNORED)
        return NULL;
    if (phase == SYSCALL_EXITED)
        demo_pass();
    CHECK(phase == SYSCALL_COMPLETE);
    return frame;
}

static void run(void)
{
    model_cases();
    kernel_setup();
    demo_enter_user(user_demo, (uintptr_t)&env);
}

DEMO_MODULE(syscall, false, run, handle_trap);
