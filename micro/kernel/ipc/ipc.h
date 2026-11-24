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
    bool used;
};

struct reply {
    struct object obj;
    struct endpoint *ep; /* 所属 endpoint：销毁时据此取消在途 call */
    struct waitq waitq;
    struct thread_msg msg;
    bool done;
    bool aborted;
    bool used;
};

void ipc_init(void);

struct endpoint *ep_create(void);
void ep_ref(struct endpoint *ep);
void ep_unref(struct endpoint *ep);
uint32_t ep_id(const struct endpoint *ep);

int ep_send(struct endpoint *ep, const ipc_message *msg);
int ep_recv(struct endpoint *ep, ipc_message *msg);

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
int ep_reply(uint32_t reply_id, const ipc_message *msg);

#endif /* MICRO_KERNEL_IPC_H */
