#include "syscall.h"

#include "csr.h"
#include "sched.h"

/*
 * 分发表。每个调用号一行：
 *   参数形状 —— 前 argc 个寄存器有意义，其余必须为零
 *   校验规则 —— 位宽、页对齐、权限位等与具体调用号相关的约束
 *   handler  —— 只通过 syscall_backend 调用内核原语
 *
 * kind/rights 描述首个句柄的对象类别和操作权限，由通用 handler 在
 * 进入后端前统一解析；这样未知对象、越权句柄和非法参数都不会触达
 * sched/memory/ipc。
 */
typedef enum syscall_phase (*syscall_handler)(struct syscall_call *,
                                              int64_t *status,
                                              struct abi_message *result);

struct syscall_entry {
    unsigned argc;
    bool tag;    /* 输入 a1 是 32 位 tag；接收结果的 meta 才含 reply */
    bool result; /* 完成后把消息写回 a1..a5 */
    enum syscall_kind kind;
    unsigned rights;
    bool (*valid)(const struct syscall_request *);
    syscall_handler handle;
};

static enum syscall_phase backend_invoke(struct syscall_call *call,
                                         int64_t *status,
                                         struct abi_message *result);
static int64_t acquire(struct syscall_call *call, uint32_t handle,
                       enum syscall_kind kind, unsigned rights);
static const struct syscall_entry *entry_for(enum abi_number nr);

static bool valid_map(const struct syscall_request *request)
{
    const struct abi_regs args = request->args;

    /* Sv39 用户空间只使用低半区；映射单位是 4 KiB 页。 */
    return abi_page_aligned(args.a[1]) && args.a[1] < (UINT64_C(1) << 38) &&
           abi_rights_valid(args.a[3]);
}

static bool valid_unmap(const struct syscall_request *request)
{
    uint64_t va = request->args.a[1];

    return abi_page_aligned(va) && va < (UINT64_C(1) << 38);
}

/* YIELD/THREAD_EXIT：没有句柄，直接进后端。 */
static enum syscall_phase handle_none(struct syscall_call *call, int64_t *status,
                                      struct abi_message *result)
{
    return backend_invoke(call, status, result);
}

/* SEND/RECV/CALL/REPLY/UNMAP：解析首个句柄，再进后端。 */
static enum syscall_phase handle_handle(struct syscall_call *call, int64_t *status,
                                        struct abi_message *result)
{
    const struct syscall_entry *entry = entry_for(call->request.nr);

    *status = acquire(call, (uint32_t)call->request.args.a[0],
                      entry->kind, entry->rights);
    if (*status != ABI_OK)
        return SYSCALL_COMPLETE;
    return backend_invoke(call, status, result);
}

/* AS_MAP：地址空间与物理帧各解析一次，两者都通过才进后端。 */
static enum syscall_phase handle_map(struct syscall_call *call, int64_t *status,
                                     struct abi_message *result)
{
    const struct syscall_entry *entry = entry_for(call->request.nr);

    *status = acquire(call, (uint32_t)call->request.args.a[0],
                      SYSCALL_AS, entry->rights);
    if (*status == ABI_OK)
        *status = acquire(call, (uint32_t)call->request.args.a[2],
                          SYSCALL_FRAME, (unsigned)call->request.args.a[3]);
    if (*status != ABI_OK)
        return SYSCALL_COMPLETE;
    return backend_invoke(call, status, result);
}

static const struct syscall_entry table[MICRO_ABI_MAX_NR + 1] = {
    [ABI_YIELD]       = {0, false, false, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_THREAD_EXIT] = {0, false, false, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_IPC_SEND]    = {6, true, false, SYSCALL_ENDPOINT, SYSCALL_SEND, 0, handle_handle},
    [ABI_IPC_RECV]    = {1, false, true, SYSCALL_ENDPOINT, SYSCALL_RECV, 0, handle_handle},
    [ABI_IPC_CALL]    = {6, true, true, SYSCALL_ENDPOINT, SYSCALL_SEND, 0, handle_handle},
    [ABI_IPC_REPLY]   = {6, true, false, SYSCALL_REPLY, SYSCALL_ANSWER, 0, handle_handle},
    [ABI_AS_MAP]      = {4, false, false, SYSCALL_AS, SYSCALL_MAP, valid_map, handle_map},
    [ABI_AS_UNMAP]    = {2, false, false, SYSCALL_AS, SYSCALL_MAP, valid_unmap, handle_handle},
    [ABI_CAP_MINT_TO] = {4, false, true, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_PM_SPAWN]    = {3, false, true, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_CAP_DELETE]  = {1, false, false, SYSCALL_NONE, 0, 0, handle_none},

    [ABI_CONSOLE_READ]    = {2, false, true, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_CONSOLE_WRITE]   = {2, false, false, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_POWEROFF]        = {0, false, false, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_PID_SELF]        = {0, false, true, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_VM_MAP]          = {3, false, true, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_PAGER_DONE]      = {1, false, false, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_AS_SELF]         = {0, false, true, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_PAGER_FAIL]      = {2, false, false, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_PAGER_READY]     = {0, false, false, SYSCALL_NONE, 0, 0, handle_none},
    /* 身份查询：入参是 reply_id，返回发起该 call 的真实 TCB cap_sid。 */
    [ABI_REPLY_CALLER_SID] = {1, false, true, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_VM_GRANT_AS]      = {2, false, true, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_PM_DISCARD]       = {2, false, true, SYSCALL_NONE, 0, 0, handle_none},
    /* 策略层在 U-mode：原始块读只有块号/用户地址/块数。 */
    [ABI_BLOCK_READ]       = {3, false, true, SYSCALL_NONE, 0, 0, handle_none},
    [ABI_BLOCK_INFO]       = {0, false, true, SYSCALL_NONE, 0, 0, handle_none},
};

static const struct syscall_entry *entry_for(enum abi_number nr)
{
    if ((unsigned)nr < ABI_YIELD || (unsigned)nr > MICRO_ABI_MAX_NR)
        return 0;
    /*
     * 调用号有空洞（12、18）。designated initializer 留空的槽 handler
     * 为 NULL；这里必须当作未知调用号，否则 dispatch 会调用空指针。
     */
    if (table[nr].handle == 0)
        return 0;
    return &table[nr];
}

/* 参数形状：尾寄存器为零、句柄/标签为 32 位、调用号附加规则。 */
static bool shape_ok(const struct syscall_request *request,
                     const struct syscall_entry *entry)
{
    const struct abi_regs args = request->args;

    if (!abi_tail_zero(args, entry->argc))
        return false;
    if (entry->kind != SYSCALL_NONE && !abi_arg_u32(args, 0, NULL))
        return false;
    if (entry->tag && !abi_arg_u32(args, 1, NULL))
        return false;
    if (request->nr == ABI_AS_MAP && !abi_arg_u32(args, 2, NULL))
        return false;
    return entry->valid == 0 || entry->valid(request);
}

static int64_t acquire(struct syscall_call *call, uint32_t handle,
                       enum syscall_kind kind, unsigned rights)
{
    const struct syscall_backend *port = call->backend;
    uint32_t *ref;
    int64_t status;

    if (call->held >= sizeof(call->request.refs) / sizeof(call->request.refs[0]))
        return ABI_NO_MEMORY;
    ref = &call->request.refs[call->held];

    status = port->acquire(port->ctx, call->request.caller, handle, kind, rights, ref);
    if (status == ABI_OK)
        call->held++;
    return status;
}

static void write_frame(struct trap_frame *frame, struct abi_regs regs)
{
    frame->a0 = regs.a[0];
    frame->a1 = regs.a[1];
    frame->a2 = regs.a[2];
    frame->a3 = regs.a[3];
    frame->a4 = regs.a[4];
    frame->a5 = regs.a[5];
}

/*
 * 完成一次调用：写回结果、释放在途引用、把阶段落到 COMPLETE/EXITED。
 * IPC_CALL 的 reply 字段由内核生成，调用者传入的值一律清零。
 */
static enum syscall_phase finish(struct syscall_call *call, enum syscall_phase phase,
                                 int64_t status, const struct abi_message *message)
{
    const struct syscall_backend *port = call->backend;

    if (phase == SYSCALL_COMPLETE) {
        if (message != 0 && call->request.nr == ABI_IPC_CALL) {
            struct abi_message cleared = *message;
            cleared.reply = 0;
            write_frame(call->frame, abi_result(status, &cleared));
        } else {
            write_frame(call->frame, abi_result(status, message));
        }
    }
    while (call->held)
        port->release(port->ctx, call->request.refs[--call->held]);
    call->waiting_thread = 0;
    call->async_reply_id = 0;
    call->async_ipc_done = false;
    call->frame = 0;
    call->phase = phase;
    return phase;
}

static bool syscall_drain_async_done(struct syscall_call *call,
                                     enum syscall_phase *out_phase)
{
    const struct syscall_entry *entry;
    int64_t st;
    struct abi_message msg;

    if (call == 0 || !call->async_ipc_done) {
        return false;
    }
    entry = entry_for(call->request.nr);
    st = call->async_ipc_status;
    msg = call->async_ipc_result;
    call->async_ipc_done = false;
    *out_phase = finish(call, SYSCALL_COMPLETE, st,
                        entry != 0 && entry->result ? &msg : 0);
    return true;
}

void syscall_wait_park(struct syscall_call *call)
{
    struct thread *self = sched_current();
    uint64_t flags;
    enum syscall_phase done;

    if (call == 0 || self == 0) {
        return;
    }
    flags = sched_lock();
    while (call->phase == SYSCALL_WAITING) {
        if (syscall_drain_async_done(call, &done)) {
            break;
        }
        /*
         * syscall_resume() 可能在 drain 之后、block 之前把 phase 落成 COMPLETE；
         * 不再 block 可避免「已写完 trap 帧却仍挂在 syscall_wq」的死锁。
         */
        if (call->phase != SYSCALL_WAITING) {
            break;
        }
        /*
         * 标记 async_parked：这期间 trap 帧被在途 reply 引用，任何
         * discard 都会写坏内存。sched_thread_terminate() 据此先撤销 reply。
         */
        self->async_parked = true;
        thread_block_locked(&self->syscall_wq);
        sched_unlock(flags);
        sched();
        flags = sched_lock();
        self->async_parked = false;
        if (syscall_drain_async_done(call, &done)) {
            break;
        }
    }
    self->async_parked = false;
    sched_unlock(flags);

    /*
     * 若本线程在等待期间被请求终止（PM discard 对已 RUNNING 的子进程），
     * 这里不再返回用户态，直接退出；trap 帧已续完或已放弃，不会被别人
     * 再引用。
     */
    if (sched_thread_kill_pending(self)) {
        thread_exit();
    }
}

static enum syscall_phase backend_invoke(struct syscall_call *call,
                                         int64_t *status,
                                         struct abi_message *result)
{
    *status = ABI_OK;
    return call->backend->invoke(call->backend->ctx, &call->request, status, result);
}

enum syscall_phase syscall_dispatch(struct syscall_call *call, struct trap_frame *frame,
                                    uint32_t caller, const struct syscall_backend *backend)
{
    const struct syscall_entry *entry;
    struct syscall_request *request;
    struct abi_message result = {0};
    enum syscall_phase phase;
    int64_t status = ABI_OK;

    if (call == 0 || frame == 0 || backend == 0)
        return SYSCALL_IGNORED;
    if (call->phase == SYSCALL_WAITING)
        return SYSCALL_WAITING;
    if (call->phase == SYSCALL_ENTERED || call->phase == SYSCALL_DISPATCHED)
        return call->phase;

    /* 只有同步 U-mode ecall 进入分发；内核自己的 ecall 不动调用帧。 */
    if (frame->scause != EXC_ECALL_U || (frame->sstatus & SSTATUS_SPP))
        return SYSCALL_IGNORED;

    *call = (struct syscall_call){
        .phase = SYSCALL_ENTERED,
        .frame = frame,
        .backend = backend,
        .async_reply_id = 0,
        .async_ipc_done = false,
    };
    /* 受理：从下一条指令继续。参数错误也走同一条路径。 */
    frame->sepc += 4;

    entry = entry_for((enum abi_number)frame->a7);
    if (entry == 0)
        return finish(call, SYSCALL_COMPLETE, ABI_BAD_NR, 0);

    request = &call->request;
    request->caller = caller;
    request->nr = (enum abi_number)frame->a7;
    request->args = (struct abi_regs){{frame->a0, frame->a1, frame->a2,
                                      frame->a3, frame->a4, frame->a5}};

    if (!shape_ok(request, entry))
        return finish(call, SYSCALL_COMPLETE, ABI_BAD_ARG, 0);

    call->phase = SYSCALL_DISPATCHED;
    phase = entry->handle(call, &status, &result);

    if (phase == SYSCALL_WAITING) {
        call->phase = SYSCALL_WAITING;
        call->waiting_thread = sched_current();
        {
            enum syscall_phase done;

            if (syscall_drain_async_done(call, &done)) {
                return done;
            }
        }
        return phase;
    }
    if (phase == SYSCALL_EXITED)
        return finish(call, SYSCALL_EXITED, status, 0);
    return finish(call, SYSCALL_COMPLETE, status, entry->result ? &result : 0);
}

bool syscall_resume(struct syscall_call *call, int64_t status, struct abi_message message)
{
    const struct syscall_entry *entry;
    struct thread *wake;
    uint64_t flags;
    bool ok;

    if (call == 0 || call->frame == 0) {
        return false;
    }

    flags = sched_lock();
    if (call->phase != SYSCALL_WAITING) {
        sched_unlock(flags);
        return false;
    }

    entry = entry_for(call->request.nr);
    if (entry == 0) {
        sched_unlock(flags);
        return false;
    }
    wake = call->waiting_thread;
    finish(call, SYSCALL_COMPLETE, status, entry->result ? &message : 0);
    if (wake != 0) {
        thread_wake_locked(wake);
    }
    ok = true;
    sched_unlock(flags);
    return ok;
}
