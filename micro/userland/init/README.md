# init 用户态启动

`init` 用初始能力和一份固定清单启动首批用户程序，并确认每个程序已经就绪。最小模型的输入是预置能力、服务描述和启动器端口，输出是首批服务的启动结果。

resident 下它是**唯一**经 PM 拉起服务的客户端，也是「服务全部就绪」的唯一汇总点。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct init_service` | `init.h` | 清单的一项：`id`（服务标识）、`grants`（本项需要的能力子集） |
| `struct init_child` | `init.h` | 启动结果：`handle` 与 `ready_receipt` |
| `struct init_model` | `init.h` | 启动状态机：`phase`、`port`、`plan[INIT_CAPACITY]`、`started[INIT_CAPACITY]`、`count`/`started_count`、`failed_service`、`error` |
| `struct init_port` | `init.h` | **唯一的依赖方向**：`start(service, child)` / `stop(handle)` |
| `enum init_phase` | `init.h` | `INIT_BOOT` / `INIT_STARTING` / `INIT_READY` / `INIT_FAILED` |
| resident 的 `ready_mask` / `spawned_mask` | `user/init_main.c` | 就绪位与已 spawn 位；**按 image 号记位**（`receipt = 0x2000 + image`） |
| resident 的 `phases[][3]` | `user/init_main.c` | 分两阶段 spawn 的清单：`{fs, console}` 然后 `{shell}` |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `pm` | 唯一的 spawn 通道 | `TAG_PM_SPAWN`（`words[1]=parent, words[2]=image`）→ `TAG_PM_SPAWN_OK(pid)` 或 `TAG_PM_ERROR(status)` |
| `sm` | 服务发现 | `TAG_SM_LOOKUP` 拿 PM 的发送句柄 |
| `capability` | 单模块 demo 的初始能力 | demo 用 `capability` 从根能力按子集派生 `grants`，验证「清单 grants 必须是 boot_caps 的子集」 |
| `fs` / `console` / `shell` | 被启动的服务 | 每个服务启动后向 `ready_ep` 发 `TAG_READY`（`words[0]=receipt`） |
| `kernel/boot` | 启动来源 | `kernel_init` 用 `loader_spawn_user` 拉起 init，`layout.init.a3 = 3`（plan 数量） |
| `docs/design/M7-integration.md` | 启动顺序的权威说明 | 阶段划分与「为什么 shell 必须最后」 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 纯模型（启动器替身） | `init_port.start/stop` 由替身实现；覆盖错序/重复确认、启动失败、提前退出、空清单、非法清单 | 先把「清单校验必须全通过才开始」「就绪确认只认当前等待项」这两条语义做对 |
| 2. 失败逆序清理 | 进入 `FAILED` 前按启动的逆序 `stop`，避免回调重入 | 半个启动序列会留下无法回收的资源 |
| 3. 系统用例 | demo 第二层用真实 `capability`/`pm`/`sm` 跑 `sm → pm → vm → vfs`，注入第三项失败 | 验证跨模块回滚：进程回收、能力引用归零、注册表归零 |
| 4. resident 换掉替身 | `init_main(recv_only, sm_send, ready_ep, plan, sid)`：lookup PM，然后逐个 `TAG_PM_SPAWN` | U-mode init 不能 `#include` 内核头，只能用 IPC |
| 5. 就绪汇总 | `ready_mask` 覆盖全部 plan 项后打印 `[init] ready` | 需要一个「所有服务都已就位」的观察点 |
| 6. **分阶段 spawn（A31）** | `phases[][]`：先 `{fs, console}` 并等两者 ready，再 `{shell}` | shell 一启动就打印 `micro>` 并响应命令；若与 fs 的自检并行，fs 的 24 B 分块日志会插进 help 的整行中间，把 `commands: help cat ps echo poweroff` 拆成两段。表现为 `make check`/stress **间歇性失败**（实测约 1/8 次） |
| 7. 位序按 image 号 | `ready_mask |= 1u << image`（不是 `<< spawn 序号`） | 阶段 0 的 fs/console 是 image 1/2，对应 bit1/bit2；用序号会在第二阶段把位序算错 |

## 最小模型

```text
Init = (boot_caps, plan, started, waiting_for, phase)

BOOT ──校验清单与初始能力──> STARTING ──全部确认就绪──> READY
  │                            │
  └──校验失败──> FAILED         └──启动失败 / 提前退出──> 清理 ──> FAILED
```

## 校验与端口契约

启动前一次性校验整份清单，任一项失败都不启动任何服务：

1. 清单不超过 `INIT_CAPACITY`，非空清单必须给出数组；
2. `id != 0`，且同一清单内无重复；
3. 每项 `grants` 是 `boot_caps` 的子集；
4. `port->start` 与 `port->stop` 成对可用。

`start(service, child)` 成功时返回本项专用的非零 `handle` 和 `ready_receipt`，且两者在已启动项中唯一；违反契约按 `INIT_BAD_START` 处理。`ready(receipt)` 只接受当前等待项的凭据，错序、重复或零凭据都返回 false。`fail()` 先进入 `FAILED`，再按启动的逆序调用 `stop`，避免回调重入本模型。

## 不变量

1. 清单按给定顺序推进，每项最多启动一次。空清单直接进入 `READY`。
2. 只有当前等待项的就绪确认能推进清单，重复确认只处理一次。
3. `READY` 表示清单中每项都已确认就绪。进入 `FAILED` 时，本轮取得的服务资源已经交还。
4. 无效启动记录（零句柄、零凭据、重复身份）不会进入清单；身份不明的句柄不会被 `stop`，由端口自行回收。

## 集成验证

demo 分两层：

- 启动器替身覆盖错序/重复确认、启动失败、提前退出、空清单、非法清单和无效启动记录；
- 系统用例用真实模块跑固定清单 `sm → pm → vm → vfs`：能力由 `capability` 模块从初始根能力按子集派生，进程由 `pm` 模块事务式创建，名称由 `sm` 模块注册；注入第三项启动失败，核对逆序清理、进程回收、能力引用与注册表归零。

独立镜像和全量内核都未开启 MMU，因此 demo 在 S-mode 运行；`init` 的模型边界仍按用户态首进程划分，真正的进程创建始终由 `init_port` 外置。

## QEMU demo

```sh
make -C micro/userland/init run
```

从仓库根目录运行，成功时输出 `[init] PASS` 并退出 QEMU。全模块内核：

```sh
make -C micro run BOOT_MODULES=all
```

`init` 的输出应为：

```text
[init] start
startup: validated plans, ordered readiness and PM/SM/capability cleanup
[init] PASS
```
