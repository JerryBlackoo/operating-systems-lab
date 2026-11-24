# sm 服务名称注册

服务管理器（service manager，不是 shared memory）维护名称到服务端点的绑定。发布者持有注册项，客户端按名称在自己能力空间内取得发送句柄。

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
