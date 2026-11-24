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

布局固定为 40 字节且无填充，其中 `words[4]` 是固定的 32 字节载荷。
`tag` 由发送者定义，`reply_id` 只在请求中由内核填写。

## 不变量

1. 消息恰好一次：不丢、不重、不截断（固定 4 个 word）。
2. 一个线程最多阻塞在一个 endpoint/等待队列上；队列里没有重复项。
3. 消息拷贝与双方状态迁移在 `sched_lock()` 内原子完成；内核不保留用户指针。
4. endpoint 销毁必须唤醒所有等待者：sendq/recvq 的线程返回 `IPC_EDEAD`；
   已交付但尚未回复的 `ep_call` 也会被标记取消并唤醒，不会永久等待。
5. reply cap 单次使用：`ep_reply` 第二次返回 `IPC_EBUSY`；endpoint 销毁后，
   对仍然存活的 reply 调用 `ep_reply` 返回 `IPC_EDEAD`。
6. `ep_send/ep_recv` 在 endpoint 已 `EP_DYING` 时立即返回 `IPC_EDEAD`。
7. `reply_id` 必须是内核签发的存活 `OBJ_REPLY` 对象；拿 endpoint id 或
   任意猜测值调用 `ep_reply` 返回 `IPC_EINVAL`。

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

1. 分配一个单次 reply 对象，并记录它所属的 endpoint；
2. 把内核签发的 `reply_id` 写进请求消息；
3. `ep_send` 阻塞到服务端接收；
4. 在 reply 的等待队列上阻塞；
5. 服务端调用 `ep_reply(reply_id, ...)` 唤醒调用者；
6. endpoint 在等待期间被销毁时，调用者被取消并返回 `IPC_EDEAD`。

reply 对象由内核分配，调用者持有一个引用，`ep_call` 返回后释放。
`ep_reply` 在同一个临界区内完成回复内容拷贝、单次标记和调用者唤醒；
若调用者尚未进入等待循环，它会在下一次检查中看到 `done`，没有丢失唤醒窗口。

调用者必须在 `ep_send/ep_recv/ep_call` 期间持有 endpoint 引用（capability 或
显式 `ep_ref`）。当前实现不在阻塞路径里临时加引用；这是 capability 模块接入前
的约定。`ep_unref` 释放最后一个引用时才会销毁 endpoint 并唤醒等待者。

有一条例外路径需要内核防御：endpoint 的所有者（例如撤销最后一个 capability 的
线程）可能在别的线程借用指针阻塞时销毁 endpoint。此时无法保证调用者手上有引用，
所以销毁路径必须主动取消 sendq/recvq 和该 endpoint 的在途 reply，而不能假设
“还有人持有引用，不会销毁”。demo 的第 2、3 个场景就是这条路径。

## 构建与自测

```sh
make -C micro/kernel/ipc test
```

预期输出：

```text
ipc: message layout ok
ipc: argument validation ok
ipc: sender blocks, receiver receives ok
ipc: receiver blocks, sender delivers ok
ipc: endpoint destroy wakes waiter ok
ipc: endpoint destroy wakes receiver ok
ipc: endpoint slot generation ok
ipc: endpoint pool exhaustion ok
ipc: invalid reply ok
ipc: reply delivery ok
ipc: all tests passed
```

主机自测和 `sched` 一样使用 fake `cpu_switch`，所以：

- 被阻塞的 `ep_send/ep_recv` 在 `sched()` 返回后会以 `IPC_PENDING` 返回；
  测试随后手动切换到对端，验证状态机和消息内容。
- 主机自测覆盖 `send/recv`、endpoint 销毁、槽位代际、非法 reply 和单次 reply 投递；
  `ep_call` 需要真实栈切换，放在 QEMU demo 中验证。

## QEMU 接线

```sh
make -C micro run BOOT_MODULES=ipc
```

自举 demo 用内核线程验证三条路径：

1. `ep_call` 阻塞到 `ep_reply`、回复载荷完整、重复回复被拒绝；
2. endpoint 销毁唤醒阻塞在 `ep_recv` 的线程，返回 `IPC_EDEAD`；
3. 请求已交付但服务端未回复时销毁 endpoint，正在等待回复的 `ep_call`
   被取消并返回 `IPC_EDEAD`。

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
  需要 capability 模块（发送者只能回复自己持有的 reply cap）。当前内核只校验
  它确实是一个存活的一次性 reply 对象，不校验调用者身份。
- 无 timeout、无优先级继承；高优先级 client 调低优先级 server 时需要考虑
  priority inheritance 或 seL4 MCS 风格 scheduling context。
- `ep_unref` 只有在最后一个引用释放时才销毁 endpoint；关闭语义需要额外的
  `ep_close`（标记 DYING 并唤醒等待者）。当前销毁会取消在途 `ep_call`，
  但没有“服务端仍可能回复”的软关闭状态。
- 消息固定 4 个 word（32 字节载荷）；大消息应走共享内存/内存 capability，
  而不是扩大 TCB。
- endpoint/reply 池是静态上限（各 16 个）；分配与槽位复用由 `sched_lock()`
  保护，但不支持动态扩容。
