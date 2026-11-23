# pm 用户态进程管理

进程由用户态服务把地址空间、线程和能力空间组合成一条带 PID 的记录。最小模型覆盖一个父进程启动一个单线程子进程，再取得它的退出状态。

## 最小模型

```text
Process = (pid, parent, address_space, thread, cap_space, state, exit_code)
PM      = (process_table, next_pid)

空槽 ──spawn──> LIVE ──确认退出──> ZOMBIE ──父进程 wait──> 空槽
```

| 操作 | 状态变化 |
| --- | --- |
| `spawn(parent, image, grants)` | 向任务替身申请地址空间、能力空间和暂未运行的初始线程。全部建立后发布 `LIVE` 记录和 PID，再放行线程 |
| `on_exit(thread, code)` | 接收任务替身确认的退出事件，交还执行资源，将退出码保存在 `ZOMBIE` 记录 |
| `wait(parent, pid)` | 核对父子关系。仍在运行时返回 `RUNNING`，已退出时取走退出码并回收记录 |

## 不变量

1. 每个已发布 PID 对应唯一进程记录，退出后保留到父进程回收。模型以递增 PID 区分先后创建的进程。
2. `LIVE` 记录持有三个执行资源的引用。创建中途失败时，按逆序交还本次取得的引用，进程表只发布完整记录。
3. 每份退出状态最多回收一次。父进程身份来自可信的请求上下文，退出事件用线程身份定位记录。

## 独立验证

用任务替身生成三种资源，走一遍 `spawn → exit → wait`。分别注入线程创建失败、错误父进程和重复 `wait`，核对退出码交付次数及资源计数。

## QEMU demo

`pm.c` 实现进程表和三个操作。任务替身记录线程放行时机与资源释放顺序。

```sh
make -C micro/userland/servers/pm run
```

从仓库根目录运行，成功时输出 `[pm] PASS` 并退出 QEMU。
