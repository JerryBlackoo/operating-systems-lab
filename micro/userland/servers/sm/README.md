# sm 服务名称注册

服务管理器（service manager，不是 shared memory）维护名称到服务端点的绑定。发布者持有注册项，客户端按名称在自己能力空间内取得发送句柄。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct sm` | `sm.h` | 名称表：`bindings[]`（固定容量）、绑定的 `sm_port`、登记计数 |
| `struct sm_binding` | `sm.h` | 一条注册项：`name`（8 字节打包）、`owner`、端点引用、`state` |
| `struct sm_port` | `sm.h` | 唯一的依赖方向：`retain(endpoint)` / `grant_send(client, endpoint, &handle)` / `release(...)` |
| `enum sm_state` | `sm.h` | `SM_UNREGISTERED` / `SM_REGISTERED` |
| `enum sm_result` | `sm.h` | `SM_OK` / `SM_EXISTS` / `SM_NOT_FOUND` / `SM_DENIED` / `SM_NO_SPACE` 等 |
| resident 的 `struct sm_entry` | `user/sm_main.c` | 一张静态表：`name`、`sid`、`source`（该服务的 WRITE|GRANT 源 cap 槽）、`live` |

resident 下名字表只有 4 项（`pm` / `fs` / `console` / `vm`），**不是动态分配**。

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `capability` | 句柄发放与身份 | `ABI_CAP_MINT_TO` → `kernel_cap_mint_to` → `cap_mint_to`；`ABI_REPLY_CALLER_SID` → `ipc_reply_caller_sid` 取真实 `cap_sid` |
| `ipc` | 端点与身份来源 | 注册用 `TAG_SM_REGISTER`（`call`），查询用 `TAG_SM_LOOKUP`；`reply.caller_sid` 是唯一可信的身份 |
| `boot/service_boot` | 预置源 cap | SM 的 CSpace slot 2/3/4/5 预置 PM/FS/console/VM 的 `WRITE｜GRANT` 源 cap |
| `pm` / `fs` / `console` / `vm` | 被注册的服务 | 各自启动后先 `TAG_SM_REGISTER`（名字 + sid），再向 init 回执 `TAG_READY` |
| `init` / `shell` / `heaptest` | 客户端 | `TAG_SM_LOOKUP` 取目标服务的发送句柄；shell 在启动时 lookup `console`/`fs`/`pm` |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 纯名称表模型 | `sm.c` + `sm_port` 替身：两个发布端点、三个客户端能力表 | 先验证「同名唯一、注销后已发句柄仍有效」这类语义 |
| 2. 注册项引用与客户端引用分开计数 | `retain` 记注册项，`grant_send` 记客户端 | 名称删除不应让已发放的句柄立即失效 |
| 3. 失败不留半个注册项 | 注册/查找/注销都原子；发放失败时注册项保留原绑定 | 客户端槽位耗尽不该毁掉已有服务名 |
| 4. resident 静态表 | 4 项静态 `sm_entries[]`，`source` 指向预置 cap 槽 | U-mode SM 无法动态分配名字；4 个服务是固定的 |
| 5. **身份改由内核提供（A3）** | `ABI_REPLY_CALLER_SID`：用 `reply_id` 向内核查真实 `cap_sid`；`register`/`lookup` 都比对 `real_sid` 与 `claimed_sid` | 原实现相信消息里的 `words[2]` 自称 sid，任何 U-mode 进程都能冒充别的服务注册名字，或让 SM 把 cap 塞进受害者的 CSpace |
| 6. 拒绝 SID 越界 | `sid == 0 \|\| sid >= USER_SID_MAX` 直接 `DENIED` | 0 是「无身份」，越界会让 `cap_mint_to` 写到 CSpace 数组之外 |
| 7. SID 定义统一（A22） | `USER_SID_*` 与 `TAG_*` 由 `shared/abi/service.h` 单一提供 | 历史上 `service_proto.h` 有一份独立副本，新增 tag 漏改一处就会让 U-mode 发的消息内核不认识 |

## 最小模型

```text
Registry : Name ⇀ (owner, endpoint_ref)

未注册 ──register──> 已注册 ──owner unregister──> 未注册
                       └──lookup──> 发放发送句柄

发布者退出 ──owner_exit──> 回收该 owner 的全部注册项
```

名称是长度有界（1..`SM_NAME_MAX`）、区分大小写、非空的字节串。注册表有固定容量；端点引用和句柄如何表示、权限如何检查，由 `sm_port` 后面的能力模块决定。

| 操作 | 契约 |
| --- | --- |
| `register(owner, name, endpoint)` | 检查名称和端点，为有效的新名称保存端点引用 |
| `lookup(client, name)` | 查到名称后，请能力层向调用者发放仅具发送权限的句柄 |
| `unregister(owner, name)` | 核对注册者身份，删除名称并释放注册项持有的引用 |
| `owner_exit(owner)` | 发布者退出时回收其全部注册项，返回回收数量 |

## 不变量

1. 同名最多一条记录。重复注册返回 `EXISTS`；名称逐字节比较，`fs` 与 `FS`、`fs` 与 `fsx` 都是不同名称。
2. 注册项持有的引用和发给客户端的引用分别计数。注销或 `owner_exit` 只释放注册项的引用；已发放的句柄由持有者和能力表管理，寿命可以超过名称。
3. 句柄按接受方能力空间发放，注册者身份取自请求上下文。发放失败（越权、客户端槽位耗尽）时返回错误，注册项保留原绑定，调用者的输出句柄不被改写。
4. 注册、查找、注销都是原子的：失败不会留下半个注册项，也不会多持有一个端点引用。

## 与其他模块的衔接

- [capability](../../../kernel/capability/README.md)：`sm_port.retain` / `grant_send` / `release` 的实现方。sm 只维护名称与登记状态，不实现权限检查。
- [pm](../pm/README.md)：进程退出路径调用 `sm_owner_exit(sm, pid)`，避免发布者死亡后名称永久占位。
- [ipc](../../../kernel/ipc/README.md)：端点通常就是 IPC endpoint；`lookup` 发放的是只带发送权限的句柄。

## 独立验证

用两个发布端点、三个客户端能力表验证：

- 非空、长度上限、大小写敏感和前缀区分；
- 非 owner 注册/注销、重复注册、能力槽位耗尽；
- 注销后名称不可查找，但客户端已持有的句柄仍可发送；
- 名称表满、发布者退出和端点引用计数归零；
- 槽位复用后旧句柄失效。

## QEMU demo

`sm.c` 实现名称表。`demo.c` 的能力替身用 `(slot, generation)` 句柄模拟三个客户端各自的能力空间，并分别追踪注册项引用和客户端句柄引用。

```sh
make -C micro/userland/servers/sm run
```

从仓库根目录运行，成功时输出 `[sm] PASS` 并退出 QEMU。也可以把模块放进内核自举清单：

```sh
make -C micro run BOOT_MODULES=sm
```
## M7 resident register / lookup

- `service_boot` 把 PM/FS/console/VM 的 `WRITE|GRANT` 源 cap 预置在 SM 自己的
  CSpace（slot 2/3/4/5），不把裸 endpoint 指针交给客户端。
- FS/console/VM 启动后先发 `TAG_SM_REGISTER`（名字在 `words[1]`，SID 在
  `words[2]`），再向 init 回执 `TAG_READY`。
- init 用 `TAG_SM_LOOKUP`（名字在 `words[1]`，目标 SID 在 `words[2]`）取得
  `TAG_SM_OK` + 真实 cap handle；SM 内部用 `ABI_CAP_MINT_TO` 派生
  `SVC_CAP_WRITE`。
- 名字表只有 4 项（pm/fs/console/vm）；未注册返回 `ABI_NOT_FOUND`，派生失败
  返回 `ABI_DENIED`。

## 已知边界

- 单模块 demo 用能力替身表；resident 用 `ABI_CAP_MINT_TO` 真派生。
- 名字表里有 `vm` 条目，但**默认链不注册**：内核 spawn 的 VM 没有 SM 发送 cap，
  heaptest 直接用预置 VM cap。只有 PM spawn 的 VM（实验路径）才 register。
- 不做动态卸载、权限策略语言或跨机服务发现。
- **身份已由内核提供（A3，2026-11-25 修复）**：`TAG_SM_REGISTER` / `TAG_SM_LOOKUP`
  都先调 `user_reply_caller_sid(msg.reply, &real_sid)`，用内核在 `ep_call` 时
  从 TCB 抄下的 `cap_sid` 作为唯一可信身份，并与消息里 `words[2]` 的自称值
  比对；不一致直接 `DENIED`。因此伪造 sid 无法冒充别的服务注册名字，也无法
  诱使 SM 把 cap 派生进受害者的 CSpace。
- **仍未做**：SM 不做名称访问控制——任何持有 SM send cap 的进程都能 lookup
  任何已注册名字。当前 resident 服务全是受信代码，所以这属于已知边界，不是
  对敌对服务的隔离承诺。要做隔离需要给每个名字加 ACL 或让 SM 检查请求者的
  能力集合。
