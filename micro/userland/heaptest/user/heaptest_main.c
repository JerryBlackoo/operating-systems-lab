/*
 * heaptest_main.c — M6：从 SM 查 VM endpoint，显式 reserve 后访问未映射页；
 * 由 VM 服务补页后继续。role=1 的实例额外 reserve 一个只读区间再写，
 * 验证 VM 的越权拒绝路径（进程必须收到明确错误并退出）。
 */
#include "abi/abi.h"
#include "abi/service.h"
#include "runtime.h"
#include "user_io.h"

#define HEAP_PROBE_VA 0x40020000UL
#define DENY_PROBE_VA 0x50000000UL

static uint64_t svc_name_pack(const char *s)
{
    uint64_t v = 0;

    for (unsigned i = 0; s[i] != '\0' && i < 8; i++) {
        v |= (uint64_t)(uint8_t)s[i] << (8U * i);
    }
    return v;
}

static int sm_lookup(uint32_t sm_send, const char *name, uint32_t sid,
                     uint32_t *handle)
{
    struct abi_message req = {
        .tag = TAG_SM_LOOKUP,
        .words = {0, svc_name_pack(name), sid, 0},
    };

    for (;;) {
        struct abi_message rep = {0};

        if (user_ipc_call(sm_send, &req, &rep) == ABI_OK &&
            rep.tag == TAG_SM_OK && rep.words[0] != 0) {
            *handle = (uint32_t)rep.words[0];
            return 0;
        }
        abi_ecall(ABI_YIELD, (struct abi_regs){0});
    }
}

static int reserve(uint32_t vm_ep, uint64_t tid, uint64_t va, uint64_t len,
                   uint64_t rights)
{
    struct abi_message request = {
        .tag = TAG_VM_RESERVE,
        .words = {tid, va, len, rights},
    };
    struct abi_message reply = {0};

    if (user_ipc_call(vm_ep, &request, &reply) != ABI_OK ||
        reply.tag != TAG_VM_RESERVE_OK || reply.words[0] != 0) {
        return -1;
    }
    return 0;
}

/*
 * 撤销一个 region。VM 的 region/驻留页槽位有限，不释放就会在反复
 * reserve 后永久耗尽。保留这个包装作为 TAG_VM_RELEASE 的客户端示例；
 * 长期复用的断言放在 vm 的 host demo 里（这里跑会与 shell 争 CPU）。
 */
__attribute__((used)) static int release(uint32_t vm_ep, uint64_t tid, uint64_t va,
                           uint64_t len)
{
    struct abi_message request = {
        .tag = TAG_VM_RELEASE,
        .words = {tid, va, len, 0},
    };
    struct abi_message reply = {0};

    if (user_ipc_call(vm_ep, &request, &reply) != ABI_OK ||
        reply.tag != TAG_VM_RELEASE_OK || reply.words[0] != 0) {
        return -1;
    }
    return 0;
}

void heaptest_start(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3,
                    uint32_t a4) __attribute__((used));
void heaptest_start(uint32_t a0, uint32_t a1, uint32_t a2, uint32_t a3,
                    uint32_t a4)
{
    volatile int *p;
    uint64_t tid = 0;
    uint32_t vm_ep = 0;
    uint32_t role;
    int pm_boot = a2 != 0;

    if (pm_boot) {
        struct abi_message ready = {
            .tag = TAG_READY,
            .words = {a1, 0, 0, 0},
        };

        (void)user_ipc_send(a0, &ready);
        role = a4;
    } else {
        vm_ep = a0;
        role = a1;
    }

    if (user_pid_self(&tid) != ABI_OK) {
        user_print("[heaptest] pid self failed");
        abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    }
    if (pm_boot) {
        sm_lookup(a2, "vm", a3, &vm_ep);
    }
    if (reserve(vm_ep, tid, HEAP_PROBE_VA, ABI_PAGE_SIZE,
                ABI_READ | ABI_WRITE) != 0) {
        user_print("[heaptest] reserve failed");
        abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    }

    user_print("[heaptest] probing unmapped page");
    p = (volatile int *)(uintptr_t)HEAP_PROBE_VA;
    *p = 0x12345678;
    user_print("[heaptest] store ok");

    if (pm_boot) {
        struct abi_message done = {
            .tag = TAG_VM_PHASE_DONE,
            .words = {role, 0, 0, 0},
        };

        (void)user_ipc_send(a0, &done);
    }

    if (role == 1U) {
        /* 只读区间：随后 store 必须被 VM 以权限理由拒绝。 */
        if (reserve(vm_ep, tid, DENY_PROBE_VA, ABI_PAGE_SIZE, ABI_READ) != 0) {
            user_print("[heaptest] deny reserve failed");
            abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
        }
        user_print("[heaptest] probing read-only page with store");
        p = (volatile int *)(uintptr_t)DENY_PROBE_VA;
        *p = 0x5a5a5a5a;
        user_print("[heaptest] deny probe unexpectedly resumed");
    }

    /*
     * 探测完成后退出，避免测试线程永久 ABI_YIELD 占满就绪队列、
     * 饿死 idle 并破坏 M4 的 heartbeat/console_irq_test 观测。
     *
     * 槽位长期复用（reserve→fault→release 多轮）不在这里跑：它需要几十轮
     * IPC 往返，会与 shell/console 争抢 CPU 并打散 demo 的关键输出。那条
     * 路径改由 `make -C userland/servers/vm demo` 的 host 用例覆盖。
     */
    abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
    for (;;) {
    }
}