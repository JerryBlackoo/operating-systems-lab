# sched：线程状态机与时间片调度

本目录实现微内核最小模型里的“时间层”：Thread 状态机、就绪队列、每线程时间片、
上下文切换，以及给 IPC 使用的 block/wake 原语。

内核不定义“进程”。调度单位是 Thread；AddressSpace 是独立对象，由 Thread 持有引用。

其他路径的代码没有修改；本目录可以独立编译和自测（主机自测使用 fake 切换）。

## 最小模型

```text
Thread = (context, state, as, time_left, msg, waitq)
Sched  = (ready_queue, current, idle)

          create
    FREE --------> READY <---- wake ---- BLOCKED
                    |  ^                  ^
               dispatch|  |tick/preempt   |block
                    v  |                  |
                 RUNNING ---------------->+
                    |
                  exit
                    v
                   DEAD
```

- 就绪队列是 FIFO；`pick_next` 就是取队首。
- 每线程独立 `time_left`，tick 只扣当前线程；耗尽后置 `need_resched`。
- `block` 原子地改状态、入等待队列；`wake` 原子地摘下并放回就绪队列。
- 没有就绪线程时回退到 idle；idle 是普通 Thread，但不在就绪队列里重复出现。
- 死线程不立即释放：下一次 `sched()` 在安全点统一回收它的 AS 和对象。

## 不变量

1. 单核上恰好一个 `RUNNING`，且它等于 `current`。
2. `READY` 等价于“在就绪队列中恰好一次”；`RUNNING` 不在任何队列。
3. `BLOCKED` 的线程挂在恰好一个等待队列上。
4. `DEAD` 的线程不在就绪队列/等待队列；回收前 `reaped=false`。
5. 无丢失唤醒：状态检查、入等待队列、唤醒摘链都在 `sched_lock()` 内完成。
6. 上下文切换只保存 callee-saved：`ra, sp, s0-s11`；caller-saved 由 trap 入口保存。
7. 就绪队列和 `need_resched` 的修改都受 `sched_lock()` 保护。
8. 从“改状态/选 next”到 `cpu_switch` 完成全程关中断；恢复 SIE 的责任交给
   被恢复的线程（它从自己的 `cpu_switch` 返回后恢复自己栈上的 flags）。
   新线程第一次运行由 `thread_bootstrap` 打开中断。
9. `reap_dead()` 只有在当前执行流不再依赖该 AS 时才释放页表；如果死线程的
   AS 仍挂在 `satp` 上，先切回 bare 映射再 `as_unref()`。
10. `sched()` 切换到 `as == NULL` 的内核线程时，若 `active_as != NULL`，必须
    `as_activate(0)` 并清 `active_as`；内核线程在 bare 映射下按物理地址访问
    内核镜像和直接映射窗口。用户线程之间的切换则 `as_activate(next->as)`。
10. `thread_block()/thread_yield()/sched_check()` 把“状态迁移 + sched()”放在
    同一个关中断区间内，避免定时器在两步之间插入一次调度。


## 代码

| 文件 | 内容 |
|---|---|
| `sched.h` | Thread/CPU 上下文/等待队列/API |
| `sched.c` | 就绪队列、RR 时间片、block/wake、idle、死线程回收、定时睡眠、周期 tick 源 |
| `switch_rv64.S` | RISC-V 64 的 `cpu_switch` / `cpu_switch_to` |
| `sched_test.c` | RR、时间片、block/wake、idle 回退、回收、容量/等待队列自测 |
| `Makefile` | 主机自测构建（`-DSCHED_HOST_TEST`，fake `cpu_switch`） |

## API

```c
void sched_init(void);
struct thread *thread_create(struct as *as, void (*entry)(void *), void *arg,
                             void *stack_base, size_t stack_size,
                             const char *name);
void thread_exit(void) __attribute__((noreturn));
void thread_yield(void);

void thread_block(struct waitq *wq);
void thread_wake(struct thread *t);
void thread_block_locked(struct waitq *wq); /* 调用者持 sched_lock */
void thread_wake_locked(struct thread *t);  /* 调用者持 sched_lock */

/* 单调时间睡眠，deadline 来自 clock 的 ns 时间轴；仅内核构建提供。 */
void thread_sleep_ns(uint64_t ns);
void thread_sleep_until(uint64_t deadline_ns);

void sched(void);
void sched_tick(void);
void sched_check(void);

/* 周期性 tick 源；调用前先 clock_init()，最后再打开 sstatus.SIE。 */
bool sched_start_tick(uint64_t period_ns); /* 武装 timer + 置 sie.STIE */
void sched_stop_tick(void);                /* 撤销 timer；按启动前状态恢复 STIE */
uint64_t sched_tick_count(void);           /* 真实 tick 回调次数 */
uint64_t sched_switch_count(void);         /* 真实 cpu_switch 次数 */
uint64_t sched_quantum_expiration_count(void); /* time_left 耗尽次数 */

void sched_bootstrap(struct thread *t); /* 启动/自测：强制 current */
void sched_set_idle(struct thread *t);
uint64_t sched_lock(void);
void sched_unlock(uint64_t flags);
```

`thread_create()` 需要调用者提供内核栈（`stack_base + stack_size`）。最小模型不负责
分配栈；启动代码可以从 memory 模块取页作为栈。

## 构建与自测

```sh
make -C micro/kernel/sched test
```

预期输出：

```text
sched: round robin ok
sched: block/wake ok
sched: idle/reap ok
sched: idle-only fallback ok
sched: create validation/capacity ok
sched: waitq FIFO/remove ok
sched: quantum carry ok
sched: all tests passed
```

主机自测的 `cpu_switch` 只记录调用，不真正切栈；因此它验证的是状态机、队列和
时间片决策，不是实际寄存器切换。RISC-V 真实切换由 `switch_rv64.S` 提供。

## QEMU 接线：真实 S 态定时器 → 调度

### 此前已有的部分

- `clock` 已有 `clock_timer_arm()`，经 SBI TIME `sbi_set_timer()` 编程 one-shot
  比较器；`clock_handle_interrupt()` 负责撤防、派发到期回调并重装下一次 deadline。
- `sched` 已有 `sched_tick()`（每 tick 扣 `time_left`，耗尽置 `need_resched`）
  与 `sched_check()`。
- `sched` demo 之前在自己的 `demo.c` 里维护一个周期性 `clock_timer` 回调，
  回调中调用 `sched_tick()`。

### 本次新增的钩子

- `sched_start_tick(period_ns)`：把 tick 源收进 sched 本体，武装 one-shot
  `clock_timer`；回调中调用 `sched_tick()`，按旧 deadline 重装下一次比较值，
  并置 `sie.STIE`。`sstatus.SIE` 留给调用者在布置完成后打开。
- `sched_stop_tick()`：取消 timer；只有启动前 STIE 原本为 0 时才清
  `sie.STIE`，避免误关别的定时器使用者。
- `sched_tick_count()` / `sched_switch_count()` /
  `sched_quantum_expiration_count()`：记录真实 tick、真实 `cpu_switch` 与
  `time_left` 耗尽次数，供 QEMU 串口核对。
- demo 不再自带 `on_tick`；trap 路径仍是
  `clock_handle_interrupt() → sched_tick()`，返回前 `sched_check()`。
- 本次接线把这段 glue 放进了正式镜像实际使用的 `shared/demo.c`
  `trap_handler`：模块 trap 先获得一次观察/接管机会（`clock`/`interrupt`
  的专用 demo 需要直接驱动自己的定时器/irq 层），未接管则默认执行
  `clock_handle_interrupt()` + `sched_check()`。`kernel/syscall/closure.c`
  的两个用户线程在 QEMU 上通过这条默认路径发生了真实抢占和 satp 切换，
  不再依赖 `sched/demo.c` 才能跑通。`arch/riscv/trap.c` 仍然只属于
  `ARCH_SELFTEST` 自检镜像。
- 从用户线程切到 `as == NULL` 的内核线程时，`sched()` 现在会
  `as_activate(0)` 并清 `active_as`；否则 satp 会继续指向无关的用户页表。

### 验收命令与结果

```sh
make -C micro BOOT_MODULES=clock,sched run
```

退出码 0。完整串口见 [`qemu-clock-sched.log`](qemu-clock-sched.log)：
开头记录了 command / exit code；OpenSBI 横幅中的 NUL 已用 `tr -d '\000'`
去掉，未改其它内容。

关键串口：

```text
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
[sched] PASS
[bootstrap] PASS
```

`scause=0x8000000000000005` 是 S 态 timer 中断（bit 63 | cause 5）。
连续三次 IRQ 的 `tick` 差分别为 9724 和 10003，说明 one-shot 比较值确实
被重装并再次触发；但间隔不一定精确等于 period（1ms @ 10MHz = 10000
ticks），可能受首次 pending、中断处理耗时以及回调按旧 deadline 重装影响。
`time_left 1->0` 与 `worker-a -> worker-b` 证明时间片耗尽后确实发生了真实
上下文切换，而不是只在宿主自测里手动调用 `sched_tick()`。

工作负载：

- `worker-a` / `worker-b`：纯计算、不主动让出，验证 5 tick 时间片抢占；
- `client` / `server`：一次同步 `ep_call/ep_reply`，验证阻塞/唤醒与调度器协同；
- `idle`：所有普通线程阻塞或死亡后兜底 `wfi`。

构建时 `MODEL_SUPPORT_sched` 会链接 `clock` 与 `ipc` 的非 demo 源文件，因此
`BOOT_MODULES=sched` 得到的是一个可运行的最小内核工作负载，而不是孤立的单元测试。

```c
object_system_init();
sched_init();
ipc_init();
clock_init();

struct thread *main = thread_create(0, kmain_thread, 0, stack, stack_size, "main");
struct thread *idle = thread_create(0, idle_loop, 0, idle_stack, idle_stack_size,
                                    "idle");
sched_set_idle(idle);
sched_bootstrap(main);

/* 1 ms one-shot tick：回调里 sched_tick()，再重装下一次 deadline。 */
CHECK(sched_start_tick(1000000ULL));
irq_enable(); /* sched_start_tick 只置 sie.STIE，不打开 sstatus.SIE。 */

/* trap：clock_handle_interrupt() -> sched_tick()
 *       sched_check() -> sched() -> cpu_switch() */

sched_stop_tick();
```

RISC-V 细节：

- `sched_lock()` 用 `csrrci sstatus, 2` 清 `SIE`，`sched_unlock()` 按旧值恢复。
- `cpu_switch` 的偏移必须与 `struct cpu_context` 一致：`ra=0, sp=8, s0=16, ...,
  s11=104`。
- 新线程的 `ra` 指向 `thread_bootstrap`，`sp` 指向栈顶（16 字节对齐）；
  `thread_bootstrap` 打开 `sstatus.SIE`，读取 `current`，调用 `entry(arg)`，
  然后 `thread_exit()`。
- `sched()` 在切换前对 `next->as != NULL` 的线程调用 `as_activate(next->as)`；
  激活与 `cpu_switch` 都在关中断状态下完成。`next->as == NULL` 的内核线程
  沿用当前内核映射。
- 回收死线程时若其 AS 仍是 `satp` 的当前目标，先 `as_activate(NULL)` 退回
  bare 映射再释放页表，避免释放后仍被 TLB 走页。
- 用户 AS 必须通过 `as_share_kernel()` 安装内核顶层映射，否则切到用户页表后
  内核栈/代码不可访问。见 `memory/README.md`。

## 已知边界

- 单核、无优先级、无 CFS/EEVDF、无实时调度、无 SMP。
- `sched_start_tick()` 假定调用者已完成 `clock_init()`，且当前 hart 的
  clock 队列可用；停止时必须先 `sched_stop_tick()` 再拆除 clock。
  它只置 `sie.STIE`，`sstatus.SIE` 由调用者统一打开。
- 主机自测（`-DSCHED_HOST_TEST`）不编译 tick 源，`sched_test` 只验证状态机、
  队列和时间片决策；真实 SBI 定时器与 `cpu_switch` 由 QEMU 验收覆盖。
- 等待队列本身不带 timeout；`thread_sleep_until()` 用一个临时的
  `clock_timer` 实现线程睡眠，不改变 `waitq` 的阻塞/唤醒语义。
- 不做进程对象；`waitpid`、信号、fd 表等属于用户态 PM/VFS。
- `thread_exit()` 依赖 idle 线程兜底；启动代码必须创建 idle。
