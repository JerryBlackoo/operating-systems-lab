#ifndef MICRO_MODEL_ABI_H
#define MICRO_MODEL_ABI_H

#include <stdint.h>

enum abi_number {
    ABI_YIELD = 1,
    ABI_THREAD_EXIT = 2,
    ABI_IPC_SEND = 3,
    ABI_IPC_RECV = 4,
    ABI_IPC_CALL = 5,
    ABI_IPC_REPLY = 6,
    ABI_AS_MAP = 7,
    ABI_AS_UNMAP = 8,
};

enum abi_status {
    ABI_OK = 0,
    ABI_BAD_NR = -1,
    ABI_BAD_ARG = -2,
    ABI_BAD_HANDLE = -3,
    ABI_DENIED = -4,
    ABI_NO_MEMORY = -5,
    ABI_CLOSED = -6,
    ABI_ALREADY_MAPPED = -7,
    ABI_NOT_MAPPED = -8,
    ABI_BUSY = -9,
};

enum {
    ABI_READ = 1,
    ABI_WRITE = 2,
    ABI_EXEC = 4,
    ABI_WORDS = 4,
    ABI_REGS = 6,
};

struct abi_regs { uint64_t a[ABI_REGS]; };

struct abi_message {
    uint32_t tag;
    uint32_t reply;
    uint64_t words[ABI_WORDS];
};

static inline struct abi_regs abi_message_args(uint32_t handle, const struct abi_message *msg)
{
    return (struct abi_regs){{
        handle, msg->tag, msg->words[0], msg->words[1], msg->words[2], msg->words[3],
    }};
}

static inline struct abi_message abi_decode_message(struct abi_regs regs)
{
    return (struct abi_message){
        .tag = (uint32_t)regs.a[1],
        .reply = (uint32_t)(regs.a[1] >> 32),
        .words = {regs.a[2], regs.a[3], regs.a[4], regs.a[5]},
    };
}

static inline int64_t abi_decode_status(struct abi_regs regs)
{
    uint64_t word = regs.a[0];

    if (word <= INT64_MAX)
        return (int64_t)word;
    return -1 - (int64_t)(UINT64_MAX - word);
}

static inline struct abi_regs abi_result(int64_t status, const struct abi_message *msg)
{
    struct abi_regs regs = {{(uint64_t)status}};

    if (status == ABI_OK && msg) {
        regs.a[1] = ((uint64_t)msg->reply << 32) | msg->tag;
        for (unsigned i = 0; i < ABI_WORDS; i++)
            regs.a[i + 2] = msg->words[i];
    }
    return regs;
}

#if defined(__riscv)
static inline struct abi_regs abi_ecall(uint64_t nr, struct abi_regs input)
{
    register uint64_t a0 __asm__("a0") = input.a[0];
    register uint64_t a1 __asm__("a1") = input.a[1];
    register uint64_t a2 __asm__("a2") = input.a[2];
    register uint64_t a3 __asm__("a3") = input.a[3];
    register uint64_t a4 __asm__("a4") = input.a[4];
    register uint64_t a5 __asm__("a5") = input.a[5];
    register uint64_t a7 __asm__("a7") = nr;

    __asm__ volatile("ecall"
                     : "+r"(a0), "+r"(a1), "+r"(a2), "+r"(a3), "+r"(a4), "+r"(a5)
                     : "r"(a7) : "memory");
    return (struct abi_regs){{a0, a1, a2, a3, a4, a5}};
}
#endif

#endif
