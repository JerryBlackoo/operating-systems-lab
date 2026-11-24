# [G2] 进程描述表 PCB 与内核线程

**依赖**：G1  
**建议 Agent**：A2

## 教材对照

| 王道 | 汤小丹 |
|------|--------|
| 2.1 进程与线程 | 2.1–2.3 进程描述、状态、控制块 |

## 目标

1. `struct proc` / `struct thread`：pid、状态（NEW/READY/RUN/SLEEP/ZOMBIE）、内核栈、上下文、所属地址空间指针（可先空）。
2. `proc_alloc` / `proc_free`；`thread_create` 内核线程入口。
3. 全局 `current`；`cpu_idle` 并入 proc 子系统初始化。
4. 调试：`kprintf` 命令或启动时打印 `proc_dump()` 列表。

## 交付

- `kernel/proc/proc.h`, `proc.c`, `thread.c`
- `docs/design/G2-process-pcb.md`：PCB 字段 ↔ 王道图注

## 验收

```bash
make && make run
# 期望：[proc] kmain_thread + kworker_demo (或等价) 状态 READY/RUN
```

至少 **2** 个线程实体；`thread_exit` 后资源可回收（允许简化：仅内核线程）。

## 非目标

- 用户进程 fork（G7）
- 多核 per-CPU runqueue

## 纪律

必读：王道 + 汤小丹对应章节；检索 learn-operate-system / 纸质节号。  
冲突时：以 docs/design/ 裁决为准并引用教材节号。  
完成后：只提交本 Goal 范围 diff，不顺手重构无关模块。