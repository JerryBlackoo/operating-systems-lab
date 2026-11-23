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
 */

#define IPC_MAX_ENDPOINTS 16
#define IPC_MAX_REPLIES 16
#define IPC_MSG_WORDS THREAD_MSG_WORDS

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

struct endpoint {
    struct object obj;
    enum ep_state state;
    struct waitq sendq;
    struct waitq recvq;
    bool used;
};

struct reply {
    struct object obj;
    struct waitq waitq;
    struct thread_msg msg;
    bool done;
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
 */
int ep_call(struct endpoint *ep, const ipc_message *msg, ipc_message *reply);
int ep_reply(uint32_t reply_id, const ipc_message *msg);

#endif /* MICRO_KERNEL_IPC_H */
