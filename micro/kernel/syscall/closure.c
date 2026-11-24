/*
 * closure.c — 统一 trap / 启动路径上的最小真实闭环。
 *
 * 这里把前面几个模块的 API 接成一条真实路径，而不是在宿主上互调：
 *
 *   S 态 timer 中断 -> shared/demo.c 的统一 glue
 *                   -> clock_handle_interrupt() -> sched_tick()
 *                   -> sched_check() -> sched() -> cpu_switch()
 *
 *   U 态 ecall      -> 同一个 shared/demo.c trap_handler
 *                   -> active_module->trap (本文件)
 *                   -> syscall_dispatch()
 *                   -> SEND/RECV 走 capability 句柄检查
 *                      REPLY 走内核签发的单次 bearer token（ep_reply 校验）
 *
 * 两个用户线程各自持有独立 AS。它们执行同一份从内核镜像拷贝到用户页的
 * U-mode 程序；A 的 AS 映射 probe VA，B 的 AS 故意不映射，从而在真实
 * satp 下留下一次 page fault。
 *
 * 本文件只依赖已存在的 memory/ipc/syscall/sched/clock API；新增的连接是
 * 用户线程首次从 U 态启动的 trap frame、内核根页表的恒等映射，以及
 * “模块 trap 先观察、统一 glue 兜底”的 timer 分派。
 */
#include "demo.h"
#include "syscall.h"

#include "capability.h"
#include "clock/clock.h"
#include "csr.h"
#include "hart.h"
#include "ipc.h"
#include "memory.h"
#include "sched.h"
#include "sbi.h"
#include "trap.h"

extern char __kernel_start[], __kernel_end[];
extern char user_prog_start[], user_prog_end[];

#define SID_CLIENT 0
#define SID_SERVER 1

#define CAP_TYPE_ENDPOINT 1

#define USER_CODE_VA    0x40000000UL
#define USER_STACK_VA   0x40100000UL
#define USER_DATA_VA    0x40200000UL
#define CLOSURE_POOL    (2UL * 1024 * 1024)
#define KSTACK_BYTES    8192
#define FRAME_TOP_RSV   2048 /* 顶部留给 thread_bootstrap 的 S 态栈/trap 帧 */
#define CLOSURE_TICK_NS 1000000ULL
#define BACKEND_REFS    8

enum { TAG_CALL = 0x101, TAG_REPLY = 0x102 };

#define MAGIC_WRITE 0x5a5a5a5a5a5a5a5aULL
#define MAGIC_REPLY 0x5a5a5a5a5a5a5a5bULL

struct closure_backend_ref {
    struct cap_object *obj;
    bool used;
};

struct closure_backend {
    struct closure_backend_ref refs[BACKEND_REFS];
};

struct uctx {
    _Alignas(16) uint8_t kstack[KSTACK_BYTES];
    struct trap_frame *frame;
    struct thread *thread;
    struct as *as;
    struct syscall_call call;
    cap_handle_t handle;
    uint64_t root_pfn;
    uint64_t satp_seen;
    const char *name;
    uint32_t sid;
    uint32_t role;
    bool exited;
    bool call_ok;
    bool recv_ok;
    bool reply_ok;
};

/* demo.c 的 handle_trap 用它决定是否把 trap 交给本闭环。 */
bool closure_active;

static struct uctx users[2];
static struct thread runner;
static struct as *kernel_as;
static struct endpoint *endpoint;
static struct closure_backend backend;
static pfn_t code_pfn;
static uint64_t last_satp;
static unsigned completed;
static unsigned illegal_calls;
static unsigned unexpected_errors;
static bool illegal_ok;
static bool fault_seen;
static bool fault_ok;
static bool precheck_a_mapped;
static bool precheck_b_unmapped;
static uint64_t fault_stval;
static uint64_t fault_satp;

/* ---- 日志：只做定长数值格式化，不引入 printf ---------------------- */

static char *put_str(char *p, const char *s)
{
    while (*s)
        *p++ = *s++;
    return p;
}

static char *put_hex(char *p, uint64_t v)
{
    static const char digits[] = "0123456789abcdef";

    *p++ = '0';
    *p++ = 'x';
    for (int shift = 60; shift >= 0; shift -= 4)
        *p++ = digits[(v >> shift) & 0xf];
    return p;
}

static char *put_dec(char *p, uint64_t v)
{
    char tmp[24];
    unsigned n = 0;

    if (v == 0) {
        *p++ = '0';
        return p;
    }
    while (v != 0) {
        tmp[n++] = (char)('0' + v % 10);
        v /= 10;
    }
    while (n != 0)
        *p++ = tmp[--n];
    return p;
}

static void log_line(char *buf, char *end)
{
    *end = '\0';
    demo_log(buf);
}

static void log_satp_change(const struct uctx *u, const char *where)
{
    uint64_t satp = csr_read(CSR_SATP);
    uint64_t root = satp & ((1ULL << 44) - 1);
    char buf[192];
    char *p = buf;

    if (satp == last_satp)
        return;

    p = put_str(p, "[as] satp switch ");
    p = put_hex(p, last_satp);
    p = put_str(p, " -> ");
    p = put_hex(p, satp);
    p = put_str(p, " root=");
    p = put_hex(p, root);
    p = put_str(p, " at ");
    p = put_str(p, where);
    if (u != NULL) {
        p = put_str(p, " ");
        p = put_str(p, u->name);
    }
    log_line(buf, p);
    last_satp = satp;
}

static char *put_label(char *p, const char *label)
{
    const char *end = label;

    p = put_str(p, label);
    while (*end != '\0')
        end++;
    if (end == label || end[-1] != '=')
        *p++ = '=';
    return p;
}

static void log_hex_field(const char *prefix, const char *name,
                          const char *field, uint64_t value)
{
    char buf[192];
    char *p = buf;

    p = put_str(p, prefix);
    if (name != NULL) {
        p = put_str(p, name);
        p = put_str(p, " ");
    }
    p = put_label(p, field);
    p = put_hex(p, value);
    log_line(buf, p);
}

static void log_dec_field(const char *prefix, const char *name,
                          const char *field, uint64_t value)
{
    char buf[192];
    char *p = buf;

    p = put_str(p, prefix);
    if (name != NULL) {
        p = put_str(p, name);
        p = put_str(p, " ");
    }
    p = put_label(p, field);
    p = put_dec(p, value);
    log_line(buf, p);
}

static struct uctx *user_for(struct thread *t)
{
    for (unsigned i = 0; i < 2; i++) {
        if (users[i].thread == t)
            return &users[i];
    }
    return NULL;
}

static int64_t frame_status(const struct trap_frame *frame)
{
    struct abi_regs regs = {{
        frame->a0, frame->a1, frame->a2, frame->a3, frame->a4, frame->a5,
    }};

    return abi_decode_status(regs);
}

/* ---- 最小 endpoint capability：只保护 SEND/RECV ------------------ */

static int endpoint_invoke(struct cap_object *obj, uint32_t op, void *args)
{
    (void)obj;
    (void)op;
    (void)args;
    return CAP_E_TYPE;
}

static void endpoint_finalize(struct cap_object *obj)
{
    ep_unref((struct endpoint *)obj->payload);
    obj->payload = NULL;
}

static const struct cap_object_ops endpoint_ops = {
    .invoke = endpoint_invoke,
    .finalize = endpoint_finalize,
};

static const struct cap_type endpoint_type = {
    .type = CAP_TYPE_ENDPOINT,
    .rights_mask = CAP_RIGHT_READ | CAP_RIGHT_WRITE,
    .supported = (1u << CAP_OP_SEND) | (1u << CAP_OP_RECV),
    .required = {
        [CAP_OP_SEND] = CAP_RIGHT_WRITE,
        [CAP_OP_RECV] = CAP_RIGHT_READ,
    },
    .ops = &endpoint_ops,
};

static int64_t cap_error_to_abi(int rc)
{
    switch (rc) {
    case CAP_OK:
        return ABI_OK;
    case CAP_E_NOMEM:
        return ABI_NO_MEMORY;
    case CAP_E_RIGHTS:
    case CAP_E_TYPE:
        return ABI_DENIED;
    case CAP_E_BUSY:
        return ABI_BUSY;
    case CAP_E_BAD_OBJECT:
        return ABI_CLOSED;
    default:
        return ABI_BAD_HANDLE;
    }
}

static int64_t ipc_error_to_abi(int rc)
{
    switch (rc) {
    case IPC_OK:
        return ABI_OK;
    case IPC_EDEAD:
        return ABI_CLOSED;
    case IPC_EBUSY:
        return ABI_BUSY;
    case IPC_EINVAL:
        return ABI_BAD_ARG;
    case IPC_ENOMEM:
        return ABI_NO_MEMORY;
    default:
        return ABI_BUSY;
    }
}

static int64_t backend_acquire(void *ctx, uint32_t caller, uint32_t handle,
                               enum syscall_kind kind, unsigned rights,
                               uint32_t *ref)
{
    struct closure_backend *backend_ctx = ctx;
    struct cap_object *obj = NULL;
    cap_rights_t need;
    int rc;

    /*
     * reply 是 ipc 模块签发的单次 bearer token；capability 的 reply cap
     * 版本在 kernel/capability/demo.c 中单独演示。这里让 REPLY 走同一
     * 分发器，但仍由 ep_reply() 校验 token 存活/单次语义。
     */
    if (kind == SYSCALL_REPLY) {
        *ref = 0;
        return ABI_OK;
    }
    if (caller >= 2)
        return ABI_BAD_HANDLE;

    switch (kind) {
    case SYSCALL_ENDPOINT:
        need = 0;
        if (rights & SYSCALL_SEND)
            need |= CAP_RIGHT_WRITE;
        if (rights & SYSCALL_RECV)
            need |= CAP_RIGHT_READ;
        break;
    default:
        return ABI_BAD_ARG;
    }
    if (need == 0)
        return ABI_BAD_ARG;

    rc = cap_acquire((cap_sid_t)caller, (cap_handle_t)handle, need, &obj);
    if (rc != CAP_OK)
        return cap_error_to_abi(rc);
    if (obj->type->type != CAP_TYPE_ENDPOINT) {
        cap_release(obj);
        return ABI_DENIED;
    }

    for (uint32_t i = 0; i < BACKEND_REFS; i++) {
        if (!backend_ctx->refs[i].used) {
            backend_ctx->refs[i].obj = obj;
            backend_ctx->refs[i].used = true;
            *ref = i + 1;
            return ABI_OK;
        }
    }
    cap_release(obj);
    return ABI_NO_MEMORY;
}

static void backend_release(void *ctx, uint32_t ref)
{
    struct closure_backend *backend_ctx = ctx;

    if (ref == 0 || ref > BACKEND_REFS)
        return;
    struct closure_backend_ref *slot = &backend_ctx->refs[ref - 1];

    if (!slot->used)
        return;
    cap_release(slot->obj);
    slot->obj = NULL;
    slot->used = false;
}

static ipc_message ipc_from_abi(const struct abi_regs *args)
{
    ipc_message msg = {0};

    msg.tag = (uint32_t)args->a[1];
    for (unsigned i = 0; i < ABI_WORDS; i++)
        msg.words[i] = args->a[i + 2];
    return msg;
}

static struct abi_message abi_from_ipc(const ipc_message *msg, uint32_t reply)
{
    struct abi_message out = {
        .tag = msg->tag,
        .reply = reply,
    };

    for (unsigned i = 0; i < ABI_WORDS; i++)
        out.words[i] = msg->words[i];
    return out;
}

static struct cap_object *backend_object(struct closure_backend *ctx,
                                         uint32_t ref)
{
    if (ref == 0 || ref > BACKEND_REFS)
        return NULL;
    struct closure_backend_ref *slot = &ctx->refs[ref - 1];

    return slot->used ? slot->obj : NULL;
}

static enum syscall_phase backend_invoke(void *ctx,
                                         const struct syscall_request *request,
                                         int64_t *status,
                                         struct abi_message *result)
{
    struct closure_backend *backend_ctx = ctx;
    struct cap_object *first = backend_object(backend_ctx, request->refs[0]);
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

        if (first == NULL) {
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

        if (first == NULL) {
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

        if (first == NULL) {
            *status = ABI_BAD_HANDLE;
            break;
        }
        rc = ep_recv((struct endpoint *)first->payload, &msg);
        *status = ipc_error_to_abi(rc);
        if (rc == IPC_OK)
            *result = abi_from_ipc(&msg, msg.reply_id);
        break;
    }

    case ABI_IPC_REPLY: {
        ipc_message msg = ipc_from_abi(&request->args);
        uint32_t token = (uint32_t)request->args.a[0];

        *status = ipc_error_to_abi(ep_reply(token, &msg));
        break;
    }

    default:
        *status = ABI_BAD_NR;
        break;
    }
    return SYSCALL_COMPLETE;
}

static const struct syscall_backend closure_backend = {
    .ctx = &backend,
    .acquire = backend_acquire,
    .release = backend_release,
    .invoke = backend_invoke,
};

/* ---- 用户线程：第一次被调度时从 S 态切到 U 态 -------------------- */

static _Noreturn void user_thread_entry(void *argument)
{
    struct uctx *u = argument;
    struct trap_frame *tf = u->frame;

    /*
     * thread_bootstrap 在调用 entry 前会打开 SIE。第一次 sret 之前不允许
     * S 态 timer 中断把 trap frame 压到预置 frame 上；回到 U 态后 S-mode
     * 中断仍可到达（U 态不受 sstatus.SIE 限制）。
     */
    irq_disable();

    /*
     * 这里必须是“最后两条指令”：直接把 sp 指到预先构造好的 trap frame，
     * 然后跳进唯一出口 trap_return。之前不再使用当前栈。
     */
    __asm__ volatile(
        "mv a0, %0\n\t"
        "mv sp, %1\n\t"
        "j trap_return\n\t"
        :
        : "r"(tf), "r"(tf)
        : "memory");
    __builtin_unreachable();
}

static void prepare_user_frame(struct uctx *u)
{
    uintptr_t top = (uintptr_t)u->kstack + sizeof(u->kstack);
    struct trap_frame *tf = (void *)(top - FRAME_TOP_RSV);

    tf->a0 = u->role;
    tf->a1 = u->handle;
    tf->a2 = USER_DATA_VA;
    tf->sp = (uintptr_t)(USER_STACK_VA + PAGE_SIZE - 16);
    tf->tp = (uintptr_t)hart_local_get();
    tf->sepc = USER_CODE_VA;
    tf->sstatus = csr_read(CSR_SSTATUS) &
                  ~(SSTATUS_SPP | SSTATUS_SIE | SSTATUS_SPIE);
    u->frame = tf;
}

/* ---- 地址空间 ---------------------------------------------------- */

static uintptr_t align_up_page(uintptr_t value)
{
    return (value + PAGE_SIZE - 1) & ~(uintptr_t)(PAGE_SIZE - 1);
}

static int build_kernel_as(void)
{
    uintptr_t kstart = (uintptr_t)__kernel_start;
    uintptr_t kend = (uintptr_t)__kernel_end;
    uintptr_t pool = align_up_page(kend);
    uintptr_t image_end = align_up_page(kend);
    size_t image_size = image_end - kstart;

    if (frame_init((paddr_t)pool, CLOSURE_POOL, (void *)pool) != MEM_OK)
        return -1;

    kernel_as = as_create();
    if (kernel_as == NULL)
        return -1;

    /*
     * 内核根页表只负责两件事：
     *   1. 用户在 U 态陷入时，S 态 trap entry / handler / 内核栈可用；
     *   2. frame_ptr() 返回的物理地址在 satp!=0 时仍然可访问。
     *
     * 内核镜像不是 frame_init 的对象；pool 则是 frame_init 的帧池，
     * 但这里的恒等映射本身非拥有（PTE 标记 RSW_NONOWNED，不增加
     * refcount）。所以走 as_map_kernel_range()，销毁 AS 时不会误
     * frame_unref 内核镜像或帧池 owner 仍持有的页。
     */
    if (as_map_kernel_range(kernel_as, (paddr_t)kstart, image_size) != MEM_OK)
        return -1;
    if (as_map_kernel_range(kernel_as, (paddr_t)pool, CLOSURE_POOL) != MEM_OK)
        return -1;

    return 0;
}

static struct as *make_user_as_for(uint64_t root)
{
    struct as *as = as_create();

    if (as == NULL)
        return NULL;
    if (as_share_kernel(as, (pfn_t)root, 2) != MEM_OK) {
        as_unref(as);
        return NULL;
    }
    return as;
}

static void setup_user(struct uctx *u, uint32_t sid, uint32_t role,
                       cap_handle_t handle, const char *name, bool map_data)
{
    pfn_t stack;
    pfn_t data;
    struct as *as;

    as = make_user_as_for(kernel_as->root_pfn);
    CHECK(as != NULL);
    CHECK(as_map(as, USER_CODE_VA, code_pfn, MEM_U | MEM_R | MEM_X) == MEM_OK);

    stack = frame_alloc();
    CHECK(stack != PFN_NONE);
    CHECK(as_map(as, USER_STACK_VA, stack,
                 MEM_U | MEM_R | MEM_W) == MEM_OK);
    frame_unref(stack);

    if (map_data) {
        data = frame_alloc();
        CHECK(data != PFN_NONE);
        CHECK(as_map(as, USER_DATA_VA, data,
                     MEM_U | MEM_R | MEM_W) == MEM_OK);
        frame_unref(data);
    }

    u->as = as;
    u->sid = sid;
    u->role = role;
    u->handle = handle;
    u->name = name;
    u->root_pfn = as->root_pfn;
    prepare_user_frame(u);

    u->thread = thread_create(as, user_thread_entry, u, u->kstack,
                              sizeof(u->kstack), name);
    CHECK(u->thread != NULL);
    /* thread_create 已持有一份引用；归还创建者那一份。 */
    as_unref(as);
}

/* ---- trap 分发 ---------------------------------------------------- */

struct trap_frame *closure_trap(struct trap_frame *frame)
{
    if (!closure_active)
        return NULL;

    struct uctx *u = user_for(sched_current());

    if ((frame->scause & SCAUSE_IRQ) &&
        (frame->scause & ~SCAUSE_IRQ) == IRQ_S_TIMER) {
        /* 只观察，实际派发/抢占交给 shared/demo.c 的统一 glue。 */
        log_satp_change(u, "timer");
        return NULL;
    }

    if (frame->scause == EXC_ECALL_U && !(frame->sstatus & SSTATUS_SPP)) {
        enum syscall_phase phase;
        uint64_t req_tag = abi_meta_tag(frame->a1);

        if (u == NULL)
            return NULL;

        log_satp_change(u, "ecall");

        /*
         * 在 dispatch 前打印请求方向：CALL/RECV 可能在这里阻塞，日志
         * 顺序（enter -> woke -> reply）就是阻塞/唤醒的证据。
         */
        if ((enum abi_number)frame->a7 == ABI_IPC_CALL)
            log_hex_field("[ipc] ", u->name, "call enter tag=", req_tag);
        else if ((enum abi_number)frame->a7 == ABI_IPC_RECV)
            log_hex_field("[ipc] ", u->name, "recv enter handle=", frame->a0);

        if (u->call.phase == SYSCALL_COMPLETE)
            u->call = (struct syscall_call){0};

        phase = syscall_dispatch(&u->call, frame, u->sid, &closure_backend);
        if (phase == SYSCALL_IGNORED)
            return NULL;
        if (phase == SYSCALL_EXITED) {
            log_dec_field("[exit] ", u->name, "thread_exit", 0);
            u->exited = true;
            completed++;
            thread_exit();
        }
        if (phase != SYSCALL_COMPLETE)
            return frame;

        int64_t status = frame_status(frame);
        uint32_t result_tag = abi_meta_tag(frame->a1);
        uint64_t result_word0 = frame->a2;

        if ((enum abi_number)frame->a7 == (enum abi_number)99) {
            char buf[192];
            char *p = buf;

            p = put_str(p, "[err] ");
            p = put_str(p, u->name);
            p = put_str(p, " illegal syscall nr=99 -> status=");
            p = put_hex(p, (uint64_t)status);
            p = put_str(p, " sepc=");
            p = put_hex(p, frame->sepc);
            if (u->sid == SID_SERVER && status == (int64_t)ABI_BAD_NR) {
                illegal_calls++;
                illegal_ok = true;
                p = put_str(p, " expected");
            } else {
                unexpected_errors++;
                p = put_str(p, " UNEXPECTED");
            }
            log_line(buf, p);
            return frame;
        }

        switch ((enum abi_number)frame->a7) {
        case ABI_IPC_CALL:
            /*
             * 内核验收回复内容，而不是只看 status。U-A 的 U-mode 程序
             * 也会检查；这里失败时 call_ok 保持 false，并让最终 CHECK
             * 和 unexpected_errors 同时暴露。
             */
            if (status == ABI_OK && u->sid == SID_CLIENT &&
                result_tag == TAG_REPLY && result_word0 == MAGIC_REPLY) {
                u->call_ok = true;
                log_hex_field("[ipc] ", u->name, "call reply tag=",
                              result_tag);
                log_hex_field("[ipc] ", u->name, "call reply word0=",
                              result_word0);
            } else {
                u->call_ok = false;
                unexpected_errors++;
                log_hex_field("[ipc] ", u->name, "call rejected status=",
                              (uint64_t)status);
                log_hex_field("[ipc] ", u->name, "call rejected tag=",
                              result_tag);
                log_hex_field("[ipc] ", u->name, "call rejected word0=",
                              result_word0);
            }
            break;
        case ABI_IPC_RECV:
            if (status == ABI_OK && u->sid == SID_SERVER &&
                result_tag == TAG_CALL && result_word0 == MAGIC_WRITE) {
                u->recv_ok = true;
                log_hex_field("[ipc] ", u->name, "recv woke tag=",
                              result_tag);
                log_hex_field("[ipc] ", u->name, "recv word0=", result_word0);
            } else {
                u->recv_ok = false;
                unexpected_errors++;
                log_hex_field("[ipc] ", u->name, "recv rejected status=",
                              (uint64_t)status);
                log_hex_field("[ipc] ", u->name, "recv rejected tag=",
                              result_tag);
                log_hex_field("[ipc] ", u->name, "recv rejected word0=",
                              result_word0);
            }
            break;
        case ABI_IPC_REPLY:
            if (status == ABI_OK && u->sid == SID_SERVER &&
                req_tag == TAG_REPLY) {
                u->reply_ok = true;
                log_hex_field("[ipc] ", u->name, "reply sent tag=", req_tag);
            } else {
                u->reply_ok = false;
                unexpected_errors++;
                log_hex_field("[ipc] ", u->name, "reply rejected status=",
                              (uint64_t)status);
                log_hex_field("[ipc] ", u->name, "reply rejected tag=",
                              req_tag);
            }
            break;
        default:
            break;
        }
        return frame;
    }

    if ((frame->scause == EXC_LOAD_PAGEFAULT ||
         frame->scause == EXC_STORE_PAGEFAULT ||
         frame->scause == EXC_INST_PAGEFAULT) &&
        !(frame->sstatus & SSTATUS_SPP)) {
        char buf[224];
        char *p = buf;
        uint64_t satp = csr_read(CSR_SATP);
        uint64_t expected_satp = (8ULL << 60) | users[1].root_pfn;
        bool accepted =
            u == &users[1] &&
            frame->scause == EXC_LOAD_PAGEFAULT &&
            frame->stval == USER_DATA_VA &&
            satp == expected_satp;

        /*
         * 只有“U-B 在 B 自己的 satp 下 load 同一个 probe VA”才算
         * 隔离证据。其它 U 态缺页（A 写失败、B 指令缺页、地址/AS 不对）
         * 一律计入 unexpected_errors，不能把 fault_seen 变成万能通过。
         */
        if (accepted) {
            fault_seen = true;
            fault_ok = true;
            fault_stval = frame->stval;
            fault_satp = satp;
        } else {
            unexpected_errors++;
        }

        p = put_str(p, "[iso] ");
        p = put_str(p, u != NULL ? u->name : "user");
        p = put_str(p, " page fault scause=");
        p = put_hex(p, frame->scause);
        p = put_str(p, " sepc=");
        p = put_hex(p, frame->sepc);
        p = put_str(p, " stval=");
        p = put_hex(p, frame->stval);
        p = put_str(p, " satp=");
        p = put_hex(p, satp);
        p = put_str(p, accepted ? " accepted" : " REJECTED");
        log_line(buf, p);

        if (u != NULL) {
            u->exited = true;
            completed++;
        }
        thread_exit();
    }

    return NULL;
}

/* ---- 主流程 ------------------------------------------------------ */

void closure_run(void)
{
    uint64_t sie_saved = csr_read(CSR_SIE);
    uint64_t sstatus_saved = csr_read(CSR_SSTATUS);
    cap_oid_t ep_oid;
    cap_handle_t client_handle = 0;
    cap_handle_t server_handle = 0;
    clock_ns_t limit;

    closure_active = false;
    last_satp = 0;
    completed = 0;
    illegal_calls = 0;
    unexpected_errors = 0;
    illegal_ok = false;
    fault_seen = false;
    fault_ok = false;
    precheck_a_mapped = false;
    precheck_b_unmapped = false;
    fault_stval = 0;
    fault_satp = 0;

    object_system_init();
    sched_init();
    ipc_init();
    cap_system_init();
    for (unsigned i = 0; i < 2; i++)
        users[i] = (struct uctx){0};
    backend = (struct closure_backend){0};

    CHECK(build_kernel_as() == 0);

    /* 用户程序模板 -> 一个只读代码帧，两个 AS 共享同一 VA/同一物理页。 */
    code_pfn = frame_alloc();
    CHECK(code_pfn != PFN_NONE);
    memset(frame_ptr(code_pfn), 0, PAGE_SIZE);
    size_t code_len = (size_t)(user_prog_end - user_prog_start);
    CHECK(code_len > 0 && code_len <= PAGE_SIZE);
    memcpy(frame_ptr(code_pfn), user_prog_start, code_len);
    /* 代码是刚写进去的字节，用户 AS 首次取指前同步 I-cache。 */
    __asm__ volatile("fence.i" ::: "memory");

    endpoint = ep_create();
    CHECK(endpoint != NULL);
    CHECK(cap_object_create(&endpoint_type, endpoint, &ep_oid) == CAP_OK);
    CHECK(cap_root_install(SID_CLIENT, 1, ep_oid, CAP_RIGHT_WRITE, 0,
                           &client_handle) == CAP_OK);
    CHECK(cap_root_install(SID_SERVER, 1, ep_oid,
                           CAP_RIGHT_READ | CAP_RIGHT_WRITE, 0,
                           &server_handle) == CAP_OK);

    /* A：客户端，probe VA 映射为可写用户页。 */
    setup_user(&users[0], SID_CLIENT, 1, client_handle, "U-A", true);
    /* B：服务端，故意不映射 probe VA。 */
    setup_user(&users[1], SID_SERVER, 2, server_handle, "U-B", false);

    /* 调用者的 code_pfn 引用归还；两个 AS 的映射各持有一次。 */
    frame_unref(code_pfn);

    /*
     * 运行前用页表本身证明隔离前提：A 的 probe VA 已映射且 U/R/W，
     * B 的同一 VA 在 translate 阶段就返回 MEM_EFAULT。这样后续 QEMU
     * 里的 load page fault 不是“碰巧没映射”。
     */
    {
        int rights = 0;
        pfn_t pfn = PFN_NONE;

        precheck_a_mapped =
            as_translate(users[0].as, USER_DATA_VA, &rights, &pfn) == MEM_OK &&
            (rights & (MEM_U | MEM_R | MEM_W)) ==
                (MEM_U | MEM_R | MEM_W) &&
            pfn != PFN_NONE;
        precheck_b_unmapped =
            as_translate(users[1].as, USER_DATA_VA, &rights, &pfn) ==
            MEM_EFAULT;
        CHECK(precheck_a_mapped);
        CHECK(precheck_b_unmapped);
        log_dec_field("[iso] ", "precheck", "A_mapped", precheck_a_mapped);
        log_dec_field("[iso] ", "precheck", "B_unmapped", precheck_b_unmapped);
    }

    CHECK(users[0].root_pfn != users[1].root_pfn);

    sched_bootstrap(&runner);
    closure_active = true;

    clock_init();
    CHECK(sched_start_tick(CLOSURE_TICK_NS));
    limit = clock_deadline_after(clock_now(), 3ULL * 1000 * 1000 * 1000);
    irq_enable();

    while (completed < 2 && clock_now() < limit)
        thread_yield();

    irq_disable();
    sched_stop_tick();
    /*
     * 双保险：sched_stop_tick() 只撤销它自己武装的 one-shot；这里再把
     * SBI 比较器撤防、STIE 清零，避免 legacy demo 阶段收到迟到的 tick。
     */
    sbi_set_timer(UINT64_MAX);
    csr_clear(CSR_SIE, SIE_STIE);
    csr_write(CSR_SIE, sie_saved);
    if (sstatus_saved & SSTATUS_SIE)
        irq_enable();
    else
        irq_disable();
    closure_active = false;

    CHECK(completed == 2);
    CHECK(users[0].exited && users[1].exited);
    CHECK(users[0].call_ok);
    CHECK(users[1].recv_ok && users[1].reply_ok);
    CHECK(illegal_calls == 1 && illegal_ok);
    CHECK(fault_seen && fault_ok);
    CHECK(fault_stval == USER_DATA_VA);
    CHECK(fault_satp == ((8ULL << 60) | users[1].root_pfn));
    CHECK(precheck_a_mapped && precheck_b_unmapped);
    CHECK(unexpected_errors == 0);
    CHECK(sched_tick_count() > 0);
    CHECK(sched_switch_count() > 0);
    CHECK(sched_quantum_expiration_count() > 0);

    log_dec_field("[unified] ", NULL, "completed", completed);
    log_dec_field("[unified] ", NULL, "illegal_syscalls", illegal_calls);
    log_dec_field("[unified] ", NULL, "unexpected_errors", unexpected_errors);
    log_dec_field("[unified] ", NULL, "sched_ticks", sched_tick_count());
    log_dec_field("[unified] ", NULL, "sched_switches", sched_switch_count());
    log_dec_field("[unified] ", NULL, "quantum_expired",
                  sched_quantum_expiration_count());
    log_hex_field("[unified] ", NULL, "U-A root_pfn=", users[0].root_pfn);
    log_hex_field("[unified] ", NULL, "U-B root_pfn=", users[1].root_pfn);
    log_hex_field("[unified] ", NULL, "fault stval=", fault_stval);
    log_hex_field("[unified] ", NULL, "fault satp=", fault_satp);
    demo_log("[unified] timer preemption + U ecall + two AS satp switch + "
             "IPC call/reply + isolation fault");

    /* 把调度器还给同一模块里的后续 legacy demo。 */
    sched_init();
    as_activate(0);
}