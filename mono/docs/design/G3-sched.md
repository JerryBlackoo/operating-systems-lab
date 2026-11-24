# G3 CPU 时间片调度（RR）

- 王道：2.2 CPU 调度
- 汤小丹：第 4 章 处理机调度

## 实现

- 就绪队列用 PCB 数组的轮转扫描，等价于单向环；`last_pick` 记录上次位置。
- `TIME_SLICE_TICKS = 5`，每个 tick 10 ms，量子上限 50 ms；教材默认可设为 10。
- 时钟中断调用 `sched_tick()`；时间片耗尽置 `need_resched`，`trap_dispatch` 在中断返回前调用 `yield()`。
- 上下文切换：RISC-V 保存 `ra/sp/s0–s11`；ARM 保存 `r4–r11/sp/lr`；切换前按进程页表写 `satp`/`TTBR0`。
- 演示：两个 CPU 密集型内核线程打印 `pid=X tick=Y`，可看到交错。

## 甘特草图

```text
worker-a  |####|      |####|      |####|
worker-b  |    |####  |    |####  |    |
         0   50ms  100ms  150ms  200ms
```

## 取舍

- 不实现 MLFQ、CFS、优先级、实时调度、多 CPU。
- RISC-V 支持时钟抢占；ARM 定时器只置 `need_resched`，由长循环/系统调用返回处的 `sched_check()` 让出，不做 IRQ 直接切栈。ARM 演示已能看到两个 pid 交错，但这是安全点协作，不是完整抢占。

## 验收

```bash
make && timeout 15s make run
# [sched] pid=3 tick=0
# [sched] pid=4 tick=0
# [sched] pid=3 tick=1
# ...
```

## 已知限制

- 时间片统计是全局计数，不是每进程独立计数。
- ARM 的完整抢占式用户态留待后续。
