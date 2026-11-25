/*
 * init_main_m6.c — M6/M7 init：固定顺序拉起 VM/heaptest，再拉起 shell。
 *
 * 复用 init_main.c 中的 sm_lookup / pm_spawn / spawn-failed 诊断实现，
 * 只在外面重定义启动顺序与 M6 阶段同步。
 */
#include "abi/abi.h"
#include "runtime.h"
#include "service_proto.h"
#include "user_io.h"

#define init_main init_main_legacy
#include "init_main.c"
#undef init_main

void init_main(uint32_t recv_only, uint32_t sm_send, uint32_t ready_ep,
               uint32_t plan, uint32_t sid)
{
    uint32_t pm = 0;
    /* 固定顺序：VM 先起，heaptest 通过 SM 查找它；随后 fs/console/shell。 */
    static const uint32_t images[] = {4, 5, 6, 1, 2, 3};
    static const char *const names[] = {"vm", "heaptest", "heaptest2", "fs",
                                        "console", "shell"};

    (void)recv_only;
    (void)plan;
    user_print_u64("[init] boot: caps=3 plan=", 6u, "");
    user_print_u64("[init] sid=", sid, "");

    sm_lookup(sm_send, "pm", sid, &pm);

    for (uint32_t i = 0; i < 6u; i++) {
        uint64_t pid = 0;
        int64_t status = ABI_OK;

        if (pm_spawn(pm, images[i], &pid, &status) != 0) {
            print_spawn_failed(images[i], status);
            for (;;) {
                abi_ecall(ABI_YIELD, (struct abi_regs){0});
            }
        }
        user_print_u64("[init] spawn ok pid=", pid, "");
        user_print(names[i]);

        /* 前三个是 M6 判别进程：等它们真正跑完再启动 shell，避免日志交错。 */
        if (i == 2u) {
            uint32_t m6_done = 0;

            while ((ready_mask & 0x38u) != 0x38u || m6_done != 0x3u) {
                struct abi_message msg = {0};

                if (user_ipc_recv(ready_ep, &msg) != ABI_OK) {
                    abi_ecall(ABI_YIELD, (struct abi_regs){0});
                    continue;
                }
                if (msg.tag == TAG_VM_PHASE_DONE) {
                    m6_done |= 1u << (uint32_t)(msg.words[0] & 1u);
                    continue;
                }
                if (msg.tag != TAG_READY) {
                    continue;
                }
                if (msg.words[0] < 0x2001 || msg.words[0] >= 0x2007) {
                    continue;
                }
                ready_mask |= 1u << (uint32_t)(msg.words[0] - 0x2001);
            }
        }
    }

    while ((ready_mask & 0x7u) != 0x7u) {
        struct abi_message msg = {0};

        if (user_ipc_recv(ready_ep, &msg) != ABI_OK || msg.tag != TAG_READY) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
            continue;
        }
        if (msg.words[0] < 0x2001 || msg.words[0] >= 0x2007) {
            continue;
        }
        ready_mask |= 1u << (uint32_t)(msg.words[0] - 0x2001);
    }
    user_print("[init] ready");

    for (;;) {
        struct abi_message msg = {0};

        if (user_ipc_recv(ready_ep, &msg) != ABI_OK) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
        }
    }
}