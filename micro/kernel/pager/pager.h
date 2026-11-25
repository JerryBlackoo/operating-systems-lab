#ifndef MICRO_KERNEL_PAGER_H
#define MICRO_KERNEL_PAGER_H

#include <stdbool.h>
#include <stdint.h>

struct endpoint;
struct trap_frame;

void pager_init(struct endpoint *vm_recv);

/* VM 服务进入 recv 前调用，表示 pager 可以开始接收缺页请求。 */
void pager_vm_ready(void);

/* 处理 U-mode 缺页；成功则阻塞后恢复，不推进 sepc。 */
bool pager_handle_user_fault(struct trap_frame *frame);

/* VM 服务在补页后唤醒缺页线程。 */
int64_t pager_complete(uint64_t fault_tid);

/*
 * VM 服务拒绝一次缺页：唤醒缺页线程并写入明确错误；该线程随后退出。
 * status 只允许负的 ABI_* 错误码。
 */
int64_t pager_fail(uint64_t fault_tid, int64_t status);

/*
 * 缺页超时：一次投递给 VM 的缺页请求最多等待 PAGER_TIMEOUT_NS。到期仍未
 * 收到 ABI_PAGER_DONE/FAIL 时，内核把该请求标成完成（ABI_TIMED_OUT）并
 * 唤醒缺页线程，由它自己打印并退出。
 *
 * 没有这条路径时，VM 卡住或被杀会让缺页线程永久阻塞在内部等待队列上，
 * 既不再响应用户，也无法被回收。
 */

/*
 * 目标 AS 的一次性补页授权票。内核投递 TAG_VM_FAULT 时登记 (tid, va, as)；
 * VM 服务调用 ABI_VM_MAP 时只能消费这张票，不能映射任意进程的任意 VA。
 *
 * as_id 必须一起校验：只比 (tid, va) 时，同一线程换到新 AS 后旧票仍然有效，
 * 补页就会落到错误的地址空间。
 */
bool pager_authorize_map(uint64_t fault_tid, uint64_t va, uint64_t as_id);


#endif