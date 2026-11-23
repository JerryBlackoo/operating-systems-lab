# ipc：同步 rendezvous、endpoint 与 call/reply

本目录实现微内核最小模型里的“关系层”：Endpoint、固定大小消息、同步交接、
单次 reply。它是四个模块里唯一同时触碰“消息 + 两个线程状态”的原子操作。

其他路径的代码没有修改；本目录可以独立编译和自测。

## 最小模型

```text
Endpoint = (state, sendq, recvq)
Message  = (tag, reply_id, words[4])

IDLE --send, 无接收者--> SEND_WAIT --recv--> IDLE（唤醒发送者）
IDLE --recv, 无发送者--> RECV_WAIT --send--> IDLE（唤醒接收者）
```

rendezvous endpoint 的关键不变量：**sendq 和 recvq 至多一个非空**。
发送者遇到等待的接收者就立即移交；否则自己入 sendq 阻塞。接收对称。

消息寄存器放在 TCB 的 `struct thread_msg` 里，不分配内核消息缓冲：

```c
struct thread_msg {
    uint32_t tag;
    uint32_t reply_id;
    uint64_t words[4];
};
```

## 不变量

1. 消息恰好一次：不丢、不重、不截断（固定 4 个 word）。
2. 一个线程最多阻塞在一个 endpoint/等待队列上；队列里没有重复项。
3. 消息拷贝与双方状态迁移在 `sched_lock()` 内原子完成；内核不保留用户指针。
4. endpoint 销毁必须唤醒所有等待者，并让 `ep_send/ep_recv` 返回 `IPC_EDEAD`。
5. reply cap 单次使用：`ep_reply` 第二次返回 `IPC_EBUSY`。
6. `ep_send/ep_recv` 在 endpoint 已 `EP_DYING` 时立即返回 `IPC_EDEAD`。

## 代码

| 文件 | 内容 |
|---|---|
| `ipc.h` | Endpoint/Reply/消息/状态码/API |
| `ipc.c` | rendezvous 状态机、endpoint 池、reply 池、call/reply |
| `ipc_test.c` | 双向 rendezvous、endpoint 销毁唤醒、非法 reply 自测 |
| `Makefile` | 主机自测构建 |

## API

```c
void ipc_init(void);

struct endpoint *ep_create(void);
void ep_ref(struct endpoint *ep);
void ep_unref(struct endpoint *ep);
uint32_t ep_id(const struct endpoint *ep);

int ep_send(struct endpoint *ep, const ipc_message *msg);
int ep_recv(struct endpoint *ep, ipc_message *msg);

int ep_call(struct endpoint *ep, const ipc_message *msg, ipc_message *reply);
int ep_reply(uint32_t reply_id, const ipc_message *msg);
```

`ep_call` 是 `send + 等待 reply`：

1. 分配一个单次 reply 对象；
2. 把 `reply_id` 写进请求消息；
3. `ep_send` 阻塞到服务端接收；
4. 在 reply 的等待队列上阻塞；
5. 服务端调用 `ep_reply(reply_id, ...)` 唤醒调用者。

reply 对象由内核分配，调用者持有一个引用，`ep_call` 返回后释放。

调用者必须在 `ep_send/ep_recv/ep_call` 期间持有 endpoint 引用（capability 或
显式 `ep_ref`）。当前实现不在阻塞路径里临时加引用；这是 capability 模块接入前
的约定。`ep_unref` 释放最后一个引用时才会销毁 endpoint 并唤醒等待者。

## 构建与自测

```sh
make -C micro/kernel/ipc test
```

预期输出：

```text
ipc: sender blocks, receiver receives ok
ipc: receiver blocks, sender delivers ok
ipc: endpoint destroy wakes waiter ok
ipc: invalid reply ok
ipc: reply delivery ok
ipc: all tests passed
```

主机自测和 `sched` 一样使用 fake `cpu_switch`，所以：

- 被阻塞的 `ep_send/ep_recv` 在 `sched()` 返回后会以 `IPC_PENDING` 返回；
  测试随后手动切换到对端，验证状态机和消息内容。
- 主机自测覆盖 `send/recv`、endpoint 销毁、非法 reply 和单次 reply 投递。

## QEMU 接线

```sh
make -C micro run BOOT_MODULES=ipc
```

自举 demo 用两条内核线程验证 `ep_call` 阻塞到 `ep_reply`、回复载荷及重复回复拒绝。

```c
object_system_init();
sched_init();
ipc_init();

struct endpoint *ep = ep_create();

/* 服务端线程 */
for (;;) {
    ipc_message msg;
    if (ep_recv(ep, &msg) != IPC_OK) break;
    ipc_message reply = make_reply(&msg);
    ep_reply(msg.reply_id, &reply);
}

/* 客户端线程 */
ipc_message request = ...;
ipc_message reply;
ep_call(ep, &request, &reply);
```

## 已知边界

- 只有同步 rendezvous；没有异步缓冲队列、没有消息大小上限策略。
- 不做 capability 转移；`reply_id` 是内核签发的 bearer token，真正的权限控制
  需要 capability 模块（发送者只能回复自己持有的 reply cap）。
- 无 timeout、无优先级继承；高优先级 client 调低优先级 server 时需要考虑
  priority inheritance 或 seL4 MCS 风格 scheduling context。
- `ep_unref` 只有在最后一个引用释放时才销毁 endpoint；关闭语义需要额外的
  `ep_close`（标记 DYING 并唤醒等待者）。
- 消息固定 4 个 word；大消息应走共享内存/内存 capability，而不是扩大 TCB。
