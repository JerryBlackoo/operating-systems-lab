/*
 * vm_main.c — M6 U-mode VM 服务：真正使用 vm.c 的 region/fault 策略。
 *
 * 内核只保留帧分配和页表原语；VM 在用户态维护
 *   Region = (target_tid/as, [lo, hi), rights)
 *   resident = (target_tid/as, page_va) -> frame
 * 并通过 endpoint + capability 接收显式 reserve 与内核投递的缺页。
 */
#include "abi/abi.h"
#include "abi/service.h"
#include "runtime.h"
#include "service_proto.h"
#include "user_io.h"
#include "vm.h"

enum { LOG_LEN = 160 };

struct vm_user_ctx {
    uint64_t tid;
    uint64_t va;
    uint32_t fake_frame;
};

static char *log_str(char *p, const char *s)
{
    while (*s) {
        *p++ = *s++;
    }
    return p;
}

static char *log_hex(char *p, uint64_t v)
{
    static const char dg[] = "0123456789abcdef";

    for (int i = 60; i >= 0; i -= 4) {
        *p++ = dg[(v >> i) & 0xf];
    }
    return p;
}

static char *log_dec(char *p, uint64_t v)
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
    while (n != 0) {
        *p++ = tmp[--n];
    }
    return p;
}

static void log_reserve(uint64_t tid, uint64_t va, uint64_t len)
{
    char line[LOG_LEN];
    char *p = line;

    p = log_str(p, "[vm] reserve tid=");
    p = log_dec(p, tid);
    p = log_str(p, " va=");
    p = log_hex(p, va);
    p = log_str(p, " len=");
    p = log_dec(p, len);
    p = log_str(p, " ok");
    *p = '\0';
    user_print(line);
}

static void log_reserve_failed(uint64_t tid, uint64_t va, uint64_t len,
                               int result)
{
    char line[LOG_LEN];
    char *p = line;

    p = log_str(p, "[vm] reserve tid=");
    p = log_dec(p, tid);
    p = log_str(p, " va=");
    p = log_hex(p, va);
    p = log_str(p, " len=");
    p = log_dec(p, len);
    p = log_str(p, " denied status=");
    p = log_dec(p, (uint64_t)(-result));
    *p = '\0';
    user_print(line);
}

static void log_map(uint64_t tid, uint64_t va, uint64_t frame){
    char line[LOG_LEN];
    char *p = line;

    p = log_str(p, "[vm] map tid=");
    p = log_dec(p, tid);
    p = log_str(p, " va=");
    p = log_hex(p, va);
    p = log_str(p, " frame=");
    p = log_hex(p, frame);
    p = log_str(p, " ok");
    *p = '\0';
    user_print(line);
}

static void log_deny(uint64_t tid, uint64_t va, const char *reason){
    char line[LOG_LEN];
    char *p = line;

    p = log_str(p, "[vm] deny tid=");
    p = log_dec(p, tid);
    p = log_str(p, " va=");
    p = log_hex(p, va);
    p = log_str(p, " reason=");
    p = log_str(p, reason);
    *p = '\0';
    user_print(line);
}

static void log_release(uint64_t tid, uint64_t va, uint64_t len)
{
    (void)tid;
    (void)va;
    (void)len;
    /*
     * 成功释放不逐条打印：槽位复用循环会跑几十轮，每条日志都要走一次
     * console IPC，既拖慢启动也淹没 demo 的关键行。失败仍然打印。
     */
}

static void log_release_failed(uint64_t tid, uint64_t va, uint64_t len,
                               int result){
    char line[LOG_LEN];
    char *p = line;

    p = log_str(p, "[vm] release tid=");
    p = log_dec(p, tid);
    p = log_str(p, " va=");
    p = log_hex(p, va);
    p = log_str(p, " len=");
    p = log_dec(p, len);
    p = log_str(p, " denied status=");
    p = log_dec(p, (uint64_t)(-result));
    *p = '\0';
    user_print(line);
}

static void log_forget(uint64_t tid, unsigned dropped)
{
    char line[LOG_LEN];
    char *p = line;

    p = log_str(p, "[vm] forget tid=");
    p = log_dec(p, tid);
    p = log_str(p, " regions=");
    p = log_dec(p, dropped);
    *p = '\0';
    user_print(line);
}

static uint32_t u_alloc(void *ctx)
{
    struct vm_user_ctx *c = ctx;

    /*
     * 模型里的 alloc 只提供一个非零驻留槽标记；真实帧由内核 ABI_VM_MAP
     * 分配并在 map 端口回传。这样 vm.c 的 region/fault 状态机不直接
     * 依赖内核物理分配器。
     */
    c->fake_frame++;
    return c->fake_frame;
}

static void u_clear(void *ctx, uint32_t frame)
{
    (void)ctx;
    (void)frame;
}

static bool u_map(void *ctx, uint32_t as, uint64_t va, uint32_t frame,
                  unsigned rights)
{
    struct vm_user_ctx *c = ctx;
    struct abi_message result = {0};
    int64_t st;

    (void)frame;
    c->tid = as;
    c->va = va;
    st = abi_decode_result(
        abi_ecall(ABI_VM_MAP,
                  (struct abi_regs){{as, va, rights, 0, 0, 0}}),
        &result);
    if (st != ABI_OK) {
        log_deny(as, va, "vm-map-error");
        return false;
    }
    log_map(as, va, result.words[0]);
    return true;
}

static void u_unmap(void *ctx, uint32_t as, uint64_t va)
{
    (void)ctx;
    (void)as;
    (void)va;
}

static void u_unref(void *ctx, uint32_t frame)
{
    (void)ctx;
    (void)frame;
}

static const struct vm_port user_port = {
    .ctx = 0, /* 在 vm_start 里绑定到栈上的 ctx */
    .alloc = u_alloc,
    .clear = u_clear,
    .map = u_map,
    .unmap = u_unmap,
    .unref = u_unref,
};

static void register_to_sm(uint32_t sm_send, uint32_t sid)
{
    struct abi_message req = {
        .tag = TAG_SM_REGISTER,
        .words = {0, svc_name_pack("vm"), sid, 0},
    };

    for (;;) {
        struct abi_message rep = {0};

        if (user_ipc_call(sm_send, &req, &rep) == ABI_OK &&
            rep.tag == TAG_SM_OK) {
            return;
        }
        abi_ecall(ABI_YIELD, (struct abi_regs){0});
    }
}

static unsigned fault_access(unsigned access)
{
    /* vm.c 要求单权限位；store 缺页在内核侧是 READ|WRITE。 */
    if (access & ABI_WRITE) {
        return ABI_WRITE;
    }
    if (access & ABI_EXEC) {
        return ABI_EXEC;
    }
    return ABI_READ;
}

void vm_start(uint32_t recv_ep, uint32_t ready_send, uint64_t receipt,
              uint32_t sm_send, uint32_t sid) __attribute__((used));
void vm_start(uint32_t recv_ep, uint32_t ready_send, uint64_t receipt,
              uint32_t sm_send, uint32_t sid)
{
    struct vm_user_ctx ctx = {0};
    struct vm_port port = user_port;
    struct vm vm = {0};
    uint64_t own_as = 0;
    char line[LOG_LEN];
    char *p = line;
    int pm_boot = receipt != 0;

    port.ctx = &ctx;
    vm.port = &port;

    (void)user_as_self(&own_as);
    p = log_str(p, "[vm] serving in U-mode as=");
    p = log_hex(p, own_as);
    *p = '\0';
    user_print(line);

    /*
     * 先宣告 pager 可用，再注册 SM、回执 ready。这样 heaptest 拿到 vm
     * 句柄并缺页时，pager 不会因为“VM 尚未 ready”而误拒绝。
     */
    (void)user_pager_ready();
    /* 内核直启 heaptest 走预置 vm 能力，无需 SM；仅 PM spawn 路径要 register。 */
    if (pm_boot && sm_send != 0) {
        register_to_sm(sm_send, sid);
    }

    if (pm_boot) {
        struct abi_message ready = {
            .tag = TAG_READY,
            .words = {receipt, 0, 0, 0},
        };

        (void)user_ipc_send(ready_send, &ready);
    }

    for (;;) {
        struct abi_message msg = {0};

        if (user_ipc_recv(recv_ep, &msg) != ABI_OK) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
            continue;
        }

        if (msg.tag == TAG_VM_RESERVE) {
            uint64_t tid = msg.words[0];
            uint64_t va = msg.words[1];
            uint64_t len = msg.words[2];
            unsigned rights = (unsigned)msg.words[3];
            int result = vm_reserve(&vm, (uint32_t)tid, va, len, rights);
            struct abi_message reply = {
                .tag = TAG_VM_RESERVE_OK,
                .words = {(uint64_t)(int64_t)result, 0, 0, 0},
            };

            if (result == VM_OK) {
                log_reserve(tid, va, len);
            } else {
                log_reserve_failed(tid, va, len, result);
            }
            (void)user_ipc_reply(msg.reply, &reply);
            continue;
        }

        if (msg.tag == TAG_VM_RELEASE) {
            uint64_t tid = msg.words[0];
            uint64_t va = msg.words[1];
            uint64_t len = msg.words[2];
            int result = vm_release(&vm, (uint32_t)tid, va, len);
            struct abi_message reply = {
                .tag = TAG_VM_RELEASE_OK,
                .words = {(uint64_t)(int64_t)result, 0, 0, 0},
            };

            if (result == VM_OK) {
                log_release(tid, va, len);
            } else {
                log_release_failed(tid, va, len, result);
            }
            (void)user_ipc_reply(msg.reply, &reply);
            continue;
        }

        if (msg.tag == TAG_VM_FORGET) {
            uint64_t tid = msg.words[0];
            uint8_t caller_sid = 0;

            /*
             * 只有 PM 能清理别人的地址空间。
             *
             * heaptest/heaptest2 也持有 VM 的发送能力（M6 需要它们直接
             * 请求 reserve/fault），若不校验身份，它们就能用 TAG_VM_FORGET
             * 清掉任意进程的区间，让对方的缺页全部失败。
             *
             * 身份只能来自内核：`user_reply_caller_sid` 返回的是 ep_call 时
             * 从 TCB 抄下的 cap_sid，与消息内容无关。
             */
            if (msg.reply == 0 ||
                user_reply_caller_sid(msg.reply, &caller_sid) != ABI_OK ||
                caller_sid != USER_SID_PM) {
                if (msg.reply != 0) {
                    struct abi_message reply = {
                        .tag = TAG_VM_ERROR,
                        .words = {(uint64_t)(int64_t)ABI_DENIED, 0, 0, 0},
                    };

                    (void)user_ipc_reply(msg.reply, &reply);
                }
                continue;
            }

            {
                unsigned dropped = vm_forget(&vm, (uint32_t)tid);
                struct abi_message reply = {
                    .tag = TAG_VM_RELEASE_OK,
                    .words = {dropped, 0, 0, 0},
                };

                log_forget(tid, dropped);
                (void)user_ipc_reply(msg.reply, &reply);
            }
            continue;
        }

        if (msg.tag != TAG_VM_FAULT) {
            /* 未知调用必须显式回错，否则带 reply 的客户端会永久等待。 */
            if (msg.reply != 0) {
                struct abi_message reply = {
                    .tag = TAG_VM_ERROR,
                    .words = {(uint64_t)(int64_t)ABI_BAD_ARG, 0, 0, 0},
                };

                (void)user_ipc_reply(msg.reply, &reply);
            }
            continue;
        }
        if (msg.reply != 0) {
            /* 缺页是内核 POST（无 reply）；用户不能伪造 fault 走 pager 路径。 */
            struct abi_message reply = {
                .tag = TAG_VM_ERROR,
                .words = {(uint64_t)(int64_t)ABI_BAD_ARG, 0, 0, 0},
            };

            (void)user_ipc_reply(msg.reply, &reply);
            continue;
        }

        {
            uint64_t tid = msg.words[0];
            uint64_t va = msg.words[1] & ~(uint64_t)(ABI_PAGE_SIZE - 1U);
            unsigned access = fault_access((unsigned)msg.words[2]);
            int result = vm_fault(&vm, (uint32_t)tid, va, access);

            if (result == VM_RESUME) {
                (void)user_pager_done(tid);
            } else {
                const char *reason;

                if (result == VM_NO_SPACE) {
                    reason = "capacity";
                } else if (result == VM_BAD_RANGE) {
                    reason = "bad-range";
                } else if (result == VM_CONFLICT) {
                    reason = "conflict";
                } else if (result == VM_MAP_ERROR) {
                    reason = "map-error";
                } else {
                    reason = "rights";
                }
                log_deny(tid, va, reason);
                (void)user_pager_fail(
                    tid, result == VM_NO_SPACE ? ABI_NO_MEMORY : ABI_DENIED);
            }
        }
    }
}