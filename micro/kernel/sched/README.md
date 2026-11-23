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

## 代码

| 文件 | 内容 |
|---|---|
| `sched.h` | Thread/CPU 上下文/等待队列/API |
| `sched.c` | 就绪队列、RR 时间片、block/wake、idle、死线程回收 |
| `switch_rv64.S` | RISC-V 64 的 `cpu_switch` / `cpu_switch_to` |
| `sched_test.c` | RR、时间片、block/wake、idle 回退、回收自测 |
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

void sched(void);
void sched_tick(void);
void sched_check(void);

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
sched: all tests passed
```

主机自测的 `cpu_switch` 只记录调用，不真正切栈；因此它验证的是状态机、队列和
时间片决策，不是实际寄存器切换。RISC-V 真实切换由 `switch_rv64.S` 提供。

## QEMU 接线

```sh
make -C micro run BOOT_MODULES=sched
```

自举 demo 用两条线程和真实 RV64 上下文切换验证阻塞、唤醒和就绪队列顺序。

```c
object_system_init();
sched_init();

/* 用 memory 模块的页作为内核栈 */
struct thread *main = thread_create(0, kmain_thread, 0, stack, stack_size, "main");
struct thread *idle = thread_create(0, idle_loop, 0, idle_stack, idle_stack_size,
                                    "idle");
sched_set_idle(idle);
sched_bootstrap(main);

/* 10 ms 定时器中断： */
sched_tick();

/* trap 返回用户/内核前： */
sched_check();
```

RISC-V 细节：

- `sched_lock()` 用 `csrrci sstatus, 2` 清 `SIE`，`sched_unlock()` 按旧值恢复。
- `cpu_switch` 的偏移必须与 `struct cpu_context` 一致：`ra=0, sp=8, s0=16, ...,
  s11=104`。
- 新线程的 `ra` 指向 `thread_bootstrap`，`sp` 指向栈顶（16 字节对齐）；
  `thread_bootstrap` 读取 `current`，调用 `entry(arg)`，然后 `thread_exit()`。
- `sched()` 在切换前对 `next->as != NULL` 的线程调用 `as_activate(next->as)`；
  `next->as == NULL` 的内核线程沿用当前内核映射。
- 用户 AS 必须通过 `as_share_kernel()` 安装内核顶层映射，否则切到用户页表后
  内核栈/代码不可访问。见 `memory/README.md`。

## 已知边界

- 单核、无优先级、无 CFS/EEVDF、无实时调度、无 SMP。
- 无线程超时；等待队列的 timeout 需要额外定时器队列。
- 不做进程对象；`waitpid`、信号、fd 表等属于用户态 PM/VFS。
- `thread_exit()` 依赖 idle 线程兜底；启动代码必须创建 idle。
