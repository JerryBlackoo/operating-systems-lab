#include "abi/abi.h"
#include "runtime.h"

enum { TAG_READY = 0x401 };

void stub_start(uint32_t ready_send, uint64_t receipt) __attribute__((used));
void stub_start(uint32_t ready_send, uint64_t receipt)
{
    struct abi_message msg = {
        .tag = TAG_READY,
        .words = {receipt, 0, 0, 0},
    };

    if (user_ipc_send(ready_send, &msg) != ABI_OK) {
        for (;;) {
            abi_ecall(ABI_YIELD, (struct abi_regs){0});
        }
    }
    abi_ecall(ABI_THREAD_EXIT, (struct abi_regs){0});
}