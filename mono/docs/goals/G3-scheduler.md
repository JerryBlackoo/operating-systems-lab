# [G3] CPU 时间片调度（RR）

**依赖**：G2  
**建议 Agent**：A2

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 2.2 CPU 调度 | 第 4 章 处理机调度 |

## 目标

1. 就绪队列（单向链表或数组即可）；**时间片轮转 RR**，量子 `TIME_SLICE_TICKS`（默认 10 个时钟 tick，可配置）。
2. 时钟中断末尾调用 `scheduler_tick`；时间片耗尽 `yield`。
3. 上下文切换：`switch.S` 保存 callee-saved + sp/pc。
4. 两个 CPU 密集内核线程：串口打印 `pid=X tick=Y` 便于肉眼看交替。

## 交付

- `kernel/proc/sched.c`, `switch.S`（arch 分文件）
- `docs/design/G3-sched.md`：说明为何不实现 MLFQ/CFS

## 验收

```bash
make && timeout 15s make run
# 期望：两行不同 pid 的日志交错；设计文档含简单甘特草图
```

可选自测：统计 30s 内两线程打印次数比 45%–55%（粗验公平）。

## 非目标

- 优先级实时调度、多 CPU

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。