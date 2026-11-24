# pm 用户态进程管理

进程由用户态服务把地址空间、线程和能力空间组合成一条带 PID 的记录。最小模型覆盖父进程启动单线程子进程，再取得它的退出状态。

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

## 已知限制

- `wait` 是非阻塞查询；阻塞父进程需要 `userland/init` 或 IPC 等待队列配合。
- 只跟踪单线程进程；多线程进程的线程组退出归 `kernel/sched` 与更高层策略。
- 没有 `kill`、信号、孤儿重挂和会话/进程组；这些不属于本最小模型。
- 进程表固定 4 槽，PID 单调不复用；耗尽后 `spawn` 返回 `NO_SPACE`。
- 父进程 ID 是可信调用者提供的身份，PM 不校验它是否仍在进程表中。
