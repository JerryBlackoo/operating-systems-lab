/*
 * heaptest_main_m6.c — M6：从 SM 查 VM endpoint，显式 reserve 后触发缺页。
 *
 * 复用 heaptest_main.c 的 SM 查找 / reserve 原语，但固定使用 PM 启动参数：
 *   a0=ready_send a1=receipt a2=sm_send a3=sid a4=role
 * role=1 的实例额外 reserve 只读区间再写，验证 VM 的越权拒绝路径。
 */
#include "abi/abi.h"
#include "abi/service.h"
#include "runtime.h"
#include "user_io.h"

#define heaptest_start heaptest_start_legacy
#include "heaptest_main.c"
#undef heaptest_start

#define HEAP_PROBE_VA_M6 0x40020000UL
#define DENY_PROBE_VA_M6 0x50000000UL

void heaptest_start(uint32_t ready_send, uint64_t receipt, uint32_t sm_send,
                    uint32_t sid, uint32_t role) __attribute__((used));
void heaptest_start(uint32_t ready_send, uint64_t receipt, uint32_t sm_send,
                    uint32_t sid, uint32_t role)
{
    volatile int *p;
    uint64_t tid = 0;
    uint32_t vm_ep = 0;

    {
        struct abi_message ready = {
            .tag = TAG_READY,
            .words = {receipt, 0, 0, 0},
        };

        (void)user_ipc_send(ready_send, &ready);
    }

    if (user_pid_self(&tid) != ABI_OK) {
        user_print("[heaptest] pid self failed");
        abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    }
    sm_lookup(sm_send, "vm", sid, &vm_ep);
    if (reserve(vm_ep, tid, HEAP_PROBE_VA_M6, ABI_PAGE_SIZE,
                ABI_READ | ABI_WRITE) != 0) {
        user_print("[heaptest] reserve failed");
        abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    }

    user_print("[heaptest] probing unmapped page");
    p = (volatile int *)(uintptr_t)HEAP_PROBE_VA_M6;
    *p = 0x12345678;
    user_print("[heaptest] store ok");

    {
        struct abi_message done = {
            .tag = TAG_VM_PHASE_DONE,
            .words = {role, 0, 0, 0},
        };

        (void)user_ipc_send(ready_send, &done);
    }

    if (role == 1U) {
        if (reserve(vm_ep, tid, DENY_PROBE_VA_M6, ABI_PAGE_SIZE, ABI_READ) != 0) {
            user_print("[heaptest] deny reserve failed");
            abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
        }
        user_print("[heaptest] probing read-only page with store");
        p = (volatile int *)(uintptr_t)DENY_PROBE_VA_M6;
        *p = 0x5a5a5a5a;
        user_print("[heaptest] deny probe unexpectedly resumed");
    }

    abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    for (;;) {
        abi_ecall(ABI_YIELD, (struct abi_regs){0});
    }
}