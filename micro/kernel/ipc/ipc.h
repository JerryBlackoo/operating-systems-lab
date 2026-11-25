#ifndef MICRO_KERNEL_IPC_H
#define MICRO_KERNEL_IPC_H

#include <stdbool.h>
#include <stdint.h>

#include "object.h"
#include "sched.h"

/*
 * 最小 IPC：同步 rendezvous + 固定大小消息 + 单次 reply。
 *
 * endpoint 的 sendq / recvq 至多一个非空：
 *   - 发送者遇到等待的接收者：直接移交并唤醒接收者；
 *   - 否则发送者入 sendq 阻塞；
 *   - 接收对称。
 *
 * 消息寄存器放在 TCB（struct thread_msg）里，不额外分配内核缓冲。
 * 布局固定、无填充：
 *   tag(4) + reply_id(4) + words[4](32) = 40 字节，其中载荷固定 32 字节。
 * 发送/接收/回复都只搬运这份定长值，不解释 words 的内容。
 *
 * reply_id 是 ep_call 时由内核签发的单次 bearer token；服务端凭 token
 * 调用一次 ep_reply。endpoint 销毁会取消所有尚未回复的在途调用。
 */

#define IPC_MAX_ENDPOINTS 16
#define IPC_MAX_REPLIES 16
/* 内核侧排队投递深度：接收者尚未 recv 时消息落在这里，不丢也不阻塞发送者。 */
#define IPC_MAX_POSTS 8
#define IPC_MSG_WORDS THREAD_MSG_WORDS
#define IPC_MSG_BYTES (IPC_MSG_WORDS * 8)

enum ipc_status {
    IPC_OK = 0,
    IPC_PENDING = 1,
    IPC_EDEAD = -1,
    IPC_EBUSY = -2,
    IPC_EINVAL = -3,
    IPC_ENOMEM = -4
};

enum ep_state {
    EP_IDLE = 0,
    EP_SEND_WAIT,
    EP_RECV_WAIT,
    EP_DYING
};

typedef struct thread_msg ipc_message;

_Static_assert(IPC_MSG_WORDS == 4, "ipc message carries four 64-bit words");
_Static_assert(sizeof(((ipc_message *)0)->words) == IPC_MSG_BYTES,
               "ipc payload is a fixed 32-byte block");
_Static_assert(sizeof(ipc_message) == 40, "ipc message has no padding");

struct endpoint {
    struct object obj;
    enum ep_state state;
    struct waitq sendq;
    struct waitq recvq;
    /* 内核侧排队投递：pager 等内核生产者可在接收者不在时先入队。 */
    struct thread_msg posts[IPC_MAX_POSTS];
    uint8_t post_head;
    uint8_t post_tail;
    uint8_t post_count;
    bool used;
};

struct reply {
    struct object obj;
    struct endpoint *ep; /* 所属 endpoint：销毁时据此取消在途 call */
    struct waitq waitq;
    struct thread_msg msg;
    /*
     * 发起 ep_call 的线程真实 cap_sid，由内核在 reply_alloc 时从 TCB 抄下来。
     * 服务端用 ipc_reply_caller_sid() 查它，从而不必相信消息里自称的 sid。
     */
    uint8_t caller_sid;
    /*
     * 消息交付给接收者时记下接收者的 cap_sid。ep_reply_from() 只接受
     * owner 本人的回复，避免任意进程拿猜测/窃取的 reply_id 抢答别人的
     * 请求（reply token 本身只是 bearer 值，不是能力）。
     */
    uint8_t owner_sid;
    bool done;
    bool aborted;
    bool used;
    /* 异步 syscall IPC_CALL：调用者在 syscall_waitq 上，不在 waitq。 */
    struct thread *async_caller;
};

void ipc_init(void);

struct endpoint *ep_create(void);
void ep_ref(struct endpoint *ep);
void ep_unref(struct endpoint *ep);
uint32_t ep_id(const struct endpoint *ep);

int ep_send(struct endpoint *ep, const ipc_message *msg);
int ep_recv(struct endpoint *ep, ipc_message *msg);

/*
 * 内核侧排队投递：接收者在 recvq 上则直接移交；否则把消息放进 endpoint
 * 的 posts 环形队列。调用者不阻塞，接收者下次 ep_recv 时按 FIFO 取走。
 * 队列满返回 IPC_ENOMEM，endpoint 已销毁返回 IPC_EDEAD。
 */
int ep_post(struct endpoint *ep, const ipc_message *msg);

/*
 * call = send + 等待 reply；服务端从收到的消息里读 reply_id，
 * 处理后调用 ep_reply(reply_id, ...)。reply_id 是单次 bearer token；
 * 真正的权限控制应由 capability 模块完成。
 *
 * 返回：
 *   IPC_OK      消息已交给接收者且收到回复；
 *   IPC_EDEAD   发送阶段 endpoint 已销毁，或等待回复期间 endpoint 被销毁；
 *   IPC_EINVAL  参数非法；
 *   IPC_ENOMEM  一次性 reply 对象池已耗尽。
 *
 * ep_reply 返回：
 *   IPC_OK      回复已投递（若调用者已阻塞则同时唤醒它）；
 *   IPC_EBUSY   该 reply_id 已经投递过（单次语义）；
 *   IPC_EDEAD   endpoint 已销毁，回复不再投递；
 *   IPC_EINVAL  reply_id 不是存活的 OBJ_REPLY 对象。
 */
int ep_call(struct endpoint *ep, const ipc_message *msg, ipc_message *reply);

/*
 * 异步 IPC_CALL 两步：先 open 拿 reply_id（并登记 async_caller），
 * 调用者在 ep_send 前写入 syscall_call.async_reply_id，再 deliver。
 */
int ep_call_open(struct endpoint *ep, uint32_t *reply_id_out);
int ep_call_deliver(struct endpoint *ep, uint32_t reply_id,
                    const ipc_message *msg);
void ep_call_cancel_async(uint32_t reply_id);

/* 兼容包装：open + deliver（不在 send 前绑定 call.async_reply_id）。 */
int ep_call_submit(struct endpoint *ep, const ipc_message *msg,
                   uint32_t *reply_id_out);

/*
 * 回复一次 call。`reply_id` 是在 ep_recv 里收到的单次令牌。
 *
 * ep_reply_from() 额外校验 sid 是否等于当初接收该请求的线程——也就是
 * 该 reply 的 owner。U-mode 的 ABI_IPC_REPLY 走这个入口：reply token 是
 * 一个可被猜到的整数值，不是能力，不能仅凭"知道这个数"就允许回复，
 * 否则任意进程都能抢答别人的请求或注入伪造应答。
 *
 * ep_reply() 不做归属校验，仅供内核内部（pager 等自有 reply）使用。
 */
int ep_reply_from(uint8_t sid, uint32_t reply_id, const ipc_message *msg);
int ep_reply(uint32_t reply_id, const ipc_message *msg);

/*
 * 查询某次 in-flight call 的真实发起者 cap_sid。
 * 返回 IPC_OK 并写入 *sid；reply_id 不是存活 reply 对象时返回 IPC_EINVAL。
 *
 * 用途：服务端（SM）需要知道"谁在请求"时只能靠这个，不能靠消息里的字段。
 */
int ipc_reply_caller_sid(uint32_t reply_id, uint8_t *sid);

/*
 * 线程销毁钩子：把该线程从所有 endpoint 队列和 reply 所有者记录里清掉。
 *
 * 阻塞在 ep_send 的线程把消息体存在自己的 TCB 里，接收者会直接读它；
 * 线程被强制终止时必须先撤销这些引用，否则接收者会读到已回收线程的
 * 内存。由 sched 的 thread_destroy 通过弱符号调用。
 */
void ipc_release_thread(struct thread *t);

#endif /* MICRO_KERNEL_IPC_H */
