#ifndef MICRO_MODEL_SYSCALL_H
#define MICRO_MODEL_SYSCALL_H

#include <stdbool.h>
#include "abi/abi.h"
#include "trap.h"

/*
 * 一次系统调用的阶段。
 *
 *   IDLE ──U-mode ecall──> ENTERED ──拒绝──> COMPLETE
 *     │                       │
 *     │                       └──校验通过──> DISPATCHED ──同步完成──> COMPLETE
 *     │                                          │
 *     │                                          ├──阻塞──> WAITING ──后端续完──> COMPLETE
 *     │                                          └──退出线程──> EXITED
 *     └──非 U-mode ecall──> IGNORED（不回写、不推进 sepc）
 *
 * ENTERED 表示「已受理」：sepc 已推进到 ecall 的下一条指令，参数已复制。
 * 因此未知编号和参数错误也从下一条指令继续，不会再次触发同一个 ecall。
 */
enum syscall_phase {
    SYSCALL_IDLE,
    SYSCALL_IGNORED,
    SYSCALL_ENTERED,
    SYSCALL_DISPATCHED,
    SYSCALL_WAITING,
    SYSCALL_COMPLETE,
    SYSCALL_EXITED,
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

/*
 * 后端只暴露三类原语，线程状态、页表和端点队列仍由各自模块维护：
 *   acquire —— 在调用者的能力空间里解析句柄，成功后在途调用持有引用
 *   release —— 撤销 acquire 持有的引用
 *   invoke  —— 执行调用号对应的原语，同步完成或返回 WAITING/EXITED
 */
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

/*
 * 唯一的入口：解码 -> 校验 -> 分发 -> 完成。
 * 非 U-mode 同步 ecall 返回 IGNORED；其余调用都返回 COMPLETE、
 * WAITING 或 EXITED。WAITING 时调用帧保持有效，由 syscall_resume 续完。
 */
enum syscall_phase syscall_dispatch(struct syscall_call *call, struct trap_frame *frame,
                                    uint32_t caller, const struct syscall_backend *backend);

/* 阻塞调用完成后写回原调用帧；只有 WAITING 状态的调用会成功。 */
bool syscall_resume(struct syscall_call *call, int64_t status, struct abi_message message);

#endif
