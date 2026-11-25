# clock：单调时基与定时器

本目录只回答两个问题：

1. **现在是什么时候？** —— 一个单调、无锁的 `clock_now()`，单位 ns。
2. **到某个时刻，怎么得到一个回调？** —— 绝对 deadline + 一个 per-hart 的单槽比较器。

墙钟、RTC、时区、NTP、POSIX timer、CPU 计费、调度策略都不在这里。

## 本模块的对象

| 对象 | 定义处 | 作用 |
| --- | --- | --- |
| `clock_ns_t` | `clock.h` | 单调纳秒时间轴（自 `clock_init()` 起）；`clock_deadline_after(base, delta)` 做溢出饱和 |
| `clock_ticks_t` | `clock.h` | 硬件 timebase 计数（QEMU virt 为 10 MHz） |
| `struct clock_timer` | `clock.h` | 一个绝对 deadline 的定时器：`next`（队列链）、`deadline`、`callback`、`arg`、`seq`（插入序号，用于同 deadline 的 FIFO 稳定性）、`state` |
| `enum clock_timer_state` | `clock.h` | `INACTIVE` / `ENQUEUED` / `RUNNING` |
| `struct clock_hart` | `timer.c` | per-hart 队列状态：`head`（按 deadline 升序）、`running`（正在执行回调的定时器）、`programmed`（已写入比较器的值） |

`struct clock_timer` 是**侵入式**的：调用者把定时器嵌在自己的结构里（例如
`struct pager_pending.watchdog`），模块只保存指针。因此调用者必须保证
「cancel 返回 true 或回调返回」之前不释放该对象。

## 与其他模块的关联

| 对方模块 | 关系 | 具体接口 |
| --- | --- | --- |
| `sched` | 唯一的时间片来源 | `sched_start_tick()` 武装 one-shot 定时器；`clock_handle_interrupt()` 里的回调调 `sched_tick()`，trap 返回前 `sched_check()` 完成切换 |
| `pager` | 缺页超时看门狗 | `pending_add()` 里 `clock_timer_init` + `clock_timer_arm(PAGER_TIMEOUT_NS)`；`pending_complete_locked` / `pending_remove_tid` 里 `clock_timer_cancel` |
| `arch/riscv`（`plic.c`） | 定时器中断的硬件适配 | `port_complete(IRQ_TIMER)` 与 `port_mask` / `port_unmask` 都通过 `sbi_set_timer` 重新编程比较器 |
| `shared/demo.c`（trap glue） | 中断入口 | S 态 timer 中断里先 `clock_handle_interrupt()`，再 `sched_check()` |
| `thread_sleep_ns` / `thread_sleep_until` | 由本模块驱动 | `sched` 提供的睡眠原语把线程挂在内部 waitq 上，由 clock 的 one-shot deadline 唤醒 |

## 演进历程

| 阶段 | 状态 | 驱动它的需求 |
| --- | --- | --- |
| 1. 只用 `rdtime` | 直接读计数器，自己换算 | 单模块 demo 需要一个单调时间 |
| 2. epoch + 频率换算 | `timebase.c`：boot 时记 `epoch`，`clock_now()` 返回 ns | 单位泄漏给所有调用者；每个用户自己除以频率容易写错 |
| 3. 单槽比较器 | 只有一个 `sbi_set_timer` 槽 | 硬件只有一个 mtimecmp，必须有人把「多个 deadline」排序后复用这一个槽 |
| 4. 有序队列 | `q_insert` 按 deadline 升序插入，同 deadline 按 `seq` FIFO | 多个等待者必须能共存；同刻到期的回调需要确定的顺序 |
| 5. 回调内禁止释放自己 | `RUNNING` 状态；只有当前回调可以 re-arm 自己 | 回调执行期间对象生命周期归 clock 管；提前释放会破坏队列链接 |
| 6. 先撤防再派发 | `clock_handle_interrupt` 先写 `CLOCK_DISARM` | QEMU 在 `mtimecmp <= mtime` 时会立刻再次拉高中断，不撤防就形成中断风暴 |
| 7. pager 超时接入 | `pager` 用 `clock_timer` 做 200 ms 看门狗 | VM 服务卡住时缺页线程必须能被唤醒，不能永远阻塞 |

## 文件

| 文件 | 职责 | 对应模型 |
| --- | --- | --- |
| `clock.h` | 公共 API、`struct clock_timer`、定时器状态 | 对外契约 |
| `timebase.c` | boot 基准、`ticks ↔ ns`、`clock_now()` | `C + φ` |
| `timer.c` | 有序队列、arm / cancel、IRQ 派发 | `Q + D + dispatch` |
| `event.c` | `rdtime`、SBI `set_timer`、`sstatus.SIE` 保存/恢复 | 硬件适配 |
| `platform.h` | `timebase.c` / `timer.c` 与平台层的内部契约 | — |
| `Makefile` | 只做 RISC-V 交叉编译检查 | — |

## 五个逻辑切面

| 切面 | 回答的问题 | 模块内状态 | 去掉它会怎样 | 参考 |
| --- | --- | --- | --- | --- |
| **S1 计数面** | 时间在走吗？ | 只读、非降的计数器 `C` | 只能忙等或数中断 | Linux `clocksource`；QEMU `rdtime` |
| **S2 换算面** | 走了多少 ns？ | epoch + 频率 | 单位泄漏给所有调用者 | Linux `tk_read_base` |
| **S3 触发面** | 到点怎么打断？ | 单槽比较器 `D` | 睡眠中无法按时回来 | Linux `clock_event_device`；seL4 MCS `setDeadline` |
| **S4 队列面** | 多个 deadline 怎么复用 `D`？ | 按 deadline 排序的 `Q` | 一个等待者独占硬件 | Linux hrtimer rbtree / timer wheel；Fuchsia timer queue |
| **S5 语义面** | 哪个时间轴？谁能改？ | 偏移、权限、周期/一次性 | 墙钟/权限/周期语义混进内核 | POSIX clocks；MINIX clock task |

S1/S2 是测量，S3/S4 是触发，S5 是语义。本目录实现 S1–S4；S5 留给用户态 clock server。

## 最小理论模型

把模块封成黑盒，忽略回调唤醒谁，最小模型是：

```text
S = (c0, m, s, E, D, Q)

C: 只读、非降的计数器
φ(c) = E + ((c - c0) * m) >> s
D ∈ ticks ∪ {∞}: 单槽比较器
Q = {(handle, deadline, cb)}: 有限集合

now()     = φ(C())
arm(h,d)  = Q ← Q ∪ {(h,d)}; 若 d < min Q_old, D ← ceil(φ⁻¹(d))
cancel(h) = Q ← Q \ {(h,·)}; 若 h 是 min, D ← (Q=∅ ? ∞ : ceil(φ⁻¹(min Q)))
fire()    = D ← ∞; X ← {h | d_h ≤ now()}; Q ← Q \ X;
            D ← (Q=∅ ? ∞ : ceil(φ⁻¹(min Q))); 执行 X
```

六条不变量：

- **I1 单调**：`clock_now()` 非降。
- **I2 武装最小**：`Q ≠ ∅ ⇒ D = ceil(φ⁻¹(min Q))`（除 `fire` 临界区）。
- **I3 不早发**：回调只在 `now() ≥ deadline` 后执行。
- **I4 不丢**：`deadline ≤ now()` 的 timer 一定在本次或下一次 `fire` 中被弹出；`Q ≠ ∅` 时硬件一定在 `min Q` 附近产生中断。
- **I5 取消**：`clock_timer_cancel()` 返回 true 后，回调不会再执行；返回 false 表示已经进入派发，调用者必须容忍迟到的通知。
- **I6 终止**：回调若插入 `deadline ≤ now()` 的新 timer，必须有“立即执行/顺延”的边界策略，防止活锁。

**为什么是最小的**

- 只有 `C`：能测间隔，不能等待。
- 只有 `D`：能等一次，不能测，不能多路复用。
- 只有 `Q`：软件账本，到点拿不到 CPU。
- 因此 `C + D + Q` 必要且充分；`φ` 只是单位归一化，不是第四种机制；S5 是策略。

**为什么不需要周期 tick**

`sleep_until(d) = arm(d, wake)`；`timeout(op, d) = arm(d, cancel_op)`；`periodic(p)` 在回调里 `arm(d + p)`；`tick(HZ)` 就是 `periodic(1/HZ)`。Linux 的 `tick_periodic`/`tick_oneshot`/`tick_nohz`、seL4 MCS 的 absolute deadline、Fuchsia 的 timer queue 都说明 tick 可以是一个 one-shot 的周期客户端。最小模型天然是 tickless。

## 实现

### 时基

`clock_timebase_init()` 记录 boot 时的计数器读数 `epoch_ticks`，并把比较器撤防。`clock_now()` 的换算是精确的“整秒 + 余数”拆分：

```text
ns = epoch_ns + (delta / f) * 1e9 + ((delta % f) * 1e9) / f
```

这样避免 `delta * 1e9` 在 10 MHz 下约 30 分钟就溢出。它与 Linux 的 `mult/shift` 定点仿射变换等价；Linux 用移位是为了避免除法，本实现用 RISC-V M 扩展的除法换精确性和可读性。

`clock_ns_to_ticks_ceil()` 对 ns → ticks **向上取整**。比较器语义是 `C >= D` 触发；向下取整会让 `D` 落在 deadline 之前，造成早发。

### 定时器队列

`timer.c` 使用单 hart 的有序侵入式链表：

- 按 `(deadline, seq)` 升序；同 deadline 按 seq FIFO。
- `min` 在 head，插入 O(n)，取消 O(n)，取下一个 deadline O(1)。
- 微内核的内核定时器数量约等于阻塞线程数加调度事件数，通常很小；数量变大再换 rbtree，不需要 Linux 的 8 层时间轮。
- 只有队列头变化时才写比较器，避免每次 arm 都做一次昂贵的 SBI 调用。

### 中断路径

`clock_handle_interrupt()` 的固定顺序：

1. 写 `CLOCK_DISARM` 撤防。
2. 读 `now()`。
3. 从 head 弹出所有 `deadline <= now` 的 timer，整批标记 `RUNNING`。
4. 把新的 head 装进比较器。
5. 逐个执行回调；回调返回后，若仍是 `RUNNING` 则置回 `INACTIVE`。

第 1 步不能省：QEMU 在 `mtimecmp <= mtime` 时会立刻再次拉高中断，不撤防就是中断风暴。

第 4 步在回调之前：回调里如果 arm 了更早的 deadline，`clock_timer_arm()` 会再次改写比较器；如果先跑回调再装 head，回调里 arm 的更早 deadline 可能被漏掉。

### 状态与语义

```text
INACTIVE --arm--> ENQUEUED --dispatch--> RUNNING --callback return--> INACTIVE
   ^                  |                                      |
   +---- cancel ------+                                      +-- arm(self) --> ENQUEUED
```

| 操作 | 状态 | 结果 |
| --- | --- | --- |
| `arm` | `ENQUEUED` | 从队列摘除后重新插入（re-arm） |
| `arm` | `RUNNING` 且是当前回调 | 允许，回调返回后保持 `ENQUEUED` |
| `arm` | `RUNNING` 且不是当前回调 | 返回 false；避免把同一批次的其它 timer 重新插队 |
| `arm` | `INACTIVE` | 插入，返回 true |
| `cancel` | `ENQUEUED` | 摘除；若曾是 head 则重装；返回 true |
| `cancel` | `RUNNING` / `INACTIVE` | 返回 false |

`clock_timer_cancel()` 返回 false 不代表“绝对安全”：如果 timer 已经进入派发批次，回调仍会执行。IPC 超时最稳的用法是**把超时当唤醒提示，醒来重新检查条件**，而不是依赖 cancel 一定拦住回调。

过去 deadline 的策略：比较器会写入一个已经过去的值，QEMU 立即拉高中断；每次中断只派发一批，回调里再次 arm 过去 deadline 会触发下一次中断。回调不要无条件这样做，否则会形成中断风暴。

## API

| 函数 | 说明 |
| --- | --- |
| `clock_init()` | 初始化时基与队列，撤防比较器；在开中断前调用一次 |
| `clock_now()` | 单调 ns，自 `clock_init()` 起 |
| `clock_ticks()` | 当前原始硬件 ticks |
| `clock_freq()` | 硬件 timebase 频率 |
| `clock_timer_init(t, cb, arg)` | 初始化定时器对象；只能在 INACTIVE 时调用 |
| `clock_timer_arm(t, deadline)` | 绝对 deadline（ns）arm / re-arm |
| `clock_timer_cancel(t)` | 取消未派发的定时器 |
| `clock_timer_is_pending(t)` | 是否仍在队列中 |
| `clock_handle_interrupt()` | S-mode 定时器中断入口 |

### 使用示例

```c
#include "clock/clock.h"   /* 内核构建时 -Ikernel；本目录单独编译用 "clock.h" */

static struct clock_timer timeout;

static void on_timeout(struct clock_timer *timer)
{
    (void)timer;
    /* 只做通知：置标志、唤醒线程、发 IPC；不要阻塞。 */
}

void setup(void)
{
    clock_init();
    clock_timer_init(&timeout, on_timeout, 0);
    clock_timer_arm(&timeout, clock_now() + 10ULL * 1000 * 1000); /* 10 ms */
}
```

周期定时器要锚定旧 deadline，而不是 `now`：

```c
static void on_period(struct clock_timer *timer)
{
    clock_timer_arm(timer, timer->deadline + 10ULL * 1000 * 1000);
}
```

### 接入 trap

trap 层确认是 S-mode 定时器中断后调用，且调用时 `SIE` 已关闭：

```c
if (scause == 0x8000000000000005UL)
    clock_handle_interrupt();
```

`clock_init()` 不会打开 `sie.STIE` 或 `sstatus.SIE`，这属于中断/trap 模块。

## QEMU / RISC-V 映射

| 模型元素 | 本实现 | 关键细节 |
| --- | --- | --- |
| `C` | `rdtime`（`time` CSR） | RV64 一条 64 位读；RV32 需要 hi/lo/hi 循环 |
| `φ` | 整秒 + 余数拆分 | QEMU virt 10 MHz，1 tick = 100 ns |
| `D` | SBI TIME `sbi_set_timer(abs_ticks)` | 绝对 timebase ticks；`UINT64_MAX` 撤防 |
| IRQ | `scause` supervisor timer | 不走 PLIC，无 claim/complete；per-hart |
| 中断屏蔽 | `csrrc/csrs sstatus` | 只保存/恢复 `SIE` |

本机 QEMU 8.2.2 的 `virt` DTB 实测：

- `/cpus/timebase-frequency = 10000000`
- `/soc/clint@2000000`，compatible `riscv,clint0`，reg `0x2000000 + 0x10000`
- 传统 CLINT 布局：`mtime = +0xBFF8`，`mtimecmp = +0x4000 + 8*hart`

本实现刻意不硬编码这些 MMIO 地址：读时间用 `rdtime`，编程比较器用 SBI。这样 QEMU 换用 ACLINT 布局时也不受影响。

启动检查：

- `mcounteren.TM` 必须由 M-mode 放行，否则 `rdtime` 非法指令；OpenSBI 默认会设置。
- 无 Sstc 时，OpenSBI 在 M-mode 定时器到期后置 `MIP.STIP`，S-mode 看到的是 supervisor timer 中断；有 Sstc 时可以直接写 `stimecmp`，本实现暂未使用。
- SBI 调用是 `ecall` 进 M-mode，延迟高于直接写 CSR；高频 timer 才需要 Sstc 优化。

## 不归本目录

- 调度策略、时间片/预算怎么用 deadline → `kernel/sched/`
- 阻塞/超时语义、回调唤醒谁 → `kernel/ipc/`、`kernel/sched/`
- trap 分发、`STIE`/`SIE`、PLIC → `kernel/interrupt/`
- 墙钟、时区、NTP、POSIX timer、CPU 计费 → 用户态 clock server（规划目录 `userland/servers/clock/`）
- 平台频率、DTB 解析、RTC 驱动 → `kernel/arch/riscv/` 或用户态 driver server

## 边界判据

问：**换一个平台（QEMU → 真实板）这段逻辑还需要吗？**

- 需要 → 本目录。
- 只有 RISC-V/QEMU 才这样 → `event.c` 或 `kernel/arch/riscv/`。
- 只有某个 server 才这样 → 用户态。

第二个判据：**这是时间机制还是时间策略？** 机制留在本目录，策略外移到 clock server。

## 参考实现

| 参考 | 先读哪里 | 借什么 | 别照搬 |
| --- | --- | --- | --- |
| Linux | `include/linux/clocksource.h`、`kernel/time/timekeeping.c`、`include/linux/clockchips.h`、`kernel/time/timer.c`、`kernel/time/hrtimer.c`、`drivers/clocksource/timer-riscv.c` | 读/编程分离、one-shot/periodic、wheel vs rbtree、tick/nohz | NTP、POSIX、多时间轴、watchdog/broadcast |
| seL4 | `include/drivers/timer/arm_generic.h`、`CONFIG_KERNEL_MCS` | MCS 用绝对 `setDeadline` + `ackDeadlineIRQ`；非 MCS 周期重装；内核只保留调度时间 | ARM generic timer 寄存器细节 |
| Fuchsia | `zircon/kernel/kernel/timer.cc`、`zircon/vdso/timer.fidl`、`arch/riscv64/timer.cc` | timer 对象 + 每 CPU 队列 + signal；单调时钟快路径 | 完整对象/权限体系 |
| MINIX 3 | `kernel/clock.c` 的 clock task | `HARD_INT → tick → 到期 → 通知` 的服务边界 | 60 Hz 周期 tick 作为核心原语 |
| L4 / QNX | L4 `l4_timeout_t`；QNX `TimerTimeout()` | timeout 作为阻塞 IPC/同步操作的属性 | 完整信号/pulse 策略 |

## 构建与验证

```sh
cd micro/kernel/clock
make          # clang --target=riscv64-unknown-elf，生成 micro/build/clock/{timebase,timer,event}.o
make clean
```

当前验证范围：

- 三个 `.c` 在 `-Wall -Wextra` 下交叉编译无警告。
- 逻辑用宿主桩替换 `event.c` 验证过：`now` 单调、10 ms 单次触发、两个 timer 的 deadline 顺序、cancel 后不再触发、过去 deadline 立即派发、周期回调锚定旧 deadline。

在仓库根目录运行以下命令。demo 用 QEMU 的 S-mode 定时器中断触发 `clock_handle_interrupt()`，检查取消及相同 deadline 的 FIFO 顺序。

```sh
make -C micro run BOOT_MODULES=clock
```

### 与调度器的真实集成（本次验收）

此前 `clock` 的 API 与 clock demo 已经各自可用：S 态 timer 经 SBI
`sbi_set_timer()` 完成 one-shot 编程，`clock_handle_interrupt()` 负责撤防、
派发和重装；`sched` demo 也已有局部回调调用 `sched_tick()`。本次没有重写这些
机制，而是把周期性 tick 源收进调度器本体：

- `sched_start_tick()` 用现有 `clock_timer_arm()` 武装一个 one-shot timer，
  回调里调用 `sched_tick()`，并按旧 deadline 重装；同时置 `sie.STIE`。
- `sched_stop_tick()` 取消 timer；只有启动前 STIE 原本为 0 时才清
  `sie.STIE`，避免误关别的定时器使用者。
- 调度器暴露真实 tick / `cpu_switch` / 时间片耗尽计数，trap 路径仍是
  `clock_handle_interrupt()` 后 `sched_check()`。这两步调用目前只接在
  `sched/demo.c`，通用 `arch/riscv/trap.c` 尚未接入。

因此本节的证据不是 clock demo 的原文复制，而是同一次 QEMU 启动里
clock 模块通过后，sched 模块用 SBI one-shot 定时器触发真实抢占。

验收命令：

```sh
make -C micro BOOT_MODULES=clock,sched run
```

退出码 0。本目录的 [`qemu-clock-sched.log`](qemu-clock-sched.log) 保存了关键
串口；完整输出在 [`../sched/qemu-clock-sched.log`](../sched/qemu-clock-sched.log)。
两份 log 开头都有 `# command` / `# exit code`；OpenSBI 横幅的 NUL 字节已用
`tr -d '\000'` 去掉，未改其它内容。

关键串口：

```text
[clock] start
timer interrupts: canceled deadline and equal-deadline FIFO
[clock] PASS
[sched] timer: SBI one-shot tick source armed at 1 ms, sie.STIE=1
[sched] worker-a running without yielding
[sched] S-timer IRQ #1: scause=0x8000000000000005 tick=318797
[sched] S-timer IRQ #2: scause=0x8000000000000005 tick=328521 delta=9724
[sched] S-timer IRQ #3: scause=0x8000000000000005 tick=338524 delta=10003
[sched] sched_tick: worker-a time_left 1->0, need_resched set
[sched] switch: worker-a -> worker-b, sched switches=2
[sched] worker-a resumed after timer preemption
[sched] integrated: irqs=17 ticks=16 switches=11 quantum_expired=3 preempt_switches=1
[bootstrap] PASS
```

`[clock] PASS` 仍是既有 clock demo 的队列/取消/FIFO 验收；新增证据是后面的
`sched` 段：S 态 timer 中断 `scause=0x8000000000000005` 连续三次触发，
相邻 `tick` 差为 9724 和 10003，说明 one-shot 比较器被重装并再次触发。
该间隔不保证精确等于 period（1ms @ 10MHz = 10000 ticks），可能受首次
pending、中断处理耗时以及回调按旧 deadline 重装影响。`worker-a
time_left 1->0` 后发生 `worker-a -> worker-b` 的真实切换。

## 已知限制

- 单 hart：`head` / `running` 是全局变量。SMP 需要按 CPU 复制队列与比较器，并处理 timer 迁移。
- `CLOCK_TIMEBASE_HZ` 固定为 QEMU virt 的 10 MHz；接入真实板时应从 DTB 的 `/cpus/timebase-frequency` 读取。
- 只使用 SBI TIME；有 Sstc 时可加直接写 `stimecmp` 的快路径。
- 没有墙钟/RTC/NTP/POSIX timer/CPU 计费；这些属于 clock server。
- `rdtime` 按 RV64 实现；RV32 需要 hi/lo/hi 原子读。
- 没有限流：回调若反复 arm 过去的 deadline，会形成中断风暴。
- `clock_handle_interrupt()` 要求调用者已关闭 `SIE`；它不负责使能 `STIE`。
- `clock_timer_cancel()` 对 `RUNNING` 返回 false，回调仍会执行；调用者必须处理迟到通知。
- 定时器对象由调用者持有，不使用动态分配；只能在 INACTIVE 时 `clock_timer_init()`，已入队时重复 init 会破坏链表。
