#ifndef MICRO_KERNEL_USER_BACKEND_H
#define MICRO_KERNEL_USER_BACKEND_H

#include "ipc/ipc.h"
#include "syscall.h"

extern const struct syscall_backend user_syscall_backend;

void user_syscall_backend_reset(void);

/* ep_reply / endpoint 销毁时续完异步 ABI_IPC_CALL。reply 为 NULL 表示无载荷。 */
void user_syscall_complete_async_ipc(struct thread *t, uint32_t reply_id,
                                     int64_t status, const ipc_message *reply);

/*
 * 放弃线程在途的异步调用（强制终止路径）。必须在释放该线程前调用，
 * 否则迟到的 ep_reply 会写进已回收的 trap 帧。见 sched_thread_terminate()。
 */
void user_syscall_abandon_async(struct thread *t);

#endif