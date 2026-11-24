# G13 init、shell 与综合演示

- 王道：1.2 发展历程（CPU 利用率、多道程序）；2.x 进程并发观感
- 汤小丹：综合

## 实现

- `userland/init`：打印 root FS 就绪和教学用 CPU 利用率对比（单道 ~40%，多道 ~80%），然后 `exec("sh")`。
- `userland/sh`：命令 `help`、`ps`、`cat`、`echo`、`fork_test`、`mkdir`、`exit`。
- `ps` 走 `OS_SYS_PS`，内核 `proc_dump` 打印 PCB 状态。
- `cat /motd` 走 G10 FS 的 `open/read/close`。
- `fork_test` 走 G7 的 `fork`，父子打印不同信息。
- 根 README 更新了已实现/未实现清单。

## 取舍

- 不做 bash 语法、管道、重定向、作业控制、历史记录。
- 利用率是固定教学数字，不是运行时采样；不冒充精确 benchmark。
- `echo hello > /tmp/x` 未实现重定向；只支持输出到 stdout。

## 验收

```bash
make && timeout 20s make run
# [init] root filesystem ready
# [demo] CPU utilization: single-program ~40%, multiprogram ~80%
# sh>

# 交互：
help
cat /motd
```

非交互运行会在 shell 等待输入时被 timeout 结束；demo 已在 exec 前打印。

## 已知限制

- shell 没有行编辑、tab 补全、ctrl-c 信号语义。
- ARM 已能跑 `init → exec sh`、`cat /motd`、`fork_test`；但没有 `sbrk/缺页`，也没有完整 IRQ 抢占。
