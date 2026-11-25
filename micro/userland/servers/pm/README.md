# pm 用户态进程管理

进程由用户态服务把地址空间、线程和能力空间组合成一条带 PID 的记录。最小模型覆盖父进程启动单线程子进程，再取得它的退出状态。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `struct pm_process` | `pm.h` | 一条进程记录：`pid`、`parent`、`as`/`caps`/`thread` 三个不透明句柄、`state`、`exit_code` |
| `struct pm` | `pm.h` | 进程服务状态：`process_table[]`、`next_pid`、绑定的 `pm_port` |
| `struct pm_port` | `pm.h` | **唯一的依赖方向**：`create_as` / `create_caps` / `create_thread` / `start` / `release(kind)` |
| `enum pm_state` | `pm.h` | `PM_EMPTY` / `PM_LIVE` / `PM_ZOMBIE`（三态） |
| `enum task_resource` | `pm.h` | `TASK_AS` / `TASK_CAPS` / `TASK_THREAD`——回滚时的释放粒度 |
| resident 的 `struct pm_proc` | `user/pm/pm_main.c` | 展示用的进程表项：`pid`、`name[12]`、`state`、`image`（`PM_MAX_PROCS = 16`） |

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `kernel_spawn` | resident 的真实 spawn | `ABI_PM_SPAWN` → `kernel_pm_spawn`（装固定 cap + `loader_spawn_user`）；失败时 `ABI_PM_DISCARD` |
| `loader` | 装载与撤回 | `loader_spawn_user` / `loader_abort_spawn` / `loader_terminate_spawn` |
| `capability` | AS 的授权链 | `ABI_PM_SPAWN words[2]` 带回 PM 侧 `MAP｜GRANT` 源 cap；PM 用 `ABI_VM_GRANT_AS` 让内核 `cap_pm_mint_vm_map` 减权给 VM，再 `ABI_CAP_DELETE` 删源 cap |
| `vm` | 补页授权 | `TAG_VM_FORGET`：子进程被 discard 后通知 VM 释放该 tid 的全部 region（VM 只接受 `USER_SID_PM`） |
| `sm` | 名称服务 | PM 启动时 `TAG_SM_REGISTER` 注册 `"pm"`；spawn 子服务时子服务自己注册 |
| `init` | 唯一调用方 | `TAG_PM_SPAWN` 拉起 fs/console/shell；`TAG_PM_PS` 供 shell 的 `ps` 查询 |
| `shell` | 只读查询 | `TAG_PM_PS`（`words[0]=index`）→ `TAG_PM_PS_OK`（pid + 8B packed name + state + total） |
| `sched` | 线程身份 | `words[1]` 是 kernel thread object id，用于 `ABI_VM_GRANT_AS` 与 `ABI_PM_DISCARD` |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 四槽进程表 + provider 替身 | `pm_init` 清表并绑定 `pm_port`；demo 用三个互不重叠的句柄空间模拟 AS/caps/thread | 先把「记录发布前资源必须全就绪」这条不变量验证清楚，不依赖真实内核 |
| 2. 逆序回滚 | 创建中途失败时按创建顺序逆序 `release(TASK_*)`，PID 不前进 | 半个进程会永久占住 AS 与线程，且 PID 跳号难以排查 |
| 3. 三态与退出码 | `LIVE` → `ZOMBIE` → 空槽；`wait` 取走退出码一次 | 父进程需要能拿到子进程的退出状态，且只能拿一次 |
| 4. resident 换掉替身 | `pm_start(recv_ep, sm_send, sid)`：只拿 recv + SM send + 自己的 SID；进程表手工登记 init/sm/pm | U-mode PM 不能 `#include` 内核头，只能通过 `ABI_PM_SPAWN` 请求内核 |
| 5. 显式 VM 授权（A2） | `ABI_PM_SPAWN` 回 `words[2]` = PM 持有的 AS 源 cap；PM 调 `ABI_VM_GRANT_AS` 让 VM 拿到 `MAP` 权 | 之前内核 `loader` 直接 `cap_root_install` 给 VM，绕过了 PM 的授权链，等于内核替用户做策略 |
| 6. spawn 失败收尾 | grant 失败 → `ABI_CAP_DELETE` 源 cap → `ABI_PM_DISCARD` → 回复 `TAG_PM_ERROR` | 否则留下一个已 spawn 但 VM 无法补页的子进程 |
| 7. 表容量前置检查 | `pm_proc_count >= PM_MAX_PROCS` 时**先**回错误，不调内核 spawn | 反之会拉起一个 PM 记不进去的孤儿服务 |
| 8. pid 在失败之后才消耗（A25） | 内核 `spawn_next_pid++` 移到所有失败分支之后 | 原实现在装 AS cap 之前就吃 pid，abort 后 `ps` 出现跳号 |
| 9. thread id 为空即拒绝（A27） | `words[1] == 0` 时直接回 `TAG_PM_ERROR`，不登记 | A26 加日志时漏了「不登记」这一步，会留下永远无法运行也無法 discard 的幽灵记录 |
| 10. VM 回收通知（A17/A19） | `TAG_VM_FORGET` 用 `call`（不是 `send`），VM 侧校验发送者是 PM | VM 看不到进程生命周期；不通知就会让死掉的进程永久占住 region/驻留页槽位 |

## 最小模型

```text
Process = (pid, parent, address_space, thread, cap_space, state, exit_code)
PM      = (process_table, next_pid)

空槽 ──spawn──> LIVE ──确认退出──> ZOMBIE ──父进程 wait──> 空槽
```

| 操作 | 状态变化 |
| --- | --- |
| `pm_spawn(parent, image, grants)` | 向任务替身申请地址空间、能力空间和暂未运行的初始线程。全部建立后发布 `LIVE` 记录和 PID，再放行线程 |
| `pm_on_exit(thread, code)` | 接收任务替身确认的退出事件，交还执行资源，将退出码保存在 `ZOMBIE` 记录 |
| `pm_wait(parent, pid)` | 核对父子关系。仍在运行时返回 `RUNNING`，已退出时取走退出码并回收记录 |

## 与其他模块的接缝

PM 不实现调度、页表和能力语义，只持有三个不透明句柄：

| 句柄 | 创建 / 释放 | 提供者 |
| --- | --- | --- |
| `as` | `create_as` / `release(TASK_AS)` | `kernel/memory` 的地址空间，或用户态 `vm` server |
| `caps` | `create_caps` / `release(TASK_CAPS)` | `kernel/capability` 能力空间，或 `init` 分配的权限包 |
| `thread` | `create_thread`、`start` / `release(TASK_THREAD)` | `kernel/sched` 线程 |

`struct pm_port` 是唯一依赖方向：PM 不包含上述模块的头文件，也不解释句柄值。换掉任一提供者，`pm.c` 不变。整内核构建把 `pm` 与这些模型链接在同一镜像中：

```sh
make -C micro run BOOT_MODULES=all
make -C micro run BOOT_MODULES='capability objects memory pm sm vm'
```

## 不变量

1. 每个已发布 PID 对应唯一进程记录，退出后保留到父进程回收。模型以递增 PID 区分先后创建的进程。
2. `LIVE` 记录持有三个执行资源的引用。创建中途失败时，按逆序交还本次取得的引用，进程表只发布完整记录。
3. 每份退出状态最多回收一次。父进程身份来自可信的请求上下文，退出事件用线程身份定位记录。
4. `ZOMBIE` 记录不再持有执行资源，只保存退出码；`wait` 成功后槽位归零，可被后续 `spawn` 复用。
5. `start` 只在记录发布后调用，因此子进程第一次运行时，`on_exit` 和父进程 `wait` 已能定位到它。

## 独立验证

`pm_init` 清空进程表并绑定端口，demo 用三个句柄空间互不重叠的提供者模拟地址空间、能力空间和线程：

- 能力空间创建失败：只回滚地址空间；
- 线程创建失败：能力空间、地址空间按创建逆序回滚，PID 不前进；
- 同一父进程启动两个子进程，`start` 中确认记录已处于 `LIVE`；
- 子进程退出后资源计数下降、`ZOMBIE` 句柄清零，退出码在 `wait` 时交付一次；
- 错误父进程返回 `DENIED`，重复 `wait` 返回 `NOT_FOUND`；
- 槽位复用后 PID 继续递增，存活进程不受影响；
- 填满 4 个槽位后的 `spawn` 在触碰资源提供者之前失败；
- PID 空间耗尽与未知线程的退出确认都被拒绝。

## QEMU demo

`pm.c` 实现进程表和三个操作。任务替身记录线程放行时机与资源释放顺序。

```sh
make -C micro/userland/servers/pm run
```

从仓库根目录运行，成功时输出 `[pm] PASS` 并退出 QEMU。

## 已知边界

- `wait` 是非阻塞查询；阻塞父进程需要 `userland/init` 或 IPC 等待队列配合。
- 只跟踪单线程进程；多线程进程的线程组退出归 `kernel/sched` 与更高层策略。
- 没有 `kill`、信号、孤儿重挂和会话/进程组；这些不属于本最小模型。
- resident 进程表固定 16 槽；单模块模型表 4 槽。PID 是 PM 本地合成序号
  （从 10 起），**不是** thread object id；耗尽后 spawn 返回 `NO_SPACE`。
- 内核侧的 pid 计数器在**所有失败步骤之后**才递增（A25）：spawn 在 AS cap
  安装等环节失败时 pid 不被消耗，`ps` 不会跳号。若内核返回的
  `words[1]`（thread id）或 `words[2]`（AS 源 cap）为空，PM 会打印明确
  日志而不是静默跳过 VM grant（A26）。
- 父进程 ID 是可信调用者提供的身份，PM 不校验它是否仍在进程表中。
## M7 resident 进程表

resident PM 不再使用 provider 替身：`pm_start(recv_ep, sm_send, sid, vm_send)`
只拿 recv cap、SM 发送 cap、自己的 SID，以及一个到 VM 的发送句柄
（`SLOT_PM_VM`，用于 `TAG_VM_FORGET`）。进程表手工登记 init/sm/pm 为 1/2/3，
每次 `ABI_PM_SPAWN` 成功后调用 `ABI_VM_GRANT_AS`（`a0`=`words[1]` thread id，
`a1`=`words[2]` PM 持有的 `MAP|GRANT` 源 cap；内核用 `cap_mint_to` 减权给 VM）。
grant 失败时 PM 先 `ABI_CAP_DELETE(words[2])`，再 `ABI_PM_DISCARD(thread_id, image)`，
并 `TAG_VM_FORGET` 通知 VM，然后向 init 回复 `TAG_PM_ERROR`。成功后再把展示
pid 与镜像名登记进表。

| 消息 | 语义 |
| --- | --- |
| `TAG_PM_SPAWN` (0x301) | `words[0]=1, words[1]=parent, words[2]=image, words[3]=reserved` |
| `TAG_PM_SPAWN_OK` (0x302) | `words[0]=pid` |
| `TAG_PM_PS` (0x311) | `words[0]=index` |
| `TAG_PM_PS_OK` (0x312) | `words[0]=pid, words[1]=name(8B packed), words[2]=state, words[3]=total` |
| `TAG_PM_ERROR` (0x313) | `words[0]=status` |

shell 的 `ps` 只持有 PM endpoint cap，按 index 逐条取记录，不直接读内核表。
内核侧 `ABI_PM_SPAWN` 的 a2（旧 `grants`）已保留不用，cap 组合由内核固定。

单模块 `pm` demo 仍用 provider 替身；resident 下 `kernel_pm_spawn` 安装 cap
并 spawn initrd ELF。默认链只允许 PM 拉 image 1–3（fs/console/shell）；
vm/heaptest/heaptest2 由内核 `loader_spawn_user` 拉起，PM 再 spawn 它们会因
SID 预置 slot 冲突（且默认 VM 未向 SM 注册）而失败，不是验收路径。
