#ifndef MICRO_MODEL_SYSCALL_H
#define MICRO_MODEL_SYSCALL_H

#include <stdbool.h>
#include "abi/abi.h"
#include "trap.h"

enum syscall_phase {
    SYSCALL_IDLE, SYSCALL_IGNORED, SYSCALL_COMPLETE, SYSCALL_WAITING, SYSCALL_EXITED,
};
enum syscall_kind { SYSCALL_NONE, SYSCALL_ENDPOINT, SYSCALL_AS, SYSCALL_FRAME, SYSCALL_REPLY };
enum {
    SYSCALL_SEND = 1 << 3,
    SYSCALL_RECV = 1 << 4,
    SYSCALL_MAP = 1 << 5,
    SYSCALL_ANSWER = 1 << 6,
};

struct syscall_request {
    uint32_t caller;
    enum abi_number nr;
    struct abi_regs args;
    uint32_t refs[2];
};

struct syscall_backend {
    void *ctx;
    int64_t (*acquire)(void *, uint32_t caller, uint32_t handle,
                       enum syscall_kind, unsigned rights, uint32_t *ref);
    void (*release)(void *, uint32_t ref);
    enum syscall_phase (*invoke)(void *, const struct syscall_request *,
                                 int64_t *status, struct abi_message *result);
};

struct syscall_call {
    enum syscall_phase phase;
    struct syscall_request request;
    struct trap_frame *frame;
    const struct syscall_backend *backend;
    unsigned held;
};

enum syscall_phase syscall_dispatch(struct syscall_call *call, struct trap_frame *frame,
                                    uint32_t caller, const struct syscall_backend *backend);
bool syscall_resume(struct syscall_call *call, int64_t status, struct abi_message message);

#endif
