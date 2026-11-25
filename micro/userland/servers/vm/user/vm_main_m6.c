/*
 * vm_main_m6.c — M6 U-mode VM 服务（由 init -> PM spawn）。
 *
 * 复用 vm_main.c 的日志辅助、vm_port 与 vm.c 的 region/fault 策略；
 * 固定使用 PM 启动参数：
 *   a0=recv a1=ready_send a2=receipt a3=sm_send a4=sid
 */
#include "abi/abi.h"
#include "abi/service.h"
#include "runtime.h"
#include "user_io.h"
#include "vm.h"

#define vm_start vm_start_legacy
#include "vm_main.c"
#undef vm_start

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

    port.ctx = &ctx;
    vm.port = &port;

    (void)user_as_self(&own_as);
    p = log_str(p, "[vm] serving in U-mode as=");
    p = log_hex(p, own_as);
    *p = '\0';
    user_print(line);

    /* 先宣告 pager 可用，再注册 SM：heaptest 拿到句柄后缺页不会被误拒绝。 */
    (void)user_pager_ready();
    register_to_sm(sm_send, sid);

    {
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

        if (msg.tag != TAG_VM_FAULT) {
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