# init 用户态启动

`init` 用初始能力和一份固定清单启动首批用户程序，并确认每个程序已经就绪。最小模型的输入是预置能力、服务描述和启动器端口，输出是首批服务的启动结果。

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
