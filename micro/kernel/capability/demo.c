/*
 * demo.c — 能力模块的组合运行示例。
 *
 * cap_selftest() 证明「表项、派生、撤销、生命周期」本身成立；本文件把
 * 能力表放进完整的内核对象链路：
 *
 *   U-mode 形状的陷阱帧
 *     -> syscall 分发表（参数形状校验）
 *     -> capability 后端（按 kind 解析 cap、检查 rights、pin 对象）
 *     -> objects / ipc / memory / sched 的原语
 *
 * 覆盖四件事：
 *   1. 客户端只拿到 SEND 子集 cap，服务端只拿到 RECV 子集 cap；
 *   2. 一次 IPC_CALL 穿过能力检查，经历阻塞、唤醒与单次 reply；
 *   3. AS_MAP / AS_UNMAP 同时校验 AS cap 与 frame cap 的权限；
 *   4. revoke 根 cap 后，跨 CSpace 的派生 cap 同时失效；最后一个 cap
 *      删除后 endpoint / AS / frame 才析构。
 */
#include "syscall/syscall.h"

#include "capability.h"
#include "csr.h"
#include "demo.h"
#include "ipc/ipc.h"
#include "memory/memory.h"
#include "object.h"
#include "sched.h"

enum {
    SID_CLIENT = 0,
    SID_SERVER = 1,
};

enum {
    CAP_TYPE_ENDPOINT = 1,
    CAP_TYPE_AS,
    CAP_TYPE_FRAME,
    CAP_TYPE_REPLY,
};

#define DEMO_VA          0x40000000UL
#define FRAME_POOL_PAGES 8
#define REPLY_POOL_SIZE  4
#define BACKEND_REFS     8

static _Alignas(PAGE_SIZE) uint8_t frame_pool[FRAME_POOL_PAGES * PAGE_SIZE];
static _Alignas(16) uint8_t client_stack[4096], server_stack[4096];

/* ---- 被能力保护的内核对象载荷 -------------------------------------- */

struct frame_holder {
    pfn_t pfn;
};

struct reply_holder {
    uint32_t token;
    bool busy;     /* 是否由某个 cap 对象占用 */
    bool consumed; /* 是否已经应答过；单次 reply */
};

static struct frame_holder frame_holder;
static struct reply_holder reply_pool[REPLY_POOL_SIZE];

/* ---- 测试观测 -------------------------------------------------------- */

struct demo_result {
    int64_t call_status;
    int64_t recv_status;
    int64_t reply_status;
    int64_t second_reply_status;
    int64_t map_status;
    int64_t unmap_status;
    uint32_t reply_tag;
    uint64_t reply_word;
    uint32_t received_tag;
    uint64_t received_word;
    uint32_t received_reply_cap;
    bool client_done;
    bool server_done;
};

struct demo_case {
    struct endpoint *ep;
    uint32_t ep_id;
    cap_oid_t ep_oid, as_oid, frame_oid;
    cap_handle_t root, client_send, server_recv, as_cap, frame_cap;
    struct as *as;
    vaddr_t va;
    pfn_t pfn;
    pfn_t last_mapped_pfn;
    struct demo_result out;
};

static struct demo_case cap_case;

/* ---- cap_type 注入：对象语义留在各自模块 ---------------------------- */

static int ipc_to_cap(int rc)
{
    switch (rc) {
    case IPC_OK:     return CAP_OK;
    case IPC_EDEAD:  return CAP_E_BAD_OBJECT;
    case IPC_EBUSY:  return CAP_E_BUSY;
    case IPC_EINVAL: return CAP_E_BAD_ARG;
    case IPC_ENOMEM: return CAP_E_NOMEM;
    default:         return CAP_E_BUSY;
    }
}

static int endpoint_invoke(struct cap_object *obj, uint32_t op, void *args)
{
    struct endpoint *ep = obj->payload;
    ipc_message *msg = args;

    if (!ep || !msg)
        return CAP_E_BAD_ARG;
    if (op == CAP_OP_SEND)
        return ipc_to_cap(ep_send(ep, msg));
    if (op == CAP_OP_RECV)
        return ipc_to_cap(ep_recv(ep, msg));
    return CAP_E_TYPE;
}

static void endpoint_finalize(struct cap_object *obj)
{
    ep_unref((struct endpoint *)obj->payload);
    obj->payload = NULL;
}

/* AS / frame 的 map 是跨两个 cap 的复合操作，走 cap_acquire，见后端。 */
static int compound_invoke(struct cap_object *obj, uint32_t op, void *args)
{
    (void)obj;
    (void)op;
    (void)args;
    return CAP_E_TYPE;
}

static void as_finalize(struct cap_object *obj)
{
    as_unref((struct as *)obj->payload);
    obj->payload = NULL;
}

static void frame_finalize(struct cap_object *obj)
{
    struct frame_holder *holder = obj->payload;

    if (holder && holder->pfn != PFN_NONE) {
        frame_unref(holder->pfn);
        holder->pfn = PFN_NONE;
    }
}

static int reply_invoke(struct cap_object *obj, uint32_t op, void *args)
{
    struct reply_holder *holder = obj->payload;
    ipc_message *msg = args;

    if (!holder || !msg || op != CAP_OP_SEND)
        return CAP_E_TYPE;
    if (holder->consumed)
        return CAP_E_BUSY;

    int rc = ep_reply(holder->token, msg);
    if (rc == IPC_OK)
        holder->consumed = true;
    return ipc_to_cap(rc);
}

static void reply_finalize(struct cap_object *obj)
{
    struct reply_holder *holder = obj->payload;

    if (holder) {
        holder->token = 0;
        holder->busy = false;
        holder->consumed = false;
    }
}

static const struct cap_object_ops endpoint_ops = {
    .invoke = endpoint_invoke,
    .finalize = endpoint_finalize,
};

static const struct cap_object_ops as_ops = {
    .invoke = compound_invoke,
    .finalize = as_finalize,
};

static const struct cap_object_ops frame_ops = {
    .invoke = compound_invoke,
    .finalize = frame_finalize,
};

static const struct cap_object_ops reply_ops = {
    .invoke = reply_invoke,
    .finalize = reply_finalize,
};

static const struct cap_type endpoint_type = {
    .type = CAP_TYPE_ENDPOINT,
    .rights_mask = CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT,
    .supported = (1u << CAP_OP_SEND) | (1u << CAP_OP_RECV),
    .required = {
        [CAP_OP_SEND] = CAP_RIGHT_WRITE,
        [CAP_OP_RECV] = CAP_RIGHT_READ,
    },
    .ops = &endpoint_ops,
};

static const struct cap_type as_type = {
    .type = CAP_TYPE_AS,
    .rights_mask = CAP_RIGHT_MAP,
    .supported = 0,
    .ops = &as_ops,
};

static const struct cap_type frame_type = {
    .type = CAP_TYPE_FRAME,
    .rights_mask = CAP_RIGHT_READ | CAP_RIGHT_WRITE |
                   CAP_RIGHT_EXEC | CAP_RIGHT_MAP,
    .supported = 0,
    .ops = &frame_ops,
};

static const struct cap_type reply_type = {
    .type = CAP_TYPE_REPLY,
    .rights_mask = CAP_RIGHT_WRITE,
    .supported = 1u << CAP_OP_SEND,
    .required = {
        [CAP_OP_SEND] = CAP_RIGHT_WRITE,
    },
    .ops = &reply_ops,
};

/* ---- syscall 后端：cap 是唯一的句柄解析入口 ------------------------- */

struct backend_ref {
    struct cap_object *obj;
    bool used;
};

struct capability_backend {
    struct backend_ref refs[BACKEND_REFS];
};

static struct capability_backend cap_backend_ctx;

static int64_t cap_error_to_abi(int rc)
{
    switch (rc) {
    case CAP_OK:           return ABI_OK;
    case CAP_E_BAD_HANDLE: return ABI_BAD_HANDLE;
    case CAP_E_RIGHTS:
    case CAP_E_TYPE:       return ABI_DENIED;
    case CAP_E_NOMEM:      return ABI_NO_MEMORY;
    case CAP_E_BUSY:       return ABI_BUSY;
    case CAP_E_BAD_OBJECT: return ABI_CLOSED;
    default:               return ABI_BAD_ARG;
    }
}

static int64_t ipc_error_to_abi(int rc)
{
    switch (rc) {
    case IPC_OK:     return ABI_OK;
    case IPC_EDEAD:  return ABI_CLOSED;
    case IPC_EBUSY:  return ABI_BUSY;
    case IPC_EINVAL: return ABI_BAD_ARG;
    case IPC_ENOMEM: return ABI_NO_MEMORY;
    default:         return ABI_BUSY;
    }
}

static int64_t mem_error_to_abi(int rc)
{
    switch (rc) {
    case MEM_OK:     return ABI_OK;
    case MEM_EEXIST: return ABI_ALREADY_MAPPED;
    case MEM_EFAULT: return ABI_NOT_MAPPED;
    case MEM_EPERM:  return ABI_DENIED;
    case MEM_ENOMEM: return ABI_NO_MEMORY;
    case MEM_EINVAL: return ABI_BAD_ARG;
    default:         return ABI_BAD_ARG;
    }
}

static cap_type_t expected_type(enum syscall_kind kind)
{
    switch (kind) {
    case SYSCALL_ENDPOINT: return CAP_TYPE_ENDPOINT;
    case SYSCALL_AS:       return CAP_TYPE_AS;
    case SYSCALL_FRAME:    return CAP_TYPE_FRAME;
    case SYSCALL_REPLY:    return CAP_TYPE_REPLY;
    default:               return 0;
    }
}

static cap_rights_t endpoint_rights(unsigned syscall_rights)
{
    cap_rights_t rights = 0;

    if (syscall_rights & SYSCALL_SEND)
        rights |= CAP_RIGHT_WRITE;
    if (syscall_rights & SYSCALL_RECV)
        rights |= CAP_RIGHT_READ;
    return rights;
}

static cap_rights_t frame_rights(unsigned abi_rights)
{
    cap_rights_t rights = CAP_RIGHT_MAP;

    if (abi_rights & ABI_READ)
        rights |= CAP_RIGHT_READ;
    if (abi_rights & ABI_WRITE)
        rights |= CAP_RIGHT_WRITE;
    if (abi_rights & ABI_EXEC)
        rights |= CAP_RIGHT_EXEC;
    return rights;
}

static int abi_to_mem_rights(unsigned abi_rights)
{
    int rights = 0;

    if (abi_rights & ABI_READ)
        rights |= MEM_R;
    if (abi_rights & ABI_WRITE)
        rights |= MEM_W;
    if (abi_rights & ABI_EXEC)
        rights |= MEM_X;
    return rights;
}

static struct cap_object *backend_lookup(struct capability_backend *backend,
                                         uint32_t ref)
{
    if (ref == 0 || ref > BACKEND_REFS)
        return NULL;
    struct backend_ref *slot = &backend->refs[ref - 1];

    return slot->used ? slot->obj : NULL;
}

static int64_t backend_acquire(void *ctx, uint32_t caller, uint32_t handle,
                               enum syscall_kind kind, unsigned rights,
                               uint32_t *ref)
{
    struct capability_backend *backend = ctx;
    struct cap_object *obj = NULL;
    cap_rights_t need;
    int rc;

    if (caller >= CAP_MAX_TASKS)
        return ABI_BAD_HANDLE;

    switch (kind) {
    case SYSCALL_ENDPOINT:
        need = endpoint_rights(rights);
        break;
    case SYSCALL_AS:
        need = CAP_RIGHT_MAP;
        break;
    case SYSCALL_FRAME:
        need = frame_rights(rights);
        break;
    case SYSCALL_REPLY:
        need = CAP_RIGHT_WRITE;
        break;
    default:
        return ABI_BAD_ARG;
    }
    if (need == 0)
        return ABI_BAD_ARG;

    for (uint32_t i = 0; i < BACKEND_REFS; i++) {
        if (backend->refs[i].used)
            continue;
        rc = cap_acquire((cap_sid_t)caller, (cap_handle_t)handle, need, &obj);
        if (rc != CAP_OK)
            return cap_error_to_abi(rc);
        if (obj->type->type != expected_type(kind)) {
            cap_release(obj);
            return ABI_DENIED;
        }
        backend->refs[i].obj = obj;
        backend->refs[i].used = true;
        *ref = i + 1;
        return ABI_OK;
    }
    return ABI_NO_MEMORY;
}

static void backend_release(void *ctx, uint32_t ref)
{
    struct capability_backend *backend = ctx;
    struct cap_object *obj;

    if (ref == 0 || ref > BACKEND_REFS)
        return;
    struct backend_ref *slot = &backend->refs[ref - 1];

    if (!slot->used)
        return;
    obj = slot->obj;
    slot->obj = NULL;
    slot->used = false;
    cap_release(obj);
}

static ipc_message ipc_from_abi(const struct abi_regs *args)
{
    ipc_message msg = {0};

    msg.tag = (uint32_t)args->a[1];
    for (unsigned i = 0; i < ABI_WORDS; i++)
        msg.words[i] = args->a[i + 2];
    return msg;
}

static struct abi_message abi_from_ipc(const ipc_message *msg,
                                       uint32_t reply)
{
    struct abi_message out = {
        .tag = msg->tag,
        .reply = reply,
    };

    for (unsigned i = 0; i < ABI_WORDS; i++)
        out.words[i] = msg->words[i];
    return out;
}

/*
 * 内核签发 reply 权限：接收方拿到一把单次使用的 reply cap，
 * 而不是 ipc 模块内部的 bearer token。
 */
static int install_reply_cap(cap_sid_t sid, uint32_t token, uint32_t *handle)
{
    struct reply_holder *holder = NULL;
    cap_oid_t oid = 0;
    int rc;

    for (uint32_t i = 0; i < REPLY_POOL_SIZE; i++) {
        if (!reply_pool[i].busy) {
            holder = &reply_pool[i];
            break;
        }
    }
    if (!holder)
        return CAP_E_NOMEM;

    holder->token = token;
    holder->busy = true;
    holder->consumed = false;

    rc = cap_object_create(&reply_type, holder, &oid);
    if (rc != CAP_OK) {
        holder->busy = false;
        return rc;
    }

    rc = cap_root_install(sid, CAP_SLOT_AUTO, oid, CAP_RIGHT_WRITE, 0, handle);
    if (rc != CAP_OK)
        (void)cap_object_discard(oid); /* finalize 把 holder 还给池子 */
    return rc;
}

static enum syscall_phase backend_invoke(void *ctx,
                                         const struct syscall_request *request,
                                         int64_t *status,
                                         struct abi_message *result)
{
    struct capability_backend *backend = ctx;
    struct cap_object *first = backend_lookup(backend, request->refs[0]);
    struct cap_object *second = backend_lookup(backend, request->refs[1]);
    int rc;

    *status = ABI_OK;
    switch (request->nr) {
    case ABI_YIELD:
        thread_yield();
        break;

    case ABI_THREAD_EXIT:
        return SYSCALL_EXITED;

    case ABI_IPC_SEND: {
        ipc_message msg = ipc_from_abi(&request->args);

        if (!first) {
            *status = ABI_BAD_HANDLE;
            break;
        }
        *status = ipc_error_to_abi(
            ep_send((struct endpoint *)first->payload, &msg));
        break;
    }

    case ABI_IPC_CALL: {
        ipc_message msg = ipc_from_abi(&request->args);
        ipc_message reply = {0};

        if (!first) {
            *status = ABI_BAD_HANDLE;
            break;
        }
        rc = ep_call((struct endpoint *)first->payload, &msg, &reply);
        *status = ipc_error_to_abi(rc);
        if (rc == IPC_OK)
            *result = abi_from_ipc(&reply, 0);
        break;
    }

    case ABI_IPC_RECV: {
        ipc_message msg = {0};
        uint32_t reply_cap = 0;

        if (!first) {
            *status = ABI_BAD_HANDLE;
            break;
        }
        rc = ep_recv((struct endpoint *)first->payload, &msg);
        *status = ipc_error_to_abi(rc);
        if (rc != IPC_OK)
            break;
        if (msg.reply_id != 0) {
            rc = install_reply_cap((cap_sid_t)request->caller, msg.reply_id,
                                   &reply_cap);
            if (rc != CAP_OK) {
                *status = cap_error_to_abi(rc);
                break;
            }
        }
        *result = abi_from_ipc(&msg, reply_cap);
        break;
    }

    case ABI_IPC_REPLY: {
        struct reply_holder *holder;
        ipc_message msg = ipc_from_abi(&request->args);

        if (!first) {
            *status = ABI_BAD_HANDLE;
            break;
        }
        holder = first->payload;
        if (holder->consumed) {
            *status = ABI_BUSY;
            break;
        }
        rc = ep_reply(holder->token, &msg);
        if (rc != IPC_OK) {
            *status = ipc_error_to_abi(rc);
            break;
        }
        holder->consumed = true;
        /* 单次 reply：成功即收回这把 cap；pin 让 finalize 推迟到 release。 */
        (void)cap_delete((cap_sid_t)request->caller,
                         (cap_handle_t)request->args.a[0]);
        *status = ABI_OK;
        break;
    }

    case ABI_AS_MAP: {
        struct as *as;
        struct frame_holder *frame;
        vaddr_t va = (vaddr_t)request->args.a[1];
        int rights;

        if (!first || !second) {
            *status = ABI_BAD_HANDLE;
            break;
        }
        as = first->payload;
        frame = second->payload;
        rights = abi_to_mem_rights((unsigned)request->args.a[3]);
        rc = as_map(as, va, frame->pfn, rights);
        *status = mem_error_to_abi(rc);
        if (rc == MEM_OK) {
            int got_rights = 0;
            pfn_t got_pfn = PFN_NONE;

            if (as_translate(as, va, &got_rights, &got_pfn) == MEM_OK)
                cap_case.last_mapped_pfn = got_pfn;
        }
        break;
    }

    case ABI_AS_UNMAP:
        if (!first) {
            *status = ABI_BAD_HANDLE;
            break;
        }
        *status = mem_error_to_abi(
            as_unmap((struct as *)first->payload,
                     (vaddr_t)request->args.a[1]));
        break;

    default:
        *status = ABI_BAD_NR;
        break;
    }
    return SYSCALL_COMPLETE;
}

static const struct syscall_backend cap_backend = {
    .ctx = &cap_backend_ctx,
    .acquire = backend_acquire,
    .release = backend_release,
    .invoke = backend_invoke,
};

/* ---- 两个主体：客户端与服务端 --------------------------------------- */

static struct trap_frame make_frame(enum abi_number nr)
{
    /*
     * 形状与 U-mode ecall 一致：scause=8，sstatus.SPP=0。
     * sepc 只在真实 trap 返回时需要；这里验证的是分发与能力检查。
     */
    struct trap_frame frame = {0};

    frame.scause = EXC_ECALL_U;
    frame.a7 = (uint64_t)nr;
    return frame;
}

static int64_t frame_status(const struct trap_frame *frame)
{
    struct abi_regs out = {{
        frame->a0, frame->a1, frame->a2, frame->a3, frame->a4, frame->a5,
    }};

    return abi_decode_status(out);
}

static void client_thread(void *argument)
{
    struct demo_case *test = argument;
    struct syscall_call call = {0};
    struct trap_frame frame;

    frame = make_frame(ABI_IPC_CALL);
    frame.a0 = test->client_send;
    frame.a1 = 7;  /* tag */
    frame.a2 = 41; /* w0 */
    (void)syscall_dispatch(&call, &frame, SID_CLIENT, &cap_backend);
    test->out.call_status = frame_status(&frame);
    test->out.reply_tag = (uint32_t)frame.a1;
    test->out.reply_word = frame.a2;

    call = (struct syscall_call){0};
    frame = make_frame(ABI_AS_MAP);
    frame.a0 = test->as_cap;
    frame.a1 = test->va;
    frame.a2 = test->frame_cap;
    frame.a3 = ABI_READ | ABI_WRITE;
    (void)syscall_dispatch(&call, &frame, SID_CLIENT, &cap_backend);
    test->out.map_status = frame_status(&frame);

    call = (struct syscall_call){0};
    frame = make_frame(ABI_AS_UNMAP);
    frame.a0 = test->as_cap;
    frame.a1 = test->va;
    (void)syscall_dispatch(&call, &frame, SID_CLIENT, &cap_backend);
    test->out.unmap_status = frame_status(&frame);

    test->out.client_done = true;
}

static void server_thread(void *argument)
{
    struct demo_case *test = argument;
    struct syscall_call call = {0};
    struct trap_frame frame;

    frame = make_frame(ABI_IPC_RECV);
    frame.a0 = test->server_recv;
    (void)syscall_dispatch(&call, &frame, SID_SERVER, &cap_backend);
    test->out.recv_status = frame_status(&frame);
    if (test->out.recv_status != ABI_OK) {
        test->out.server_done = true;
        return;
    }

    test->out.received_tag = (uint32_t)frame.a1;
    test->out.received_word = frame.a2;
    test->out.received_reply_cap = (uint32_t)(frame.a1 >> 32);

    call = (struct syscall_call){0};
    frame = make_frame(ABI_IPC_REPLY);
    frame.a0 = test->out.received_reply_cap;
    frame.a1 = 8;
    frame.a2 = 42;
    (void)syscall_dispatch(&call, &frame, SID_SERVER, &cap_backend);
    test->out.reply_status = frame_status(&frame);

    /* 第二次使用同一把 reply cap：内核已在首次应答后收回。 */
    call = (struct syscall_call){0};
    frame = make_frame(ABI_IPC_REPLY);
    frame.a0 = test->out.received_reply_cap;
    frame.a1 = 8;
    frame.a2 = 42;
    (void)syscall_dispatch(&call, &frame, SID_SERVER, &cap_backend);
    test->out.second_reply_status = frame_status(&frame);

    test->out.server_done = true;
}

/* ---- 组装：对象 + 权限 + 两个主体 ------------------------------------ */

static void setup_case(void)
{
    struct demo_case *test = &cap_case;
    cap_handle_t recv_tmp = 0;

    test->ep = ep_create();
    CHECK(test->ep != NULL);
    test->ep_id = ep_id(test->ep);
    CHECK(cap_object_create(&endpoint_type, test->ep, &test->ep_oid) == CAP_OK);

    /* 根 cap 留在客户端；两个子集 cap 分别给发送方和接收方。 */
    CHECK(cap_root_install(SID_CLIENT, 0, test->ep_oid,
                           CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_GRANT,
                           0, &test->root) == CAP_OK);
    CHECK(cap_mint(SID_CLIENT, test->root, 1,
                   CAP_RIGHT_READ | CAP_RIGHT_GRANT, 0, &recv_tmp) == CAP_OK);
    CHECK(cap_move(SID_CLIENT, recv_tmp, SID_SERVER, 0,
                   &test->server_recv) == CAP_OK);
    CHECK(cap_mint(SID_CLIENT, test->root, 2,
                   CAP_RIGHT_WRITE, 0, &test->client_send) == CAP_OK);

    test->as = as_create();
    CHECK(test->as != NULL);
    CHECK(cap_object_create(&as_type, test->as, &test->as_oid) == CAP_OK);
    CHECK(cap_root_install(SID_CLIENT, 3, test->as_oid,
                           CAP_RIGHT_MAP, 0, &test->as_cap) == CAP_OK);

    test->pfn = frame_alloc();
    CHECK(test->pfn != PFN_NONE);
    frame_holder.pfn = test->pfn;
    CHECK(cap_object_create(&frame_type, &frame_holder,
                            &test->frame_oid) == CAP_OK);
    CHECK(cap_root_install(SID_CLIENT, 4, test->frame_oid,
                           CAP_RIGHT_READ | CAP_RIGHT_WRITE | CAP_RIGHT_MAP,
                           0, &test->frame_cap) == CAP_OK);

    test->va = DEMO_VA;
    test->last_mapped_pfn = PFN_NONE;
}

static void check_revocation(void)
{
    struct demo_case *test = &cap_case;
    struct cap_info info;

    /* 根 cap 的 revoke 收起两条跨 CSpace 的子边。 */
    CHECK(cap_revoke(SID_CLIENT, test->root) == CAP_OK);
    CHECK(cap_info(SID_CLIENT, test->client_send, &info) == CAP_E_BAD_HANDLE);
    CHECK(cap_info(SID_SERVER, test->server_recv, &info) == CAP_E_BAD_HANDLE);
    CHECK(cap_info(SID_CLIENT, test->root, &info) == CAP_OK);
    CHECK(cap_object_refs(test->ep_oid) == 1);

    CHECK(cap_delete(SID_CLIENT, test->root) == CAP_OK);
    CHECK(object_lookup(test->ep_id, OBJ_ENDPOINT) == NULL);

    /* 先删 frame cap：最后一个 cap 消失，帧才释放。 */
    CHECK(cap_delete(SID_CLIENT, test->frame_cap) == CAP_OK);
    CHECK(frame_refcount(test->pfn) == 0);
    CHECK(frame_holder.pfn == PFN_NONE);

    /* 再删 AS cap：页表页随 AS 一起回收。 */
    CHECK(cap_delete(SID_CLIENT, test->as_cap) == CAP_OK);
}

static void run(void)
{
    struct demo_case *test = &cap_case;
    struct thread runner = {.name = "bootstrap"};
    uint64_t free_frames;
    int rights = 0;
    pfn_t pfn = PFN_NONE;

    CHECK(cap_selftest() == 0);

    object_system_init();
    sched_init();
    sched_bootstrap(&runner);
    ipc_init();
    cap_system_init();
    CHECK(frame_init((paddr_t)(uintptr_t)frame_pool, sizeof(frame_pool),
                     frame_pool) == MEM_OK);
    free_frames = frame_free_count();

    setup_case();
    CHECK(thread_create(NULL, client_thread, test, client_stack,
                        sizeof(client_stack), "cap-client") != NULL);
    CHECK(thread_create(NULL, server_thread, test, server_stack,
                        sizeof(server_stack), "cap-server") != NULL);

    for (unsigned i = 0; i < 32 &&
                         !(test->out.client_done && test->out.server_done); i++)
        thread_yield();

    CHECK(test->out.client_done && test->out.server_done);
    CHECK(test->out.call_status == ABI_OK);
    CHECK(test->out.recv_status == ABI_OK);
    CHECK(test->out.received_tag == 7 && test->out.received_word == 41);
    CHECK(test->out.received_reply_cap != 0);
    CHECK(test->out.reply_status == ABI_OK);
    CHECK(test->out.second_reply_status == ABI_BAD_HANDLE);
    CHECK(test->out.reply_tag == 8 && test->out.reply_word == 42);
    CHECK(test->out.map_status == ABI_OK);
    CHECK(test->out.unmap_status == ABI_OK);
    CHECK(test->last_mapped_pfn == test->pfn);
    CHECK(as_translate(test->as, test->va, &rights, &pfn) == MEM_EFAULT);

    check_revocation();
    CHECK(frame_free_count() == free_frames);
    CHECK(cap_live_caps() == 0);
    CHECK(cap_live_objects() == 0);
    CHECK(cap_check_invariants() == CAP_OK);

    demo_log("capability: caps gate IPC call/reply, AS map and cross-CSpace revoke");
}

DEMO_MODULE(capability, false, run, NULL);
