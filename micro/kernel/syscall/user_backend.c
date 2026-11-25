#include "user_backend.h"

#include "cap_as.h"
#include "cap_endpoint.h"
#include "capability.h"
#include "console/console.h"
#include "demo.h"
#include "ipc/ipc.h"
#include "kernel_spawn.h"
#include "sbi.h"
#include "sched.h"
#include "service_boot.h"
#include "storage/storage_bringup.h"
#include "memory.h"
#include "object.h"
#include "pager/pager.h"

#define USER_BACKEND_REFS 8

struct user_backend_ref {
    struct cap_object *obj;
    bool used;
};

struct user_backend_ctx {
    struct user_backend_ref refs[USER_BACKEND_REFS];
};

static struct user_backend_ctx user_backend;


static int abi_to_mem_rights(uint64_t abi)
{
    int rights = MEM_U;

    if (abi & ABI_READ) {
        rights |= MEM_R;
    }
    if (abi & ABI_WRITE) {
        rights |= MEM_W;
    }
    if (abi & ABI_EXEC) {
        rights |= MEM_X;
    }
    return rights;
}

static int64_t mem_error_to_abi(int rc)
{
    switch (rc) {
    case MEM_OK:
        return ABI_OK;
    case MEM_ENOMEM:
        return ABI_NO_MEMORY;
    case MEM_EPERM:
        return ABI_DENIED;
    case MEM_EEXIST:
        return ABI_ALREADY_MAPPED;
    case MEM_EFAULT:
    case MEM_EINVAL:
    default:
        return ABI_BAD_ARG;
    }
}

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

static void log_cap_denied(enum syscall_kind kind, unsigned rights)
{
    const char *line;

    if (kind != SYSCALL_ENDPOINT || !(rights & SYSCALL_SEND)) {
        return;
    }
    line = "[cap] denied: missing right SEND";
    while (*line != '\0') {
        console_putc(*line++);
    }
    console_putc('\n');
}

static ipc_message ipc_from_abi(const struct abi_regs *args)
{
    ipc_message msg = {0};

    msg.tag = (uint32_t)args->a[1];
    for (unsigned i = 0; i < ABI_WORDS; i++) {
        msg.words[i] = args->a[i + 2];
    }
    return msg;
}

static struct abi_message abi_from_ipc(const ipc_message *msg, uint32_t reply)
{
    struct abi_message out = {
        .tag = msg->tag,
        .reply = reply,
    };

    for (unsigned i = 0; i < ABI_WORDS; i++) {
        out.words[i] = msg->words[i];
    }
    return out;
}

static struct cap_object *backend_object(struct user_backend_ctx *ctx, uint32_t ref)
{
    if (ref == 0 || ref > USER_BACKEND_REFS) {
        return NULL;
    }
    return ctx->refs[ref - 1].used ? ctx->refs[ref - 1].obj : NULL;
}

static int64_t user_backend_acquire(void *ctx, uint32_t caller, uint32_t handle,
                                    enum syscall_kind kind, unsigned rights,
                                    uint32_t *ref)
{
    struct user_backend_ctx *backend_ctx = ctx;
    struct cap_object *obj = NULL;
    cap_rights_t need = 0;
    int rc;

    if (kind == SYSCALL_REPLY) {
        *ref = 0;
        return ABI_OK;
    }
    if (caller >= CAP_MAX_TASKS) {
        return ABI_BAD_HANDLE;
    }

    switch (kind) {
    case SYSCALL_ENDPOINT:
        if (rights & SYSCALL_SEND) {
            need |= CAP_RIGHT_WRITE;
        }
        if (rights & SYSCALL_RECV) {
            need |= CAP_RIGHT_READ;
        }
        break;
    default:
        return ABI_BAD_ARG;
    }
    if (need == 0) {
        return ABI_BAD_ARG;
    }

    rc = cap_acquire((cap_sid_t)caller, (cap_handle_t)handle, need, &obj);
    if (rc != CAP_OK) {
        if (rc == CAP_E_RIGHTS) {
            log_cap_denied(kind, rights);
        }
        return cap_error_to_abi(rc);
    }
    if (obj->type->type != CAP_TYPE_ENDPOINT) {
        cap_release(obj);
        return ABI_DENIED;
    }

    for (uint32_t i = 0; i < USER_BACKEND_REFS; i++) {
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

static void user_backend_release(void *ctx, uint32_t ref)
{
    struct user_backend_ctx *backend_ctx = ctx;

    if (ref == 0 || ref > USER_BACKEND_REFS) {
        return;
    }
    struct user_backend_ref *slot = &backend_ctx->refs[ref - 1];

    if (!slot->used) {
        return;
    }
    cap_release(slot->obj);
    slot->obj = NULL;
    slot->used = false;
}

static enum syscall_phase user_backend_invoke(void *ctx,
                                              const struct syscall_request *request,
                                              int64_t *status,
                                              struct abi_message *result)
{
    struct user_backend_ctx *backend_ctx = ctx;
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

        if (first == NULL) {
            *status = ABI_BAD_HANDLE;
            break;
        }
#if defined(MICRO_POST_BOOT_RESIDENT) && MICRO_POST_BOOT_RESIDENT &&          \
    defined(MICRO_SYSCALL_ASYNC_IPC) && MICRO_SYSCALL_ASYNC_IPC
        {
            struct thread *self = sched_current();
            struct syscall_call *active =
                self != 0 ? (struct syscall_call *)self->ucall_words : 0;
            uint32_t reply_id = 0;
            struct endpoint *ep = (struct endpoint *)first->payload;

            /*
             * 没有 per-thread syscall_call 就没有可以续完的 trap 帧；
             * 此时退回同步 ep_call，避免「请求已投递但没人等回复」
             * 却给调用方返回空的 ABI_OK。
             */
            if (active == 0) {
                ipc_message reply = {0};

                rc = ep_call(ep, &msg, &reply);
                *status = ipc_error_to_abi(rc);
                if (rc == IPC_OK && result != NULL) {
                    *result = abi_from_ipc(&reply, 0);
                }
                break;
            }
            rc = ep_call_open(ep, &reply_id);
            if (rc != IPC_OK) {
                *status = ipc_error_to_abi(rc);
                break;
            }
            active->async_reply_id = reply_id;
            active->waiting_thread = self;
            rc = ep_call_deliver(ep, reply_id, &msg);
            *status = ipc_error_to_abi(rc);
            if (rc != IPC_OK) {
                ep_call_cancel_async(reply_id);
                active->async_reply_id = 0;
                break;
            }
            return SYSCALL_WAITING;
        }
#else
        {
            ipc_message reply = {0};

            rc = ep_call((struct endpoint *)first->payload, &msg, &reply);
            *status = ipc_error_to_abi(rc);
            if (rc == IPC_OK && result != NULL) {
                *result = abi_from_ipc(&reply, 0);
            }
        }
#endif
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
        if (rc == IPC_OK && result != NULL) {
            *result = abi_from_ipc(&msg, msg.reply_id);
        }
        break;
    }

    case ABI_IPC_REPLY: {
        ipc_message msg = ipc_from_abi(&request->args);
        uint32_t token = (uint32_t)request->args.a[0];

        *status = ipc_error_to_abi(
            ep_reply_from((uint8_t)request->caller, token, &msg));
        break;
    }

    case ABI_CAP_MINT_TO: {
        cap_handle_t out = 0;
        int64_t rc;

        rc = kernel_cap_mint_to(
            (uint8_t)request->caller, (cap_handle_t)request->args.a[0],
            (cap_sid_t)request->args.a[1],
            (cap_rights_t)request->args.a[2],
            (cap_badge_t)request->args.a[3], &out);
        *status = rc;
        if (rc == ABI_OK && result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {out, 0, 0, 0},
            };
        }
        break;
    }

    case ABI_PM_SPAWN: {
        uint64_t pid = 0;
        uint64_t tid = 0;
        cap_handle_t pm_as = 0;
        int64_t rc;

        rc = kernel_pm_spawn(
            (uint8_t)request->caller, request->args.a[0],
            (uint32_t)request->args.a[1], (cap_rights_t)request->args.a[2],
            &pid, &tid, &pm_as);
        *status = rc;
        if (rc == ABI_OK && result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {pid, tid, pm_as, 0},
            };
        }
        break;
    }

    case ABI_VM_GRANT_AS: {
        struct object *obj;
        struct thread *target;

        if (request->caller != SERVICE_SID_PM) {
            *status = ABI_DENIED;
            break;
        }
        obj = object_lookup((uint32_t)request->args.a[0], OBJ_THREAD);
        if (obj == 0) {
            *status = ABI_BAD_ARG;
            break;
        }
        target = (struct thread *)obj;
        if (target->as == 0) {
            object_unref(obj);
            *status = ABI_BAD_ARG;
            break;
        }
        if (request->args.a[1] == 0) {
            if (cap_vm_ensure_as_map(target->as) != CAP_OK ||
                !cap_vm_can_map_as(target->as)) {
                object_unref(obj);
                *status = ABI_NO_MEMORY;
                break;
            }
        } else {
            if (cap_pm_mint_vm_map((cap_sid_t)request->caller,
                                   (cap_handle_t)request->args.a[1],
                                   target->as) != CAP_OK) {
                object_unref(obj);
                *status = ABI_DENIED;
                break;
            }
            (void)cap_delete((cap_sid_t)request->caller,
                             (cap_handle_t)request->args.a[1]);
        }
        *status = ABI_OK;
        if (result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {object_get_id(&target->as->obj), 0, 0, 0},
            };
        }
        object_unref(obj);
        break;
    }

    case ABI_PM_DISCARD:
        *status = kernel_pm_discard(
            (uint8_t)request->caller, (uint32_t)request->args.a[0],
            (uint32_t)request->args.a[1]);
        break;

    case ABI_CAP_DELETE:
        *status = cap_error_to_abi(cap_delete((cap_sid_t)request->caller,
                                              (cap_handle_t)request->args.a[0]));
        break;


    case ABI_BLOCK_READ: {
        size_t n = 0;
        int64_t rc;

        rc = kernel_storage_block_read_user(
            (uint8_t)request->caller, request->args.a[0],
            (uintptr_t)request->args.a[1], (size_t)request->args.a[2], &n);
        *status = rc;
        if (rc == ABI_OK && result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {n, 0, 0, 0},
            };
        }
        break;
    }

    case ABI_BLOCK_INFO: {
        if (request->caller != SERVICE_SID_FS) {
            *status = ABI_DENIED;
            break;
        }
        *status = ABI_OK;
        if (result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {kernel_storage_capacity_blocks(),
                          (uint64_t)kernel_storage_block_bytes(), 0, 0},
            };
        }
        break;
    }

    case ABI_CONSOLE_READ: {
        char kbuf[CONSOLE_LINE_MAX];
        size_t n = 0;
        struct thread *cur;

        if (request->caller != SERVICE_SID_CONSOLE) {
            *status = ABI_DENIED;
            break;
        }
        if (!console_read_line(kbuf, sizeof(kbuf), &n)) {
            *status = ABI_BUSY;
            break;
        }
        if (n > (size_t)request->args.a[1]) {
            n = (size_t)request->args.a[1];
        }
        cur = sched_current();
        if (cur == 0 || cur->as == 0) {
            *status = ABI_DENIED;
            break;
        }
        if (n != 0 && as_copyout(cur->as, (vaddr_t)request->args.a[0], kbuf, n) !=
                         MEM_OK) {
            *status = ABI_BAD_ARG;
            break;
        }
        if (result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {n, 0, 0, 0},
            };
        }
        break;
    }

    case ABI_CONSOLE_WRITE: {
        char kbuf[128];
        size_t n = (size_t)request->args.a[1];
        struct thread *cur;

        if (request->caller != SERVICE_SID_CONSOLE) {
            *status = ABI_DENIED;
            break;
        }
        if (n == 0) {
            break;
        }
        if (n > sizeof(kbuf)) {
            n = sizeof(kbuf);
        }
        cur = sched_current();
        if (cur == 0 || cur->as == 0) {
            *status = ABI_DENIED;
            break;
        }
        if (as_copyin(cur->as, (vaddr_t)request->args.a[0], kbuf, n) != MEM_OK) {
            *status = ABI_BAD_ARG;
            break;
        }
        for (size_t i = 0; i < n; i++) {
            console_putc(kbuf[i]);
        }
        break;
    }

    case ABI_PID_SELF: {
        struct thread *cur = sched_current();

        if (cur == 0) {
            *status = ABI_DENIED;
            break;
        }
        *status = ABI_OK;
        if (result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {object_get_id(&cur->obj), 0, 0, 0},
            };
        }
        break;
    }

    case ABI_AS_SELF: {
        struct thread *cur = sched_current();

        if (cur == 0 || cur->as == 0) {
            *status = ABI_DENIED;
            break;
        }
        *status = ABI_OK;
        if (result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {cur->as->root_pfn, 0, 0, 0},
            };
        }
        break;
    }

    case ABI_POWEROFF:
        if (request->caller != SERVICE_SID_SHELL) {
            *status = ABI_DENIED;
            break;
        }
        console_puts("[kmain] shutdown requested by shell\n");
        sbi_shutdown();
        __builtin_unreachable();

    case ABI_VM_MAP: {
        struct object *obj;
        struct thread *target;
        pfn_t pfn;
        int map_rights;
        if (request->caller != SERVICE_SID_VM) {
            *status = ABI_DENIED;
            break;
        }
        if (!abi_page_aligned(request->args.a[1])) {
            *status = ABI_BAD_ARG;
            break;
        }
        obj = object_lookup((uint32_t)request->args.a[0], OBJ_THREAD);
        if (obj == 0) {
            *status = ABI_BAD_ARG;
            break;
        }
        target = (struct thread *)obj;
        if (target->as == 0) {
            object_unref(obj);
            *status = ABI_BAD_ARG;
            break;
        }
        /*
         * 授权票必须同时绑定 (tid, va, as)。只查 (tid, va) 时，缺页后线程
         * 若已换到另一个 AS，旧票仍能通过，补页就会写进错误的地址空间。
         * 必须先取到 target->as 才能校验，所以这个检查放在 object_lookup 之后。
         */
        if (!pager_authorize_map(request->args.a[0], request->args.a[1],
                                 object_get_id(&target->as->obj))) {
            object_unref(obj);
            *status = ABI_DENIED;
            break;
        }
        if (!cap_vm_can_map_as(target->as)) {
            object_unref(obj);
            *status = ABI_DENIED;
            break;
        }
        map_rights = abi_to_mem_rights(request->args.a[2]);
        if ((map_rights & (MEM_R | MEM_W | MEM_X)) == 0 ||
            ((map_rights & MEM_W) && (map_rights & MEM_X))) {
            object_unref(obj);
            *status = ABI_BAD_ARG;
            break;
        }
        pfn = frame_alloc();
        /* frame_alloc() 的失败值是 PFN_NONE(-1)，不是 0；判 0 会把 OOM
         * 误报成 ALREADY_MAPPED，也会漏掉 first_pfn 恰为 0 的配置。 */
        if (pfn == PFN_NONE) {
            object_unref(obj);
            *status = ABI_NO_MEMORY;
            break;
        }
        {
            uint8_t *page = (uint8_t *)frame_ptr(pfn);
            size_t i = 0;

            for (; i + sizeof(uint64_t) <= PAGE_SIZE; i += sizeof(uint64_t)) {
                *(uint64_t *)(page + i) = 0;
            }
            for (; i < PAGE_SIZE; i++) {
                page[i] = 0;
            }
        }
        {
            int mrc = as_map(target->as, (vaddr_t)request->args.a[1], pfn,
                             map_rights);

            if (mrc != MEM_OK) {
                frame_unref(pfn);
                object_unref(obj);
                *status = mem_error_to_abi(mrc);
                break;
            }
        }
        /*
         * 映射成功后 AS 持有自己的引用；这里必须放掉 frame_alloc() 的
         * 临时引用，否则每个 demand page 都会永久泄漏一帧。
         */
        frame_unref(pfn);

        object_unref(obj);
        if (result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {pfn, 0, 0, 0},
            };
        }
        break;
    }

    case ABI_PAGER_DONE:
        if (request->caller != SERVICE_SID_VM) {
            *status = ABI_DENIED;
        } else {
            *status = pager_complete(request->args.a[0]);
        }
        break;

    case ABI_PAGER_FAIL:
        if (request->caller != SERVICE_SID_VM) {
            *status = ABI_DENIED;
        } else {
            *status =
                pager_fail(request->args.a[0], (int64_t)request->args.a[1]);
        }
        break;

    case ABI_PAGER_READY:
        if (request->caller != SERVICE_SID_VM) {
            *status = ABI_DENIED;
        } else {
            pager_vm_ready();
        }
        break;

    /*
     * 只有 SM 能查"这次 call 的真实发起者是谁"。
     * 返回的是内核在 ep_call 时从 TCB 抄下的 cap_sid，与消息内容无关，
     * 因此不可被用户态伪造。SM 用它替代 msg.words[] 里的自称 sid。
     */
    case ABI_REPLY_CALLER_SID: {
        uint8_t sid = 0;

        /*
         * SM 用它做名称服务鉴权；VM 用它校验 TAG_VM_FORGET 的发送者确实是
         * PM——否则任何持有 VM 发送能力的进程（heaptest 等）都能清掉别人
         * 的地址空间区间。
         */
        if (request->caller != SERVICE_SID_SM &&
            request->caller != SERVICE_SID_VM) {
            *status = ABI_DENIED;
            break;
        }
        if (ipc_reply_caller_sid((uint32_t)request->args.a[0], &sid) != IPC_OK) {
            *status = ABI_BAD_HANDLE;
            break;
        }
        *status = ABI_OK;
        if (result != NULL) {
            *result = (struct abi_message){
                .tag = 0,
                .reply = 0,
                .words = {sid, 0, 0, 0},
            };
        }
        break;
    }

    default:
        *status = ABI_BAD_NR;
        break;
    }
    return SYSCALL_COMPLETE;
}

const struct syscall_backend user_syscall_backend = {
    .ctx = &user_backend,
    .acquire = user_backend_acquire,
    .release = user_backend_release,
    .invoke = user_backend_invoke,
};

void user_syscall_backend_reset(void)
{
    user_backend = (struct user_backend_ctx){0};
}

void user_syscall_complete_async_ipc(struct thread *t, uint32_t reply_id,
                                     int64_t status, const ipc_message *reply)
{
    struct syscall_call *call;
    struct abi_message out = {0};
    uint64_t flags;

    if (t == 0 || reply_id == 0) {
        return;
    }
    call = (struct syscall_call *)t->ucall_words;
    if (status == ABI_OK && reply != 0) {
        out = abi_from_ipc(reply, 0);
    }

    flags = sched_lock();
    if (call->request.nr != ABI_IPC_CALL || call->async_reply_id != reply_id) {
        sched_unlock(flags);
        return;
    }
    if (call->phase != SYSCALL_WAITING) {
        call->async_ipc_done = true;
        call->async_ipc_status = status;
        call->async_ipc_result = out;
        sched_unlock(flags);
        return;
    }
    call->async_reply_id = 0;
    sched_unlock(flags);
    if (syscall_resume(call, status, out)) {
        return;
    }
    flags = sched_lock();
    call->async_ipc_done = true;
    call->async_ipc_status = status;
    call->async_ipc_result = out;
    sched_unlock(flags);
}

/*
 * 放弃一个线程在途的异步 ABI_IPC_CALL（强制终止路径专用）。
 *
 * 必须在真正释放线程之前调用：否则稍后到达的 ep_reply 会经
 * syscall_resume 写进已经被回收的 trap 帧与栈。
 *
 * 做法是撤掉 reply 对象（ep_call_cancel_async 归还槽位）并把 call 置成
 * COMPLETE 状态、清 async_parked，让 sched_thread_discard 允许摘除。
 */
void user_syscall_abandon_async(struct thread *t)
{
    struct syscall_call *call;
    uint32_t reply_id;
    uint64_t flags;

    if (t == 0) {
        return;
    }
    call = (struct syscall_call *)t->ucall_words;

    flags = sched_lock();
    reply_id = call->async_reply_id;
    call->async_reply_id = 0;
    call->async_ipc_done = false;
    if (call->phase == SYSCALL_WAITING) {
        call->frame = 0;   /* 之后即使有人 resume 也写不了用户帧 */
        call->phase = SYSCALL_COMPLETE;
        call->waiting_thread = 0;
    }
    t->async_parked = false;
    sched_unlock(flags);

    if (reply_id != 0) {
        ep_call_cancel_async(reply_id);
    }
}