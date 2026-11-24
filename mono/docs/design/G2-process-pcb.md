# G2 进程描述表 PCB 与内核线程

- 王道：2.1 进程与线程
- 汤小丹：2.1–2.3 进程描述、状态、控制块

## 实现

- `struct proc`：pid、状态（UNUSED/NEW/READY/RUNNING/SLEEPING/ZOMBIE）、名称、上下文、内核栈顶、页表、堆顶、父进程、文件描述符槽。
- 16 个静态 PCB 和 16 个 16 KiB 静态内核栈，避免要求物理页连续。
- `proc_alloc/proc_free/proc_dump`；`thread_create` 创建内核线程；`thread_exit` 置 ZOMBIE 后切换。
- `current` 全局指针；空闲线程 pid=1，循环 `wfi`。

## 取舍

- 不实现多核 per-CPU runqueue；单核教学模型。
- 内核线程的栈和 PCB 都是静态数组，`proc_free` 只回收槽位，不释放物理页。
- 进程状态命名与教材一致；Linux 的 `task_struct` 只作为对照，不照搬。

## 验收

```bash
make && timeout 10s make run
# [proc] 0 RUN kmain
# [proc] 1 READY idle
# [proc] 3 READY worker-a
# [proc] 4 READY worker-b
```

## 已知限制

- 用户进程的地址空间切换在 G7 完成。
- ZOMBIE 用户进程暂不回收，等待 G7 的父进程语义。
