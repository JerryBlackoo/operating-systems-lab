# ipc：同步 rendezvous、endpoint 与 call/reply

本目录实现微内核最小模型里的「关系层」：Endpoint、固定大小消息、同步交接、
单次 reply。它是四个模块里唯一同时触碰「消息 + 两个线程状态」的原子操作。

其他路径的代码没有修改；本目录可以独立编译和自测。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct endpoint` | `ipc.h` | 一条连接的一端：`state`、`sendq`、`recvq`，以及内核侧排队投递的 `posts[IPC_MAX_POSTS]` 环形队列 |
| `enum ep_state` | `ipc.h` | `EP_IDLE` / `EP_SEND_WAIT` / `EP_RECV_WAIT` / `EP_DYING` |
| `ipc_message`（= `thread_msg`） | `ipc.h` | 固定的 40 字节消息：`tag` + `reply_id` + `words[4]`；**存放在 TCB 里**，不额外分配内核缓冲 |
| `struct reply` | `ipc.h` | 一次 `ep_call` 的单次回复凭证：所属 `ep`、`waitq`、结果消息、`caller_sid`（发起者真实身份）、`owner_sid`（有权回复者）、`done`/`aborted`、`async_caller` |
| `posts[]` 环形队列 | `ipc.h` | 内核侧非阻塞投递；接收者不在时消息落在这里，不丢也不阻塞发送者 |
| 全局池 `endpoints[]` / `replies[]` | `ipc.c` | `IPC_MAX_ENDPOINTS = 16`、`IPC_MAX_REPLIES = 16` 的静态池 |
| `IPC_ASYNC_ABI_*` | `ipc.c` | 异步 syscall 完成时回报给 `user_syscall_complete_async_ipc` 的 ABI 状态常量 |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `objects` | 注册 `OBJ_ENDPOINT` / `OBJ_REPLY` | `ep_create` / `reply_alloc`；`ep_destroy` / `reply_destroy` 是析构回调 |
| `sched` | 阻塞/唤醒的原子基础 | 整个模块的临界区都在 `sched_lock()` 内；用 `thread_block_locked` / `thread_wake_locked` / `waitq_*` |
| `capability` | endpoint 的 cap 类型 | `cap_endpoint.c` 定义 `CAP_TYPE_ENDPOINT`，`finalize` 里 `ep_unref`；`SYSCALL_ENDPOINT` 的 acquire 检查 `READ`/`WRITE` 权限 |
| `syscall/user_backend` | ABI 落地 | `ABI_IPC_SEND`→`ep_send`、`ABI_IPC_CALL`→`ep_call_open`+`ep_call_deliver`（异步）或 `ep_call`（同步）、`ABI_IPC_REPLY`→`ep_reply_from` |
| `syscall`（异步路径） | 早完成与续完 | `ep_reply` 调 `user_syscall_complete_async_ipc`；`syscall_resume` 写回 trap 帧并唤醒 |
| `pager` | 内核侧投递 | `ep_post(vm_ep, TAG_VM_FAULT)` 把缺页请求排队给 U-mode VM，不阻塞缺页线程的对手方 |
| `pm` / `sm` / `fs` / `console` / `vm` | 全部服务通信 | 服务之间的一切调用都走本模块；`TAG_SM_*` / `TAG_PM_*` 等只是 tag 约定 |
| `console`（内核侧） | 行输入唤醒 | `flush_line` 里对 `line_waitq` 做 `thread_wake_locked`（不经过 endpoint） |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 纯 rendezvous | 只有 `ep_send` / `ep_recv`，无 reply | 单模块 demo：发送者阻塞、接收者唤醒、消息内容完整 |
| 2. 单次 reply 令牌 | `ep_call` = send + 等回复；服务端凭 `reply_id` 回一次 | 请求/应答必须能配对，且不能被回复两次 |
| 3. reply 对象池 | `replies[16]`，`reply_alloc` / `reply_destroy`；`reply_id` 是 `OBJ_REPLY` 的 object id | 在途调用需要独立生命周期，不能塞进 endpoint |
| 4. endpoint 销毁时取消在途 call | `ep_destroy` 扫 `replies[]`，标记 `aborted` 并唤醒等待者 | 服务端崩溃时调用者不能永远等下去 |
| 5. 内核侧非阻塞投递 | `ep_post` + `posts[IPC_MAX_POSTS]` | pager 在缺页路径上不能阻塞；VM 恰好不在 `recv` 时消息也不能丢 |
| 6. 身份记录 | `reply.caller_sid` 在 `reply_alloc` 时从 TCB 抄下 | 消息体里没有发送者字段，服务端原先只能相信用户自报的 sid（A3） |
| 7. 回复者归属校验（A10） | `reply.owner_sid` 在消息交付接收者时绑定；`ep_reply_from(sid, ...)` 校验；U-mode 一律走带 sid 的入口 | `reply_id` 只是可猜的整数值，不是能力；不校验时任何进程都能抢答别人的请求、注入伪造应答 |
| 8. 异步 syscall 的早完成缓存 | `async_ipc_done` / `async_ipc_status` / `async_ipc_result` | `ep_reply` 可能早于 `syscall_dispatch` 置 `WAITING`，此时先把结果缓存，park 时再 drain |
| 9. reply 槽位回收（A5） | `ep_reply` / `ep_call_cancel_async` / `ep_destroy` 三条路径都归还 `ep_call_open` 的**创建引用** | 泄漏时 16 次 `IPC_CALL` 后槽位永久耗尽，`help` 之后所有命令静默无输出 |
| 10. 线程销毁清理（A11） | `ipc_release_thread()` 由 `thread_destroy` 弱钩子调用：摘 sendq/recvq、清 TCB 载荷、清 `async_caller` | 阻塞在 `ep_send` 的线程把消息体存在自己 TCB 里，接收者直接读那块内存；线程被强制终止后不清就会读到已回收的内存 |
| 11. 异步 IPC 成为默认 | `ABI_IPC_CALL` 默认走 `ep_call_open`/`deliver` + `syscall_wait_park` + `syscall_resume`，发起线程在 trap 里阻塞 | 让调用者不再占用内核栈同步等待；`make MICRO_SYSCALL_ASYNC_IPC=0` 保留同步回退 |

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
ipc: kernel post queue FIFO/full/wake ok
ipc: receiver blocks, sender delivers ok
ipc: endpoint destroy wakes waiter ok
ipc: endpoint destroy wakes receiver ok
ipc: endpoint slot generation ok
ipc: endpoint pool exhaustion ok
ipc: invalid reply ok
ipc: reply delivery ok
ipc: async reply slot reuse ok
ipc: reply owner check ok
ipc: thread release unlinks queues ok
ipc: all tests passed
```

主机自测和 `sched` 一样使用 fake `cpu_switch`，所以：

- 被阻塞的 `ep_send/ep_recv` 在 `sched()` 返回后会以 `IPC_PENDING` 返回；
  测试随后手动切换到对端，验证状态机和消息内容。
- 主机自测覆盖 `send/recv`、endpoint 销毁、槽位代际、非法 reply、单次 reply 投递、
  异步 call 的 reply 槽位回收、回复者归属校验，以及线程销毁时的队列清理；
  `ep_call` 需要真实栈切换，放在 QEMU demo 中验证。

## 回复者归属（owner）

`reply_id` 是 `ep_call` 签发的单次 bearer token，但它只是一个**整数值**，
不是 capability：任何进程都能构造或猜到一个 id。因此"谁有权回复"不能由
令牌本身决定。

- 消息真正交给接收者时（`ep_send` 直达 / `ep_recv` 从 sendq 取 / `ep_post`
  队列取），内核把接收者的 `cap_sid` 写进 `reply.owner_sid`；
- `ep_reply_from(sid, reply_id, msg)` 校验 `sid` 与 `owner_sid` 一致，
  不一致返回 `IPC_EINVAL` 且**不消耗**单次令牌（真正的 owner 仍可回复）；
- `ABI_IPC_REPLY` 一律走 `ep_reply_from()`，用内核记录的 `request->caller`；
- `ep_reply()` 是 `ep_reply_from(0, ...)`，供内核自有 reply（如 pager）使用，
  不做归属校验。

## 线程销毁与队列

阻塞在 `ep_send` 的线程把消息体存在自己的 TCB（`thread.msg`）里，接收者
直接读这块内存。线程被强制终止时 `thread_destroy` 会调用
`ipc_release_thread()`：

- 把它从所有 `sendq`/`recvq` 摘除（否则队列里留下悬空指针）；
- 清空 TCB 载荷（否则接收者读到已回收线程的垃圾）；
- 清掉 `reply.async_caller` 中指向它的记录。

没有这一步，`ABI_PM_DISCARD` 终止一个正在 `ep_send` 上阻塞的子进程就会
破坏 IPC 队列。`ipc_test` 的 `thread release unlinks queues` 用例覆盖它。

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

- 用户态发送仍是同步 rendezvous；没有用户侧异步缓冲队列。内核生产者
  可用 `ep_post` 的 8 条 posts 队列（pager 缺页通知），不阻塞发送者。
- 不做 capability 转移；`reply_id` 是内核签发的 bearer token，真正的权限控制
  需要 capability 模块（发送者只能回复自己持有的 reply cap）。当前内核只校验
  它确实是一个存活的一次性 reply 对象，不校验调用者身份。
- 无 timeout、无优先级继承；高优先级 client 调低优先级 server 时需要考虑
  priority inheritance 或 seL4 MCS 风格 scheduling context。
- `ep_unref` 只有在最后一个引用释放时才销毁 endpoint；关闭语义需要额外的
  `ep_close`（标记 DYING 并唤醒等待者）。当前销毁会取消在途 `ep_call`，
  但没有“服务端仍可能回复”的软关闭状态。
- 消息固定 4 个 word（32 字节载荷）。大消息的正规做法是共享内存/内存
  capability；当前 resident 没有共享页，只用 24 B 分块绕过。
- endpoint/reply 池是静态上限（各 16 个）；分配与槽位复用由 `sched_lock()`
  保护，但不支持动态扩容。
- resident 下 SM/PM/FS/console/shell/vm 经 capability 句柄 `user_ipc_*` 交换
  4-word 消息；文件内容/输出按 24 B、路径按 16 B 分块，没有共享页，也不扩容消息体。
