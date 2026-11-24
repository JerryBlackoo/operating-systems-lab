# [G4] 同步与互斥（信号量 + sleep/wakeup）

**依赖**：G2（与 G3 可并行，但需 tick 驱动睡眠超时则依赖 G3）  
**建议 Agent**：A3

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 2.3 同步与互斥 | 第 3 章 进程同步 |

## 目标

1. 自旋锁 `spinlock`（关中断或 arch 等价）用于短临界区。
2. `sem_t`：`sem_wait` / `sem_post`；等待队列挂在 PCB/线程上。
3. 内核演示：**生产者-消费者**，缓冲长度 N=8，跑满 1000 次生产消费无丢失。
4. design 文档讨论**丢失 wakeup** 及本实现的避免方式（持锁检查条件）。

## 交付

- `kernel/sync/sem.c`, `mutex.c`（mutex 可为 sem(1)）
- `kernel/tests/prodcons.c` 或 boot 调用的 `sync_selftest()`

## 验收

```bash
make && make run
# 期望：[sync] prodcons ok count=1000
```

## 非目标

- 完整管程 / 条件变量 POSIX 语义
- 用户态 futex

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。